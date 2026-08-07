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
            // Phase 1.6 — write side is wired too. Each row goes
            // straight into a V::RawJson opaque fragment so
            // jwc_write_json writes the pre-encoded bytes verbatim
            // instead of round-tripping through V::Object's
            // FxHashMap. That's the actual /json-large RPS win.
            ".jwc_write_json(&mut __json_buf)",
            "V::RawJson(JwcStr::from(__json_buf))",
        ],
    },
    Case {
        name: "after_block_emits_separate_fn",
        source: r#"
            middleware Logger {
                let _ = "before";
            } after {
                let _ = "after-side-effect";
            }
            route GET "/x" use Logger {
                return "ok";
            }
            function main() {
                print("ok");
            }
        "#,
        expected_output: "ok\n",
        expected_emit_contains: &[
            // Codegen lowercases the resolved middleware FQN — the
            // emitted Rust fn is `mw_logger`, not `mw_Logger`. Both the
            // before-body fn and the after-body fn appear, and the
            // route dispatcher calls the after-fn after the handler.
            "fn mw_logger() ->",
            "fn mw_logger_after() ->",
            "mw_logger_after().await",
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
        name: "after_block_sees_response_status",
        source: r#"
            middleware Logger {
                let _ = "before";
            } after {
                let _ = response_status();
                let _ = "after-side-effect";
            }
            route GET "/x" use Logger {
                return "ok";
            }
            function main() {
                print("ok");
            }
        "#,
        expected_output: "ok\n",
        // The dispatcher MUST populate Request::response_status before
        // the after-chain runs, so `response_status()` inside an
        // after-block is non-null. The codegen-level proof points:
        //   - jwc_set_response_status is emitted once per route,
        //   - jwc_status_of(&__resp) computes the status from the
        //     handler return value with zero body allocation,
        //   - the after-fn dispatch follows in reverse order (already
        //     pinned by `after_block_emits_separate_fn` above).
        expected_emit_contains: &[
            "jwc_set_response_status(jwc_status_of(&__resp))",
            "mw_logger_after().await",
            "jwc_b_response_status()",
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
    // Stage 2C parity check: an object literal with field reads must
    // produce identical stdout under both the interpreter and the native
    // codegen. Mirrors the `object_literal_field_read` conformance case
    // but additionally verifies that the emitted Rust source contains
    // the field-read shape (`jwc_print(` for each access). Pins the
    // Value::Record migration: regardless of which backing store the
    // construction sites use, the observable output stays identical.
    Case {
        name: "object_literal_via_native_codegen_matches_interpreter",
        source: r#"
            function main() {
                let b = { id: 1, name: "Brand" };
                print(b.id);
                print(b.name);
            }
        "#,
        expected_output: "1\nBrand\n",
        expected_emit_contains: &["jwc_print(", "\"Brand\""],
    },
    // `jwt_sign` / `jwt_verify` parity. The interpreter's stdout is the
    // golden value; the emit assertions pin that codegen routes both
    // calls at the crypto-prelude implementations rather than rejecting
    // them. `jwt_verify` returns the payload JSON, so the round-trip is
    // observable without leaking the (secret-dependent) token itself
    // into the expected output.
    Case {
        name: "jwt_sign_verify_round_trip",
        source: r#"
            function main() {
                let token = jwt_sign({ sub: "user-1" }, "s3cret");
                let claims = json_parse(jwt_verify(token, "s3cret"));
                print(claims.sub);
                print(length(split(token, ".")));
            }
        "#,
        expected_output: "user-1\n3\n",
        expected_emit_contains: &["jwc_b_jwt_sign(", "jwc_b_jwt_verify(", "jwc_print("],
    },
    // The middleware -> handler context hand-off, which is the reason
    // `context` / `setContext` had to become native-capable: this is the
    // shape every JWT-authenticated JWC route uses.
    Case {
        name: "context_round_trip",
        source: r#"
            function main() {
                setContext("userId", "u-42");
                print(context("userId"));
                print(context("missing"));
            }
        "#,
        expected_output: "u-42\nnull\n",
        expected_emit_contains: &["jwc_b_setContext(", "jwc_b_context("],
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

// ─────────────────────────────────────────────────────────────────────────────
// Phase 1 [1.0-blocker] — typed-shape Record migration
// ─────────────────────────────────────────────────────────────────────────────
//
// The native AOT runtime gained `V::Record { field_names, values }` to mirror
// the interpreter's `Value::Record`, and object-literal codegen was rewired to
// emit `v_record(Arc::clone(__jwc_shape_N()), vec![...])` against a shared
// `__jwc_shape_N` constant. Two parity properties matter:
//
//   1. The fast-path emission shape is actually used (no fallback to
//      `JwcObj::default() + .insert()`), AND interpreter stdout still matches.
//   2. Same-shape literals across a function share ONE shape constant — the
//      whole point of the migration (the bench app's /json-large route builds
//      1000 same-shape objects per request).

#[tokio::test]
async fn object_literal_emits_v_record() {
    let source = r#"
        function main() {
            let b = { id: 1, name: "x" };
            print(b.name);
        }
    "#;
    let program = parse_program(source).expect("parse");
    validate_program(&program).expect("validate");

    // Interpreter golden output.
    let result = run_main(&program).await.expect("interpreter run");
    assert_eq!(result.output, "x\n", "interpreter stdout drifted");

    let tmp = tempfile::tempdir().expect("tempdir");
    let out_path = emit_rust_source(&program, tmp.path(), "object_literal_emits_v_record", false)
        .expect("emit_rust_source");
    let body = fs::read_to_string(&out_path).expect("read emitted file");

    // Fast-path construction — the literal MUST emit v_record, not v_obj.
    assert!(
        body.contains("v_record("),
        "emitted source missing v_record() construction — object literal didn't take the typed-shape fast path"
    );
    // And the shape constant getter the literal references must be present.
    assert!(
        body.contains("__jwc_shape_0()"),
        "emitted source missing __jwc_shape_0() getter for the {{id, name}} shape"
    );
    // The literal-construction fallback shape (`let mut __o: JwcObj =
    // JwcObj::default(); __o.insert(...)`) MUST NOT appear — if this ever
    // regresses, the bench /json-large gap reopens silently. Note that
    // `JwcObj::default()` itself is still emitted by validation scaffolding
    // and entity struct→V serializers, so we grep for the literal-site
    // shape specifically (`let mut __o: JwcObj`).
    assert!(
        !body.contains("let mut __o: JwcObj"),
        "emitted source contains `let mut __o: JwcObj` — object literal fell back to FxHashMap construction"
    );
}

#[tokio::test]
async fn object_literal_array_dedupes_shape() {
    // Five same-shape literals inside a loop: codegen must intern the shape
    // ONCE so all five constructions share the same `__jwc_shape_0()` getter,
    // not allocate five distinct field-name Arcs. This is what makes the
    // /json-large hot loop pay one shape alloc instead of N.
    let source = r#"
        function main() {
            let xs = [];
            let i = 0;
            while (i < 5) {
                push(xs, { id: i, name: "row" });
                i = i + 1;
            }
            print(length(xs));
        }
    "#;
    let program = parse_program(source).expect("parse");
    validate_program(&program).expect("validate");

    let result = run_main(&program).await.expect("interpreter run");
    assert_eq!(result.output, "5\n", "interpreter stdout drifted");

    let tmp = tempfile::tempdir().expect("tempdir");
    let out_path = emit_rust_source(
        &program,
        tmp.path(),
        "object_literal_array_dedupes_shape",
        false,
    )
    .expect("emit_rust_source");
    let body = fs::read_to_string(&out_path).expect("read emitted file");

    // Exactly one shape getter for `{id, name}`. The dedup table keys on the
    // declaration-order field list, so all five sites collapse to shape 0.
    let getter_decls = body.matches("fn __jwc_shape_0()").count();
    assert_eq!(
        getter_decls, 1,
        "expected exactly one `fn __jwc_shape_0()` declaration, found {} — shape dedup broke",
        getter_decls
    );
    // No second shape index — the loop only ever builds the one shape, so
    // `__jwc_shape_1` must not be emitted as a declaration anywhere.
    assert!(
        !body.contains("fn __jwc_shape_1()"),
        "emitted source declared __jwc_shape_1() — second shape leaked, dedup broke"
    );
    // Five literal sites referencing the same getter.
    let getter_refs = body.matches("__jwc_shape_0()").count();
    // Decl is one of these; sites referencing it inside `v_record` add the
    // rest. Lower-bound: 1 decl + at least 1 call (the literal). Five-loop
    // emission unrolls into one literal site (the while-body is emitted once
    // and reused per iteration), so we expect >= 2 occurrences (decl + use).
    assert!(
        getter_refs >= 2,
        "expected the __jwc_shape_0() getter to be referenced at least once from a literal site, found only {} occurrence(s) total",
        getter_refs
    );
}
