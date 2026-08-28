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

        // `mail.*` opens an SMTP session, so it is async for the same
        // reason again.
        if let Some(name) = path.strip_prefix("mail.") {
            return self.mail_call(name, &vals).await;
        }

        // `http.*` is a network round trip, so it is async too.
        if let Some(name) = path.strip_prefix("http.") {
            return self.http_call(name, &vals).await;
        }

        // `jwt.verify_jwks` fetches the provider's key set — a network
        // round trip on the first call and on a rotation, cached in
        // between. The synchronous `builtin` table below cannot await, so
        // it is dispatched here beside the other three.
        if path == "jwt.verify_jwks" {
            return self.jwt_verify_jwks(&vals).await;
        }

        // Yields the runtime rather than blocking a worker thread, which
        // is the whole difference between a script pausing and a server
        // stalling. The checker refuses it inside a request anyway.
        if path == "sleep_ms" {
            let ms = vals.first().and_then(|v| v.as_i64()).unwrap_or(0).max(0);
            tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            return Ok(Value::Null);
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
        // The ceiling is here rather than on expression nesting because
        // this is the recursion that runs out of machine stack: a JWC call
        // frame is a chain of boxed futures, and polling it costs the
        // whole chain's depth. Without this a recursive function did not
        // report anything — it overflowed the thread's stack and aborted
        // the process, taking every other in-flight request with it.
        self.enter_call(path)?;
        let saved = self.enter_function();
        for (i, p) in f.params.iter().enumerate() {
            let v = args.get(i).cloned().unwrap_or(Value::Null);
            self.bind_param(&p.name.name, v);
        }
        let r = Box::pin(self.run_body(&f.body)).await;
        self.leave_function(saved);
        self.leave_call();
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
    /// builtins.md §7c — outbound HTTP.
    ///
    /// A non-2xx is **not** a raise: a 404 from a remote service is an
    /// answer, and a language that turns it into a fault forces every
    /// caller to wrap the call to find out. `http.status` is how to ask.
    ///
    /// What does raise is the request never happening — an SSRF gate
    /// refusing it, DNS failing, the timeout expiring. `BadRequest` so it
    /// is catchable, because a remote service being unreachable is not the
    /// program being wrong.
    /// `jwt.verify_jwks(token, jwks_url)` — RS256 against an OIDC
    /// provider's published key set.
    ///
    /// `src/jwks.rs` — 395 lines with a cache, a negative cache and a
    /// refetch-storm guard — has shipped in every binary since it was
    /// written and no program could call it: the checker had no arm, so
    /// `jwt.verify_jwks(...)` was `E0204`, "unknown function". The native
    /// prelude carries the whole thing a second time, equally unreachable.
    ///
    /// The answer is `jwt.verify`'s: `Record{sub, exp, iat}?`, null for a
    /// token that does not verify. A **fetch** failure is not null — an
    /// unreachable identity provider is an outage, and answering null
    /// would report it as "every credential is wrong".
    async fn jwt_verify_jwks(&mut self, a: &[Value]) -> Exec<Value> {
        let token = text(a.first().unwrap_or(&Value::Null));
        let url = text(a.get(1).unwrap_or(&Value::Null));

        // Same outbound gate as `http.*`: a JWKS URL is a URL the program
        // supplies, and an identity provider on a private network needs
        // the block left off.
        if let Err(e) = crate::http::check_url(&url) {
            return Err(fault(e.to_string()));
        }

        // The `kid` selects the key and comes from the *unverified*
        // header. `jwks::rsa_key_for` is what keeps an attacker-chosen
        // `kid` from turning into an outbound fetch per request.
        let kid = crate::jwt::split_token(&token)
            .ok()
            .and_then(|t| t.kid().map(str::to_string));

        let key = match crate::jwks::rsa_key_for(&url, kid.as_deref()).await {
            Ok(k) => k,
            Err(e) => return Err(fault(format!("jwt.verify_jwks: {e}"))),
        };

        Ok(match crate::jwt::verify_rs256(&token, &key.n, &key.e) {
            Ok(payload) => jwt_claims_record(&payload),
            Err(_) => Value::Null,
        })
    }

    async fn http_call(&mut self, name: &str, a: &[Value]) -> Exec<Value> {
        let s = |i: usize| text(a.get(i).unwrap_or(&Value::Null));
        let (method, body) = match name {
            "post" => ("POST", Some(s(1))),
            "get" | "json" | "status" => ("GET", None),
            other => {
                return Err(fault(format!(
                    "unknown function `http.{other}`. There is get, post, json \
                     and status (builtins.md §7c)."
                )))
            }
        };

        match crate::http::request(method, &s(0), body).await {
            Ok(reply) => Ok(match name {
                "status" => Value::Int(reply.status as i64),
                "json" => Value::Raw(reply.body),
                _ => Value::Text(reply.body),
            }),
            Err(e) => Err(Abort::Thrown(Thrown {
                error: "BadRequest".into(),
                args: vec![Value::Text(e.to_string())],
            })),
        }
    }

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
            "exists" => r::exists(&s(0)).await.map(Value::Bool),
            "ping" => r::ping().await.map(|_| Value::Bool(true)),
            // The primitive `rate_limit` is built on. A program that needs
            // a different atomic sequence had no way to write one; keys
            // and args arrive as JSON arrays because the language has no
            // varargs.
            "eval" => {
                let list = |raw: String| -> Vec<String> {
                    serde_json::from_str::<Vec<serde_json::Value>>(&raw)
                        .map(|vs| {
                            vs.into_iter()
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s,
                                    other => other.to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                r::eval(&s(0), &list(s(1)), &list(s(2)))
                    .await
                    .map(|v| v.map(Value::Text).unwrap_or(Value::Null))
            }
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

    /// The `mail` package surface (builtins.md §8), over
    /// [`crate::mail`].
    ///
    /// `mail.send` used to be a one-line stub in the synchronous table:
    /// `"mail.send" => Value::Null`. It typechecked, it ran, and it
    /// delivered nothing — a password-reset route was silently a no-op.
    /// Like `redis.*`, an unconfigured relay raises rather than answering:
    /// `mail.enabled()` is what to branch on when the send is optional.
    async fn mail_call(&mut self, name: &str, a: &[Value]) -> Exec<Value> {
        let s = |i: usize| text(a.get(i).unwrap_or(&Value::Null));

        if name == "enabled" {
            return Ok(Value::Bool(crate::mail::is_configured()));
        }
        if name != "send" {
            return Err(fault(format!(
                "unknown function `mail.{name}`. The package provides send \
                 and enabled (builtins.md §8)."
            )));
        }
        if !crate::mail::is_configured() {
            return Err(fault(
                "`mail.send(...)` needs an SMTP relay: set JWC_SMTP_HOST, \
                 JWC_SMTP_USER, JWC_SMTP_PASSWORD and JWC_SMTP_FROM. \
                 `mail.enabled()` is what to branch on when the send is \
                 optional.",
            ));
        }
        crate::mail::send(&s(0), &s(1), &s(2))
            .await
            .map_err(|e| fault(format!("{e:#}")))?;
        Ok(Value::Null)
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
            // builtins.md §7f — the last of 0.9's registry.
            "unix_timestamp" => Value::Bigint(chrono::Utc::now().timestamp()),
            // Inclusive low, exclusive high, so `random_int(0, len)` is an
            // index. A backwards or empty range answers the low bound
            // rather than panicking on an empty sample.
            "random_int" => {
                let lo = a.first().and_then(|v| v.as_i64()).unwrap_or(0);
                let hi = a.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                Value::Int(if hi > lo {
                    lo + (rand::RngCore::next_u64(&mut rand::thread_rng()) % ((hi - lo) as u64))
                        as i64
                } else {
                    lo
                })
            }
            "array.take" => {
                let n = a.get(1).and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
                match a.first() {
                    Some(Value::Array(items)) => {
                        Value::Array(items.iter().take(n).cloned().collect())
                    }
                    other => other.cloned().unwrap_or(Value::Null),
                }
            }
            // Answers a new array rather than mutating: a JWC value is not
            // a reference, and a `push` that appeared to mutate one would
            // be the only place in the language where it did.
            "array.push" => match a.first() {
                Some(Value::Array(items)) => {
                    let mut out = items.clone();
                    out.push(a.get(1).cloned().unwrap_or(Value::Null));
                    Value::Array(out)
                }
                other => other.cloned().unwrap_or(Value::Null),
            },
            "array.range" => {
                let lo = a.first().and_then(|v| v.as_i64()).unwrap_or(0);
                let hi = a.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                Value::Array((lo..hi).map(Value::Int).collect())
            }

            // builtins.md §7e — the filesystem. The checker refuses these
            // outside a plain `function` (§7e.1), so nothing here has to
            // re-litigate where it was called from.
            //
            // A missing file is `null`, not a raise: "is it there" is the
            // question `file.exists` answers, and making `read` raise
            // forces a catch around the ordinary case.
            "file.read" => match std::fs::read_to_string(s(0)) {
                Ok(text) => Value::Text(text),
                Err(_) => Value::Null,
            },
            "file.size" => match std::fs::metadata(s(0)) {
                Ok(m) => Value::Bigint(m.len() as i64),
                Err(_) => Value::Null,
            },
            "file.exists" => Value::Bool(std::path::Path::new(&s(0)).is_file()),
            "file.delete" => Value::Bool(std::fs::remove_file(s(0)).is_ok()),
            "file.write" => Value::Bool(std::fs::write(s(0), s(1)).is_ok()),
            "file.append" => {
                use std::io::Write as _;
                let ok = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(s(0))
                    .and_then(|mut f| f.write_all(s(1).as_bytes()))
                    .is_ok();
                Value::Bool(ok)
            }
            "directory.exists" => Value::Bool(std::path::Path::new(&s(0)).is_dir()),
            "directory.create" => Value::Bool(std::fs::create_dir_all(s(0)).is_ok()),
            // Sorted, because the order a filesystem hands entries back in
            // is not stable and a program that iterates one should not
            // depend on it.
            "directory.list" => {
                let mut names: Vec<String> = std::fs::read_dir(s(0))
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                names.sort();
                Value::Array(names.into_iter().map(Value::Text).collect())
            }

            // builtins.md §7d — JSON. `parse` answers `Raw`, which is the
            // same thing a `jsonb` column reads as: it splices into a
            // response and is not read field-wise. That is the honest
            // shape for text whose structure the compiler cannot know.
            "json.parse" => {
                let raw = text(&arg(0));
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(v) => Value::Raw(v.to_string()),
                    Err(e) => {
                        return Err(Abort::Thrown(Thrown {
                            error: "BadRequest".into(),
                            args: vec![Value::Text(format!("not JSON: {e}"))],
                        }))
                    }
                }
            }
            "json.stringify" => {
                let mut out = String::new();
                arg(0).write_json(&mut out);
                Value::Text(out)
            }

            // builtins.md §7b — the terminal. `write` leaves the cursor
            // where it is so a prompt can be answered on the same line,
            // which is the whole reason it is separate from `writeln`.
            // Both flush: a prompt that appears after the answer was due
            // is what 0.9's buffered `print` did, and why it is not back.
            "console.write" | "console.writeln" | "console.error" => {
                use std::io::Write as _;
                let text = arg(0).display_text();
                let newline = path == "console.writeln";
                if path == "console.error" {
                    eprint!("{text}");
                    let _ = std::io::stderr().flush();
                } else {
                    print!("{text}{}", if newline { "\n" } else { "" });
                    let _ = std::io::stdout().flush();
                }
                Value::Null
            }
            // `null` at EOF, so `while (console.read() != null)` ends.
            "console.read" => {
                let mut line = String::new();
                match std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line) {
                    Ok(0) | Err(_) => Value::Null,
                    Ok(_) => Value::Text(
                        line.trim_end_matches('\n')
                            .trim_end_matches('\r')
                            .to_string(),
                    ),
                }
            }

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

            // routing.md §6.5 — verbatim body, declared type. Every other
            // builder JSON-encodes, which for an HTML page means the browser
            // is handed a quoted string.
            "content" => Value::Response {
                status: 200,
                body: s(1),
                headers: vec![("content-type".into(), crate::check::normalize_media(&s(0)))],
            },
            "text" | "html" => Value::Response {
                status: 200,
                body: s(0),
                headers: vec![(
                    "content-type".into(),
                    crate::check::normalize_media(if path == "text" {
                        "text/plain; charset=utf-8"
                    } else {
                        "text/html; charset=utf-8"
                    }),
                )],
            },

            // The program's own statement of where it listens. `main` is
            // evaluated at boot precisely so this runs — the argument is an
            // expression, and `serve(int(env("PORT") ?? "8080"))` is the
            // form the spec's sample uses.
            "serve" => {
                self.serve_port = Some(n(0) as u16);
                Value::Null
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
            "string.escape_html" => Value::Text(escape_html(&s(0))),
            "string.escape_url" => Value::Text(escape_url(&s(0))),
            "string.matches" => {
                Value::Bool(crate::validate::jwc_regex_is_match_strict(&s(1), &s(0)))
            }
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
            "hash.sha1" => Value::Text(crate::hash::sha1_hex(&s(0))),
            "hash.md5" => Value::Text(crate::hash::md5_hex(&s(0))),
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
                Ok(payload) => jwt_claims_record(&payload),
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
            "response.duration_us" => Value::Bigint(self.response_micros.unwrap_or(0) as i64),
            // Whole milliseconds, truncated. A route that answers in 400us
            // reports 0, which is the honest answer at this resolution and
            // is why `duration_us` exists next to it.
            "response.duration_ms" => {
                Value::Bigint((self.response_micros.unwrap_or(0) / 1_000) as i64)
            }
            "response.set_header" | "response.add_header" => {
                self.extra_headers.push((s(0), s(1)));
                Value::Null
            }

            // ---- packages (builtins.md §8)
            //
            // `redis.*` and `mail.*` are handled ahead of this table, in
            // `redis_call` and `mail_call` — they are async. `cache.*` is
            // a mutex and a map, so it belongs here.
            // ---- sockets (builtins.md §9). Queued, not written: the
            // connection task owns the socket's write half, and a handler
            // that panicked mid-frame would otherwise leave the peer
            // reading a partial message forever.
            "socket.send" => {
                if let Some(out) = self.socket_out.as_mut() {
                    out.push(crate::exec::SocketOut::Text(s(0)));
                }
                Value::Null
            }
            "socket.close" => {
                if let Some(out) = self.socket_out.as_mut() {
                    out.push(crate::exec::SocketOut::Close);
                }
                Value::Null
            }

            "cache.get" => match crate::cache::get(&s(0)) {
                Some(v) => Value::Text(v),
                None => Value::Null,
            },
            // A negative TTL is a caller mistake, not "expire in the past";
            // 0 already means "no expiry". `redis.set` clamps the same way.
            "cache.set" => {
                let ttl = a.get(2).and_then(|v| v.as_i64()).unwrap_or(0).max(0);
                crate::cache::set(&s(0), &s(1), ttl as u64);
                Value::Bool(true)
            }
            "cache.del" => Value::Int(crate::cache::del(&s(0))),
            "cache.clear" => {
                crate::cache::clear();
                Value::Null
            }

            _ => return Ok(None),
        }))
    }
}

/// `{sub, exp, iat}?` from a verified JWT payload — the answer both
/// `jwt.verify` (HS256) and `jwt.verify_jwks` (RS256) give, so a caller
/// can move from a shared secret to an identity provider without
/// rewriting the code that reads the claims.
///
/// A token whose signature checks out and whose `exp` has passed is null,
/// not an error: builtins.md §6 makes invalid and expired the same answer.
fn jwt_claims_record(payload: &str) -> Value {
    let Ok(j) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Value::Null;
    };
    let exp = j.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    if exp != 0 && exp < chrono::Utc::now().timestamp() {
        return Value::Null;
    }
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
            Value::Bigint(j.get("iat").and_then(|v| v.as_i64()).unwrap_or(0)),
        ),
    ])
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

// The escapers are a file the native backend pastes into the crate it
// generates, so an escaped string is the same string from either backend
// (builtins.md §4a).
include!("escape_core.rs.in");
