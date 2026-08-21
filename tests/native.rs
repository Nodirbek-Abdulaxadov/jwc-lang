//! The native AOT backend — codegen, and the refusal that guards it.
//!
//! Restored in 0.9.901. The v0.25.0 cutover deleted the whole backend, and
//! the roadmap entry that authorised it gave one reason: a second
//! implementation of the query compiler would have to move in lockstep with
//! the first. That reason does not survive the 1.0 front-end — `query_sql`
//! already lowers a query to a SQL string at compile time — but the tier
//! this pass covers is still smaller than the language, so the refusal
//! below matters as much as the emission.
//!
//! Cargo is not invoked here. Building the generated crate takes tens of
//! seconds and needs a Rust toolchain; what this pins is the source that
//! goes into it. The end-to-end check — generate, build, run, and compare
//! against `jwc serve` — is in the release checklist.

use jwc::workspace::Workspace;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn generate(dir: &str) -> String {
    let ws = Workspace::load(repo_root().join(dir)).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    jwc::native::codegen_for_test(&ws).expect("codegen")
}

#[test]
fn a_route_becomes_a_boxed_handler_the_router_can_hold() {
    let rust = generate("tests/native_codegen");

    // `Router` stores `fn() -> Pin<Box<dyn Future>>` — a fn *pointer*. An
    // `async fn` is a distinct fn item with an anonymous future type and
    // cannot coerce to one, so the body lives in its own `async fn` and the
    // registered symbol boxes it.
    assert!(
        rust.contains("async fn jwc_route_get__hello_body() -> V {"),
        "the body should be its own async fn"
    );
    assert!(
        rust.contains("Box::pin(jwc_route_get__hello_body())"),
        "the registered symbol should box the body"
    );
    assert!(rust.contains(r#"router.add("GET", "/hello","#));
}

#[test]
fn the_port_comes_from_the_program_not_the_environment() {
    let rust = generate("tests/native_codegen");

    // config.md §3.2.2 — `serve(port)` in `main` is where a program says
    // where it listens, and the interpreter evaluates `main` at boot for
    // exactly that. Reading `PORT` from the environment here instead would
    // make the two backends disagree about a program that hardcodes it.
    assert!(rust.contains("async fn jwc_user_main() -> V {"));
    assert!(
        rust.contains("JWC_SERVE_PORT.store("),
        "`serve(n)` should record the port"
    );
    assert!(
        !rust.contains("std::env::var(\"PORT\")"),
        "the environment must not override what the program declared"
    );
}

#[test]
fn short_circuiting_operators_are_emitted_inline() {
    let rust = generate("tests/native_codegen");
    // `??` must not evaluate its right side when the left is present, and a
    // call would. Same for `and` / `or`.
    assert!(
        rust.contains("if matches!(__l, V::Null)"),
        "`??` should be emitted inline, not as a call"
    );
}

#[test]
fn a_program_the_pass_cannot_lower_is_refused_by_name() {
    let dir = std::env::temp_dir().join("jwc_native_reject");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        "namespace a;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         table Notes of App.s { id bigint primary key identity; }\n",
    )
    .expect("write");

    let ws = Workspace::load(&dir).expect("load");
    let err = jwc::native::codegen_for_test(&ws).expect_err("a table is outside this pass");
    let msg = err.to_string();
    let _ = std::fs::remove_dir_all(&dir);

    // Named, not a shrug: a native binary that quietly dropped a table
    // would be a far worse outcome than one that will not build.
    assert!(msg.contains("table `Notes`"), "{msg}");
    assert!(
        msg.contains("jwc serve"),
        "the message should say what does work: {msg}"
    );
}
