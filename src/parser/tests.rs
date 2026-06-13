//! Parser + validate integration tests.
//!
//! Moved out of the parent parser module so `mod.rs` stays under the
//! 1,000-line budget. Pulls in the same `super::*` glob that the original
//! inline `mod tests` did — `Program`, `ModelKind`, etc. flow in via the
//! `crate::ast::*` re-exports below.

use super::*;
use crate::ast::{ModelKind, Program, Stmt};

#[test]
fn atomic_update_set_parses_and_validates() {
    // Canonical jwc-shortener pattern: `hits = hits + 1` — the read+write
    // race-condition fix. Should validate cleanly against the entity.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Link of AppDb {
            id uuid pk;
            code text(40);
            hits int;
        }
        route POST "click/{code}" {
            let c = path_param("code");
            update AppDb.Link set hits = hits + 1 where Link.code == @c;
            return text("ok");
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    // Confirm the statement is the new variant, not the legacy
    // whole-row `DbUpdate`.
    let body = &program.routes[0].body;
    let stmt = &body[1];
    assert!(
        matches!(stmt, crate::ast::Stmt::DbUpdateSet { .. }),
        "expected DbUpdateSet, got {:?}",
        stmt
    );
}

#[test]
fn legacy_whole_row_update_still_parses() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Link of AppDb { id uuid pk; }
        function go() {
            let x = new Link();
            update x in AppDb.Link;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let body = &program.functions[0].body;
    assert!(matches!(body[1], crate::ast::Stmt::DbUpdate { .. }));
}

#[test]
fn atomic_update_rejects_unknown_column() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Link of AppDb { id uuid pk; code text(40); }
        route POST "bump" {
            update AppDb.Link set hits = hits + 1 where Link.code == @c;
            return text("ok");
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(
        err.contains("Unknown column 'hits'"),
        "expected unknown-column error, got: {err}"
    );
}

#[test]
fn atomic_update_requires_where_clause() {
    // Mirror `delete from CTX.Table` behaviour — refuse the wide-open
    // form at parse time. A `set ...;` without `where` would touch
    // every row, which is almost certainly a bug.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity Link of AppDb { id uuid pk; hits int; }
        route POST "bump" {
            update AppDb.Link set hits = hits + 1;
            return text("ok");
        }
    "#;
    let err = parse_program(src).unwrap_err().to_string();
    assert!(err.contains("'where' clause"), "got: {err}");
}

#[test]
fn duplicate_route_error_carries_line_col_and_snippet() {
    // Two identical routes — second declaration triggers E005. The
    // validator now stamps `at line X, col Y` + snippet on the
    // SECOND `route` keyword, not the first.
    let src = "route GET \"ping\" {\n    return text(\"a\");\n}\n\nroute GET \"ping\" {\n    return text(\"b\");\n}\n";
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Duplicate route"), "missing msg: {err}");
    assert!(err.contains("at line 5, col 1"), "wrong loc: {err}");
    assert!(err.contains("5 | route"), "missing snippet: {err}");
    assert!(err.contains("^ here"), "missing caret: {err}");
}

#[test]
fn duplicate_function_error_carries_line_col() {
    let src = "function foo() { return 1; }\nfunction foo() { return 2; }\n";
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Duplicate function name"));
    assert!(err.contains("at line 2, col 1"));
}

#[test]
fn multi_file_validator_error_names_offending_file() {
    // Two parse_program_with_label calls feed merge_program — the
    // resulting Program has both source files behind one Vec, and
    // each decl's file_idx points at its own source. A duplicate
    // route across files must render with the SECOND file's label,
    // not the first.
    let file_a = "route GET \"ping\" {\n    return text(\"a\");\n}\n";
    let file_b = "// pong handler\n// duplicates the GET above\nroute GET \"ping\" {\n    return text(\"b\");\n}\n";
    let prog_a = parse_program_with_label(file_a, "main.jwc").unwrap();
    let prog_b = parse_program_with_label(file_b, "extras.jwc").unwrap();
    let mut combined = prog_a;
    crate::project::merge_program(&mut combined, prog_b).unwrap();
    let err = validate_program(&combined).unwrap_err().to_string();
    assert!(err.contains("Duplicate route"), "missing msg: {err}");
    assert!(
        err.contains("at extras.jwc:3:1"),
        "expected per-file location, got: {err}"
    );
    assert!(err.contains("^ here"), "missing snippet caret: {err}");
}

#[test]
fn validator_falls_back_when_source_is_empty() {
    // A hand-built Program (no parse_program → no .source) should
    // still surface the bare error message, not panic, not omit info.
    let mut program = Program::default();
    program.functions.push(crate::ast::FunctionDecl {
        name: "x".into(),
        params: Vec::new(),
        return_type: None,
        body: Vec::new(),
        is_async: false,
        namespace: Vec::new(),
        visibility: crate::ast::Visibility::Private,
        offset: 0,
        file_idx: 0,
    });
    program.functions.push(crate::ast::FunctionDecl {
        name: "x".into(),
        params: Vec::new(),
        return_type: None,
        body: Vec::new(),
        is_async: false,
        namespace: Vec::new(),
        visibility: crate::ast::Visibility::Private,
        offset: 0,
        file_idx: 0,
    });
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Duplicate function name: x"));
    assert!(!err.contains("at line "), "should not invent loc: {err}");
}

#[test]
fn unknown_column_in_where_suggests_close_match() {
    // `email` vs `emial` typo — Levenshtein 1 ≤ threshold 2.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            email text(80);
        }
        route GET "/u" {
            let r = select User from AppDb.User where User.emial == @s first;
            return text("ok");
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'emial'"), "got: {err}");
    assert!(
        err.contains("did you mean 'email'?"),
        "missing suggestion: {err}"
    );
}

#[test]
fn unknown_column_without_close_match_omits_suggestion() {
    // `xyz` is far from any real field — no suggestion should be
    // appended so the error stays tight.
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            email text(80);
        }
        route GET "/u" {
            let r = select User from AppDb.User where User.xyz == @s first;
            return text("ok");
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'xyz'"), "got: {err}");
    assert!(
        !err.contains("did you mean"),
        "should not invent suggestion: {err}"
    );
}

#[test]
fn parser_error_includes_line_col_and_snippet() {
    // Garbage token on line 3 — parser bails with `error_here(...)`.
    let src = "function ok() {\n    print(1);\n    !!! ;\n}\n";
    let err = parse_program(src).unwrap_err().to_string();
    assert!(
        err.contains("at line 3, col "),
        "expected line:col header, got: {err}"
    );
    // The rustc-style snippet ("3 | ..." + "^ here") makes the failure
    // placeable without opening an editor.
    assert!(
        err.contains("3 | "),
        "expected line gutter in snippet, got: {err}"
    );
    assert!(
        err.contains("^ here"),
        "expected caret in snippet, got: {err}"
    );
}

#[test]
fn parses_minimal_program() {
    let src = r#"
        dbcontext AppDb : Postgres;

        entity User of AppDb {
            id uuid;
            name text(50);
            balance decimal(18,2);
        }
    "#;

    let program = parse_program(src).unwrap();
    assert_eq!(program.dbcontexts.len(), 1);
    let entities = program
        .models
        .iter()
        .filter(|m| m.kind == ModelKind::Entity)
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].context_name.as_deref(), Some("AppDb"));
    validate_program(&program).unwrap();
}

#[test]
fn const_self_reference_is_circular_error() {
    let src = r#"
        const X = X + 1;
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("circular"), "got: {err}");
}

#[test]
fn const_with_call_is_non_const_error() {
    let src = r#"
        const Y = db_query("q");
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("constant expression"), "got: {err}");
}

#[test]
fn const_referenced_in_main_validates() {
    let src = r#"
        const PI = 3;
        function main() { print(PI); }
    "#;

    let program = parse_program(src).unwrap();
    assert_eq!(program.consts.len(), 1);
    validate_program(&program).unwrap();
}

#[test]
fn fails_when_entity_references_unknown_dbcontext() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of MissingDb { id uuid; }
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("unknown dbcontext"));
}

#[test]
fn fails_when_select_uses_wrong_context_for_entity() {
    let src = r#"
        dbcontext AppDb : Postgres;
        dbcontext AuditDb : Postgres;

        entity User of AppDb {
            id uuid;
        }

        function bad() {
            let x = select User from AuditDb.User;
            return x;
        }
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("bound to dbcontext"));
}

#[test]
fn fails_when_db_statement_targets_unknown_table_in_context() {
    let src = r#"
        dbcontext AppDb : Postgres;

        entity User of AppDb {
            id uuid;
        }

        function bad(user) {
            insert user into AppDb.Todo;
        }
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown table/entity"));
}

#[test]
fn parses_control_flow_program() {
    let src = r#"
        function main() {
            let i = 0;
            while (i < 5) {
                if (i == 2) {
                    i = i + 1;
                    continue;
                }
                print(i);
                if (i == 3) {
                    break;
                }
                i = i + 1;
            }
        }
    "#;

    let program = parse_program(src).unwrap();
    assert_eq!(program.functions.len(), 1);
    validate_program(&program).unwrap();
}

#[test]
fn parses_route_program() {
    let src = r#"
        route GET "/health" {
            print("ok");
        }

        function main() {
            dispatch("GET", "/health");
        }
    "#;

    let program = parse_program(src).unwrap();
    assert_eq!(program.routes.len(), 1);
    validate_program(&program).unwrap();
}

#[test]
fn fails_on_duplicate_entity() {
    let src = r#"
        entity User { id uuid; }
        entity User { id uuid; }
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Duplicate model name"));
}

#[test]
fn fails_on_unknown_type() {
    let src = r#"
        entity User { id weirdtype; }
    "#;

    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown type"));
}

#[test]
fn parses_db_select_expr() {
    let src = r#"
        function getAll() {
            let cars = select CarEntity from db.Cars;
            return cars;
        }
    "#;
    let program = parse_program(src).unwrap();
    assert_eq!(program.functions.len(), 1);
    // Verify the body has Let with DbSelect expr
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { name, value } => {
            assert_eq!(name, "cars");
            match value {
                crate::ast::Expr::DbSelect {
                    entity,
                    table,
                    first,
                    ..
                } => {
                    assert_eq!(entity, "CarEntity");
                    assert_eq!(table, "Cars");
                    assert!(!first);
                }
                _ => panic!("expected DbSelect"),
            }
        }
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn parses_db_select_where_first() {
    let src = r#"
        function getOne(id) {
            let car = select CarEntity from db.Cars where CarEntity.id == @id first;
            return car;
        }
    "#;
    let program = parse_program(src).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect {
                where_clause,
                first,
                ..
            } => {
                assert!(first);
                let wc = where_clause.as_ref().unwrap();
                let atom = match wc.as_ref() {
                    crate::ast::WhereExpr::Atom(a) => a,
                    _ => panic!("expected atom"),
                };
                assert_eq!(atom.field, "CarEntity.id");
                assert_eq!(atom.op, "==");
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn select_where_unknown_column_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            name varchar(60);
        }

        function pickOne(name) {
            let u = select User from AppDb.User where User.nm == @name first;
            return u;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'nm'"));
}

#[test]
fn select_orderby_unknown_column_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            name varchar(60);
        }

        function listAll() {
            let xs = select User from AppDb.User orderby User.created_at desc;
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'created_at'"));
}

#[test]
fn select_where_known_column_passes() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            name varchar(60);
        }

        function pickByName(name) {
            let u = select User from AppDb.User where User.name == @name first;
            return u;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
}

#[test]
fn parses_db_select_orderby_limit_offset() {
    let src = r#"
        function listCars(country) {
            let cars = select CarEntity from db.Cars
                where CarEntity.country == @country
                orderby CarEntity.created_at desc
                limit 20 offset 10;
            return cars;
        }
    "#;
    let program = parse_program(src).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect {
                where_clause,
                order_by,
                limit,
                offset,
                first,
                ..
            } => {
                assert!(!first);
                assert!(where_clause.is_some());
                let ob = order_by.as_ref().expect("expected orderby");
                assert_eq!(ob.field, "CarEntity.created_at");
                assert_eq!(ob.dir, crate::ast::SortDir::Desc);
                assert!(matches!(limit.as_deref(), Some(crate::ast::Expr::Int(20))));
                assert!(matches!(offset.as_deref(), Some(crate::ast::Expr::Int(10))));
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn parses_entity_projection_subset() {
    let src = r#"
        dbcontext AppDb : Postgres;

        entity User of AppDb {
            id uuid pk;
            name varchar(60);
            email varchar(120);
            password varchar(200);
        }

        function pickPublic() {
            let xs = select User { name, email } from AppDb.User;
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { projection, .. } => {
                assert_eq!(projection, &vec!["name".to_string(), "email".to_string()]);
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn projection_with_unknown_column_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            name varchar(60);
        }
        function bad() {
            let xs = select User { name, gender } from AppDb.User;
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'gender'"));
}

#[test]
fn star_projection_rejects_brace_list() {
    let src = r#"
        function bad() {
            let xs = select * { name } from AppDb.User;
            return xs;
        }
    "#;
    let err = parse_program(src).unwrap_err().to_string();
    assert!(err.contains("projection") && err.contains("'*'"));
}

#[test]
fn parses_entity_navigation_and_with_clause() {
    let src = r#"
        dbcontext AppDb : Postgres;

        entity User of AppDb {
            id uuid pk;
            name varchar(60);
            posts: List<Post> via Post.user_id;
            profile: Profile via Profile.user_id;
        }

        entity Post of AppDb {
            id uuid pk;
            user_id uuid references User.id;
            title varchar(200);
        }

        entity Profile of AppDb {
            id uuid pk;
            user_id uuid references User.id;
            bio varchar(300);
        }

        function getOne(id) {
            let u = select User with posts, profile from AppDb.User
                where User.id == @id first;
            return u;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let user = program.models.iter().find(|m| m.name == "User").unwrap();
    assert_eq!(user.navigations.len(), 2);
    assert_eq!(user.navigations[0].name, "posts");
    assert_eq!(
        user.navigations[0].kind,
        crate::ast::NavigationKind::OneToMany
    );
    assert_eq!(user.navigations[0].target_entity, "Post");
    assert_eq!(user.navigations[0].target_field, "user_id");
    assert_eq!(
        user.navigations[1].kind,
        crate::ast::NavigationKind::OneToOne
    );

    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { with_relations, .. } => {
                assert_eq!(
                    with_relations,
                    &vec!["posts".to_string(), "profile".to_string()]
                );
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn unknown_navigation_in_with_clause_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb { id uuid pk; name varchar(60); }

        function bad() {
            let u = select User with ghosts from AppDb.User first;
            return u;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("no navigation property 'ghosts'"));
}

#[test]
fn navigation_to_unknown_entity_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            ghosts: List<Ghost> via Ghost.user_id;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("unknown entity 'Ghost'"));
}

#[test]
fn parses_select_count_aggregation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            country varchar(2);
        }

        function total(country) {
            let n = select count(*) from AppDb.User where User.country == @country;
            return n;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbCount {
                table,
                where_clause,
                ..
            } => {
                assert_eq!(table, "User");
                assert!(where_clause.is_some());
            }
            _ => panic!("expected DbCount"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn count_with_unknown_column_in_where_fails_validation() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb { id uuid pk; }

        function total() {
            let n = select count(*) from AppDb.User where User.missing == 1;
            return n;
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("Unknown column 'missing'"));
}

#[test]
fn parses_where_with_like_operator() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            email varchar(120);
        }

        function search(q) {
            let xs = select User from AppDb.User where User.email like @q;
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { where_clause, .. } => {
                let wc = where_clause.as_ref().unwrap();
                let atom = match wc.as_ref() {
                    crate::ast::WhereExpr::Atom(a) => a,
                    _ => panic!("expected atom"),
                };
                assert_eq!(atom.op, "like");
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn parses_where_with_in_list() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            role varchar(20);
        }

        function admins() {
            let xs = select User from AppDb.User where User.role in ("admin", "owner");
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { where_clause, .. } => {
                let wc = where_clause.as_ref().unwrap();
                match wc.as_ref() {
                    crate::ast::WhereExpr::InList { field, values } => {
                        assert_eq!(field, "User.role");
                        assert_eq!(values.len(), 2);
                    }
                    _ => panic!("expected InList"),
                }
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn in_with_empty_list_fails_to_parse() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb { id uuid pk; }

        function empty() {
            let xs = select User from AppDb.User where User.id in ();
            return xs;
        }
    "#;
    let err = parse_program(src).unwrap_err().to_string();
    assert!(err.contains("at least one value"));
}

#[test]
fn parses_compound_where_with_and_or_and_parens() {
    let src = r#"
        dbcontext AppDb : Postgres;
        entity User of AppDb {
            id uuid pk;
            age int;
            country varchar(2);
            is_admin bool;
        }

        function pick(country, min) {
            let xs = select User from AppDb.User
                where (User.age >= @min and User.country == @country)
                   or User.is_admin == true;
            return xs;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { where_clause, .. } => {
                let wc = where_clause.as_ref().unwrap();
                assert!(matches!(wc.as_ref(), crate::ast::WhereExpr::Or(_, _)));
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn parses_db_select_orderby_default_asc() {
    let src = r#"
        function listAll() {
            let cars = select CarEntity from db.Cars orderby CarEntity.name;
            return cars;
        }
    "#;
    let program = parse_program(src).unwrap();
    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { order_by, .. } => {
                let ob = order_by.as_ref().unwrap();
                assert_eq!(ob.dir, crate::ast::SortDir::Asc);
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[test]
fn parses_db_insert_update_delete() {
    let src = r#"
        function mutations(car) {
            insert car into db.Cars;
            update car in db.Cars;
            delete car from db.Cars;
        }
    "#;
    let program = parse_program(src).unwrap();
    let body = &program.functions[0].body;
    assert!(matches!(body[0], crate::ast::Stmt::DbInsert { .. }));
    assert!(matches!(body[1], crate::ast::Stmt::DbUpdate { .. }));
    assert!(matches!(body[2], crate::ast::Stmt::DbDelete { .. }));
}

#[test]
fn parses_new_entity_and_field_assign() {
    let src = r#"
        function create() {
            let car = new CarEntity();
            car.model = "Tesla";
            return car;
        }
    "#;
    let program = parse_program(src).unwrap();
    let body = &program.functions[0].body;
    // let car = new CarEntity()
    match &body[0] {
        crate::ast::Stmt::Let { value, .. } => {
            assert!(matches!(value, crate::ast::Expr::NewEntity { .. }));
        }
        _ => panic!("expected Let"),
    }
    // car.model = "Tesla"
    assert!(matches!(body[1], crate::ast::Stmt::FieldAssign { .. }));
}

#[test]
fn typed_param_field_access_is_checked_at_compile_time() {
    let src = r#"
        class RegisterReq {
            username string;
            email string;
            password string;
        }
        function register(req: RegisterReq) {
            print(req.username);
            print(req.ghost);
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(
        err.contains("field 'ghost' is not declared on RegisterReq"),
        "got: {err}"
    );
}

#[test]
fn typed_param_field_access_passes_for_known_fields() {
    let src = r#"
        class RegisterReq {
            username string;
            email string;
        }
        function register(req: RegisterReq) {
            print(req.username);
            print(req.email);
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
}

#[test]
fn nullable_typed_param_still_checks_fields() {
    let src = r#"
        class RegisterReq { name string; }
        function register(req: RegisterReq?) {
            print(req.surname);
        }
    "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("'surname' is not declared on RegisterReq"));
}

#[test]
fn list_typed_param_skips_field_check() {
    let src = r#"
        class Tag { name string; }
        function uses(xs: List<Tag>) {
            // `xs.something` doesn't make sense on a list, so we don't
            // try to type-check it — runtime would report the misuse.
            print(xs.anything);
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
}

#[test]
fn parses_typed_params_and_return_type() {
    let src = r#"
        function add(a: int, b: int): int {
            return a + b;
        }
        function greet(name: string) {
            print(name);
        }
        function id(x) {
            return x;
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let add = &program.functions[0];
    assert_eq!(add.name, "add");
    assert_eq!(add.params[0].name, "a");
    assert_eq!(add.params[0].ty, Some("int".to_string()));
    assert_eq!(add.params[1].name, "b");
    assert_eq!(add.params[1].ty, Some("int".to_string()));
    assert_eq!(add.return_type, Some("int".to_string()));

    let greet = &program.functions[1];
    assert_eq!(greet.params[0].ty, Some("string".to_string()));
    assert_eq!(greet.return_type, None);

    let id = &program.functions[2];
    assert_eq!(id.params[0].ty, None);
}

#[tokio::test]
async fn runner_type_mismatch_returns_error() {
    let src = r#"
        function takesInt(x: int) { print(x); }
        function main() { takesInt(true); }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let result = crate::runner::run_main(&program).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Type error"));
    assert!(msg.contains("'x'"));
    assert!(msg.contains("int"));
}

#[test]
fn parses_dome_functions_and_qualified_calls() {
    let src = r#"
        dome BrandService {
            function getAll() {
                return 42;
            }
        }

        function main() {
            let x = BrandService.getAll();
            print(x);
        }
    "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    assert!(program
        .functions
        .iter()
        .any(|f| f.name == "BrandService.getAll"));
}

#[test]
fn parses_class_models() {
    let src = r#"
        class BrandDto {
            id int;
            name string;
        }

        function main() {
            let dto = new BrandDto();
            dto.name = "A";
            print(dto.name);
        }
    "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    assert!(program
        .models
        .iter()
        .any(|m| m.kind == ModelKind::Class && m.name == "BrandDto"));
}

#[test]
fn fails_on_type_keyword_model_decl() {
    let src = r#"
        type BrandView {
            id int;
        }
    "#;

    let err = parse_program(src).unwrap_err().to_string();
    // The exact preamble varies with the keyword list; the stable hint
    // is that the parser names the legal top-level forms.
    assert!(
        err.contains("expected") && err.contains("entity/class"),
        "unexpected error: {err}"
    );
}

// --- Sprint 3B: dotted catch type parser + validator -----------------------

/// Pull the single `Stmt::Try` out of a function body so tests can poke at
/// its `catch_type` field directly. Panics if the shape doesn't match.
fn try_catch_type_of(program: &Program, fn_name: &str) -> Option<String> {
    let func = program
        .functions
        .iter()
        .find(|f| f.name == fn_name)
        .unwrap_or_else(|| panic!("function `{}` not found", fn_name));
    for stmt in &func.body {
        if let crate::ast::Stmt::Try { catch_type, .. } = stmt {
            return catch_type.clone();
        }
    }
    panic!("no try statement in `{}`", fn_name);
}

#[test]
fn catch_type_single_ident_preserved() {
    // Pre-Sprint-3B behaviour: bare `DbError` parses to `Some("DbError")`.
    let src = r#"
        function f() {
            try {
                let x = 1;
            } catch (e: DbError) {
                print(e);
            }
        }
    "#;
    let program = parse_program(src).unwrap();
    assert_eq!(
        try_catch_type_of(&program, "f"),
        Some("DbError".to_string())
    );
    validate_program(&program).unwrap();
}

#[test]
fn catch_type_two_segment_dotted_parses() {
    let src = r#"
        function f() {
            try {
                let x = 1;
            } catch (e: DbError.UniqueViolation) {
                print(e);
            }
        }
    "#;
    let program = parse_program(src).unwrap();
    assert_eq!(
        try_catch_type_of(&program, "f"),
        Some("DbError.UniqueViolation".to_string())
    );
    validate_program(&program).unwrap();
}

#[test]
fn catch_type_three_segment_dotted_parses_even_if_unknown() {
    // The parser is permissive about depth; the validator is the gatekeeper.
    // Use a totally bogus root so this test isolates parsing only.
    let src = r#"
        function f() {
            try {
                let x = 1;
            } catch (e: Foo.Bar.Baz) {
                print(e);
            }
        }
    "#;
    let program = parse_program(src).unwrap();
    assert_eq!(
        try_catch_type_of(&program, "f"),
        Some("Foo.Bar.Baz".to_string())
    );
    // Validator should reject the unknown root.
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(
        err.contains("unknown catch type") && err.contains("Foo.Bar.Baz"),
        "unexpected validator error: {err}"
    );
}

#[test]
fn catch_type_trailing_dot_is_parse_error() {
    let src = r#"
        function f() {
            try {
                let x = 1;
            } catch (e: DbError.) {
                print(e);
            }
        }
    "#;
    let err = parse_program(src).unwrap_err().to_string();
    assert!(
        err.contains("type segment after '.'") || err.contains("after '.'"),
        "unexpected parse error: {err}"
    );
}

#[test]
fn validator_rejects_unknown_root_but_accepts_unknown_subtype_of_known_root() {
    // Bare unknown kind — must be rejected with a "did you mean" hint when
    // a close match exists (the table includes `DbError`, so `DbErrr`
    // should suggest it).
    let bad = r#"
        function f() {
            try {
                let x = 1;
            } catch (e: DbErrr) {
                print(e);
            }
        }
    "#;
    let bad_prog = parse_program(bad).unwrap();
    let err = validate_program(&bad_prog).unwrap_err().to_string();
    assert!(
        err.contains("unknown catch type") && err.contains("did you mean"),
        "expected a did-you-mean hint, got: {err}"
    );

    // Future-compat: a dotted type whose ROOT is a known kind passes the
    // validator even when the subtype isn't (yet) in the static table.
    let good = r#"
        function g() {
            try {
                let x = 1;
            } catch (e: DbError.NewSubtype) {
                print(e);
            }
        }
    "#;
    let good_prog = parse_program(good).unwrap();
    validate_program(&good_prog).expect("known-root dotted type must validate");
}

// --- Sprint 4B: savepoint syntax + nested-transaction rejection ----------

#[test]
fn savepoint_parses_inside_transaction() {
    let src = r#"
        function main() {
            transaction {
                savepoint sp1 {
                    print("ok");
                }
            }
        }
    "#;
    let prog = parse_program(src).expect("savepoint inside transaction must parse");
    let func = prog
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main fn");
    let outer = func.body.first().expect("outer stmt");
    let Stmt::Transaction { body } = outer else {
        panic!("expected outer Transaction, got {outer:?}");
    };
    let inner = body.first().expect("inner stmt");
    let Stmt::Savepoint { name, .. } = inner else {
        panic!("expected Savepoint, got {inner:?}");
    };
    assert_eq!(name, "sp1");
}

#[test]
fn nested_transaction_rejected_at_parse_time_with_e016() {
    let src = r#"
        function main() {
            transaction {
                transaction {
                    print("hi");
                }
            }
        }
    "#;
    let err = parse_program(src)
        .expect_err("nested transaction must fail")
        .to_string();
    assert!(
        err.contains("E016"),
        "expected E016 code in error, got: {err}"
    );
    assert!(
        err.contains("savepoint"),
        "expected hint about savepoint, got: {err}"
    );
}

#[test]
fn savepoint_with_invalid_identifier_rejected() {
    // `expect_ident` already constrains the keyword, but our defensive
    // pattern check rejects anything the lexer ever lets through that
    // wouldn't survive raw SQL interpolation.
    let src = r#"
        function main() {
            transaction {
                savepoint 123foo {
                    print("hi");
                }
            }
        }
    "#;
    assert!(parse_program(src).is_err());
}
