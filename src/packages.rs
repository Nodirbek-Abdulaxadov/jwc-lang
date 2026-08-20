//! The package content model (packages.md, gap N8).
//!
//! A package is imported, not deployed, and the line between the two is
//! **migrations**. A package that declares a table brings DDL with it, so
//! installing a dependency would mean applying someone else's schema change
//! to your database — and there is no safe version of that: two packages can
//! want the same table name, an upgrade becomes a migration you did not
//! write, and `jwc migrate new` would have to diff against sources you do
//! not control.
//!
//! These checks run only when `jwcproj.json` says `"type": "pkg"`. An
//! unknown `type` reads as an app, because the model only ever *restricts*:
//! a typo must not silently unlock declarations a package may not have.

use crate::ast::Decl;
use crate::diag::Diagnostic;
use crate::symbols::Symbols;
use crate::workspace::{Kind, Loc, Workspace};

pub fn check(ws: &Workspace, sym: &Symbols) -> Vec<(Loc, Diagnostic)> {
    let mut out = Vec::new();
    let Some(manifest) = &ws.manifest else {
        return out;
    };
    if manifest.kind != Kind::Package {
        return out;
    }

    for (fi, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            let loc = |span| Loc { file: fi, span };
            let schema_object = match d {
                Decl::Database(_) => Some("database"),
                Decl::Schema(_) => Some("schema"),
                Decl::Table(_) => Some("table"),
                Decl::View(_) => Some("view"),
                // schema.md §5 — without `of` an enum is a `varchar` plus a
                // check, so it creates nothing and is allowed.
                Decl::Enum(e) if e.schema.is_some() => Some("enum … of"),
                _ => None,
            };
            if let Some(what) = schema_object {
                out.push((
                    loc(d.span()),
                    Diagnostic::error(
                        "E1501",
                        d.span(),
                        format!(
                            "a package may not declare {} `{what}`",
                            if what.starts_with('e') { "an" } else { "a" }
                        ),
                    )
                    .note(
                        "installing a dependency would mean applying its schema change \
                         to your database — take the table name as a parameter, or \
                         have the application declare it",
                    )
                    .clause("packages.md §2.1"),
                ));
                continue;
            }
            let application_object = match d {
                Decl::Routes(_) => Some("routes"),
                Decl::ErrorHandler(_) => Some("errorHandler"),
                _ => None,
            };
            if let Some(what) = application_object {
                out.push((
                    loc(d.span()),
                    Diagnostic::error(
                        "E1502",
                        d.span(),
                        format!("a package may not declare `{what}`"),
                    )
                    .note(if what == "routes" {
                        "mounting is a decision about a URL space the package cannot \
                         see; export a `service` and let the application route to it"
                    } else {
                        "errors.md §4.1 allows exactly one `errorHandler` per program, \
                         so importing two packages that carry one would be a compile \
                         error about a construct neither author wrote"
                    })
                    .clause("packages.md §2.2"),
                ));
            }
        }
    }

    // §3.4 — an exported function that can raise and declares nothing.
    let bodies = crate::wiring::function_bodies(ws);
    for (fi, file) in ws.files.iter().enumerate() {
        for d in &file.program.decls {
            let Decl::Service(s) = d else { continue };
            for f in &s.functions {
                if !f.raises.is_empty() {
                    continue;
                }
                let inferred = crate::wiring::raises_from(sym, &bodies, &f.body);
                if inferred.is_empty() {
                    continue;
                }
                out.push((
                    Loc {
                        file: fi,
                        span: f.span,
                    },
                    Diagnostic::warning(
                        "W1501",
                        f.span,
                        format!(
                            "`{}.{}` can raise `{}` and declares no `raises`",
                            s.name.name,
                            f.name.name,
                            inferred.iter().cloned().collect::<Vec<_>>().join("`, `")
                        ),
                    )
                    .note(
                        "a consumer compiles against the declaration, not the body; \
                         an absent one silently reads as \"raises nothing\"",
                    )
                    .clause("packages.md §3.4"),
                ));
            }
        }
    }

    out
}
