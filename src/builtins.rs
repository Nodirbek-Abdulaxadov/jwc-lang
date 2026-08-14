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
//! ## `min_args` / `max_args` are the contract, and they are enforced
//!
//! They used to be informational, with the real check living in each
//! `eval_*_call` body — so a wrong-arity call reached the backends. The
//! interpreter caught it at runtime; native codegen didn't, and several of
//! its variadic branches padded the missing slots with `V::Null`, turning
//! `raw_sql(sql, a, b)` into a statement that discarded the query and
//! returned 200. `serve("0.0.0.0", 8081)` took the host as the port and
//! bound `:0`. Neither produced a diagnostic anywhere.
//!
//! `typecheck::check_program` (E022) now rejects those at `jwc check`,
//! before either backend sees the program. That makes this table
//! load-bearing: **a row must match what the interpreter actually
//! accepts**, or working programs get rejected. Widen a row only after
//! widening the `eval_*_call` body it describes.
//!
//! The runtime checks stay where they are. They're unreachable from a
//! checked program, but the interpreter is also driven directly by tests
//! and the LSP, and defence in depth here costs one comparison.
//!
//! Adding a new built-in:
//! 1. Add a `BuiltinDef` row here (set `native` to match codegen support).
//! 2. Implement runtime dispatch in the `Expr::Call` arm of `runner::mod`.
//! 3. If native AOT supports it, add the `jwc_b_<name>` impl in
//!    `native_prelude.rs.in` and the codegen branch in `native_build.rs`,
//!    and set `native: true`. Otherwise leave `native: false` and the
//!    program is rejected at native-build time, which is the safer default.

/// Metadata for one built-in function. Both `native` and the arity pair
/// are load-bearing — see module docs.
pub struct BuiltinDef {
    /// Canonical name as written in a JWC program.
    pub name: &'static str,
    /// camelCase / snake_case aliases the interpreter also dispatches.
    pub aliases: &'static [&'static str],
    /// Minimum arg count. Enforced by `typecheck` (E022); must match the
    /// interpreter's own runtime check.
    pub min_args: usize,
    /// Maximum arg count; `None` = variadic. Enforced by `typecheck`
    /// (E022); must match the interpreter's own runtime check.
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
    // `len` is the same function, not a second one. It used to carry its own
    // row with `native: false`, so `length(xs)` compiled under `--native` and
    // `len(xs)` — which the interpreter dispatches to this exact body — was
    // rejected as an unknown function. Same shape as the
    // `setConnectionString` / `set_connection_string` split: one function,
    // two rows, two different answers.
    BuiltinDef {
        name: "length",
        aliases: &["len"],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "lower",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "upper",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "trim",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "contains",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "starts_with",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "ends_with",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "replace",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "split",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "substring",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "take",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "first",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "last",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "json_parse",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "json_stringify",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    // ── HTTP request inspection (native) ─────────────────────────────────
    BuiltinDef {
        name: "path_param",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "query_param",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "body",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "header",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "client_ip",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "request_id",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "response_status",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "response_duration_ms",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "response_duration_us",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "request_path",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "request_method",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── HTTP response helpers (native) ───────────────────────────────────
    BuiltinDef {
        name: "json",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    // Phase 4 [1.0-blocker] — `json_unchecked(s)` skips the string-validation
    // path that `json(s)` now performs by default. Use only when the caller
    // has already validated the payload (e.g. a SELECT result or a `body()`
    // string known to be JSON).
    BuiltinDef {
        name: "json_unchecked",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "text",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "html",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "response",
        aliases: &["raw"],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "ok",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "created",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "not_found",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "no_content",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "unauthorized",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "forbidden",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "internal_error",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "status_code",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    // camelCase aliases historically present in BUILTINS as standalone
    // entries. Kept as separate `native: true` rows (each contributes its
    // own string to the whitelist) so the native set is byte-identical.
    BuiltinDef {
        name: "notFound",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "noContent",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "internalError",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "statusCode",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "badRequest",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "bad_request",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    // ── DB connection (native) ───────────────────────────────────────────
    // `set_connection_string` is an ALIAS, not a second def. It used to be
    // its own row with `native: false`, so the two spellings of one function
    // disagreed about AOT support: `setConnectionString()` compiled and
    // `set_connection_string()` was rejected as an unknown function. Every
    // other camel/snake pair in this table (`setContext`/`set_context`,
    // `notFound`/`not_found`) is one def with an alias — this is that.
    BuiltinDef {
        name: "setConnectionString",
        aliases: &["set_connection_string"],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
    // ── WebSocket (native) ───────────────────────────────────────────────
    BuiltinDef {
        name: "ws_send",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "ws_recv",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "ws_close",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── Async I/O (native) ───────────────────────────────────────────────
    BuiltinDef {
        name: "sleep_ms",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "http_get",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "fetch_json",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    // ── Env / coercion (native) ──────────────────────────────────────────
    BuiltinDef {
        name: "env",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "int",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    // ── Time & identifiers (native) ──────────────────────────────────────
    BuiltinDef {
        name: "now",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "uuid",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── In-memory cache (native) ─────────────────────────────────────────
    BuiltinDef {
        name: "cache_get",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "cache_set",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "cache_del",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "cache_clear",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── Redis (native, shared across processes) ──────────────────────────
    //
    // The cross-process counterpart to `cache_*` above: same key/value
    // shape and the same `ttl_secs == 0 means no expiry` contract, so the
    // `redis` package can fall back from one to the other without the
    // meaning of an argument changing.
    //
    // These rows are NOT behind `#[cfg(feature = "redis")]`, deliberately.
    // `BUILTIN_DEFS` feeds typecheck arity (E022), the LSP completion list
    // and the generated `docs/docs/reference/builtins.md` — if the rows
    // came and went with a build flag, `jwc check` would accept or reject
    // the same program depending on how the binary was compiled, and
    // `tests/builtins_doc_sync.rs` would pass or fail for the same reason.
    // Only the *implementation* is feature-gated (`src/redis_engine.rs`);
    // without it these raise an error naming the missing build flag.
    //
    // All of them are async, so every one MUST also appear in the
    // `is_async_builtin` list in `native_build.rs` — see the note above the
    // file built-ins.
    BuiltinDef {
        name: "redis_get",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "redis_set",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "redis_del",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "redis_exists",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "redis_incr",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "redis_expire",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "redis_eval",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "redis_ping",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "redis_enabled",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── Buffered telemetry write (native) ────────────────────────────────
    //
    // `log_insert(Entity, record)` queues a row for the batched writer in
    // `src/log_writer.rs` instead of writing it inline. Deliberately a
    // separate name rather than a mode of `insert`: the durability contract
    // is different (rows are lost on crash, and dropped rather than queued
    // without bound when the writer falls behind), and that difference
    // belongs at the call site where someone reading the handler can see it.
    //
    // The one built-in here that is intentionally NOT async. `try_send` on a
    // bounded channel never suspends, and that is the entire point — this
    // runs on the request path. It must therefore stay out of the
    // `is_async_builtin` list in `native_build.rs`, so codegen emits the
    // call without `.await` and the prelude mirror stays a plain `fn`.
    BuiltinDef {
        name: "log_insert",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    // ── Raw SQL escape hatch (native) ────────────────────────────────────
    BuiltinDef {
        name: "raw_sql",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    // ── Array helpers (native) ───────────────────────────────────────────
    BuiltinDef {
        name: "range",
        aliases: &[],
        min_args: 1,
        max_args: Some(3),
        native: true,
    },
    BuiltinDef {
        name: "push",
        aliases: &["append"],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "join",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    // ── Hash / crypto (native) ───────────────────────────────────────────
    BuiltinDef {
        name: "sha256",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "sha1",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "md5",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "hmac_sha256",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    // ── Console I/O (native) ─────────────────────────────────────────────
    //
    // Dotted names work because the parser flattens `a.b(...)` into a single
    // `Expr::Call` name, so `lookup` / `is_builtin` / the E022 arity check
    // all see the literal string "console.write". Native codegen maps the
    // `.` to `_` in `builtin_fn_name` (`native_build.rs`), giving
    // `jwc_b_console_write`.
    //
    // `console.write` / `console.error` write through IMMEDIATELY. The
    // `print` statement does NOT: it appends to `Vm::output`, which
    // `cmd::run` flushes only after `main()` returns (`runner/exec.rs`,
    // `cmd/run.rs`) and which `dispatch` consumes as the implicit response
    // body of a fall-through route. Native `jwc_print` is a bare `println!`,
    // so mixing the two forms orders differently on the two backends.
    // Documented in `docs/docs/stdlib/io.md`; don't "fix" one side alone.
    BuiltinDef {
        name: "console.write",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "console.writeln",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "console.error",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "console.read",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // ── File + directory I/O (native) ────────────────────────────────────
    //
    // Paths reach the OS verbatim — no jail, no allowlist, no root env var.
    // A route that does `file.read(path_param("f"))` is a local-file-include
    // vulnerability in the application, and that is the application author's
    // problem by design. Recorded as an accepted risk in
    // `docs/spec/threat-model.md`, NOT as an oversight.
    //
    // All of these are async (`tokio::fs`, so a slow mount can't park a
    // reactor worker mid-request). Every one MUST also appear in the
    // `is_async_builtin` list in `native_build.rs` or codegen emits the call
    // without `.await` and the generated crate fails to compile.
    BuiltinDef {
        name: "file.read",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "file.write",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "file.append",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "file.exists",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "file.delete",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "file.copy",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "file.move",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "file.size",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "file.lines",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "directory.list",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "directory.create",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "directory.exists",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "directory.delete",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    // ── Interpreter-only built-ins (native: false) ───────────────────────
    // Dispatched by the interpreter's `Expr::Call` arm but NOT accepted by
    // native AOT codegen. Listing them here was previously omitted; doing so
    // now makes lint W006 and the native-build validator aware of them
    // (warning-only / more-correct). It does NOT widen the native whitelist
    // because `native: false`.
    BuiltinDef {
        name: "dispatch",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: false,
    },
    BuiltinDef {
        name: "context",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "setContext",
        aliases: &["set_context"],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "http_post",
        aliases: &[],
        min_args: 1,
        max_args: Some(3),
        native: false,
    },
    BuiltinDef {
        name: "jwt_sign",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "jwt_verify",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "jwt_verify_jwks",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "hash_password",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: true,
    },
    BuiltinDef {
        name: "verify_password",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "send_email",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: false,
    },
    BuiltinDef {
        name: "register_job_handler",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: false,
    },
    BuiltinDef {
        name: "enqueue",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: false,
    },
    BuiltinDef {
        name: "enqueue_urgent",
        aliases: &[],
        min_args: 2,
        max_args: Some(2),
        native: false,
    },
    BuiltinDef {
        name: "job_count",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: false,
    },
    BuiltinDef {
        name: "dlq_count",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: false,
    },
    BuiltinDef {
        name: "dlq_drain",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: false,
    },
    BuiltinDef {
        name: "db_query",
        aliases: &[],
        min_args: 1,
        max_args: Some(1),
        native: false,
    },
    BuiltinDef {
        name: "request_body",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    BuiltinDef {
        name: "unix_timestamp",
        aliases: &[],
        min_args: 0,
        max_args: Some(0),
        native: true,
    },
    // Not cryptographic — `uuid()` is the builtin for unguessable values.
    BuiltinDef {
        name: "random_int",
        aliases: &[],
        min_args: 1,
        max_args: Some(2),
        native: true,
    },
    BuiltinDef {
        name: "set_json_field",
        aliases: &[],
        min_args: 3,
        max_args: Some(3),
        native: false,
    },
    // ── Server entry point ───────────────────────────────────────────────
    //
    // Also listed in [`SPECIAL_BUILTINS`] because codegen emits it inline
    // rather than through `jwc_b_serve` dispatch. It lives here too so the
    // arity check below covers it: `serve("0.0.0.0", 8081)` used to pass
    // `jwc check`, then bind port 0 in a native build because codegen took
    // `args.first()` — the host — as the port.
    BuiltinDef {
        name: "serve",
        aliases: &[],
        min_args: 0,
        max_args: Some(1),
        native: true,
    },
];

/// Built-ins that codegen handles itself (not via `jwc_b_<name>` dispatch).
pub const SPECIAL_BUILTINS: &[&str] = &["serve"];

/// Look up a built-in by canonical name or alias, case-insensitively —
/// the same matching rule the interpreter's dispatch uses.
pub fn lookup(name: &str) -> Option<&'static BuiltinDef> {
    BUILTIN_DEFS.iter().find(|def| {
        def.name.eq_ignore_ascii_case(name)
            || def.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    })
}

impl BuiltinDef {
    /// `true` if a call with `n` arguments is within this built-in's
    /// declared arity.
    pub fn accepts_arity(&self, n: usize) -> bool {
        n >= self.min_args && self.max_args.is_none_or(|max| n <= max)
    }

    /// Human-readable arity for a diagnostic: `"2"`, `"1 or 2"`,
    /// `"1 to 3"`, `"at least 1"`.
    pub fn arity_label(&self) -> String {
        match self.max_args {
            Some(max) if max == self.min_args => max.to_string(),
            Some(max) if max == self.min_args + 1 => format!("{} or {}", self.min_args, max),
            Some(max) => format!("{} to {}", self.min_args, max),
            None => format!("at least {}", self.min_args),
        }
    }
}

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
