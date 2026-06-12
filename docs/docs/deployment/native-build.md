---
sidebar_position: 1
---

# Native build

Two flavours.

## Bundled launcher (default)

```bash
jwc build              # → bin/debug/<app>[.exe]
jwc build --release    # → bin/release/<app>[.exe]
```

Copies the `jwc` runtime + a thin launcher into the project's `bin/` dir. The launcher self-resolves the project root and runs the interpreter. Cheap (~50 ms to build), works on any machine that has the `jwc` binary nearby.

**Use when:** you want a one-file deploy that runs the project — no toolchain on the target host. Caveat: the runtime is the size of the full `jwc` binary (~15 MB).

## AOT (real native binary)

```bash
jwc build --native --release
```

Generates Rust source from the JWC AST → invokes `cargo build --release` → emits a stripped binary under `bin/release/<app>[.exe]`.

Sample binary sizes from `examples/`:
- `hellocompile` (hello world) — **1.1 MB**
- `async_demo` (with `reqwest` + rustls) — **2.9 MB**

**Use when:** binary size, startup time, or CPU-bound throughput matter. Needs a Rust toolchain on `PATH` (`rustup`).

## Inspecting the generated source

```bash
jwc build --native --emit-rust-source
# → bin/<profile>/<app>.generated.rs
```

Skips cargo entirely. Useful for debugging codegen / opening an issue.

## Cross-compile

```bash
jwc build --native --target x86_64-unknown-linux-musl --release
```

Output goes to `bin/<target>/<profile>/<app>`. Supported triples (v1 allowlist):

- `x86_64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

The host toolchain must have the target installed (`rustup target add <triple>`).

## What doesn't compile yet (native AOT)

The native pipeline is partial. These constructs are interpreter-only today (clear error at build time):

- Class methods on user-defined `class` (DTOs work; methods don't)
- `cache_*` family
- Some queue primitives

As of v0.4.0, `hash_password` / `verify_password` are accepted by `jwc build --native` (previously interpreter-only). Graceful shutdown — `serve(port)` draining in-flight requests on Ctrl+C — also works in native builds.

Roadmap [Phase 4 + Sprint 13](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md) tracks the closing list.

## Why the AOT binary is fast: monomorphization

The Production Readiness Plan's Phase 1 closes the `/json-large` benchmark gap (13.0k → axum/dotnet tier) by **monomorphizing entity shapes**: each `entity Name { ... }` declared in your program becomes a concrete Rust struct in the generated source, not a dynamic hashmap.

Two wins compound on the hot path:

- `struct JwcEnt_<Entity>` per entity — fields with their declared Postgres types (`i64`, `String`, `Option<String>`, `chrono::DateTime<Utc>`, …). A `select Brand from AppDb.Brand` reads straight into these typed slots via `JwcEnt_Brand::jwc_from_row(&Row)`, skipping the dynamic `FxHashMap<String, V>` construction that the interpreter and earlier AOT paths used.

- `V::RawJson` opaque fragments — once a row is monomorphized, `JwcEnt_Brand::jwc_write_json(&mut String)` emits the JSON bytes directly into the response buffer. The result is wrapped as `V::RawJson(JwcStr::from(buf))` so the existing JSON serializer writes those pre-encoded bytes verbatim instead of round-tripping through `V::Object`. That's the concrete RPS win on `/json-large`.

You don't opt into this — every `entity` declaration triggers it automatically when you `jwc build --native`. To inspect the emitted struct + serializer for an entity in your program, run `jwc build --native --emit-rust-source` and grep for `struct JwcEnt_` and `jwc_write_json` in the resulting `.generated.rs` file.

Dynamic payloads (`json_parse` of arbitrary input, object literals with computed keys) still use `V::Object` — the monomorphized path is a fast lane, not a replacement. The compiler picks per expression.
