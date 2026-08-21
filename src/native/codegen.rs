//! The 1.0 AST to Rust.
//!
//! Written against `crate::ast`, not ported from the 0.9.x codegen — that
//! one named `RouteDecl` with a bare path, `MountDecl`, `ModelKind` and
//! `validate body`, and none of those exist. What *is* carried over is the
//! shape of the emission: one Rust `async fn` per route, values as the
//! prelude's `V`, and built-ins as `jwc_b_*` calls into the prelude.
//!
//! ## Scope
//!
//! This pass covers the tier that needs no database: routes, control flow,
//! expressions, and the built-ins the prelude already implements. Anything
//! outside it is refused by [`reject_unsupported`] with the construct
//! named, because a native build that silently dropped a query would be far
//! worse than one that will not start.
//!
//! Queries are the next tier and they are cheap here, which is the reason
//! this backend is worth rebuilding at all: `query_sql` already lowers a
//! query to a SQL string and a parameter list at compile time, so codegen
//! embeds the same string the interpreter sends. There is no second query
//! compiler and no semantics that can drift.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

use crate::ast::{BinOp, Block, Decl, Expr, ExprKind, ObjEntry, SetItem, Stmt, UnaryOp};
use crate::model::SchemaModel;
use crate::workspace::Workspace;

/// The prelude built-ins declared `async fn`. Emitting `.await` on a
/// plain value does not compile, and omitting it where one is needed
/// yields a future where a `V` was wanted, so the list is derived from
/// the prelude rather than guessed.
const ASYNC_BUILTINS: &[&str] = &[
    "jwc_b_v1_redis_del",
    "jwc_b_v1_redis_enabled",
    "jwc_b_v1_redis_expire",
    "jwc_b_v1_redis_get",
    "jwc_b_v1_redis_incr",
    "jwc_b_v1_redis_rate_limit",
    "jwc_b_v1_redis_set",
    "jwc_b_console_read",
    "jwc_b_directory_create",
    "jwc_b_directory_delete",
    "jwc_b_directory_exists",
    "jwc_b_directory_list",
    "jwc_b_fetch_json",
    "jwc_b_file_append",
    "jwc_b_file_copy",
    "jwc_b_file_delete",
    "jwc_b_file_exists",
    "jwc_b_file_lines",
    "jwc_b_file_move",
    "jwc_b_file_read",
    "jwc_b_file_size",
    "jwc_b_file_write",
    "jwc_b_http_get",
    "jwc_b_jwt_verify_jwks",
    "jwc_b_raw_sql",
    "jwc_b_redis_del",
    "jwc_b_redis_enabled",
    "jwc_b_redis_eval",
    "jwc_b_redis_exists",
    "jwc_b_redis_expire",
    "jwc_b_redis_get",
    "jwc_b_redis_incr",
    "jwc_b_redis_ping",
    "jwc_b_redis_set",
    "jwc_b_setConnectionString",
    "jwc_b_set_connection_string",
    "jwc_b_sleep_ms",
    "jwc_b_ws_close",
    "jwc_b_ws_recv",
    "jwc_b_ws_send",
];

/// The 1.0 built-in name on the left, the prelude function on the right.
///
/// The prelude predates the 1.0 vocabulary, so its names are the 0.9.x
/// ones: `jwc_b_lower`, not `string.lower`. Mapping here rather than
/// renaming 5,030 lines of working runtime keeps the diff on the side that
/// is actually changing.
fn prelude_fn(name: &str) -> Option<&'static str> {
    Some(match name {
        // Responses — routing.md §6.1.
        "json" => "jwc_b_json",
        "created" => "jwc_b_created",
        "noContent" => "jwc_b_no_content",
        "badRequest" => "jwc_b_bad_request",
        "unauthorized" => "jwc_b_unauthorized",
        "forbidden" => "jwc_b_forbidden",
        "notFound" => "jwc_b_not_found",
        "internalError" => "jwc_b_internal_error",
        "statusCode" => "jwc_b_status_code",
        "accepted" => "jwc_b_accepted",
        "conflict" => "jwc_b_conflict",
        "tooManyRequests" => "jwc_b_too_many_requests",
        "redirect" => "jwc_b_redirect",
        "content" => "jwc_b_content",

        // Text — builtins.md §4.
        "string.lower" => "jwc_b_lower",
        "string.upper" => "jwc_b_upper",
        "string.trim" => "jwc_b_trim",
        "string.replace" => "jwc_b_replace",
        "string.split" => "jwc_b_split",
        "string.join" => "jwc_b_join",
        "string.len" => "jwc_b_length",
        "string.contains" => "jwc_b_contains",
        "string.starts_with" => "jwc_b_starts_with",
        "string.ends_with" => "jwc_b_ends_with",

        // Arrays — builtins.md §5.
        "array.len" => "jwc_b_len",
        "array.first" => "jwc_b_first",
        "array.last" => "jwc_b_last",
        "array.contains" => "jwc_b_contains",

        // Hashing and tokens — builtins.md §6.
        "hash.password" => "jwc_b_hash_password",
        "hash.sha256" => "jwc_b_sha256",
        "hash.hmac_sha256" => "jwc_b_hmac_sha256",
        "jwt.sign" => "jwc_b_v1_jwt_sign",
        "jwt.verify" => "jwc_b_jwt_verify",

        // The request — builtins.md §7.
        "request.header" => "jwc_b_header",
        "request.query" => "jwc_b_query_param",
        "request.method" => "jwc_b_request_method",
        "request.path" => "jwc_b_request_path",
        "request.id" => "jwc_b_request_id",
        "request.client_ip" => "jwc_b_client_ip",
        "request.raw_body" => "jwc_b_request_body",
        "response.status" => "jwc_b_response_status",
        "response.duration_ms" => "jwc_b_response_duration_ms",
        "response.duration_us" => "jwc_b_response_duration_us",
        "response.set_header" => "jwc_b_response_set_header",
        "response.add_header" => "jwc_b_response_add_header",
        "request.route" => "jwc_b_request_route",

        // Coercions and the environment — builtins.md §2.
        "int" => "jwc_b_v1_int",
        "bigint" => "jwc_b_v1_bigint",
        "numeric" => "jwc_b_v1_numeric",
        "boolean" => "jwc_b_v1_boolean",
        "uuid" => "jwc_b_v1_uuid",
        "timestamptz" => "jwc_b_v1_timestamptz",
        "enum" => "jwc_b_v1_enum",
        "env" => "jwc_b_env",

        // Date — builtins.md §3.
        "date.now" => "jwc_b_v1_date_now",
        "date.today" => "jwc_b_v1_date_today",
        "date.days" => "jwc_b_v1_date_days",
        "date.hours" => "jwc_b_v1_date_hours",
        "date.minutes" => "jwc_b_v1_date_minutes",
        "date.seconds" => "jwc_b_v1_date_seconds",
        "date.parse" => "jwc_b_v1_date_parse",
        "date.format" => "jwc_b_v1_date_format",

        // The rest of text — builtins.md §4.
        "string.of" => "jwc_b_v1_string_of",
        "string.slice" => "jwc_b_v1_string_slice",
        "string.pad_left" => "jwc_b_v1_string_pad_left",
        "string.pad_right" => "jwc_b_v1_string_pad_right",
        "string.matches" => "jwc_b_v1_string_matches",
        "string.split_csv" => "jwc_b_v1_string_split_csv",
        "string.strip_prefix" => "jwc_b_v1_string_strip_prefix",

        // The rest of arrays — builtins.md §5.
        "array.is_empty" => "jwc_b_v1_array_is_empty",
        "array.sum" => "jwc_b_v1_array_sum",
        "array.sum_product" => "jwc_b_v1_array_sum_product",
        "array.min" => "jwc_b_v1_array_min",
        "array.max" => "jwc_b_v1_array_max",
        "array.pluck" => "jwc_b_v1_array_pluck",
        "array.sorted" => "jwc_b_v1_array_sorted",

        // The rest of hashing — builtins.md §6.
        "hash.verify" => "jwc_b_v1_hash_verify",
        "hash.hmac_verify" => "jwc_b_v1_hmac_verify",
        "crypto.token" => "jwc_b_v1_crypto_token",
        "crypto.constant_time_eq" => "jwc_b_v1_constant_time_eq",

        // Redis — builtins.md §8.
        "redis.get" => "jwc_b_v1_redis_get",
        "redis.set" => "jwc_b_v1_redis_set",
        "redis.del" => "jwc_b_v1_redis_del",
        "redis.incr" => "jwc_b_v1_redis_incr",
        "redis.expire" => "jwc_b_v1_redis_expire",
        "redis.rate_limit" => "jwc_b_v1_redis_rate_limit",
        "redis.enabled" => "jwc_b_v1_redis_enabled",

        // The rest of the request — builtins.md §7.
        "request.query_all" => "jwc_b_v1_request_query_all",
        "request.peer_ip" => "jwc_b_v1_request_peer_ip",
        "debug.dump" => "jwc_b_v1_debug_dump",

        _ => return None,
    })
}

/// Built-ins the 1.0 language has and the restored prelude does not.
///
/// Named individually so the refusal says which one, and so the list is a
/// worklist rather than a shrug. Each is a prelude addition, not a codegen
/// problem.
const PRELUDE_GAPS: &[&str] = &[
    // Typed by the checker (`check.rs`), implemented by neither backend:
    // `exec_call.rs` has no arm for it either, so `jwc serve` does not run
    // this one. Named here so the refusal is honest about which it is.
    "date.add",
    // A query construct: `raw(sql, …)` runs SQL, which this pass reaches
    // through `query_sql` rather than through the built-in table.
    "raw",
];

/// Refuse a program this pass cannot lower, naming the construct.
///
/// The old backend had the same gate and the same reason: a native binary
/// that quietly dropped a route, a query or a middleware would be a worse
/// outcome than one that refuses to build. `jwc serve` runs everything.
pub fn reject_unsupported(ws: &Workspace) -> Result<()> {
    let mut blocked: Vec<String> = Vec::new();
    for file in &ws.files {
        for decl in &file.program.decls {
            // A view is a named query the model resolves like a table;
            // `query_sql` lowers reads through it, but the DDL that creates
            // it is `jwc migrate`'s job, not the binary's.
            if let Decl::View(d) = decl {
                blocked.push(format!("view `{}`", d.name.name));
            }
        }
    }
    blocked.sort();
    blocked.dedup();
    if !blocked.is_empty() {
        bail!(
            "native build does not cover this program yet:\n  {}\n\n\
             `jwc serve` runs the whole language today.",
            blocked.join("\n  ")
        );
    }
    Ok(())
}

/// What kind of body is being emitted. Only `return` differs, and it
/// differs the way `Flow` does in the interpreter: a middleware that
/// returns is short-circuiting the chain, which is not the same event as a
/// route handler returning its response.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Route handler, `service`/top-level function, `after` block:
    /// `async fn(..) -> JwcResult`.
    Value,
    /// Middleware body: `async fn() -> Result<Option<V>, JwcThrown>`,
    /// where `None` is "fell through" (middleware.md §4.2).
    Middleware,
}

struct Ctx<'a> {
    /// The schema, so a query can be lowered to SQL here rather than at
    /// run time. This is the whole reason the backend is cheap to rebuild:
    /// `query_sql` produces the statement the interpreter would send, so
    /// there is no second query compiler to keep in step.
    model: &'a SchemaModel,
    symbols: &'a crate::symbols::Symbols,
    max_page_size: i64,
    /// Object-literal shapes, interned so the field-name `Arc` is allocated
    /// once per distinct key set rather than per construction.
    shapes: BTreeMap<Vec<String>, usize>,
    /// Projected field orders, interned the same way: a query's response
    /// key order is fixed at compile time (queries.md §6.1).
    field_lists: BTreeMap<Vec<String>, usize>,
    /// Every prelude function this program reached. Which prelude files —
    /// and therefore which cargo dependencies — the crate needs is read off
    /// this rather than from a hand-kept list, so a new built-in cannot be
    /// added without its dependency following it.
    used: std::collections::BTreeSet<String>,
    /// Set by a `request.body() as <Class>`. Drives whether the class table
    /// is worth emitting and whether the crate needs `regex`.
    uses_validation: bool,
    /// Set by a `page` query. The cursor is HMAC-signed, so the crate needs
    /// the crypto prelude whether or not the program hashes anything.
    uses_page: bool,
    /// Locals whose type is a declared `class`, from the two places the AST
    /// says so outright: a typed function parameter, and
    /// `let x = request.body() as C`. A `...` spread's columns come from
    /// this, and nothing else in the pass needs a type.
    classes_in_scope: BTreeMap<String, String>,
    mode: Mode,
    /// How many `transaction { }` blocks enclose the statement being
    /// emitted. Each one is an `async` block, so a `return` inside it has
    /// to travel out through every layer rather than exiting the closest.
    tx_depth: usize,
}

impl Ctx<'_> {
    fn shape_id(&mut self, keys: Vec<String>) -> usize {
        let next = self.shapes.len();
        *self.shapes.entry(keys).or_insert(next)
    }

    fn field_list_id(&mut self, fields: Vec<String>) -> usize {
        let next = self.field_lists.len();
        *self.field_lists.entry(fields).or_insert(next)
    }

    fn class_of_local(&self, name: &str) -> Option<String> {
        self.classes_in_scope.get(name).cloned()
    }

    /// errors.md §4.3 — the status a declared error carries. Resolved here,
    /// at compile time, so the binary needs no name → status map.
    fn error_status(&self, name: &str) -> Result<u16> {
        match self.symbols.errors.get(name) {
            Some(e) => Ok(e.status),
            // The checker rejects an undeclared error long before codegen;
            // reaching here means the symbol table and the AST disagree.
            None => bail!("`{name}` is not a declared error"),
        }
    }
}

/// A generated crate: the source, and which halves of the runtime it needs.
pub struct Generated {
    pub source: String,
    pub needs_db: bool,
    pub needs_http_client: bool,
    pub needs_crypto: bool,
    pub needs_redis: bool,
    pub needs_regex: bool,
}

/// Lower a checked workspace to a Rust source file.
pub fn generate(ws: &Workspace) -> Result<Generated> {
    reject_unsupported(ws)?;

    let built = crate::model::build(ws);
    let symbols = crate::symbols::build(ws, &built.model);
    // The routing table, the middleware chain per route and the `after`
    // order all come from `wiring`, which is the same module `serve.rs`
    // reads. Rebuilding any of it here is how the two backends would come
    // to disagree about which middleware ran first.
    let wired = crate::wiring::wire(ws, &symbols);
    // `server { max_page_size }` bounds a `limit`, and the query compiler
    // needs it to lower one. Read from the same declaration `serve.rs`
    // reads so the two backends cap at the same number.
    let mut server = crate::exec::ServerConfig::default();
    for file in &ws.files {
        for decl in &file.program.decls {
            if let Decl::Server(d) = decl {
                server = crate::serve::read_server_config(d);
            }
        }
    }

    let mut ctx = Ctx {
        model: &built.model,
        symbols: &symbols,
        max_page_size: server.max_page_size,
        shapes: BTreeMap::new(),
        field_lists: BTreeMap::new(),
        used: std::collections::BTreeSet::new(),
        uses_validation: false,
        uses_page: false,
        classes_in_scope: BTreeMap::new(),
        mode: Mode::Value,
        tx_depth: 0,
    };
    let mut out = String::new();

    out.push_str("\n// ── generated from the program ──\n");
    out.push_str(
        "\nstatic JWC_SERVE_PORT: ::std::sync::atomic::AtomicU16 = \
         ::std::sync::atomic::AtomicU16::new(8080);\n",
    );
    emit_constraint_messages(&mut out, &built.model);
    emit_cursor_secret(&mut out, ws);
    let uses_regex = emit_classes(&mut out, &symbols);

    // Route bodies are keyed by (method, declared pattern), the same key
    // `serve.rs` uses to find the body for a resolved route.
    let mut bodies: BTreeMap<(String, String), &Block> = BTreeMap::new();
    let mut middleware: BTreeMap<String, &crate::ast::MiddlewareDecl> = BTreeMap::new();

    for file in &ws.files {
        for decl in &file.program.decls {
            match decl {
                Decl::Routes(r) => {
                    for route in &r.routes {
                        let pattern = strip_types(&join_path(&r.prefix, &route.suffix));
                        bodies.insert((route.method.name.clone(), pattern), &route.body);
                    }
                }
                Decl::Middleware(m) => {
                    middleware.insert(m.name.name.clone(), m);
                }
                _ => {}
            }
        }
    }

    // --- functions, services -------------------------------------------------
    for file in &ws.files {
        for decl in &file.program.decls {
            match decl {
                Decl::Function(f) if f.name.name == "main" => {
                    ctx.mode = Mode::Value;
                    out.push_str("\nasync fn jwc_user_main() -> JwcResult {\n");
                    emit_block(&mut out, &f.body, 1, &mut ctx)?;
                    out.push_str("    Ok(V::Null)\n}\n");
                }
                Decl::Function(f) => emit_function(&mut out, &f.name.name, f, &mut ctx)?,
                Decl::Service(sv) => {
                    for f in &sv.functions {
                        let name = format!("{}.{}", sv.name.name, f.name.name);
                        emit_function(&mut out, &name, f, &mut ctx)?;
                    }
                }
                _ => {}
            }
        }
    }

    // --- middleware ----------------------------------------------------------
    for (name, m) in &middleware {
        ctx.mode = Mode::Middleware;
        out.push_str(&format!(
            "\n/// middleware `{name}`. `None` is a fall-through; `Some(r)` is\n\
             /// the response that short-circuits the chain (middleware.md §4.2).\n\
             async fn {}() -> Result<Option<V>, JwcThrown> {{\n",
            mw_fn(name)
        ));
        emit_block(&mut out, &m.body, 1, &mut ctx)?;
        out.push_str("    Ok(None)\n}\n");
        if let Some(after) = &m.after {
            ctx.mode = Mode::Value;
            out.push_str(&format!(
                "\nasync fn {}_after() -> JwcResult {{\n",
                mw_fn(name)
            ));
            emit_block(&mut out, after, 1, &mut ctx)?;
            out.push_str("    Ok(V::Null)\n}\n");
        }
    }
    ctx.mode = Mode::Value;

    // --- routes --------------------------------------------------------------
    let mut routes: Vec<(String, String, String)> = Vec::new();
    for route in &wired.routes {
        let name = handler_name(&route.method, &route.pattern);
        let Some(body) = bodies.get(&(route.method.clone(), route.pattern.clone())) else {
            bail!(
                "no body for {} {} — the router and the source disagree",
                route.method,
                route.pattern
            );
        };
        out.push_str(&format!("\nasync fn {name}_body() -> JwcResult {{\n"));
        emit_block(&mut out, body, 1, &mut ctx)?;
        out.push_str("    Ok(V::Null)\n}\n");
        emit_route_dispatch(&mut out, &name, route, &middleware);
        // `Router` stores `fn() -> Pin<Box<dyn Future>>`, a fn *pointer*,
        // and an `async fn` is a distinct fn *item* with an anonymous
        // future type, so it cannot coerce — hence the boxing wrapper.
        out.push_str(&format!(
            "\nfn {name}() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = V> + Send>> {{\n\
             \x20   Box::pin({name}_dispatch())\n}}\n"
        ));
        routes.push((
            route.method.to_uppercase(),
            typed_pattern(route),
            name.clone(),
        ));
    }

    // Which prelude a function came from decides which cargo dependency the
    // crate needs, and the prelude source is the only honest answer to that
    // question — a second list would be one more thing to forget.
    let defines = |prelude: &str, f: &str| prelude.contains(&format!("fn {f}("));
    let needs_db = ctx.used.iter().any(|f| defines(super::PRELUDE_DB, f));
    // A page's cursor is HMAC-signed, and the HMAC lives in the crypto
    // prelude — so a program that pages needs it even if it hashes nothing.
    let needs_crypto = ctx.uses_page || ctx.used.iter().any(|f| defines(super::PRELUDE_CRYPTO, f));
    let needs_redis = ctx.used.iter().any(|f| defines(super::PRELUDE_REDIS, f));
    let needs_http_client = ctx.used.iter().any(|f| defines(super::PRELUDE_HTTP, f));

    emit_shapes(&mut out, &ctx);
    emit_field_lists(&mut out, &ctx);
    emit_dispatch(&mut out, &routes);
    let db_boot = if needs_db {
        // `serve::serve` builds the pool before it binds; a binary that
        // waited for the first query would answer `/readyz` 200 with no
        // connection behind it.
        "    jwc_db_init().await;\n"
    } else {
        ""
    };
    out.push_str(&format!(
        "\n#[tokio::main(flavor = \"multi_thread\")]\nasync fn main() {{\n\
         \x20   jwc_install_panic_hook();\n\
         \x20   jwc_load_dotenv();\n\
         \x20   // `main` runs, and `serve(port)` inside it records where to\n\
         \x20   // listen — the same order the interpreter uses, so a program\n\
         \x20   // that hardcodes its port gets that port on both backends.\n\
         \x20   let _ = jwc_user_main().await;\n\
         {db_boot}\
         \x20   jwc_serve_impl(JWC_SERVE_PORT.load(::std::sync::atomic::Ordering::SeqCst)).await;\n}}\n"
    ));

    // The three operational endpoints exist in every binary; the halves
    // that report on Postgres and Redis live in preludes that do not. These
    // shims are the seam — forwarding when the prelude is present, honest
    // about its absence when it is not.
    out.push_str("\n// ── operational shims ──\n");
    let needs_regex =
        (uses_regex && ctx.uses_validation) || ctx.used.contains("jwc_b_v1_string_matches");
    out.push_str(if needs_regex {
        "fn jwc_regex_is_match(pattern: &str, s: &str) -> bool {\n\
         \x20   // A rule whose regex does not compile passes rather than\n\
         \x20   // failing every request — `validate.rs` makes the same\n\
         \x20   // choice, and the checker is where a bad pattern should\n\
         \x20   // have been caught.\n\
         \x20   regex::Regex::new(pattern).map(|r: regex::Regex| r.is_match(s)).unwrap_or(true)\n}\n"
    } else {
        "fn jwc_regex_is_match(_pattern: &str, _s: &str) -> bool { true }\n"
    });
    // `string.matches` is the caller's own argument, so a pattern that does
    // not compile is `false` — where a `pattern()` rule the checker accepted
    // passes rather than failing every request.
    out.push_str(if needs_regex {
        "fn jwc_regex_is_match_strict(pattern: &str, s: &str) -> bool {\n\
         \x20   regex::Regex::new(pattern).map(|r: regex::Regex| r.is_match(s)).unwrap_or(false)\n}\n"
    } else {
        "fn jwc_regex_is_match_strict(_pattern: &str, _s: &str) -> bool { false }\n"
    });
    out.push_str(if needs_redis {
        "fn jwc_redis_metrics_hook() -> String { jwc_redis_metrics() }\n"
    } else {
        "fn jwc_redis_metrics_hook() -> String { String::new() }\n"
    });
    out.push_str(&format!(
        "const JWC_ROUTE_COUNT: usize = {};\n",
        routes.len()
    ));
    out.push_str(if needs_db {
        "fn jwc_op_metrics() -> String { jwc_metrics_body() }\n"
    } else {
        // No DB prelude, so no pool gauges — but `jwc_routes` is declared
        // by the program, not by a dependency, and the interpreter reports
        // it either way.
        "fn jwc_op_metrics() -> String {\n\
         \x20   format!(\n\
         \x20       \"{{}}# HELP jwc_routes Declared routes.\\n# TYPE jwc_routes gauge\\njwc_routes {{}}\\n\",\n\
         \x20       jwc_redis_metrics_hook(),\n\
         \x20       JWC_ROUTE_COUNT\n\
         \x20   )\n}\n"
    });
    out.push_str("async fn jwc_op_readiness() -> Vec<&'static str> {\n");
    out.push_str("    let mut failed: Vec<&'static str> = Vec::new();\n");
    if needs_db {
        out.push_str(
            "    if DB_POOL.get().is_none() {\n\
             \x20       failed.push(\"db_uninitialised\");\n\
             \x20   } else if jwc_db_try_query(\"SELECT 1\", vec![]).await.is_err() {\n\
             \x20       failed.push(\"db_unreachable\");\n\
             \x20   }\n",
        );
    }
    if needs_redis {
        // Only when Redis is configured — a deployment that never set
        // `JWC_REDIS_URL` must not start failing its probe because the
        // runtime grew a Redis driver.
        out.push_str(
            "    if jwc_redis_pool().is_some() && !jwc_truthy(&jwc_b_redis_ping().await) {\n\
             \x20       failed.push(\"redis_unreachable\");\n\
             \x20   }\n",
        );
    }
    out.push_str("    failed\n}\n");

    let mut source = String::new();
    source.push_str(super::PRELUDE_BASE);
    source.push_str(super::PRELUDE_V1);
    if needs_db {
        source.push_str(super::PRELUDE_DB);
    }
    if needs_crypto {
        source.push_str(super::PRELUDE_CRYPTO);
    }
    if needs_redis {
        source.push_str(super::PRELUDE_REDIS);
    }
    if needs_http_client || needs_crypto {
        // Crypto pulls the HTTP prelude in for the JWKS fetch even when the
        // program never calls `http_get`; `render_cargo_toml` makes the same
        // deduction, and the two have to agree.
        source.push_str(super::PRELUDE_HTTP);
    }
    source.push_str(&out);

    Ok(Generated {
        source,
        needs_db,
        needs_http_client,
        needs_crypto,
        needs_redis,
        needs_regex,
    })
}

/// The classes, as the prelude's validator reads them.
///
/// Emitted from the same `ClassSym`s the checker built, so a rule the
/// checker accepted is a rule the binary enforces — the pattern rule that
/// compiled to an is-it-a-string check in an earlier backend, and let
/// `javascript:` past `pattern(r"^https?://")`, is exactly what a second
/// hand-written description of a class costs.
fn emit_classes(out: &mut String, symbols: &crate::symbols::Symbols) -> bool {
    use crate::types::{Scalar, Ty};

    fn base(t: &Ty) -> &Ty {
        match t {
            Ty::Optional(inner) | Ty::Array(inner) => base(inner),
            other => other,
        }
    }
    fn literal_i64(e: &Expr) -> Option<i64> {
        match &*e.kind {
            ExprKind::Int(n) => n.parse().ok(),
            _ => None,
        }
    }
    fn literal_f64(e: &Expr) -> Option<f64> {
        match &*e.kind {
            ExprKind::Int(n) | ExprKind::Decimal(n) => n.parse().ok(),
            _ => None,
        }
    }
    fn literal_str(e: &Expr) -> Option<&str> {
        match &*e.kind {
            ExprKind::Str(s) | ExprKind::RawStr(s) => Some(s),
            _ => None,
        }
    }

    let mut uses_regex = false;
    out.push_str("\nstatic JWC_CLASSES: &[(&str, &[JwcClassField])] = &[\n");
    for (name, class) in &symbols.classes {
        out.push_str(&format!("    ({}, &[\n", rust_str_literal(name)));
        for f in &class.fields {
            let b = base(&f.ty);
            let (ty, cls) = match b {
                Ty::Scalar(Scalar::Boolean) => ("JwcTy::Boolean", String::new()),
                Ty::Scalar(Scalar::Bigint) => ("JwcTy::Bigint", String::new()),
                Ty::Scalar(Scalar::Int | Scalar::Smallint) => ("JwcTy::Int", String::new()),
                Ty::Scalar(Scalar::Numeric) => ("JwcTy::Numeric", String::new()),
                Ty::Scalar(Scalar::Jsonb) => ("JwcTy::Jsonb", String::new()),
                Ty::Class(n) => ("JwcTy::Class", n.clone()),
                _ => ("JwcTy::Text", String::new()),
            };
            out.push_str("        JwcClassField {\n");
            out.push_str(&format!(
                "            name: {},\n",
                rust_str_literal(&f.name)
            ));
            out.push_str(&format!("            ty: {ty},\n"));
            out.push_str(&format!("            class: {},\n", rust_str_literal(&cls)));
            out.push_str(&format!(
                "            is_array: {},\n",
                matches!(&f.ty, Ty::Array(_))
            ));
            out.push_str("            rules: &[\n");
            for (rule, args) in &f.rules {
                if rule == "pattern" {
                    uses_regex = true;
                }
                let limit = args.first().and_then(literal_i64);
                let bound = args.first().and_then(literal_f64);
                let pattern = args.first().and_then(literal_str).unwrap_or("");
                out.push_str(&format!(
                    "                JwcRule {{ name: {}, limit: {}, bound: {}, pattern: {} }},\n",
                    rust_str_literal(rule),
                    match limit {
                        Some(l) => format!("Some({l})"),
                        None => "None".to_string(),
                    },
                    match bound {
                        // `{:?}` so an integral bound still prints as a
                        // float literal — `Some(0)` would not typecheck.
                        Some(b) => format!("Some({b:?})"),
                        None => "None".to_string(),
                    },
                    rust_str_literal(if rule == "pattern" { pattern } else { "" }),
                ));
            }
            out.push_str("            ],\n");
            out.push_str("        },\n");
        }
        out.push_str("    ]),\n");
    }
    out.push_str("];\n");
    uses_regex
}

/// `server { cursor_secret }`, as an expression the binary evaluates at
/// boot — not as the value `jwc build` happened to read.
///
/// It is almost always `env("CURSOR_SECRET")`, and baking in whatever that
/// was on the build machine would sign every deployment's cursors with the
/// builder's secret. `serve.rs::config_string` reads the same three forms.
fn emit_cursor_secret(out: &mut String, ws: &Workspace) {
    fn render(e: &Expr) -> Option<String> {
        match &*e.kind {
            ExprKind::Str(s) => Some(format!("String::from({})", rust_str_literal(s))),
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Name(n) = &*callee.kind else {
                    return None;
                };
                if n.name != "env" {
                    return None;
                }
                let ExprKind::Str(name) = &*args.first()?.kind else {
                    return None;
                };
                Some(format!(
                    "std::env::var({}).unwrap_or_default()",
                    rust_str_literal(name)
                ))
            }
            ExprKind::Coalesce { lhs, rhs } => {
                let l = render(lhs)?;
                let r = render(rhs)?;
                Some(format!(
                    "{{ let __s = {l}; if __s.is_empty() {{ {r} }} else {{ __s }} }}"
                ))
            }
            _ => None,
        }
    }

    let mut expr = "String::new()".to_string();
    for file in &ws.files {
        for decl in &file.program.decls {
            let Decl::Server(d) = decl else { continue };
            for e in &d.entries {
                let crate::ast::ServerEntry::Set(a) = e else {
                    continue;
                };
                if a.key.name == "cursor_secret" {
                    if let Some(r) = render(&a.value) {
                        expr = r;
                    }
                }
            }
        }
    }
    out.push_str(&format!(
        "\n/// `server {{ cursor_secret }}` — read at boot, the way the\n\
         /// interpreter reads it.\n\
         fn jwc_cursor_secret_source() -> &'static str {{\n\
         \x20   static S: ::std::sync::OnceLock<String> = ::std::sync::OnceLock::new();\n\
         \x20   S.get_or_init(|| {expr})\n}}\n"
    ));
}

/// `db::install_messages` builds this map at boot from the schema model;
/// the binary gets it as a static instead.
fn emit_constraint_messages(out: &mut String, model: &SchemaModel) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for t in &model.tables {
        for u in &t.uniques {
            if let Some(m) = &u.message {
                pairs.push((u.name.clone(), m.clone()));
            }
        }
        for c in &t.checks {
            if let Some(m) = &c.message {
                pairs.push((c.name.clone(), m.clone()));
            }
        }
    }
    pairs.sort();
    pairs.dedup();
    out.push_str("\n/// constraint name -> the sentence its declaration carries.\n");
    out.push_str("const JWC_CONSTRAINT_MESSAGES_TABLE: &[(&str, &str)] = &[\n");
    for (name, msg) in pairs {
        out.push_str(&format!(
            "    ({}, {}),\n",
            rust_str_literal(&name),
            rust_str_literal(&msg)
        ));
    }
    out.push_str("];\n");
}

fn emit_function(
    out: &mut String,
    name: &str,
    f: &crate::ast::FunctionDecl,
    ctx: &mut Ctx,
) -> Result<()> {
    ctx.mode = Mode::Value;
    out.push_str(&format!("\nasync fn {}(", user_fn(name)));
    ctx.classes_in_scope.clear();
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{}: V", local(&p.name.name)));
        if let crate::ast::TypeKind::Named(n) = &p.ty.kind {
            let named = n.text();
            if ctx.symbols.classes.contains_key(&named) {
                ctx.classes_in_scope.insert(p.name.name.clone(), named);
            }
        }
    }
    out.push_str(") -> JwcResult {\n");
    emit_block(out, &f.body, 1, ctx)?;
    out.push_str("    Ok(V::Null)\n}\n");
    Ok(())
}

/// The chain, the handler, the error handler and the `after` blocks — the
/// order `serve.rs::dispatch` runs them in, unrolled for one route.
fn emit_route_dispatch(
    out: &mut String,
    name: &str,
    route: &crate::wiring::ResolvedRoute,
    middleware: &BTreeMap<String, &crate::ast::MiddlewareDecl>,
) {
    out.push_str(&format!("\nasync fn {name}_dispatch() -> V {{\n"));
    out.push_str("    let mut outcome: Option<V> = None;\n");
    out.push_str("    let mut raised: Option<JwcThrown> = None;\n");
    // Every middleware that *started* runs its `after` block, including the
    // one that short-circuited (middleware.md §4.3), so the count is what
    // the after loop below is keyed on.
    out.push_str("    let mut started = 0usize;\n");
    if !route.chain.is_empty() {
        out.push_str("    'chain: {\n");
        for m in &route.chain {
            if !middleware.contains_key(m) {
                continue;
            }
            out.push_str("        started += 1;\n");
            out.push_str(&format!("        match {}().await {{\n", mw_fn(m)));
            out.push_str("            Ok(Some(r)) => { outcome = Some(r); break 'chain; }\n");
            out.push_str("            Ok(None) => {}\n");
            out.push_str("            Err(t) => { raised = Some(t); break 'chain; }\n");
            out.push_str("        }\n");
        }
        out.push_str("    }\n");
    }
    out.push_str("    if outcome.is_none() && raised.is_none() {\n");
    out.push_str(&format!(
        "        match {name}_body().await {{\n\
         \x20           Ok(v) => outcome = Some(v),\n\
         \x20           Err(t) => raised = Some(t),\n\
         \x20       }}\n"
    ));
    out.push_str("    }\n");
    out.push_str(
        "    let mut response = match (outcome, raised) {\n\
         \x20       (Some(r), _) => r,\n\
         \x20       (None, Some(t)) => jwc_thrown_response(t),\n\
         \x20       (None, None) => jwc_b_internal_error(v_str(\"internal_error\")),\n\
         \x20   };\n",
    );

    // §5.1–§5.2 — reverse order, every outcome, and `response.status()` sees
    // the status actually being sent.
    let afters: Vec<&String> = route
        .chain
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, m)| middleware.get(*m).is_some_and(|d| d.after.is_some()))
        .map(|(_, m)| m)
        .collect();
    if !afters.is_empty() {
        out.push_str("    jwc_set_response_status(jwc_status_of(&response));\n");
        for m in afters {
            let idx = route.chain.iter().position(|x| x == m).unwrap_or(0);
            out.push_str(&format!("    if started > {idx} {{\n"));
            out.push_str(&format!("        let _ = {}_after().await;\n", mw_fn(m)));
            out.push_str(
                "        response = jwc_response_with_headers(response, jwc_drain_extra_headers());\n",
            );
            out.push_str("    }\n");
        }
    }
    out.push_str("    response\n}\n");
}

fn mw_fn(name: &str) -> String {
    format!("jwc_mw_{}", name.replace('.', "_"))
}

/// The pattern with each parameter's declared type kept, which is what the
/// router needs to parse a segment before middleware (routing.md §3.2).
fn typed_pattern(route: &crate::wiring::ResolvedRoute) -> String {
    let mut s = String::new();
    for seg in &route.segments {
        s.push('/');
        match seg {
            crate::wiring::Segment::Literal(l) => s.push_str(l),
            crate::wiring::Segment::Param { name, ty } => {
                s.push_str(&format!("{{{name}: {ty}}}"));
            }
        }
    }
    if s.is_empty() {
        s.push('/');
    }
    s
}

/// `/notes/{id: bigint}` -> `/notes/{id}`, matching `wiring::render`.
fn strip_types(path: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut skipping = false;
    for c in path.chars() {
        match c {
            '{' => {
                depth += 1;
                skipping = false;
                out.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                skipping = false;
                out.push(c);
            }
            ':' if depth > 0 => skipping = true,
            _ if skipping => {}
            _ => out.push(c),
        }
    }
    out
}

fn join_path(prefix: &str, suffix: &str) -> String {
    let p = prefix.trim_end_matches('/');
    let s = suffix.trim_start_matches('/');
    if s.is_empty() {
        if p.is_empty() {
            "/".into()
        } else {
            p.to_string()
        }
    } else if p.is_empty() {
        format!("/{s}")
    } else {
        format!("{p}/{s}")
    }
}

fn handler_name(method: &str, path: &str) -> String {
    let mut s = format!("jwc_route_{}_", method.to_lowercase());
    for c in path.chars() {
        s.push(if c.is_ascii_alphanumeric() { c } else { '_' });
    }
    s
}

fn user_fn(name: &str) -> String {
    format!("jwc_fn_{}", name.replace('.', "_"))
}

fn local(name: &str) -> String {
    format!("v_{name}")
}

fn emit_shapes(out: &mut String, ctx: &Ctx) {
    if ctx.shapes.is_empty() {
        return;
    }
    out.push_str("\n// ── interned object-literal shapes ──\n");
    let mut pairs: Vec<(&Vec<String>, &usize)> = ctx.shapes.iter().collect();
    pairs.sort_by_key(|(_, i)| **i);
    for (keys, idx) in pairs {
        out.push_str(&format!(
            "#[inline]\nfn jwc_shape_{idx}() -> &'static ::std::sync::Arc<Vec<JwcStr>> {{\n\
             \x20   static S: ::std::sync::OnceLock<::std::sync::Arc<Vec<JwcStr>>> = ::std::sync::OnceLock::new();\n\
             \x20   S.get_or_init(|| ::std::sync::Arc::new(vec![",
        ));
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "::std::borrow::Cow::Borrowed({})",
                rust_str_literal(k)
            ));
        }
        out.push_str("]))\n}\n");
    }
}

/// The projection order of each distinct query, as a `&'static [&str]`.
fn emit_field_lists(out: &mut String, ctx: &Ctx) {
    if ctx.field_lists.is_empty() {
        return;
    }
    out.push_str("\n// ── projection orders ──\n");
    let mut pairs: Vec<(&Vec<String>, &usize)> = ctx.field_lists.iter().collect();
    pairs.sort_by_key(|(_, i)| **i);
    for (fields, idx) in pairs {
        out.push_str(&format!("const JWC_FIELDS_{idx}: &[&str] = &["));
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&rust_str_literal(f));
        }
        out.push_str("];\n");
    }
}

fn emit_dispatch(out: &mut String, routes: &[(String, String, String)]) {
    out.push_str("\nasync fn jwc_serve_impl(port: u16) {\n");
    out.push_str("    let mut router = Router::new();\n");
    for (method, path, name) in routes {
        out.push_str(&format!(
            "    router.add({}, {}, {name});\n",
            rust_str_literal(method),
            rust_str_literal(path),
        ));
    }
    out.push_str("    HttpServer::new(port, router).run().await;\n}\n");
}

fn emit_block(out: &mut String, body: &Block, indent: usize, ctx: &mut Ctx) -> Result<()> {
    for stmt in body {
        emit_stmt(out, stmt, indent, ctx)?;
    }
    Ok(())
}

/// How a `return` leaves the body being emitted.
///
/// Inside a `transaction { }` the body is an `async` block, so `return`
/// would exit the block, not the function — the value is handed to the
/// wrapper instead, which re-returns it one layer out. Repeat per nesting
/// level and it reaches the function it was written in.
fn emit_return(out: &mut String, pad: &str, value: &str, ctx: &Ctx) {
    if ctx.tx_depth > 0 {
        out.push_str(&format!("{pad}return Ok(Some({value}));\n"));
    } else {
        match ctx.mode {
            Mode::Value => out.push_str(&format!("{pad}return Ok({value});\n")),
            Mode::Middleware => out.push_str(&format!("{pad}return Ok(Some({value}));\n")),
        }
    }
}

/// The value a bare `return;` carries out of the current body.
fn void_return_value(ctx: &Ctx) -> &'static str {
    match ctx.mode {
        Mode::Value => "V::Null",
        // `Flow::ReturnVoid` in a middleware is a 204 (`serve.rs`).
        Mode::Middleware => "jwc_b_no_content(V::Null)",
    }
}

fn emit_stmt(out: &mut String, stmt: &Stmt, indent: usize, ctx: &mut Ctx) -> Result<()> {
    let pad = "    ".repeat(indent);
    match stmt {
        Stmt::Let { name, value, .. } => {
            // The other place the AST names a local's class outright.
            if let ExprKind::Cast { ty, .. } = &*value.kind {
                ctx.classes_in_scope
                    .insert(name.name.clone(), ty.name.clone());
            }
            let v = emit_expr(value, ctx)?;
            out.push_str(&format!("{pad}let mut {} = {v};\n", local(&name.name)));
        }
        Stmt::Assign { target, value, .. } => {
            let v = emit_expr(value, ctx)?;
            match target {
                crate::ast::AssignTarget::Local(i) => {
                    out.push_str(&format!("{pad}{} = {v};\n", local(&i.name)));
                }
                crate::ast::AssignTarget::Context(i) => {
                    out.push_str(&format!(
                        "{pad}jwc_b_set_context(v_str({}), {v});\n",
                        rust_str_literal(&i.name)
                    ));
                }
            }
        }
        Stmt::If {
            cond,
            then,
            otherwise,
            ..
        } => {
            let c = emit_expr(cond, ctx)?;
            out.push_str(&format!("{pad}if jwc_truthy(&{c}) {{\n"));
            emit_block(out, then, indent + 1, ctx)?;
            if let Some(alt) = otherwise {
                out.push_str(&format!("{pad}}} else {{\n"));
                emit_block(out, alt, indent + 1, ctx)?;
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::For {
            binder,
            iterable,
            body,
            ..
        } => {
            let it = emit_expr(iterable, ctx)?;
            out.push_str(&format!(
                "{pad}for {} in jwc_to_array({it}).iter().cloned() {{\n",
                local(&binder.name)
            ));
            emit_block(out, body, indent + 1, ctx)?;
            out.push_str(&format!("{pad}}}\n"));
        }
        // middleware.md §4.2 — `return <Response>` short-circuits the chain;
        // a bare `return` short-circuits it with a 204, which is what
        // `Flow::ReturnVoid` becomes in `serve.rs`.
        Stmt::Return { value, .. } => {
            let e = match value {
                Some(v) => emit_expr(v, ctx)?,
                None => void_return_value(ctx).to_string(),
            };
            emit_return(out, &pad, &e, ctx);
        }
        Stmt::Break { .. } => out.push_str(&format!("{pad}break;\n")),
        Stmt::Continue { .. } => out.push_str(&format!("{pad}continue;\n")),
        Stmt::Throw { error, args, .. } => {
            let e = emit_throw(&error.name, args.first(), ctx)?;
            out.push_str(&format!("{pad}return Err({e});\n"));
        }
        Stmt::Expr { expr, .. } => {
            let e = emit_expr(expr, ctx)?;
            out.push_str(&format!("{pad}let _ = {e};\n"));
        }
        // `assert` is `jwc test`, not a server binary.
        // writes.md §5 — one connection, `BEGIN` … `COMMIT`, and a rollback
        // on any error leaving the block. The pin is what makes every
        // statement inside land on the same connection; without it the
        // block would commit nothing and roll back nothing.
        Stmt::Transaction { body, .. } => {
            let d = ctx.tx_depth;
            out.push_str(&format!("{pad}{{\n"));
            out.push_str(&format!("{pad}    let __tx{d} = jwc_tx_begin().await?;\n"));
            out.push_str(&format!(
                "{pad}    let __r{d}: Result<Option<V>, JwcThrown> = JWC_TX_CONN\n\
                 {pad}        .scope(__tx{d}.clone(), async {{\n"
            ));
            ctx.tx_depth += 1;
            emit_block(out, body, indent + 3, ctx)?;
            ctx.tx_depth -= 1;
            out.push_str(&format!(
                "{pad}            Ok(None)\n{pad}        }})\n{pad}        .await;\n"
            ));
            // COMMIT when the block left normally, ROLLBACK otherwise —
            // including when it left by `return`, which commits, because
            // `Flow::Return` is `Ok` in the interpreter too.
            out.push_str(&format!(
                "{pad}    jwc_tx_end(__tx{d}, __r{d}.is_ok()).await;\n"
            ));
            out.push_str(&format!("{pad}    if let Some(__v) = __r{d}? {{\n"));
            emit_return(out, &format!("{pad}        "), "__v", ctx);
            out.push_str(&format!("{pad}    }}\n"));
            out.push_str(&format!("{pad}}}\n"));
        }
        Stmt::Assert { .. } => bail!("`assert` belongs to `jwc test`, not a native binary"),
    }
    Ok(())
}

fn emit_expr(e: &Expr, ctx: &mut Ctx) -> Result<String> {
    Ok(match &*e.kind {
        ExprKind::Int(n) => format!("V::Int({n})"),
        ExprKind::Decimal(d) => format!("v_str({})", rust_str_literal(d)),
        ExprKind::Str(s) | ExprKind::RawStr(s) => format!("v_str({})", rust_str_literal(s)),
        ExprKind::Bool(b) => format!("V::Bool({b})"),
        ExprKind::Null => "V::Null".into(),

        ExprKind::Local(i) => format!("{}.clone()", local(&i.name)),
        ExprKind::PathParam(i) => {
            format!("jwc_b_path_param(v_str({}))", rust_str_literal(&i.name))
        }
        ExprKind::Name(i) => bail!(
            "native build cannot resolve the bare name `{}` outside a query",
            i.name
        ),

        ExprKind::Field { base, field } => {
            // `context.<key>` is a read, not a field access on a value.
            if let ExprKind::Name(n) = &*base.kind {
                if n.name == "context" {
                    return Ok(format!(
                        "jwc_b_context(v_str({}))",
                        rust_str_literal(&field.name)
                    ));
                }
            }
            let b = emit_expr(base, ctx)?;
            format!("jwc_get_field(&{b}, {})", rust_str_literal(&field.name))
        }

        ExprKind::Index { base, index } => {
            let b = emit_expr(base, ctx)?;
            let i = emit_expr(index, ctx)?;
            format!("jwc_get_field(&{b}, jwc_str_view(&{i}).unwrap_or(\"\"))")
        }

        ExprKind::Unary { op, rhs } => {
            let r = emit_expr(rhs, ctx)?;
            match op {
                UnaryOp::Not => format!("V::Bool(!jwc_truthy(&{r}))"),
                UnaryOp::Neg => format!("jwc_neg({r})"),
            }
        }

        ExprKind::Binary { op, lhs, rhs } => {
            let l = emit_expr(lhs, ctx)?;
            let r = emit_expr(rhs, ctx)?;
            match op {
                // Arithmetic and concatenation: `V` in, `V` out.
                BinOp::Add => format!("jwc_add({l}, {r})"),
                BinOp::Sub => format!("jwc_sub({l}, {r})"),
                BinOp::Mul => format!("jwc_mul({l}, {r})"),
                BinOp::Div => format!("jwc_div({l}, {r})"),
                BinOp::Rem => format!("jwc_mod({l}, {r})"),

                // Comparison: the prelude's helpers take references and
                // answer a Rust `bool`, so the result is lifted back into a
                // `V` here rather than pretending they compose.
                BinOp::Eq | BinOp::EqOpt => format!("V::Bool(jwc_eq(&{l}, &{r}))"),
                BinOp::Ne => format!("V::Bool(!jwc_eq(&{l}, &{r}))"),
                BinOp::Lt => format!("V::Bool(jwc_lt(&{l}, &{r}))"),
                BinOp::Le => format!("V::Bool(jwc_lte(&{l}, &{r}))"),
                BinOp::Gt => format!("V::Bool(jwc_gt(&{l}, &{r}))"),
                BinOp::Ge => format!("V::Bool(jwc_gte(&{l}, &{r}))"),

                // `and` / `or` must short-circuit, so they are emitted
                // inline: a call would evaluate both sides.
                BinOp::And => format!(
                    "{{ let __l = {l}; if !jwc_truthy(&__l) {{ V::Bool(false) }} else {{ V::Bool(jwc_truthy(&{r})) }} }}"
                ),
                BinOp::Or => format!(
                    "{{ let __l = {l}; if jwc_truthy(&__l) {{ V::Bool(true) }} else {{ V::Bool(jwc_truthy(&{r})) }} }}"
                ),

                BinOp::Like | BinOp::ILike => {
                    bail!("`like` is a query operator; it has no meaning outside one")
                }
            }
        }

        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            let c = emit_expr(cond, ctx)?;
            let t = emit_expr(then, ctx)?;
            let o = emit_expr(otherwise, ctx)?;
            format!("if jwc_truthy(&{c}) {{ {t} }} else {{ {o} }}")
        }

        ExprKind::Coalesce { lhs, rhs } => {
            let l = emit_expr(lhs, ctx)?;
            let r = emit_expr(rhs, ctx)?;
            format!("{{ let __l = {l}; if matches!(__l, V::Null) {{ {r} }} else {{ __l }} }}")
        }

        ExprKind::Object(entries) => {
            let mut keys = Vec::new();
            let mut vals = Vec::new();
            for entry in entries {
                match entry {
                    ObjEntry::Field { key, value, .. } => {
                        keys.push(key.name.clone());
                        vals.push(emit_expr(value, ctx)?);
                    }
                    ObjEntry::Spread { .. } => {
                        bail!("native build does not cover `...` spread in an object yet")
                    }
                }
            }
            let id = ctx.shape_id(keys);
            format!(
                "v_record(jwc_shape_{id}().clone(), vec![{}])",
                vals.join(", ")
            )
        }

        ExprKind::Array(items) => {
            let mut parts = Vec::new();
            for i in items {
                parts.push(emit_expr(i, ctx)?);
            }
            format!("v_arr(vec![{}])", parts.join(", "))
        }

        ExprKind::Call { callee, args, .. } => {
            let name = callee_name(callee)?;
            let mut parts = Vec::new();
            for a in args {
                parts.push(emit_expr(a, ctx)?);
            }
            if name == "serve" {
                let port = parts
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "V::Int(8080)".into());
                return Ok(format!(
                    "{{ JWC_SERVE_PORT.store(jwc_to_int(&{port}).unwrap_or(8080) as u16, ::std::sync::atomic::Ordering::SeqCst); V::Null }}"
                ));
            }
            // The prelude's arity, not 1.0's: `noContent()` takes no
            // argument in the language and one in the runtime, and a
            // response builder ignores it.
            if matches!(name.as_str(), "noContent" | "internalError") && parts.is_empty() {
                return Ok(format!("{}(V::Null)", prelude_fn(&name).unwrap_or("")));
            }
            if name == "request.query" {
                // 1.0 answers `text?`; the prelude takes the absent-value as
                // a second argument, and `null` is what `text?` means.
                return Ok(format!(
                    "jwc_b_query_param({}, V::Null)",
                    parts.first().cloned().unwrap_or_else(|| "V::Null".into())
                ));
            }
            // `int(s)` and `bigint(s)` raise `BadRequest` on a value that
            // is not a number (types.md §7.2), so they return a `Result`
            // and the call site propagates.
            if matches!(name.as_str(), "int" | "bigint") {
                ctx.used.insert(format!("jwc_b_v1_{name}"));
                return Ok(format!(
                    "jwc_b_v1_{name}({})?",
                    parts.first().cloned().unwrap_or_else(|| "V::Null".into())
                ));
            }
            // `enum(E, x)` names a type first; the type is not a value.
            if name == "enum" {
                ctx.used.insert("jwc_b_v1_enum".to_string());
                return Ok(format!(
                    "jwc_b_v1_enum({})",
                    parts.get(1).cloned().unwrap_or_else(|| "V::Null".into())
                ));
            }
            if let Some(f) = prelude_fn(&name) {
                // The prelude mixes sync and async builtins, so the suffix
                // is looked up rather than guessed: an `.await` on a plain
                // value does not compile, and a missing one yields a future
                // where a `V` was wanted.
                ctx.used.insert(f.to_string());
                let call = format!("{f}({})", parts.join(", "));
                if ASYNC_BUILTINS.contains(&f) {
                    format!("{call}.await")
                } else {
                    call
                }
            } else if PRELUDE_GAPS.contains(&name.as_str()) {
                bail!(
                    "`{name}` is a 1.0 built-in the restored prelude does not \
                     implement yet — it predates the 1.0 vocabulary. \
                     `jwc serve` has it."
                )
            } else {
                // A user function can throw, and a throw is a `Result`, so
                // the call site propagates. `?` returns from whichever body
                // is being emitted — which is the enclosing `async` block
                // inside a `transaction`, and the transaction wrapper
                // re-raises it. Same path the interpreter's `Abort` takes.
                format!("{}({}).await?", user_fn(&name), parts.join(", "))
            }
        }

        // routing.md §6.2 — the header suffix attaches to the response it
        // decorates, so it survives being nested in `created(...)`.
        ExprKind::WithHeaders { value, headers } => {
            let v = emit_expr(value, ctx)?;
            let mut parts = Vec::new();
            for h in headers {
                let ObjEntry::Field { key, value, .. } = h else {
                    bail!("a `with {{ … }}` entry is a header name and a value")
                };
                let hv = emit_expr(value, ctx)?;
                parts.push(format!(
                    "({}.to_string(), body_string({hv}))",
                    rust_str_literal(&key.name)
                ));
            }
            format!("jwc_with_headers({v}, vec![{}])", parts.join(", "))
        }
        ExprKind::Cookie { .. } => bail!("native build does not cover `cookie(...)` yet"),
        // routing.md §5.2 — the cast is what validates.
        ExprKind::Cast { value: _, ty } => {
            ctx.uses_validation = true;
            format!("jwc_validate_body({})?", rust_str_literal(&ty.name))
        }
        ExprKind::Select(sel) => emit_select(sel, ctx)?,
        ExprKind::In { .. } => bail!("native build does not cover `in (...)` yet"),
        ExprKind::Exists { .. } => bail!("`exists` is a query construct"),

        // errors.md §5 — `<expr> or throw E(msg)`: absent becomes the error.
        ExprKind::OrThrow { value, error, args } => {
            let v = emit_expr(value, ctx)?;
            let t = emit_throw(&error.name, args.first(), ctx)?;
            format!("{{ let __v = {v}; if matches!(__v, V::Null) {{ return Err({t}); }} __v }}")
        }

        // errors.md §7 — `<expr> catch E (err) { … }`. The block must
        // diverge, which the checker enforces (E0812), so the arm needs no
        // value of its own.
        ExprKind::CatchPostfix {
            value,
            error,
            binder,
            body,
        } => {
            let v = emit_expr(value, ctx)?;
            let arm = render_block(body, 4, ctx)?;
            format!(
                "{{\n    let __c: JwcResult = async {{ Ok::<V, JwcThrown>({v}) }}.await;\n\
                     match __c {{\n\
                         Ok(__v) => __v,\n\
                         Err(__t) if __t.error == {} => {{\n\
                             let {} = __t.payload();\n{arm}\
                         }}\n\
                         Err(__t) => return Err(__t),\n\
                     }}\n}}",
                rust_str_literal(&error.name),
                local(&binder.name),
            )
        }

        ExprKind::Insert(i) => emit_insert(i, ctx)?,
        ExprKind::Update(u) => emit_update(u, ctx)?,
        ExprKind::Delete(d) => emit_delete(d, ctx)?,
    })
}

/// Build the `JwcThrown` a `throw` or an `or throw` raises.
fn emit_throw(error: &str, arg: Option<&Expr>, ctx: &mut Ctx) -> Result<String> {
    let status = ctx.error_status(error)?;
    let msg = match arg {
        Some(a) => emit_expr(a, ctx)?,
        None => "V::Null".into(),
    };
    Ok(format!(
        "JwcThrown::new({}, {status}, {msg})",
        rust_str_literal(error)
    ))
}

/// A block rendered as text rather than pushed onto the output — needed
/// where a block sits inside an expression.
fn render_block(body: &Block, indent: usize, ctx: &mut Ctx) -> Result<String> {
    let mut out = String::new();
    emit_block(&mut out, body, indent, ctx)?;
    Ok(out)
}

/// Lower a `select` to the statement the interpreter would send.
///
/// `query_sql` does the work: it produces the SQL text, the parameter list
/// with each one's cast, and the shape. Codegen embeds the string and emits
/// the binds, so the two backends cannot disagree about what a query means
/// — there is one query compiler and this calls it.
fn emit_select(sel: &crate::ast::SelectExpr, ctx: &mut Ctx) -> Result<String> {
    let plan = crate::query::plan(sel, ctx.symbols);
    let mut c = crate::query_sql::Compiler::new(ctx.model).max_page_size(ctx.max_page_size);
    let Some(compiled) = c.compile(sel, &plan) else {
        // The compiler names the piece it could not lower; a generic
        // "not expressible" here would throw that away.
        bail!("{}", c.gap());
    };
    emit_built(
        &crate::sql::Built {
            sql: compiled.sql,
            params: compiled.params,
            shape: compiled.shape,
            record: compiled.record,
            fields: compiled.fields,
            page: compiled.page,
        },
        &[],
        ctx,
    )
}

fn emit_insert(i: &crate::ast::InsertExpr, ctx: &mut Ctx) -> Result<String> {
    // `write_fields`: the values are evaluated *here*, once, and bound. An
    // `insert` whose value calls `uuid()` must not have it called twice.
    let mut names: Vec<(String, Expr)> = Vec::new();
    let mut preset: Vec<String> = Vec::new();
    for e in &i.values {
        match e {
            ObjEntry::Field {
                key, value, span, ..
            } => {
                names.push((key.name.clone(), placeholder(*span)));
                preset.push(emit_expr(value, ctx)?);
            }
            // A spread's column list depends on which fields the source
            // record actually carries, which is a runtime fact. The
            // statement's column list is compile-time here, so the two
            // cannot be reconciled without building the SQL at run time.
            ObjEntry::Spread { .. } => bail!(
                "native build does not lower `...` spread in an `insert` — the \
                 column list depends on the value's shape at run time. \
                 `jwc serve` runs it."
            ),
        }
    }
    let mut b = crate::sql::Builder::new(ctx.model);
    let Some(built) = b.insert(i, &names) else {
        bail!("this insert is not expressible yet");
    };
    // Every parameter of an INSERT is a value the writer already computed:
    // the builder binds the placeholder `Expr`, and `exec::run_insert`
    // hands the values straight to `run_sql_with`, bypassing `bind_params`.
    // Re-deriving them from the placeholder would bind `null` for each.
    emit_built_with(&built, &preset, true, ctx)
}

/// The most optional (`=?`) assignments one `update` may carry.
///
/// Which columns the statement sets is a run-time fact, so each combination
/// is a different statement and all of them are compiled here. Eight is 256
/// statements — already far past anything a PATCH endpoint writes, and the
/// point at which "compile them all" stops being the right answer.
const MAX_OPTIONAL_SETS: usize = 8;

fn emit_update(u: &crate::ast::UpdateExpr, ctx: &mut Ctx) -> Result<String> {
    // Three kinds of assignment, in source order because the parameter
    // order follows it: an expression the database computes, a value bound
    // unconditionally, and a value bound only when it is present.
    enum Assign {
        Sql(Expr),
        Bound(String, crate::token::Span),
        /// `=?` — writes.md §3.3. Set unless the value is null.
        Optional(String, crate::token::Span),
        /// A field of a `...` spread — types.md §9.2. Set when the source
        /// **carries** the key, which is not the same as the value being
        /// non-null: an explicit null sets the column to null.
        Spread(String, String, crate::token::Span),
    }

    let mut plan: Vec<(String, Assign)> = Vec::new();
    for it in &u.sets {
        match it {
            SetItem::Set {
                column,
                value,
                optional,
                span,
            } => {
                // writes.md §2.3 — an expression that reads the row's own
                // columns belongs in the database: `set value = value + 1`
                // is an increment, and computing it here would need a read
                // first, which is the race the rule is about.
                if reads_a_column(&u.table, value, ctx) {
                    plan.push((column.name.clone(), Assign::Sql(value.clone())));
                    continue;
                }
                let v = emit_expr(value, ctx)?;
                plan.push((
                    column.name.clone(),
                    if *optional {
                        Assign::Optional(v, *span)
                    } else {
                        Assign::Bound(v, *span)
                    },
                ));
            }
            // types.md §9.2 — a spread sets the fields the value actually
            // carries. *Which* fields it could carry is the source's
            // declared type, and that is in the AST: a function parameter
            // declares one, and `let x = request.body() as C` names one.
            SetItem::Spread {
                source,
                except,
                span,
            } => {
                let Some(class) = ctx.class_of_local(&source.name) else {
                    bail!(
                        "native build cannot see the shape of `${}` — a `...` \
                         spread's columns come from the value's declared type, \
                         and this one is neither a typed parameter nor a \
                         `request.body() as <Class>`. `jwc serve` reads the \
                         shape at run time.",
                        source.name
                    );
                };
                let Some(sym) = ctx.symbols.classes.get(&class).cloned() else {
                    bail!("`{class}` is not a declared class");
                };
                for f in &sym.fields {
                    if except.iter().any(|x| x.name == f.name) {
                        continue;
                    }
                    // A field the table has no column for is dropped by the
                    // builder anyway; dropping it here keeps it out of the
                    // presence mask, which is what bounds the combinations.
                    plan.push((
                        f.name.clone(),
                        Assign::Spread(
                            format!(
                                "jwc_get_field(&{}, {})",
                                local(&source.name),
                                rust_str_literal(&f.name)
                            ),
                            format!(
                                "jwc_has_field(&{}, {})",
                                local(&source.name),
                                rust_str_literal(&f.name)
                            ),
                            *span,
                        ),
                    ));
                }
            }
        }
    }

    let optional_count = plan
        .iter()
        .filter(|(_, a)| matches!(a, Assign::Optional(..) | Assign::Spread(..)))
        .count();

    // The ordinary case: one statement, one set of binds.
    if optional_count == 0 {
        let sets: Vec<(String, crate::sql::SetValue)> = plan
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    match a {
                        Assign::Sql(e) => crate::sql::SetValue::Sql(e.clone()),
                        Assign::Bound(_, span)
                        | Assign::Optional(_, span)
                        | Assign::Spread(_, _, span) => {
                            crate::sql::SetValue::Bound(placeholder(*span))
                        }
                    },
                )
            })
            .collect();
        let preset: Vec<String> = plan
            .iter()
            .filter_map(|(_, a)| match a {
                Assign::Bound(v, _) => Some(v.clone()),
                _ => None,
            })
            .collect();
        let mut b = crate::sql::Builder::new(ctx.model);
        let Some(built) = b.update(u, &sets) else {
            bail!("this update is not expressible yet");
        };
        // An UPDATE mixes the two: `Bind::Preset` for each `set`,
        // `Bind::Expr` for the `where`, which is why `exec::run_update` goes
        // through `bind_params` where `run_insert` does not.
        return emit_built_with(&built, &preset, false, ctx);
    }

    if optional_count > MAX_OPTIONAL_SETS {
        bail!(
            "native build does not lower an `update` with {optional_count} \
             optional assignments — each combination is a different statement \
             and this compiles all of them, which stops being reasonable \
             past {MAX_OPTIONAL_SETS}. `jwc serve` builds the statement per \
             request."
        );
    }

    // Evaluate every value once, ahead of the branch: an `=?` whose value
    // calls `date.now()` must not be called once to test for presence and
    // again to bind.
    let mut out = String::from("{\n");
    let mut opt_slots: Vec<usize> = Vec::new();
    for (i, (_, a)) in plan.iter().enumerate() {
        match a {
            Assign::Bound(v, _) | Assign::Optional(v, _) => {
                out.push_str(&format!("    let __set{i} = {v};\n"));
                if matches!(a, Assign::Optional(..)) {
                    opt_slots.push(i);
                }
            }
            Assign::Spread(v, present, _) => {
                out.push_str(&format!("    let __set{i} = {v};\n"));
                out.push_str(&format!("    let __has{i} = {present};\n"));
                opt_slots.push(i);
            }
            Assign::Sql(_) => {}
        }
    }

    // A bit per `=?`, set when the value is present. types.md §6.5 keeps
    // absent and null distinguishable, and `=?` treats both as "skip" —
    // which is what the interpreter's `if *optional && v.is_null()` does.
    out.push_str("    let __mask: usize = 0");
    for (bit, slot) in opt_slots.iter().enumerate() {
        let test = match plan.get(*slot).map(|(_, a)| a) {
            Some(Assign::Spread(..)) => format!("__has{slot}"),
            _ => format!("!matches!(__set{slot}, V::Null)"),
        };
        out.push_str(&format!(
            "\n        | if {test} {{ 1 << {bit} }} else {{ 0 }}"
        ));
    }
    out.push_str(";\n");

    out.push_str("    match __mask {\n");
    let variants = 1usize << optional_count;
    for mask in 0..variants {
        let mut sets: Vec<(String, crate::sql::SetValue)> = Vec::new();
        let mut preset: Vec<String> = Vec::new();
        for (i, (name, a)) in plan.iter().enumerate() {
            match a {
                Assign::Sql(e) => {
                    sets.push((name.clone(), crate::sql::SetValue::Sql(e.clone())));
                }
                Assign::Bound(_, span) => {
                    sets.push((
                        name.clone(),
                        crate::sql::SetValue::Bound(placeholder(*span)),
                    ));
                    preset.push(format!("__set{i}.clone()"));
                }
                Assign::Optional(_, span) | Assign::Spread(_, _, span) => {
                    let bit = opt_slots.iter().position(|s| *s == i).unwrap_or(0);
                    if mask & (1 << bit) != 0 {
                        sets.push((
                            name.clone(),
                            crate::sql::SetValue::Bound(placeholder(*span)),
                        ));
                        preset.push(format!("__set{i}.clone()"));
                    }
                }
            }
        }
        let arm = if mask == variants - 1 {
            "_"
        } else {
            &mask.to_string()
        };
        if sets.is_empty() {
            // writes.md §3.3 — every assignment skipped. The interpreter
            // falls back to selecting the row as it stands rather than
            // emitting an empty SET, and so does this.
            let probe = crate::ast::SelectExpr {
                binder: crate::ast::Ident::new("x", u.span),
                source: u.table.clone(),
                joins: vec![],
                filter: u.filter.clone(),
                group_by: vec![],
                having: None,
                projection: u.projection.clone(),
                order_by: u.order_by.clone(),
                limit: None,
                page: None,
                first: u.first,
                span: u.span,
            };
            let e = emit_select(&probe, ctx)?;
            out.push_str(&format!("        {arm} => {e},\n"));
            continue;
        }
        let mut b = crate::sql::Builder::new(ctx.model);
        let Some(built) = b.update(u, &sets) else {
            bail!("this update is not expressible yet");
        };
        let e = emit_built_with(&built, &preset, false, ctx)?;
        out.push_str(&format!("        {arm} => {e},\n"));
    }
    out.push_str("    }\n}");
    Ok(out)
}

fn emit_delete(d: &crate::ast::DeleteExpr, ctx: &mut Ctx) -> Result<String> {
    let mut b = crate::sql::Builder::new(ctx.model);
    let Some(built) = b.delete(d) else {
        bail!("this delete is not expressible yet");
    };
    emit_built(&built, &[], ctx)
}

/// One compiled statement, emitted as a call into the prelude's shared
/// statement boundary.
///
/// `preset` are values the caller already evaluated — an `insert`'s or
/// `update`'s, which must not be re-evaluated, exactly as
/// `exec::bind_params` treats them.
fn emit_built(built: &crate::sql::Built, preset: &[String], ctx: &mut Ctx) -> Result<String> {
    emit_built_with(built, preset, false, ctx)
}

/// `all_preset` is the INSERT case: the builder marks every parameter
/// `Bind::Expr` over a placeholder, and the values come positionally from
/// the writer instead.
fn emit_built_with(
    built: &crate::sql::Built,
    preset: &[String],
    all_preset: bool,
    ctx: &mut Ctx,
) -> Result<String> {
    let mut preset = preset.iter();
    let mut binds = Vec::new();
    for p in &built.params {
        let take_preset = all_preset || matches!(p.bind, crate::sql::Bind::Preset);
        if take_preset {
            match preset.next() {
                Some(v) => binds.push(format!("jwc_param_str({v})")),
                None => bail!("statement wants more values than the writer supplied"),
            }
            continue;
        }
        match &p.bind {
            crate::sql::Bind::Expr(e) => {
                let v = emit_expr(e, ctx)?;
                binds.push(format!("jwc_param_str({v})"));
            }
            crate::sql::Bind::Preset => unreachable!("handled above"),
            // queries.md §9 — the cursor's key values, read once before any
            // of them are bound. `Cursor(i)` is the i-th key.
            crate::sql::Bind::Cursor(i) => {
                binds.push(format!("jwc_param_str(__cursor_key({i}))"));
            }
        }
    }
    ctx.used.insert("jwc_db_run".to_string());
    let fields = ctx.field_list_id(built.fields.clone());

    if let Some(plan) = &built.page {
        ctx.uses_page = true;
        // The cursor is read once, before any parameter is bound: its keys
        // are the *caller's* values and every `Bind::Cursor` above reads
        // from the same tuple.
        let arity = built
            .params
            .iter()
            .filter(|p| matches!(p.bind, crate::sql::Bind::Cursor(_)))
            .map(|p| match p.bind {
                crate::sql::Bind::Cursor(i) => i + 1,
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        let after = match &plan.after {
            Some(e) => emit_expr(e, ctx)?,
            None => "V::Null".to_string(),
        };
        return Ok(format!(
            "{{\n\
                 let __cursor = jwc_cursor_binds(&{after}, {arity})?;\n\
                 let __cursor_key = |i: usize| -> V {{\n\
                     match __cursor.get(i) {{\n\
                         Some(Some(s)) => v_str(s.clone()),\n\
                         _ => V::Null,\n\
                     }}\n\
                 }};\n\
                 jwc_db_page({}, vec![{}], {}, JWC_FIELDS_{fields}).await?\n\
             }}",
            rust_str_literal(&built.sql),
            binds.join(", "),
            plan.raw_items,
        ));
    }

    let shape = match built.shape {
        crate::sql::Shape::None => "JWC_SHAPE_NONE",
        crate::sql::Shape::First => "JWC_SHAPE_FIRST",
        crate::sql::Shape::Rows => "JWC_SHAPE_ROWS",
    };
    Ok(format!(
        "jwc_db_run({}, vec![{}], {shape}, {}, JWC_FIELDS_{fields}).await?",
        rust_str_literal(&built.sql),
        binds.join(", "),
        built.record,
    ))
}

/// True when an expression reads a column of the table being written —
/// `exec::reads_a_column`.
fn reads_a_column(table: &crate::ast::QualifiedTable, e: &Expr, ctx: &Ctx) -> bool {
    let object = ctx
        .symbols
        .by_path
        .get(&table.text())
        .cloned()
        .unwrap_or_else(|| table.object.name.clone());
    let Some(t) = ctx.model.tables.iter().find(|t| t.declared == object) else {
        return false;
    };
    match &*e.kind {
        ExprKind::Name(n) => t.column(&n.name).is_some(),
        ExprKind::Binary { lhs, rhs, .. } => {
            reads_a_column(table, lhs, ctx) || reads_a_column(table, rhs, ctx)
        }
        ExprKind::Unary { rhs, .. } => reads_a_column(table, rhs, ctx),
        _ => false,
    }
}

/// The builder wants an `Expr` per bound value but never looks at it — the
/// value comes from `preset`. `exec.rs` passes the same stand-in.
fn placeholder(span: crate::token::Span) -> Expr {
    Expr::new(ExprKind::Null, span)
}

fn callee_name(e: &Expr) -> Result<String> {
    Ok(match &*e.kind {
        ExprKind::Name(i) => i.name.clone(),
        ExprKind::Field { base, field } => {
            let b = callee_name(base)?;
            format!("{b}.{}", field.name)
        }
        _ => bail!("native build cannot resolve this call target"),
    })
}

/// A Rust string literal for `s`, escaped.
fn rust_str_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
