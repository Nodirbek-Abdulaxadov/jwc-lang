# PERF_PLAN — closing the gap to rust-axum

> Source of truth: [`http-framework-benchmark`](https://github.com/just-web-code/http-framework-benchmark)
> measured on `Intel i5-10400 / 32GB / Win11`, bombardier v1.2.6, 15s @ warmup 3s.

## Baseline (jwc v0.4.0 native AOT vs rust-axum 0.8)

| Endpoint | jwc-app (RPS) | rust-axum (RPS) | gap |
|---|---:|---:|---:|
| `/ping` | 123,256 | 143,576 | **1.16×** |
| `/json-small` | 117,729 | 141,247 | **1.20×** |
| `/json-large` | 13,064 | 22,384 | **1.71×** |
| `/cpu` | 68.0 | 190.2 | **2.80×** |
| `/async-delay` | 44,325 | 43,979 | **0.99×** ✓ |

Aggregate: jwc 298k vs rust-axum 351k (1.18×). `/async-delay` already at parity
(tokio scheduler does the heavy lifting); everything else is the cost of the
dynamic value model.

## Root cause (already documented in benchmark README)

The native AOT path emits a uniform tagged value enum:

```rust
// src/native_prelude.rs.in:75
#[derive(Clone, Debug)]
enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),                  // every literal → heap alloc
    Array(Vec<V>),
    Object(BTreeMap<String, V>),  // log-N field access + node splits
}
```

Costs paid on every operation (vs rust-axum's monomorphic `String` / `struct` /
`serde_json::Value`):
1. **Tagged dispatch** — `match V` on `+`, `.field`, `[i]`. No monomorphization.
2. **`BTreeMap` lookup** — O(log n) object access, node alloc on insert.
3. **`#[derive(Clone)]` semantics** — `let x = arr[i]` deep-copies a `Vec<V>` /
   `BTreeMap<String,V>` subtree (interpreter semantics ported verbatim).
4. **`String` everywhere** — no `Cow<'static, str>` / `&'static str`; literals
   re-allocate per request.
5. **`thread_local!` return slot** — `RefCell::borrow_mut()` on every fn call.
6. **`jwc_classify_error`** — `lc.contains()` chain on hot path even when no
   error is in flight.

## Phased plan

### Phase A — cheap wins (1–2 weeks, no architecture change)

| # | Change | File(s) | Expected effect |
|---|---|---|---|
| A1 | `BTreeMap<String, V>` → `FxHashMap<String, V>` | `src/native_prelude.rs.in:83`, all `.insert`/`.get` sites | object access O(log n) → O(1) |
| A2 | Wrap `V::Array`/`V::Object` payloads in `Rc<…>` (or `Arc<…>` for Send) | `src/native_prelude.rs.in:75-84` + codegen | `Clone` becomes refcount bump |
| A3 | Add `V::StaticStr(&'static str)` variant; emit it for source literals | `src/native_prelude.rs.in`, `src/native_build.rs:2213` | per-request literal alloc ↓ |
| A4 | Bumpalo arena scoped per request; drop at response end | request handler wrapper in `native_prelude.rs.in` | allocator churn ↓ |
| A5 | `Cargo.toml` profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`; pass `RUSTFLAGS="-C target-cpu=native"` from `cargo` invocation | generated `Cargo.toml` in `src/native_build.rs` | LLVM inline + dead-code |
| A6 | Move `jwc_classify_error` lc.contains() chain off the hot path (only run on the panic/error branch) | `src/native_prelude.rs.in:141` | branch eliminated on success |
| A7 | Pre-size response `String`/`Vec` with `with_capacity` in `jwc_to_json` and json builtin | `native_prelude.rs.in` | realloc-free serialization |

**Targets:** `/ping` 123k → 150k, `/json-small` 117k → 145k, `/json-large`
13k → 17k. No regressions on tail latency.

### Phase B — selective specialization (3–6 weeks)

| # | Change | Where | Effect |
|---|---|---|---|
| B1 | Entity-typed access — `let u: User = ...` lowers to real `struct User`, never `V::Object` | `src/native_build.rs` codegen, entity-known sites | `/json-large` +30-40% |
| B2 | Integer-only flow peephole — operands proven `V::Int` → emit `i64` directly, skip the V wrap/unwrap dance | new `src/native_specialize.rs` pre-codegen pass | tight loops & `/cpu` |
| B3 | Known-shape object literal `{a:1, b:"x", c:true}` with no dynamic keys → anonymous struct + `#[derive(Serialize)]` | `src/native_build.rs:2200-2230` | json builders allocator ↓ |
| B4 | `json(obj)` builtin — when `obj` is statically typed (entity / known-shape literal), emit `serde_json::to_vec(&obj)` directly instead of traversing the V tree | `runner.rs::call_builtin` + native_build | json hot path |
| B5 | SHA-256 builtin — receive `&[u8]` slice, return `[u8; 32]`; let codegen handle V↔bytes conversion only when crossing the user boundary | `src/native_prelude.rs.in` sha256 wrapper | `/cpu` 68 → 110+ RPS |

**Targets:** `/json-large` 17k → 22k (rust-axum ≈ 22.4k), `/cpu` 68 → 110+ RPS,
no observable change on `/async-delay`.

### Phase C — full type inference + monomorphization (3–6 months)

| # | Change |
|---|---|
| C1 | Flow-based type inference (Crystal-style) — propagate types across `let`, function returns, branch joins |
| C2 | Function monomorphization — multiple call sites with different types → multiple specialized Rust fns |
| C3 | Union types only at branch joins where types genuinely differ; emit concrete `enum Foo { I64(i64), Str(String) }` instead of universal `V` |
| C4 | Keep `V` scoped to `json_parse(body)` boundary and any computed-key object literal — the *only* unavoidably-dynamic surfaces |
| C5 | New `src/typeck.rs` becomes a mandatory pass between `validate_program` and codegen |

**Target:** rust-axum parity on typed code paths (the 80% of real apps).

### Phase D — LLVM IR backend (deferred, ROADMAP Phase 4.1/4.2)

| # | Change |
|---|---|
| D1 | `inkwell` crate — emit LLVM IR directly from typed AST |
| D2 | Skip `cargo build` shell-out — much faster end-to-end `jwc build --native` |
| D3 | Cross-compile linux/macOS/windows without Rust toolchain on the build host |

## Quality gates (mandatory at each phase end)

1. `http-framework-benchmark/.dist/bench-full.ps1` full run, both before & after.
2. **No p99 regression** on any endpoint. Throughput must monotonically improve
   or stay flat; tail latency must not grow.
3. `cargo test` clean + `cargo test --test integration_db` clean (with Docker).
4. `examples/testapp` and `examples/microblog`: `jwc lint` clean, `jwc build
   --native --release` succeeds, e2e suite green.
5. **Per-request allocation counter** via `#[global_allocator] dhat::Alloc` —
   each phase publishes Δ-alloc-count for the five endpoints. PRs that *raise*
   the alloc count without a throughput win are blocked.

## First sprint (concrete, today)

1. **A1**: `BTreeMap` → `FxHashMap` — single commit, ~30 min, mechanical sed +
   add `rustc-hash` to generated `Cargo.toml`.
2. **A2**: `Rc<…>` payloads for `V::Array` / `V::Object` — ~2 h, touches Clone
   semantics, needs codegen review for `&mut V` patterns.
3. **A3**: `V::StaticStr` — ~3-4 h. Codegen emits `V::StaticStr("…")` for literal
   string positions; pattern matches widened to handle both `Str` and `StaticStr`.
4. Bench before/after, capture both runs under `http-framework-benchmark/.dist/results/`.
5. Commit message: `perf: phase A1-A3 — hot value model cleanup`.

## Stretch (Phase A++)

- `serde_json::value::RawValue` for pass-through bodies (no parse-then-reserialize).
- Connection-keepalive tuning: pin hyper to `http1_keepalive_timeout(Duration::from_secs(75))`,
  `http1_max_buf_size(64*1024)` — mirrors rust-axum defaults but jwc's generated
  axum config doesn't override these.
- `mimalloc` global allocator on Windows targets (default jemalloc on Linux).
