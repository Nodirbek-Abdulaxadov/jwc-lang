//! The package content model and the export boundary (packages.md).
//!
//! No database: everything here is the checker's. Each case is a whole
//! project — a `jwcproj.json` plus sources — because the model turns on
//! what the manifest says the project *is*, and a fixture without one is
//! an application.

use std::path::Path;
use std::process::Command;

fn project(kind: &str, source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("jwcproj.json"),
        format!(r#"{{ "name": "demo", "version": "0.1.0", "type": "{kind}" }}"#),
    )
    .expect("manifest");
    std::fs::write(dir.path().join("main.jwc"), source).expect("source");
    dir
}

fn check(dir: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_jwc"))
        .args(["check", dir.to_str().expect("utf8")])
        .output()
        .expect("run jwc");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const SCHEMA: &str = "namespace demo;\n\
                      database App : Postgres;\n\
                      schema s of App;\n\
                      table T of App.s { id bigint primary key identity; }\n";

#[test]
fn a_package_may_not_bring_a_migration() {
    // packages.md §2.1 — the line between an app and a package is DDL.
    let dir = project("pkg", SCHEMA);
    let out = check(dir.path());
    for what in ["`database`", "`schema`", "`table`"] {
        assert!(
            out.contains("E1501") && out.contains(what),
            "{what} was allowed:\n{out}"
        );
    }
    // The same sources as an application are fine. Nothing about the
    // declarations changed; what changed is what the project is.
    let app = project("app", SCHEMA);
    assert!(!check(app.path()).contains("E1501"), "{}", check(app.path()));
}

#[test]
fn an_of_enum_is_a_type_and_a_bare_enum_is_not() {
    // schema.md §5 — without `of`, an enum is a `varchar` plus a check. It
    // creates nothing, so it crosses no line.
    let dir = project(
        "pkg",
        "namespace demo;\nenum Colour { red, green }\n",
    );
    let out = check(dir.path());
    assert!(!out.contains("E1501"), "{out}");

    let dir = project(
        "pkg",
        "namespace demo;\ndatabase App : Postgres;\nschema s of App;\n\
         enum Plan of App.s { free, pro }\n",
    );
    assert!(check(dir.path()).contains("E1501"), "{}", check(dir.path()));
}

#[test]
fn routes_and_the_error_handler_belong_to_the_application() {
    let dir = project(
        "pkg",
        "namespace demo;\nroutes \"/x\" {\n\
         \x20   route GET \"\" { return json({ ok: true }); }\n}\n",
    );
    let out = check(dir.path());
    assert!(out.contains("E1502"), "{out}");
    // The reason is in the message, because "not allowed" alone leaves the
    // author with nowhere to put the code.
    assert!(out.contains("export a `service`"), "{out}");
}

#[test]
fn an_exported_function_that_can_raise_says_so() {
    // packages.md §3.4 — a consumer compiles against the declaration.
    let src = "namespace demo;\n\
               error Nope(message: text) = 418 : \"nope\";\n\
               service Demo {\n\
               \x20   function boom() -> text {\n\
               \x20       throw Nope(\"nope\");\n\
               \x20   }\n\
               }\n";
    let dir = project("pkg", src);
    let out = check(dir.path());
    assert!(out.contains("W1501"), "{out}");
    assert!(out.contains("can raise `Nope`"), "{out}");

    // Declaring it silences the warning.
    let declared = src.replace(
        "function boom() -> text {",
        "function boom() -> text raises (Nope) {",
    );
    let dir = project("pkg", &declared);
    assert!(!check(dir.path()).contains("W1501"), "{}", check(dir.path()));

    // In an application `raises` is E1003 and the warning does not apply:
    // there is no boundary to declare across.
    let dir = project("app", src);
    assert!(!check(dir.path()).contains("W1501"), "{}", check(dir.path()));
}

#[test]
fn a_declared_raise_set_may_widen_but_not_narrow() {
    // packages.md §3.3 / errors.md §3.3 — E1002. A caller who handles
    // exactly what the declaration names must not meet an error nothing
    // told them about.
    let narrowing = "namespace demo;\n\
                     error A(message: text) = 418 : \"a\";\n\
                     error B(message: text) = 419 : \"b\";\n\
                     service Demo {\n\
                     \x20   function both(flag: boolean) -> text raises (A) {\n\
                     \x20       if ($flag) { throw A(\"a\"); }\n\
                     \x20       throw B(\"b\");\n\
                     \x20   }\n\
                     }\n";
    let dir = project("pkg", narrowing);
    let out = check(dir.path());
    assert!(out.contains("E1002"), "{out}");
    assert!(out.contains("`B`"), "{out}");

    // Widening is allowed: a package may name an error it does not raise
    // yet, which is how a raise set stays stable across a minor version.
    let widening = narrowing.replace("raises (A)", "raises (A, B, NotFound)");
    let dir = project("pkg", &widening);
    let out = check(dir.path());
    assert!(!out.contains("E1002"), "{out}");
}
