//! Single source of truth for built-in function metadata.
//!
//! Lives outside `native_build.rs` so the lint pass, the validator, and any
//! future tooling (LSP completion, docs generator) can reference the same
//! table without pulling in the codegen-heavy native module. The interpreter
//! itself dispatches via the `Expr::Call` arm in `runner::mod` (method bodies
//! in `runner::builtins`); the `BUILTIN_DEFS` table below mirrors that
//! dispatch so every lookup agrees.
//!
//! ## The `native` flag is the load-bearing invariant
//!
//! `native_build.rs` rejects any call whose name isn't in the native AOT
//! whitelist (the "safer default" — unknown calls fail at native-build time
//! rather than miscompiling). That whitelist is exactly the set of
//! name+aliases on defs marked `native: true`, plus [`SPECIAL_BUILTINS`].
//! Historically this was a flat `BUILTINS` list; the `native: true` defs here
//! reproduce that list string-for-string (case-sensitive). Interpreter-only
//! built-ins (`dispatch`, `http_post`, `jwt_*`, jobs/email, etc.) carry
//! `native: false`: the interpreter runs them, but `jwc build --native`
//! still rejects programs that use them, preserving prior behaviour.
//!
//! `min_args` / `max_args` are INFORMATIONAL only (for tooling / docs). The
//! runtime arity checks still live in each `eval_*_call` body — this table
//! adds no enforcement.
//!
//! Adding a new built-in:
//! 1. Add a `BuiltinDef` row here (set `native` to match codegen support).
//! 2. Implement runtime dispatch in the `Expr::Call` arm of `runner::mod`.
//! 3. If native AOT supports it, add the `jwc_b_<name>` impl in
//!    `native_prelude.rs.in` and the codegen branch in `native_build.rs`,
//!    and set `native: true`. Otherwise leave `native: false` and the
//!    program is rejected at native-build time, which is the safer default.

/// Metadata for one built-in function. See module docs for the meaning of
/// `native` (load-bearing) vs. `min_args`/`max_args` (informational).
pub struct BuiltinDef {
    /// Canonical name as written in a JWC program.
    pub name: &'static str,
    /// camelCase / snake_case aliases the interpreter also dispatches.
    pub aliases: &'static [&'static str],
    /// Minimum arg count the interpreter enforces (informational).
    pub min_args: usize,
    /// Maximum arg count the interpreter enforces; `None` = variadic
    /// (informational).
    pub max_args: Option<usize>,
    /// `true` if native AOT codegen accepts this built-in. Drives the
    /// native-build whitelist — see module docs.
    pub native: bool,
}

/// The single built-in registry. Rows with `native: true` reproduce the
/// historical flat `BUILTINS` list string-for-string (the native whitelist);
/// rows with `native: false` are interpreter-only.
pub static BUILTIN_DEFS: &[BuiltinDef] = &[
    // ── String helpers (native) ─────────────────────────────────────────
    BuiltinDef { name: "length", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "lower", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "upper", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "trim", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "contains", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "starts_with", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "ends_with", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "replace", aliases: &[], min_args: 3, max_args: Some(3), native: true },
    BuiltinDef { name: "split", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "first", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "last", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "json_parse", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "json_stringify", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    // ── HTTP request inspection (native) ─────────────────────────────────
    BuiltinDef { name: "path_param", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "query_param", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "body", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "header", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "request_path", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "request_method", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    // ── HTTP response helpers (native) ───────────────────────────────────
    BuiltinDef { name: "json", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "text", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "html", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "ok", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    BuiltinDef { name: "created", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "not_found", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "no_content", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "unauthorized", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "forbidden", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "internal_error", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    BuiltinDef { name: "status_code", aliases: &[], min_args: 1, max_args: Some(2), native: true },
    // camelCase aliases historically present in BUILTINS as standalone
    // entries. Kept as separate `native: true` rows (each contributes its
    // own string to the whitelist) so the native set is byte-identical.
    BuiltinDef { name: "notFound", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "noContent", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "internalError", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    BuiltinDef { name: "statusCode", aliases: &[], min_args: 1, max_args: Some(2), native: true },
    BuiltinDef { name: "badRequest", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    BuiltinDef { name: "bad_request", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    // ── DB connection (native) ───────────────────────────────────────────
    BuiltinDef { name: "setConnectionString", aliases: &[], min_args: 0, max_args: Some(1), native: true },
    // ── WebSocket (native) ───────────────────────────────────────────────
    BuiltinDef { name: "ws_send", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "ws_recv", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "ws_close", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    // ── Async I/O (native) ───────────────────────────────────────────────
    BuiltinDef { name: "sleep_ms", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "http_get", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "fetch_json", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    // ── Env / coercion (native) ──────────────────────────────────────────
    BuiltinDef { name: "env", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "int", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    // ── Time & identifiers (native) ──────────────────────────────────────
    BuiltinDef { name: "now", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    BuiltinDef { name: "uuid", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    // ── In-memory cache (native) ─────────────────────────────────────────
    BuiltinDef { name: "cache_get", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "cache_set", aliases: &[], min_args: 2, max_args: Some(3), native: true },
    BuiltinDef { name: "cache_del", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "cache_clear", aliases: &[], min_args: 0, max_args: Some(0), native: true },
    // ── Raw SQL escape hatch (native) ────────────────────────────────────
    BuiltinDef { name: "raw_sql", aliases: &[], min_args: 1, max_args: None, native: true },
    // ── Array helpers (native) ───────────────────────────────────────────
    BuiltinDef { name: "range", aliases: &[], min_args: 1, max_args: Some(3), native: true },
    BuiltinDef { name: "push", aliases: &["append"], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "join", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    // ── Hash / crypto (native) ───────────────────────────────────────────
    BuiltinDef { name: "sha256", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "sha1", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "md5", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "hmac_sha256", aliases: &[], min_args: 2, max_args: Some(2), native: true },

    // ── Interpreter-only built-ins (native: false) ───────────────────────
    // Dispatched by the interpreter's `Expr::Call` arm but NOT accepted by
    // native AOT codegen. Listing them here was previously omitted; doing so
    // now makes lint W006 and the native-build validator aware of them
    // (warning-only / more-correct). It does NOT widen the native whitelist
    // because `native: false`.
    BuiltinDef { name: "dispatch", aliases: &[], min_args: 1, max_args: None, native: false },
    BuiltinDef { name: "context", aliases: &[], min_args: 1, max_args: Some(1), native: false },
    BuiltinDef { name: "setContext", aliases: &["set_context"], min_args: 2, max_args: Some(2), native: false },
    BuiltinDef { name: "http_post", aliases: &[], min_args: 1, max_args: Some(2), native: false },
    BuiltinDef { name: "jwt_sign", aliases: &[], min_args: 1, max_args: Some(2), native: false },
    BuiltinDef { name: "jwt_verify", aliases: &[], min_args: 1, max_args: Some(2), native: false },
    BuiltinDef { name: "hash_password", aliases: &[], min_args: 1, max_args: Some(1), native: true },
    BuiltinDef { name: "verify_password", aliases: &[], min_args: 2, max_args: Some(2), native: true },
    BuiltinDef { name: "send_email", aliases: &[], min_args: 1, max_args: None, native: false },
    BuiltinDef { name: "register_job_handler", aliases: &[], min_args: 2, max_args: Some(2), native: false },
    BuiltinDef { name: "enqueue", aliases: &[], min_args: 1, max_args: None, native: false },
    BuiltinDef { name: "enqueue_urgent", aliases: &[], min_args: 1, max_args: None, native: false },
    BuiltinDef { name: "job_count", aliases: &[], min_args: 0, max_args: None, native: false },
    BuiltinDef { name: "dlq_count", aliases: &[], min_args: 0, max_args: None, native: false },
    BuiltinDef { name: "dlq_drain", aliases: &[], min_args: 0, max_args: None, native: false },
    BuiltinDef { name: "db_query", aliases: &[], min_args: 1, max_args: None, native: false },
    BuiltinDef { name: "request_body", aliases: &[], min_args: 0, max_args: Some(0), native: false },
    BuiltinDef { name: "len", aliases: &[], min_args: 1, max_args: Some(1), native: false },
    BuiltinDef { name: "unix_timestamp", aliases: &[], min_args: 0, max_args: Some(0), native: false },
    BuiltinDef { name: "set_json_field", aliases: &[], min_args: 3, max_args: Some(3), native: false },
    BuiltinDef { name: "set_connection_string", aliases: &[], min_args: 0, max_args: Some(1), native: false },
];

/// Built-ins that codegen handles itself (not via `jwc_b_<name>` dispatch).
pub const SPECIAL_BUILTINS: &[&str] = &["serve"];

/// True if `name` matches any built-in's canonical name or alias,
/// case-insensitively (mirroring the interpreter's `eq_ignore_ascii_case`
/// dispatch). Includes interpreter-only built-ins.
pub fn is_builtin(name: &str) -> bool {
    BUILTIN_DEFS.iter().any(|def| {
        def.name.eq_ignore_ascii_case(name)
            || def.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    })
}

/// Every name + alias whose def is native-codegen capable (`native == true`).
/// This is the native AOT whitelist (combine with [`SPECIAL_BUILTINS`]).
/// Order follows `BUILTIN_DEFS`; strings are returned verbatim (case-sensitive)
/// so the native accept-set is byte-identical to the historical `BUILTINS`.
pub fn native_builtin_names() -> Vec<&'static str> {
    let mut out = Vec::new();
    for def in BUILTIN_DEFS {
        if def.native {
            out.push(def.name);
            out.extend_from_slice(def.aliases);
        }
    }
    out
}
