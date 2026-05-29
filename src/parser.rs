use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{
    AggregateKind, DbContextDecl, DbOrderBy, DbWhere, ErrorHandlerDecl, Expr, FieldDecl,
    FieldReference, FunctionDecl, MiddlewareDecl, ModelDecl, ModelKind, NavigationField,
    NavigationKind, OnDeleteAction, Program, RouteDecl, RouteProtocol, SortDir, Stmt, TypeSpec,
    TypedParam, ValidateField, ValidateRule, Visibility, WhereExpr,
};

fn visibility_from(is_pub: bool) -> Visibility {
    if is_pub {
        Visibility::Public
    } else {
        Visibility::Private
    }
}
use crate::diag::SourceMap;
use crate::lexer::{Keyword, Lexer, TemplatePart, Token, TokenKind};

pub fn parse_program(source: &str) -> Result<Program> {
    let mut parser = Parser::new(source)?;
    parser.parse_program()
}

/// FQN helper used by validate_program. Identical shape to `runner::fqn_key`
/// but duplicated here to avoid a dependency from parser → runner.
fn ns_fqn(namespace: &[String], name: &str) -> String {
    if namespace.is_empty() {
        name.to_lowercase()
    } else {
        let ns = namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        format!("{}.{}", ns, name.to_lowercase())
    }
}

pub fn validate_program(program: &Program) -> Result<()> {
    let mut ctx_names = HashSet::new();
    let mut ctx_drivers: HashMap<String, String> = HashMap::new();
    for ctx in &program.dbcontexts {
        let key = ns_fqn(&ctx.namespace, &ctx.name);
        if !ctx_names.insert(key) {
            bail!("Duplicate dbcontext name: {}", ctx.name);
        }
        if ctx.driver.trim().is_empty() {
            bail!("dbcontext '{}' has empty driver", ctx.name);
        }
        ctx_drivers.insert(ctx.name.to_lowercase(), ctx.driver.to_lowercase());
    }

    let mut model_names = HashSet::new();
    let mut entity_names = HashSet::new();
    let mut entity_contexts: HashMap<String, Option<String>> = HashMap::new();
    let mut db_tables: HashSet<(String, String)> = HashSet::new();
    let mut entity_fields_by_table: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut entity_navigations: HashMap<String, HashMap<String, NavigationField>> = HashMap::new();
    for model in &program.models {
        let model_key = ns_fqn(&model.namespace, &model.name);
        if !model_names.insert(model_key) {
            bail!("Duplicate model name: {}", model.name);
        }

        if model.kind != ModelKind::Entity {
            continue;
        }
        let key = ns_fqn(&model.namespace, &model.name);
        if !entity_names.insert(key) {
            bail!("Duplicate entity name: {}", model.name);
        }

        let resolved_context = resolve_entity_context_name(program, model, &ctx_names)?;
        let resolved_context_lc = resolved_context.as_ref().map(|v| v.to_lowercase());
        entity_contexts.insert(model.name.to_lowercase(), resolved_context_lc.clone());
        if let Some(ctx_name) = resolved_context_lc {
            db_tables.insert((ctx_name.clone(), model.name.to_lowercase()));
            db_tables.insert((ctx_name.clone(), to_snake_case(&model.name).to_lowercase()));

            let fields: Vec<String> = model.fields.iter().map(|f| f.name.to_lowercase()).collect();
            entity_fields_by_table.insert(
                (ctx_name.clone(), model.name.to_lowercase()),
                fields.clone(),
            );
            entity_fields_by_table.insert(
                (ctx_name, to_snake_case(&model.name).to_lowercase()),
                fields,
            );
        }

        let mut field_names = HashSet::new();
        let resolved_driver = resolve_entity_driver(program, model, &ctx_drivers)?;
        for field in &model.fields {
            let field_key = field.name.to_lowercase();
            if !field_names.insert(field_key) {
                bail!(
                    "Duplicate field '{}' in entity '{}'",
                    field.name,
                    model.name
                );
            }

            validate_type_spec_for_driver(&field.ty, &resolved_driver)
                .map_err(|err| anyhow!("Entity '{}', field '{}': {err}", model.name, field.name))?;

            if field.is_auto_increment {
                let ty_lc = field.ty.name.to_ascii_lowercase();
                if ty_lc != "int" && ty_lc != "integer" && ty_lc != "bigint" {
                    bail!(
                        "Entity '{}', field '{}': autoincrement is only valid on int / bigint types (got '{}')",
                        model.name,
                        field.name,
                        field.ty.name
                    );
                }
            }
        }

        if !model.navigations.is_empty() {
            let mut nav_names: HashSet<String> = HashSet::new();
            let mut nav_map: HashMap<String, NavigationField> = HashMap::new();
            for nav in &model.navigations {
                let key = nav.name.to_lowercase();
                if !nav_names.insert(key.clone()) {
                    bail!(
                        "Entity '{}': duplicate navigation '{}'",
                        model.name,
                        nav.name
                    );
                }
                nav_map.insert(key, nav.clone());
            }
            entity_navigations.insert(model.name.to_lowercase(), nav_map);
        }
    }

    // After all entities are registered: resolve each navigation's target.
    for model in &program.models {
        if model.kind != ModelKind::Entity {
            continue;
        }
        for nav in &model.navigations {
            let target_key = nav.target_entity.to_lowercase();
            let target = program
                .models
                .iter()
                .find(|m| m.kind == ModelKind::Entity && m.name.to_lowercase() == target_key)
                .ok_or_else(|| {
                    anyhow!(
                        "Entity '{}' navigation '{}' references unknown entity '{}'",
                        model.name,
                        nav.name,
                        nav.target_entity
                    )
                })?;
            let field_key = nav.target_field.to_lowercase();
            if !target
                .fields
                .iter()
                .any(|f| f.name.to_lowercase() == field_key)
            {
                bail!(
                    "Entity '{}' navigation '{}' references unknown column '{}.{}'",
                    model.name,
                    nav.name,
                    nav.target_entity,
                    nav.target_field
                );
            }
        }
    }

    // Foreign key references — verify target entity + column exist after all
    // entities have been registered.
    for model in &program.models {
        if model.kind != ModelKind::Entity {
            continue;
        }
        for field in &model.fields {
            let Some(reference) = &field.references else {
                continue;
            };
            let target_key = reference.entity.to_lowercase();
            let target = program
                .models
                .iter()
                .find(|m| m.kind == ModelKind::Entity && m.name.to_lowercase() == target_key)
                .ok_or_else(|| {
                    anyhow!(
                        "Entity '{}' field '{}' references unknown entity '{}'",
                        model.name,
                        field.name,
                        reference.entity
                    )
                })?;

            let column_key = reference.column.to_lowercase();
            if !target
                .fields
                .iter()
                .any(|f| f.name.to_lowercase() == column_key)
            {
                bail!(
                    "Entity '{}' field '{}' references unknown column '{}.{}'",
                    model.name,
                    field.name,
                    reference.entity,
                    reference.column
                );
            }
        }
    }

    let mut fn_names = HashSet::new();
    for function in &program.functions {
        let key = ns_fqn(&function.namespace, &function.name);
        if !fn_names.insert(key) {
            bail!("Duplicate function name: {}", function.name);
        }

        let mut param_names = HashSet::new();
        for param in &function.params {
            let param_key = param.name.to_lowercase();
            if !param_names.insert(param_key) {
                bail!(
                    "Function '{}': duplicate parameter '{}'",
                    function.name,
                    param.name
                );
            }
        }
    }

    let mut route_keys = HashSet::new();
    for route in &program.routes {
        let method = route.method.to_ascii_uppercase();
        if !matches!(
            method.as_str(),
            "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "WS" | "SSE"
        ) {
            bail!("Unsupported route method: {}", route.method);
        }

        let key = format!("{} {}", method, route.path);
        if !route_keys.insert(key) {
            bail!("error[E005]: Duplicate route: {} {}", method, route.path);
        }

        if route.handler.is_some() && !route.body.is_empty() {
            bail!("Route cannot define both handler and inline body");
        }
        if route.handler.is_none() && route.body.is_empty() {
            bail!("Route must define either handler or inline body");
        }

        if let Some(handler) = &route.handler {
            let handler_key = handler.to_lowercase();
            // Accept either a fully-qualified name or a bare function name that
            // matches any declared function (any namespace).
            let matches_any = fn_names.contains(&handler_key)
                || program.functions.iter().any(|f| {
                    let fqn = ns_fqn(&f.namespace, &f.name);
                    fqn == handler_key || f.name.to_lowercase() == handler_key
                });
            if !matches_any {
                // Qualified handler (`pkg.fn`) may live in a dependency that
                // single-file validation can't see — let the runtime resolver
                // catch a real miss.
                if !handler.contains('.') {
                    bail!("Route handler '{}' is not defined as a function", handler);
                }
            }
        }

        if route.handler.is_none() {
            validate_stmts(
                &route.body,
                &ctx_names,
                &entity_contexts,
                &db_tables,
                &entity_fields_by_table,
            )?;
        }
    }

    for function in &program.functions {
        validate_stmts(
            &function.body,
            &ctx_names,
            &entity_contexts,
            &db_tables,
            &entity_fields_by_table,
        )
        .map_err(|err| anyhow!("Function '{}': {err}", function.name))?;
    }

    let mut mw_names = HashSet::new();
    for mw in &program.middlewares {
        let key = ns_fqn(&mw.namespace, &mw.name);
        if !mw_names.insert(key) {
            bail!("Duplicate middleware name: {}", mw.name);
        }
        validate_stmts(
            &mw.body,
            &ctx_names,
            &entity_contexts,
            &db_tables,
            &entity_fields_by_table,
        )
        .map_err(|err| anyhow!("Middleware '{}': {err}", mw.name))?;
    }

    if let Some(handler) = &program.error_handler {
        validate_stmts(
            &handler.body,
            &ctx_names,
            &entity_contexts,
            &db_tables,
            &entity_fields_by_table,
        )
        .map_err(|err| anyhow!("errorHandler: {err}"))?;
    }

    for route in &program.routes {
        for mw_ref in &route.middlewares {
            let key = mw_ref.to_lowercase();
            let matches_any = mw_names.contains(&key)
                || program.middlewares.iter().any(|m| {
                    let fqn = ns_fqn(&m.namespace, &m.name);
                    fqn == key || m.name.to_lowercase() == key
                });
            if !matches_any {
                // Qualified references (`pkg.Mw`) may target a namespace that
                // lives in another file or a dependency package — single-file
                // validation can't see those. Defer the check to runtime.
                if mw_ref.contains('.') {
                    continue;
                }
                bail!(
                    "Route {} {} references unknown middleware '{}'",
                    route.method,
                    route.path,
                    mw_ref
                );
            }
        }
    }

    // After the main pass, walk every DbSelect's `with_relations` and verify
    // each name is a real navigation declared on the queried entity.
    for function in &program.functions {
        check_with_relations_in_stmts(&function.body, &entity_navigations)?;
    }
    for route in &program.routes {
        check_with_relations_in_stmts(&route.body, &entity_navigations)?;
    }
    for mw in &program.middlewares {
        check_with_relations_in_stmts(&mw.body, &entity_navigations)?;
    }

    // Compile-time field-access check for typed function parameters.
    let model_fields_for_typecheck: HashMap<String, HashSet<String>> = program
        .models
        .iter()
        .map(|m| {
            (
                m.name.to_lowercase(),
                m.fields.iter().map(|f| f.name.to_lowercase()).collect(),
            )
        })
        .collect();

    for function in &program.functions {
        let mut locals: HashMap<String, String> = HashMap::new();
        for param in &function.params {
            if let Some(ty) = &param.ty {
                if let Some(base) = strip_type_to_model_name(ty) {
                    if model_fields_for_typecheck.contains_key(&base.to_lowercase()) {
                        locals.insert(param.name.to_lowercase(), base);
                    }
                }
            }
        }
        check_typed_field_access_in_stmts(&function.body, &mut locals, &model_fields_for_typecheck)
            .map_err(|err| anyhow!("Function '{}': {err}", function.name))?;
    }

    // Compile-time mutation-field check: every `var.field = ...`, `insert var`,
    // `update var`, `delete var` where `var` is locally bound to `new Entity()`
    // must reference a field that actually exists on that entity, and (for
    // DB writes) the target table must match the bound entity.
    //
    // Lives at the bottom of `validate_program` on purpose — earlier passes
    // (dbcontext / table existence / WHERE column checks) get to fire first.
    let entity_fields_for_mutations: HashMap<String, Vec<String>> = program
        .models
        .iter()
        .filter(|m| m.kind == ModelKind::Entity)
        .map(|m| {
            (
                m.name.to_lowercase(),
                m.fields.iter().map(|f| f.name.to_lowercase()).collect(),
            )
        })
        .collect();

    for function in &program.functions {
        let mut bindings: EntityBindings = HashMap::new();
        check_mutation_fields_in_stmts(&function.body, &mut bindings, &entity_fields_for_mutations)
            .map_err(|err| anyhow!("Function '{}': {err}", function.name))?;
    }
    for route in &program.routes {
        if route.handler.is_some() {
            continue;
        }
        let label = format!("Route {} {}", route.method.to_ascii_uppercase(), route.path);
        let mut bindings: EntityBindings = HashMap::new();
        check_mutation_fields_in_stmts(&route.body, &mut bindings, &entity_fields_for_mutations)
            .map_err(|err| anyhow!("{label}: {err}"))?;
    }
    for mw in &program.middlewares {
        let mut bindings: EntityBindings = HashMap::new();
        check_mutation_fields_in_stmts(&mw.body, &mut bindings, &entity_fields_for_mutations)
            .map_err(|err| anyhow!("Middleware '{}': {err}", mw.name))?;
    }
    if let Some(handler) = &program.error_handler {
        let mut bindings: EntityBindings = HashMap::new();
        check_mutation_fields_in_stmts(&handler.body, &mut bindings, &entity_fields_for_mutations)
            .map_err(|err| anyhow!("errorHandler: {err}"))?;
    }

    Ok(())
}

/// Map of locally-bound variable name (lowercased) -> entity name (original
/// case, as written in source). Tracks `let v = new Entity();` bindings within
/// a function/route/middleware body so we can spot bogus `v.field = ...`
/// writes and mismatched `insert v into ctx.Table;` at compile time.
type EntityBindings = HashMap<String, String>;

fn check_mutation_fields_in_stmts(
    stmts: &[Stmt],
    bindings: &mut EntityBindings,
    entity_fields: &HashMap<String, Vec<String>>,
) -> Result<()> {
    for stmt in stmts {
        check_mutation_fields_in_stmt(stmt, bindings, entity_fields)?;
    }
    Ok(())
}

fn check_mutation_fields_in_stmt(
    stmt: &Stmt,
    bindings: &mut EntityBindings,
    entity_fields: &HashMap<String, Vec<String>>,
) -> Result<()> {
    match stmt {
        Stmt::Let { name, value } | Stmt::Assign { name, value } => {
            let key = name.to_lowercase();
            if let Expr::NewEntity { entity } = value {
                bindings.insert(key, entity.clone());
            } else {
                // Re-assigning to something other than `new Entity()` drops
                // the previous binding — we can no longer prove the var still
                // refers to that entity.
                bindings.remove(&key);
            }
            Ok(())
        }
        Stmt::FieldAssign { var, field, .. } => {
            if let Some(entity) = bindings.get(&var.to_lowercase()) {
                if let Some(fields) = entity_fields.get(&entity.to_lowercase()) {
                    let needle = field.to_lowercase();
                    if !fields.iter().any(|f| f == &needle) {
                        bail!("Unknown column '{}' on entity '{}'", field, entity);
                    }
                }
            }
            Ok(())
        }
        Stmt::DbInsert { var, table, .. }
        | Stmt::DbUpdate { var, table, .. }
        | Stmt::DbDelete { var, table, .. } => {
            if let Some(entity) = bindings.get(&var.to_lowercase()) {
                if !table_matches_entity(table, entity) {
                    bail!(
                        "variable '{}' is bound to entity '{}' but the target table is '{}'",
                        var,
                        entity,
                        table
                    );
                }
            }
            Ok(())
        }
        Stmt::DbDeleteWhere { .. }
        | Stmt::Print(_)
        | Stmt::Expr(_)
        | Stmt::Return(_)
        | Stmt::Break
        | Stmt::Continue
        | Stmt::ValidateBody { .. } => Ok(()),
        Stmt::If {
            then_body,
            else_body,
            ..
        } => {
            let snapshot = bindings.clone();
            let mut then_state = snapshot.clone();
            check_mutation_fields_in_stmts(then_body, &mut then_state, entity_fields)?;
            let end_state = if let Some(eb) = else_body {
                let mut else_state = snapshot.clone();
                check_mutation_fields_in_stmts(eb, &mut else_state, entity_fields)?;
                intersect_bindings(&then_state, &else_state)
            } else {
                // Else may not execute — keep only bindings that survived the
                // then-branch unchanged from before the if.
                intersect_bindings(&then_state, &snapshot)
            };
            *bindings = end_state;
            Ok(())
        }
        Stmt::While { body, .. } => {
            let snapshot = bindings.clone();
            let mut body_state = snapshot.clone();
            check_mutation_fields_in_stmts(body, &mut body_state, entity_fields)?;
            // Loop body may not run; keep only bindings that match the
            // pre-loop state.
            *bindings = intersect_bindings(&body_state, &snapshot);
            Ok(())
        }
        Stmt::ForIn { var, body, .. } => {
            let snapshot = bindings.clone();
            let mut body_state = snapshot.clone();
            // Loop variable shadowing: clear any prior binding for it before
            // walking the body so an inner `v.field = ...` doesn't reuse an
            // outer entity assumption.
            body_state.remove(&var.to_lowercase());
            check_mutation_fields_in_stmts(body, &mut body_state, entity_fields)?;
            body_state.remove(&var.to_lowercase());
            *bindings = intersect_bindings(&body_state, &snapshot);
            Ok(())
        }
        Stmt::Try {
            body,
            catch_body,
            catch_var,
            ..
        } => {
            let snapshot = bindings.clone();
            let mut try_state = snapshot.clone();
            check_mutation_fields_in_stmts(body, &mut try_state, entity_fields)?;
            let mut catch_state = snapshot.clone();
            catch_state.remove(&catch_var.to_lowercase());
            check_mutation_fields_in_stmts(catch_body, &mut catch_state, entity_fields)?;
            catch_state.remove(&catch_var.to_lowercase());
            *bindings = intersect_bindings(&try_state, &catch_state);
            Ok(())
        }
        Stmt::Transaction { body } => check_mutation_fields_in_stmts(body, bindings, entity_fields),
    }
}

/// Intersect two binding maps: keep an entry only if both sides agree on the
/// same entity name (case-insensitive). Drops disagreements rather than
/// false-positiving downstream — we'd rather under-report than complain about
/// a field that's legitimate on one branch's bound entity.
fn intersect_bindings(a: &EntityBindings, b: &EntityBindings) -> EntityBindings {
    let mut out = HashMap::with_capacity(a.len().min(b.len()));
    for (k, va) in a {
        if let Some(vb) = b.get(k) {
            if va.eq_ignore_ascii_case(vb) {
                out.insert(k.clone(), va.clone());
            }
        }
    }
    out
}

/// Strip nullable / `Optional<T>` wrappers down to a plain model name. Returns
/// `None` for `List<T>` (field access on a list of items doesn't apply to the
/// list variable itself).
fn strip_type_to_model_name(ty: &str) -> Option<String> {
    let mut t = ty.trim();
    if let Some(stripped) = t.strip_suffix('?') {
        t = stripped.trim();
    }
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("list<") {
        return None;
    }
    if lower.starts_with("optional<") && t.ends_with('>') {
        let inner = &t[9..t.len() - 1];
        return Some(inner.trim().to_string());
    }
    Some(t.to_string())
}

fn check_typed_field_access_in_stmts(
    stmts: &[Stmt],
    locals: &mut HashMap<String, String>,
    model_fields: &HashMap<String, HashSet<String>>,
) -> Result<()> {
    for stmt in stmts {
        check_typed_field_access_in_stmt(stmt, locals, model_fields)?;
    }
    Ok(())
}

fn check_typed_field_access_in_stmt(
    stmt: &Stmt,
    locals: &mut HashMap<String, String>,
    model_fields: &HashMap<String, HashSet<String>>,
) -> Result<()> {
    match stmt {
        Stmt::Let { name, value } => {
            check_typed_field_access_in_expr(value, locals, model_fields)?;
            // The new binding is untyped — clear any older type entry so we
            // don't accidentally inherit it from a shadowed name.
            locals.remove(&name.to_lowercase());
            Ok(())
        }
        Stmt::Assign { name, value } => {
            check_typed_field_access_in_expr(value, locals, model_fields)?;
            locals.remove(&name.to_lowercase());
            Ok(())
        }
        Stmt::FieldAssign { var, field, value } => {
            check_typed_field_access_in_expr(value, locals, model_fields)?;
            if let Some(model_name) = locals.get(&var.to_lowercase()).cloned() {
                if let Some(fields) = model_fields.get(&model_name.to_lowercase()) {
                    if !fields.contains(&field.to_lowercase()) {
                        bail!(
                            "Type error: field '{}' is not declared on {}",
                            field,
                            model_name
                        );
                    }
                }
            }
            Ok(())
        }
        Stmt::Print(e) | Stmt::Expr(e) => check_typed_field_access_in_expr(e, locals, model_fields),
        Stmt::Return(Some(e)) => check_typed_field_access_in_expr(e, locals, model_fields),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::ValidateBody { .. } => Ok(()),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            check_typed_field_access_in_expr(cond, locals, model_fields)?;
            check_typed_field_access_in_stmts(then_body, locals, model_fields)?;
            if let Some(eb) = else_body {
                check_typed_field_access_in_stmts(eb, locals, model_fields)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            check_typed_field_access_in_expr(cond, locals, model_fields)?;
            check_typed_field_access_in_stmts(body, locals, model_fields)
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            check_typed_field_access_in_stmts(body, locals, model_fields)?;
            check_typed_field_access_in_stmts(catch_body, locals, model_fields)
        }
        Stmt::Transaction { body } => check_typed_field_access_in_stmts(body, locals, model_fields),
        Stmt::ForIn { var, iter, body } => {
            check_typed_field_access_in_expr(iter, locals, model_fields)?;
            // Loop variable is currently untyped — it's an array element from
            // a JSON shape we don't track at compile time.
            let key = var.to_lowercase();
            let prior = locals.remove(&key);
            let res = check_typed_field_access_in_stmts(body, locals, model_fields);
            if let Some(p) = prior {
                locals.insert(key, p);
            }
            res
        }
        Stmt::DbInsert { .. }
        | Stmt::DbUpdate { .. }
        | Stmt::DbDelete { .. }
        | Stmt::DbDeleteWhere { .. } => Ok(()),
    }
}

fn check_typed_field_access_in_expr(
    expr: &Expr,
    locals: &HashMap<String, String>,
    model_fields: &HashMap<String, HashSet<String>>,
) -> Result<()> {
    match expr {
        Expr::FieldGet { var, field } => {
            if let Some(model_name) = locals.get(&var.to_lowercase()) {
                if let Some(fields) = model_fields.get(&model_name.to_lowercase()) {
                    if !fields.contains(&field.to_lowercase()) {
                        bail!(
                            "Type error: field '{}' is not declared on {}",
                            field,
                            model_name
                        );
                    }
                }
            }
            Ok(())
        }
        Expr::Call { args, .. } => {
            for a in args {
                check_typed_field_access_in_expr(a, locals, model_fields)?;
            }
            Ok(())
        }
        Expr::Await(inner) | Expr::Neg(inner) | Expr::Not(inner) => {
            check_typed_field_access_in_expr(inner, locals, model_fields)
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
            check_typed_field_access_in_expr(a, locals, model_fields)?;
            check_typed_field_access_in_expr(b, locals, model_fields)
        }
        Expr::ObjectLit(fields) => {
            for (_, v) in fields {
                check_typed_field_access_in_expr(v, locals, model_fields)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for item in items {
                check_typed_field_access_in_expr(item, locals, model_fields)?;
            }
            Ok(())
        }
        Expr::DbSelect {
            where_clause,
            limit,
            offset,
            ..
        } => {
            if let Some(wc) = where_clause {
                check_typed_where(wc, locals, model_fields)?;
            }
            if let Some(l) = limit {
                check_typed_field_access_in_expr(l, locals, model_fields)?;
            }
            if let Some(o) = offset {
                check_typed_field_access_in_expr(o, locals, model_fields)?;
            }
            Ok(())
        }
        Expr::DbCount { where_clause, .. } | Expr::DbAggregate { where_clause, .. } => {
            if let Some(wc) = where_clause {
                check_typed_where(wc, locals, model_fields)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn check_typed_where(
    expr: &WhereExpr,
    locals: &HashMap<String, String>,
    model_fields: &HashMap<String, HashSet<String>>,
) -> Result<()> {
    match expr {
        WhereExpr::Atom(wc) => check_typed_field_access_in_expr(&wc.rhs, locals, model_fields),
        WhereExpr::InList { values, .. } => {
            for v in values {
                check_typed_field_access_in_expr(v, locals, model_fields)?;
            }
            Ok(())
        }
        WhereExpr::Between { low, high, .. } => {
            check_typed_field_access_in_expr(low, locals, model_fields)?;
            check_typed_field_access_in_expr(high, locals, model_fields)
        }
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            check_typed_where(l, locals, model_fields)?;
            check_typed_where(r, locals, model_fields)
        }
    }
}

fn check_with_relations_in_stmts(
    stmts: &[Stmt],
    entity_navigations: &HashMap<String, HashMap<String, NavigationField>>,
) -> Result<()> {
    for stmt in stmts {
        check_with_relations_in_stmt(stmt, entity_navigations)?;
    }
    Ok(())
}

fn check_with_relations_in_stmt(
    stmt: &Stmt,
    entity_navigations: &HashMap<String, HashMap<String, NavigationField>>,
) -> Result<()> {
    match stmt {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::FieldAssign { value, .. }
        | Stmt::Print(value)
        | Stmt::Expr(value)
        | Stmt::Return(Some(value)) => check_with_relations_in_expr(value, entity_navigations),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            check_with_relations_in_expr(cond, entity_navigations)?;
            check_with_relations_in_stmts(then_body, entity_navigations)?;
            if let Some(b) = else_body {
                check_with_relations_in_stmts(b, entity_navigations)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            check_with_relations_in_expr(cond, entity_navigations)?;
            check_with_relations_in_stmts(body, entity_navigations)
        }
        Stmt::Try {
            body, catch_body, ..
        } => {
            check_with_relations_in_stmts(body, entity_navigations)?;
            check_with_relations_in_stmts(catch_body, entity_navigations)
        }
        Stmt::Transaction { body } => check_with_relations_in_stmts(body, entity_navigations),
        Stmt::ForIn { iter, body, .. } => {
            check_with_relations_in_expr(iter, entity_navigations)?;
            check_with_relations_in_stmts(body, entity_navigations)
        }
        _ => Ok(()),
    }
}

fn check_with_relations_in_expr(
    expr: &Expr,
    entity_navigations: &HashMap<String, HashMap<String, NavigationField>>,
) -> Result<()> {
    match expr {
        Expr::DbSelect {
            entity,
            with_relations,
            ..
        } => {
            if !with_relations.is_empty() {
                if entity == "*" {
                    bail!("select * cannot use 'with <relation>' — name the entity explicitly");
                }
                let entity_key = entity.to_lowercase();
                let nav_map = entity_navigations.get(&entity_key);
                for rel in with_relations {
                    let rel_key = rel.to_lowercase();
                    let known = nav_map.map(|m| m.contains_key(&rel_key)).unwrap_or(false);
                    if !known {
                        bail!(
                            "Entity '{}' has no navigation property '{}' (used in 'select ... with {}')",
                            entity,
                            rel,
                            rel
                        );
                    }
                }
            }
            Ok(())
        }
        Expr::Await(inner) | Expr::Neg(inner) => {
            check_with_relations_in_expr(inner, entity_navigations)
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
            check_with_relations_in_expr(a, entity_navigations)?;
            check_with_relations_in_expr(b, entity_navigations)
        }
        Expr::Call { args, .. } => {
            for a in args {
                check_with_relations_in_expr(a, entity_navigations)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for item in items {
                check_with_relations_in_expr(item, entity_navigations)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_stmts(
    stmts: &[Stmt],
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
    entity_fields_by_table: &HashMap<(String, String), Vec<String>>,
) -> Result<()> {
    for stmt in stmts {
        validate_stmt(
            stmt,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        )?;
    }
    Ok(())
}

fn validate_stmt(
    stmt: &Stmt,
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
    entity_fields_by_table: &HashMap<(String, String), Vec<String>>,
) -> Result<()> {
    match stmt {
        Stmt::Let { value, .. } => validate_expr(
            value,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::Assign { value, .. } => validate_expr(
            value,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::FieldAssign { value, .. } => validate_expr(
            value,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::Print(value) => validate_expr(
            value,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            validate_expr(
                cond,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_stmts(
                then_body,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            if let Some(else_body) = else_body {
                validate_stmts(
                    else_body,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            validate_expr(
                cond,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_stmts(
                body,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
        Stmt::Break | Stmt::Continue => Ok(()),
        Stmt::Expr(expr) => validate_expr(
            expr,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::Return(None) => Ok(()),
        Stmt::Return(Some(expr)) => validate_expr(
            expr,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::ValidateBody { fields } => {
            if fields.is_empty() {
                bail!("error[E007]: validate body block has no fields");
            }
            Ok(())
        }
        Stmt::Try {
            body,
            catch_type,
            catch_body,
            ..
        } => {
            if let Some(t) = catch_type {
                if !crate::runner::JWC_ERROR_KINDS.contains(&t.as_str()) {
                    let kinds = crate::runner::JWC_ERROR_KINDS.join(", ");
                    let hint = match crate::runner::closest_known_kind(t) {
                        Some(s) => format!(" — did you mean `{}`?", s),
                        None => String::new(),
                    };
                    bail!(
                        "error[E008]: unknown catch type `{}`{}. Known kinds: {}",
                        t,
                        hint,
                        kinds
                    );
                }
            }
            validate_stmts(
                body,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_stmts(
                catch_body,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
        Stmt::Transaction { body } => validate_stmts(
            body,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Stmt::ForIn { iter, body, .. } => {
            validate_expr(
                iter,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_stmts(
                body,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
        Stmt::DbInsert {
            context_var, table, ..
        }
        | Stmt::DbUpdate {
            context_var, table, ..
        }
        | Stmt::DbDelete {
            context_var, table, ..
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;
            validate_table_in_context(&ctx_key, table, db_tables)
        }
        Stmt::DbDeleteWhere {
            context_var,
            table,
            where_clause,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;
            validate_table_in_context(&ctx_key, table, db_tables)?;
            let fields = lookup_table_fields(&ctx_key, table, entity_fields_by_table);
            if let Some(fields) = fields {
                check_where_columns(where_clause, fields, context_var, table)?;
            }
            validate_where_expr(
                where_clause,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
    }
}

fn validate_expr(
    expr: &Expr,
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
    entity_fields_by_table: &HashMap<(String, String), Vec<String>>,
) -> Result<()> {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_) => Ok(()),
        Expr::Call { args, .. } => {
            for arg in args {
                validate_expr(
                    arg,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        Expr::FieldGet { .. } | Expr::NewEntity { .. } => Ok(()),
        Expr::DbCount {
            context_var,
            table,
            where_clause,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;
            validate_table_in_context(&ctx_key, table, db_tables)?;
            let fields = lookup_table_fields(&ctx_key, table, entity_fields_by_table);
            if let (Some(fields), Some(wc)) = (fields, where_clause.as_deref()) {
                check_where_columns(wc, fields, context_var, table)?;
            }
            if let Some(wc) = where_clause {
                validate_where_expr(
                    wc,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        Expr::DbAggregate {
            kind: _,
            field,
            context_var,
            table,
            where_clause,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;
            validate_table_in_context(&ctx_key, table, db_tables)?;
            let fields = lookup_table_fields(&ctx_key, table, entity_fields_by_table);
            if let Some(fields) = fields {
                let col = strip_entity_prefix(field);
                if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                    bail!(
                        "Unknown column '{}' in aggregate over {}.{}",
                        col,
                        context_var,
                        table
                    );
                }
                if let Some(wc) = where_clause.as_deref() {
                    check_where_columns(wc, fields, context_var, table)?;
                }
            }
            if let Some(wc) = where_clause {
                validate_where_expr(
                    wc,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        Expr::DbSelect {
            entity,
            context_var,
            table,
            where_clause,
            order_by,
            limit,
            offset,
            first: _,
            // `with_relations` is validated in `check_with_relations_in_expr`,
            // a separate walk that has access to `entity_navigations`.
            with_relations: _,
            projection,
            group_by,
            having,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;

            if entity != "*" {
                let entity_key = entity.to_lowercase();
                let expected_ctx = entity_contexts.get(&entity_key).ok_or_else(|| {
                    anyhow!(
                        "error[E002]: Unknown entity '{}' used in select expression",
                        entity
                    )
                })?;

                if let Some(expected_ctx) = expected_ctx {
                    if &ctx_key != expected_ctx {
                        bail!(
                            "Entity '{}' is bound to dbcontext '{}', but select uses '{}'",
                            entity,
                            expected_ctx,
                            context_var
                        );
                    }
                }

                if !table_matches_entity(table, entity) {
                    bail!(
                        "select {} from {}.{} has table/entity mismatch",
                        entity,
                        context_var,
                        table
                    );
                }
            }

            validate_table_in_context(&ctx_key, table, db_tables)?;

            // Compile-time column existence check for WHERE / ORDER BY / projection.
            let fields = lookup_table_fields(&ctx_key, table, entity_fields_by_table);
            if let Some(fields) = fields {
                if let Some(wc) = where_clause {
                    check_where_columns(wc, fields, context_var, table)?;
                }
                if let Some(ob) = order_by {
                    let col = strip_entity_prefix(&ob.field);
                    if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                        bail!(
                            "Unknown column '{}' in ORDER BY of {}.{}",
                            col,
                            context_var,
                            table
                        );
                    }
                }
                for col in projection {
                    if !fields.iter().any(|f| f.eq_ignore_ascii_case(col)) {
                        bail!(
                            "Unknown column '{}' in projection of {}.{}",
                            col,
                            context_var,
                            table
                        );
                    }
                }
                for grp in group_by {
                    let col = strip_entity_prefix(grp);
                    if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                        bail!(
                            "error[E004]: Unknown column '{}' in GROUP BY of {}.{}",
                            col,
                            context_var,
                            table
                        );
                    }
                }
            }

            if having.is_some() && group_by.is_empty() {
                bail!(
                    "error[E009]: `having` requires `group by` — found `having` on select {} from {}.{} without a `group by` clause",
                    entity,
                    context_var,
                    table
                );
            }
            if let Some(hv) = having {
                validate_where_expr(
                    hv,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }

            if let Some(where_clause) = where_clause {
                validate_where_expr(
                    where_clause,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            if let Some(limit_expr) = limit {
                validate_expr(
                    limit_expr,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            if let Some(offset_expr) = offset {
                validate_expr(
                    offset_expr,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }

            Ok(())
        }
        Expr::Await(inner) | Expr::Not(inner) => validate_expr(
            inner,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Expr::ObjectLit(fields) => {
            for (_, value) in fields {
                validate_expr(
                    value,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        Expr::Add(l, r)
        | Expr::Sub(l, r)
        | Expr::Mul(l, r)
        | Expr::Div(l, r)
        | Expr::Mod(l, r)
        | Expr::Eq(l, r)
        | Expr::Neq(l, r)
        | Expr::Lt(l, r)
        | Expr::Lte(l, r)
        | Expr::Gt(l, r)
        | Expr::Gte(l, r)
        | Expr::And(l, r)
        | Expr::Or(l, r) => {
            validate_expr(
                l,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_expr(
                r,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
        Expr::Neg(inner) => validate_expr(
            inner,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        Expr::ArrayLit(items) => {
            for item in items {
                validate_expr(
                    item,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
    }
}

fn check_where_columns(
    expr: &WhereExpr,
    fields: &[String],
    context_var: &str,
    table: &str,
) -> Result<()> {
    match expr {
        WhereExpr::Atom(wc) => {
            let col = strip_entity_prefix(&wc.field);
            if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                bail!(
                    "Unknown column '{}' in WHERE of {}.{}",
                    col,
                    context_var,
                    table
                );
            }
            Ok(())
        }
        WhereExpr::InList { field, .. } | WhereExpr::Between { field, .. } => {
            let col = strip_entity_prefix(field);
            if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                bail!(
                    "Unknown column '{}' in WHERE of {}.{}",
                    col,
                    context_var,
                    table
                );
            }
            Ok(())
        }
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            check_where_columns(l, fields, context_var, table)?;
            check_where_columns(r, fields, context_var, table)
        }
    }
}

fn validate_where_expr(
    expr: &WhereExpr,
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
    entity_fields_by_table: &HashMap<(String, String), Vec<String>>,
) -> Result<()> {
    match expr {
        WhereExpr::Atom(wc) => validate_expr(
            &wc.rhs,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
        WhereExpr::InList { values, .. } => {
            for v in values {
                validate_expr(
                    v,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
            }
            Ok(())
        }
        WhereExpr::Between { low, high, .. } => {
            validate_expr(
                low,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_expr(
                high,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            validate_where_expr(
                l,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )?;
            validate_where_expr(
                r,
                ctx_names,
                entity_contexts,
                db_tables,
                entity_fields_by_table,
            )
        }
    }
}

fn lookup_table_fields<'a>(
    ctx_key: &str,
    table: &str,
    entity_fields_by_table: &'a HashMap<(String, String), Vec<String>>,
) -> Option<&'a Vec<String>> {
    let direct = (ctx_key.to_string(), table.to_lowercase());
    if let Some(v) = entity_fields_by_table.get(&direct) {
        return Some(v);
    }
    let snake = (ctx_key.to_string(), to_snake_case(table).to_lowercase());
    entity_fields_by_table.get(&snake)
}

fn strip_entity_prefix(path: &str) -> String {
    if let Some(pos) = path.rfind('.') {
        path[pos + 1..].to_string()
    } else {
        path.to_string()
    }
}

fn validate_context_exists(context_var: &str, ctx_names: &HashSet<String>) -> Result<String> {
    if ctx_names.is_empty() {
        return Ok(context_var.to_lowercase());
    }

    let key = context_var.to_lowercase();
    if !ctx_names.contains(&key) {
        bail!("Unknown dbcontext '{}' used in DB statement", context_var);
    }
    Ok(key)
}

fn validate_table_in_context(
    context_var_lc: &str,
    table: &str,
    db_tables: &HashSet<(String, String)>,
) -> Result<()> {
    if db_tables.is_empty() {
        return Ok(());
    }

    let table_key = table.to_lowercase();
    if db_tables.contains(&(context_var_lc.to_string(), table_key.clone())) {
        return Ok(());
    }

    let snake = to_snake_case(table).to_lowercase();
    if db_tables.contains(&(context_var_lc.to_string(), snake)) {
        return Ok(());
    }

    bail!(
        "Unknown table/entity '{}.{}' for compile-time DB validation",
        context_var_lc,
        table
    )
}

fn table_matches_entity(table: &str, entity: &str) -> bool {
    if table.eq_ignore_ascii_case(entity) {
        return true;
    }
    to_snake_case(table).eq_ignore_ascii_case(&to_snake_case(entity))
}

fn to_snake_case(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, ch) in input.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if idx != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn resolve_entity_driver(
    program: &Program,
    entity: &ModelDecl,
    ctx_drivers: &HashMap<String, String>,
) -> Result<String> {
    let known_ctx_names = ctx_drivers.keys().cloned().collect::<HashSet<_>>();
    if let Some(context_name) = resolve_entity_context_name(program, entity, &known_ctx_names)? {
        let key = context_name.to_lowercase();
        let driver = ctx_drivers.get(&key).ok_or_else(|| {
            anyhow!(
                "error[E001]: Entity '{}' references unknown dbcontext '{}'",
                entity.name,
                context_name
            )
        })?;
        return Ok(driver.clone());
    }

    Ok("postgres".to_string())
}

fn resolve_entity_context_name(
    program: &Program,
    entity: &ModelDecl,
    ctx_names: &HashSet<String>,
) -> Result<Option<String>> {
    if let Some(context_name) = &entity.context_name {
        let key = context_name.to_lowercase();
        if !ctx_names.contains(&key) {
            bail!(
                "error[E001]: Entity '{}' references unknown dbcontext '{}'",
                entity.name,
                context_name
            );
        }
        return Ok(Some(context_name.clone()));
    }

    if program.dbcontexts.len() == 1 {
        return Ok(Some(program.dbcontexts[0].name.clone()));
    }

    if program.dbcontexts.len() > 1 {
        bail!(
            "Entity '{}' must specify 'of <DbContextName>' when multiple dbcontexts are declared",
            entity.name
        );
    }

    Ok(None)
}

fn validate_type_spec_for_driver(ty: &TypeSpec, driver: &str) -> Result<()> {
    if driver.eq_ignore_ascii_case("postgres") {
        return validate_type_spec_postgres(ty);
    }

    bail!(
        "Postgres is currently the only supported dbcontext driver (got '{driver}'). \
         Multi-driver support is planned for Phase 2."
    )
}

fn validate_type_spec_postgres(ty: &TypeSpec) -> Result<()> {
    match ty.name.as_str() {
        "int" => {
            if !(ty.args.is_empty() || ty.args.len() == 2) {
                bail!("int accepts either no args or exactly 2 args");
            }
            if ty.args.len() == 2 && ty.args[0] > ty.args[1] {
                bail!("int(min,max) requires min <= max");
            }
            Ok(())
        }
        "bigint" | "bool" | "uuid" | "datetime" | "json" | "bytes" | "byte[]" | "bytea" => {
            if !ty.args.is_empty() {
                bail!("{} does not accept args", ty.name);
            }
            Ok(())
        }
        "text" => {
            if ty.args.len() > 1 {
                bail!("text accepts zero args or one length arg");
            }
            Ok(())
        }
        "varchar" => {
            if ty.args.len() != 1 {
                bail!("varchar requires exactly one arg: varchar(length)");
            }
            Ok(())
        }
        "decimal" => {
            if ty.args.len() != 2 {
                bail!("decimal requires exactly two args: decimal(precision,scale)");
            }
            Ok(())
        }
        other => bail!("Unknown type '{other}'"),
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Token,
    source_map: SourceMap,
    /// Active namespace for the current file. `namespace foo.bar;` sets this
    /// for the rest of the file. Cleared by `__jwc_namespace_reset__;` (see
    /// `project::load_project_from_root` for the inter-file reset trick).
    current_namespace: Vec<String>,
    /// Stack of group contexts. Each entry contributes one path segment and
    /// (optionally) middleware names to every `route` and `mount` parsed
    /// inside it. Frames are pushed/popped around `group { ... }` blocks.
    group_stack: Vec<GroupFrame>,
}

#[derive(Debug, Clone, Default)]
struct GroupFrame {
    /// Just this frame's own prefix segment (already starts with `/` or is empty).
    prefix: String,
    /// Just this frame's own middleware list (does not include outer frames).
    middlewares: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self> {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            source_map: SourceMap::new(source),
            current_namespace: Vec::new(),
            group_stack: Vec::new(),
        })
    }

    /// The accumulated path prefix from every enclosing group, joined.
    /// Always starts with `/` when non-empty; returns empty string at root.
    fn group_prefix(&self) -> String {
        let mut out = String::new();
        for frame in &self.group_stack {
            out.push_str(&frame.prefix);
        }
        out
    }

    /// All middleware names contributed by enclosing groups, in outer→inner
    /// order. Each route's own `use Mw, ...` list is appended after this.
    fn group_middlewares(&self) -> Vec<String> {
        let mut out = Vec::new();
        for frame in &self.group_stack {
            out.extend(frame.middlewares.iter().cloned());
        }
        out
    }

    fn parse_program(&mut self) -> Result<Program> {
        let mut program = Program::default();
        // Pending visibility modifier: Some(true) → next decl is public,
        // Some(false) → explicit private, None → no modifier (defaults to
        // private). Tracking three states catches `public public function`
        // and `public private function` as user errors.
        let mut pending_vis: Option<bool> = None;

        while !matches!(self.current.kind, TokenKind::Eof) {
            // `public` / `private` modifiers immediately before a declaration.
            if matches!(self.current.kind, TokenKind::Keyword(Keyword::Public)) {
                if pending_vis.is_some() {
                    return Err(self.error_here("duplicate visibility modifier"));
                }
                self.bump()?;
                pending_vis = Some(true);
                continue;
            }
            if matches!(self.current.kind, TokenKind::Keyword(Keyword::Private)) {
                if pending_vis.is_some() {
                    return Err(self.error_here("duplicate visibility modifier"));
                }
                self.bump()?;
                pending_vis = Some(false);
                continue;
            }

            // Snapshot the pending visibility for this iteration. The match
            // arms that accept a modifier call `take()` so the next loop
            // iteration starts clean; arms that reject one leave it as-is
            // and we error out below.
            let next_pub = pending_vis.unwrap_or(false);

            match &self.current.kind {
                TokenKind::Keyword(Keyword::Import) => {
                    pending_vis = None;
                    self.parse_import_stmt(&mut program)?;
                }
                TokenKind::Keyword(Keyword::Namespace) => {
                    pending_vis = None;
                    self.parse_namespace_stmt(&program)?;
                }
                TokenKind::Keyword(Keyword::Mount) => {
                    pending_vis = None;
                    program.mounts.push(self.parse_mount_stmt()?);
                }
                TokenKind::Keyword(Keyword::Group) => {
                    pending_vis = None;
                    self.parse_group_block(&mut program)?;
                }
                TokenKind::Keyword(Keyword::DbContext) => {
                    pending_vis = None;
                    let mut decl = self.parse_dbcontext_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    program.dbcontexts.push(decl);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    pending_vis = None;
                    let mut decl = self.parse_model_decl(ModelKind::Entity)?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.models.push(decl);
                }
                TokenKind::Keyword(Keyword::Class) => {
                    pending_vis = None;
                    let mut decl = self.parse_model_decl(ModelKind::Class)?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.models.push(decl);
                }
                TokenKind::Keyword(Keyword::Route) => {
                    pending_vis = None;
                    let mut decl = self.parse_route_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    // Visibility on routes is currently a no-op — routes are
                    // activation-gated via `register` instead.
                    program.routes.push(decl);
                }
                TokenKind::Keyword(Keyword::Function) => {
                    pending_vis = None;
                    let mut fn_decl = self.parse_function_decl(None)?;
                    fn_decl.namespace = self.current_namespace.clone();
                    fn_decl.visibility = visibility_from(next_pub);
                    program.functions.push(fn_decl);
                }
                TokenKind::Keyword(Keyword::Async) => {
                    pending_vis = None;
                    self.bump()?;
                    if !matches!(self.current.kind, TokenKind::Keyword(Keyword::Function)) {
                        return Err(self.error_here("expected 'function' after 'async'"));
                    }
                    let mut fn_decl = self.parse_function_decl(None)?;
                    fn_decl.is_async = true;
                    fn_decl.namespace = self.current_namespace.clone();
                    fn_decl.visibility = visibility_from(next_pub);
                    program.functions.push(fn_decl);
                }
                TokenKind::Keyword(Keyword::Dome) => {
                    pending_vis = None;
                    self.parse_dome_decl(&mut program, next_pub)?;
                }
                TokenKind::Keyword(Keyword::Middleware) => {
                    pending_vis = None;
                    let mut decl = self.parse_middleware_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(next_pub);
                    program.middlewares.push(decl);
                }
                TokenKind::Keyword(Keyword::ErrorHandler) => {
                    if pending_vis.is_some() {
                        return Err(
                            self.error_here("visibility modifier is not valid on errorHandler")
                        );
                    }
                    if program.error_handler.is_some() {
                        return Err(self.error_here("only one errorHandler is allowed per project"));
                    }
                    self.bump()?;
                    self.expect_symbol('(')?;
                    let catch_var =
                        self.expect_ident("expected error variable name after 'errorHandler('")?;
                    self.expect_symbol(')')?;
                    let body = self.parse_block()?;
                    program.error_handler = Some(ErrorHandlerDecl { catch_var, body });
                }
                _ => {
                    return Err(self.error_here(
                        "expected import, namespace, mount, group, public, private, dbcontext, entity/class, route, function, middleware, errorHandler, or dome",
                    ));
                }
            }
        }

        if pending_vis.is_some() {
            return Err(
                self.error_here("trailing visibility modifier has no declaration to apply to")
            );
        }

        Ok(program)
    }

    fn parse_dbcontext_decl(&mut self) -> Result<DbContextDecl> {
        self.bump()?;
        let name = self.expect_ident("expected dbcontext name")?;
        self.expect_symbol(':')?;
        let driver = self.expect_ident("expected driver name after ':'")?;

        if self.check_symbol('{') {
            self.skip_braced_block()?;
        } else {
            self.expect_symbol(';')?;
        }

        Ok(DbContextDecl {
            name,
            driver,
            namespace: Vec::new(),
        })
    }

    fn skip_braced_block(&mut self) -> Result<()> {
        self.expect_symbol('{')?;
        let mut depth = 1usize;
        while depth > 0 {
            match &self.current.kind {
                TokenKind::Symbol('{') => {
                    depth += 1;
                    self.bump()?;
                }
                TokenKind::Symbol('}') => {
                    depth -= 1;
                    self.bump()?;
                }
                TokenKind::Eof => return Err(self.error_here("unterminated block")),
                _ => self.bump()?,
            }
        }
        Ok(())
    }

    /// `import foo.bar;` — opens a package namespace for the current file.
    /// Records the import scoped to the file's current namespace so the
    /// resolver only applies it where it was declared.
    fn parse_import_stmt(&mut self, program: &mut Program) -> Result<()> {
        self.expect_keyword(Keyword::Import)?;
        let path = self.parse_qualified_path()?;
        self.expect_symbol(';')?;
        program.imports.push(crate::ast::ImportDecl {
            path,
            in_namespace: self.current_namespace.clone(),
        });
        Ok(())
    }

    /// `namespace foo.bar;` — sets the namespace for the rest of the file.
    /// Only one `namespace` per file. Empty `program` check is used to
    /// require it appear before any declaration.
    fn parse_namespace_stmt(&mut self, program: &Program) -> Result<()> {
        self.expect_keyword(Keyword::Namespace)?;
        let path = self.parse_qualified_path()?;
        self.expect_symbol(';')?;
        if !self.current_namespace.is_empty() {
            return Err(self.error_here("only one 'namespace' declaration is allowed per file"));
        }
        if !program.functions.is_empty()
            || !program.models.is_empty()
            || !program.routes.is_empty()
            || !program.middlewares.is_empty()
            || !program.dbcontexts.is_empty()
        {
            return Err(
                self.error_here("'namespace' must appear before any declaration in the file")
            );
        }
        self.current_namespace = path;
        Ok(())
    }

    /// `mount foo [at "/prefix"];` — activate a library namespace's routes.
    /// Inherits any prefix/middleware from enclosing `group` blocks.
    fn parse_mount_stmt(&mut self) -> Result<crate::ast::MountDecl> {
        self.expect_keyword(Keyword::Mount)?;
        let target = self.parse_qualified_path()?;
        if target.is_empty() {
            return Err(self.error_here("expected namespace name after 'mount'"));
        }

        // Optional `at "/prefix"` segment.
        let mut own_prefix: Option<String> = None;
        if let TokenKind::Ident(v) = &self.current.kind {
            if v.eq_ignore_ascii_case("at") {
                self.bump()?;
                let p = self.expect_string("expected prefix string after 'at'")?;
                if !p.starts_with('/') {
                    return Err(self.error_here("mount prefix must start with '/'"));
                }
                own_prefix = Some(p);
            }
        }
        self.expect_symbol(';')?;

        // Compose with enclosing group context.
        let group_prefix = self.group_prefix();
        let final_prefix = match (group_prefix.is_empty(), own_prefix) {
            (true, None) => None,
            (true, Some(p)) => Some(p),
            (false, None) => Some(group_prefix),
            (false, Some(p)) => Some(format!("{}{}", group_prefix, p)),
        };

        Ok(crate::ast::MountDecl {
            target,
            prefix: final_prefix,
            middlewares: self.group_middlewares(),
        })
    }

    /// `group ["/prefix"] [use Mw1, Mw2] { ITEMS... }` — wrap inner routes
    /// and mounts with a shared prefix and middleware chain. Either the
    /// prefix or the `use` clause (or both) must be present.
    fn parse_group_block(&mut self, program: &mut Program) -> Result<()> {
        self.expect_keyword(Keyword::Group)?;

        let mut prefix = String::new();
        if let TokenKind::String(_) = &self.current.kind {
            let p = self.expect_string("expected group prefix string")?;
            if !p.starts_with('/') {
                return Err(self.error_here("group prefix must start with '/'"));
            }
            prefix = p;
        }

        let mut middlewares: Vec<String> = Vec::new();
        if matches!(self.current.kind, TokenKind::Keyword(Keyword::Use)) {
            self.bump()?;
            // Each middleware is a qualified path (`Mw` or `pkg.Mw`).
            middlewares.push(self.parse_qualified_name()?);
            while self.check_symbol(',') {
                self.bump()?;
                middlewares.push(self.parse_qualified_name()?);
            }
        }

        if prefix.is_empty() && middlewares.is_empty() {
            return Err(self.error_here("group must have a prefix string, a `use` clause, or both"));
        }

        self.expect_symbol('{')?;
        self.group_stack.push(GroupFrame {
            prefix,
            middlewares,
        });
        // Parse body as a mini top-level loop — only items that respect
        // group context are allowed (route, mount, nested group).
        while !self.check_symbol('}') {
            match &self.current.kind {
                TokenKind::Keyword(Keyword::Route) => {
                    let mut decl = self.parse_route_decl()?;
                    decl.namespace = self.current_namespace.clone();
                    program.routes.push(decl);
                }
                TokenKind::Keyword(Keyword::Mount) => {
                    program.mounts.push(self.parse_mount_stmt()?);
                }
                TokenKind::Keyword(Keyword::Group) => {
                    self.parse_group_block(program)?;
                }
                TokenKind::Eof => {
                    self.group_stack.pop();
                    return Err(self.error_here("unterminated 'group' block"));
                }
                _ => {
                    self.group_stack.pop();
                    return Err(self.error_here(
                        "only 'route', 'mount', or nested 'group' allowed inside a group block",
                    ));
                }
            }
        }
        self.expect_symbol('}')?;
        self.group_stack.pop();
        Ok(())
    }

    fn parse_qualified_name(&mut self) -> Result<String> {
        let parts = self.parse_qualified_path()?;
        Ok(parts.join("."))
    }

    /// Parses a dot-separated identifier sequence into its parts.
    fn parse_qualified_path(&mut self) -> Result<Vec<String>> {
        let mut parts = vec![self.expect_ident("expected identifier")?];
        while self.check_symbol('.') {
            self.expect_symbol('.')?;
            parts.push(self.expect_ident("expected identifier after '.'")?);
        }
        Ok(parts)
    }

    fn parse_model_decl(&mut self, kind: ModelKind) -> Result<ModelDecl> {
        match kind {
            ModelKind::Entity => self.expect_keyword(Keyword::Entity)?,
            ModelKind::Class => self.expect_keyword(Keyword::Class)?,
        }

        let name = self.expect_ident("expected model name")?;

        let context_name = if kind == ModelKind::Entity {
            if let TokenKind::Ident(v) = &self.current.kind {
                if v.eq_ignore_ascii_case("of") {
                    self.bump()?;
                    Some(self.expect_ident("expected dbcontext name after 'of'")?)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        self.expect_symbol('{')?;

        let mut fields = Vec::new();
        let mut navigations = Vec::new();
        while !self.check_symbol('}') {
            let field_name = self.expect_ident("expected field name")?;

            // `field: TypeRef via Target.col;` — navigation property.
            if self.check_symbol(':') {
                self.bump()?;
                let nav = self.parse_navigation_remainder(&field_name)?;
                navigations.push(nav);
                continue;
            }

            let ty = self.parse_type_spec()?;
            let mut is_nullable = false;
            let mut is_primary_key = false;
            let mut is_auto_increment = false;
            let mut references: Option<FieldReference> = None;

            loop {
                match self.current.kind.clone() {
                    TokenKind::Ident(v) if v.eq_ignore_ascii_case("nullable") => {
                        is_nullable = true;
                        self.bump()?;
                    }
                    TokenKind::Ident(v) if v.eq_ignore_ascii_case("pk") => {
                        is_primary_key = true;
                        self.bump()?;
                    }
                    TokenKind::Ident(v)
                        if v.eq_ignore_ascii_case("autoincrement")
                            || v.eq_ignore_ascii_case("auto_increment")
                            || v.eq_ignore_ascii_case("serial") =>
                    {
                        is_auto_increment = true;
                        self.bump()?;
                    }
                    TokenKind::Ident(v) if v.eq_ignore_ascii_case("references") => {
                        self.bump()?;
                        references = Some(self.parse_field_reference()?);
                    }
                    _ => break,
                }
            }
            self.expect_symbol(';')?;

            fields.push(FieldDecl {
                name: field_name,
                ty,
                is_nullable,
                is_primary_key,
                is_auto_increment,
                references,
            });
        }

        self.expect_symbol('}')?;
        Ok(ModelDecl {
            kind,
            name,
            context_name,
            fields,
            navigations,
            namespace: Vec::new(),
            visibility: Visibility::Private,
        })
    }

    /// Parse the rest of `name: List<Target> via Target.col;` or
    /// `name: Target via Target.col;` after the `:` has been consumed.
    fn parse_navigation_remainder(&mut self, field_name: &str) -> Result<NavigationField> {
        let head = self.expect_ident("expected target type after ':'")?;
        let (kind, target_entity) = if head.eq_ignore_ascii_case("List") {
            self.expect_symbol('<')?;
            let inner = self.expect_ident("expected entity name inside List<...>")?;
            self.expect_symbol('>')?;
            (NavigationKind::OneToMany, inner)
        } else {
            (NavigationKind::OneToOne, head)
        };

        let via_kw = self.expect_ident("expected 'via' in navigation declaration")?;
        if !via_kw.eq_ignore_ascii_case("via") {
            return Err(self.error_here("expected 'via' in navigation declaration"));
        }

        let target = self.expect_ident("expected target entity name after 'via'")?;
        self.expect_symbol('.')?;
        let target_col = self.expect_ident("expected target column name after '.'")?;
        if !target.eq_ignore_ascii_case(&target_entity) {
            return Err(self.error_here(
                "navigation 'via' must reference the same entity declared on the left side",
            ));
        }
        self.expect_symbol(';')?;

        Ok(NavigationField {
            name: field_name.to_string(),
            kind,
            target_entity,
            target_field: target_col,
        })
    }

    fn parse_dome_decl(&mut self, program: &mut Program, is_pub: bool) -> Result<()> {
        self.expect_keyword(Keyword::Dome)?;
        let dome_name = self.expect_ident("expected dome name")?;
        self.expect_symbol('{')?;

        while !self.check_symbol('}') {
            match &self.current.kind {
                TokenKind::Keyword(Keyword::Function) => {
                    let mut decl = self.parse_function_decl(Some(&dome_name))?;
                    decl.namespace = self.current_namespace.clone();
                    decl.visibility = visibility_from(is_pub);
                    program.functions.push(decl);
                }
                _ => return Err(self.error_here("expected function declaration inside dome block")),
            }
        }

        self.expect_symbol('}')?;
        Ok(())
    }

    fn parse_route_decl(&mut self) -> Result<RouteDecl> {
        self.expect_keyword(Keyword::Route)?;
        let method = self.expect_ident("expected HTTP method (GET/POST/PUT/DELETE/PATCH/WS)")?;
        let own_path = self.expect_string("expected route path string")?;

        // Apply enclosing group prefix to the path (if any). Routes outside
        // any group keep their own path verbatim.
        let group_prefix = self.group_prefix();
        let path = if group_prefix.is_empty() {
            own_path
        } else {
            format!("{}{}", group_prefix, own_path)
        };

        // Optional `use M1[, M2, ...]` middleware list. Group-supplied
        // middlewares run BEFORE the route's own list. Each entry is a
        // qualified path (`Mw` or `pkg.Mw`) — FQN resolved at runtime.
        let mut middlewares = self.group_middlewares();
        if self.current.kind == TokenKind::Keyword(Keyword::Use) {
            self.bump()?;
            middlewares.push(self.parse_qualified_name()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                middlewares.push(self.parse_qualified_name()?);
            }
        }

        let protocol = if method.eq_ignore_ascii_case("ws") {
            RouteProtocol::Ws
        } else if method.eq_ignore_ascii_case("sse") {
            RouteProtocol::Sse
        } else {
            RouteProtocol::Http
        };
        let method = match protocol {
            RouteProtocol::Ws => "WS".to_string(),
            RouteProtocol::Sse => "SSE".to_string(),
            RouteProtocol::Http => method,
        };

        if self.check_symbol('-') {
            self.expect_symbol('-')?;
            self.expect_symbol('>')?;
            let handler = self.parse_qualified_name()?;
            self.expect_symbol(';')?;
            return Ok(RouteDecl {
                method,
                path,
                handler: Some(handler),
                body: Vec::new(),
                middlewares,
                protocol,
                namespace: Vec::new(),
            });
        }

        let body = self.parse_block()?;
        Ok(RouteDecl {
            method,
            path,
            handler: None,
            body,
            middlewares,
            protocol,
            namespace: Vec::new(),
        })
    }

    fn parse_field_reference(&mut self) -> Result<FieldReference> {
        let entity = self.expect_ident("expected target entity name after 'references'")?;
        self.expect_symbol('.')?;
        let column = self.expect_ident("expected target column name after '.'")?;

        let on_delete = if self.check_ident_eq("on") {
            self.bump()?;
            let delete_kw = self.expect_ident("expected 'delete' after 'on'")?;
            if !delete_kw.eq_ignore_ascii_case("delete") {
                return Err(self.error_here("only 'on delete' is supported"));
            }
            let action_kw = self.expect_ident("expected action after 'on delete'")?;
            match action_kw.to_ascii_lowercase().as_str() {
                "cascade" => OnDeleteAction::Cascade,
                "restrict" => OnDeleteAction::Restrict,
                "set" => {
                    let null_kw = self.expect_ident("expected 'null' after 'set'")?;
                    if !null_kw.eq_ignore_ascii_case("null") {
                        return Err(self.error_here("only 'set null' is supported"));
                    }
                    OnDeleteAction::SetNull
                }
                other => {
                    return Err(self.error_here(&format!(
                        "unknown ON DELETE action '{other}' (cascade/restrict/set null)"
                    )))
                }
            }
        } else {
            OnDeleteAction::NoAction
        };

        Ok(FieldReference {
            entity,
            column,
            on_delete,
        })
    }

    fn parse_middleware_decl(&mut self) -> Result<MiddlewareDecl> {
        self.expect_keyword(Keyword::Middleware)?;
        let name = self.expect_ident("expected middleware name")?;
        let body = self.parse_block()?;
        Ok(MiddlewareDecl {
            name,
            body,
            namespace: Vec::new(),
            visibility: Visibility::Private,
        })
    }

    fn parse_type_spec(&mut self) -> Result<TypeSpec> {
        let name = self.expect_ident("expected type name")?;
        let mut args = Vec::new();

        if self.check_symbol('(') {
            self.expect_symbol('(')?;
            args.push(self.parse_signed_number("expected type argument")?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                args.push(self.parse_signed_number("expected type argument after ','")?);
            }
            self.expect_symbol(')')?;
        }

        Ok(TypeSpec { name, args })
    }

    fn parse_signed_number(&mut self, msg: &str) -> Result<i64> {
        let sign = if self.check_symbol('-') {
            self.expect_symbol('-')?;
            -1
        } else {
            1
        };
        let number = self.expect_number(msg)?;
        Ok(sign * number)
    }

    fn parse_function_decl(&mut self, dome_name: Option<&str>) -> Result<FunctionDecl> {
        self.expect_keyword(Keyword::Function)?;

        let base_name = self.expect_ident("expected function name")?;
        let name = if let Some(dome_name) = dome_name {
            format!("{}.{}", dome_name, base_name)
        } else {
            base_name
        };
        self.expect_symbol('(')?;

        let mut params = Vec::new();
        if !self.check_symbol(')') {
            params.push(self.parse_typed_param()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                params.push(self.parse_typed_param()?);
            }
        }

        self.expect_symbol(')')?;

        // Optional return-type annotation: `: TypeName`
        let return_type = if self.check_symbol(':') {
            self.expect_symbol(':')?;
            Some(self.parse_type_ref()?)
        } else {
            None
        };

        self.expect_symbol('{')?;

        let mut body = Vec::new();
        while !self.check_symbol('}') {
            body.push(self.parse_stmt()?);
        }

        self.expect_symbol('}')?;
        Ok(FunctionDecl {
            name,
            params,
            return_type,
            body,
            is_async: false,
            namespace: Vec::new(),
            visibility: Visibility::Private,
        })
    }

    /// Parse a single parameter: `name` or `name: TypeRef`
    fn parse_typed_param(&mut self) -> Result<TypedParam> {
        let name = self.expect_ident("expected parameter name")?;
        let ty = if self.check_symbol(':') {
            self.expect_symbol(':')?;
            Some(self.parse_type_ref()?)
        } else {
            None
        };
        Ok(TypedParam { name, ty })
    }

    /// Parse a type reference. Supports plain names (`int`), generics
    /// (`List<User>`, `Optional<int>`), and the trailing `?` nullable marker.
    /// Result is the source-equivalent string (e.g. `"List<User>"`, `"int?"`).
    fn parse_type_ref(&mut self) -> Result<String> {
        let base = self.expect_ident("expected type name")?;
        let mut s = base;

        if self.check_symbol('<') {
            self.expect_symbol('<')?;
            let inner = self.parse_type_ref()?;
            self.expect_symbol('>')?;
            s = format!("{s}<{inner}>");
        }

        if self.check_symbol('?') {
            self.expect_symbol('?')?;
            s.push('?');
        }

        Ok(s)
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        match &self.current.kind {
            TokenKind::Keyword(Keyword::Validate) => self.parse_validate_body_stmt(),
            TokenKind::Keyword(Keyword::Try) => self.parse_try_stmt(),
            TokenKind::Keyword(Keyword::Transaction) => {
                self.bump()?;
                let body = self.parse_block()?;
                Ok(Stmt::Transaction { body })
            }
            TokenKind::Keyword(Keyword::Let) => {
                self.bump()?;
                let name = self.expect_ident("expected variable name")?;
                self.expect_symbol('=')?;
                let value = self.parse_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Let { name, value })
            }
            TokenKind::Keyword(Keyword::Print) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let value = self.parse_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol(';')?;
                Ok(Stmt::Print(value))
            }
            TokenKind::Keyword(Keyword::Return) => {
                self.bump()?;
                if self.check_symbol(';') {
                    self.expect_symbol(';')?;
                    Ok(Stmt::Return(None))
                } else {
                    let value = self.parse_expr()?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Return(Some(value)))
                }
            }
            TokenKind::Keyword(Keyword::If) => self.parse_if_stmt(),
            TokenKind::Keyword(Keyword::While) => self.parse_while_stmt(),
            TokenKind::Keyword(Keyword::For) => self.parse_for_stmt(),
            TokenKind::Keyword(Keyword::Break) => {
                self.bump()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Break)
            }
            TokenKind::Keyword(Keyword::Continue) => {
                self.bump()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Continue)
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("insert") => {
                self.bump()?;
                let var = self.expect_ident("expected variable name after 'insert'")?;
                let kw = self.expect_ident("expected 'into'")?;
                if !kw.eq_ignore_ascii_case("into") {
                    return Err(self.error_here("expected 'into' in insert statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbInsert {
                    var,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("update") => {
                self.bump()?;
                let var = self.expect_ident("expected variable name after 'update'")?;
                if self.current.kind != TokenKind::Keyword(Keyword::In) {
                    return Err(self.error_here("expected 'in' in update statement"));
                }
                self.bump()?;
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbUpdate {
                    var,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("delete") => {
                self.bump()?;

                if self.check_ident_eq("from") {
                    // Bulk form: `delete from CTX.Table where ... ;`
                    self.bump()?;
                    let (ctx, table) = self.parse_db_ref()?;
                    if !self.check_ident_eq("where") {
                        return Err(self
                            .error_here("bulk 'delete from CTX.Table' requires a 'where' clause"));
                    }
                    self.bump()?;
                    let where_clause = Box::new(self.parse_where_or()?);
                    self.expect_symbol(';')?;
                    return Ok(Stmt::DbDeleteWhere {
                        context_var: ctx,
                        table,
                        where_clause,
                    });
                }

                let var = self.expect_ident("expected variable name after 'delete'")?;
                let kw = self.expect_ident("expected 'from'")?;
                if !kw.eq_ignore_ascii_case("from") {
                    return Err(self.error_here("expected 'from' in delete statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbDelete {
                    var,
                    context_var: ctx,
                    table,
                })
            }
            TokenKind::Ident(_) => {
                let name = match self.current.kind.clone() {
                    TokenKind::Ident(v) => v,
                    _ => unreachable!(),
                };
                self.bump()?;
                if self.check_symbol('.') {
                    self.bump()?;
                    let member = self.expect_ident("expected member name after '.'")?;
                    if self.check_symbol('(') {
                        let call = self.parse_call_after_name(format!("{}.{}", name, member))?;
                        self.expect_symbol(';')?;
                        Ok(Stmt::Expr(call))
                    } else {
                        self.expect_symbol('=')?;
                        let value = self.parse_expr()?;
                        self.expect_symbol(';')?;
                        Ok(Stmt::FieldAssign {
                            var: name,
                            field: member,
                            value,
                        })
                    }
                } else if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let value = self.parse_expr()?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Assign { name, value })
                } else if self.check_symbol('(') {
                    let call = self.parse_call_after_name(name)?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::Expr(call))
                } else {
                    self.expect_symbol(';')?;
                    Ok(Stmt::Expr(Expr::Var(name)))
                }
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_if_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::If)?;
        self.expect_symbol('(')?;
        let cond = self.parse_expr()?;
        self.expect_symbol(')')?;

        let then_body = self.parse_block()?;
        let else_body = if self.current.kind == TokenKind::Keyword(Keyword::Else) {
            self.bump()?;
            // `else if (...) { ... }` desugars to `else { if (...) { ... } }`
            // so chains of else-if branches parse without curly braces in
            // between.
            if self.current.kind == TokenKind::Keyword(Keyword::If) {
                Some(vec![self.parse_if_stmt()?])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };

        Ok(Stmt::If {
            cond,
            then_body,
            else_body,
        })
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::While)?;
        self.expect_symbol('(')?;
        let cond = self.parse_expr()?;
        self.expect_symbol(')')?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_try_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::Try)?;
        let body = self.parse_block()?;

        self.expect_keyword(Keyword::Catch)?;
        self.expect_symbol('(')?;
        let catch_var = self.expect_ident("expected catch variable name")?;
        let catch_type = if self.check_symbol(':') {
            self.expect_symbol(':')?;
            Some(self.expect_ident("expected error type after ':'")?)
        } else {
            None
        };
        self.expect_symbol(')')?;
        let catch_body = self.parse_block()?;

        Ok(Stmt::Try {
            body,
            catch_var,
            catch_type,
            catch_body,
        })
    }

    fn parse_for_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::For)?;
        let var = self.expect_ident("expected variable name after 'for'")?;
        if self.current.kind != TokenKind::Keyword(Keyword::In) {
            return Err(self.error_here("expected 'in' after 'for <var>'"));
        }
        self.bump()?;
        let iter = self.parse_expr()?;
        let body = self.parse_block()?;
        Ok(Stmt::ForIn { var, iter, body })
    }

    fn parse_validate_body_stmt(&mut self) -> Result<Stmt> {
        self.expect_keyword(Keyword::Validate)?;

        let body_kw = self.expect_ident("expected 'body' after 'validate'")?;
        if !body_kw.eq_ignore_ascii_case("body") {
            return Err(self.error_here("only 'validate body' is supported"));
        }

        self.expect_symbol('{')?;

        let mut fields: Vec<ValidateField> = Vec::new();
        while !self.check_symbol('}') {
            let field_name = self.expect_ident("expected field name in validate body block")?;
            self.expect_symbol(':')?;

            let mut rules = Vec::new();
            rules.push(self.parse_validate_rule()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                rules.push(self.parse_validate_rule()?);
            }
            self.expect_symbol(';')?;

            fields.push(ValidateField {
                name: field_name,
                rules,
            });
        }

        self.expect_symbol('}')?;
        Ok(Stmt::ValidateBody { fields })
    }

    fn parse_validate_rule(&mut self) -> Result<ValidateRule> {
        let name = self.expect_ident("expected validation rule name")?;
        let lower = name.to_ascii_lowercase();

        match lower.as_str() {
            "required" => Ok(ValidateRule::Required),
            "minlength" => {
                let n = self.parse_int_arg("minLength")?;
                Ok(ValidateRule::MinLength(n))
            }
            "maxlength" => {
                let n = self.parse_int_arg("maxLength")?;
                Ok(ValidateRule::MaxLength(n))
            }
            "min" => {
                let n = self.parse_number_arg("min")?;
                Ok(ValidateRule::Min(n))
            }
            "max" => {
                let n = self.parse_number_arg("max")?;
                Ok(ValidateRule::Max(n))
            }
            "pattern" => {
                self.expect_symbol('(')?;
                let regex_src = self.expect_string("expected regex string argument for pattern")?;
                self.expect_symbol(')')?;
                if regex::Regex::new(&regex_src).is_err() {
                    return Err(self.error_here("invalid regex passed to pattern()"));
                }
                Ok(ValidateRule::Pattern(regex_src))
            }
            other => Err(self.error_here(&format!("unknown validation rule '{other}'"))),
        }
    }

    fn parse_int_arg(&mut self, rule_name: &str) -> Result<i64> {
        self.expect_symbol('(')?;
        let n = self.parse_signed_number(&format!("expected integer argument for {rule_name}"))?;
        self.expect_symbol(')')?;
        Ok(n)
    }

    fn parse_number_arg(&mut self, rule_name: &str) -> Result<String> {
        self.expect_symbol('(')?;
        let sign = if self.check_symbol('-') {
            self.expect_symbol('-')?;
            "-"
        } else {
            ""
        };
        let token = match self.current.kind.clone() {
            TokenKind::Number(v) => v,
            _ => return Err(self.error_here(&format!("expected numeric argument for {rule_name}"))),
        };
        self.bump()?;
        self.expect_symbol(')')?;
        Ok(format!("{sign}{token}"))
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_symbol('{')?;
        let mut body = Vec::new();
        while !self.check_symbol('}') {
            body.push(self.parse_stmt()?);
        }
        self.expect_symbol('}')?;
        Ok(body)
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_and_expr()?;
        while self.current.kind == TokenKind::Keyword(Keyword::Or) {
            self.bump()?;
            let right = self.parse_and_expr()?;
            expr = Expr::Or(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_and_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_eq_expr()?;
        while self.current.kind == TokenKind::Keyword(Keyword::And) {
            self.bump()?;
            let right = self.parse_eq_expr()?;
            expr = Expr::And(Box::new(expr), Box::new(right));
        }
        Ok(expr)
    }

    fn parse_eq_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_cmp_expr()?;

        loop {
            if self.check_symbol('=') {
                self.expect_symbol('=')?;
                self.expect_symbol('=')?;
                let right = self.parse_cmp_expr()?;
                expr = Expr::Eq(Box::new(expr), Box::new(right));
                continue;
            }

            if self.check_symbol('!') {
                self.expect_symbol('!')?;
                self.expect_symbol('=')?;
                let right = self.parse_cmp_expr()?;
                expr = Expr::Neq(Box::new(expr), Box::new(right));
                continue;
            }

            break;
        }

        Ok(expr)
    }

    fn parse_cmp_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_add_expr()?;

        loop {
            if self.check_symbol('<') {
                self.expect_symbol('<')?;
                if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let right = self.parse_add_expr()?;
                    expr = Expr::Lte(Box::new(expr), Box::new(right));
                } else {
                    let right = self.parse_add_expr()?;
                    expr = Expr::Lt(Box::new(expr), Box::new(right));
                }
                continue;
            }

            if self.check_symbol('>') {
                self.expect_symbol('>')?;
                if self.check_symbol('=') {
                    self.expect_symbol('=')?;
                    let right = self.parse_add_expr()?;
                    expr = Expr::Gte(Box::new(expr), Box::new(right));
                } else {
                    let right = self.parse_add_expr()?;
                    expr = Expr::Gt(Box::new(expr), Box::new(right));
                }
                continue;
            }

            break;
        }

        Ok(expr)
    }

    fn parse_add_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_mul_expr()?;
        while self.check_symbol('+') || self.check_symbol('-') {
            if self.check_symbol('+') {
                self.expect_symbol('+')?;
                let right = self.parse_mul_expr()?;
                expr = Expr::Add(Box::new(expr), Box::new(right));
            } else {
                self.expect_symbol('-')?;
                let right = self.parse_mul_expr()?;
                expr = Expr::Sub(Box::new(expr), Box::new(right));
            }
        }
        Ok(expr)
    }

    fn parse_mul_expr(&mut self) -> Result<Expr> {
        let mut expr = self.parse_unary_expr()?;
        while self.check_symbol('*') || self.check_symbol('/') || self.check_symbol('%') {
            if self.check_symbol('*') {
                self.expect_symbol('*')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Mul(Box::new(expr), Box::new(right));
            } else if self.check_symbol('/') {
                self.expect_symbol('/')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Div(Box::new(expr), Box::new(right));
            } else {
                self.expect_symbol('%')?;
                let right = self.parse_unary_expr()?;
                expr = Expr::Mod(Box::new(expr), Box::new(right));
            }
        }
        Ok(expr)
    }

    fn parse_unary_expr(&mut self) -> Result<Expr> {
        if self.check_symbol('-') {
            self.expect_symbol('-')?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Neg(Box::new(expr)));
        }
        // Unary `!`. Disambiguated from `!=` because that operator is only
        // parsed at the equality precedence level (after a primary operand),
        // never as a leading token.
        if self.check_symbol('!') {
            self.expect_symbol('!')?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Not(Box::new(expr)));
        }
        if matches!(self.current.kind, TokenKind::Keyword(Keyword::Await)) {
            self.bump()?;
            let expr = self.parse_unary_expr()?;
            return Ok(Expr::Await(Box::new(expr)));
        }
        self.parse_primary_expr()
    }

    fn parse_primary_expr(&mut self) -> Result<Expr> {
        match self.current.kind.clone() {
            TokenKind::Number(value) => {
                self.bump()?;
                if value.contains('.') {
                    Ok(Expr::Float(value))
                } else {
                    let int_value = value
                        .parse::<i64>()
                        .map_err(|_| self.error_here("invalid integer literal"))?;
                    Ok(Expr::Int(int_value))
                }
            }
            TokenKind::String(value) => {
                self.bump()?;
                Ok(Expr::Str(value))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump()?;
                Ok(Expr::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump()?;
                Ok(Expr::Bool(false))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.bump()?;
                Ok(Expr::Null)
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("select") => {
                self.bump()?;
                self.parse_select_expr()
            }
            TokenKind::TemplateStr(parts) => {
                let parts = parts.clone();
                self.bump()?;
                let mut result: Option<Expr> = None;
                for part in parts {
                    let seg = match part {
                        TemplatePart::Literal(s) => Expr::Str(s),
                        TemplatePart::Hole(src) => parse_template_hole(&src)?,
                    };
                    result = Some(match result {
                        None => seg,
                        Some(left) => Expr::Add(Box::new(left), Box::new(seg)),
                    });
                }
                Ok(result.unwrap_or(Expr::Str(String::new())))
            }
            TokenKind::Ident(name) if name.eq_ignore_ascii_case("new") => {
                self.bump()?;
                let entity = self.expect_ident("expected entity name after 'new'")?;
                self.expect_symbol('(')?;
                self.expect_symbol(')')?;
                Ok(Expr::NewEntity { entity })
            }
            TokenKind::Ident(name) => {
                self.bump()?;
                if self.check_symbol('(') {
                    self.parse_call_after_name(name)
                } else if self.check_symbol('.') {
                    self.bump()?;
                    let member = self.expect_ident("expected field or function name after '.'")?;
                    if self.check_symbol('(') {
                        self.parse_call_after_name(format!("{}.{}", name, member))
                    } else {
                        Ok(Expr::FieldGet {
                            var: name,
                            field: member,
                        })
                    }
                } else {
                    Ok(Expr::Var(name))
                }
            }
            TokenKind::Symbol('(') => {
                self.expect_symbol('(')?;
                let expr = self.parse_expr()?;
                self.expect_symbol(')')?;
                Ok(expr)
            }
            TokenKind::Symbol('{') => self.parse_object_literal(),
            TokenKind::Symbol('[') => self.parse_array_literal(),
            _ => Err(self.error_here("expected expression")),
        }
    }

    /// Parse an array literal: `[ expr [, expr]* [,]? ]`. The empty form `[]`
    /// is valid; a trailing comma is allowed. Elements may be heterogeneous.
    fn parse_array_literal(&mut self) -> Result<Expr> {
        self.expect_symbol('[')?;
        let mut items: Vec<Expr> = Vec::new();
        if !self.check_symbol(']') {
            loop {
                items.push(self.parse_expr()?);
                if !self.check_symbol(',') {
                    break;
                }
                self.expect_symbol(',')?;
                if self.check_symbol(']') {
                    break;
                }
            }
        }
        self.expect_symbol(']')?;
        Ok(Expr::ArrayLit(items))
    }

    /// Parse an expression-position object literal: `{ key: expr [, key: expr]* }`.
    /// Statement-position `{ ... }` blocks never reach this path because their
    /// callers (function body, if/while, validate body, select projection)
    /// consume the brace directly.
    fn parse_object_literal(&mut self) -> Result<Expr> {
        self.expect_symbol('{')?;
        let mut fields: Vec<(String, Expr)> = Vec::new();
        if !self.check_symbol('}') {
            loop {
                let key = self.expect_ident("expected key in object literal")?;
                self.expect_symbol(':')?;
                let value = self.parse_expr()?;
                fields.push((key, value));
                if !self.check_symbol(',') {
                    break;
                }
                self.expect_symbol(',')?;
                if self.check_symbol('}') {
                    break;
                }
            }
        }
        self.expect_symbol('}')?;
        Ok(Expr::ObjectLit(fields))
    }

    fn parse_call_after_name(&mut self, name: String) -> Result<Expr> {
        self.expect_symbol('(')?;
        let mut args = Vec::new();
        if !self.check_symbol(')') {
            args.push(self.parse_expr()?);
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                args.push(self.parse_expr()?);
            }
        }
        self.expect_symbol(')')?;
        Ok(Expr::Call { name, args })
    }

    /// Parse `CTX.TABLE` — returns `(context_var, table)`
    fn parse_db_ref(&mut self) -> Result<(String, String)> {
        let ctx = self.expect_ident("expected context variable")?;
        self.expect_symbol('.')?;
        let table = self.expect_ident("expected table name after '.'")?;
        Ok((ctx, table))
    }

    /// Parse field path: `ident` or `ident.ident` — returns the full string
    fn parse_field_path(&mut self) -> Result<String> {
        let first = self.expect_ident("expected field name")?;
        if self.check_symbol('.') {
            self.bump()?;
            let second = self.expect_ident("expected field name after '.'")?;
            Ok(format!("{}.{}", first, second))
        } else {
            Ok(first)
        }
    }

    /// Parse `select [Entity|*|count(*)] from CTX.TABLE
    ///        [where COND [and|or COND ...]]
    ///        [orderby FIELD [asc|desc]]
    ///        [limit N] [offset N] [first]`
    fn parse_select_expr(&mut self) -> Result<Expr> {
        // `count(*)` aggregation form
        if self.check_ident_eq("count") {
            self.bump()?;
            self.expect_symbol('(')?;
            self.expect_symbol('*')?;
            self.expect_symbol(')')?;

            let from_kw = self.expect_ident("expected 'from' after count(*)")?;
            if !from_kw.eq_ignore_ascii_case("from") {
                return Err(self.error_here("expected 'from' after count(*)"));
            }
            let (ctx, table) = self.parse_db_ref()?;
            let where_clause = if self.check_ident_eq("where") {
                self.bump()?;
                Some(Box::new(self.parse_where_or()?))
            } else {
                None
            };
            return Ok(Expr::DbCount {
                context_var: ctx,
                table,
                where_clause,
            });
        }

        // `sum|avg|min|max(Entity.col)` aggregation form
        let agg_kind = if self.check_ident_eq("sum") {
            Some(AggregateKind::Sum)
        } else if self.check_ident_eq("avg") {
            Some(AggregateKind::Avg)
        } else if self.check_ident_eq("min") {
            Some(AggregateKind::Min)
        } else if self.check_ident_eq("max") {
            Some(AggregateKind::Max)
        } else {
            None
        };
        if let Some(kind) = agg_kind {
            self.bump()?;
            self.expect_symbol('(')?;
            let field = self.parse_field_path()?;
            self.expect_symbol(')')?;
            let from_kw = self.expect_ident("expected 'from' after aggregate(...)")?;
            if !from_kw.eq_ignore_ascii_case("from") {
                return Err(self.error_here("expected 'from' after aggregate(...)"));
            }
            let (ctx, table) = self.parse_db_ref()?;
            let where_clause = if self.check_ident_eq("where") {
                self.bump()?;
                Some(Box::new(self.parse_where_or()?))
            } else {
                None
            };
            return Ok(Expr::DbAggregate {
                kind,
                field,
                context_var: ctx,
                table,
                where_clause,
            });
        }

        // entity name or `*`
        let entity = if self.check_symbol('*') {
            self.bump()?;
            "*".to_string()
        } else {
            self.expect_ident("expected entity name or '*' after 'select'")?
        };

        // optional `{ col1, col2, ... }` projection
        let projection = if self.check_symbol('{') {
            self.expect_symbol('{')?;
            if entity == "*" {
                return Err(
                    self.error_here("projection `{ ... }` requires a named entity, not '*'")
                );
            }
            let mut cols = Vec::new();
            if !self.check_symbol('}') {
                cols.push(self.expect_ident("expected column name in projection")?);
                while self.check_symbol(',') {
                    self.expect_symbol(',')?;
                    cols.push(self.expect_ident("expected column name after ','")?);
                }
            }
            self.expect_symbol('}')?;
            if cols.is_empty() {
                return Err(self.error_here("projection `{ ... }` must list at least one column"));
            }
            cols
        } else {
            Vec::new()
        };

        // optional `with rel1, rel2, ...`
        let with_relations = if self.check_ident_eq("with") {
            self.bump()?;
            let mut rels = vec![self.expect_ident("expected navigation name after 'with'")?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                rels.push(self.expect_ident("expected navigation name after ','")?);
            }
            rels
        } else {
            Vec::new()
        };

        // `from`
        let from_kw = self.expect_ident("expected 'from' after entity name")?;
        if !from_kw.eq_ignore_ascii_case("from") {
            return Err(self.error_here("expected 'from' in select expression"));
        }

        let (ctx, table) = self.parse_db_ref()?;

        // optional `where COND [and|or COND ...]`
        let where_clause = if self.check_ident_eq("where") {
            self.bump()?;
            Some(Box::new(self.parse_where_or()?))
        } else {
            None
        };

        // optional `group by Entity.col [, Entity.col ...]`
        // `group` is a lexer keyword (used by `parse_group_block` at the
        // top level) so we match by TokenKind rather than identifier text.
        // `by` is read as a plain identifier.
        let group_by = if self.current.kind == TokenKind::Keyword(Keyword::Group) {
            self.bump()?;
            let by_kw = self.expect_ident("expected 'by' after 'group'")?;
            if !by_kw.eq_ignore_ascii_case("by") {
                return Err(self.error_here("expected 'by' after 'group'"));
            }
            let mut cols = vec![self.parse_field_path()?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                cols.push(self.parse_field_path()?);
            }
            cols
        } else {
            Vec::new()
        };

        // optional `having COND [and|or COND ...]` — only meaningful after a
        // `group by`, but the parser is permissive; validate_program enforces
        // the dependency.
        let having = if self.check_ident_eq("having") {
            self.bump()?;
            Some(Box::new(self.parse_where_or()?))
        } else {
            None
        };

        // optional `orderby FIELD [asc|desc]`
        let order_by = if self.check_ident_eq("orderby") {
            self.bump()?;
            let field = self.parse_field_path()?;
            let dir = if self.check_ident_eq("desc") {
                self.bump()?;
                SortDir::Desc
            } else if self.check_ident_eq("asc") {
                self.bump()?;
                SortDir::Asc
            } else {
                SortDir::Asc
            };
            Some(DbOrderBy { field, dir })
        } else {
            None
        };

        // optional `limit N`
        let limit = if self.check_ident_eq("limit") {
            self.bump()?;
            Some(Box::new(self.parse_db_int_arg("limit")?))
        } else {
            None
        };

        // optional `offset N`
        let offset = if self.check_ident_eq("offset") {
            self.bump()?;
            Some(Box::new(self.parse_db_int_arg("offset")?))
        } else {
            None
        };

        // optional `first`
        let first = if self.check_ident_eq("first") {
            self.bump()?;
            true
        } else {
            false
        };

        Ok(Expr::DbSelect {
            entity,
            context_var: ctx,
            table,
            where_clause,
            order_by,
            limit,
            offset,
            first,
            with_relations,
            projection,
            group_by,
            having,
        })
    }

    fn parse_where_or(&mut self) -> Result<WhereExpr> {
        let mut left = self.parse_where_and()?;
        while self.current.kind == TokenKind::Keyword(Keyword::Or) {
            self.bump()?;
            let right = self.parse_where_and()?;
            left = WhereExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_where_and(&mut self) -> Result<WhereExpr> {
        let mut left = self.parse_where_atom()?;
        while self.current.kind == TokenKind::Keyword(Keyword::And) {
            self.bump()?;
            let right = self.parse_where_atom()?;
            left = WhereExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_where_atom(&mut self) -> Result<WhereExpr> {
        if self.check_symbol('(') {
            self.expect_symbol('(')?;
            let inner = self.parse_where_or()?;
            self.expect_symbol(')')?;
            return Ok(inner);
        }

        let field = self.parse_field_path()?;

        if self.check_ident_eq("between") {
            self.bump()?;
            let low = self.parse_in_value()?;
            if self.current.kind != TokenKind::Keyword(Keyword::And) {
                return Err(
                    self.error_here("expected 'and' between bounds in 'between ... and ...'")
                );
            }
            self.bump()?;
            let high = self.parse_in_value()?;
            return Ok(WhereExpr::Between { field, low, high });
        }

        if self.current.kind == TokenKind::Keyword(Keyword::In) {
            self.bump()?;
            self.expect_symbol('(')?;
            let mut values = Vec::new();
            if !self.check_symbol(')') {
                values.push(self.parse_in_value()?);
                while self.check_symbol(',') {
                    self.expect_symbol(',')?;
                    values.push(self.parse_in_value()?);
                }
            }
            self.expect_symbol(')')?;
            if values.is_empty() {
                return Err(self.error_here("'in (...)' must list at least one value"));
            }
            return Ok(WhereExpr::InList { field, values });
        }

        // `is null` / `is not null` — surface syntax for IS NULL / IS NOT NULL.
        // Encoded as an Atom with rhs=Null and op `==`/`!=`; the SQL builder
        // already translates that to IS NULL / IS NOT NULL.
        if self.check_ident_eq("is") {
            self.bump()?;
            let negated = if self.check_ident_eq("not") {
                self.bump()?;
                true
            } else {
                false
            };
            if !self.check_ident_eq("null") {
                return Err(self.error_here("expected 'null' after 'is' or 'is not'"));
            }
            self.bump()?;
            let op = if negated { "!=" } else { "==" }.to_string();
            return Ok(WhereExpr::Atom(DbWhere {
                field,
                op,
                rhs: Expr::Null,
            }));
        }

        let op = if self.check_ident_eq("like") {
            self.bump()?;
            "like".to_string()
        } else if self.check_ident_eq("ilike") {
            self.bump()?;
            "ilike".to_string()
        } else {
            self.parse_cmp_op()?
        };

        let rhs = self.parse_at_or_expr()?;
        Ok(WhereExpr::Atom(DbWhere { field, op, rhs }))
    }

    fn parse_in_value(&mut self) -> Result<Expr> {
        self.parse_at_or_expr()
    }

    /// Parse the RHS of a where comparison / `in` value. `@name` is shorthand
    /// for an `Expr::Var`; `@name.field` extends it to `Expr::FieldGet` so
    /// callers can write `where User.id == @req.userId first;` instead of
    /// staging an extra `let` binding.
    fn parse_at_or_expr(&mut self) -> Result<Expr> {
        if !self.check_symbol('@') {
            return self.parse_expr();
        }
        self.bump()?;
        let name = self.expect_ident("expected parameter name after '@'")?;
        if self.check_symbol('.') {
            self.bump()?;
            let field = self.expect_ident("expected field name after '@var.'")?;
            Ok(Expr::FieldGet { var: name, field })
        } else {
            Ok(Expr::Var(name))
        }
    }

    /// Accepts `@param`, integer literal, or any expression — runtime ensures
    /// the value is an integer when binding to LIMIT/OFFSET.
    fn parse_db_int_arg(&mut self, clause: &str) -> Result<Expr> {
        if self.check_symbol('@') {
            self.bump()?;
            let name =
                self.expect_ident(&format!("expected parameter name after '@' in {clause}"))?;
            return Ok(Expr::Var(name));
        }
        self.parse_expr()
    }

    fn check_ident_eq(&self, expected: &str) -> bool {
        matches!(&self.current.kind, TokenKind::Ident(v) if v.eq_ignore_ascii_case(expected))
    }

    /// Parse a comparison operator token sequence: `=`, `==`, `!=`, `<`, `<=`, `>`, `>=`
    fn parse_cmp_op(&mut self) -> Result<String> {
        if self.check_symbol('=') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok("==".to_string())
            } else {
                Ok("=".to_string())
            }
        } else if self.check_symbol('!') {
            self.bump()?;
            self.expect_symbol('=')?;
            Ok("!=".to_string())
        } else if self.check_symbol('<') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok("<=".to_string())
            } else {
                Ok("<".to_string())
            }
        } else if self.check_symbol('>') {
            self.bump()?;
            if self.check_symbol('=') {
                self.bump()?;
                Ok(">=".to_string())
            } else {
                Ok(">".to_string())
            }
        } else {
            Err(self.error_here("expected comparison operator in where clause"))
        }
    }

    fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current.kind, TokenKind::Symbol(c) if c == expected)
    }

    fn expect_symbol(&mut self, expected: char) -> Result<()> {
        if self.check_symbol(expected) {
            self.bump()?;
            Ok(())
        } else {
            Err(self.error_here(&format!("expected '{}'", expected)))
        }
    }

    fn expect_keyword(&mut self, expected: Keyword) -> Result<()> {
        if self.current.kind == TokenKind::Keyword(expected.clone()) {
            self.bump()?;
            Ok(())
        } else {
            Err(self.error_here("unexpected token"))
        }
    }

    fn expect_ident(&mut self, msg: &str) -> Result<String> {
        match &self.current.kind {
            TokenKind::Ident(value) => {
                let value = value.clone();
                self.bump()?;
                Ok(value)
            }
            _ => Err(self.error_here(msg)),
        }
    }

    fn expect_number(&mut self, msg: &str) -> Result<i64> {
        match self.current.kind.clone() {
            TokenKind::Number(value) => {
                self.bump()?;
                if value.contains('.') {
                    return Err(self.error_here("expected integer number"));
                }
                value.parse::<i64>().map_err(|_| self.error_here(msg))
            }
            _ => Err(self.error_here(msg)),
        }
    }

    fn expect_string(&mut self, msg: &str) -> Result<String> {
        match self.current.kind.clone() {
            TokenKind::String(value) => {
                self.bump()?;
                Ok(value)
            }
            _ => Err(self.error_here(msg)),
        }
    }

    fn bump(&mut self) -> Result<()> {
        self.current = self.lexer.next_token()?;
        Ok(())
    }

    fn error_here(&self, msg: &str) -> anyhow::Error {
        let (line, col) = self.source_map.line_col(self.current.offset);
        anyhow!("{msg} at line {line}, col {col}")
    }
}

/// Parse a single expression from a template string hole source, e.g. `env("PG_USER")`.
fn parse_template_hole(src: &str) -> Result<Expr> {
    let mut p = Parser::new(src.trim())?;
    let expr = p.parse_expr()?;
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
