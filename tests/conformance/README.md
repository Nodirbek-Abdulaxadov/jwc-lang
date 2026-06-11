# JWC Conformance Suite

Phase 0 of `next/PRODUCTION_READINESS_PLAN.md` (Specification & compatibility
contract). The cases under `cases/` are the source of truth for "what JWC
means" — every construct in the spec must be covered here, and every case is
exercised against BOTH execution strategies (interpreter and native AOT)
unless explicitly opted out.

## Shape

```
tests/conformance/
├── README.md         ← you are here
├── cases/
│   ├── case_<name>.jwc           ← JWC program; must define `function main()`
│   └── case_<name>.stdout.txt    ← exact expected interpreter stdout
└── (legacy flat cases from the v0 layout may also live here — they're not
   discovered by the v1 harness)
```

The harness lives at `tests/conformance.rs` and generates one `#[tokio::test]`
function per case (`cargo test --test conformance case_arithmetic` runs just
the arithmetic case, etc.).

## Matching rule

`.stdout.txt` is compared **byte-for-byte** against the interpreter's
captured stdout, after one normalization pass: CRLF (`\r\n`) is converted
to LF (`\n`) on the expected file so this suite is robust to a Windows
git checkout. The interpreter emits LF only; we do not normalize the
actual side, so a stray CR in interpreter output WILL fail the case.

## How to add a case

1. Pick a feature with no dedicated case yet — see the "Coverage" map at the
   bottom of `next/PRODUCTION_READINESS_PLAN.md` Phase 0.
2. Drop two files into `cases/`:
   - `case_<feature>.jwc` — small, deterministic, DB-free, no HTTP, no env
     reads, no `now()` / `uuid()` values asserted on (use a boolean
     `length(now()) > 10` shim if you need to exercise them).
   - `case_<feature>.stdout.txt` — exact stdout the interpreter must produce.
3. Run `cargo test --test conformance case_<feature> -- --nocapture`. On
   failure the harness prints both the expected and actual stdout for diff.

The harness discovers files at runtime — no registration needed.

## Native AOT opt-out

If a case uses a feature the native AOT codegen (`src/native_build.rs`) does
not yet support (db queries, typed `catch`, `await`, middleware bodies), put
the marker on the **first line** of the `.jwc` file:

```
// CONFORMANCE: interpreter-only
function main() { ... }
```

The harness will still run the interpreter half, but skip native emission for
that case. The target is to keep this list short and shrinking — every
opt-out is a documented native-codegen gap that Phase 7 of the readiness
plan tracks.

## What the harness does NOT do

- It does NOT shell out to `cargo build` on the emitted native source
  (~30s/case is too slow for routine CI). Instead it asserts the emitted
  Rust source compiles to a well-formed file with `fn main` and the
  generated-by header. Full compile-and-run parity is a deferred phase
  (mirrors `tests/native_parity.rs`).
- It does NOT run the HTTP server, hit the database, or touch the network.
  All cases must produce their golden stdout from `function main()` alone.
- It does NOT panic when the native AOT step trips on something — it
  collects the error per case and reports the file path so failures don't
  cascade across the suite.

## Why this is the foundation

Phase 0 of the production readiness plan gates v1.0. The spec is being
extracted from the parser; this directory is what tells us when a spec
sentence drifted from the implementation. If you change language semantics,
a conformance case MUST be updated in the same change — otherwise the
"spec" silently lies.
