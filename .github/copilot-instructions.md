# Copilot Instructions — JWC prototype repo

## Goal
Build a minimal but real end-to-end prototype for **JWC (Just Web Code)**:
- Parse a small JWC subset (`dbcontext`, `entity`)
- Validate at “compile-time” (CLI time)
- Generate Postgres schema SQL (`CREATE TABLE`)

The project is intentionally small and iteration-focused.

## Current state (baseline)
- Rust CLI crate `jwc`
- Commands:
  - `cargo run -- check <file.jwc>`
  - `cargo run -- gen-sql <file.jwc>`
- Parser supports:
  - `dbcontext Name : Driver;`
  - `entity Name { field type; ... }`
  - Types: `int`, `bigint`, `text`, `varchar`, `decimal`, `bool`, `uuid`, `datetime`, `json`
- Unit tests exist; `cargo test` should pass.

## Non-goals (until explicitly requested)
- No full web framework, router, auth, caching implementation
- No WASM/frontend features
- No ORM/LINQ layer
- No multi-db runtime connectors

## Working rules
- Prefer small, reviewable patches; do not refactor unrelated code.
- Keep public CLI stable unless explicitly requested.
- If adding features, also add 1–3 focused unit tests.
- Don’t create extra markdown documentation files unless requested.

## Windows build note
This repo targets `x86_64-pc-windows-msvc` by default.
- If Rust build fails with `link.exe not found`, ensure Visual Studio Build Tools (MSVC C++ workload) are installed.
- Repo may include `.cargo/config.toml` pointing to `link.exe`. Avoid hardcoding tool paths unless necessary; prefer documenting or making it configurable.

## Quality bar
- `cargo test` must pass after changes.
- Error messages should be actionable (include entity/field names when possible).
- Validation should fail fast and clearly on unknown types, duplicate names, or invalid args.

## Next-step options (pick one per iteration)
1) **Migration diffing**: compare two entity versions and emit `ALTER TABLE`.
2) **Typed select**: parse a minimal `select ... from ... where ...` and validate entity/field existence.
3) **Better diagnostics**: line/col spans, nicer parse errors.

## Repo commands
- Tests: `cargo test`
- Run CLI: `cargo run -- <subcommand> <file>`
