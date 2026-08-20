//! `E0900` — the pre-1.0 vocabulary.
//!
//! ROADMAP's criterion for v0.21.0 is "10 `E0900` tests for the old
//! grammar's 10 keywords". Each case checks the code *and* that the message
//! names the replacement: a diagnostic that only says "removed" sends the
//! reader to a changelog.

fn only_diag(src: &str) -> (String, String) {
    let p = jwc::parse_str("<removed>", src);
    let d = p
        .diags
        .iter()
        .find(|d| d.code == "E0900")
        .unwrap_or_else(|| panic!("no E0900 for:\n{src}\ngot:\n{}", p.render_all()));
    (d.code.to_string(), d.message.clone())
}

macro_rules! removed {
    ($name:ident, $src:expr, $expect:expr) => {
        #[test]
        fn $name() {
            let (code, msg) = only_diag($src);
            assert_eq!(code, "E0900");
            assert!(
                msg.contains($expect),
                "message should point at the replacement.\n  got: {msg}\n  want substring: {}",
                $expect
            );
        }
    };
}

removed!(
    entity_points_at_table,
    "entity Accounts { id bigint; }",
    "table Accounts of App.auth"
);
removed!(
    dbcontext_points_at_database_and_schema,
    "dbcontext AppDb { }",
    "database App : Postgres"
);
removed!(dbset_points_at_table, "dbset Accounts;", "table T of App.s");
removed!(
    via_points_at_the_on_clause,
    "function f() { return select T from App.s.T via U; }",
    "the join's 'on' clause"
);
removed!(
    nav_points_at_joins,
    "table T of App.s { nav category Categories; }",
    "joins are written in the query"
);
removed!(
    validate_points_at_the_cast,
    "routes \"/x\" { route POST \"\" { validate body Register; return json(1); } }",
    "request.body() as ClassName"
);
removed!(
    new_points_at_insert,
    "function f() { let a = new Todo from $req; }",
    "insert into App.s.X"
);
removed!(
    patch_points_at_update,
    "function f() { patch $todo from $req; }",
    "update App.s.X set"
);
removed!(
    mount_points_at_full_paths,
    "mount \"/api\" { }",
    "every route declares its full path"
);
removed!(dome_is_gone, "dome { }", "it has no replacement");

/// `with` and `group` are live 1.0 keywords, so they get an ordinary parse
/// error rather than `E0900` (routing.md §10). This pins that decision:
/// if someone adds them to the removed list, correct code starts failing.
#[test]
fn live_keywords_do_not_get_e0900() {
    for src in [
        "routes \"/x\" { route GET \"\" { return json(1) with { \"A\": \"b\" }; } }",
        "function f() { return select T from App.s.T group by a as { n: count(b) }; }",
    ] {
        let p = jwc::parse_str("<live>", src);
        assert!(
            !p.has_errors(),
            "live keyword rejected:\n{}",
            p.render_all()
        );
    }
}

#[test]
fn every_removed_keyword_has_a_test() {
    use jwc::token::REMOVED_KEYWORDS;
    assert_eq!(
        REMOVED_KEYWORDS.len(),
        10,
        "the removed-keyword table changed; add or drop a test above"
    );
    for (kw, msg) in REMOVED_KEYWORDS {
        assert!(
            msg.contains("removed in 1.0"),
            "`{kw}` message should say it was removed: {msg}"
        );
        assert!(
            msg.len() > 30,
            "`{kw}` message should name the replacement, not just the removal: {msg}"
        );
    }
}
