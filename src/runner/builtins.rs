//! Built-in function evaluators for the JWC interpreter `Vm`.
//!
//! These `eval_*_call` methods were split out of `runner/mod.rs` to keep the
//! main interpreter file manageable. They are dispatched from the
//! `Expr::Call { name, args }` arm of `Vm::eval_expr` in the parent module,
//! so each is `pub(super)` to remain visible there. This is a pure code move:
//! behaviour is identical to when these lived inside `runner/mod.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value as JsonValue;
use tokio_postgres::types::ToSql;

use crate::ast::Expr;

// Pull in `Vm`, the `Value`/`Flow` enums, `WS_HANDLE`, the `engine` import
// alias, and the private free helper functions (`http_client`, etc.) defined
// in the parent `runner` module. A child module may name private items of
// its ancestors.
use super::sql::{boxed_params_to_refs, json_value_to_sql_param};
use super::util::{
    apply_headers_reqwest, assemble_url_from_pg_env, check_outbound_url,
    connection_string_from_arg, http_response_to_json_string,
};
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
            // Absent and no explicit default → empty string, matching
            // `path_param` / `env`. Returning null here forced every caller to
            // null-check before feeding a typed `string` parameter (pain log).
            (None, None) => Ok(Value::Str(String::new())),
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
        check_outbound_url(&url)?;

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
        check_outbound_url(&url)?;

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
        // verify_hs256 tolerates a `Bearer ` prefix, so a handler can pass
        // `header("authorization")` straight through.
        Ok(Value::Str(crate::jwt::verify_hs256(&token, &secret)?))
    }

    /// `jwt_verify_jwks(token, jwks_url)` — RS256 verification against an
    /// OIDC provider's published key set.
    ///
    /// Deliberately a separate built-in rather than a third argument on
    /// `jwt_verify`: that one's second parameter is a shared secret, and
    /// overloading it to sometimes mean "a URL to fetch a public key
    /// from" is the kind of ambiguity that ends in an algorithm-confusion
    /// bug. The two never share a code path beyond claim validation.
    ///
    /// `exp` / `nbf` / `iss` / `aud` are checked exactly as they are for
    /// HS256 — same `VerifyOptions`, same env vars.
    pub(super) async fn eval_jwt_verify_jwks_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("jwt_verify_jwks(token, jwks_url) expects exactly 2 args");
        }
        let token = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify_jwks(token, jwks_url): token must be string, got {}",
                other.type_name()
            ),
        };
        let jwks_url = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!(
                "jwt_verify_jwks(token, jwks_url): jwks_url must be string, got {}",
                other.type_name()
            ),
        };
        // The JWKS URL is normally operator config rather than request
        // data, but it goes through the same outbound gate as every other
        // HTTP built-in so one policy covers all egress. Note for
        // internal identity providers: `JWC_HTTP_BLOCK_PRIVATE` would
        // block the fetch, so leave it off (or allowlist the host) when
        // the IdP lives on a private network.
        check_outbound_url(&jwks_url)?;

        // Read `kid` from the header BEFORE any signature check — that is
        // what picks the key. Nothing from the unverified payload is
        // trusted or returned unless the signature checks out.
        let split = crate::jwt::split_token(&token)?;
        let kid = split.kid().map(str::to_string);
        drop(split);

        let key = crate::jwks::rsa_key_for(&jwks_url, kid.as_deref()).await?;
        Ok(Value::Str(crate::jwt::verify_rs256(
            &token, &key.n, &key.e,
        )?))
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

    /// `client_ip()` — the original client's IP when the request flows
    /// through a proxy (Cloudflare, nginx, k8s ingress). Reads the
    /// header named by `JWC_REAL_IP_HEADER` (default `x-forwarded-for`),
    /// then walks the comma-separated chain from RIGHT to LEFT, peeling
    /// off any entries listed in `JWC_TRUSTED_PROXIES`. The first
    /// untrusted entry is the original client; everything to its right
    /// is a trusted forwarder.
    ///
    /// `JWC_TRUSTED_PROXIES` is a comma-separated list of exact IPs or
    /// prefixes (e.g. `JWC_TRUSTED_PROXIES=10.,127.0.0.1,::1`). Empty /
    /// unset means "no proxies are trusted" — the rightmost entry is
    /// returned. That's a sane default for projects behind a load
    /// balancer where the LB always overwrites the rightmost slot.
    ///
    /// Returns null when no such header is present or every chain entry
    /// matches a trusted prefix.
    ///
    /// Closes the dogfooding gap where jwc-shortener had to hand-roll
    /// `header("x-forwarded-for")` per app and got Cloudflare's
    /// `cf-connecting-ip` precedence wrong.
    pub(super) async fn eval_client_ip_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("client_ip() expects no args");
        }
        let header_name =
            std::env::var("JWC_REAL_IP_HEADER").unwrap_or_else(|_| "x-forwarded-for".to_string());
        let key = header_name.to_ascii_lowercase();
        let raw = self
            .current_headers
            .as_ref()
            .and_then(|h| h.get(&key).cloned());
        let Some(raw) = raw else {
            return Ok(Value::Null);
        };
        let trusted_raw = std::env::var("JWC_TRUSTED_PROXIES").unwrap_or_default();
        let trusted: Vec<&str> = trusted_raw
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        // RIGHT-to-LEFT walk: nginx / go's net/http convention. The
        // rightmost entry was written by the closest hop; the first
        // entry that isn't a trusted forwarder is the real client.
        for entry in raw.split(',').rev() {
            let candidate = entry.trim();
            if candidate.is_empty() {
                continue;
            }
            let is_trusted = trusted.iter().any(|prefix| candidate.starts_with(prefix));
            if !is_trusted {
                return Ok(Value::Str(candidate.to_string()));
            }
        }
        Ok(Value::Null)
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
        // Route on what the statement actually returns, not on its leading
        // keyword. Prefix matching sent `UPDATE ... RETURNING url` to the exec
        // path, so the caller got the affected-row count and the returned
        // column was dropped — silently, which is how a redirect ended up
        // with `Location: 1`.
        let outcome = engine::query_or_exec(&sql, &param_refs).await?;

        // Cache invalidation is a separate question from routing: a statement
        // that returns rows may still have written some (`UPDATE ...
        // RETURNING`, `WITH x AS (DELETE ...) SELECT`). Erring toward
        // clearing costs a cache refill; erring the other way serves stale
        // reads, which is why the old prefix check under-invalidated a
        // mutating `WITH`.
        if may_mutate(&sql) {
            engine::invalidate_result_cache()?;
        }

        match outcome {
            engine::RawSqlOutcome::Rows(result) => {
                if result.is_empty() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Str(result))
                }
            }
            engine::RawSqlOutcome::Affected(affected) => Ok(Value::Int(affected as i64)),
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
            // **Phase 1** — object literals now come in as Record; render to
            // JSON once so the existing string-form parser stays the only
            // schema-validating code path.
            Value::Record { .. } => {
                connection_string_from_arg(&super::value_to_json(&value).to_string())?
            }
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
            Value::Array(a) => Ok(Value::Int(a.len() as i64)),
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
            Value::Array(items) => Ok(Value::Bool(items.contains(&needle))),
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

    /// `substring(s, start, len)` — char-based slice. `start` is a 0-based
    /// char index. Out-of-range `start` or non-positive `len` yields an empty
    /// string (no panic, no exception) — matches the dogfooding need for a
    /// safe truncation builtin that replaces the `split(s, "")` for-loop
    /// workaround in `jwc-shortener`.
    pub(super) async fn eval_substring_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("substring(s, start, len) expects exactly 3 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("substring: s must be string, got {}", other.type_name()),
        };
        let start = match self.eval_expr(&args[1], vars).await? {
            Value::Int(n) => n,
            other => bail!("substring: start must be int, got {}", other.type_name()),
        };
        let len = match self.eval_expr(&args[2], vars).await? {
            Value::Int(n) => n,
            other => bail!("substring: len must be int, got {}", other.type_name()),
        };
        Ok(Value::Str(slice_chars(&s, start, Some(len))))
    }

    /// `take(s, n)` — first `n` chars of `s`. `n <= 0` yields an empty
    /// string. Equivalent to `substring(s, 0, n)`, kept as its own name
    /// because "take the first N" reads more naturally than a 3-arg slice in
    /// auth / token / log-prefix code paths.
    pub(super) async fn eval_take_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("take(s, n) expects exactly 2 args");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            other => bail!("take: s must be string, got {}", other.type_name()),
        };
        let n = match self.eval_expr(&args[1], vars).await? {
            Value::Int(n) => n,
            other => bail!("take: n must be int, got {}", other.type_name()),
        };
        Ok(Value::Str(slice_chars(&s, 0, Some(n))))
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
            Value::Array(a) => {
                let elem = if first { a.first() } else { a.last() };
                return Ok(elem.cloned().unwrap_or(Value::Null));
            }
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

    /// `range(n)` → `[0, …, n-1]`; `range(start, end)`; `range(start, end, step)`.
    /// Step must be positive (mirrors native `(start..end).step_by(step)`).
    pub(super) async fn eval_range_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.is_empty() || args.len() > 3 {
            bail!("range(n) / range(start, end) / range(start, end, step) expects 1-3 args");
        }
        let mut nums = Vec::with_capacity(args.len());
        for a in args {
            match self.eval_expr(a, vars).await? {
                Value::Int(n) => nums.push(n),
                other => bail!("range: args must be int, got {}", other.type_name()),
            }
        }
        let (start, end, step) = match nums.as_slice() {
            [n] => (0i64, *n, 1i64),
            [s, e] => (*s, *e, 1i64),
            [s, e, st] => (*s, *e, *st),
            _ => unreachable!(),
        };
        if step <= 0 {
            bail!("range: step must be positive, got {step}");
        }
        let mut out = Vec::new();
        let mut i = start;
        while i < end {
            out.push(Value::Int(i));
            i += step;
        }
        Ok(Value::Array(out))
    }

    /// `push(arr, x)` / `append(arr, x)` — append `x` to the array bound to the
    /// first argument (which must be a variable), mutating it in place, and
    /// return the resulting array.
    pub(super) async fn eval_push_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("push(arr, x) expects exactly 2 args");
        }
        let elem = self.eval_expr(&args[1], vars).await?;
        let var_name = match &args[0] {
            Expr::Var(n) => n.to_lowercase(),
            _ => bail!("push(arr, x): first arg must be an array variable"),
        };
        let current = vars
            .get(&var_name)
            .cloned()
            .ok_or_else(|| anyhow!("push: undefined variable"))?;
        let mut items = match current {
            Value::Array(items) => items,
            Value::Null => Vec::new(),
            other => bail!(
                "push: first arg must be an array, got {}",
                other.type_name()
            ),
        };
        items.push(elem);
        let result = Value::Array(items);
        vars.insert(var_name, result.clone());
        Ok(result)
    }

    /// `join(arr, sep)` — stringify each element and concatenate with `sep`. O(n).
    pub(super) async fn eval_join_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("join(arr, sep) expects exactly 2 args");
        }
        let arr = self.eval_expr(&args[0], vars).await?;
        let sep = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("join: sep must be string, got {}", other.type_name()),
        };
        let items: Vec<Value> = match arr {
            Value::Array(items) => items,
            Value::Null => Vec::new(),
            Value::Str(s) => match serde_json::from_str::<JsonValue>(&s) {
                Ok(JsonValue::Array(a)) => a.iter().map(json_to_value).collect(),
                _ => bail!("join: first arg must be an array"),
            },
            other => bail!(
                "join: first arg must be an array, got {}",
                other.type_name()
            ),
        };
        let joined = items
            .iter()
            .map(|v| v.as_string())
            .collect::<Vec<_>>()
            .join(&sep);
        Ok(Value::Str(joined))
    }

    pub(super) async fn eval_sha256_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("sha256(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!("sha256(s): s must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(crate::hash::sha256_hex(&s)))
    }

    pub(super) async fn eval_sha1_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("sha1(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!("sha1(s): s must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(crate::hash::sha1_hex(&s)))
    }

    pub(super) async fn eval_md5_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("md5(s) expects exactly 1 arg");
        }
        let s = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!("md5(s): s must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(crate::hash::md5_hex(&s)))
    }

    pub(super) async fn eval_hmac_sha256_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("hmac_sha256(key, msg) expects exactly 2 args");
        }
        let key = match self.eval_expr(&args[0], vars).await? {
            Value::Str(s) => s,
            other => bail!("hmac_sha256: key must be string, got {}", other.type_name()),
        };
        let msg = match self.eval_expr(&args[1], vars).await? {
            Value::Str(s) => s,
            other => bail!("hmac_sha256: msg must be string, got {}", other.type_name()),
        };
        Ok(Value::Str(crate::hash::hmac_sha256_hex(&key, &msg)))
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

        let obj = self.eval_expr(&args[0], vars).await?;
        let field = match self.eval_expr(&args[1], vars).await? {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: field must be string, got {}",
                other.type_name()
            ),
        };
        let value = self.eval_expr(&args[2], vars).await?;

        // **Phase 1 [1.0-blocker]** — Record fast path. When the first arg
        // already arrives as a `Value::Record` (object literal, json_parse
        // output, future entity instance) we skip the parse/mutate/stringify
        // round-trip entirely:
        //   - Existing field: CoW the `values` Arc via `Arc::make_mut` and
        //     overwrite the slot at the field's position. The shared
        //     `field_names` Arc is reused as-is (refcount bump).
        //   - New field: fall back to the V::Str JSON path below by
        //     serialising the Record to its JSON form first. Growing a
        //     Record means allocating a NEW `field_names` Arc (the old one
        //     may be shared across thousands of rows of the same shape), and
        //     the V::Str path already handles the dynamic-shape case
        //     correctly — keeping a single fallback is simpler than a second
        //     Record-grow code path here, and field-add-after-construction
        //     is the rare case (almost all `set_json_field` callers patch an
        //     existing field on a request body / config).
        if let Value::Record {
            field_names,
            values,
        } = obj
        {
            if let Some(idx) = field_names
                .iter()
                .position(|f| f.as_ref() == field.as_str())
            {
                let mut new_values = values;
                let values_mut = Arc::make_mut(&mut new_values);
                values_mut[idx] = value;
                return Ok(Value::Record {
                    field_names,
                    values: new_values,
                });
            }
            // New field on a Record — re-route through the V::Str fallback
            // by materialising the Record as JSON. Cheap: a 2-10 field
            // record's JSON form is small, and this branch is rare.
            let json_str = value_to_json(&Value::Record {
                field_names,
                values,
            })
            .to_string();
            return self
                .set_json_field_via_string(json_str, field, value)
                .map(Value::Str);
        }

        let source = match obj {
            Value::Str(v) => v,
            other => bail!(
                "set_json_field: first arg must be string json, got {}",
                other.type_name()
            ),
        };
        let json_str = self.set_json_field_via_string(source, field, value)?;
        Ok(Value::Str(json_str))
    }

    /// Shared JSON parse/mutate/stringify path used by both the V::Str
    /// branch and the Record-grow fallback in `eval_set_json_field_call`.
    fn set_json_field_via_string(
        &self,
        source: String,
        field: String,
        value: Value,
    ) -> Result<String> {
        let mut json_value: JsonValue = serde_json::from_str(&source)
            .with_context(|| "set_json_field: invalid json in first arg")?;
        let object = json_value
            .as_object_mut()
            .ok_or_else(|| anyhow!("set_json_field: first arg must be a json object"))?;
        object.insert(field, value_to_json(&value));
        Ok(json_value.to_string())
    }

    // ── console.* ────────────────────────────────────────────────────────

    /// `console.write(s)` / `console.writeln(s)` / `console.error(s)` —
    /// write to the process's real stdout/stderr, right now.
    ///
    /// `writeln` is `write` plus a trailing newline. It exists because that
    /// is the common case, and because reaching for `print` to get the
    /// newline lands you in the buffer instead — which is the whole thing
    /// this family is here to avoid.
    ///
    /// Deliberately NOT the `print` statement: `print` appends to
    /// `Vm::output`, which `cmd::run` flushes only after `main()` returns and
    /// which `dispatch` consumes as the implicit response body of a
    /// fall-through route. This writes past both, which is what makes it the
    /// correct way to log from inside a handler.
    ///
    /// No trailing newline — callers who want one pass it. The explicit
    /// `flush()` matters because Rust's stdout is a `LineWriter`: a payload
    /// with no `\n` would otherwise sit in the buffer and "immediate" would
    /// be a lie on a pipe.
    ///
    /// Takes any value via `as_string()` rather than demanding `Value::Str`,
    /// matching `print` — `console.write(42)` should not be a type error.
    pub(super) async fn eval_console_write_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        to_stderr: bool,
        newline: bool,
    ) -> Result<Value> {
        let name = match (to_stderr, newline) {
            (true, _) => "console.error",
            (false, true) => "console.writeln",
            (false, false) => "console.write",
        };
        if args.len() != 1 {
            bail!("{name}(s) expects exactly 1 arg");
        }
        let mut s = self.eval_expr(&args[0], vars).await?.as_string();
        if newline {
            s.push('\n');
        }
        use std::io::Write;
        let res = if to_stderr {
            let stream = std::io::stderr();
            let mut lock = stream.lock();
            lock.write_all(s.as_bytes()).and_then(|()| lock.flush())
        } else {
            let stream = std::io::stdout();
            let mut lock = stream.lock();
            lock.write_all(s.as_bytes()).and_then(|()| lock.flush())
        };
        res.with_context(|| format!("{name} failed"))?;
        Ok(Value::Null)
    }

    /// `console.read()` — one line from stdin, trailing `\r?\n` stripped.
    ///
    /// `null` at EOF, never `""`: an empty string is a legitimate line (bare
    /// Enter), so conflating the two makes `while console.read() != null`
    /// unwritable and turns every read loop into an infinite one.
    ///
    /// `spawn_blocking` over `std::io::stdin()` rather than
    /// `tokio::io::stdin()`: the tokio handle is fresh per call, so wrapping
    /// it in a `BufReader` to get `read_line` drops whatever the reader read
    /// ahead past the newline when it goes out of scope — consecutive calls
    /// would silently lose input. `std::io::stdin()` is a process-global
    /// buffered handle, so read-ahead survives.
    pub(super) async fn eval_console_read_call(&mut self, args: &[Expr]) -> Result<Value> {
        if !args.is_empty() {
            bail!("console.read() expects exactly 0 args");
        }
        let read = tokio::task::spawn_blocking(|| {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map(|n| (n, line))
        })
        .await
        .map_err(|e| anyhow!("console.read() task failed: {e}"))?;
        let (n, mut line) = read.context("console.read() failed")?;
        if n == 0 {
            return Ok(Value::Null);
        }
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(Value::Str(line))
    }

    // ── file.* / directory.* ─────────────────────────────────────────────
    //
    // Every real I/O failure below goes through `.with_context(...)` on the
    // `io::Result`, which keeps the `io::Error` as the anyhow source so
    // `classify_jwc_error`'s typed downcast can reach it. The arity and
    // type-mismatch `bail!`s deliberately do NOT embed the path: they
    // produce a bare anyhow with no `io::Error` in the chain, and
    // `classify_jwc_error` would fall through to its substring scan — where
    // a path like `/var/backups/app.sql` classifies as `DbError`.

    /// Evaluate arg `idx` as a path string. Never puts the value into the
    /// error message — see the note above.
    async fn eval_path_arg(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        idx: usize,
        name: &str,
    ) -> Result<String> {
        match self.eval_expr(&args[idx], vars).await? {
            Value::Str(s) => Ok(s),
            other => bail!("{name}: path must be string, got {}", other.type_name()),
        }
    }

    /// `file.read(path)` — whole-file read as a string.
    ///
    /// Raises rather than returning null on a missing file: `file.exists()`
    /// is the existence probe, and a null return would conflate missing /
    /// permission-denied / is-a-directory / not-UTF-8 into one value.
    ///
    /// `tokio::fs`, not `std::fs`: reachable from a route handler, so a slow
    /// mount must not park a runtime worker.
    pub(super) async fn eval_file_read_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("file.read(path) expects exactly 1 arg");
        }
        let path = self.eval_path_arg(args, vars, 0, "file.read(path)").await?;
        let contents = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("file.read({path}) failed"))?;
        Ok(Value::Str(contents))
    }

    /// `file.write(path, content)` — create or truncate.
    /// `file.append(path, content)` — create if absent, else append.
    pub(super) async fn eval_file_write_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        append: bool,
    ) -> Result<Value> {
        let name = if append { "file.append" } else { "file.write" };
        if args.len() != 2 {
            bail!("{name}(path, content) expects exactly 2 args");
        }
        let path = self
            .eval_path_arg(args, vars, 0, &format!("{name}(path, content)"))
            .await?;
        let content = self.eval_expr(&args[1], vars).await?.as_string();
        if append {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .with_context(|| format!("{name}({path}) failed"))?;
            f.write_all(content.as_bytes())
                .await
                .with_context(|| format!("{name}({path}) failed"))?;
        } else {
            tokio::fs::write(&path, content.as_bytes())
                .await
                .with_context(|| format!("{name}({path}) failed"))?;
        }
        Ok(Value::Null)
    }

    /// `file.exists(path)` — true only for a regular file.
    ///
    /// Never raises: a permission error on the parent directory yields
    /// `false`. That means `false` is "not visible to this process as a
    /// file", not strictly "does not exist". `directory.exists` is the
    /// mirror for directories, so the pair answers "what is this" in one
    /// call each.
    pub(super) async fn eval_fs_exists_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
        want_dir: bool,
    ) -> Result<Value> {
        let name = if want_dir {
            "directory.exists"
        } else {
            "file.exists"
        };
        if args.len() != 1 {
            bail!("{name}(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, &format!("{name}(path)"))
            .await?;
        let found = match tokio::fs::metadata(&path).await {
            Ok(md) => {
                if want_dir {
                    md.is_dir()
                } else {
                    md.is_file()
                }
            }
            Err(_) => false,
        };
        Ok(Value::Bool(found))
    }

    /// `file.delete(path)` — raises if absent. Idempotent delete is
    /// `if (file.exists(p)) { file.delete(p); }`, which says what it means.
    pub(super) async fn eval_file_delete_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("file.delete(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, "file.delete(path)")
            .await?;
        tokio::fs::remove_file(&path)
            .await
            .with_context(|| format!("file.delete({path}) failed"))?;
        Ok(Value::Null)
    }

    /// `file.copy(src, dst)` — overwrites `dst`.
    pub(super) async fn eval_file_copy_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("file.copy(src, dst) expects exactly 2 args");
        }
        let src = self
            .eval_path_arg(args, vars, 0, "file.copy(src, dst)")
            .await?;
        let dst = self
            .eval_path_arg(args, vars, 1, "file.copy(src, dst)")
            .await?;
        tokio::fs::copy(&src, &dst)
            .await
            .with_context(|| format!("file.copy({src} -> {dst}) failed"))?;
        Ok(Value::Null)
    }

    /// `file.move(src, dst)` — overwrites `dst`, works across filesystems.
    ///
    /// `rename` returns `EXDEV` across devices (a `/tmp` → volume move, an
    /// overlayfs → bind-mount move — routine in containers) and on Windows
    /// fails when `dst` exists. A blanket "on any error, copy instead" would
    /// turn a missing-source `NotFound` into a confusing copy error, so the
    /// fallback is gated to leave genuine source problems alone. This is
    /// what `mv` does.
    pub(super) async fn eval_file_move_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("file.move(src, dst) expects exactly 2 args");
        }
        let src = self
            .eval_path_arg(args, vars, 0, "file.move(src, dst)")
            .await?;
        let dst = self
            .eval_path_arg(args, vars, 1, "file.move(src, dst)")
            .await?;
        match tokio::fs::rename(&src, &dst).await {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Err(e).with_context(|| format!("file.move({src} -> {dst}) failed"));
            }
            Err(_) => {
                tokio::fs::copy(&src, &dst).await.with_context(|| {
                    format!("file.move({src} -> {dst}): cross-device copy failed")
                })?;
                tokio::fs::remove_file(&src)
                    .await
                    .with_context(|| format!("file.move({src} -> {dst}): source unlink failed"))?;
            }
        }
        Ok(Value::Null)
    }

    /// `file.size(path)` — size in bytes.
    pub(super) async fn eval_file_size_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("file.size(path) expects exactly 1 arg");
        }
        let path = self.eval_path_arg(args, vars, 0, "file.size(path)").await?;
        let md = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("file.size({path}) failed"))?;
        Ok(Value::Int(md.len() as i64))
    }

    /// `file.lines(path)` — file split on `\r?\n`.
    ///
    /// A trailing newline does NOT produce a final empty element: a
    /// well-formed text file ends with `\n`, and surfacing that as an extra
    /// blank line makes every `for` over the result off by one.
    pub(super) async fn eval_file_lines_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("file.lines(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, "file.lines(path)")
            .await?;
        let contents = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("file.lines({path}) failed"))?;
        let lines: Vec<Value> = contents
            .lines()
            .map(|l| Value::Str(l.to_string()))
            .collect();
        Ok(Value::Array(lines))
    }

    /// `directory.list(path)` — entry names, sorted.
    ///
    /// Names, not full paths: matches `ls` / `os.listdir` / `readdirSync`,
    /// and `path + "/" + name` reconstructs a full path trivially while the
    /// reverse is awkward in JWC's string surface.
    ///
    /// The sort is part of the contract, not an implementation detail:
    /// `read_dir` order is filesystem-dependent (ext4 hands entries back in
    /// hash order), so without it every golden test is flaky and every
    /// generated listing is non-reproducible.
    ///
    /// Non-UTF-8 names come through `to_string_lossy` rather than being
    /// skipped — a name with U+FFFD in it is debuggable, a silently missing
    /// entry is not. Such a name will not round-trip back into `file.read`.
    pub(super) async fn eval_directory_list_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("directory.list(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, "directory.list(path)")
            .await?;
        let mut rd = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("directory.list({path}) failed"))?;
        let mut names: Vec<String> = Vec::new();
        while let Some(entry) = rd
            .next_entry()
            .await
            .with_context(|| format!("directory.list({path}) failed"))?
        {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(Value::Array(names.into_iter().map(Value::Str).collect()))
    }

    /// `directory.create(path)` — recursive and idempotent
    /// (`create_dir_all`). Plain `create_dir` fails on the single most
    /// common call (`directory.create("out/reports")` where `out` is
    /// absent) and again when the target already exists, which turns every
    /// "ensure the directory" into an exists-check dance.
    pub(super) async fn eval_directory_create_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("directory.create(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, "directory.create(path)")
            .await?;
        tokio::fs::create_dir_all(&path)
            .await
            .with_context(|| format!("directory.create({path}) failed"))?;
        Ok(Value::Null)
    }

    /// `directory.delete(path)` — NOT recursive.
    ///
    /// `remove_dir`, so a non-empty directory is an error. Paths here are
    /// unrestricted by design, and a recursive variant would make
    /// `directory.delete(query_param("d"))` a one-call `rm -rf`. Callers who
    /// genuinely want a tree walk can drive it with `directory.list`. The
    /// asymmetry with the recursive `directory.create` is deliberate:
    /// creating too much is recoverable, deleting too much is not.
    pub(super) async fn eval_directory_delete_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("directory.delete(path) expects exactly 1 arg");
        }
        let path = self
            .eval_path_arg(args, vars, 0, "directory.delete(path)")
            .await?;
        tokio::fs::remove_dir(&path)
            .await
            .with_context(|| format!("directory.delete({path}) failed"))?;
        Ok(Value::Null)
    }
}

/// Char-based string slice shared by `substring` / `take`. Negative `start`
/// or a non-positive `len` returns an empty string; running past the end of
/// `s` is not an error — we just take what's there. UTF-8 safe because we
/// iterate `chars()` instead of indexing bytes.
fn slice_chars(s: &str, start: i64, len: Option<i64>) -> String {
    if start < 0 {
        return String::new();
    }
    let start = start as usize;
    match len {
        Some(n) if n <= 0 => String::new(),
        Some(n) => s.chars().skip(start).take(n as usize).collect(),
        None => s.chars().skip(start).collect(),
    }
}

/// Whether a raw statement might have written something, used only to decide
/// cache invalidation.
///
/// Deliberately crude and deliberately over-eager: a column named
/// `updated_at` makes a plain `SELECT` look mutating, which costs one cache
/// refill. The opposite mistake — missing a write and serving a stale read —
/// is the one that produces wrong answers, and it's what the previous
/// leading-keyword check did to `WITH x AS (UPDATE ...) SELECT`.
fn may_mutate(sql: &str) -> bool {
    const WRITE_WORDS: &[&str] = &[
        "insert", "update", "delete", "merge", "truncate", "alter", "drop", "create", "grant",
        "revoke", "call", "do ",
    ];
    let lowered = sql.to_ascii_lowercase();
    WRITE_WORDS.iter().any(|w| lowered.contains(w))
}

#[cfg(test)]
mod raw_sql_tests {
    use super::may_mutate;

    /// Routing is decided by the prepared statement's result columns, but
    /// cache invalidation still needs a guess. It must lean toward clearing.
    #[test]
    fn mutating_statements_invalidate_the_cache() {
        for sql in [
            "UPDATE \"link\" SET hits = hits + 1 WHERE code = $1 RETURNING url",
            "WITH b AS (UPDATE \"link\" SET hits = hits + 1 RETURNING url) SELECT url FROM b",
            "INSERT INTO t(a) VALUES ($1) RETURNING id",
            "DELETE FROM t WHERE id = $1",
            "TRUNCATE t",
        ] {
            assert!(may_mutate(sql), "should invalidate: {sql}");
        }
    }

    #[test]
    fn plain_reads_leave_the_cache_alone() {
        for sql in [
            "SELECT coalesce(json_agg(t),'[]')::text FROM (SELECT id FROM link) t",
            "SELECT count(*)::text FROM link WHERE code = $1",
        ] {
            assert!(!may_mutate(sql), "should not invalidate: {sql}");
        }
    }

    /// Over-eager is the safe direction: a column named `updated_at` makes a
    /// read look mutating, which costs a cache refill and nothing else.
    #[test]
    fn over_invalidation_is_tolerated_under_invalidation_is_not() {
        assert!(may_mutate("SELECT updated_at FROM t"));
    }
}
