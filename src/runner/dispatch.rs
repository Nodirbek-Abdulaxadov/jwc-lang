//! HTTP route dispatch, middleware chain execution, and `errorHandler` glue.
//!
//! `dispatch_route` is the public entry point — it matches the incoming
//! `(method, path)` pair against `Vm::routes`, runs the request through any
//! attached middlewares, calls the handler (or executes the inline body),
//! parses the status / content-type sentinels out of the JSON envelope, and
//! runs the response-phase `after { }` blocks in reverse order. This is the
//! single most state-touching method on `Vm` — keeping it isolated in its
//! own file makes the borrow story easier to follow.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use async_recursion::async_recursion;
use serde_json::{json, Value as JsonValue};

use crate::ast::{ErrorHandlerDecl, Expr, Stmt};

use super::{Flow, Value, Vm};

impl<'a> Vm<'a> {
    /// Dispatch a single HTTP request directly (used by the real HTTP server).
    /// Returns (http_status_code, response_body).
    pub async fn dispatch_route(
        &mut self,
        method: &str,
        path: &str,
    ) -> Result<(u16, String, Option<String>, Vec<(String, String)>)> {
        // Split `?query` off the path for route matching; keep the query for
        // `query_param(name)` lookups inside the handler.
        let (clean_path, query_params) = split_path_and_query(path);

        // Find matching route index and collect params (avoid holding borrow across mut calls)
        let mut found_idx: Option<usize> = None;
        let mut found_params: HashMap<String, String> = HashMap::new();

        for (i, route) in self.routes.iter().enumerate() {
            if !route.method.eq_ignore_ascii_case(method) {
                continue;
            }
            if let Some(params) = match_route_pattern(&route.path, &clean_path) {
                found_idx = Some(i);
                found_params = params;
                break;
            }
        }

        let Some(idx) = found_idx else {
            return Ok((
                404,
                format!(
                    "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{method}\",\"path\":\"{clean_path}\"}}"
                ),
                None,
                Vec::new(),
            ));
        };

        // Clone what we need so we can mutably borrow self below
        let handler: Option<String> = self.routes[idx].handler.clone();
        let body_stmts: Vec<Stmt> = self.routes[idx].body.clone();
        let middleware_names: Vec<String> = self.routes[idx].middlewares.clone();
        // Route's own namespace — pushed onto the stack so bare-name calls
        // inside the inline body resolve against the route's namespace and
        // its file-level imports (same as a function call would).
        let route_namespace: Vec<String> = self.routes[idx].namespace.clone();

        let previous = self.current_path_params.take();
        let previous_query = self.current_query_params.take();
        let previous_method = self.current_method.take();
        let previous_request_path = self.current_request_path.take();
        let previous_started = self.current_request_started.take();
        let previous_dirty = std::mem::take(&mut self.dirty_fields);
        self.current_path_params = Some(found_params);
        self.current_query_params = Some(query_params);
        self.current_method = Some(method.to_string());
        self.current_request_path = Some(clean_path.to_string());
        self.current_request_started = Some(std::time::Instant::now());
        self.current_namespace_stack.push(route_namespace);

        // Run middlewares first; if any returns a value, short-circuit
        // the request with that response.
        let mut middleware_response: Option<String> = None;
        for mw_name in &middleware_names {
            if let Some(resp) = self.run_middleware(mw_name).await? {
                middleware_response = Some(resp);
                break;
            }
        }

        // Build the body response. If anything within the route fails AND a
        // top-level `errorHandler` is declared, give that handler a chance to
        // produce the response instead of bubbling up as 500.
        let body_result: Result<Option<String>> = if let Some(resp) = middleware_response {
            Ok(Some(resp))
        } else if let Some(ref handler_name) = handler {
            let args = self.build_handler_args(handler_name);
            self.call_function(handler_name, args)
                .await
                .map(|v| v.map(|v| v.as_string()))
        } else {
            let mut route_vars = HashMap::new();
            self.exec_block(&body_stmts, &mut route_vars)
                .await
                .map(|flow| match flow {
                    Flow::Return(Some(v)) => Some(v.as_string()),
                    Flow::Return(None) => Some("null".to_string()),
                    _ => {
                        if !self.output.is_empty() {
                            let out = self.output.trim_end_matches('\n').to_string();
                            self.output.clear();
                            Some(out)
                        } else {
                            None
                        }
                    }
                })
        };

        let response_str = match body_result {
            Ok(v) => v,
            Err(e) => {
                if let Some(handler) = self.error_handler {
                    let handler = handler.clone();
                    Some(self.run_error_handler(&handler, &e).await?)
                } else {
                    self.current_path_params = previous;
                    self.current_query_params = previous_query;
                    self.current_method = previous_method;
                    self.current_request_path = previous_request_path;
                    self.dirty_fields = previous_dirty;
                    self.current_namespace_stack.pop();
                    return Err(e);
                }
            }
        };
        let body = response_str.unwrap_or_else(|| "null".to_string());

        // Derive HTTP status from a "status" field in JSON, default 200.
        // Then strip the internal "status" field before sending to client.
        //
        // Built-ins like `html(...)` / `text(...)` mark their output with two
        // sentinel keys — `__jwc_content_type__` and `__jwc_body__` — that
        // travel inside the same JSON envelope so the existing status-field
        // strip pass still applies. Both keys are removed here and the raw
        // body string is returned as-is (no JSON re-encoding).
        let (status, clean_body, content_type, extra_headers) =
            if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&body) {
                let code = doc
                    .get("status")
                    .and_then(|s| s.as_u64())
                    .and_then(|s| u16::try_from(s).ok())
                    .filter(|s| *s >= 100 && *s < 600)
                    .unwrap_or(200);
                let mut ct: Option<String> = None;
                let mut raw_body: Option<String> = None;
                let mut headers: Vec<(String, String)> = Vec::new();
                if let Some(obj) = doc.as_object_mut() {
                    obj.remove("status");
                    if let Some(JsonValue::String(s)) = obj.remove("__jwc_content_type__") {
                        ct = Some(s);
                    }
                    if let Some(JsonValue::String(s)) = obj.remove("__jwc_body__") {
                        raw_body = Some(s);
                    }
                    if let Some(JsonValue::Object(hmap)) = obj.remove("__jwc_headers__") {
                        for (k, v) in hmap {
                            let val = match v {
                                JsonValue::String(s) => s,
                                other => other.to_string(),
                            };
                            headers.push((k, val));
                        }
                    }
                }
                let body_out = if code == 204 {
                    String::new()
                } else if let Some(raw) = raw_body {
                    raw
                } else {
                    doc.to_string()
                };
                (code, body_out, ct, headers)
            } else {
                (200, body, None, Vec::new())
            };

        // Response-phase middleware. Walk the applied chain in REVERSE
        // so an `after` block can wrap (log + flush) around the inner
        // middleware's `after` work — mirroring Express / koa / ring
        // semantics. The handler-derived status flows into
        // `current_response_status` so `response_status()` /
        // `response_duration_ms()` can be called from inside the block.
        self.current_response_status = Some(status);
        for mw_name in middleware_names.iter().rev() {
            if let Err(e) = self.run_middleware_after(mw_name).await {
                eprintln!("[JWC] middleware '{mw_name}' after-body error: {e}");
            }
        }
        self.current_response_status = None;

        self.current_path_params = previous;
        self.current_query_params = previous_query;
        self.current_method = previous_method;
        self.current_request_path = previous_request_path;
        self.current_request_started = previous_started;
        self.dirty_fields = previous_dirty;
        self.current_namespace_stack.pop();

        Ok((status, clean_body, content_type, extra_headers))
    }

    /// Run the `after { ... }` block of one middleware, if it has one.
    /// Errors are caught at the dispatcher level and logged — they don't
    /// surface to the client because the response has already been
    /// produced. Side effects (metrics, logging) are the explicit use
    /// case here.
    pub(super) async fn run_middleware_after(&mut self, mw_name: &str) -> Result<()> {
        let mw_decl = match self.middlewares.get(&mw_name.to_lowercase()) {
            Some(decl) => *decl,
            None => return Ok(()),
        };
        let Some(after) = mw_decl.after_body.as_ref() else {
            return Ok(());
        };
        let stmts = after.clone();
        let mut vars: HashMap<String, Value> = HashMap::new();
        let _ = self.exec_block(&stmts, &mut vars).await?;
        Ok(())
    }

    #[async_recursion]
    pub(super) async fn eval_dispatch_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("dispatch(method, path) expects exactly 2 args");
        }

        let method = self.eval_expr(&args[0], vars).await?;
        let path = self.eval_expr(&args[1], vars).await?;

        let method = match method {
            Value::Str(v) => v.to_ascii_uppercase(),
            other => bail!(
                "dispatch(method, path): method must be string, got {}",
                other.type_name()
            ),
        };

        let path = match path {
            Value::Str(v) => v,
            other => bail!(
                "dispatch(method, path): path must be string, got {}",
                other.type_name()
            ),
        };

        // Routes are owned data (Vec<RouteDecl>) since mount-expansion now
        // produces fresh copies. We pick the index here, then clone what we
        // need before any &mut self call to keep the borrow checker happy.
        let mut matched: Option<(usize, std::collections::HashMap<String, String>)> = None;
        for (i, route) in self.routes.iter().enumerate() {
            if !method.eq_ignore_ascii_case(&route.method) {
                continue;
            }
            if let Some(params) = match_route_pattern(&route.path, &path) {
                matched = Some((i, params));
                break;
            }
        }

        if let Some((idx, params)) = matched {
            let handler: Option<String> = self.routes[idx].handler.clone();
            let body_stmts: Vec<Stmt> = self.routes[idx].body.clone();
            let route_namespace: Vec<String> = self.routes[idx].namespace.clone();

            let previous = self.current_path_params.take();
            self.current_path_params = Some(params);
            self.current_namespace_stack.push(route_namespace);

            if let Some(handler_name) = handler {
                let args = self.build_handler_args(&handler_name);
                let result = self.call_function(&handler_name, args).await?;
                if let Some(value) = result {
                    self.output.push_str(&value.as_string());
                    self.output.push('\n');
                }
            } else {
                let mut route_vars = HashMap::new();
                let flow = self.exec_block(&body_stmts, &mut route_vars).await?;
                match flow {
                    Flow::Break | Flow::ContinueLoop => {
                        self.current_path_params = previous;
                        self.current_namespace_stack.pop();
                        bail!("break/continue cannot be used at route top-level");
                    }
                    Flow::Return(Some(value)) => {
                        self.output.push_str(&value.as_string());
                        self.output.push('\n');
                    }
                    Flow::Return(None) => {
                        self.output.push_str("null\n");
                    }
                    Flow::Continue => {}
                }
            }

            self.current_path_params = previous;
            self.current_namespace_stack.pop();
            return Ok(Value::Bool(true));
        }

        self.output.push_str(&format!(
            "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{}\",\"path\":\"{}\"}}\n",
            method, path
        ));
        Ok(Value::Bool(false))
    }

    /// Run the project's top-level `errorHandler (e) { ... }` block with
    /// `e` bound to the error envelope. Returns the handler's response body
    /// string (post-`as_string()`), or `null` if the handler didn't return.
    pub(super) async fn run_error_handler(
        &mut self,
        handler: &ErrorHandlerDecl,
        err: &anyhow::Error,
    ) -> Result<String> {
        let mut all: Vec<String> = err.chain().map(|c| c.to_string()).collect();
        let message = if all.is_empty() {
            "unknown error".to_string()
        } else {
            all.remove(0)
        };
        let payload = json!({
            "message": message,
            "causes": all,
        });
        let mut handler_vars: HashMap<String, Value> = HashMap::new();
        handler_vars.insert(
            handler.catch_var.to_lowercase(),
            Value::Str(payload.to_string()),
        );

        let flow = self.exec_block(&handler.body, &mut handler_vars).await?;
        Ok(match flow {
            Flow::Return(Some(v)) => v.as_string(),
            Flow::Return(None) => "null".to_string(),
            _ => "null".to_string(),
        })
    }

    /// Look up the primary-key column names for `table` (matching the entity
    /// name or its snake_case form). Falls back to `["id"]` when no `pk` is
    /// declared, so ad-hoc tables not modelled as JWC entities still work.
    pub(super) fn resolve_pk_fields(&self, table: &str) -> Vec<String> {
        let lc = table.to_lowercase();
        if let Some(pks) = self.pk_by_table.get(&lc) {
            if !pks.is_empty() {
                return pks.clone();
            }
        }
        let snake = crate::sql::to_snake_case(table).to_lowercase();
        if let Some(pks) = self.pk_by_table.get(&snake) {
            if !pks.is_empty() {
                return pks.clone();
            }
        }
        vec!["id".to_string()]
    }

    /// Execute a middleware by name. Returns `Some(response_body)` when the
    /// middleware short-circuits the request (returns a value), otherwise
    /// `None` to fall through to the route handler.
    pub(super) async fn run_middleware(&mut self, name: &str) -> Result<Option<String>> {
        let mw = self
            .middlewares
            .get(&name.to_lowercase())
            .copied()
            .ok_or_else(|| anyhow!("Unknown middleware: {name}"))?;

        let mw_body: Vec<Stmt> = mw.body.clone();
        let mut mw_vars = HashMap::new();
        let flow = self.exec_block(&mw_body, &mut mw_vars).await?;

        // Middleware contract:
        //   no return / bare `return;` / `return null;`  → continue to next
        //   middleware (or handler)
        //   `return <value>;` with a non-null Value     → short-circuit and
        //   send that value as the HTTP response body
        match flow {
            Flow::Continue => Ok(None),
            Flow::Return(None) => Ok(None),
            Flow::Return(Some(Value::Null)) => Ok(None),
            Flow::Return(Some(v)) => Ok(Some(v.as_string())),
            Flow::Break | Flow::ContinueLoop => {
                bail!("'break'/'continue' cannot be used at middleware top level")
            }
        }
    }

    /// Build the argument list for a `route ... -> handler;` style call by
    /// matching the handler's declared parameter names against the current path
    /// and query params. Missing values become `Value::Null`; `check_param_type`
    /// later coerces strings to the declared type.
    pub(super) fn build_handler_args(&self, handler_name: &str) -> Vec<Value> {
        let Some(handler) = self.functions.get(&handler_name.to_lowercase()).copied() else {
            return Vec::new();
        };

        handler
            .params
            .iter()
            .map(|param| {
                let key = param.name.clone();
                let from_path = self
                    .current_path_params
                    .as_ref()
                    .and_then(|m| m.get(&key).or_else(|| m.get(&key.to_lowercase())))
                    .cloned();
                if let Some(s) = from_path {
                    return Value::Str(s);
                }

                let from_query = self
                    .current_query_params
                    .as_ref()
                    .and_then(|m| m.get(&key).or_else(|| m.get(&key.to_lowercase())))
                    .cloned();
                match from_query {
                    Some(s) => Value::Str(s),
                    None => Value::Null,
                }
            })
            .collect()
    }
}

/// Split `"/items?limit=10&q=hi"` into `("/items", { "limit": "10", "q": "hi" })`.
/// Percent-decoding is intentionally not done here — keep it lazy for now.
pub(super) fn split_path_and_query(raw: &str) -> (String, HashMap<String, String>) {
    let mut iter = raw.splitn(2, '?');
    let path = iter.next().unwrap_or("").to_string();
    let mut params = HashMap::new();

    if let Some(query) = iter.next() {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            match pair.split_once('=') {
                Some((k, v)) if !k.is_empty() => {
                    params.insert(k.to_string(), v.to_string());
                }
                None if !pair.is_empty() => {
                    params.insert(pair.to_string(), String::new());
                }
                _ => {}
            }
        }
    }

    (path, params)
}

pub(super) fn match_route_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_segments: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path_segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if pattern_segments.len() != path_segments.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (p, v) in pattern_segments.iter().zip(path_segments.iter()) {
        if p.starts_with('{') && p.ends_with('}') && p.len() > 2 {
            let key = p.trim_start_matches('{').trim_end_matches('}').to_string();
            params.insert(key, (*v).to_string());
            continue;
        }

        if p != v {
            return None;
        }
    }

    Some(params)
}

/// Append `; charset=utf-8` to text-ish MIME types that don't already declare a
/// charset; binary/other types pass through verbatim. Mirrors the native
/// `jwc_normalize_content_type` helper so both runtimes set identical headers.
pub(super) fn normalize_content_type(mime: &str) -> String {
    if mime.to_ascii_lowercase().contains("charset") {
        mime.to_string()
    } else if mime.starts_with("text/") {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    }
}

/// Build a 200 HTTP response envelope carrying a raw body under an explicit
/// Content-Type. Shared by `response`/`raw`/`text`/`html`. The two sentinel
/// keys are recognised and stripped by `dispatch_route`.
pub(super) fn content_type_response(body: String, mime: &str) -> Value {
    let envelope = json!({
        "status": 200,
        "__jwc_content_type__": normalize_content_type(mime),
        "__jwc_body__": body,
    });
    Value::Str(envelope.to_string())
}
