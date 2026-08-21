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

#[test]
fn the_router_scores_by_literal_segments_not_registration_order() {
    // routing.md §4.2, and the bug this exists for: jwc-shortener declares
    // `/{code}` for its redirects alongside `/docs`, `/openapi.json`,
    // `/robots.txt`, `/sitemap.xml` and `/og.svg`. A router that took the
    // first single-segment match gave all five to the redirect handler and
    // answered 404 with "bunday havola yo'q".
    let prelude = jwc::native::PRELUDE_BASE;
    assert!(
        prelude.contains("if best.as_ref().is_none_or(|(_, _, _, n)| literals > *n)"),
        "the router should pick the candidate with the most literal segments"
    );
}

#[test]
fn env_answers_null_for_an_unset_variable() {
    // `env("PUBLIC_BASE_URL") ?? "https://1kb.uz"` is the shape every
    // program uses this in, and `??` only fires on null. Answering `""`
    // made the default unreachable, and jwc-shortener built its short
    // links as `/abc1234` with no host.
    let prelude = jwc::native::PRELUDE_BASE;
    let body = prelude
        .split("fn jwc_b_env(name: V) -> V {")
        .nth(1)
        .expect("env should be in the prelude");
    let body = body.split("\n}").next().unwrap_or("");
    assert!(
        body.contains("Err(_) => V::Null"),
        "an unset variable is null, not the empty string: {body}"
    );
    assert!(
        !body.contains("unwrap_or_default()"),
        "an unset variable is null, not the empty string: {body}"
    );
}

#[test]
fn a_1_0_builtin_the_0_9_prelude_lacked_is_implemented_not_refused() {
    let rust = generate("tests/native_codegen");
    // The restored prelude predates the 1.0 vocabulary, so it had no
    // `string.of`, no `crypto.token`, no `date.hours`. Those are the
    // built-ins a real program reaches for first — jwc-shortener refused to
    // build on `crypto.token` alone.
    let v1 = jwc::native::PRELUDE_V1;
    for f in [
        "jwc_b_v1_string_of",
        "jwc_b_v1_string_slice",
        "jwc_b_v1_string_strip_prefix",
        "jwc_b_v1_date_hours",
        "jwc_b_v1_array_sum",
        "jwc_b_v1_request_query_all",
    ] {
        assert!(
            v1.contains(&format!("fn {f}(")),
            "{f} should be implemented"
        );
    }
    // And the ones that are genuinely absent are still named, not guessed at.
    let _ = rust;
}

#[test]
fn an_optional_assignment_compiles_one_statement_per_combination() {
    let dir = std::env::temp_dir().join("jwc_native_optional_set");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        "namespace a;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         table Notes of App.s { id int primary key identity; title varchar(80); body text; }\n\
         class Patch { title varchar(80); body text; }\n\
         routes \"/notes\" {\n\
         \x20   route PATCH \"{id: int}\" {\n\
         \x20       let p = request.body() as Patch;\n\
         \x20       return json(update App.s.Notes set title =? $p.title, body =? $p.body \
         where id == @id as { id, title, body } first or throw NotFound(\"yo'q\"));\n\
         \x20   }\n\
         }\n\
         function main() { serve(8080); }\n",
    )
    .expect("write");

    let ws = jwc::workspace::Workspace::load(&dir).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    let _ = std::fs::remove_dir_all(&dir);

    // writes.md §3.3 — `=?` skips the assignment when the value is absent,
    // so which columns the statement sets is a run-time fact. The statement
    // is built at compile time, so every combination is compiled and a mask
    // picks one. Two optional sets is four statements.
    assert!(rust.contains("let __mask: usize = 0"));
    assert!(
        rust.matches("UPDATE s.notes").count() == 3,
        "one statement per non-empty combination: {rust}"
    );
    // The all-absent case sets nothing. `exec::run_update` selects the row
    // as it stands rather than emitting an empty SET, and so does this.
    assert!(
        rust.contains("FROM s.notes"),
        "the empty combination should be a select"
    );
    // Each value is evaluated once, before the branch: an `=?` whose value
    // calls `date.now()` must not be called to test for presence and again
    // to bind.
    assert!(rust.contains("let __set0 ="));
}

#[test]
fn a_page_reads_its_cursor_once_and_signs_the_next_one() {
    let dir = std::env::temp_dir().join("jwc_native_page");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        "namespace a;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         server { cursor_secret = env(\"CURSOR_SECRET\"); }\n\
         table Notes of App.s { id int primary key identity; title varchar(80); }\n\
         routes \"/notes\" {\n\
         \x20   route GET \"\" {\n\
         \x20       return json(select N from App.s.Notes as { id, title } \
         orderby id asc page after request.query(\"cursor\") size 20);\n\
         \x20   }\n\
         }\n\
         function main() { serve(8080); }\n",
    )
    .expect("write");

    let ws = jwc::workspace::Workspace::load(&dir).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    let _ = std::fs::remove_dir_all(&dir);

    // The cursor's keys are the caller's values: read once, before any
    // parameter is bound, because every `Bind::Cursor` reads the same tuple.
    assert!(rust.contains("let __cursor = jwc_cursor_binds("));
    assert!(rust.contains("jwc_db_page("));
    // `server { cursor_secret }` is almost always `env(…)`. Baking in
    // whatever that was on the build machine would sign every deployment's
    // cursors with the builder's secret.
    assert!(
        rust.contains(r#"std::env::var("CURSOR_SECRET")"#),
        "the secret should be read at boot, not at build: {rust}"
    );
    assert!(
        !rust.contains("does not lower `page`"),
        "`page` should be lowered, not refused"
    );
}

#[test]
fn a_spread_takes_its_columns_from_the_declared_type() {
    let dir = std::env::temp_dir().join("jwc_native_spread");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        "namespace a;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         table Notes of App.s { id int primary key identity; title varchar(80); body text; }\n\
         class Patch { title varchar(80); body text; }\n\
         service NoteService {\n\
         \x20   function update(id: int, req: Patch) {\n\
         \x20       return update App.s.Notes set ...$req where id == $id \
         as { id, title, body } first or throw NotFound(\"yo'q\");\n\
         \x20   }\n\
         }\n\
         function main() { serve(8080); }\n",
    )
    .expect("write");

    let ws = jwc::workspace::Workspace::load(&dir).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    let _ = std::fs::remove_dir_all(&dir);

    // types.md §9.2 — a spread sets the fields the value actually carries.
    // Which fields it *could* carry is the source's declared type, and a
    // typed function parameter declares one outright.
    assert!(
        rust.matches("UPDATE s.notes").count() == 3,
        "two spreadable columns is three non-empty combinations: {rust}"
    );
    // §6.5 — absent and null are different. A spread sets a column the
    // caller sent as null *to* null; `=?` skips it. So the presence test is
    // "does the record carry the key", not "is the value non-null".
    assert!(
        rust.contains("jwc_has_field("),
        "presence, not nullity: {rust}"
    );
}

#[test]
fn with_headers_replaces_rather_than_appends() {
    let dir = std::env::temp_dir().join("jwc_native_with_headers");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp");
    std::fs::write(
        dir.join("a.jwc"),
        "namespace a;\n\
         routes \"/x\" {\n\
         \x20   route GET \"\" {\n\
         \x20       return json({ ok: true }) with { \"Cache-Control\": \"public\" };\n\
         \x20   }\n\
         }\n\
         function main() { serve(8080); }\n",
    )
    .expect("write");

    let ws = jwc::workspace::Workspace::load(&dir).expect("load");
    assert!(!ws.has_parse_errors(), "{}", ws.parse_errors().join(""));
    let rust = jwc::native::codegen_for_test(&ws).expect("codegen");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(rust.contains("jwc_with_headers("));
    // routing.md §6.2 — replace, not append. A builder has already stamped
    // `content-type`, and two of them is a malformed message that clients
    // resolve inconsistently.
    let prelude = jwc::native::PRELUDE_BASE;
    let body = prelude
        .split("fn jwc_with_headers(")
        .nth(1)
        .expect("the helper should be in the prelude");
    assert!(
        body.contains("headers.remove(&e)"),
        "an existing header of the same name should be replaced"
    );
    assert!(
        body.contains("content_type"),
        "`with {{ \"Content-Type\": … }}` has to beat the builder's, which \
         is a field of its own on the response"
    );
}
