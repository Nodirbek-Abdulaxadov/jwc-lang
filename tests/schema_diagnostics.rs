//! Schema-level diagnostics (schema.md §11).
//!
//! One case per rule, each asserting the code *and* that the message names
//! the fix — a diagnostic that only says "invalid" makes the reader go
//! looking for the clause.

use jwc::{diag::Severity, model, workspace::Workspace};

/// Parse `src` as a single file, build the model, and return every
/// diagnostic as `(code, message, note)`.
fn diagnose(src: &str) -> Vec<(String, String, String)> {
    let dir = std::env::temp_dir().join(format!(
        "jwc_v1_diag_{}",
        // Deterministic per test body: the hash of the source.
        {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            src.hash(&mut h);
            h.finish()
        }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    std::fs::write(dir.join("a.jwc"), src).expect("write");

    let ws = Workspace::load(&dir).expect("load");
    assert!(
        !ws.has_parse_errors(),
        "fixture must parse:\n{}",
        ws.parse_errors().join("")
    );
    let built = model::build(&ws);
    let out = built
        .diags
        .iter()
        .map(|(_, d)| {
            (
                d.code.to_string(),
                d.message.clone(),
                d.note.clone().unwrap_or_default(),
            )
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn expect(src: &str, code: &str, message_contains: &str) {
    let diags = diagnose(src);
    let hit = diags
        .iter()
        .find(|(c, _, _)| c == code)
        .unwrap_or_else(|| panic!("no {code} for:\n{src}\ngot: {diags:#?}"));
    let haystack = format!("{} {}", hit.1, hit.2);
    assert!(
        haystack.contains(message_contains),
        "{code} should mention `{message_contains}`\n  message: {}\n  note: {}",
        hit.1,
        hit.2
    );
}

fn expect_clean(src: &str) {
    let diags: Vec<_> = diagnose(src)
        .into_iter()
        .filter(|(c, _, _)| c.starts_with('E'))
        .collect();
    assert!(diags.is_empty(), "expected no errors, got {diags:#?}");
}

const PRELUDE: &str = "database App : Postgres;\nschema s of App;\n";

fn with(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

#[test]
fn e0401_identity_on_a_non_integer() {
    expect(
        &with("table T of App.s { id text primary key identity; }"),
        "E0401",
        "smallint, int or bigint",
    );
}

#[test]
fn e0402_non_constant_default() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a int default 1 + 2; }"),
        "E0402",
        "evaluated by Postgres",
    );
}

#[test]
fn e0403_default_now_on_a_non_temporal_column() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a int default now(); }"),
        "E0403",
        "`a`",
    );
}

#[test]
fn e0420_two_primary_keys() {
    expect(
        &with("table T of App.s { id bigint primary key identity; b int; primary key (id, b); }"),
        "E0420",
        "column-level `primary key` or the table-level form",
    );
}

#[test]
fn e0421_foreign_key_column_count_mismatch() {
    expect(
        &with(
            "table P of App.s { id bigint primary key identity; }\n\
             table C of App.s { id bigint primary key identity; a bigint; b bigint;\n\
               foreign key (a, b) references App.s.P (id); }",
        ),
        "E0421",
        "one constraint, not one per column",
    );
}

#[test]
fn e0422_foreign_key_target_is_not_unique() {
    expect(
        &with(
            "table P of App.s { id bigint primary key identity; label text; }\n\
             table C of App.s { id bigint primary key identity; a text;\n\
               foreign key (a) references App.s.P (label); }",
        ),
        "E0422",
        "Postgres requires a foreign key to reference a unique column set",
    );
}

#[test]
fn e0422_names_a_partial_unique_as_insufficient() {
    // A partial unique index is not a unique constraint, so it cannot back
    // a foreign key. Postgres says so at deploy time; this says so now.
    expect(
        &with(
            "table P of App.s { id bigint primary key identity; label text; live boolean;\n\
               unique (label) where live == true : \"m\"; }\n\
             table C of App.s { id bigint primary key identity; a text;\n\
               foreign key (a) references App.s.P (label); }",
        ),
        "E0422",
        "a partial unique index does not qualify",
    );
}

#[test]
fn e0422_unknown_target_table() {
    expect(
        &with(
            "table C of App.s { id bigint primary key identity; a bigint;\n\
               foreign key (a) references App.s.Nope (id); }",
        ),
        "E0422",
        "not a declared table",
    );
}

#[test]
fn e0423_set_null_on_a_not_null_column() {
    expect(
        &with(
            "table P of App.s { id bigint primary key identity; }\n\
             table C of App.s { id bigint primary key identity; a bigint;\n\
               foreign key (a) references App.s.P (id) on delete set null; }",
        ),
        "E0423",
        "NOT NULL",
    );
}

#[test]
fn e0430_on_update_other_than_now() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a timestamptz on update 1; }"),
        "E0430",
        "only `now()`",
    );
}

#[test]
fn e0431_unknown_access_method() {
    expect(
        &with(
            "table T of App.s { id bigint primary key identity; a text; index on (a) using bloom2; }",
        ),
        "E0431",
        "btree, hash, gin, gist, brin, spgist",
    );
}

#[test]
fn e0431_gin_on_a_scalar() {
    // Found by applying the golden files to a real Postgres: `using gin` on
    // a varchar needs gin_trgm_ops, which JWC does not install.
    expect(
        &with(
            "table T of App.s { id bigint primary key identity; a varchar(40); index on (a) using gin; }",
        ),
        "E0431",
        "gin_trgm_ops",
    );
}

#[test]
fn gin_on_jsonb_and_arrays_is_fine() {
    expect_clean(&with(
        "table T of App.s { id bigint primary key identity; a jsonb; b text[];\n\
           index on (a) using gin; index on (b) using gin; }",
    ));
}

#[test]
fn e0450_unknown_schema() {
    expect(
        "database App : Postgres;\ntable T of App.nope { id bigint primary key identity; }",
        "E0450",
        "schema <name> of <Database>",
    );
}

#[test]
fn e0451_index_on_a_column_that_does_not_exist() {
    expect(
        &with("table T of App.s { id bigint primary key identity; index on (nope); }"),
        "E0451",
        "nope",
    );
}

#[test]
fn e0452_required_is_a_class_rule() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a text required; }"),
        "E0452",
        "write `T?` to make it nullable",
    );
}

#[test]
fn e0453_unknown_column_rule() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a text wibble(1); }"),
        "E0453",
        "minLength, maxLength, min, max, pattern, oneOf",
    );
}

#[test]
fn e0301_unknown_column_type() {
    expect(
        &with("table T of App.s { id bigint primary key identity; a Nope; }"),
        "E0301",
        "declared `enum`",
    );
}

#[test]
fn e1203_two_databases() {
    expect(
        "database A : Postgres;\ndatabase B : Postgres;",
        "E1203",
        "one connection per program",
    );
}

#[test]
fn e0110_physical_name_collision() {
    expect(
        &with(
            "table A of App.s as \"same\" { id bigint primary key identity; }\n\
             table B of App.s as \"same\" { id bigint primary key identity; }",
        ),
        "E0110",
        "both map to",
    );
}

#[test]
fn e0111_duplicate_declaration() {
    expect(
        "database App : Postgres;\nschema s of App;\nschema s of App;",
        "E0111",
        "declared more than once",
    );
}

#[test]
fn w0401_table_without_a_primary_key() {
    let diags = diagnose(&with("table T of App.s { a int; }"));
    let hit = diags
        .iter()
        .find(|(c, _, _)| c == "W0401")
        .expect("W0401 for a table with no primary key");
    assert!(hit.1.contains("has no primary key"));
    // A warning, not an error: log tables legitimately have none.
    let errors: Vec<_> = diags
        .iter()
        .filter(|(c, _, _)| c.starts_with('E'))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn the_sample_has_no_schema_errors() {
    let ws = Workspace::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/spec/v1/sample"),
    )
    .expect("load sample");
    let built = model::build(&ws);
    let errors: Vec<String> = built
        .diags
        .iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .map(|(loc, d)| ws.render(*loc, d))
        .collect();
    assert!(errors.is_empty(), "{}", errors.join(""));
}

#[test]
fn editing_a_constraint_message_changes_no_ddl() {
    // schema.md §8.3 — the whole point of deriving names from table +
    // columns + predicate rather than from the message.
    use jwc::ddl;

    let a = with(
        "table T of App.s { id bigint primary key identity; a text;\n\
           unique (a) : \"first message\"; }",
    );
    let b = with(
        "table T of App.s { id bigint primary key identity; a text;\n\
           unique (a) : \"a completely different sentence\"; }",
    );

    let sql = |src: &str| {
        let dir = std::env::temp_dir().join(format!(
            "jwc_v1_msg_{}",
            src.len() + src.as_bytes()[src.len() - 3] as usize
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jwc"), src).unwrap();
        let ws = Workspace::load(&dir).unwrap();
        let built = model::build(&ws);
        let out = ddl::render(&ws, &ddl::emit(&built.model), false);
        let _ = std::fs::remove_dir_all(&dir);
        out
    };

    assert_eq!(sql(&a), sql(&b));
}

#[test]
fn e1202_unknown_init_key() {
    // config.md §2.4 — a typo is silent otherwise: the pool takes its
    // default and the deployment runs with settings nobody chose, which
    // shows up as latency rather than as an error.
    expect(
        "database App : Postgres {\n  init() {\n    pool_size = 20;\n    \
         poool_size = 30;\n  }\n}\nschema s of App;",
        "E1202",
        "poool_size",
    );
}

#[test]
fn a_known_init_key_is_accepted() {
    let diags = diagnose(
        "database App : Postgres {\n  init() {\n    pool_size = 20;\n    \
         statement_timeout = \"10s\";\n    tls = false;\n  }\n}\nschema s of App;",
    );
    assert!(
        !diags.iter().any(|(c, _, _)| c == "E1202"),
        "the documented keys must be accepted: {diags:?}"
    );
}

#[test]
fn e0424_a_function_a_check_may_not_call() {
    // schema.md §4.4 — a check is stored in the database and re-evaluated
    // on every write, so it may only call what is portable enough to live
    // there.
    expect(
        &with(
            "table T of App.s { id bigint primary key identity; a text;\n\
               check (md5(a) != \"\") : \"nope\"; }",
        ),
        "E0424",
        "char_length",
    );
}

#[test]
fn the_canonical_check_functions_are_accepted() {
    let diags = diagnose(&with(
        "table T of App.s { id bigint primary key identity; a text;\n\
         check (char_length(lower(a)) > 2) : \"qisqa\"; }",
    ));
    assert!(
        !diags.iter().any(|(c, _, _)| c == "E0424"),
        "the canonical set must be accepted: {diags:?}"
    );
}

#[test]
fn e1201_io_inside_init() {
    // config.md §2.3 — `init()` runs before any connection is opened, so a
    // query there is circular and I/O is a surprise at boot.
    expect(
        "database App : Postgres {\n  init() {\n    \
         application_name = http.get(\"http://config\");\n  }\n}\nschema s of App;",
        "E1201",
        "before any connection",
    );
}

#[test]
fn env_and_the_coercions_are_allowed_in_init() {
    let diags = diagnose(
        "database App : Postgres {\n  init() {\n    \
         pool_size = int(env(\"DB_POOL\") ?? \"20\");\n    \
         tls = env(\"DB_TLS\") == \"1\";\n  }\n}\nschema s of App;",
    );
    assert!(
        !diags.iter().any(|(c, _, _)| c == "E1201"),
        "`env()` and the coercions are what `init()` is for: {diags:?}"
    );
}

/// A foreign key resolves its target by **declared** name, not by the
/// snake_case of it — schema.md §4.2 with names.md §4.2.
///
/// `as "…"` exists so a program can keep the physical names a database
/// already has, which is exactly what porting an existing service needs.
/// Deriving the target's physical name from the reference instead of from
/// the target made that unusable: a table renamed with `as` could not be
/// pointed at, and the diagnostic named a table the source never wrote.
///
/// Found porting MyWallet, whose four tables are `user`, `wallet`,
/// `category` and `transaction` — singular, and `user` is a reserved word.
#[test]
fn a_foreign_key_finds_a_target_that_renamed_itself() {
    let src = with(
        "table Users of App.s as \"user\" {\n\
         \x20   id bigint primary key identity;\n\
         }\n\
         table Wallets of App.s as \"wallet\" {\n\
         \x20   id bigint primary key identity;\n\
         \x20   user_id bigint;\n\
         \x20   foreign key (user_id) references App.s.Users (id) on delete cascade;\n\
         }\n",
    );
    let diags = diagnose(&src);
    assert!(
        !diags.iter().any(|(c, _, _)| c == "E0422"),
        "the target is declared right there:\n{diags:#?}"
    );

    // And the emitted DDL points at the physical name, quoted because
    // `user` is reserved.
    let dir = std::env::temp_dir().join("jwc_v1_fk_rename");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    std::fs::write(dir.join("a.jwc"), &src).expect("write");
    let ws = Workspace::load(&dir).expect("load");
    let built = model::build(&ws);
    let sql: String = jwc::ddl::emit(&built.model)
        .iter()
        .map(|s| s.sql.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        sql.contains("REFERENCES s.\"user\" (id)"),
        "expected the overridden name in the reference:\n{sql}"
    );
}

/// A target that genuinely is not declared still says so, and says it with
/// the name the source wrote.
#[test]
fn e0422_names_the_undeclared_target() {
    expect(
        &with(
            "table Wallets of App.s {\n\
             \x20   id bigint primary key identity;\n\
             \x20   user_id bigint;\n\
             \x20   foreign key (user_id) references App.s.Users (id);\n\
             }\n",
        ),
        "E0422",
        "not a declared table",
    );
}
