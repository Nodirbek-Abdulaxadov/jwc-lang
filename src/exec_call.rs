//! Call dispatch: builtins, free functions and service methods.
//!
//! The builtin surface is builtins.md. There is no `call_builtin` table —
//! the match below *is* the table, and its arms are grouped by namespace so
//! a reader can find `date.*` without grepping.

use crate::ast::*;
use crate::exec::{Abort, Exec, Flow, Thrown, Vm};
use crate::value::Value;
use anyhow::anyhow;
use base64::Engine as _;

/// INCR then EXPIRE, in one round trip, for `redis.rate_limit`.
///
/// One script rather than two calls because the two-call form has a
/// window: if the process dies between them the counter is left with no
/// TTL, never resets, and that key is blocked forever. The `n == 1` guard
/// is what makes it a fixed window instead of one that keeps sliding
/// forward under sustained traffic and so never expires.
const RATE_LIMIT: &str = "local n = redis.call('INCR', KEYS[1]) \
                          if n == 1 then redis.call('EXPIRE', KEYS[1], ARGV[1]) end \
                          return n";

fn fault(msg: impl Into<String>) -> Abort {
    Abort::Fault(anyhow!(msg.into()))
}

fn text(v: &Value) -> String {
    match v {
        Value::Text(s) | Value::Numeric(s) | Value::Timestamptz(s) | Value::Interval(s) => {
            s.clone()
        }
        Value::Int(n) | Value::Bigint(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => {
            let mut s = String::new();
            other.write_json(&mut s);
            s
        }
    }
}

impl<'a> Vm<'a> {
    pub(super) async fn call(&mut self, callee: &Expr, args: &[Expr]) -> Exec<Value> {
        let Some(path) = path_of(callee) else {
            return Err(fault("this call target is not a name"));
        };

        // Aggregates only exist inside a query, which the SQL builder
        // handles; reaching one here means the query compiler is needed.
        if matches!(path.as_str(), "count" | "sum" | "min" | "max" | "avg") {
            return Err(fault(format!(
                "`{path}(...)` needs the query compiler (v0.25.0)"
            )));
        }

        // `enum(E, x)` takes a type name, so its first argument is not a
        // value (builtins.md §2).
        if path == "enum" {
            let v = self.eval(&args[1]).await?;
            return Ok(if v.is_null() {
                Value::Null
            } else {
                Value::Text(text(&v))
            });
        }

        let mut vals = Vec::new();
        for a in args {
            vals.push(self.eval(a).await?);
        }

        // `raw` runs SQL, so it cannot live in the synchronous table.
        if path == "raw" {
            return self.run_raw(args, &vals).await;
        }

        // `redis.*` talks to a server, for the same reason.
        if let Some(name) = path.strip_prefix("redis.") {
            return self.redis_call(name, &vals).await;
        }

        if let Some(v) = self.builtin(&path, &vals)? {
            return Ok(v);
        }
        self.user_function(&path, vals).await
    }

    async fn user_function(&mut self, path: &str, args: Vec<Value>) -> Exec<Value> {
        let Some(f) = self.program.functions.get(path).cloned() else {
            return Err(fault(format!("unknown function `{path}`")));
        };
        let saved = self.enter_function();
        for (i, p) in f.params.iter().enumerate() {
            let v = args.get(i).cloned().unwrap_or(Value::Null);
            self.bind_param(&p.name.name, v);
        }
        let r = Box::pin(self.run_body(&f.body)).await;
        self.leave_function(saved);
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::Null),
        }
    }

    /// writes.md §6 — hand-written SQL, with `{}` bound in order.
    ///
    /// The placeholders are rewritten to `$1…$n` and the arguments are
    /// bound. Nothing is interpolated: the SQL is a literal the checker
    /// already counted the holes in, so there is no path by which a
    /// caller's value reaches the statement as text.
    async fn run_raw(&mut self, args: &[Expr], vals: &[Value]) -> Exec<Value> {
        let Some(ExprKind::Str(template)) = args.first().map(|a| &*a.kind) else {
            return Err(fault("`raw()`'s SQL must be a literal"));
        };
        let (sql, n) = rewrite_placeholders(template);

        let binds: Vec<Option<String>> = vals.iter().skip(1).map(|v| v.to_bind()).collect();
        if binds.len() != n {
            return Err(fault(format!(
                "`raw()` has {n} placeholder(s) and {} argument(s)",
                binds.len()
            )));
        }
        // Wrapped the same way every other query is, so a `raw` result is
        // the same kind of value as a compiled one.
        let wrapped = format!("SELECT coalesce(json_agg(q), '[]'::json)::text FROM ({sql}) q");
        let text = crate::db::run(&wrapped, &binds, crate::sql::Shape::Rows)
            .await
            .map_err(crate::exec::map_db_error)?;
        Ok(Value::Raw(text.unwrap_or_else(|| "[]".into())))
    }

    /// The `redis` package surface (builtins.md §8), over the driver in
    /// [`crate::redis_engine`].
    ///
    /// These were stubs: `enabled` answered `false`, `rate_limit` answered
    /// **`true`**, and `get` / `set` / `del` / `incr` / `expire` were not
    /// there at all, so a program using them typechecked clean and then
    /// faulted with `unknown function` on every request. The
    /// `rate_limit` stub was the dangerous one — a limiter written against
    /// the documented API admitted every request, and nothing anywhere
    /// said so.
    ///
    /// Without a reachable server they raise. Answering anyway is what
    /// the stub did, and for a rate limiter "no server" must never read as
    /// "allowed".
    async fn redis_call(&mut self, name: &str, a: &[Value]) -> Exec<Value> {
        use crate::redis_engine as r;
        let s = |i: usize| text(a.get(i).unwrap_or(&Value::Null));
        let n = |i: usize| a.get(i).and_then(|v| v.as_i64()).unwrap_or(0);

        if name == "enabled" {
            return Ok(Value::Bool(r::is_enabled()));
        }
        if !r::is_enabled() {
            return Err(fault(format!(
                "`redis.{name}(...)` needs a Redis server: set `JWC_REDIS_URL` \
                 and build with `--features redis`. `redis.enabled()` is what \
                 to branch on when the call is optional."
            )));
        }

        let out = match name {
            "get" => r::get(&s(0))
                .await
                .map(|v| v.map(Value::Text).unwrap_or(Value::Null)),
            // A negative TTL is not "expire in the past" — it is a caller
            // mistake, and 0 already means "no expiry" (both backends read
            // it that way). Clamping keeps a sign error from deleting the
            // key it was meant to write.
            "set" => r::set(&s(0), &s(1), n(2).max(0) as u64)
                .await
                .map(|()| Value::Bool(true)),
            "del" => r::del(&s(0)).await.map(Value::Int),
            "incr" => r::incr(&s(0)).await.map(Value::Bigint),
            "expire" => r::expire(&s(0), n(1)).await.map(Value::Bool),
            "rate_limit" => {
                let limit = n(1);
                let window = n(2);
                r::eval(RATE_LIMIT, &[s(0)], &[window.to_string()])
                    .await
                    .map(|hits| {
                        let hits: i64 = hits.and_then(|h| h.parse().ok()).unwrap_or(i64::MAX);
                        Value::Bool(hits <= limit)
                    })
            }
            _ => {
                return Err(fault(format!(
                    "unknown function `redis.{name}`. The package provides \
                     get, set, del, incr, expire, rate_limit and enabled \
                     (builtins.md §8)."
                )))
            }
        };
        out.map_err(|e| fault(format!("redis.{name}: {e:#}")))
    }

    /// Returns `None` when the path is not a builtin, so the caller can try
    /// user functions.
    fn builtin(&mut self, path: &str, a: &[Value]) -> Exec<Option<Value>> {
        let arg = |i: usize| a.get(i).cloned().unwrap_or(Value::Null);
        let s = |i: usize| text(&arg(i));
        let n = |i: usize| arg(i).as_i64().unwrap_or(0);

        Ok(Some(match path {
            // ---- debug (tooling.md §3)
            //
            // Returns its argument unchanged, so wrapping a subexpression in
            // it changes nothing but what is printed. Outside `--dev` it
            // prints nothing at all rather than erroring: a debug statement
            // that survived review should not be what takes an endpoint
            // down.
            "debug.dump" => {
                if crate::exec::dev_mode() {
                    eprintln!("[dump] {}", arg(0).debug_text());
                }
                arg(0)
            }

            // ---- responses (routing.md §6.1)
            "json" => self.respond(200, &arg(0)),
            "created" => self.respond(201, &arg(0)),
            "accepted" => self.respond(202, &arg(0)),
            "noContent" => self.respond_empty(204),
            "badRequest" => self.respond(400, &arg(0)),
            "unauthorized" => self.respond_message(401, &s(0)),
            "forbidden" => self.respond_message(403, &s(0)),
            "notFound" => self.respond_message(404, &s(0)),
            "conflict" => self.respond_message(409, &s(0)),
            "tooManyRequests" => self.respond_message(429, &s(0)),
            "internalError" => self.respond_message(500, "internal_error"),
            "statusCode" => {
                let code = n(0) as u16;
                self.respond(code, &arg(1))
            }
            "redirect" => {
                let code = n(0) as u16;
                let mut r = self.respond_empty(code);
                if let Value::Response { headers, .. } = &mut r {
                    headers.push(("location".into(), s(1)));
                }
                r
            }

            // ---- env and coercions (types.md §7.2)
            "env" => match std::env::var(s(0)) {
                Ok(v) => Value::Text(v),
                Err(_) => Value::Null,
            },
            "int" | "bigint" => {
                let raw = s(0);
                let parsed: Option<i64> = raw.trim().parse().ok();
                match parsed {
                    Some(v) if path == "int" => Value::Int(v),
                    Some(v) => Value::Bigint(v),
                    // The failure class depends on where the value came
                    // from; the checker decided that statically, and the
                    // runtime raises the client-facing one because every
                    // reachable call site here is request-shaped.
                    None => {
                        return Err(Abort::Thrown(Thrown {
                            error: "BadRequest".into(),
                            args: vec![Value::Text(format!("`{raw}` is not a number"))],
                        }))
                    }
                }
            }
            "numeric" => Value::Numeric(s(0)),
            "boolean" => Value::Bool(matches!(s(0).as_str(), "true" | "1")),
            "uuid" | "timestamptz" => Value::Text(s(0)),

            // ---- date (builtins.md §3)
            "date.now" => Value::Timestamptz(
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            ),
            "date.today" => Value::Text(chrono::Utc::now().date_naive().to_string()),
            "date.days" => Value::Interval(format!("P{}D", n(0))),
            "date.hours" => Value::Interval(format!("PT{}H", n(0))),
            "date.minutes" => Value::Interval(format!("PT{}M", n(0))),
            "date.seconds" => Value::Interval(format!("PT{}S", n(0))),
            "date.parse" => Value::Timestamptz(s(0)),
            "date.format" => Value::Text(s(0)),

            // ---- string (builtins.md §4)
            "string.of" => Value::Text(text(&arg(0))),
            "string.len" => Value::Int(s(0).chars().count() as i64),
            "string.lower" => Value::Text(s(0).to_lowercase()),
            "string.upper" => Value::Text(s(0).to_uppercase()),
            "string.trim" => Value::Text(s(0).trim().to_string()),
            "string.replace" => Value::Text(s(0).replace(&s(1), &s(2))),
            "string.starts_with" => Value::Bool(s(0).starts_with(&s(1))),
            "string.ends_with" => Value::Bool(s(0).ends_with(&s(1))),
            "string.contains" => Value::Bool(s(0).contains(&s(1))),
            "string.split" => Value::Array(
                s(0).split(&s(1))
                    .map(|p| Value::Text(p.to_string()))
                    .collect(),
            ),
            "string.split_csv" => Value::Array(
                s(0).split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| Value::Text(p.to_string()))
                    .collect(),
            ),
            "string.join" => {
                let parts = match arg(0) {
                    Value::Array(items) => items.iter().map(text).collect::<Vec<_>>(),
                    other => vec![text(&other)],
                };
                Value::Text(parts.join(&s(1)))
            }
            "string.pad_left" => {
                let base = s(0);
                let width = n(1) as usize;
                let pad = s(2);
                let mut out = String::new();
                while out.chars().count() + base.chars().count() < width && !pad.is_empty() {
                    out.push_str(&pad);
                }
                Value::Text(format!("{out}{base}"))
            }
            "string.pad_right" => {
                let base = s(0);
                let width = n(1) as usize;
                let pad = s(2);
                let mut out = base.clone();
                while out.chars().count() < width && !pad.is_empty() {
                    out.push_str(&pad);
                }
                Value::Text(out)
            }
            "string.slice" => {
                let base = s(0);
                let from = n(1).max(0) as usize;
                let len = n(2).max(0) as usize;
                Value::Text(base.chars().skip(from).take(len).collect())
            }
            "string.matches" => match regex::Regex::new(&s(1)) {
                Ok(r) => Value::Bool(r.is_match(&s(0))),
                Err(_) => Value::Bool(false),
            },
            // The correct spelling of the sample's old `string.replace(h,
            // "Bearer ", "")`, which also stripped the literal from the
            // middle of a token.
            "string.strip_prefix" => {
                let base = s(0);
                Value::Text(base.strip_prefix(&s(1)).unwrap_or(&base).to_string())
            }

            // ---- array (builtins.md §5) — the lambda replacement
            "array.len" => Value::Int(items(&arg(0)).len() as i64),
            "array.is_empty" => Value::Bool(items(&arg(0)).is_empty()),
            "array.sum" => {
                let field = s(1);
                let total: f64 = items(&arg(0))
                    .iter()
                    .filter_map(|r| r.field(&field).and_then(numeric))
                    .sum();
                Value::Numeric(trim_decimal(total))
            }
            "array.sum_product" => {
                let (fa, fb) = (s(1), s(2));
                let total: f64 = items(&arg(0))
                    .iter()
                    .filter_map(|r| {
                        Some(r.field(&fa).and_then(numeric)? * r.field(&fb).and_then(numeric)?)
                    })
                    .sum();
                Value::Numeric(trim_decimal(total))
            }
            "array.min" | "array.max" => {
                let field = s(1);
                let mut vals: Vec<f64> = items(&arg(0))
                    .iter()
                    .filter_map(|r| r.field(&field).and_then(numeric))
                    .collect();
                vals.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
                match if path == "array.min" {
                    vals.first()
                } else {
                    vals.last()
                } {
                    Some(v) => Value::Numeric(trim_decimal(*v)),
                    None => Value::Null,
                }
            }
            "array.pluck" => {
                let field = s(1);
                Value::Array(
                    items(&arg(0))
                        .iter()
                        .map(|r| r.field(&field).cloned().unwrap_or(Value::Null))
                        .collect(),
                )
            }
            "array.contains" => Value::Bool(items(&arg(0)).contains(&arg(1))),
            "array.first" => items(&arg(0)).first().cloned().unwrap_or(Value::Null),
            "array.last" => items(&arg(0)).last().cloned().unwrap_or(Value::Null),
            "array.sorted" => {
                let field = s(1);
                let mut xs = items(&arg(0));
                xs.sort_by(|p, q| {
                    let a = p.field(&field).and_then(numeric).unwrap_or(0.0);
                    let b = q.field(&field).and_then(numeric).unwrap_or(0.0);
                    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
                });
                Value::Array(xs)
            }

            // ---- hash / jwt / crypto (builtins.md §6)
            "hash.password" => Value::Text(
                crate::password::hash_password(&s(0)).map_err(|e| fault(e.to_string()))?,
            ),
            "hash.verify" => {
                Value::Bool(crate::password::verify_password(&s(0), &s(1)).unwrap_or(false))
            }
            "hash.sha256" => Value::Text(crate::hash::sha256_hex(&s(0))),
            "hash.hmac_sha256" => Value::Text(crate::hash::hmac_sha256_hex(&s(1), &s(0))),
            "hash.hmac_verify" => {
                // (payload, signature, secret) — `hmac_sha256_hex` takes
                // (key, msg).
                let expected = crate::hash::hmac_sha256_hex(&s(2), &s(0));
                Value::Bool(constant_time_eq(&expected, &s(1)))
            }
            "crypto.constant_time_eq" => Value::Bool(crate::cursor::constant_time_eq(
                s(0).as_bytes(),
                s(1).as_bytes(),
            )),
            "crypto.token" => {
                let len = n(0).clamp(1, 128) as usize;
                let mut bytes = vec![0u8; len];
                getrandom_fill(&mut bytes);
                Value::Text(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
            }
            "jwt.sign" => {
                let now = chrono::Utc::now().timestamp();
                let ttl_minutes = n(2).max(1);
                let sub = arg(0).field("sub").map(text).unwrap_or_default();
                let payload = serde_json::json!({
                    "sub": sub,
                    "iat": now,
                    "exp": now + ttl_minutes * 60,
                })
                .to_string();
                Value::Text(
                    crate::jwt::sign_hs256(&payload, &s(1)).map_err(|e| fault(e.to_string()))?,
                )
            }
            "jwt.verify" => match crate::jwt::verify_hs256(&s(0), &s(1)) {
                Ok(payload) => match serde_json::from_str::<serde_json::Value>(&payload) {
                    Ok(j) => {
                        let exp = j.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
                        // An expired token verifies its signature and still
                        // fails: `jwt.verify` returns null for both
                        // (builtins.md §6).
                        if exp != 0 && exp < chrono::Utc::now().timestamp() {
                            Value::Null
                        } else {
                            Value::Record(vec![
                                (
                                    "sub".into(),
                                    Value::Text(
                                        j.get("sub")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                    ),
                                ),
                                ("exp".into(), Value::Bigint(exp)),
                                (
                                    "iat".into(),
                                    Value::Bigint(
                                        j.get("iat").and_then(|v| v.as_i64()).unwrap_or(0),
                                    ),
                                ),
                            ])
                        }
                    }
                    Err(_) => Value::Null,
                },
                Err(_) => Value::Null,
            },

            // ---- request / response (builtins.md §7)
            "request.raw_body" => Value::Text(self.request.body.clone()),
            "request.header" => {
                let key = s(0).to_lowercase();
                match self.request.headers.get(&key) {
                    Some(v) => Value::Text(v.clone()),
                    None => Value::Null,
                }
            }
            "request.query" => {
                let key = s(0);
                match self.request.query.iter().find(|(k, _)| *k == key) {
                    Some((_, v)) => Value::Text(v.clone()),
                    None => Value::Null,
                }
            }
            "request.query_all" => Value::Array(
                self.request
                    .query
                    .iter()
                    .filter(|(k, _)| *k == s(0))
                    .map(|(_, v)| Value::Text(v.clone()))
                    .collect(),
            ),
            "request.method" => Value::Text(self.request.method.clone()),
            "request.path" => Value::Text(self.request.path.clone()),
            "request.route" => Value::Text(self.request.route.clone()),
            "request.id" => Value::Text(self.request.id.clone()),
            "request.peer_ip" => Value::Text(self.request.peer_ip.clone()),
            "request.client_ip" => Value::Text(self.request.client_ip.clone()),
            "response.status" => Value::Int(self.response_status.unwrap_or(200) as i64),
            "response.set_header" | "response.add_header" => {
                self.extra_headers.push((s(0), s(1)));
                Value::Null
            }

            // ---- packages (builtins.md §8)
            //
            // `redis.*` is handled ahead of this table, in `redis_call` —
            // it is async.
            "mail.send" => Value::Null,

            _ => return Ok(None),
        }))
    }
}

fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(xs) => xs.clone(),
        Value::Raw(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(serde_json::Value::Array(xs)) => xs.iter().map(Value::from_json).collect(),
            _ => Vec::new(),
        },
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) | Value::Bigint(n) => Some(*n as f64),
        Value::Numeric(s) | Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

fn trim_decimal(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn getrandom_fill(buf: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(buf);
}

pub fn path_of(e: &Expr) -> Option<String> {
    match &*e.kind {
        ExprKind::Name(i) => Some(i.name.clone()),
        ExprKind::Field { base, field } => Some(format!("{}.{}", path_of(base)?, field.name)),
        _ => None,
    }
}

/// Response construction. A response is a **value** (routing.md §6.1), so
/// `created(json($account))` composes: `json` builds one at 200 and
/// `created` re-statuses it to 201 rather than wrapping it again.
impl<'a> Vm<'a> {
    fn respond(&mut self, status: u16, value: &Value) -> Value {
        match value {
            Value::Response { body, headers, .. } => Value::Response {
                status,
                body: body.clone(),
                headers: headers.clone(),
            },
            other => {
                let mut body = String::new();
                other.write_json(&mut body);
                Value::Response {
                    status,
                    body,
                    headers: vec![(
                        "content-type".into(),
                        "application/json; charset=utf-8".into(),
                    )],
                }
            }
        }
    }

    fn respond_message(&mut self, status: u16, message: &str) -> Value {
        self.respond(
            status,
            &Value::Record(vec![("error".into(), Value::Text(message.to_string()))]),
        )
    }

    fn respond_empty(&mut self, status: u16) -> Value {
        Value::Response {
            status,
            body: String::new(),
            headers: Vec::new(),
        }
    }
}

/// `{}` → `$1…$n`, in order.
///
/// The only transformation `raw` performs. It is separate so it can be
/// tested without a database: getting the numbering wrong would bind the
/// right values to the wrong holes, which is a silent wrong answer rather
/// than an error.
pub(super) fn rewrite_placeholders(template: &str) -> (String, usize) {
    let mut sql = String::with_capacity(template.len());
    let mut rest = template;
    let mut n = 0usize;
    while let Some(at) = rest.find("{}") {
        sql.push_str(&rest[..at]);
        n += 1;
        // Bound as text, like every other parameter in the language — the
        // author writes the cast, because hand-written SQL carries no type
        // information to derive one from. `where org_id = ({})::bigint`.
        sql.push_str(&format!("(${n}::text)"));
        rest = &rest[at + 2..];
    }
    sql.push_str(rest);
    (sql, n)
}

#[cfg(test)]
mod raw_tests {
    use super::rewrite_placeholders;

    #[test]
    fn numbers_placeholders_in_order() {
        assert_eq!(
            rewrite_placeholders("select {} where a = {} and b = {}"),
            (
                "select ($1::text) where a = ($2::text) and b = ($3::text)".to_string(),
                3
            )
        );
    }

    #[test]
    fn no_placeholders_is_the_statement_unchanged() {
        assert_eq!(
            rewrite_placeholders("select 1"),
            ("select 1".to_string(), 0)
        );
    }

    #[test]
    fn a_lone_brace_is_not_a_placeholder() {
        // `{` and `}` appear in jsonb literals and array constructors.
        assert_eq!(
            rewrite_placeholders("select '{\"a\": 1}'::jsonb, {}"),
            ("select '{\"a\": 1}'::jsonb, ($1::text)".to_string(), 1)
        );
    }
}
