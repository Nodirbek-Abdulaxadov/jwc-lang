# JWC fuzzing

Fuzz harness for the JWC language frontend. Two `libFuzzer` targets via
[`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html):

| Target  | Entry point                              | What it tests                         |
|---------|------------------------------------------|---------------------------------------|
| `lex`   | `jwc::lexer::Lexer::new` + `next_token`  | Tokeniser robustness on raw bytes     |
| `parse` | `jwc::parser::parse_program`             | End-to-end frontend (lex + parse)     |

Both targets reject non-UTF-8 input early — the CLI loads source via
`std::fs::read_to_string`, which has the same gate, so fuzzing non-UTF-8
paths would only find unreachable bugs. Any `Err` return from the
lexer/parser is treated as a success outcome; only panics, OOMs, or
timeouts count as findings.

## Why this crate is outside the workspace

The repo's root `Cargo.toml` has no `[workspace]` section. We keep
`fuzz/` standalone (its own `Cargo.toml` with `jwc = { path = ".." }`)
so the main `cargo build` / `cargo test` flow never tries to link
against libFuzzer (which needs the nightly toolchain + the `cargo-fuzz`
helper). Fuzzing is opt-in: explicit `cargo fuzz run` invocations only.

## Running locally

```bash
# one-time tooling install (requires nightly Rust for libFuzzer linkage)
cargo install cargo-fuzz --locked

# quick smoke run (a few seconds) — use this when iterating on the
# harness itself or after touching lexer/parser
cargo fuzz run --manifest-path fuzz/Cargo.toml lex   -- -max_total_time=30
cargo fuzz run --manifest-path fuzz/Cargo.toml parse -- -max_total_time=30

# overnight soak (matches the CI budget, 8h per target)
cargo fuzz run --manifest-path fuzz/Cargo.toml lex   -- -max_total_time=28800
cargo fuzz run --manifest-path fuzz/Cargo.toml parse -- -max_total_time=28800
```

Crashes land under `fuzz/artifacts/<target>/`. Minimise with
`cargo fuzz tmin`, then check in the minimised reproducer alongside a
regression test in `tests/`.

## Corpus

`fuzz/corpus/<target>/` holds seed inputs. We ship a tiny starter set
(empty input, a few keyword / operator / string-literal snippets, one
function/entity/route declaration each). The libFuzzer engine evolves
the corpus on disk as it discovers new coverage edges; do not hand-edit
the evolved files. Add new hand-written seeds whenever a new language
construct lands.

## CI cadence

`.github/workflows/fuzz.yml` runs nightly at 03:00 UTC on
`ubuntu-latest`. Each target gets an 8-hour wall-clock budget
(`-max_total_time=28800`). The workflow uses `continue-on-error: true`
on the fuzz steps so a crash in `lex` does not skip the `parse` run; any
crash artifacts are uploaded as a CI artifact named
`fuzz-artifacts-<run-id>` for triage.
