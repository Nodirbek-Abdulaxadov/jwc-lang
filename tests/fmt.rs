//! `jwc v1 fmt` acceptance: the printer is a fixed point, and its output
//! re-parses to the same tree.
//!
//! ROADMAP's criterion for v0.21.0 is "`jwc fmt` is idempotent on the
//! corpus". Idempotence alone is weak — a printer that emits nothing is
//! idempotent — so this also checks that formatting preserves the parse.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sample_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(&repo_root().join("docs/spec/v1/sample"), &mut out);
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

fn fmt(label: &str, src: &str) -> String {
    let parsed = jwc::parse_str(label, src);
    assert!(
        !parsed.has_errors(),
        "input must parse before formatting ({label}):\n{}",
        parsed.render_all()
    );
    jwc::fmt::format_program(&parsed.program)
}

#[test]
fn formatting_the_sample_is_idempotent() {
    for f in sample_files() {
        let src = std::fs::read_to_string(&f).expect("read");
        let label = f.display().to_string();
        let once = fmt(&label, &src);
        let twice = fmt(&format!("{label} (2nd pass)"), &once);
        assert_eq!(
            once,
            twice,
            "fmt is not a fixed point for {}\n--- once ---\n{once}\n--- twice ---\n{twice}",
            f.display()
        );
    }
}

#[test]
fn formatted_output_reparses_to_the_same_shape() {
    use jwc::ast::Decl;
    for f in sample_files() {
        let src = std::fs::read_to_string(&f).expect("read");
        let label = f.display().to_string();
        let before = jwc::parse_str(&label, &src);
        let printed = jwc::fmt::format_program(&before.program);
        let after = jwc::parse_str(&label, &printed);
        assert!(
            !after.has_errors(),
            "formatted output must parse ({}):\n{}\n--- source ---\n{printed}",
            f.display(),
            after.render_all()
        );
        assert_eq!(
            before.program.decls.len(),
            after.program.decls.len(),
            "declaration count changed for {}",
            f.display()
        );
        for (a, b) in before.program.decls.iter().zip(&after.program.decls) {
            assert_eq!(
                std::mem::discriminant(a),
                std::mem::discriminant(b),
                "declaration kind changed for {}",
                f.display()
            );
            if let (Decl::Table(x), Decl::Table(y)) = (a, b) {
                assert_eq!(
                    x.columns.len(),
                    y.columns.len(),
                    "columns of {}",
                    x.name.name
                );
                assert_eq!(
                    x.constraints.len(),
                    y.constraints.len(),
                    "constraints of {}",
                    x.name.name
                );
                assert_eq!(
                    x.indexes.len(),
                    y.indexes.len(),
                    "indexes of {}",
                    x.name.name
                );
            }
        }
    }
}

#[test]
fn doc_comments_survive_a_round_trip() {
    let src = "\
--- Tenant table.
table Orgs of App.org {
    --- URL-safe handle.
    slug varchar(40) unique : \"taken\";
}
";
    let once = fmt("<docs>", src);
    assert!(
        once.contains("--- Tenant table."),
        "table doc lost:\n{once}"
    );
    assert!(
        once.contains("--- URL-safe handle."),
        "column doc lost:\n{once}"
    );
    assert_eq!(once, fmt("<docs2>", &once));
}

#[test]
fn line_comments_survive_a_round_trip() {
    let src = "\
-- why this exists
function f() {
    -- and this
    let a = 1;
}
";
    let once = fmt("<comments>", src);
    assert!(
        once.contains("-- why this exists"),
        "decl comment lost:\n{once}"
    );
    assert!(once.contains("-- and this"), "stmt comment lost:\n{once}");
    assert_eq!(once, fmt("<comments2>", &once));
}

#[test]
fn corpus_snippets_are_fixed_points() {
    // Reuses the corpus from the parse test by re-declaring the tricky
    // shapes: everything with layout decisions in the printer.
    let cases: &[(&str, &str)] = &[
        (
            "insert_with_returning",
            "function f() { return insert into App.s.T { a = 1, b = 2 } as { id }; }",
        ),
        (
            "insert_on_conflict",
            "function f() { return insert into App.s.T { a = 1 } on conflict (a) do nothing as { id }; }",
        ),
        (
            "update_first_or_throw",
            "function f() { return update App.s.T set a = 1 where id == 1 as { id } first or throw NotFound(\"m\"); }",
        ),
        (
            "delete_first",
            "function f() { return delete from App.s.T where id == 1 as { id } first; }",
        ),
        (
            "select_nested_projection",
            "view V of App.s { select T from App.s.T left join App.s.U on U.id == T.u_id as one u as { id, u: { id, name } } }",
        ),
        (
            "catch_postfix",
            "function f() { let a = insert into App.s.T { a = 1 } as { id } catch Conflict (e) { return 1; }; }",
        ),
        (
            "page_clause",
            "function f() { return select T from App.s.T orderby id desc page after $c size 50 max 100; }",
        ),
        (
            "middleware_full",
            "middleware M(@id: bigint) requires A provides k: text { let a = 1; after { return; } }",
        ),
        (
            "server_block",
            "server { a = 1; cors { origins = [\"x\"]; credentials = true; } }",
        ),
        (
            "error_handler",
            "errorHandler (e) { catch NotFound (err) { return notFound($err.message); } catch (err) { return internalError(); } }",
        ),
        (
            "routes_with_headers",
            "routes \"/x\" use A { route GET \"\" use B { return json(1) with { \"Location\": \"/y\" }; } }",
        ),
        (
            "nested_if_else",
            "function f() { if ($a) { return 1; } else if ($b) { return 2; } else { return 3; } }",
        ),
        (
            "assert_fails",
            "test \"t\" { assert fails Conflict { let a = 1; }; }",
        ),
    ];
    for (name, src) in cases {
        let once = fmt(name, src);
        let twice = fmt(name, &once);
        assert_eq!(
            once, twice,
            "{name} is not a fixed point:\n{once}\n---\n{twice}"
        );
    }
}

/// The specification's sample is checked in **already formatted**. That is
/// stronger than idempotence on its own: it makes the printer's output the
/// artefact three people read in ROADMAP's v0.20.0 review, so a layout
/// regression shows up as a diff on the sample rather than only in a test
/// fixture.
#[test]
fn the_sample_is_checked_in_formatted() {
    let mut unformatted = Vec::new();
    for f in sample_files() {
        let src = std::fs::read_to_string(&f).expect("read");
        let parsed = jwc::parse_str(f.display().to_string(), &src);
        assert!(
            !parsed.has_errors(),
            "{}\n{}",
            f.display(),
            parsed.render_all()
        );
        let printed = jwc::fmt::format_program(&parsed.program);
        if printed != src {
            unformatted.push(f.display().to_string());
        }
    }
    assert!(
        unformatted.is_empty(),
        "run `cargo run --bin jwc -- v1 fmt docs/spec/v1/sample`; unformatted: {unformatted:#?}"
    );
}
