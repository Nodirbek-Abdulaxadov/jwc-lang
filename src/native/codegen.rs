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

use crate::ast::{BinOp, Block, Decl, Expr, ExprKind, ObjEntry, Stmt, UnaryOp};
use crate::workspace::Workspace;

/// The prelude built-ins declared `async fn`. Emitting `.await` on a
/// plain value does not compile, and omitting it where one is needed
/// yields a future where a `V` was wanted, so the list is derived from
/// the prelude rather than guessed.
const ASYNC_BUILTINS: &[&str] = &[
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
        "jwt.sign" => "jwc_b_jwt_sign",
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

        // Coercions and the environment.
        "int" => "jwc_b_int",
        "env" => "jwc_b_env",

        _ => return None,
    })
}

/// Built-ins the 1.0 language has and the restored prelude does not.
///
/// Named individually so the refusal says which one, and so the list is a
/// worklist rather than a shrug. Each is a prelude addition, not a codegen
/// problem.
const PRELUDE_GAPS: &[&str] = &[
    "string.of",
    "string.slice",
    "string.pad_left",
    "string.pad_right",
    "string.matches",
    "string.split_csv",
    "string.strip_prefix",
    "array.is_empty",
    "array.sum",
    "array.sum_product",
    "array.min",
    "array.max",
    "array.pluck",
    "array.sorted",
    "date.now",
    "date.today",
    "date.days",
    "date.hours",
    "date.minutes",
    "date.seconds",
    "date.add",
    "date.parse",
    "date.format",
    "hash.verify",
    "hash.hmac_verify",
    "crypto.token",
    "crypto.constant_time_eq",
    "content",
    "redirect",
    "accepted",
    "conflict",
    "tooManyRequests",
    "cookie",
    "bigint",
    "numeric",
    "boolean",
    "uuid",
    "timestamptz",
    "enum",
    "raw",
    "request.body",
    "request.query_all",
    "request.route",
    "request.peer_ip",
    "response.set_header",
    "response.add_header",
    "debug.dump",
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
            match decl {
                Decl::Table(d) => blocked.push(format!("table `{}`", d.name.name)),
                Decl::View(d) => blocked.push(format!("view `{}`", d.name.name)),
                Decl::Service(d) => blocked.push(format!("service `{}`", d.name.name)),
                Decl::Middleware(d) => blocked.push(format!("middleware `{}`", d.name.name)),
                Decl::Database(_) => blocked.push("a `database` declaration".into()),
                _ => {}
            }
        }
    }
    blocked.sort();
    blocked.dedup();
    if !blocked.is_empty() {
        bail!(
            "native build does not cover this program yet:\n  {}\n\n\
             This pass lowers the database-free tier — routes, control flow, \
             expressions and the built-ins the prelude implements. Queries \
             are next and are cheap, because `query_sql` already produces \
             the SQL at compile time.\n\n\
             `jwc serve` runs the whole language today.",
            blocked.join("\n  ")
        );
    }
    Ok(())
}

struct Ctx {
    /// Object-literal shapes, interned so the field-name `Arc` is allocated
    /// once per distinct key set rather than per construction.
    shapes: BTreeMap<Vec<String>, usize>,
}

impl Ctx {
    fn shape_id(&mut self, keys: Vec<String>) -> usize {
        let next = self.shapes.len();
        *self.shapes.entry(keys).or_insert(next)
    }
}

/// Lower a checked workspace to a Rust source file.
pub fn generate(ws: &Workspace) -> Result<String> {
    reject_unsupported(ws)?;

    let mut ctx = Ctx {
        shapes: BTreeMap::new(),
    };
    let mut out = String::new();

    out.push_str(super::PRELUDE_BASE);
    out.push_str("\n// ── generated from the program ──\n");
    out.push_str(
        "\nstatic JWC_SERVE_PORT: ::std::sync::atomic::AtomicU16 = \
         ::std::sync::atomic::AtomicU16::new(8080);\n",
    );

    let mut routes: Vec<(String, String, String)> = Vec::new();
    for file in &ws.files {
        for decl in &file.program.decls {
            match decl {
                Decl::Routes(r) => {
                    for route in &r.routes {
                        let path = join_path(&r.prefix, &route.suffix);
                        let name = handler_name(&route.method.name, &path);
                        // The body goes in its own `async fn` and the
                        // registered symbol is a plain `fn` that boxes it.
                        // `Router` stores `fn() -> Pin<Box<dyn Future>>`, a
                        // fn *pointer*, and an `async fn` is a distinct fn
                        // *item* with an anonymous future type, so it cannot
                        // coerce.
                        out.push_str(&format!("\nasync fn {name}_body() -> V {{\n"));
                        emit_block(&mut out, &route.body, 1, &mut ctx)?;
                        out.push_str("    V::Null\n}\n");
                        out.push_str(&format!(
                            "\nfn {name}() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = V> + Send>> {{\n                             \x20   Box::pin({name}_body())\n}}\n"
                        ));
                        routes.push((route.method.name.to_uppercase(), path, name));
                    }
                }
                Decl::Function(f) if f.name.name == "main" => {
                    out.push_str("\nasync fn jwc_user_main() -> V {\n");
                    emit_block(&mut out, &f.body, 1, &mut ctx)?;
                    out.push_str("    V::Null\n}\n");
                }
                Decl::Function(f) => {
                    out.push_str(&format!("\nasync fn {}(", user_fn(&f.name.name)));
                    for (i, p) in f.params.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&format!("{}: V", local(&p.name.name)));
                    }
                    out.push_str(") -> V {\n");
                    emit_block(&mut out, &f.body, 1, &mut ctx)?;
                    out.push_str("    V::Null\n}\n");
                }
                _ => {}
            }
        }
    }

    emit_shapes(&mut out, &ctx);
    emit_dispatch(&mut out, &routes);
    out.push_str(
        "\n#[tokio::main(flavor = \"multi_thread\")]\nasync fn main() {\n\
         \x20   jwc_install_panic_hook();\n\
         \x20   jwc_load_dotenv();\n\
         \x20   // `main` runs, and `serve(port)` inside it records where to\n\
         \x20   // listen — the same order the interpreter uses, so a program\n\
         \x20   // that hardcodes its port gets that port on both backends.\n\
         \x20   let _ = jwc_user_main().await;\n\
         \x20   jwc_serve_impl(JWC_SERVE_PORT.load(::std::sync::atomic::Ordering::SeqCst)).await;\n}\n",
    );
    Ok(out)
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

fn emit_stmt(out: &mut String, stmt: &Stmt, indent: usize, ctx: &mut Ctx) -> Result<()> {
    let pad = "    ".repeat(indent);
    match stmt {
        Stmt::Let { name, value, .. } => {
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
                        "{pad}jwc_b_set_context({}, {v});\n",
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
        Stmt::Return { value, .. } => match value {
            Some(v) => {
                let e = emit_expr(v, ctx)?;
                out.push_str(&format!("{pad}return {e};\n"));
            }
            None => out.push_str(&format!("{pad}return V::Null;\n")),
        },
        Stmt::Break { .. } => out.push_str(&format!("{pad}break;\n")),
        Stmt::Continue { .. } => out.push_str(&format!("{pad}continue;\n")),
        Stmt::Throw { error, args, .. } => {
            let msg = match args.first() {
                Some(a) => emit_expr(a, ctx)?,
                None => "V::Null".into(),
            };
            // The prelude models a thrown error as the value the dispatcher
            // turns into a response, which is what `jwc_error_value` builds.
            out.push_str(&format!(
                "{pad}return jwc_error_value(format!(\"{{}}: {{}}\", {}, jwc_str_view(&{msg}).unwrap_or(\"\")));\n",
                rust_str_literal(&error.name)
            ));
        }
        Stmt::Expr { expr, .. } => {
            let e = emit_expr(expr, ctx)?;
            out.push_str(&format!("{pad}let _ = {e};\n"));
        }
        Stmt::Transaction { .. } => bail!("native build does not cover `transaction` yet"),
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
        ExprKind::PathParam(i) => format!("jwc_b_path_param({})", rust_str_literal(&i.name)),
        ExprKind::Name(i) => bail!(
            "native build cannot resolve the bare name `{}` outside a query",
            i.name
        ),

        ExprKind::Field { base, field } => {
            // `context.<key>` is a read, not a field access on a value.
            if let ExprKind::Name(n) = &*base.kind {
                if n.name == "context" {
                    return Ok(format!("jwc_b_context({})", rust_str_literal(&field.name)));
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
            if name == "request.query" {
                // 1.0 answers `text?`; the prelude takes the absent-value as
                // a second argument, and `null` is what `text?` means.
                return Ok(format!(
                    "jwc_b_query_param({}, V::Null)",
                    parts.first().cloned().unwrap_or_else(|| "V::Null".into())
                ));
            }
            if let Some(f) = prelude_fn(&name) {
                // The prelude mixes sync and async builtins, so the suffix
                // is looked up rather than guessed: an `.await` on a plain
                // value does not compile, and a missing one yields a future
                // where a `V` was wanted.
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
                format!("{}({}).await", user_fn(&name), parts.join(", "))
            }
        }

        ExprKind::WithHeaders { .. } => {
            bail!("native build does not cover `with {{ … }}` headers yet")
        }
        ExprKind::Cookie { .. } => bail!("native build does not cover `cookie(...)` yet"),
        ExprKind::Cast { .. } => bail!("native build does not cover `as <Class>` yet"),
        ExprKind::Select(_) | ExprKind::Insert(_) | ExprKind::Update(_) | ExprKind::Delete(_) => {
            bail!("native build does not cover queries yet")
        }
        ExprKind::In { .. } => bail!("native build does not cover `in (...)` yet"),
        ExprKind::Exists { .. } => bail!("`exists` is a query construct"),
        ExprKind::OrThrow { .. } => bail!("native build does not cover `or throw` yet"),
        ExprKind::CatchPostfix { .. } => bail!("native build does not cover a postfix `catch` yet"),
    })
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
