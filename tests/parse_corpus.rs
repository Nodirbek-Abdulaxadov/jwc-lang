//! v0.21.0 acceptance: the front-end reads the specification's sample with
//! zero errors, and the corpus exercises every grammar production.
//!
//! ROADMAP's done-criterion for v0.21.0 is
//!   "`jwc check --parse-only` reads the sample's files with 0 errors;
//!    `tests/parse_corpus/` covers every production of the grammar."

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sample_files() -> Vec<PathBuf> {
    let root = repo_root().join("docs/spec/v1/sample");
    let mut out = Vec::new();
    collect(&root, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("jwc") {
            out.push(p);
        }
    }
}

#[test]
fn sample_parses_with_zero_errors() {
    let files = sample_files();
    assert!(!files.is_empty(), "no sample .jwc files found");

    let mut failures = String::new();
    for f in &files {
        let parsed = jwc::parse_file(f).expect("read sample file");
        if parsed.has_errors() {
            failures.push_str(&format!("\n=== {} ===\n{}", f.display(), parsed.render_all()));
        }
    }
    assert!(
        failures.is_empty(),
        "the specification's own sample must parse cleanly:{failures}"
    );
}

#[test]
fn sample_declaration_counts_match_the_spec() {
    use jwc::ast::Decl;
    let mut tables = 0;
    let mut views = 0;
    let mut classes = 0;
    let mut services = 0;
    let mut middlewares = 0;
    let mut routes_blocks = 0;
    let mut routes = 0;
    let mut enums = 0;
    let mut tests = 0;

    for f in sample_files() {
        let parsed = jwc::parse_file(&f).expect("read");
        for d in &parsed.program.decls {
            match d {
                Decl::Table(_) => tables += 1,
                Decl::View(_) => views += 1,
                Decl::Class(_) => classes += 1,
                Decl::Service(_) => services += 1,
                Decl::Middleware(_) => middlewares += 1,
                Decl::Enum(_) => enums += 1,
                Decl::Test(_) => tests += 1,
                Decl::Routes(r) => {
                    routes_blocks += 1;
                    routes += r.routes.len();
                }
                _ => {}
            }
        }
    }

    // These are the numbers docs/spec/v1/sample/README.md advertises. A
    // mismatch means either the sample or its README drifted.
    assert_eq!(tables, 13, "tables");
    assert_eq!(views, 5, "views");
    assert_eq!(enums, 5, "enums");
    assert_eq!(classes, 14, "classes");
    assert_eq!(services, 4, "services");
    assert_eq!(middlewares, 7, "middleware");
    assert!(routes_blocks >= 9, "routes blocks: {routes_blocks}");
    assert_eq!(routes, 26, "routes");
    assert_eq!(tests, 4, "tests");
}

// ---------------------------------------------------------------- corpus

/// Every production in `docs/spec/v1/grammar.ebnf` that has observable
/// syntax, with a snippet that exercises it. The coverage test below fails
/// if the grammar grows a production this list does not name.
fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("namespace_decl", "namespace a.b.c;"),
        ("import_decl", "import db.org;"),
        (
            "database_decl",
            "database App : Postgres { init() { pool_size = 20; } }",
        ),
        ("schema_decl", "schema auth of App;"),
        ("schema_decl.physical", "schema auth of App as \"authz\";"),
        ("table_decl", "table T of App.s { id bigint; }"),
        (
            "table_decl.physical_was",
            "table T of App.s as \"t_x\" was \"old_t\" { id bigint; }",
        ),
        ("doc_comment", "--- doc\ntable T of App.s {\n--- col\nid bigint; }"),
        ("column_def.optional", "table T of App.s { a text?; }"),
        ("column_def.array", "table T of App.s { a text[]; }"),
        (
            "column_modifier.pk_identity",
            "table T of App.s { id bigint primary key identity; }",
        ),
        (
            "column_modifier.unique_msg",
            "table T of App.s { e text unique : \"taken\"; }",
        ),
        (
            "column_modifier.private_server",
            "table T of App.s { a text private; b bigint server; }",
        ),
        (
            "column_modifier.default",
            "table T of App.s { a timestamptz default now(); }",
        ),
        (
            "column_modifier.on_update",
            "table T of App.s { a timestamptz default now() on update now(); }",
        ),
        (
            "column_modifier.physical_was",
            "table T of App.s { a text as \"aa\" was \"bb\"; }",
        ),
        (
            "column_modifier.rules",
            "table T of App.s { a varchar(9) minLength(1), pattern(r\"^x$\"); }",
        ),
        ("pk_constraint", "table T of App.s { primary key (a, b); }"),
        (
            "fk_constraint",
            "table T of App.s { foreign key (a) references App.s.U (id); }",
        ),
        (
            "fk_action",
            "table T of App.s { foreign key (a) references App.s.U (id) on delete cascade on update restrict; }",
        ),
        (
            "ref_action.all",
            "table T of App.s { foreign key (a) references App.s.U (id) on delete set null; \
             foreign key (b) references App.s.U (id) on delete set default; \
             foreign key (c) references App.s.U (id) on delete no action; }",
        ),
        ("uq_constraint", "table T of App.s { unique (a, b) : \"m\"; }"),
        (
            "uq_constraint.partial",
            "table T of App.s { unique (a) where b == null : \"m\"; }",
        ),
        ("check_constraint", "table T of App.s { check (a > 0) : \"m\"; }"),
        ("index_def", "table T of App.s { index on (a, b); }"),
        (
            "index_def.partial_desc_nulls_using",
            "table T of App.s { index on (a desc nulls last) where b == null using gin; }",
        ),
        ("enum_decl", "enum E { a, b }"),
        ("enum_decl.typed", "enum E of App.s as \"e_t\" { a, b }"),
        (
            "view_decl",
            "view V of App.s { select T from App.s.T as { id } }",
        ),
        ("class_decl", "class C { a text; }"),
        (
            "class_field.rules",
            "class C { a text required, minLength(2); b int transient; }",
        ),
        ("error_decl", "error E(code: text) = 402 : \"m\";"),
        ("error_decl.bare", "error E = 429;"),
        ("service_decl", "service S { function f() { return 1; } }"),
        (
            "function_decl.full",
            "function f(a: bigint, b: text = \"x\") -> { n: int } raises (NotFound) { return { n: 1 }; }",
        ),
        ("middleware_decl", "middleware M { let a = 1; }"),
        (
            "middleware_decl.full",
            "middleware M(@id: bigint) requires A, B provides k: text { let a = 1; after { return; } }",
        ),
        ("routes_decl", "routes \"/x\" { route GET \"\" { return json(1); } }"),
        (
            "routes_decl.use",
            "routes \"/x\" use A, B { route POST \"y\" use C { return json(1); } }",
        ),
        (
            "error_handler_decl",
            "errorHandler (e) { catch NotFound (err) { return notFound($err.message); } catch (err) { return internalError(); } }",
        ),
        ("server_decl", "server { a = 1; cors { origins = [\"x\"]; } }"),
        ("test_decl", "test \"t\" { assert 1 == 1; }"),
        ("assert_stmt.fails", "test \"t\" { assert fails Conflict { let a = 1; }; }"),
        ("assert_stmt.fails_untyped", "test \"t\" { assert fails { let a = 1; }; }"),
        (
            "type_ref.scalars",
            "table T of App.s { a bigint; b int; c smallint; d numeric(14, 2); e varchar(9); \
             f text; g boolean; h timestamptz; i date; j time; k interval; l uuid; \
             m jsonb; n inet; o bytea; }",
        ),
        ("type_ref.record", "function f() -> { a: text, b: int } { return { a: \"\", b: 1 }; }"),
        ("type_ref.array_optional", "class C { a text[]?; }"),
        ("let_stmt", "function f() { let a = 1; }"),
        ("let_stmt.typed", "function f() { let a: bigint = 1; }"),
        ("assign_stmt.local", "function f() { let a = 1; $a = 2; }"),
        ("assign_stmt.context", "middleware M provides k: int { context.k = 1; }"),
        ("if_stmt", "function f() { if (true) { return 1; } }"),
        ("if_stmt.else", "function f() { if (true) { return 1; } else { return 2; } }"),
        (
            "if_stmt.else_if",
            "function f() { if (true) { return 1; } else if (false) { return 2; } else { return 3; } }",
        ),
        ("for_stmt", "function f() { for (x in $xs) { let a = $x; } }"),
        ("return_stmt.bare", "middleware M { after { return; } }"),
        ("throw_stmt", "function f() { throw NotFound(\"x\"); }"),
        ("throw_stmt.bare", "function f() { throw QuotaExceeded; }"),
        ("transaction_stmt", "function f() { transaction { return 1; } }"),
        (
            "select_expr",
            "function f() { return select T from App.s.T; }",
        ),
        (
            "select_expr.all_clauses",
            "function f() { return select T from App.s.T \
             left join App.s.U U on U.id == T.u_id as one u \
             where a == 1 group by a having count(b) > 1 as { a, n: count(b) } \
             orderby a desc nulls last limit 10; }",
        ),
        (
            "select_expr.first",
            "function f() { return select T from App.s.T where id == 1 first; }",
        ),
        (
            "join_clause.inner",
            "function f() { return select T from App.s.T inner join App.s.U on U.id == T.u_id as one u; }",
        ),
        (
            "join_result.many_order_limit",
            "function f() { return select T from App.s.T left join App.s.U on U.t_id == T.id as many us orderby id desc limit 5; }",
        ),
        (
            "join_result.under",
            "function f() { return select T from App.s.T \
             left join App.s.U on U.t_id == T.id as many us \
             left join App.s.V on V.id == U.v_id as one v under us; }",
        ),
        (
            "projection.nested",
            "function f() { return select T from App.s.T left join App.s.U on U.id == T.u_id as one u as { id, u: { id, name } }; }",
        ),
        (
            "page_clause",
            "function f() { return select T from App.s.T orderby id desc page after $c size 50 max 100; }",
        ),
        (
            "insert_expr",
            "function f() { return insert into App.s.T { a = 1 } as { id }; }",
        ),
        (
            "conflict_clause.nothing",
            "function f() { return insert into App.s.T { a = 1 } on conflict (a) do nothing as { id }; }",
        ),
        (
            "conflict_clause.update",
            "function f() { return insert into App.s.T { a = 1 } on conflict (a) do update set b = 2; }",
        ),
        (
            "update_expr",
            "function f() { return update App.s.T set a = 1, b =? $x, ...$req where id == 1 as { id } first; }",
        ),
        (
            "delete_expr",
            "function f() { return delete from App.s.T where id == 1 as { id } first; }",
        ),
        ("or_throw", "function f() { let a = $x or throw NotFound(\"m\"); }"),
        (
            "catch_postfix",
            "function f() { let a = insert into App.s.T { a = 1 } as { id } catch Conflict (err) { return 1; }; }",
        ),
        ("coalesce_expr", "function f() { let a = $x ?? 1; }"),
        ("ternary_expr", "function f() { let a = $x ? 1 : 2; }"),
        ("or_expr", "function f() { let a = $x or $y; }"),
        ("and_expr", "function f() { let a = $x and $y; }"),
        ("not_expr", "function f() { let a = !$x; }"),
        (
            "compare_expr.ops",
            "function f() { let a = $x == 1 and $y != 2 and $z < 3 and $w <= 4 and $v > 5 and $u >= 6; }",
        ),
        ("compare_expr.optional", "function f() { return select T from App.s.T where a ==? $x; }"),
        ("compare_expr.like", "function f() { return select T from App.s.T where a like $x; }"),
        ("compare_expr.ilike", "function f() { return select T from App.s.T where a ilike $x; }"),
        ("compare_expr.in", "function f() { return select T from App.s.T where a in (1, 2); }"),
        ("compare_expr.not_in", "function f() { return select T from App.s.T where a not in ($xs); }"),
        (
            "compare_expr.exists",
            "function f() { return select T from App.s.T where exists (select U from App.s.U where U.t_id == T.id); }",
        ),
        (
            "compare_expr.not_exists",
            "function f() { return select T from App.s.T where not exists (select U from App.s.U where U.t_id == T.id); }",
        ),
        ("additive", "function f() { let a = 1 + 2 - 3; }"),
        ("multiplicative", "function f() { let a = 1 * 2 / 3 % 4; }"),
        ("unary.neg", "function f() { let a = -1; }"),
        ("postfix.field", "function f() { let a = $x.y.z; }"),
        ("postfix.index", "function f() { let a = $x[0]; }"),
        ("postfix.call", "function f() { let a = g(1, 2); }"),
        ("call_args.filter", "function f() { return select T from App.s.T group by a as { n: count(b where b > 1) }; }"),
        ("param_ref", "middleware M(@id: bigint) { let a = @id; }"),
        ("local_ref", "function f() { let a = 1; let b = $a; }"),
        ("object_literal", "function f() { let a = { x: 1, y: 2 }; }"),
        ("object_literal.assign", "function f() { return insert into App.s.T { a = 1 }; }"),
        ("object_literal.string_key", "function f() { let a = { \"X-Id\": 1 }; }"),
        ("spread", "function f() { return insert into App.s.T { ...$req }; }"),
        ("spread.except", "function f() { return insert into App.s.T { ...$req except (password) }; }"),
        ("array_literal", "function f() { let a = [1, 2, 3]; }"),
        ("response_expr.with", "routes \"/x\" { route GET \"\" { return json(1) with { \"Location\": \"/y\" }; } }"),
        ("literal.number", "function f() { let a = 1; let b = 1.5; }"),
        ("literal.string", "function f() { let a = \"x\"; }"),
        ("raw_string", "function f() { let a = r\"^x$\"; }"),
        ("literal.bool_null", "function f() { let a = true; let b = false; let c = null; }"),
        ("cast", "routes \"/x\" { route POST \"\" { let r = request.body() as C; return json($r); } }"),
        ("context_read_optional", "middleware M { after { let a = context.k?; } }"),
        (
            "response_expr.cookie",
            "routes \"/x\" { route GET \"\" { return json(1) with { \"A\": \"b\" } cookie(\"sid\", $s, { http_only: true }); } }",
        ),
    ]
}

#[test]
fn corpus_parses_cleanly() {
    let mut failures = String::new();
    for (name, src) in corpus() {
        let parsed = jwc::parse_str(format!("<{name}>"), src);
        if parsed.has_errors() {
            failures.push_str(&format!("\n=== {name} ===\n{}", parsed.render_all()));
        }
    }
    assert!(failures.is_empty(), "corpus snippets must parse:{failures}");
}

/// Every production name in grammar.ebnf must be exercised by at least one
/// corpus entry. The check is on the production's base name, so
/// `column_modifier.rules` covers `column_modifier`.
#[test]
fn corpus_covers_every_grammar_production() {
    let grammar = std::fs::read_to_string(repo_root().join("docs/spec/v1/grammar.ebnf"))
        .expect("read grammar.ebnf");

    let mut productions: BTreeSet<String> = BTreeSet::new();
    for line in grammar.lines() {
        let line = line.trim_start();
        if line.starts_with("(*") || line.is_empty() {
            continue;
        }
        if let Some((lhs, _)) = line.split_once('=') {
            let name = lhs.trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            {
                productions.insert(name.to_string());
            }
        }
    }

    // Productions with no syntax of their own: they are alternations or
    // lexical classes that every other entry already exercises.
    let structural: &[&str] = &[
        "program",
        "declaration",
        "dotted_name",
        "db_body",
        "init_block",
        "assignment",
        "qualified_schema",
        "qualified_table",
        "table_member",
        "table_constraint",
        "column_modifiers",
        "index_col",
        "field_rules",
        "field_rule",
        "rule_call",
        "param_list",
        "param",
        "raises_clause",
        "binder_list",
        "binder",
        "provides_clause",
        "ctx_decl",
        "after_block",
        "http_method",
        "use_clause",
        "catch_arm",
        "server_entry",
        "base_type",
        "record_type",
        "scalar_type",
        "block",
        "statement",
        "expr_stmt",
        "expr",
        "expr_list",
        "source",
        "join_kind",
        "object_shape",
        "proj_field",
        "order_by",
        "sort_key",
        "limit_clause",
        "set_clause",
        "set_item",
        "throw_target",
        "primary",
        "obj_entry",
        "call_args",
        "literal",
        "ident_list",
        "col_ref",
        "ident",
        "number",
        "string",
        "character",
        "letter",
        "digit",
        "newline",
        "or_throw_expr",
        "compare_op",
        "route_decl",
        "function_decl",
        "projection",
    ];

    let covered: BTreeSet<String> = corpus()
        .iter()
        .map(|(name, _)| {
            name.split_once('.')
                .map(|(base, _)| base.to_string())
                .unwrap_or_else(|| name.to_string())
        })
        .collect();

    let missing: Vec<&String> = productions
        .iter()
        .filter(|p| !covered.contains(*p) && !structural.contains(&p.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "grammar productions with no corpus entry: {missing:?}\n\
         add a snippet to corpus() or list the production as structural"
    );
}
