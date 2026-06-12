//! Sprint 8 follow-up: golden parity harness between the interpreter
//! (`runner::run_main`) and the native AOT codegen (`native_build::emit_rust_source`).
//!
//! For each small in-test JWC program we:
//!   1. parse + validate the source,
//!   2. run it through the async interpreter and capture stdout,
//!   3. emit the equivalent Rust source into a `TempDir`,
//!   4. assert the emitted Rust file is well-formed and contains the
//!      builtin call shapes (e.g. `jwc_print`) that, once compiled, would
//!      produce the same output the interpreter just produced.
//!
//! v1 deliberately stops short of shelling out to `cargo build` on the
//! emitted source. A full compile-and-run parity check (v2) needs a live
//! cargo toolchain at test time and ~30s/program — out of scope here.
//! Instead we treat the interpreter's stdout as the source of truth and
//! verify the codegen *would* emit equivalent calls.

use std::fs;

use jwc::native_build::emit_rust_source;
use jwc::parser::{parse_program, validate_program};
use jwc::runner::run_main;

struct Case {
    name: &'static str,
    source: &'static str,
    /// Exact interpreter stdout we expect (acts as the golden value).
    expected_output: &'static str,
    /// Substrings that MUST appear in the emitted Rust source. These are
    /// the builtin call shapes that make the native binary produce the
    /// same stdout as the interpreter once compiled.
    expected_emit_contains: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        name: "hello",
        source: r#"
            function main() {
                print("hello");
            }
        "#,
        expected_output: "hello\n",
        expected_emit_contains: &["jwc_print(", "\"hello\""],
    },
    Case {
        name: "add",
        source: r#"
            function add(a: int, b: int): int {
                return a + b;
            }
            function main() {
                print(add(2, 3));
            }
        "#,
        expected_output: "5\n",
        expected_emit_contains: &["jwc_print(", "fn add"],
    },
    // ── v0.4.0 builtins ──────────────────────────────────────────────────
    Case {
        name: "array_literal_join",
        source: r#"
            function main() {
                let xs = [1, 2, 3];
                print(join(xs, "-"));
            }
        "#,
        expected_output: "1-2-3\n",
        expected_emit_contains: &["vec!", "jwc_b_join("],
    },
    Case {
        name: "range_push_join",
        source: r#"
            function main() {
                let xs = [];
                for i in range(0, 4) {
                    push(xs, i * i);
                }
                print(join(xs, ","));
            }
        "#,
        expected_output: "0,1,4,9\n",
        expected_emit_contains: &["jwc_b_range(", "jwc_push(", "jwc_b_join("],
    },
    Case {
        name: "sha256",
        source: r#"
            function main() {
                print(sha256("hello"));
            }
        "#,
        expected_output: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\n",
        expected_emit_contains: &["jwc_b_sha256("],
    },
    Case {
        name: "hmac_sha256",
        source: r#"
            function main() {
                print(hmac_sha256("Jefe", "what do ya want for nothing?"));
            }
        "#,
        expected_output: "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843\n",
        expected_emit_contains: &["jwc_b_hmac_sha256("],
    },
    Case {
        name: "substring_basic",
        source: r#"
            function main() {
                print(substring("hello world", 6, 5));
            }
        "#,
        expected_output: "world\n",
        expected_emit_contains: &["jwc_b_substring("],
    },
    Case {
        name: "take_basic",
        source: r#"
            function main() {
                print(take("hello", 3));
            }
        "#,
        expected_output: "hel\n",
        expected_emit_contains: &["jwc_b_take("],
    },
    Case {
        name: "simple_select_uses_typed_read",
        source: r#"
            dbcontext AppDb : Postgres;
            entity Brand of AppDb {
                id bigint pk;
                name text(80);
            }
            route GET "brands" {
                let bs = select Brand from AppDb.Brand;
                return bs;
            }
            function main() {
                print("brands route");
            }
        "#,
        expected_output: "brands route\n",
        // Phase 1 wiring: a SELECT with no projection and no `with`
        // relations now goes through `jwc_db_query_rows` →
        // `JwcEnt_Brand::jwc_from_row` → `jwc_to_v`, skipping the
        // dynamic FxHashMap construction on the read side. The output
        // still flows through V::Object so downstream serialisers stay
        // unchanged; the write-side switchover is the next slice.
        expected_emit_contains: &[
            "jwc_db_query_rows(",
            "JwcEnt_Brand::jwc_from_row(",
            ".jwc_to_v()",
        ],
    },
    Case {
        name: "monomorphized_struct_emitted",
        source: r#"
            dbcontext AppDb : Postgres;
            entity Brand of AppDb {
                id bigint pk;
                name text(80);
                slug text(80) nullable;
                created_at datetime;
            }
            function main() {
                print("brand monomorphized");
            }
        "#,
        expected_output: "brand monomorphized\n",
        // Phase 1 spike: the codegen now emits one concrete Rust struct
        // per entity alongside a `jwc_to_v` serializer. Wiring it onto
        // the hot path is the next slice.
        expected_emit_contains: &[
            "struct JwcEnt_Brand",
            "id: i64,",
            "name: String,",
            "slug: Option<String>,",
            "fn jwc_to_v(&self)",
            // Phase 1.5: row reader + direct JSON writer prove the
            // monomorphized path has everything it needs to bypass
            // V::Object on select / response. Wiring follows next slice.
            "fn jwc_from_row(row: &tokio_postgres::Row)",
            "fn jwc_write_json(&self, out: &mut String)",
        ],
    },
    Case {
        name: "module_const",
        source: r#"
            const PI = 3;
            const TAU = PI * 2;
            function main() {
                print(TAU);
            }
        "#,
        expected_output: "6\n",
        expected_emit_contains: &["fn jwc_const_pi", "fn jwc_const_tau", "jwc_const_tau()"],
    },
];

#[tokio::test]
async fn interpreter_and_native_emit_agree_for_small_programs() {
    for case in CASES {
        // 1. parse + validate
        let program = parse_program(case.source)
            .unwrap_or_else(|e| panic!("[{}] parse failed: {e}", case.name));
        validate_program(&program)
            .unwrap_or_else(|e| panic!("[{}] validate failed: {e}", case.name));

        // 2. run the interpreter — this is the golden output
        let result = run_main(&program)
            .await
            .unwrap_or_else(|e| panic!("[{}] interpreter run failed: {e}", case.name));
        assert_eq!(
            result.output, case.expected_output,
            "[{}] interpreter stdout drifted from golden value",
            case.name
        );

        // 3. emit Rust source into an isolated temp dir
        let tmp =
            tempfile::tempdir().unwrap_or_else(|e| panic!("[{}] tempdir failed: {e}", case.name));
        let out_path = emit_rust_source(&program, tmp.path(), case.name, false)
            .unwrap_or_else(|e| panic!("[{}] emit_rust_source failed: {e}", case.name));

        // 4. well-formed emission
        assert!(
            out_path.exists(),
            "[{}] emit_rust_source returned a path that doesn't exist: {}",
            case.name,
            out_path.display()
        );
        let body = fs::read_to_string(&out_path)
            .unwrap_or_else(|e| panic!("[{}] read emitted file failed: {e}", case.name));
        assert!(
            !body.is_empty(),
            "[{}] emitted Rust source is empty",
            case.name
        );
        assert!(
            body.starts_with("// Generated by jwc build --native. DO NOT EDIT."),
            "[{}] emitted source missing generated-by header; head: {}",
            case.name,
            &body[..body.len().min(120)]
        );
        assert!(
            body.contains("fn main"),
            "[{}] emitted source is missing a `fn main` entry point",
            case.name
        );

        // 5. builtin call-shape parity: every expected substring must appear
        for needle in case.expected_emit_contains {
            assert!(
                body.contains(needle),
                "[{}] emitted Rust source missing expected substring `{}`",
                case.name,
                needle
            );
        }
    }
}
