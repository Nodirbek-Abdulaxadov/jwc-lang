//! Expression evaluation — `Vm::eval_expr` and the two numeric helpers it
//! shares with the comparison operators.
//!
//! `eval_expr` is the giant `match Expr` arm. The `Expr::Call` branch is the
//! dispatcher for every built-in name; each named builtin delegates to a
//! `Vm::eval_*_call` method living in `runner/builtins.rs`. Keeping the
//! dispatch table here (rather than in `builtins.rs`) lets the call sites
//! see the order user code expects: name shadowing rules (`substring`,
//! `take`) and short-circuit behaviour on Phase-1 fast paths stay readable
//! next to the rest of the expression match.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_recursion::async_recursion;
use serde_json::{json, Value as JsonValue};
use tokio_postgres::types::ToSql;

use crate::ast::Expr;

use super::dispatch::content_type_response;
use super::http_client;
use super::sql::{
    aggregate_sql_op, boxed_params_to_refs, build_navigation_subqueries, build_select_sql,
    build_where_sql, field_path_to_col,
};
use super::util::{closest_match, current_utc_iso8601};
use super::{engine, json_to_value, materialize_select_result, value_to_json, Value, Vm};

impl<'a> Vm<'a> {
    #[async_recursion]
    pub(super) async fn eval_expr(
        &mut self,
        expr: &Expr,
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        match expr {
            Expr::Int(v) => Ok(Value::Int(*v)),
            Expr::Float(v) => {
                let parsed = v
                    .parse::<f64>()
                    .map_err(|_| anyhow!("Invalid float literal: {v}"))?;
                Ok(Value::Float(parsed))
            }
            Expr::Str(v) => Ok(Value::Str(v.clone())),
            Expr::Bool(v) => Ok(Value::Bool(*v)),
            Expr::Null => Ok(Value::Null),
            Expr::Var(name) => {
                let key = name.to_lowercase();
                if let Some(v) = vars.get(&key) {
                    Ok(v.clone())
                } else if let Some(v) = self.consts.get(&key) {
                    Ok(v.clone())
                } else {
                    let suggestion = closest_match(name, vars.keys());
                    match suggestion {
                        Some(s) => Err(anyhow!("Undefined variable: {name}. Did you mean '{s}'?")),
                        None => Err(anyhow!("Undefined variable: {name}")),
                    }
                }
            }
            Expr::NewEntity { entity: _ } => Ok(Value::Str("{}".to_string())),
            Expr::FieldGet { var, field } => {
                let obj_val = vars
                    .get(&var.to_lowercase())
                    .cloned()
                    .ok_or_else(|| anyhow!("Undefined variable: {var}"))?;
                match obj_val {
                    // **Phase 1** — Record fast path: O(N) name lookup, no JSON
                    // parse, no string allocation. Returns Null for unknown
                    // fields to match the V::Str arm's `None`/`Null` behavior.
                    Value::Record { .. } => Ok(obj_val
                        .record_field(field.as_str())
                        .cloned()
                        .unwrap_or(Value::Null)),
                    Value::Str(s) => {
                        let doc: serde_json::Value = serde_json::from_str(&s)
                            .with_context(|| format!("FieldGet: '{}' is not valid JSON", var))?;
                        match doc.get(field.as_str()) {
                            Some(serde_json::Value::String(s)) => Ok(Value::Str(s.clone())),
                            Some(serde_json::Value::Number(n)) => {
                                if let Some(i) = n.as_i64() {
                                    Ok(Value::Int(i))
                                } else if let Some(f) = n.as_f64() {
                                    Ok(Value::Float(f))
                                } else {
                                    Ok(Value::Null)
                                }
                            }
                            Some(serde_json::Value::Bool(b)) => Ok(Value::Bool(*b)),
                            Some(serde_json::Value::Null) | None => Ok(Value::Null),
                            Some(v) => Ok(Value::Str(v.to_string())),
                        }
                    }
                    Value::Null => Ok(Value::Null),
                    other => bail!(
                        "FieldGet: '{}' is not a JSON object, got {}",
                        var,
                        other.type_name()
                    ),
                }
            }
            Expr::DbAggregate {
                kind,
                field,
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let col = field_path_to_col(field);
                let (agg_sql, kind_tag) = aggregate_sql_op(*kind, &col);

                let mut shape_bits: Vec<String> = vec![format!("agg:{kind_tag}:{col}")];
                let mut cache_bits: Vec<String> = vec![format!("agg:{kind_tag}:{col}")];
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

                let where_sql = if let Some(wc) = where_clause {
                    let s = build_where_sql(
                        wc,
                        &mut params,
                        &mut shape_bits,
                        &mut cache_bits,
                        vars,
                        self,
                        None,
                    )
                    .await?;
                    format!(" WHERE {}", s)
                } else {
                    String::new()
                };

                let sql = format!(
                    "SELECT ({})::text FROM \"{}\"{};",
                    agg_sql, table_name, where_sql
                );
                let shape_key = format!("aggregate|table:{table_name}|{}", shape_bits.join("|"));
                let cache_key = format!(
                    "result|aggregate|table:{table_name}|{}",
                    cache_bits.join("|")
                );

                let param_refs = boxed_params_to_refs(&params);
                let compiled = engine::get_or_compile_sql(&shape_key, || Ok(sql.clone()))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled, &param_refs)
                        .await?;
                let trimmed = result.trim();
                if trimmed.is_empty() || trimmed == "null" {
                    return Ok(Value::Null);
                }
                if let Ok(i) = trimmed.parse::<i64>() {
                    return Ok(Value::Int(i));
                }
                if let Ok(f) = trimmed.parse::<f64>() {
                    return Ok(Value::Float(f));
                }
                Ok(Value::Str(trimmed.to_string()))
            }
            Expr::DbCount {
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let mut shape_bits: Vec<String> = Vec::new();
                let mut cache_bits: Vec<String> = Vec::new();
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

                let where_sql = if let Some(wc) = where_clause {
                    let s = build_where_sql(
                        wc,
                        &mut params,
                        &mut shape_bits,
                        &mut cache_bits,
                        vars,
                        self,
                        None,
                    )
                    .await?;
                    format!(" WHERE {}", s)
                } else {
                    String::new()
                };

                let sql = format!(
                    "SELECT COUNT(*)::text FROM \"{}\"{};",
                    table_name, where_sql
                );
                let shape_key = format!(
                    "count|table:{table_name}|{}",
                    if shape_bits.is_empty() {
                        "no_where".to_string()
                    } else {
                        shape_bits.join("|")
                    }
                );
                let cache_key = format!(
                    "result|count|table:{table_name}|{}",
                    if cache_bits.is_empty() {
                        "no_where".to_string()
                    } else {
                        cache_bits.join("|")
                    }
                );

                let param_refs = boxed_params_to_refs(&params);
                let compiled = engine::get_or_compile_sql(&shape_key, || Ok(sql.clone()))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled, &param_refs)
                        .await?;
                let n = result.trim().parse::<i64>().with_context(|| {
                    format!("count(*): expected integer text, got '{}'", result.trim())
                })?;
                Ok(Value::Int(n))
            }
            Expr::DbSelect {
                entity,
                context_var: _,
                table,
                where_clause,
                order_by,
                limit,
                offset,
                first,
                with_relations,
                projection,
                aggregates,
                aliased_cols,
                joins,
                group_by,
                having,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let nav_subqueries = build_navigation_subqueries(
                    entity,
                    &table_name,
                    with_relations,
                    &self.models,
                    &self.pk_by_table,
                )?;
                let (sql, boxed_params, shape_key, cache_key) = build_select_sql(
                    table_name,
                    entity,
                    where_clause.as_deref(),
                    order_by.as_ref(),
                    limit.as_deref(),
                    offset.as_deref(),
                    *first,
                    &nav_subqueries,
                    projection,
                    aggregates,
                    aliased_cols,
                    joins,
                    group_by,
                    having.as_deref(),
                    vars,
                    self,
                )
                .await?;
                // Cache hit short-circuit: skip get_or_compile_sql + DB roundtrip.
                if let Some(cached) = engine::try_cached_result(&cache_key)? {
                    return Ok(materialize_select_result(&cached));
                }
                let param_refs = boxed_params_to_refs(&boxed_params);
                let compiled_sql = engine::get_or_compile_sql(&shape_key, || Ok(sql))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled_sql, &param_refs)
                        .await?;
                Ok(materialize_select_result(&result))
            }
            Expr::Call { name, args } => {
                if name.eq_ignore_ascii_case("dispatch") {
                    return self.eval_dispatch_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("path_param") {
                    return self.eval_path_param_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("query_param") {
                    return self.eval_query_param_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("request_path") {
                    if !args.is_empty() {
                        bail!("request_path() expects no args");
                    }
                    return Ok(self
                        .current_request_path
                        .clone()
                        .map(Value::Str)
                        .unwrap_or(Value::Null));
                }

                if name.eq_ignore_ascii_case("request_method") {
                    if !args.is_empty() {
                        bail!("request_method() expects no args");
                    }
                    return Ok(self
                        .current_method
                        .clone()
                        .map(Value::Str)
                        .unwrap_or(Value::Null));
                }

                if name.eq_ignore_ascii_case("header") {
                    return self.eval_header_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("client_ip") {
                    return self.eval_client_ip_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("request_id") {
                    if !args.is_empty() {
                        bail!("request_id() expects no args");
                    }
                    return Ok(self
                        .current_request_id
                        .clone()
                        .map(Value::Str)
                        .unwrap_or(Value::Null));
                }

                if name.eq_ignore_ascii_case("response_status") {
                    if !args.is_empty() {
                        bail!("response_status() expects no args");
                    }
                    return Ok(self
                        .current_response_status
                        .map(|s| Value::Int(s as i64))
                        .unwrap_or(Value::Null));
                }

                if name.eq_ignore_ascii_case("response_duration_ms") {
                    if !args.is_empty() {
                        bail!("response_duration_ms() expects no args");
                    }
                    return Ok(self
                        .current_request_started
                        .map(|t| Value::Int(t.elapsed().as_millis() as i64))
                        .unwrap_or(Value::Null));
                }

                if name.eq_ignore_ascii_case("context") {
                    return self.eval_context_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("setContext")
                    || name.eq_ignore_ascii_case("set_context")
                {
                    return self.eval_context_set_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("http_get") {
                    return self.eval_http_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("http_post") {
                    return self.eval_http_post_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("jwt_sign") {
                    return self.eval_jwt_sign_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("jwt_verify") {
                    return self.eval_jwt_verify_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("hash_password") {
                    return self.eval_hash_password_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("ws_send") {
                    return self.eval_ws_send_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("ws_recv") {
                    return self.eval_ws_recv_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("ws_close") {
                    return self.eval_ws_close_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("verify_password") {
                    return self.eval_verify_password_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_get") {
                    return self.eval_cache_get_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_set") {
                    return self.eval_cache_set_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_del") {
                    return self.eval_cache_del_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("cache_clear") {
                    return self.eval_cache_clear_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("send_email") {
                    return self.eval_send_email_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("register_job_handler") {
                    return self.eval_register_job_handler_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("enqueue") {
                    return self.eval_enqueue_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("enqueue_urgent") {
                    return self.eval_enqueue_urgent_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("job_count") {
                    return self.eval_job_count_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("dlq_count") {
                    return self.eval_dlq_count_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("dlq_drain") {
                    return self.eval_dlq_drain_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("unauthorized") {
                    return Ok(Value::Str(
                        r#"{"__jwc_status__":401,"error":"Unauthorized"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("forbidden") {
                    return Ok(Value::Str(
                        r#"{"__jwc_status__":403,"error":"Forbidden"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("db_query") {
                    return self.eval_db_query_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("raw_sql") {
                    return self.eval_raw_sql_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("request_body") {
                    return self.eval_request_body_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("uuid") {
                    return self.eval_uuid_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("length") || name.eq_ignore_ascii_case("len") {
                    return self.eval_length_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("lower") {
                    return self
                        .eval_string_call(args, vars, "lower", |s| s.to_lowercase())
                        .await;
                }
                if name.eq_ignore_ascii_case("upper") {
                    return self
                        .eval_string_call(args, vars, "upper", |s| s.to_uppercase())
                        .await;
                }
                if name.eq_ignore_ascii_case("trim") {
                    return self
                        .eval_string_call(args, vars, "trim", |s| s.trim().to_string())
                        .await;
                }
                if name.eq_ignore_ascii_case("contains") {
                    return self.eval_contains_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("starts_with") {
                    return self
                        .eval_two_string_bool_call(args, vars, "starts_with", |s, p| {
                            s.starts_with(p)
                        })
                        .await;
                }
                if name.eq_ignore_ascii_case("ends_with") {
                    return self
                        .eval_two_string_bool_call(args, vars, "ends_with", |s, p| s.ends_with(p))
                        .await;
                }
                if name.eq_ignore_ascii_case("replace") {
                    return self.eval_replace_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("split") {
                    return self.eval_split_call(args, vars).await;
                }
                // `substring` / `take` defer to a user-declared function of
                // the same name when one exists — neither name is reserved,
                // and pre-existing programs use `take` as a pass-through verb
                // in their own code. Shadowing would silently break them.
                if name.eq_ignore_ascii_case("substring")
                    && !self.functions.contains_key(&name.to_lowercase())
                {
                    return self.eval_substring_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("take")
                    && !self.functions.contains_key(&name.to_lowercase())
                {
                    return self.eval_take_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("first") {
                    return self.eval_first_or_last_call(args, vars, true).await;
                }
                if name.eq_ignore_ascii_case("last") {
                    return self.eval_first_or_last_call(args, vars, false).await;
                }
                if name.eq_ignore_ascii_case("range") {
                    return self.eval_range_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("push") || name.eq_ignore_ascii_case("append") {
                    return self.eval_push_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("join") {
                    return self.eval_join_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("json_parse") {
                    return self.eval_json_parse_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("json_stringify") {
                    return self.eval_json_stringify_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("sha256") {
                    return self.eval_sha256_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("sha1") {
                    return self.eval_sha1_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("md5") {
                    return self.eval_md5_call(args, vars).await;
                }
                if name.eq_ignore_ascii_case("hmac_sha256") {
                    return self.eval_hmac_sha256_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("now") {
                    if !args.is_empty() {
                        bail!("now() expects no args");
                    }
                    return Ok(Value::Str(current_utc_iso8601()?));
                }

                if name.eq_ignore_ascii_case("unix_timestamp") {
                    if !args.is_empty() {
                        bail!("unix_timestamp() expects no args");
                    }
                    let secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| anyhow!("System clock error"))?
                        .as_secs() as i64;
                    return Ok(Value::Int(secs));
                }

                if name.eq_ignore_ascii_case("set_json_field") {
                    return self.eval_set_json_field_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("body") {
                    return self.eval_request_body_call(args, vars).await;
                }

                // ── `serve(port?)` — starts HTTP server from main() ───────
                if name.eq_ignore_ascii_case("serve") {
                    let port: u16 = if let Some(arg) = args.first() {
                        match self.eval_expr(arg, vars).await? {
                            Value::Int(n) if n > 0 && n <= 65535 => n as u16,
                            Value::Int(n) => bail!("serve(): invalid port {n}"),
                            other => {
                                bail!("serve(port): port must be int, got {}", other.type_name())
                            }
                        }
                    } else {
                        8080
                    };
                    self.serve_requested = Some(port);
                    return Ok(Value::Void);
                }

                // ── `env("VAR_NAME")` — read an environment variable ──────
                if name.eq_ignore_ascii_case("env") {
                    if args.len() != 1 {
                        bail!("env(name) expects exactly 1 arg");
                    }
                    let var_name = self.eval_expr(&args[0], vars).await?;
                    let var_name = match var_name {
                        Value::Str(s) => s,
                        other => bail!("env(name): name must be string, got {}", other.type_name()),
                    };
                    let val = std::env::var(&var_name).unwrap_or_default();
                    return Ok(Value::Str(val));
                }

                // ── `int(v)` — coerce to integer (mirrors native jwc_b_int) ──
                if name.eq_ignore_ascii_case("int") {
                    if args.len() != 1 {
                        bail!("int(v) expects exactly 1 arg");
                    }
                    let n = match self.eval_expr(&args[0], vars).await? {
                        Value::Int(n) => n,
                        Value::Float(f) => f as i64,
                        Value::Str(s) => s.parse::<i64>().unwrap_or(0),
                        Value::Bool(b) => i64::from(b),
                        _ => 0,
                    };
                    return Ok(Value::Int(n));
                }

                // ── `setConnectionString(...)` — pin DATABASE_URL for this process.
                //
                // Three legal forms:
                //   setConnectionString();                           // pull from env (.env auto-loaded)
                //   setConnectionString("postgres://user:p@h:port/db");
                //   setConnectionString({
                //       host: "localhost", port: 5432,
                //       user: "postgres",  password: "x",
                //       database: "myapp"
                //   });
                if name.eq_ignore_ascii_case("setConnectionString")
                    || name.eq_ignore_ascii_case("set_connection_string")
                {
                    return self.eval_set_connection_string_call(args, vars).await;
                }

                if name.eq_ignore_ascii_case("sleep_ms") {
                    if args.len() != 1 {
                        bail!("sleep_ms(n) expects exactly 1 arg");
                    }
                    let n = match self.eval_expr(&args[0], vars).await? {
                        Value::Int(n) if n >= 0 => n as u64,
                        Value::Int(n) => bail!("sleep_ms(n): n must be >= 0, got {n}"),
                        other => bail!("sleep_ms(n): n must be int, got {}", other.type_name()),
                    };
                    tokio::time::sleep(std::time::Duration::from_millis(n)).await;
                    return Ok(Value::Null);
                }

                if name.eq_ignore_ascii_case("fetch_json") {
                    if args.len() != 1 {
                        bail!("fetch_json(url) expects exactly 1 arg");
                    }
                    let url = match self.eval_expr(&args[0], vars).await? {
                        Value::Str(s) => s,
                        other => bail!(
                            "fetch_json(url): url must be string, got {}",
                            other.type_name()
                        ),
                    };
                    super::util::check_url_allowlisted(&url)?;
                    let resp = http_client()
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| anyhow!("fetch_json({url}) failed: {e}"))?;
                    let body = resp
                        .text()
                        .await
                        .map_err(|e| anyhow!("fetch_json: read body failed: {e}"))?;
                    let parsed: JsonValue = serde_json::from_str(&body)
                        .map_err(|e| anyhow!("fetch_json: invalid JSON: {e}"))?;
                    return Ok(json_to_value(&parsed));
                }

                // ── HTTP response helpers ──────────────────────────────────
                //
                // Phase 4 [1.0-blocker] — `json(s)` on a Value::Str validates
                // the string is well-formed JSON before passthrough. The
                // interpreter is the dev / debug runtime, so the cost is
                // acceptable; the README footgun (a `Value::Str("not-json")`
                // body silently shipping as a 200 with malformed bytes) is
                // closed here. `json_unchecked(s)` keeps the old behaviour for
                // callers that have already validated the payload (e.g. SELECT
                // result fast path).
                if name.eq_ignore_ascii_case("json") || name.eq_ignore_ascii_case("json_unchecked")
                {
                    let is_unchecked = name.eq_ignore_ascii_case("json_unchecked");
                    let label = if is_unchecked {
                        "json_unchecked"
                    } else {
                        "json"
                    };
                    if args.len() != 1 {
                        bail!("{}(val) expects exactly 1 arg", label);
                    }
                    let val = self.eval_expr(&args[0], vars).await?;
                    return Ok(match val {
                        Value::Str(s) => {
                            if !is_unchecked {
                                if let Err(e) = serde_json::from_str::<serde_json::Value>(&s) {
                                    let preview: String = s.chars().take(40).collect::<String>();
                                    let ellipsis = if s.chars().count() > 40 { "..." } else { "" };
                                    bail!(
                                        "json(): argument is not valid JSON — got '{}{}' (use json_unchecked() to skip validation): {}",
                                        preview,
                                        ellipsis,
                                        e
                                    );
                                }
                            }
                            Value::Str(s)
                        }
                        Value::Array(_) | Value::Record { .. } => {
                            Value::Str(value_to_json(&val).to_string())
                        }
                        other => Value::Str(other.as_string()),
                    });
                }

                // `text(body)` / `html(body)` ship `body` verbatim to the wire
                // under `text/plain` / `text/html` (charset appended). The two
                // sentinel keys (`__jwc_content_type__`, `__jwc_body__`) are
                // recognised and stripped by `dispatch_route`, which forwards
                // the raw body bytes and declared content-type to the transport.
                if name.eq_ignore_ascii_case("text") {
                    if args.len() != 1 {
                        bail!("text(body) expects exactly 1 arg");
                    }
                    let body = self.eval_expr(&args[0], vars).await?.as_string();
                    return Ok(content_type_response(body, "text/plain"));
                }
                if name.eq_ignore_ascii_case("html") {
                    if args.len() != 1 {
                        bail!("html(body) expects exactly 1 arg");
                    }
                    let body = self.eval_expr(&args[0], vars).await?.as_string();
                    return Ok(content_type_response(body, "text/html"));
                }
                // `response(body, mime)` / `raw(body, mime)` — custom MIME
                // escape hatch. text-ish types get `; charset=utf-8` appended.
                if name.eq_ignore_ascii_case("response") || name.eq_ignore_ascii_case("raw") {
                    if args.len() != 2 {
                        bail!("response(body, mime) expects exactly 2 args");
                    }
                    let body = self.eval_expr(&args[0], vars).await?.as_string();
                    let mime = match self.eval_expr(&args[1], vars).await? {
                        Value::Str(s) => s,
                        other => bail!(
                            "response(body, mime): mime must be string, got {}",
                            other.type_name()
                        ),
                    };
                    return Ok(content_type_response(body, &mime));
                }

                if name.eq_ignore_ascii_case("created") {
                    if args.len() != 1 {
                        bail!("created(val) expects exactly 1 arg");
                    }
                    let val = self.eval_expr(&args[0], vars).await?;
                    let s = val.as_string();
                    let result = if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&s)
                    {
                        match doc.as_object_mut() {
                            Some(obj) => {
                                obj.insert("__jwc_status__".into(), json!(201));
                                doc.to_string()
                            }
                            None => format!(r#"{{"__jwc_status__":201,"data":{s}}}"#),
                        }
                    } else {
                        format!(r#"{{"__jwc_status__":201,"data":{s:?}}}"#)
                    };
                    return Ok(Value::Str(result));
                }

                // `ok(value?)` — explicit 200 response. Mirrors `created`:
                // an object body gets `status: 200` baked in, anything else is
                // wrapped as `{ "status": 200, "data": <value> }`.
                if name.eq_ignore_ascii_case("ok") {
                    let s = if let Some(arg) = args.first() {
                        self.eval_expr(arg, vars).await?.as_string()
                    } else {
                        return Ok(Value::Str(r#"{"__jwc_status__":200}"#.to_string()));
                    };
                    let result = if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&s)
                    {
                        match doc.as_object_mut() {
                            Some(obj) => {
                                obj.insert("__jwc_status__".into(), json!(200));
                                doc.to_string()
                            }
                            None => format!(r#"{{"__jwc_status__":200,"data":{s}}}"#),
                        }
                    } else {
                        format!(r#"{{"__jwc_status__":200,"data":{s:?}}}"#)
                    };
                    return Ok(Value::Str(result));
                }

                if name.eq_ignore_ascii_case("notFound") || name.eq_ignore_ascii_case("not_found") {
                    return Ok(Value::Str(
                        r#"{"__jwc_status__":404,"error":"Not Found"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("noContent") || name.eq_ignore_ascii_case("no_content")
                {
                    return Ok(Value::Str(r#"{"__jwc_status__":204}"#.to_string()));
                }

                if name.eq_ignore_ascii_case("internalError")
                    || name.eq_ignore_ascii_case("internal_error")
                {
                    let msg = if let Some(arg) = args.first() {
                        self.eval_expr(arg, vars).await?.as_string()
                    } else {
                        "Internal Server Error".to_string()
                    };
                    let escaped = msg.replace('"', "\\\"");
                    return Ok(Value::Str(format!(
                        r#"{{"__jwc_status__":500,"error":"{escaped}"}}"#
                    )));
                }

                if name.eq_ignore_ascii_case("badRequest")
                    || name.eq_ignore_ascii_case("bad_request")
                {
                    let msg = if let Some(arg) = args.first() {
                        self.eval_expr(arg, vars).await?.as_string()
                    } else {
                        "Bad Request".to_string()
                    };
                    let escaped = msg.replace('"', "\\\"");
                    return Ok(Value::Str(format!(
                        r#"{{"__jwc_status__":400,"error":"{escaped}"}}"#
                    )));
                }

                // `statusCode(code, body_or_headers?)` — set the HTTP status
                // for the response. For 3xx with an object 2nd arg, the
                // object's fields become response headers (so
                // `statusCode(302, { Location: url })` produces a real
                // browser-followed redirect). For other statuses the 2nd arg
                // is rendered as the response body.
                if name.eq_ignore_ascii_case("statusCode")
                    || name.eq_ignore_ascii_case("status_code")
                {
                    if args.is_empty() || args.len() > 2 {
                        bail!("statusCode(code, body_or_headers?) expects 1 or 2 args");
                    }
                    let status = match self.eval_expr(&args[0], vars).await? {
                        Value::Int(n) if (100..600).contains(&n) => n as u16,
                        Value::Int(n) => bail!("statusCode: invalid status {n}"),
                        other => bail!("statusCode: code must be int, got {}", other.type_name()),
                    };
                    let is_redirect = (300..400).contains(&status);
                    let body_val = if let Some(arg) = args.get(1) {
                        Some(self.eval_expr(arg, vars).await?)
                    } else {
                        None
                    };
                    if is_redirect {
                        if let Some(Value::Str(s)) = &body_val {
                            if let Ok(JsonValue::Object(map)) = serde_json::from_str::<JsonValue>(s)
                            {
                                let envelope = json!({
                                    "__jwc_status__": status,
                                    "__jwc_headers__": JsonValue::Object(map),
                                    "__jwc_content_type__": "text/html; charset=utf-8",
                                    "__jwc_body__": "",
                                });
                                return Ok(Value::Str(envelope.to_string()));
                            }
                        }
                    }
                    let body_str = body_val.map(|v| v.as_string()).unwrap_or_default();
                    if let Ok(mut doc) = serde_json::from_str::<JsonValue>(&body_str) {
                        if let Some(obj) = doc.as_object_mut() {
                            obj.insert("__jwc_status__".into(), json!(status));
                            return Ok(Value::Str(doc.to_string()));
                        }
                    }
                    let envelope = json!({
                        "__jwc_status__": status,
                        "__jwc_content_type__": "text/plain; charset=utf-8",
                        "__jwc_body__": body_str,
                    });
                    return Ok(Value::Str(envelope.to_string()));
                }
                // ──────────────────────────────────────────────────────────

                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval_expr(arg, vars).await?);
                }
                Ok(self
                    .call_function(name, values)
                    .await?
                    .unwrap_or(Value::Void))
            }
            Expr::Await(inner) => self.eval_expr(inner, vars).await,
            Expr::Not(inner) => match self.eval_expr(inner, vars).await? {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => bail!(
                    "Unsupported unary '!' for {} (only bool is allowed)",
                    other.type_name()
                ),
            },
            Expr::ObjectLit(fields) => {
                // **Phase 1 [1.0-blocker]** — static-shape object literal emits
                // a typed `Value::Record` (keys are AST `String` literals, so
                // shape is known at construction). Field access then skips the
                // JSON parse round-trip entirely on this path.
                let mut pairs: Vec<(String, Value)> = Vec::with_capacity(fields.len());
                for (key, expr) in fields {
                    let value = self.eval_expr(expr, vars).await?;
                    pairs.push((key.clone(), value));
                }
                Ok(Value::record_from_pairs(pairs))
            }
            Expr::ArrayLit(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.eval_expr(item, vars).await?);
                }
                Ok(Value::Array(out))
            }
            Expr::Add(left, right) => {
                let left = self.eval_expr(left, vars).await?;
                let right = self.eval_expr(right, vars).await?;
                match (left, right) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
                    (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
                    (Value::Str(a), b) => Ok(Value::Str(format!("{a}{}", b.as_string()))),
                    (a, Value::Str(b)) => Ok(Value::Str(format!("{}{b}", a.as_string()))),
                    (a, b) => bail!(
                        "Unsupported '+' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Sub(left, right) => {
                self.eval_numeric_bin(left, right, vars, |a, b| a - b, |a, b| a - b)
                    .await
            }
            Expr::Mul(left, right) => {
                self.eval_numeric_bin(left, right, vars, |a, b| a * b, |a, b| a * b)
                    .await
            }
            Expr::Div(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("division by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
                    (Value::Float(_), Value::Float(0.0)) => bail!("division by zero"),
                    (Value::Int(_), Value::Float(0.0)) => bail!("division by zero"),
                    (Value::Float(_), Value::Int(0)) => bail!("division by zero"),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 / b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / b as f64)),
                    (a, b) => bail!(
                        "Unsupported '/' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Mod(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("modulo by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                    (Value::Float(_), Value::Float(0.0)) => bail!("modulo by zero"),
                    (Value::Int(_), Value::Float(0.0)) => bail!("modulo by zero"),
                    (Value::Float(_), Value::Int(0)) => bail!("modulo by zero"),
                    (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
                    (Value::Int(a), Value::Float(b)) => Ok(Value::Float((a as f64) % b)),
                    (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % (b as f64))),
                    (a, b) => bail!(
                        "Unsupported '%' for {} and {}",
                        a.type_name(),
                        b.type_name()
                    ),
                }
            }
            Expr::Neg(inner) => {
                let value = self.eval_expr(inner, vars).await?;
                match value {
                    Value::Int(v) => Ok(Value::Int(-v)),
                    Value::Float(v) => Ok(Value::Float(-v)),
                    other => bail!("Unsupported unary '-' for {}", other.type_name()),
                }
            }
            Expr::Eq(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                Ok(Value::Bool(l == r))
            }
            Expr::Neq(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                let r = self.eval_expr(right, vars).await?;
                Ok(Value::Bool(l != r))
            }
            Expr::Lt(left, right) => self.eval_numeric_cmp(left, right, vars, |a, b| a < b).await,
            Expr::Lte(left, right) => {
                self.eval_numeric_cmp(left, right, vars, |a, b| a <= b)
                    .await
            }
            Expr::Gt(left, right) => self.eval_numeric_cmp(left, right, vars, |a, b| a > b).await,
            Expr::Gte(left, right) => {
                self.eval_numeric_cmp(left, right, vars, |a, b| a >= b)
                    .await
            }
            Expr::And(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                match l {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => {
                        let r = self.eval_expr(right, vars).await?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'and' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'and' expects bool, got {}", other.type_name()),
                }
            }
            Expr::Or(left, right) => {
                let l = self.eval_expr(left, vars).await?;
                match l {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => {
                        let r = self.eval_expr(right, vars).await?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'or' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'or' expects bool, got {}", other.type_name()),
                }
            }
        }
    }

    pub(super) async fn eval_numeric_bin<FInt, FFloat>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        int_func: FInt,
        float_func: FFloat,
    ) -> Result<Value>
    where
        FInt: Fn(i64, i64) -> i64,
        FFloat: Fn(f64, f64) -> f64,
    {
        let l = self.eval_expr(left, vars).await?;
        let r = self.eval_expr(right, vars).await?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(int_func(a, b))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(float_func(a, b))),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(float_func(a as f64, b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(float_func(a, b as f64))),
            (a, b) => bail!(
                "Unsupported numeric op for {} and {}",
                a.type_name(),
                b.type_name()
            ),
        }
    }

    pub(super) async fn eval_numeric_cmp<F>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        func: F,
    ) -> Result<Value>
    where
        F: Fn(f64, f64) -> bool,
    {
        let l = self.eval_expr(left, vars).await?;
        let r = self.eval_expr(right, vars).await?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(func(a as f64, b as f64))),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(func(a, b))),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Bool(func(a as f64, b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Bool(func(a, b as f64))),
            (a, b) => bail!(
                "Unsupported comparison for {} and {}",
                a.type_name(),
                b.type_name()
            ),
        }
    }
}
