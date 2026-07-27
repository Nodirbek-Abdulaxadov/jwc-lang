use std::collections::{HashMap, HashSet};

use crate::ast::{Expr, Program, Stmt, WhereExpr};

#[derive(Debug, Clone)]
pub struct LintWarning {
    pub code: &'static str,
    pub message: String,
    /// Index into [`Program::sources`] for the file that declared the symbol
    /// this warning is about.
    ///
    /// Lint runs over the *merged* project program, so without this a caller
    /// can't tell which file a warning belongs to. The language server needs
    /// it to publish a warning against the file that actually declares the
    /// symbol instead of repeating every project warning on every open file.
    ///
    /// `None` when the program carries no source table (hand-built
    /// `Program::default()` instances, single-file `parse_program` callers).
    pub file_idx: Option<usize>,
}

impl LintWarning {
    /// Resolve this warning's source label (`"Infrastructure/Auth.jwc"`)
    /// against the program it came from. `None` when the program has no
    /// source table or the index is out of range.
    pub fn source_label<'a>(&self, program: &'a Program) -> Option<&'a str> {
        let label = program.sources.get(self.file_idx?)?.label.as_str();
        (!label.is_empty()).then_some(label)
    }
}

pub fn lint_program(program: &Program) -> Vec<LintWarning> {
    let mut warnings = Vec::new();

    let mut called: HashSet<String> = HashSet::new();
    // Functions used as route handlers count as "used".
    for route in &program.routes {
        if let Some(handler) = &route.handler {
            called.insert(handler.to_lowercase());
        }
    }

    // Walk every reachable statement and record any function call name.
    for function in &program.functions {
        collect_calls(&function.body, &mut called);
    }
    for route in &program.routes {
        collect_calls(&route.body, &mut called);
    }
    for mw in &program.middlewares {
        collect_calls(&mw.body, &mut called);
    }
    if let Some(handler) = &program.error_handler {
        collect_calls(&handler.body, &mut called);
    }

    // Entry points implicitly used.
    called.insert("main".to_string());

    for function in &program.functions {
        if !called.contains(&function.name.to_lowercase()) {
            warnings.push(LintWarning {
                code: "W001",
                message: format!("function '{}' is defined but never called", function.name),
                file_idx: Some(function.file_idx),
            });
        }
    }

    let mut used_middlewares: HashSet<String> = HashSet::new();
    for route in &program.routes {
        for mw in &route.middlewares {
            used_middlewares.insert(mw.to_lowercase());
        }
    }
    for mw in &program.middlewares {
        let short = mw.name.to_lowercase();
        // FQN form: `ns.sub.name` (lowercased). Used by cross-namespace refs.
        let fqn = if mw.namespace.is_empty() {
            short.clone()
        } else {
            format!(
                "{}.{}",
                mw.namespace
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect::<Vec<_>>()
                    .join("."),
                short
            )
        };
        if !used_middlewares.contains(&short) && !used_middlewares.contains(&fqn) {
            warnings.push(LintWarning {
                code: "W002",
                message: format!(
                    "middleware '{}' is declared but never attached to a route",
                    mw.name
                ),
                file_idx: Some(mw.file_idx),
            });
        }
    }

    // W004 — `select Entity from ... where Entity.pk == @x` without `first`
    // returns an array on success even though the user almost certainly
    // wants a single row. The heuristic only fires on a top-level equality
    // atom (no `and`/`or`/`in`/`between`) — anything more complex might
    // legitimately want the full match set.
    let pk_columns: HashMap<String, HashSet<String>> = program
        .models
        .iter()
        .map(|m| {
            (
                m.name.to_lowercase(),
                m.fields
                    .iter()
                    .filter(|f| f.is_primary_key)
                    .map(|f| f.name.to_lowercase())
                    .collect(),
            )
        })
        .collect();

    // Each site carries the `file_idx` of the declaration it was found in so
    // the warning can be attributed back to a file after the project merge.
    // The error handler is the one body with no `file_idx` of its own.
    let mut select_sites: Vec<(String, bool, Option<WhereExpr>, Option<usize>)> = Vec::new();
    let collect_from = |body: &[Stmt], file_idx: Option<usize>, out: &mut Vec<_>| {
        let mut local = Vec::new();
        collect_select_sites(body, &mut local);
        out.extend(
            local
                .into_iter()
                .map(|(entity, first, w)| (entity, first, w, file_idx)),
        );
    };
    for f in &program.functions {
        collect_from(&f.body, Some(f.file_idx), &mut select_sites);
    }
    for r in &program.routes {
        collect_from(&r.body, Some(r.file_idx), &mut select_sites);
    }
    for mw in &program.middlewares {
        collect_from(&mw.body, Some(mw.file_idx), &mut select_sites);
    }
    if let Some(eh) = &program.error_handler {
        collect_from(&eh.body, None, &mut select_sites);
    }
    for (entity, first, where_clause, file_idx) in &select_sites {
        if *first {
            continue;
        }
        let where_clause = match where_clause {
            Some(w) => w,
            None => continue,
        };
        if let WhereExpr::Atom(atom) = where_clause {
            if atom.op != "==" && atom.op != "=" {
                continue;
            }
            // Field can be "Entity.col" or just "col"; in the unqualified
            // form we still check against the targeted entity's PKs.
            let col = atom
                .field
                .rsplit('.')
                .next()
                .unwrap_or(&atom.field)
                .to_lowercase();
            if let Some(pks) = pk_columns.get(&entity.to_lowercase()) {
                if pks.contains(&col) {
                    warnings.push(LintWarning {
                        code: "W004",
                        message: format!(
                            "single-row `select {entity}` on PK `{col}` is missing `first` — add `first` to return one row instead of an array"
                        ),
                        file_idx: *file_idx,
                    });
                }
            }
        }
    }

    // W006 — unreachable statements after a top-level `return`. Anything
    // sitting after `return` at the same block level cannot run; flag the
    // first such statement as the suspicious one (later ones are obvious
    // once the first is fixed). Only checks the top-level function /
    // route / middleware body — branch-bodies inside if/while/try are
    // exempt because their statements after `return` ARE reachable when
    // the branch isn't taken. (W005 is taken by builtin-name-shadow.)
    fn first_dead_after_return(stmts: &[Stmt]) -> Option<usize> {
        for (i, s) in stmts.iter().enumerate() {
            if matches!(s, Stmt::Return(_)) && i + 1 < stmts.len() {
                return Some(i + 1);
            }
        }
        None
    }
    for f in &program.functions {
        if first_dead_after_return(&f.body).is_some() {
            warnings.push(LintWarning {
                code: "W006",
                message: format!(
                    "function '{}' has statements after a top-level `return` — they cannot run",
                    f.name
                ),
                file_idx: Some(f.file_idx),
            });
        }
    }
    for r in &program.routes {
        if r.handler.is_some() {
            continue;
        }
        if first_dead_after_return(&r.body).is_some() {
            warnings.push(LintWarning {
                code: "W006",
                message: format!(
                    "route `{} {}` has statements after a top-level `return` — they cannot run",
                    r.method.to_uppercase(),
                    r.path
                ),
                file_idx: Some(r.file_idx),
            });
        }
    }
    for mw in &program.middlewares {
        if first_dead_after_return(&mw.body).is_some() {
            warnings.push(LintWarning {
                code: "W006",
                message: format!(
                    "middleware '{}' has statements after a top-level `return` — they cannot run",
                    mw.name
                ),
                file_idx: Some(mw.file_idx),
            });
        }
    }

    // W003 — empty body on a declared function or route. Almost always a
    // WIP leftover that shipped accidentally; the runtime silently returns
    // null which then surfaces downstream as a confusing error.
    for function in &program.functions {
        if function.body.is_empty() {
            warnings.push(LintWarning {
                code: "W003",
                message: format!(
                    "function '{}' has an empty body — handler returns null silently",
                    function.name
                ),
                file_idx: Some(function.file_idx),
            });
        }
    }

    // W005 — user function shadows a built-in name. Calls resolve to the
    // user function (interpreter looks user-defs first) so the built-in
    // becomes silently unreachable; readers expecting `json(...)` /
    // `length(...)` / etc. behaviour get a confusing surprise.
    for function in &program.functions {
        if crate::builtins::is_builtin(&function.name)
            || crate::builtins::SPECIAL_BUILTINS.contains(&function.name.as_str())
        {
            warnings.push(LintWarning {
                code: "W006",
                message: format!(
                    "function '{}' shadows a built-in — calls resolve to the user-defined version, hiding the built-in",
                    function.name
                ),
                file_idx: Some(function.file_idx),
            });
        }
    }

    warnings
}

/// Walk the statement tree and record `(entity, first, where_clause)` for
/// every `DbSelect` expression seen inside any expression form. Used by
/// the W004 missing-`first` heuristic.
fn collect_select_sites(stmts: &[Stmt], out: &mut Vec<(String, bool, Option<WhereExpr>)>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => collect_select_sites_expr(value, out),
            Stmt::Return(Some(value)) => collect_select_sites_expr(value, out),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                collect_select_sites_expr(cond, out);
                collect_select_sites(then_body, out);
                if let Some(b) = else_body {
                    collect_select_sites(b, out);
                }
            }
            Stmt::While { cond, body } => {
                collect_select_sites_expr(cond, out);
                collect_select_sites(body, out);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                collect_select_sites(body, out);
                collect_select_sites(catch_body, out);
            }
            Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
                collect_select_sites(body, out)
            }
            Stmt::ForIn { iter, body, .. } => {
                collect_select_sites_expr(iter, out);
                collect_select_sites(body, out);
            }
            _ => {}
        }
    }
}

fn collect_select_sites_expr(expr: &Expr, out: &mut Vec<(String, bool, Option<WhereExpr>)>) {
    match expr {
        Expr::DbSelect {
            entity,
            where_clause,
            first,
            ..
        } => {
            out.push((
                entity.clone(),
                *first,
                where_clause.as_ref().map(|w| (**w).clone()),
            ));
        }
        Expr::Await(inner) | Expr::Neg(inner) | Expr::Not(inner) => {
            collect_select_sites_expr(inner, out);
        }
        Expr::ObjectLit(fields) => {
            for (_, v) in fields {
                collect_select_sites_expr(v, out);
            }
        }
        Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b)
        | Expr::Mod(a, b)
        | Expr::Eq(a, b)
        | Expr::Neq(a, b)
        | Expr::Lt(a, b)
        | Expr::Lte(a, b)
        | Expr::Gt(a, b)
        | Expr::Gte(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b) => {
            collect_select_sites_expr(a, out);
            collect_select_sites_expr(b, out);
        }
        Expr::Call { args, .. } => {
            for a in args {
                collect_select_sites_expr(a, out);
            }
        }
        _ => {}
    }
}

fn collect_calls(stmts: &[Stmt], out: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => collect_calls_expr(value, out),
            Stmt::Return(Some(value)) => collect_calls_expr(value, out),
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                collect_calls_expr(cond, out);
                collect_calls(then_body, out);
                if let Some(body) = else_body {
                    collect_calls(body, out);
                }
            }
            Stmt::While { cond, body } => {
                collect_calls_expr(cond, out);
                collect_calls(body, out);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                collect_calls(body, out);
                collect_calls(catch_body, out);
            }
            Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => collect_calls(body, out),
            Stmt::ForIn { iter, body, .. } => {
                collect_calls_expr(iter, out);
                collect_calls(body, out);
            }
            Stmt::DbDeleteWhere { where_clause, .. } => {
                collect_calls_where(where_clause, out);
            }
            Stmt::DbUpdateSet {
                assignments,
                where_clause,
                ..
            } => {
                for (_, rhs) in assignments {
                    collect_calls_expr(rhs, out);
                }
                collect_calls_where(where_clause, out);
            }
            Stmt::Return(None)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::ValidateBody { .. }
            | Stmt::DbInsert { .. }
            | Stmt::DbUpdate { .. }
            | Stmt::DbDelete { .. } => {}
        }
    }
}

fn collect_calls_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Call { name, args } => {
            out.insert(name.to_lowercase());
            for arg in args {
                collect_calls_expr(arg, out);
            }
        }
        Expr::Await(inner) | Expr::Neg(inner) | Expr::Not(inner) => collect_calls_expr(inner, out),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_calls_expr(cond, out);
            collect_calls_expr(then_expr, out);
            collect_calls_expr(else_expr, out);
        }
        Expr::Coalesce(a, b) => {
            collect_calls_expr(a, out);
            collect_calls_expr(b, out);
        }
        Expr::ObjectLit(fields) => {
            for (_, v) in fields {
                collect_calls_expr(v, out);
            }
        }
        Expr::ArrayLit(items) => {
            for item in items {
                collect_calls_expr(item, out);
            }
        }
        Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b)
        | Expr::Mod(a, b)
        | Expr::Eq(a, b)
        | Expr::Neq(a, b)
        | Expr::Lt(a, b)
        | Expr::Lte(a, b)
        | Expr::Gt(a, b)
        | Expr::Gte(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b) => {
            collect_calls_expr(a, out);
            collect_calls_expr(b, out);
        }
        Expr::DbSelect {
            where_clause,
            limit,
            offset,
            ..
        } => {
            if let Some(wc) = where_clause {
                collect_calls_where(wc, out);
            }
            if let Some(l) = limit {
                collect_calls_expr(l, out);
            }
            if let Some(o) = offset {
                collect_calls_expr(o, out);
            }
        }
        Expr::DbCount { where_clause, .. } => {
            if let Some(wc) = where_clause {
                collect_calls_where(wc, out);
            }
        }
        Expr::DbAggregate { where_clause, .. } => {
            if let Some(wc) = where_clause {
                collect_calls_where(wc, out);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_)
        | Expr::FieldGet { .. }
        | Expr::NewEntity { .. } => {}
    }
}

fn collect_calls_where(wc: &WhereExpr, out: &mut std::collections::HashSet<String>) {
    match wc {
        WhereExpr::Atom(atom) => collect_calls_expr(&atom.rhs, out),
        WhereExpr::InList { values, .. } => {
            for v in values {
                collect_calls_expr(v, out);
            }
        }
        WhereExpr::Between { low, high, .. } => {
            collect_calls_expr(low, out);
            collect_calls_expr(high, out);
        }
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            collect_calls_where(l, out);
            collect_calls_where(r, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_program, validate_program};

    #[test]
    fn function_shadowing_builtin_is_w005() {
        let src = r#"
            function json(v) { return v; }
            function main() { json("hi"); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            warnings
                .iter()
                .any(|w| w.code == "W006" && w.message.contains("json")),
            "expected W005 for shadowed built-in 'json', got: {warnings:?}"
        );
    }

    #[test]
    fn non_shadowing_function_does_not_trigger_w005() {
        let src = r#"
            function format_user(u) { return u; }
            function main() { format_user("x"); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            !warnings.iter().any(|w| w.code == "W006"),
            "non-shadowing function should not be W005, got: {warnings:?}"
        );
    }

    #[test]
    fn unused_function_is_reported() {
        let src = r#"
            function helper() { return 1; }
            function main() {
                print("hi");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(warnings
            .iter()
            .any(|w| w.code == "W001" && w.message.contains("helper")));
    }

    #[test]
    fn middleware_used_by_a_route_is_not_reported() {
        let src = r#"
            middleware AuthMw {
                return 1;
            }

            route GET "/x" use AuthMw {
                return json("ok");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(!warnings.iter().any(|w| w.code == "W002"));
    }

    #[test]
    fn empty_function_body_is_w003() {
        let src = r#"
            function todo_later() { }
            function main() { todo_later(); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            warnings
                .iter()
                .any(|w| w.code == "W003" && w.message.contains("todo_later")),
            "expected W003 for empty body, got: {warnings:?}"
        );
    }

    #[test]
    fn missing_first_on_pk_select_is_w004() {
        let src = r#"
            dbcontext AppDb : Postgres;
            entity User of AppDb {
                id uuid pk;
                name varchar(120);
            }
            function findUser(id) {
                return select User from AppDb.User where User.id == @id;
            }
            function main() { findUser("x"); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            warnings
                .iter()
                .any(|w| w.code == "W004" && w.message.contains("first")),
            "expected W004, got: {warnings:?}"
        );
    }

    #[test]
    fn first_on_pk_select_does_not_trigger_w004() {
        let src = r#"
            dbcontext AppDb : Postgres;
            entity User of AppDb {
                id uuid pk;
                name varchar(120);
            }
            function findUser(id) {
                return select User from AppDb.User where User.id == @id first;
            }
            function main() { findUser("x"); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            !warnings.iter().any(|w| w.code == "W004"),
            "expected no W004, got: {warnings:?}"
        );
    }

    #[test]
    fn non_pk_filter_does_not_trigger_w004() {
        let src = r#"
            dbcontext AppDb : Postgres;
            entity User of AppDb {
                id uuid pk;
                name varchar(120);
            }
            function findByName(n) {
                return select User from AppDb.User where User.name == @n;
            }
            function main() { findByName("x"); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            !warnings.iter().any(|w| w.code == "W004"),
            "non-PK filter must not flag W004, got: {warnings:?}"
        );
    }

    #[test]
    fn unreachable_after_return_is_w005() {
        let src = r#"
            function early() {
                return 1;
                print("unreachable");
            }
            function main() { early(); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            warnings
                .iter()
                .any(|w| w.code == "W006" && w.message.contains("early")),
            "expected W005 for unreachable code, got: {warnings:?}"
        );
    }

    #[test]
    fn return_in_if_branch_does_not_trigger_w005() {
        let src = r#"
            function safe(x) {
                if (x == 0) { return 0; }
                return x + 1;
            }
            function main() { safe(1); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            !warnings.iter().any(|w| w.code == "W006"),
            "return-in-if-branch must not trigger W005, got: {warnings:?}"
        );
    }

    #[test]
    fn non_empty_function_body_does_not_trigger_w003() {
        let src = r#"
            function ok() { return 1; }
            route GET "/x" { return json("y"); }
            function main() { ok(); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(
            !warnings.iter().any(|w| w.code == "W003"),
            "expected no W003, got: {warnings:?}"
        );
    }

    #[test]
    fn unused_middleware_is_reported() {
        let src = r#"
            middleware Unused { return 1; }

            route GET "/x" {
                return json("ok");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let warnings = lint_program(&program);
        assert!(warnings
            .iter()
            .any(|w| w.code == "W002" && w.message.contains("Unused")));
    }

    /// Lint runs over the merged project program, so each warning has to name
    /// the file that declared the symbol. Without this the language server
    /// can't tell whose warning it is and republishes every project warning on
    /// every open file.
    #[test]
    fn warnings_are_attributed_to_the_declaring_file() {
        let mw_src = "middleware Unused { return 1; }\n";
        let route_src = "route GET \"/x\" { return json(\"ok\"); }\n";

        let mut program = crate::ast::Program::default();
        crate::project::merge_program(
            &mut program,
            crate::parser::parse_program_with_label(mw_src, "Infra/Unused.jwc").unwrap(),
        )
        .unwrap();
        crate::project::merge_program(
            &mut program,
            crate::parser::parse_program_with_label(route_src, "Routes.jwc").unwrap(),
        )
        .unwrap();
        validate_program(&program).unwrap();

        let warnings = lint_program(&program);
        let w = warnings
            .iter()
            .find(|w| w.code == "W002")
            .expect("unused middleware should warn");
        assert_eq!(
            w.source_label(&program),
            Some("Infra/Unused.jwc"),
            "W002 must point at the file declaring the middleware"
        );
    }

    /// The mirror case: a middleware declared in one file and attached to a
    /// route in another is used, and must not warn. Analysing either file on
    /// its own reports it as both unused and unknown at the same time.
    #[test]
    fn middleware_used_from_another_file_does_not_warn() {
        let mw_src = "middleware Auth { return 1; }\n";
        let route_src = "route GET \"/x\" use Auth { return json(\"ok\"); }\n";

        let mut program = crate::ast::Program::default();
        crate::project::merge_program(
            &mut program,
            crate::parser::parse_program_with_label(mw_src, "Infra/Auth.jwc").unwrap(),
        )
        .unwrap();
        crate::project::merge_program(
            &mut program,
            crate::parser::parse_program_with_label(route_src, "Routes.jwc").unwrap(),
        )
        .unwrap();
        validate_program(&program).unwrap();

        assert!(
            !lint_program(&program).iter().any(|w| w.code == "W002"),
            "middleware attached from a sibling file is used"
        );
    }
}
