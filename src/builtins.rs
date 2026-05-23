//! Single source of truth for built-in function names.
//!
//! Lives outside `native_build.rs` so the lint pass, the validator, and any
//! future tooling (LSP completion, docs generator) can reference the same
//! list without pulling in the codegen-heavy native module. The interpreter
//! itself dispatches via `runner::Vm::call_builtin` — both lookups must
//! agree, but the native AOT path already enforces that via
//! `reject_unsupported`.
//!
//! Adding a new built-in:
//! 1. Add it here (`BUILTINS` or `SPECIAL_BUILTINS`).
//! 2. Implement runtime dispatch in `runner::Vm::call_builtin`.
//! 3. If native AOT supports it, add the `jwc_b_<name>` impl in
//!    `native_prelude.rs.in` and the codegen branch in `native_build.rs`.
//!    Otherwise the program will be rejected at native-build time, which
//!    is the safer default.

/// User-callable built-ins. Names match the form a JWC program writes:
/// `length(...)`, `json(...)`, etc. The interpreter and native pipeline
/// both look up calls against this list; the lint pass uses it for
/// W005 (function shadows a built-in).
pub const BUILTINS: &[&str] = &[
    "length",
    "lower",
    "upper",
    "trim",
    "contains",
    "starts_with",
    "ends_with",
    "replace",
    "split",
    "first",
    "last",
    "json_parse",
    "json_stringify",
    // HTTP request inspection — only meaningful inside a route body.
    "path_param",
    "query_param",
    "body",
    "header",
    // HTTP response helpers.
    "json",
    "text",
    "ok",
    "created",
    "not_found",
    "no_content",
    "unauthorized",
    "forbidden",
    "internal_error",
    "status_code",
    // camelCase aliases the interpreter exposes; normalized in builtin_fn_name.
    "notFound",
    "noContent",
    "internalError",
    "statusCode",
    "badRequest",
    "bad_request",
    // DB.
    "setConnectionString",
    // WebSocket — only valid inside a `route WS "/path" { ... }` body.
    "ws_send",
    "ws_recv",
    "ws_close",
    // Async I/O built-ins (real tokio after the async migration).
    "sleep_ms",
    "http_get",
    "fetch_json",
    // Process env / type coercion (sync, no IO).
    "env",
    "int",
];

/// Built-ins that codegen handles itself (not via `jwc_b_<name>` dispatch).
pub const SPECIAL_BUILTINS: &[&str] = &["serve"];
