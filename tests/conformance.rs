//! Phase 0 conformance suite (PRODUCTION_READINESS_PLAN, "Specification &
//! compatibility contract").
//!
//! `tests/conformance/cases/` holds `case_<name>.jwc` programs each paired
//! with a sibling `case_<name>.stdout.txt` containing the exact stdout the
//! interpreter must produce. This directory is the single source of truth
//! for "what JWC means", run against BOTH execution strategies:
//!
//!   1. Interpreter — `runner::run_main`, stdout compared byte-for-byte
//!      against the `.stdout.txt` file (after CRLF→LF normalization on the
//!      expected side, since git may check fixtures out with Windows line
//!      endings on Windows hosts).
//!   2. Native AOT — `native_build::emit_rust_source` into a `TempDir`,
//!      asserting a well-formed, non-empty emission with the generated-by
//!      header and a `fn main` entry point. Like `tests/native_parity.rs`,
//!      we deliberately do NOT shell out to `cargo build` on the emitted
//!      source — that adds ~30s/case and a hard cargo dependency for no
//!      additional semantic coverage beyond what the interpreter already
//!      provides. Full compile-and-run parity is a later phase.
//!
//! ## Per-case tests
//!
//! Each `case_<name>` gets a dedicated `#[tokio::test]` function in the
//! `cases` module, so:
//!
//! ```text
//! cargo test --test conformance case_arithmetic
//! ```
//!
//! runs only the arithmetic case. The `discovery` test fails if a `.jwc`
//! file under `cases/` is not wired up to a per-case test below, keeping
//! the registry honest as the suite grows.
//!
//! ## Opt-out
//!
//! A program whose FIRST line is exactly
//! `// CONFORMANCE: interpreter-only` (case-insensitive on the marker word)
//! skips the native-emission step — for programs exercising a language
//! feature the AOT codegen doesn't accept yet.
//!
//! ## Skip behaviour (mirrors `tests/integration_db.rs`)
//!
//! The harness NEVER panics on environmental failures (missing case file,
//! unreadable directory). It emits `eprintln!("SKIPPED ...")` and returns
//! `Ok(())` for environment-shaped failures, and only fails the test on a
//! genuine semantic mismatch.

use std::fs;
use std::path::{Path, PathBuf};

use jwc::native_build::emit_rust_source;
use jwc::parser::{parse_program, validate_program};
use jwc::runner::run_main;

/// First-line marker that skips the native-emission step for one program.
/// Case-insensitive on the prefix so authors don't have to memorize the
/// exact casing.
const INTERPRETER_ONLY_MARKER: &str = "// conformance: interpreter-only";

/// First-line marker that skips the case entirely. Used for tests that
/// document the *contract* of an in-flight feature (typed-catch dotted
/// subtypes, no-DB DbError classification) before the runtime/parser
/// surface needed to drive the test actually exists. The case stays in
/// the registry (so it cannot drift out of the suite) and the .jwc /
/// .stdout pair stays on disk (so reviewers see the intended shape) —
/// the harness just `eprintln!`s a SKIPPED line and returns `Ok(())`
/// the same way it does for environment-shaped failures.
///
/// Once the blocking feature lands, drop the marker line and the case
/// becomes a normal active test. The skip is intentionally lightweight
/// (no separate registry, no compile-time gate) so removing the marker
/// is the ONLY edit required to re-enable the case.
const DEFERRED_MARKER: &str = "// conformance: deferred";

fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("conformance")
        .join("cases")
}

/// Normalize Windows line endings on the expected side: git may check the
/// `.stdout.txt` fixtures out with CRLF, while the interpreter always emits
/// `\n` — without this every Windows checkout would fail every case.
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Returns `Ok(())` on a true semantic pass, `Err(msg)` on a real
/// mismatch, or `Ok(())` *plus* an `eprintln!("SKIPPED ...")` for an
/// environment-shaped failure (missing files, IO error). Mirrors the
/// integration_db.rs convention: never panic for environmental reasons.
async fn run_case(name: &str) -> Result<(), String> {
    let dir = cases_dir();
    let jwc_path = dir.join(format!("{name}.jwc"));
    let expected_path = dir.join(format!("{name}.stdout.txt"));

    let source = match fs::read_to_string(&jwc_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SKIPPED [{name}]: cannot read {}: {e}", jwc_path.display());
            return Ok(());
        }
    };
    let expected = match fs::read_to_string(&expected_path) {
        Ok(s) => normalize_newlines(&s),
        Err(e) => {
            eprintln!(
                "SKIPPED [{name}]: cannot read {}: {e}",
                expected_path.display()
            );
            return Ok(());
        }
    };

    // Deferred cases skip BEFORE parse — the source itself may exercise
    // syntax that's still in flight (e.g. dotted catch types). Detecting
    // the marker on the raw source line keeps the skip path independent
    // of the parser's current shape.
    let deferred = source
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|line| line.eq_ignore_ascii_case(DEFERRED_MARKER));
    if deferred {
        eprintln!(
            "SKIPPED [{name}]: deferred (see header comment in {}). \
             Drop the `// CONFORMANCE: deferred` line to re-enable.",
            jwc_path.display()
        );
        return Ok(());
    }

    // 1. parse + validate
    let program = parse_program(&source).map_err(|e| format!("parse failed: {e}"))?;
    validate_program(&program).map_err(|e| format!("validate failed: {e}"))?;

    // 2. interpreter run — exact stdout match against the golden file
    let result = run_main(&program)
        .await
        .map_err(|e| format!("interpreter run failed: {e}"))?;
    if result.output != expected {
        return Err(format!(
            "interpreter stdout mismatch\n--- expected ({}) ---\n{}--- actual ---\n{}",
            expected_path.display(),
            display_for_diff(&expected),
            display_for_diff(&result.output),
        ));
    }

    // 3. native AOT emission (skipped for interpreter-only programs).
    let interpreter_only = source
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|line| line.eq_ignore_ascii_case(INTERPRETER_ONLY_MARKER));
    if interpreter_only {
        eprintln!("[{name}] interpreter-only (native AOT step skipped by marker)");
        return Ok(());
    }

    // tempfile is already a dev-dependency (Cargo.toml); using std::env::temp_dir
    // would force us to invent our own cleanup. Reuse the existing crate.
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => {
            // No tempdir → no cargo toolchain expected to be reachable; skip
            // rather than fail.
            eprintln!("SKIPPED [{name}] (native AOT): tempdir creation failed: {e}");
            return Ok(());
        }
    };
    let out_path = emit_rust_source(&program, tmp.path(), name, false)
        .map_err(|e| format!("emit_rust_source failed: {e}"))?;
    if !out_path.exists() {
        return Err(format!(
            "emit_rust_source returned a path that doesn't exist: {}",
            out_path.display()
        ));
    }
    let body = fs::read_to_string(&out_path).map_err(|e| format!("read emitted file: {e}"))?;
    if body.is_empty() {
        return Err("emitted Rust source is empty".to_string());
    }
    if !body.starts_with("// Generated by jwc build --native. DO NOT EDIT.") {
        return Err(format!(
            "emitted source missing generated-by header; head: {}",
            &body[..body.len().min(120)]
        ));
    }
    if !body.contains("fn main") {
        return Err("emitted source is missing a `fn main` entry point".to_string());
    }

    Ok(())
}

/// Render a string for the diff snippet so trailing newlines / invisible
/// characters become visible at a glance.
fn display_for_diff(s: &str) -> String {
    if s.is_empty() {
        "<empty>\n".to_string()
    } else if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n<no trailing newline>\n")
    }
}

/// Cases registered with a per-test function below. Adding a new case:
///   1. drop `case_<name>.jwc` + `case_<name>.stdout.txt` into `cases/`,
///   2. add the name to this slice,
///   3. add a `#[tokio::test] async fn case_<name>() { ... }` shim below.
///
/// The discovery test cross-checks this list against the directory so
/// step 2 cannot be forgotten silently.
const REGISTERED_CASES: &[&str] = &[
    "case_arithmetic",
    "case_array_helpers",
    "case_arrays",
    "case_bool_short_circuit",
    "case_catch_falls_through_when_type_mismatches",
    "case_console_write_bypasses_print_buffer",
    "case_const",
    "case_equality",
    "case_file_copy_move",
    "case_file_io_round_trip",
    "case_for_loop",
    "case_format_float",
    "case_functions",
    "case_hash",
    "case_if_else",
    "case_int_overflow",
    "case_json",
    "case_json_helpers",
    "case_literals",
    "case_misc_helpers",
    "case_nested_object_literal",
    "case_now_smoke",
    "case_object_array_loop",
    "case_object_literal_field_read",
    "case_object_literal_field_write",
    "case_object_literal_json_round_trip",
    "case_private_helper_called_locally",
    "case_raw_string",
    "case_strings",
    "case_substring",
    "case_try_catch",
    "case_typed_catch_db_error",
    "case_typed_catch_io_error",
    "case_typed_catch_specific_subtype",
    "case_typed_catch_super_kind",
    "case_unary_not",
    "case_unix_timestamp",
    "case_while",
];

// ── Per-case tests ───────────────────────────────────────────────────────
// One #[tokio::test] per .jwc fixture so `cargo test case_arithmetic`
// runs exactly that case and nothing else. Each shim is a thin wrapper
// around `run_case(name)` — the real logic lives in `run_case`.

mod cases {
    use super::run_case;

    macro_rules! conformance_test {
        ($name:ident) => {
            // Each case runs on a dedicated 8 MiB-stack thread with its
            // own tokio current_thread runtime. The default tokio test
            // thread is ~2 MiB and the tree-walking interpreter's
            // async_recursion frames eat that fast on recursive cases
            // (`fib(10)` in case_functions). Isolating per-thread also
            // means concurrent #[test] cases can't poison each other's
            // env vars or panic states.
            #[test]
            fn $name() {
                let join = std::thread::Builder::new()
                    .name(format!("conformance-{}", stringify!($name)))
                    .stack_size(8 * 1024 * 1024)
                    .spawn(|| {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect(
                                "INVARIANT: tokio current_thread runtime builds with no features",
                            );
                        rt.block_on(run_case(stringify!($name)))
                    })
                    .expect("INVARIANT: OS allows a single thread spawn");
                let result = join.join().expect("INVARIANT: conformance thread joined");
                if let Err(msg) = result {
                    panic!("[{}] {}", stringify!($name), msg);
                }
            }
        };
    }

    conformance_test!(case_arithmetic);
    conformance_test!(case_array_helpers);
    conformance_test!(case_arrays);
    conformance_test!(case_bool_short_circuit);
    conformance_test!(case_catch_falls_through_when_type_mismatches);
    conformance_test!(case_console_write_bypasses_print_buffer);
    conformance_test!(case_const);
    conformance_test!(case_equality);
    conformance_test!(case_format_float);
    conformance_test!(case_file_copy_move);
    conformance_test!(case_file_io_round_trip);
    conformance_test!(case_for_loop);
    conformance_test!(case_functions);
    conformance_test!(case_hash);
    conformance_test!(case_if_else);
    conformance_test!(case_int_overflow);
    conformance_test!(case_json);
    conformance_test!(case_json_helpers);
    conformance_test!(case_literals);
    conformance_test!(case_misc_helpers);
    conformance_test!(case_nested_object_literal);
    conformance_test!(case_now_smoke);
    conformance_test!(case_object_array_loop);
    conformance_test!(case_object_literal_field_read);
    conformance_test!(case_object_literal_field_write);
    conformance_test!(case_object_literal_json_round_trip);
    conformance_test!(case_private_helper_called_locally);
    conformance_test!(case_raw_string);
    conformance_test!(case_strings);
    conformance_test!(case_substring);
    conformance_test!(case_try_catch);
    conformance_test!(case_typed_catch_db_error);
    conformance_test!(case_typed_catch_io_error);
    conformance_test!(case_typed_catch_specific_subtype);
    conformance_test!(case_typed_catch_super_kind);
    conformance_test!(case_unary_not);
    conformance_test!(case_unix_timestamp);
    conformance_test!(case_while);
}

/// Cross-check the on-disk case set against `REGISTERED_CASES` so adding a
/// `.jwc` fixture without wiring it up to a `#[tokio::test]` is caught
/// loudly. Without this, a forgotten registration would let a case sit
/// unexecuted forever, silently shrinking coverage.
#[test]
fn discovery_every_case_file_is_registered() {
    let dir = cases_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("SKIPPED discovery: cannot read {}: {e}", dir.display());
            return;
        }
    };

    let mut on_disk: Vec<String> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "jwc"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .filter(|n| n.starts_with("case_"))
        .collect();
    on_disk.sort();

    let mut registered: Vec<String> = REGISTERED_CASES.iter().map(|s| (*s).to_string()).collect();
    registered.sort();

    let missing_from_registry: Vec<&String> =
        on_disk.iter().filter(|n| !registered.contains(n)).collect();
    let missing_from_disk: Vec<&String> =
        registered.iter().filter(|n| !on_disk.contains(n)).collect();

    if !missing_from_registry.is_empty() || !missing_from_disk.is_empty() {
        panic!(
            "conformance registry drift:\n  on-disk but not registered: {:?}\n  registered but not on disk: {:?}",
            missing_from_registry, missing_from_disk
        );
    }

    // Belt-and-braces: also confirm each registered case has a sibling
    // .stdout.txt — a .jwc with no expected file is a silent half-case.
    let mut orphaned: Vec<String> = Vec::new();
    for name in &registered {
        let expected = dir.join(format!("{name}.stdout.txt"));
        if !expected.exists() {
            orphaned.push(name.clone());
        }
    }
    if !orphaned.is_empty() {
        panic!(
            "conformance cases missing sibling .stdout.txt: {:?}",
            orphaned
        );
    }
}
