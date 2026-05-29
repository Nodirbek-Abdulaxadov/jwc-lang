//! v0.4.0 array builtins, exercised end-to-end through the interpreter
//! (parse → validate → run_main). Native parity for the same shapes is
//! covered by `tests/native_parity.rs`.

use jwc::parser::{parse_program, validate_program};
use jwc::runner::run_main;

async fn run(src: &str) -> String {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    validate_program(&program).unwrap_or_else(|e| panic!("validate failed: {e}"));
    run_main(&program)
        .await
        .unwrap_or_else(|e| panic!("run failed: {e}"))
        .output
}

#[tokio::test]
async fn array_literal_and_iteration() {
    let out = run(r#"
        function main() {
            let xs = [1, 2, 3];
            let acc = 0;
            for x in xs { acc = acc + x; }
            print(acc);
            print(xs);
        }
    "#)
    .await;
    assert_eq!(out, "6\n[1,2,3]\n");
}

#[tokio::test]
async fn empty_and_heterogeneous_arrays() {
    let out = run(r#"
        function main() {
            print([]);
            print([1, "two", true]);
        }
    "#)
    .await;
    assert_eq!(out, "[]\n[1,\"two\",true]\n");
}

#[tokio::test]
async fn range_variants() {
    let out = run(r#"
        function main() {
            print(join(range(4), ","));
            print(join(range(2, 5), ","));
            print(join(range(0, 10, 3), ","));
        }
    "#)
    .await;
    assert_eq!(out, "0,1,2,3\n2,3,4\n0,3,6,9\n");
}

#[tokio::test]
async fn push_append_and_join() {
    let out = run(r#"
        function main() {
            let xs = [];
            for i in range(0, 5) { push(xs, i * i); }
            append(xs, 100);
            print(join(xs, "-"));
            print(length(xs));
        }
    "#)
    .await;
    assert_eq!(out, "0-1-4-9-16-100\n6\n");
}
