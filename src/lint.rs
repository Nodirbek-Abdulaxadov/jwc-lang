use std::collections::HashSet;

use crate::ast::{Expr, Program, Stmt, WhereExpr};

#[derive(Debug, Clone)]
pub struct LintWarning {
    pub code: &'static str,
    pub message: String,
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
                message: format!(
                    "function '{}' is defined but never called",
                    function.name
                ),
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
        if !used_middlewares.contains(&mw.name.to_lowercase()) {
            warnings.push(LintWarning {
                code: "W002",
                message: format!(
                    "middleware '{}' is declared but never attached to a route",
                    mw.name
                ),
            });
        }
    }

    warnings
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
            Stmt::Transaction { body } => collect_calls(body, out),
            Stmt::DbDeleteWhere { where_clause, .. } => {
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
        Expr::ObjectLit(fields) => {
            for (_, v) in fields {
                collect_calls_expr(v, out);
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
        assert!(warnings.iter().any(|w| w.code == "W001" && w.message.contains("helper")));
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
        assert!(warnings.iter().any(|w| w.code == "W002" && w.message.contains("Unused")));
    }
}
