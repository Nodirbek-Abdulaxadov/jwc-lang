//! Gradual static type checker (Sprint 3 #15 — Phase 3 scaffolding).
//!
//! This pass runs after [`crate::parser::validate_program`] and before any
//! runtime path (`runner::run_main`, `native_build`). It only fires on
//! function signatures that the author already annotated — unannotated
//! params and unannotated return types stay dynamic. That property is
//! what makes the rollout safe: existing programs that don't add types
//! can't be broken by enabling the checker.
//!
//! ## Scope of this first cut
//!
//! Three checks are wired in (the rest of the gradual surface lands in
//! follow-up sprints, per `PRODUCTION_READINESS_PLAN.md` Phase 3):
//!
//! 1. **E018 — function return type vs body returns.** If a
//!    [`FunctionDecl::return_type`] is declared, every `return <expr>;`
//!    in the body must produce a statically inferable value compatible
//!    with the declared type. Inference is conservative: literals match
//!    by shape (`Int → int`, `Float → double`, `Str → string`,
//!    `Bool → bool`, `Array → array`, `Record → object`), calls to
//!    other typed functions match by their declared return type, and
//!    everything else is `dynamic` and skipped (gradual property).
//!
//! 2. **E019 — call-site arity.** When `Expr::Call { name, args }`
//!    resolves to a known user function (not a built-in / not variadic),
//!    `args.len()` must equal the declared parameter count.
//!
//! 3. **E020 — call-site argument type.** For each typed parameter, the
//!    matching argument's static type (if any) must be compatible with
//!    the parameter's declared type. Variable reads, field accesses,
//!    and any other not-yet-typed shape are inferred as `dynamic` and
//!    skipped — same gradual rule.
//!
//! ## Conformance with existing fixtures
//!
//! `examples/testapp/src/services/BrandService.jwc` and friends rely on
//! the gradual property heavily — `getBrandById(id: int)` is called with
//! `@id` (a `Var`) or `data.id` (`FieldGet`); both infer to `dynamic`
//! and the checker waves them through. Don't tighten inference without
//! re-running both the lib suite AND the `examples/testapp` end-to-end
//! pipeline (`cargo run -- check examples/testapp/main.jwc`).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::ast::{Expr, FunctionDecl, Program, Stmt};
use crate::builtins;

/// One of the lattice points the checker can assign to an expression.
/// Anything we can't analyse statically lands in [`Ty::Dynamic`] and the
/// checker silently accepts it — that's the gradual property.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ty {
    Int,
    Double,
    String,
    Bool,
    Array,
    Object,
    /// The user's declared type by name (e.g. `User`, `BrandEntity`,
    /// `BrandCreateRequest`). The checker doesn't yet know the structure
    /// of these — they only compare equal to themselves.
    Named(String),
    /// Couldn't infer or the expression's shape is intentionally untyped.
    /// All compatibility checks against `Dynamic` succeed.
    Dynamic,
}

impl Ty {
    /// Normalise a user-written type name (case-insensitive, trimmed) to
    /// a [`Ty`]. Recognised primitive aliases follow the JWC reference:
    /// `int`, `long`, `bool`, `boolean`, `string`, `text`, `double`,
    /// `float`, `decimal`, `array`, `list`, `object`, `json`. Anything
    /// else becomes [`Ty::Named`] preserving the original casing.
    fn parse_declared(name: &str) -> Ty {
        let trimmed = name.trim();
        // Strip nullable suffix `?` — the checker treats `int?` exactly
        // like `int` at this stage (presence/absence is enforced
        // elsewhere by `check_typed_value` at runtime).
        let bare = trimmed.trim_end_matches('?');
        match bare.to_ascii_lowercase().as_str() {
            "int" | "long" | "integer" | "i64" | "i32" => Ty::Int,
            "bool" | "boolean" => Ty::Bool,
            "string" | "str" | "text" => Ty::String,
            "double" | "float" | "decimal" | "number" => Ty::Double,
            "array" | "list" => Ty::Array,
            "object" | "json" | "map" => Ty::Object,
            _ => Ty::Named(bare.to_string()),
        }
    }

    /// Human-readable label for diagnostics.
    fn label(&self) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Double => "double".into(),
            Ty::String => "string".into(),
            Ty::Bool => "bool".into(),
            Ty::Array => "array".into(),
            Ty::Object => "object".into(),
            Ty::Named(n) => n.clone(),
            Ty::Dynamic => "dynamic".into(),
        }
    }

    /// True when `value` may flow into a slot declared as `slot`. The
    /// relation is intentionally permissive: `Dynamic` is compatible
    /// with everything (gradual), `Int` is compatible with `Double`
    /// (widening), and named types only match themselves
    /// (case-insensitive).
    fn assignable_to(&self, slot: &Ty) -> bool {
        match (self, slot) {
            (Ty::Dynamic, _) | (_, Ty::Dynamic) => true,
            (a, b) if a == b => true,
            // Widening: `int` → `double` is fine; an `int` literal is a
            // valid numeric argument to a `double` slot.
            (Ty::Int, Ty::Double) => true,
            (Ty::Named(a), Ty::Named(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

/// Lookup table of user-function FQN (lowercased simple name, namespaces
/// flattened) → (param count, param types, return type). Only functions
/// the checker can reason about are recorded — anonymous params keep
/// `Ty::Dynamic` so arg-type checks no-op for them.
struct FnTable {
    /// Map from lowercased simple name (without namespace) to a vector
    /// of candidates. The checker is namespace-agnostic at the call site
    /// (matches the runtime's resolver behaviour for short names), so we
    /// only enforce arity / arg types when EXACTLY one candidate exists.
    /// Multiple candidates ⇒ ambiguity ⇒ skip (gradual).
    by_short: HashMap<String, Vec<FnSig>>,
}

#[derive(Clone)]
struct FnSig {
    params: Vec<Ty>,
    /// Declared return type if any.
    ret: Option<Ty>,
}

impl FnTable {
    fn build(program: &Program) -> Self {
        let mut by_short: HashMap<String, Vec<FnSig>> = HashMap::new();
        for f in &program.functions {
            let params = f
                .params
                .iter()
                .map(|p| p.ty.as_deref().map(Ty::parse_declared).unwrap_or(Ty::Dynamic))
                .collect::<Vec<_>>();
            let ret = f.return_type.as_deref().map(Ty::parse_declared);
            by_short
                .entry(f.name.to_ascii_lowercase())
                .or_default()
                .push(FnSig { params, ret });
        }
        FnTable { by_short }
    }

    /// Lookup the unique signature for `name`. Returns `None` when the
    /// name is a built-in, when no user function matches, or when
    /// multiple overloads share the name (ambiguous → skip).
    fn unique(&self, name: &str) -> Option<&FnSig> {
        if builtins::is_builtin(name) {
            return None;
        }
        let candidates = self.by_short.get(&name.to_ascii_lowercase())?;
        if candidates.len() == 1 {
            Some(&candidates[0])
        } else {
            None
        }
    }
}

/// Statically infer the type of `expr` in isolation. Returns `Ty::Dynamic`
/// for any shape the checker doesn't model — locals, field accesses,
/// arithmetic, DB selects, etc. Callers MUST treat `Dynamic` as
/// "assume the user knows best" (the gradual property).
fn infer_expr(expr: &Expr, fns: &FnTable) -> Ty {
    match expr {
        Expr::Int(_) => Ty::Int,
        Expr::Float(_) => Ty::Double,
        Expr::Str(_) => Ty::String,
        Expr::Bool(_) => Ty::Bool,
        Expr::Null => Ty::Dynamic,
        Expr::ArrayLit(_) => Ty::Array,
        Expr::ObjectLit(_) => Ty::Object,
        Expr::NewEntity { entity } => Ty::Named(entity.clone()),
        Expr::Call { name, .. } => {
            // Calls to typed user functions carry their declared return
            // type; built-ins and untyped functions stay dynamic so we
            // don't accidentally over-constrain.
            if let Some(sig) = fns.unique(name) {
                sig.ret.clone().unwrap_or(Ty::Dynamic)
            } else {
                Ty::Dynamic
            }
        }
        Expr::Await(inner) => infer_expr(inner, fns),
        // Arithmetic / comparisons / DB ops — could be tightened later;
        // for now stay dynamic so existing untyped programs don't trip.
        _ => Ty::Dynamic,
    }
}

/// Walk every statement in `body` and yield each `Stmt::Return(Some(_))`
/// expression. Recurses through control-flow blocks so a return nested
/// in an `if`/`for`/`while`/`try`/`transaction`/`savepoint` is still
/// checked. Bare `return;` (no value) is intentionally NOT yielded — it
/// produces `null` at runtime and that's compatible with every declared
/// return type at this stage (gradual: `return;` is "I have nothing
/// useful to return", not "I lied about my return type").
fn collect_returns<'a>(body: &'a [Stmt], out: &mut Vec<&'a Expr>) {
    for stmt in body {
        match stmt {
            Stmt::Return(Some(value)) => out.push(value),
            Stmt::Return(None) => {}
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                collect_returns(then_body, out);
                if let Some(eb) = else_body {
                    collect_returns(eb, out);
                }
            }
            Stmt::While { body, .. }
            | Stmt::ForIn { body, .. }
            | Stmt::Transaction { body }
            | Stmt::Savepoint { body, .. } => {
                collect_returns(body, out);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                collect_returns(body, out);
                collect_returns(catch_body, out);
            }
            _ => {}
        }
    }
}

/// Check function `f` against the rules described in the module docs.
fn check_function(f: &FunctionDecl, fns: &FnTable) -> Result<()> {
    // ── E018: return-type vs body returns ────────────────────────────
    if let Some(declared) = f.return_type.as_deref().map(Ty::parse_declared) {
        let mut returns = Vec::new();
        collect_returns(&f.body, &mut returns);
        for ret_expr in returns {
            let actual = infer_expr(ret_expr, fns);
            if !actual.assignable_to(&declared) {
                bail!(
                    "error[E018]: return type mismatch in function '{}': declared {}, got {}",
                    f.name,
                    declared.label(),
                    actual.label()
                );
            }
        }
    }

    // ── E019 / E020: walk every call site in the body ────────────────
    let mut calls = Vec::new();
    collect_calls(&f.body, &mut calls);
    for expr in calls {
        let Expr::Call { name, args } = expr else {
            continue;
        };
        let Some(sig) = fns.unique(name) else { continue };

        // E019: arity must match exactly. User functions in JWC don't
        // support defaults or rest params today, so any mismatch is a
        // hard error.
        if args.len() != sig.params.len() {
            bail!(
                "error[E019]: wrong number of arguments to '{}': expected {}, got {}",
                name,
                sig.params.len(),
                args.len()
            );
        }

        // E020: typed parameters must receive a compatible argument.
        // Untyped params (`Ty::Dynamic`) accept anything — the function
        // author opted out of the check for that slot.
        for (idx, (arg, param_ty)) in args.iter().zip(sig.params.iter()).enumerate() {
            if matches!(param_ty, Ty::Dynamic) {
                continue;
            }
            let arg_ty = infer_expr(arg, fns);
            if !arg_ty.assignable_to(param_ty) {
                bail!(
                    "error[E020]: argument {} to '{}' has wrong type: expected {}, got {}",
                    idx + 1,
                    name,
                    param_ty.label(),
                    arg_ty.label()
                );
            }
        }
    }

    Ok(())
}

/// Walk every statement in `body` and yield each `Expr::Call` it
/// contains, including calls embedded in `let` / `assign` / `print` /
/// `if`-conditions / `return` values / nested control flow. The
/// returned references borrow the original AST.
fn collect_calls<'a>(body: &'a [Stmt], out: &mut Vec<&'a Expr>) {
    for stmt in body {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::FieldAssign { value, .. }
            | Stmt::Print(value)
            | Stmt::Expr(value) => collect_calls_in_expr(value, out),
            Stmt::Return(Some(value)) => collect_calls_in_expr(value, out),
            Stmt::Return(None) => {}
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                collect_calls_in_expr(cond, out);
                collect_calls(then_body, out);
                if let Some(eb) = else_body {
                    collect_calls(eb, out);
                }
            }
            Stmt::While { cond, body } => {
                collect_calls_in_expr(cond, out);
                collect_calls(body, out);
            }
            Stmt::ForIn { iter, body, .. } => {
                collect_calls_in_expr(iter, out);
                collect_calls(body, out);
            }
            Stmt::Try {
                body, catch_body, ..
            } => {
                collect_calls(body, out);
                collect_calls(catch_body, out);
            }
            Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
                collect_calls(body, out);
            }
            _ => {}
        }
    }
}

/// Recursive call walker for a single expression. Records every nested
/// [`Expr::Call`] including the outer one.
fn collect_calls_in_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if matches!(expr, Expr::Call { .. }) {
        out.push(expr);
    }
    match expr {
        Expr::Call { args, .. } => {
            for a in args {
                collect_calls_in_expr(a, out);
            }
        }
        Expr::Await(inner) | Expr::Not(inner) | Expr::Neg(inner) => {
            collect_calls_in_expr(inner, out);
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
            collect_calls_in_expr(a, out);
            collect_calls_in_expr(b, out);
        }
        Expr::ObjectLit(entries) => {
            for (_, v) in entries {
                collect_calls_in_expr(v, out);
            }
        }
        Expr::ArrayLit(items) => {
            for v in items {
                collect_calls_in_expr(v, out);
            }
        }
        _ => {}
    }
}

/// Entry point: type-check every function in `program`. Run AFTER
/// [`crate::parser::validate_program`] so we can rely on its
/// invariants (e.g. no duplicate names within a namespace).
///
/// The pass is intentionally short-circuiting — the first violation is
/// reported and the rest of the program is left unchecked. That matches
/// the validator's existing behaviour and keeps error messages high-
/// signal during the gradual rollout.
pub fn typecheck_program(program: &Program) -> Result<()> {
    let fns = FnTable::build(program);

    // Functions defined at the top level.
    for f in &program.functions {
        check_function(f, &fns)?;
    }

    // Route handlers and middleware bodies can also contain calls; the
    // arity / arg-type checks apply there too. Return-type checks are
    // skipped (routes don't have a declared return type at this stage).
    for route in &program.routes {
        let mut calls = Vec::new();
        collect_calls(&route.body, &mut calls);
        for expr in calls {
            check_call(expr, &fns)?;
        }
    }
    for mw in &program.middlewares {
        let mut calls = Vec::new();
        collect_calls(&mw.body, &mut calls);
        if let Some(after) = &mw.after_body {
            collect_calls(after, &mut calls);
        }
        for expr in calls {
            check_call(expr, &fns)?;
        }
    }

    Ok(())
}

/// E019 + E020 check for a single call expression. Pulled out so route
/// + middleware bodies can share it with the function-body walker.
fn check_call(expr: &Expr, fns: &FnTable) -> Result<()> {
    let Expr::Call { name, args } = expr else {
        return Ok(());
    };
    let Some(sig) = fns.unique(name) else {
        return Ok(());
    };

    if args.len() != sig.params.len() {
        bail!(
            "error[E019]: wrong number of arguments to '{}': expected {}, got {}",
            name,
            sig.params.len(),
            args.len()
        );
    }
    for (idx, (arg, param_ty)) in args.iter().zip(sig.params.iter()).enumerate() {
        if matches!(param_ty, Ty::Dynamic) {
            continue;
        }
        let arg_ty = infer_expr(arg, fns);
        if !arg_ty.assignable_to(param_ty) {
            bail!(
                "error[E020]: argument {} to '{}' has wrong type: expected {}, got {}",
                idx + 1,
                name,
                param_ty.label(),
                arg_ty.label()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_program, validate_program};

    fn check(src: &str) -> Result<()> {
        let program = parse_program(src).expect("parse");
        validate_program(&program).expect("validate");
        typecheck_program(&program)
    }

    #[test]
    fn return_type_mismatch_int_vs_string_rejected() {
        let src = r#"
            function bad(): int {
                return "hello";
            }
            function main() {}
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E018"), "missing E018 code: {err}");
        assert!(err.contains("bad"), "missing fn name: {err}");
        assert!(err.contains("int"), "missing declared type: {err}");
        assert!(err.contains("string"), "missing actual type: {err}");
    }

    #[test]
    fn return_type_match_ok() {
        let src = r#"
            function answer(): int {
                return 42;
            }
            function main() {}
        "#;
        check(src).expect("clean program must typecheck");
    }

    #[test]
    fn arity_mismatch_raises_clear_error() {
        let src = r#"
            function greet(name: string): string {
                return name;
            }
            function main() {
                let s = greet("a", "b");
                print(s);
            }
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E019"), "missing E019 code: {err}");
        assert!(err.contains("greet"), "missing callee name: {err}");
        assert!(err.contains("expected 1"), "missing expected count: {err}");
        assert!(err.contains("got 2"), "missing actual count: {err}");
    }

    #[test]
    fn arg_type_mismatch_int_arg_to_string_param_rejected() {
        let src = r#"
            function greet(name: string): string {
                return name;
            }
            function main() {
                let s = greet(42);
                print(s);
            }
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E020"), "missing E020 code: {err}");
        assert!(err.contains("greet"), "missing callee name: {err}");
        assert!(err.contains("string"), "missing expected ty: {err}");
        assert!(err.contains("int"), "missing actual ty: {err}");
    }

    #[test]
    fn unannotated_function_skips_checks() {
        // No return type, no param types — must not raise even though
        // the body returns multiple shapes and the caller passes any
        // number of args.
        let src = r#"
            function dynamic_fn(x, y) {
                if (x == 1) { return "hello"; }
                if (x == 2) { return 42; }
                return [1, 2, 3];
            }
            function main() {
                let a = dynamic_fn(1, 2);
                let b = dynamic_fn("string", true);
                print(a);
                print(b);
            }
        "#;
        check(src).expect("unannotated calls must stay dynamic");
    }

    #[test]
    fn array_literal_returned_from_string_typed_function_rejected() {
        let src = r#"
            function whoops(): string {
                return [1, 2, 3];
            }
            function main() {}
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E018"), "missing E018: {err}");
        assert!(err.contains("string"), "missing declared: {err}");
        assert!(err.contains("array"), "missing inferred: {err}");
    }

    // ── extra coverage ───────────────────────────────────────────────

    #[test]
    fn dynamic_arg_via_variable_is_allowed() {
        // `name` is a Var → infers Dynamic → typed `string` param
        // accepts it. This is the gradual property the testapp fixture
        // relies on.
        let src = r#"
            function shout(name: string): string {
                return name;
            }
            function main() {
                let n = "loud";
                let r = shout(n);
                print(r);
            }
        "#;
        check(src).expect("dynamic arg must flow into typed param");
    }

    #[test]
    fn typed_call_chain_return_type_propagates() {
        // `inner()` declares `: int`, so `return inner();` from an
        // `: int`-typed outer function should typecheck — the return
        // type of a typed call is known and matches.
        let src = r#"
            function inner(): int {
                return 1;
            }
            function outer(): int {
                return inner();
            }
            function main() {}
        "#;
        check(src).expect("typed call should propagate its return type");
    }

    #[test]
    fn typed_call_chain_return_type_mismatch_rejected() {
        let src = r#"
            function inner(): string {
                return "x";
            }
            function outer(): int {
                return inner();
            }
            function main() {}
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E018"), "missing E018: {err}");
        assert!(err.contains("outer"), "wrong function flagged: {err}");
    }

    #[test]
    fn builtin_call_is_not_checked() {
        // `upper` is a string built-in. If the typechecker mistakenly
        // looked it up in the user-function table it'd either produce
        // an arity false positive or stamp the result with the wrong
        // type — neither should happen.
        let src = r#"
            function main() {
                let s = upper("hello");
                print(s);
            }
        "#;
        check(src).expect("builtin arity must not be enforced here");
    }

    #[test]
    fn nested_return_inside_if_is_checked() {
        let src = r#"
            function bad(flag: bool): int {
                if (flag) {
                    return "wrong";
                }
                return 1;
            }
            function main() {}
        "#;
        let err = check(src).unwrap_err().to_string();
        assert!(err.contains("E018"), "nested return not walked: {err}");
    }

    #[test]
    fn int_to_double_widening_allowed() {
        // Numeric literal `1` flowing into a `double` slot is the
        // intended ergonomic.
        let src = r#"
            function pi_ish(): double {
                return 3;
            }
            function main() {}
        "#;
        check(src).expect("int → double widening must be allowed");
    }

    #[test]
    fn entity_return_type_matches_new_entity() {
        // `new User()` infers to Named("User"); a function declared
        // `: User` must accept it. Names match case-insensitively to
        // mirror the runtime's resolver.
        let src = r#"
            class User { name string; }
            function make(): User {
                return new User();
            }
            function main() {}
        "#;
        check(src).expect("new EntityName matches declared entity return");
    }
}
