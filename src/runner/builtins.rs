//! Built-in function evaluators for the JWC interpreter `Vm`.
//!
//! These `eval_*_call` methods were split out of `runner/mod.rs` to keep the
//! main interpreter file manageable. They are dispatched from the
//! `Expr::Call { name, args }` arm of `Vm::eval_expr` in the parent module,
//! so each is `pub(super)` to remain visible there. This is a pure code move:
//! behaviour is identical to when these lived inside `runner/mod.rs`.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value as JsonValue;
use tokio_postgres::types::ToSql;

use crate::ast::Expr;

// Pull in `Vm`, the `Value`/`Flow` enums, `WS_HANDLE`, the `engine` import
// alias, and the private free helper functions (`http_client`,
// `apply_headers_reqwest`, `boxed_params_to_refs`, etc.) defined in the parent
// `runner` module. A child module may name private items of its ancestors.
use super::*;

impl<'a> Vm<'a> {
    pub(super) async fn eval_path_param_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("path_param(name) expects exactly 1 arg");
        }

        let name = self.eval_expr(&args[0], vars).await?;
        let name = match name {
            Value::Str(v) => v,
            other => bail!(
                "path_param(name): name must be string, got {}",
                other.type_name()
            ),
        };

        let params = self
            .current_path_params
            .as_ref()
            .ok_or_else(|| anyhow!("path_param() can only be used inside route execution"))?;

        match params.get(&name) {
            Some(v) => Ok(Value::Str(v.clone())),
            None => Ok(Value::Null),
        }
    }

    pub(super) async fn eval_query_param_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("query_param(name[, default]) expects 1 or 2 args");
        }

        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "query_param(name): name must be string, got {}",
                other.type_name()
            ),
        };

        let value = self
            .current_query_params
            .as_ref()
            .and_then(|q| q.get(&name).cloned());

        match (value, args.get(1)) {
            (Some(v), _) => Ok(Value::Str(v)),
            (None, Some(default_expr)) => self.eval_expr(default_expr, vars).await,
            (None, None) => Ok(Value::Null),
        }
    }

    pub(super) async fn eval_http_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("http_get(url[, headers_json]) expects 1 or 2 args");
        }
        let url = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "http_get(url): url must be string, got {}",
                other.type_name()
            ),
        };

        let headers_json = if let Some(arg) = args.get(1) {
            match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => bail!(
                    "http_get(url, headers): headers must be json string or null, got {}",
                    other.type_name()
                ),
            }
        } else {
            None
        };

        let mut req = http_client().get(&url);
        if let Some(json_str) = headers_json {
            req = apply_headers_reqwest(req, &json_str)?;
        }

        let response = req
            .send()
            .await
            .map_err(|e| anyhow!("http_get({url}) failed: {e}"))?;
        Ok(Value::Str(http_response_to_json_string(response).await?))
    }

    pub(super) async fn eval_http_post_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 3 {
            bail!("http_post(url[, body[, headers_json]]) expects 1, 2 or 3 args");
        }
        let url = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "http_post(url): url must be string, got {}",
                other.type_name()
            ),
        };

        let body_str = match args.get(1) {
            None => None,
            Some(arg) => match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => Some(other.as_string()),
            },
        };

        let headers_json = if let Some(arg) = args.get(2) {
            match self.eval_expr(arg, vars).await? {
                Value::Str(s) => Some(s),
                Value::Null => None,
                other => bail!(
                    "http_post(url, body, headers): headers must be json string or null, got {}",
                    other.type_name()
                ),
            }
        } else {
            None
        };

        let mut req = http_client().post(&url);
        if let Some(json_str) = headers_json {
            req = apply_headers_reqwest(req, &json_str)?;
        }

        let body_is_json = body_str
            .as_deref()
            .map(|s| serde_json::from_str::<JsonValue>(s).is_ok())
            .unwrap_or(false);
        if body_is_json {
            req = req.header("content-type", "application/json");
        }

        let response = match body_str {
            Some(b) => req.body(b).send().await,
            None => req.send().await,
        }
        .map_err(|e| anyhow!("http_post({url}) failed: {e}"))?;

        Ok(Value::Str(http_response_to_json_string(response).await?))
    }

    pub(super) async fn eval_jwt_sign_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("jwt_sign(payload_json, secret) expects exactly 2 args");
        }
        let payload = self.eval_expr(&args[0], vars).await?.as_string();
        let secret = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_sign(payload, secret): secret must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::jwt::sign_hs256(&payload, &secret)?))
    }

    pub(super) async fn eval_jwt_verify_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("jwt_verify(token, secret) expects exactly 2 args");
        }
        let token = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify(token, secret): token must be string, got {}",
                other.type_name()
            ),
        };
        let secret = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify(token, secret): secret must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::jwt::verify_hs256(&token, &secret)?))
    }

    pub(super) async fn eval_ws_send_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("ws_send(msg) expects exactly 1 arg");
        }
        let msg = self.eval_expr(&args[0], vars).await?.as_string();
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_send(): only valid inside a WS route handler"),
        };
        let guard = cell.lock().await;
        let sent = match guard.as_ref() {
            Some(w) => w.tx.send(msg).is_ok(),
            None => bail!("ws_send(): only valid inside a WS route handler"),
        };
        if sent {
            Ok(Value::Void)
        } else {
            bail!("ws_send: client disconnected")
        }
    }

    pub(super) async fn eval_ws_recv_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("ws_recv() expects no args");
        }
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_recv(): only valid inside a WS route handler"),
        };
        let mut guard = cell.lock().await;
        let result = match guard.as_mut() {
            Some(w) => w.rx.recv().await,
            None => bail!("ws_recv(): only valid inside a WS route handler"),
        };
        match result {
            None => Ok(Value::Null), // client closed
            Some(text) => Ok(Value::Str(text)),
        }
    }

    pub(super) async fn eval_ws_close_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("ws_close() expects no args");
        }
        let cell = match WS_HANDLE.try_with(|c| c.clone()) {
            Ok(c) => c,
            Err(_) => bail!("ws_close(): only valid inside a WS route handler"),
        };
        cell.lock().await.take();
        Ok(Value::Void)
    }

    pub(super) async fn eval_hash_password_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("hash_password(pwd) expects exactly 1 arg");
        }
        let pwd = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "hash_password(pwd): pwd must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Str(crate::password::hash_password(&pwd)?))
    }

    pub(super) async fn eval_verify_password_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("verify_password(pwd, stored_hash) expects exactly 2 args");
        }
        let pwd = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "verify_password(pwd, hash): pwd must be string, got {}",
                other.type_name()
            ),
        };
        let stored = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "verify_password(pwd, hash): hash must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Bool(crate::password::verify_password(
            &pwd, &stored,
        )?))
    }

    pub(super) async fn eval_cache_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("cache_get(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_get(key): key must be string, got {}",
                other.type_name()
            ),
        };
        match crate::cache::get(&key) {
            Some(v) => Ok(Value::Str(v)),
            None => Ok(Value::Null),
        }
    }

    pub(super) async fn eval_cache_set_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("cache_set(key, value, ttl_secs) expects exactly 3 args");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_set(key, value, ttl_secs): key must be string, got {}",
                other.type_name()
            ),
        };
        let value = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_set(key, value, ttl_secs): value must be string, got {}",
                other.type_name()
            ),
        };
        let ttl = match self.eval_expr(&args[2], vars).await? {
            Value::Int(n) if n >= 0 => n as u64,
            Value::Int(n) => {
                bail!("cache_set(key, value, ttl_secs): ttl_secs must be >= 0, got {n}")
            }
            other => bail!(
                "cache_set(key, value, ttl_secs): ttl_secs must be int, got {}",
                other.type_name()
            ),
        };
        crate::cache::set(&key, &value, ttl);
        Ok(Value::Void)
    }

    pub(super) async fn eval_cache_del_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("cache_del(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "cache_del(key): key must be string, got {}",
                other.type_name()
            ),
        };
        crate::cache::del(&key);
        Ok(Value::Void)
    }

    pub(super) async fn eval_cache_clear_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("cache_clear() expects no args");
        }
        crate::cache::clear();
        Ok(Value::Void)
    }

    pub(super) async fn eval_send_email_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("send_email(to, subject, body_html) expects exactly 3 args");
        }
        let to = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): to must be string, got {}",
                other.type_name()
            ),
        };
        let subject = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): subject must be string, got {}",
                other.type_name()
            ),
        };
        let body_html = match self.eval_expr(&args[2], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "send_email(to, subject, body_html): body_html must be string, got {}",
                other.type_name()
            ),
        };
        crate::email::send_email(&to, &subject, &body_html)?;
        Ok(Value::Void)
    }

    pub(super) async fn eval_register_job_handler_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("register_job_handler(name, handler_fn_name) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "register_job_handler(name, handler_fn_name): name must be string, got {}",
                other.type_name()
            ),
        };
        let handler = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "register_job_handler(name, handler_fn_name): handler_fn_name must be string, got {}",
                other.type_name()
            ),
        };
        if !self.functions.contains_key(&handler.to_lowercase()) {
            bail!(
                "error[E010]: register_job_handler: handler function '{}' is not defined in this program",
                handler
            );
        }
        crate::queue::register_handler(&name, &handler);
        Ok(Value::Void)
    }

    pub(super) async fn eval_enqueue_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("enqueue(name, payload_json) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue(name, payload_json): name must be string, got {}",
                other.type_name()
            ),
        };
        let payload = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue(name, payload_json): payload_json must be string, got {}",
                other.type_name()
            ),
        };
        crate::queue::enqueue(&name, &payload);
        Ok(Value::Void)
    }

    pub(super) async fn eval_enqueue_urgent_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("enqueue_urgent(name, payload_json) expects exactly 2 args");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue_urgent(name, payload_json): name must be string, got {}",
                other.type_name()
            ),
        };
        let payload = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "enqueue_urgent(name, payload_json): payload_json must be string, got {}",
                other.type_name()
            ),
        };
        crate::queue::enqueue_urgent(&name, &payload);
        Ok(Value::Void)
    }

    pub(super) async fn eval_job_count_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("job_count() expects no args");
        }
        Ok(Value::Int(crate::queue::pending_count() as i64))
    }

    pub(super) async fn eval_dlq_count_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("dlq_count() expects no args");
        }
        Ok(Value::Int(crate::queue::dlq_count() as i64))
    }

    /// Drain every permanently-failed job from the queue's dead-letter
    /// queue and return them as a JSON array. Each entry is
    /// `{name, payload, attempts, last_error}`. After this returns the
    /// DLQ is empty, so user code must persist anything it wants to keep.
    pub(super) async fn eval_dlq_drain_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("dlq_drain() expects no args");
        }
        let entries = crate::queue::dlq_drain();
        let arr: Vec<JsonValue> = entries
            .into_iter()
            .map(|f| {
                serde_json::json!({
                    "name": f.job.name,
                    "payload": f.job.payload,
                    "attempts": f.job.attempts,
                    "last_error": f.last_error,
                })
            })
            .collect();
        Ok(Value::Str(JsonValue::Array(arr).to_string()))
    }

    pub(super) async fn eval_header_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("header(name) expects exactly 1 arg");
        }
        let name = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "header(name): name must be string, got {}",
                other.type_name()
            ),
        };
        let key = name.to_ascii_lowercase();
        let value = self
            .current_headers
            .as_ref()
            .and_then(|h| h.get(&key).cloned());
        match value {
            Some(v) => Ok(Value::Str(v)),
            None => Ok(Value::Null),
        }
    }

    pub(super) async fn eval_context_get_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("context(key) expects exactly 1 arg");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "context(key): key must be string, got {}",
                other.type_name()
            ),
        };
        Ok(self
            .request_context
            .get(&key)
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub(super) async fn eval_context_set_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("setContext(key, value) expects exactly 2 args");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "setContext(key, value): key must be string, got {}",
                other.type_name()
            ),
        };
        let value = self.eval_expr(&args[1], vars).await?;
        self.request_context.insert(key, value);
        Ok(Value::Void)
    }

    pub(super) async fn eval_raw_sql_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 2 {
            bail!("raw_sql(sql[, params_json]) expects 1 or 2 args");
        }
        let sql = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "raw_sql(sql, ...): sql must be string, got {}",
                other.type_name()
            ),
        };

        let mut boxed_params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
        if let Some(arg) = args.get(1) {
            let raw = match self.eval_expr(arg, vars).await? {
                Value::Str(s) => s,
                Value::Null => "[]".to_string(),
                other => bail!(
                    "raw_sql(sql, params): params must be a JSON array string, got {}",
                    other.type_name()
                ),
            };
            let parsed: JsonValue = serde_json::from_str(&raw)
                .map_err(|_| anyhow!("raw_sql: params must be a JSON array, got invalid json"))?;
            let arr = parsed
                .as_array()
                .ok_or_else(|| anyhow!("raw_sql: params must be a JSON array"))?;
            for v in arr {
                boxed_params.push(json_value_to_sql_param(v));
            }
        }

        let param_refs = boxed_params_to_refs(&boxed_params);
        let lowered = sql.trim_start().to_ascii_lowercase();
        // For SELECT-shaped queries return the first column as text; everything
        // else routes through `exec` to also support write statements.
        if lowered.starts_with("select") || lowered.starts_with("with") {
            let result = engine::query_text(&sql, &param_refs).await?;
            if result.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(Value::Str(result))
            }
        } else {
            let affected = engine::exec(&sql, &param_refs).await?;
            engine::invalidate_result_cache()?;
            Ok(Value::Int(affected as i64))
        }
    }

    pub(super) async fn eval_set_connection_string_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() > 1 {
            bail!(
                "setConnectionString() expects 0 or 1 args (use a URL string, an \
                 object literal, or no args to pull from the env)"
            );
        }

        // Form 1: no args → read from env. `.env` was already loaded by the
        // CLI before `main()` ran, so PG_* / DATABASE_URL should be present.
        if args.is_empty() {
            if let Ok(url) = std::env::var("DATABASE_URL") {
                // SAFETY: setter is only called from single-threaded main startup.
                std::env::set_var("DATABASE_URL", url);
                return Ok(Value::Void);
            }
            if let Some(url) = assemble_url_from_pg_env() {
                std::env::set_var("DATABASE_URL", url);
                return Ok(Value::Void);
            }
            bail!(
                "setConnectionString(): no DATABASE_URL and no PG_HOST/PORT/USER/PASSWORD/DATABASE in env"
            );
        }

        let value = self.eval_expr(&args[0], vars).await?;
        let url_string = match value {
            Value::Str(s) => connection_string_from_arg(&s)?,
            other => bail!(
                "setConnectionString(arg): arg must be a URL string or an object literal, got {}",
                other.type_name()
            ),
        };
        std::env::set_var("DATABASE_URL", url_string);
        Ok(Value::Void)
    }

    pub(super) async fn eval_db_query_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_query(sql) expects exactly 1 arg");
        }

        let sql = self.eval_expr(&args[0], vars).await?;
        let sql = match sql {
            Value::Str(v) => v,
            other => bail!(
                "db_query(sql): sql must be string, got {}",
                other.type_name()
            ),
        };

        let database_url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("JWC_DATABASE_URL"))
            .map_err(|_| anyhow!("DATABASE_URL (or JWC_DATABASE_URL) is required for db_query"))?;

        engine::init_engine(&database_url)?;
        let value = engine::query_text(&sql, &[]).await?;
        if value.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value))
        }
    }

    pub(super) async fn eval_request_body_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("request_body() expects no args");
        }

        // Prefer the body injected by run_request().await, fall back to env var for legacy use
        let body = self
            .request_body
            .clone()
            .or_else(|| std::env::var("JWC_REQUEST_BODY").ok())
            .unwrap_or_else(|| "null".to_string());
        Ok(Value::Str(body))
    }

    /// `length(x)` — characters for a string, element count for a JSON array
    /// carried as a string, key count for a JSON object, 0 for null. Falls
    /// back to a friendly error for other shapes.
    pub(super) async fn eval_length_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("length(x) expects exactly 1 arg");
        }
        match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&s) {
                    if let Some(arr) = parsed.as_array() {
                        return Ok(Value::Int(arr.len() as i64));
                    }
                    if let Some(obj) = parsed.as_object() {
                        return Ok(Value::Int(obj.len() as i64));
                    }
                }
                Ok(Value::Int(s.chars().count() as i64))
            }
            Value::Null => Ok(Value::Int(0)),
            other => bail!("length(x): unsupported type {}", other.type_name()),
        }
    }

    pub(super) async fn eval_string_call<F: Fn(&str) -> String>(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        name: &str,
        op: F,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("{name}(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("{name}(s): s must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(op(&s)))
    }

    pub(super) async fn eval_two_string_bool_call<F: Fn(&str, &str) -> bool>(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        name: &str,
        op: F,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("{name}(s, p) expects exactly 2 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Bool(false)),
            other => bail!(
                "{name}: first arg must be string, got {}",
                other.type_name()
            ),
        };
        let p = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "{name}: second arg must be string, got {}",
                other.type_name()
            ),
        };
        Ok(Value::Bool(op(&s, &p)))
    }

    /// `contains(s, sub)` — substring check for strings, element check for
    /// JSON arrays carried as strings, key check for JSON objects.
    pub(super) async fn eval_contains_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("contains(haystack, needle) expects exactly 2 args");
        }
        let haystack = self.eval_expr(&args[0], vars).await?;
        let needle = self.eval_expr(&args[1], vars).await?;

        match haystack {
            Value::Null => Ok(Value::Bool(false)),
            Value::Str(s) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&s) {
                    if let Some(arr) = parsed.as_array() {
                        let target = value_to_json_smart(&needle);
                        return Ok(Value::Bool(arr.iter().any(|v| v == &target)));
                    }
                    if let Some(obj) = parsed.as_object() {
                        if let Value::Str(key) = needle {
                            return Ok(Value::Bool(obj.contains_key(&key)));
                        }
                    }
                }
                match needle {
                    Value::Str(sub) => Ok(Value::Bool(s.contains(&sub))),
                    other => bail!(
                        "contains: needle must be string for string haystack, got {}",
                        other.type_name()
                    ),
                }
            }
            other => bail!(
                "contains: haystack must be string/array, got {}",
                other.type_name()
            ),
        }
    }

    pub(super) async fn eval_replace_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("replace(s, from, to) expects exactly 3 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("replace: s must be string, got {}", other.type_name()),
        };
        let from = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("replace: from must be string, got {}", other.type_name()),
        };
        let to = match self.eval_expr(&args[2], vars).await? {
            Value::Str(s) => s,
            other => bail!("replace: to must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(s.replace(&from, &to)))
    }

    pub(super) async fn eval_split_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("split(s, sep) expects exactly 2 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Str("[]".to_string())),
            other => bail!("split: s must be string, got {}", other.type_name()),
        };
        let sep = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("split: sep must be string, got {}", other.type_name()),
        };
        let pieces: Vec<JsonValue> = s
            .split(&sep)
            .map(|p| JsonValue::String(p.to_string()))
            .collect();
        Ok(Value::Str(JsonValue::Array(pieces).to_string()))
    }

    pub(super) async fn eval_first_or_last_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        first: bool,
    ) -> Result<Value> {
        let name = if first { "first" } else { "last" };
        if args.len() != 1 {
            bail!("{name}(xs) expects exactly 1 arg");
        }
        let raw = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!(
                "{name}: xs must be array (JSON string), got {}",
                other.type_name()
            ),
        };
        let parsed: JsonValue =
            serde_json::from_str(&raw).map_err(|_| anyhow!("{name}: xs is not valid JSON"))?;
        let arr = parsed
            .as_array()
            .ok_or_else(|| anyhow!("{name}: xs is not a JSON array"))?;
        let elem = if first { arr.first() } else { arr.last() };
        Ok(elem.map(json_to_value).unwrap_or(Value::Null))
    }

    pub(super) async fn eval_json_parse_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("json_parse(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("json_parse: s must be string, got {}", other.type_name()),
        };
        let parsed: JsonValue =
            serde_json::from_str(&s).map_err(|e| anyhow!("json_parse: invalid JSON: {e}"))?;
        Ok(json_to_value(&parsed))
    }

    pub(super) async fn eval_json_stringify_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("json_stringify(v) expects exactly 1 arg");
        }
        let v = self.eval_expr(&args[0], vars).await?;
        Ok(Value::Str(value_to_json_smart(&v).to_string()))
    }

    pub(super) async fn eval_uuid_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("uuid() expects no args");
        }
        // RFC 4122 v4 — random, includes proper version/variant bits, no
        // sub-nanosecond collisions like the old `SystemTime::now()` hack.
        Ok(Value::Str(uuid::Uuid::new_v4().to_string()))
    }

    pub(super) async fn eval_set_json_field_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("set_json_field(obj_json, field, value) expects 3 args");
        }

        let source = match self.eval_expr(&args[0], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: first arg must be string json, got {}",
                other.type_name()
            ),
        };
        let field = match self.eval_expr(&args[1], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: field must be string, got {}",
                other.type_name()
            ),
        };
        let value = self.eval_expr(&args[2], vars).await?;

        let mut json_value: JsonValue = serde_json::from_str(&source)
            .with_context(|| "set_json_field: invalid json in first arg")?;
        let object = json_value
            .as_object_mut()
            .ok_or_else(|| anyhow!("set_json_field: first arg must be a json object"))?;

        object.insert(field, value_to_json(&value));

        Ok(Value::Str(json_value.to_string()))
    }
}
