//! The native AOT backend — codegen, and the refusal that guards it.
//!
//! Restored in 0.9.901. The v0.25.0 cutover deleted the whole backend, and
//! the roadmap entry that authorised it gave one reason: a second
//! implementation of the query compiler would have to move in lockstep with
//! the first. That reason does not survive the 1.0 front-end — `query_sql`
//! already lowers a query to a SQL string at compile time, and this pass
//! calls it rather than reimplementing it.
//!
//! Cargo is not invoked here. Building the generated crate takes tens of
//! seconds and needs a Rust toolchain; what this pins is the source that
//! goes into it. The end-to-end check — generate, build, run, and diff every
//! response against `jwc serve` — is in the release checklist.

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
        rust.contains("async fn jwc_route_get__hello_body() -> JwcResult {"),
        "the body should be its own async fn"
    );
    assert!(
        rust.contains("Box::pin(jwc_route_get__hello_dispatch())"),
        "the registered symbol should box the dispatcher"
    );
    assert!(rust.contains(r#"router.add("GET", "/hello","#));
}

#[test]
fn the_route_is_registered_with_its_parameter_types() {
    let rust = generate("tests/native_codegen");

    // routing.md §3.2 — a path parameter is parsed to its declared type
    // *before* any middleware, and a segment that does not parse is a 400
    // there. The router can only do that if the type reached it.
    assert!(
        rust.contains(r#"router.add("GET", "/notes/{id: bigint}","#),
        "the declared type should travel with the pattern"
    );
}

#[test]
fn the_port_comes_from_the_program_not_the_environment() {
    let rust = generate("tests/native_codegen");

    // config.md §3.2.2 — `serve(port)` in `main` is where a program says
    // where it listens, and the interpreter evaluates `main` at boot for
    // exactly that. Reading `PORT` from the environment here instead would
    // make the two backends disagree about a program that hardcodes it.
    assert!(rust.contains("async fn jwc_user_main() -> JwcResult {"));
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
fn a_query_is_lowered_by_the_one_query_compiler() {
    let rust = generate("tests/native_codegen");

    // The SQL text is embedded, not built at run time — it is the string
    // `query_sql` produced, which is the same string `jwc serve` sends. A
    // second query compiler here is the thing this backend must never grow.
    assert!(
        rust.contains("FROM s.notes t0"),
        "the select should carry its SQL: {rust}"
    );
    assert!(
        rust.contains("INSERT INTO s.notes"),
        "the insert should carry its SQL"
    );
    assert!(
        rust.contains("UPDATE s.notes"),
        "the update should carry its SQL"
    );
    assert!(
        rust.contains("DELETE FROM s.notes"),
        "the delete should carry its SQL"
    );
    // Projection order is a promise of the response, and a parsed JSON
    // object is a hash map, so the order is emitted alongside the statement.
    assert!(rust.contains(r#"const JWC_FIELDS_0: &[&str] = &["id", "title"];"#));
}

#[test]
fn an_insert_binds_the_values_the_writer_computed() {
    let rust = generate("tests/native_codegen");

    // The builder marks every INSERT parameter `Bind::Expr` over a
    // placeholder and `exec::run_insert` supplies the values positionally,
    // bypassing `bind_params`. Re-deriving them from the placeholder binds
    // `null` for every column — which Postgres reports as a not-null
    // violation on a column the program clearly set.
    let stmt = rust
        .split("INSERT INTO s.notes")
        .nth(1)
        .expect("an insert should be emitted");
    let binds = stmt.split(".await").next().unwrap_or("");
    assert!(
        binds.contains("jwc_get_field"),
        "the bound value should be the writer's expression, not a placeholder: {binds}"
    );
}

#[test]
fn a_middleware_chain_runs_before_the_handler_and_after_it_in_reverse() {
    let rust = generate("tests/native_codegen");

    // middleware.md §4.2 — `None` is a fall-through, `Some(r)` short-circuits.
    assert!(rust.contains("async fn jwc_mw_Guard() -> Result<Option<V>, JwcThrown> {"));
    // §4.3 — every middleware that *started* runs its `after` block,
    // including the one that short-circuited, so the dispatcher counts them.
    assert!(rust.contains("started += 1;"));
    assert!(rust.contains("jwc_mw_Guard_after().await"));
    // §5.1 — `response.status()` inside an after block sees the status
    // actually being sent.
    assert!(rust.contains("jwc_set_response_status(jwc_status_of(&response));"));
}

#[test]
fn a_transaction_commits_on_a_return_and_rolls_back_on_a_throw() {
    let rust = generate("tests/native_codegen");

    // writes.md §5 — the connection is pinned for the block, or the BEGIN
    // lands on one pooled connection and the statements on others.
    assert!(rust.contains("jwc_tx_begin().await?"));
    assert!(rust.contains("JWC_TX_CONN"));
    // `Flow::Return` is `Ok` in the interpreter, so a `return` inside the
    // block commits — the rollback is for the error path only.
    assert!(rust.contains(".is_ok()).await;"));
}

#[test]
fn a_declared_errors_status_is_resolved_at_compile_time() {
    let rust = generate("tests/native_codegen");

    // errors.md §4.3 — the status comes from the declaration, so the binary
    // needs no name → status map at run time.
    assert!(
        rust.contains(r#"JwcThrown::new("Unauthorized", 401,"#),
        "the throw should carry the declared status"
    );
    assert!(rust.contains(r#"JwcThrown::new("NotFound", 404,"#));
}

#[test]
fn a_class_reaches_the_binary_with_its_rules_intact() {
    let rust = generate("tests/native_codegen");

    // A rule the checker accepted has to be a rule the binary enforces. The
    // table is emitted from the same `ClassSym`s, so there is no second
    // description of a class to drift.
    assert!(rust.contains("static JWC_CLASSES: &[(&str, &[JwcClassField])] = &["));
    assert!(rust.contains(r#"name: "required", limit: None"#));
    assert!(rust.contains(r#"name: "minLength", limit: Some(2)"#));
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
         table Notes of App.s { id bigint primary key identity; title varchar(200); }\n\
         view Titles of App.s {\n\
         \x20   select N from App.s.Notes as { id, title }\n\
         }\n",
    )
    .expect("write");

    let ws = Workspace::load(&dir).expect("load");
    let err = jwc::native::codegen_for_test(&ws).expect_err("a view is outside this pass");
    let msg = err.to_string();
    let _ = std::fs::remove_dir_all(&dir);

    // Named, not a shrug: a native binary that quietly dropped a view would
    // be a far worse outcome than one that will not build.
    assert!(msg.contains("view `Titles`"), "{msg}");
    assert!(
        msg.contains("jwc serve"),
        "the message should say what does work: {msg}"
    );
}
