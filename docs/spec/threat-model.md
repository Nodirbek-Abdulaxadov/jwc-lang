# JWC Threat Model

This document is the runtime-security baseline for the JWC compiler and
server. It enumerates the threats we have explicitly mitigated, where
the mitigation landed, and the residual risk an operator should keep
in mind. Each row cites `file:line` for the actual code so a reviewer
can audit drift in a future change.

PRs that touch the HTTP server, the SQL layer, the JWT helpers, or any
log path that handles connection strings MUST re-read this document
and update the relevant row.

## Mitigations

| # | Threat | Current state | Mitigation shipped | Residual risk / escalation |
|---|--------|---------------|--------------------|----------------------------|
| 1 | Path traversal in `{param}` capture | Mitigated | `src/runner/dispatch.rs::match_route_pattern` (line ~470) calls `is_traversal_segment` for every `{param}` capture. Segments equal to `.` / `..`, or containing `/` / `\` / NUL, fail the match and the dispatcher falls through to a clean 404 instead of running the handler. Regression coverage: `match_route_pattern_rejects_dot_dot_param`, `match_route_pattern_rejects_single_dot_param`, `match_route_pattern_rejects_backslash_in_param`, `is_traversal_segment_recognises_classic_patterns`. | URL-decoded `%2f` / `%2e` bytes are normalised by the HTTP layer before reaching the matcher, so the matcher only sees real `.` / `..` strings. If a future change adds a builtin that uses `path_param(name)` as a filesystem key, the value is already screened — but the burden moves to that builtin to also reject absolute paths starting with `/` or `C:\`. |
| 2 | Header injection via `response(body, mime)` / extra headers | Mitigated | Two parallel checks, kept shape-equivalent: interpreter path (`src/server.rs` lines 848–855) uses `axum::http::HeaderName::parse` + `HeaderValue::parse`, which reject `\r` / `\n` / NUL by design; native-AOT path (`src/native_prelude.rs.in` line ~1617) runs an explicit byte loop over both name AND value (the value check was added in Phase 6 — earlier code only screened the name). | Both paths SILENTLY drop the offending pair rather than 500-ing, on the grounds that the redirect URL is the high-value response and a bogus `Location` header is recoverable. Operators who want to know when this fires should grep server logs for handler output that omits the header. |
| 3 | SSRF via `http_get` / `fetch_json` / `http_post` | Mitigated (opt-in) | `src/runner/util.rs::check_url_allowlisted` reads `JWC_HTTP_ALLOWLIST` (CSV hostnames) into a `OnceLock` and gates every outbound HTTP builtin. Empty / unset = no restriction (backwards-compatible). Wired into `src/runner/builtins.rs::eval_http_get_call` (line ~104), `eval_http_post_call` (line ~144), and `src/runner/eval.rs::fetch_json` (line ~654). Mirrored in the native-AOT prelude as `jwc_check_url_allowlisted` (`src/native_prelude.rs.in` line ~1666) so `jwc build --native` binaries enforce the same rule. Env var registered in `src/config.rs::REGISTRY` so it appears in the boot config table. Tests: `ssrf_allowlist_blocks_unlisted_host`, `ssrf_allowlist_permits_listed_host`, `ssrf_allowlist_empty_means_no_restriction`. | The default is permissive — operators who route untrusted user input into the outbound URL MUST set `JWC_HTTP_ALLOWLIST`. The check is hostname-exact (no wildcard, no port match). DNS rebinding is NOT mitigated here — if you depend on a private allowlist, run the egress through a corporate proxy that re-resolves the name. |
| 4 | JWT `exp` enforcement | Mitigated | `src/jwt.rs::verify_hs256` checks the `exp` claim after signature verification. Absent `exp` → accepted (long-lived API keys keep working). Present-but-past `exp` → rejected with a message containing `"token expired"`. The error classifies as `JwtError.Expired` via `src/runner/mod.rs::JWC_ERROR_KINDS` + the `classify_jwc_error` JWT branch. Tests: `jwt_verify_accepts_token_without_exp`, `jwt_verify_rejects_expired_token`, `jwt_verify_accepts_future_exp`. | No `nbf` (not-before) check yet — a token with a future `nbf` would still verify. No clock-skew tolerance (a 1-second-future `exp` is accepted, a 1-second-past `exp` is rejected). Both are deliberate: Phase 6 closes the deferral from Sprint 3A; nbf + skew lands when a user actually asks for it. |
| 5 | SQL interpolation surface | Clean | Audit of `format!.*SELECT/INSERT/UPDATE/DELETE/WHERE` across `src/runner/sql.rs`, `src/runner/exec.rs`, `src/runner/eval.rs`, and `src/native_build.rs`: every match formats a TABLE name (resolved by the compiler from the AST via `sql::to_snake_case`, not user-controlled), a COLUMN name (validated by `parser::validate_program`), an aggregate function name (compiler-resolved), or an operator literal. User-controlled values flow through `Vec<Box<dyn ToSql + Sync + Send>>` (boxed_params) and bind to `$N` placeholders via `tokio_postgres::query`. Sites verified: `eval.rs:129`, `eval.rs:181`, `exec.rs:456`, `sql.rs:336`, `native_build.rs:2382/2452/2516/2576/3322/3535`. The escape hatch `raw_sql(sql[, params_json])` (`builtins.rs:724`) takes the SQL string verbatim and binds `params_json` positionally — documented as a footgun in `SECURITY.md`. | If a future change introduces a builtin that constructs SQL from a user-controlled identifier (table / column name), it MUST quote-and-validate the identifier (no Postgres-side `quote_ident` is invoked today because no such builtin exists). |

## Out of scope (recorded, not mitigated)

- DNS rebinding behind `JWC_HTTP_ALLOWLIST` — see row 3.
- Timing-side-channel attacks on `verify_password` / `verify_hs256` — the
  underlying crates (`argon2`, `hmac`) use constant-time primitives, but
  a downstream caller that does `if verify_hs256(...).is_ok() { return
  "ok"; } else { return "fail"; }` will leak the same signal as any
  password check.
- Memory zeroisation — JWT secrets and DB passwords sit in `String` /
  `Vec<u8>` until drop; we do not pin or wipe.

## How to update this document

1. Identify the threat row your change touches.
2. Update the `Mitigation shipped` cell with the new `file:line`.
3. Add or extend the regression test referenced in the same cell.
4. If the change introduces a NEW threat class not on the list,
   add a new row and run the change past the security-review skill
   (`/security-review`) before merging.
