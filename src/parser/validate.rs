//! Semantic validation for parsed programs.
//!
//! This module holds [`validate_program`] (the entry point) plus all of its
//! local helpers — duplicate detection, navigation/foreign-key checking,
//! constant-expression rules, mutation-field tracking against `new Entity()`
//! bindings, typed-parameter field-access checking, and the
//! navigation-relation (`with`) walk.
//!
//! The body-level statement/expression walk (`validate_stmts`, `validate_stmt`,
//! `validate_expr`, the WHERE-clause validators) and the dbcontext / entity /
//! type-spec resolution helpers live in
//! [`super::validate_walk`] so each file stays under the per-file budget.
//!
//! Everything here is a free function over the AST; nothing touches the
//! `Parser` state object.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{
    Expr, FunctionDecl, ImportDecl, ModelKind, NavigationField, Program, Stmt, Visibility,
    WhereExpr,
};
use crate::diag::SourceMap;

use super::validate_walk::{
    resolve_entity_context_name, resolve_entity_driver, table_matches_entity, to_snake_case,
    validate_stmts, validate_type_spec_for_driver,
};

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

/// Render `<msg> at <file>:line:col\n<snippet>` when a decl carries a
/// real offset AND its [`Program::sources`] entry has a non-empty text
/// buffer. Falls back to the bare `<msg>` shape when offset is `0`, the
/// `file_idx` is out of range, or the source text for that file is empty
/// — hand-built `Program::default()` instances in tests stay legible.
/// The label is omitted when empty (single-file `parse_program` calls
/// leave it blank), keeping single-file output identical to the previous
/// `at line X, col Y` shape so the LSP regex in
/// `src/bin/jwc_lsp.rs::extract_line_col` still resolves.
fn loc_in(program: &Program, file_idx: usize, offset: usize, msg: &str) -> anyhow::Error {
    let Some(file) = program.sources.get(file_idx) else {
        return anyhow!("{msg}");
    };
    if offset == 0 || file.text.is_empty() || offset >= file.text.len() {
        return anyhow!("{msg}");
    }
    let sm = SourceMap::new(&file.text);
    let (line, col) = sm.line_col(offset);
    let snippet = sm.snippet(offset);
    if file.label.is_empty() {
        anyhow!("{msg} at line {line}, col {col}{snippet}")
    } else {
        anyhow!(
            "{msg} at {file}:{line}:{col}{snippet}",
            file = file.label,
            line = line,
            col = col,
            snippet = snippet
        )
    }
}

pub fn validate_program(program: &Program) -> Result<()> {
    let mut ctx_names = HashSet::new();
    let mut ctx_drivers: HashMap<String, String> = HashMap::new();
    for ctx in &program.dbcontexts {
        let key = ns_fqn(&ctx.namespace, &ctx.name);
        if !ctx_names.insert(key) {
            return Err(loc_in(
                program,
                ctx.file_idx,
                ctx.offset,
                &format!("Duplicate dbcontext name: {}", ctx.name),
            ));
        }
        if ctx.driver.trim().is_empty() {
            return Err(loc_in(
                program,
                ctx.file_idx,
                ctx.offset,
                &format!("dbcontext '{}' has empty driver", ctx.name),
            ));
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
            return Err(loc_in(
                program,
                model.file_idx,
                model.offset,
                &format!("Duplicate model name: {}", model.name),
            ));
        }

        if model.kind != ModelKind::Entity {
            continue;
        }
        let key = ns_fqn(&model.namespace, &model.name);
        if !entity_names.insert(key) {
            return Err(loc_in(
                program,
                model.file_idx,
                model.offset,
                &format!("Duplicate entity name: {}", model.name),
            ));
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
            if let crate::ast::NavigationKind::ManyToMany = nav.kind {
                // m2m: the near/far FK columns live on the join table.
                let j = nav
                    .join
                    .as_ref()
                    .expect("ManyToMany navigation must carry join-table coordinates");
                let jt_key = j.table.to_lowercase();
                let join_tbl = program
                    .models
                    .iter()
                    .find(|m| m.kind == ModelKind::Entity && m.name.to_lowercase() == jt_key)
                    .ok_or_else(|| {
                        anyhow!(
                            "Entity '{}' navigation '{}' references unknown join table '{}'",
                            model.name,
                            nav.name,
                            j.table
                        )
                    })?;
                for (col, which) in [(&j.near_col, "near"), (&j.far_col, "far")] {
                    if !join_tbl
                        .fields
                        .iter()
                        .any(|f| f.name.eq_ignore_ascii_case(col))
                    {
                        bail!(
                            "Entity '{}' navigation '{}': join table '{}' has no {} column '{}'",
                            model.name,
                            nav.name,
                            j.table,
                            which,
                            col
                        );
                    }
                }
            } else {
                // The join FK column lives on the target for has-many/has-one,
                // but on *this* entity for belongs-to (this entity holds the FK).
                let (fk_owner, fk_owner_name) = match nav.kind {
                    crate::ast::NavigationKind::BelongsTo => (model, model.name.as_str()),
                    _ => (target, nav.target_entity.as_str()),
                };
                let field_key = nav.target_field.to_lowercase();
                if !fk_owner
                    .fields
                    .iter()
                    .any(|f| f.name.to_lowercase() == field_key)
                {
                    bail!(
                        "Entity '{}' navigation '{}' references unknown column '{}.{}'",
                        model.name,
                        nav.name,
                        fk_owner_name,
                        nav.target_field
                    );
                }
            }
            // Projected nav columns must exist on the target entity.
            for col in &nav.projection {
                let col_key = col.to_lowercase();
                if !target
                    .fields
                    .iter()
                    .any(|f| f.name.to_lowercase() == col_key)
                {
                    bail!(
                        "Entity '{}' navigation '{}' projection references unknown column '{}.{}'",
                        model.name,
                        nav.name,
                        nav.target_entity,
                        col
                    );
                }
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
            return Err(loc_in(
                program,
                function.file_idx,
                function.offset,
                &format!("error[E015]: Duplicate function name: {}", function.name),
            ));
        }

        let mut param_names = HashSet::new();
        for param in &function.params {
            let param_key = param.name.to_lowercase();
            if !param_names.insert(param_key) {
                return Err(loc_in(
                    program,
                    function.file_idx,
                    function.offset,
                    &format!(
                        "Function '{}': duplicate parameter '{}'",
                        function.name, param.name
                    ),
                ));
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
            return Err(loc_in(
                program,
                route.file_idx,
                route.offset,
                &format!("Unsupported route method: {}", route.method),
            ));
        }

        let key = format!("{} {}", method, route.path);
        if !route_keys.insert(key) {
            return Err(loc_in(
                program,
                route.file_idx,
                route.offset,
                &format!("error[E005]: Duplicate route: {} {}", method, route.path),
            ));
        }

        if route.handler.is_some() && !route.body.is_empty() {
            return Err(loc_in(
                program,
                route.file_idx,
                route.offset,
                "Route cannot define both handler and inline body",
            ));
        }
        if route.handler.is_none() && route.body.is_empty() {
            return Err(loc_in(
                program,
                route.file_idx,
                route.offset,
                "Route must define either handler or inline body",
            ));
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
                    return Err(loc_in(
                        program,
                        route.file_idx,
                        route.offset,
                        &format!(
                            "error[E014]: Route handler '{}' is not defined as a function",
                            handler
                        ),
                    ));
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
            return Err(loc_in(
                program,
                mw.file_idx,
                mw.offset,
                &format!("Duplicate middleware name: {}", mw.name),
            ));
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

    validate_consts(program)?;

    // Cross-namespace visibility. AOT codegen (`src/native_build.rs`) lowers
    // every `Expr::Call` to a flat Rust call where every emitted function is
    // crate-public — the AST `Visibility::Private` marker has no Rust-level
    // analogue. So if a `private function helper()` declared in namespace A is
    // referenced from namespace B, the native build would silently link
    // anyway. `validate_program` is the single static gate for that rule —
    // both the interpreter (`runner::check_visibility`) and the AOT path
    // depend on this pass having rejected the program already.
    //
    // See `docs/spec/visibility.md` for the full surface description.
    check_visibility(program)?;

    Ok(())
}

/// Build the function lookup table the static visibility pass uses:
/// FQN (lowercased, dot-joined) → reference to the decl. Same key shape as
/// `runner::fqn_key` so the static resolver can mirror runtime resolution
/// exactly.
fn build_fn_table(program: &Program) -> HashMap<String, &FunctionDecl> {
    let mut out: HashMap<String, &FunctionDecl> = HashMap::new();
    for f in &program.functions {
        let key = ns_fqn(&f.namespace, &f.name);
        out.insert(key, f);
    }
    out
}

/// Group import declarations by the namespace that contains them, mirroring
/// `Vm::imports_by_namespace`. The key is the dot-joined lowercased namespace
/// (empty string = root); the value is the list of `using <path>;` entries
/// declared in any file that lives in that namespace.
fn build_imports_by_ns(program: &Program) -> HashMap<String, Vec<&ImportDecl>> {
    let mut out: HashMap<String, Vec<&ImportDecl>> = HashMap::new();
    for imp in &program.imports {
        let ns_key = imp
            .in_namespace
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        out.entry(ns_key).or_default().push(imp);
    }
    out
}

/// Static analogue of `Vm::resolve_function` (src/runner/mod.rs). Returns the
/// callee a runtime resolver would pick for `name` when called from
/// `caller_ns`. Walks the same priority chain:
///   1. exact FQN match in the function table (handles both `pkg.fn` calls
///      and root-namespace bare names);
///   2. caller's own namespace;
///   3. each `import` declared in the caller's namespace;
///   4. nothing (we do NOT raise here — `validate_program`'s earlier
///      route-handler / call-site checks already cover "undefined function";
///      a resolution miss in the visibility pass just skips visibility for
///      this call site).
fn resolve_callee<'a>(
    name: &str,
    caller_ns: &[String],
    fn_table: &HashMap<String, &'a FunctionDecl>,
    imports_by_ns: &HashMap<String, Vec<&ImportDecl>>,
) -> Option<&'a FunctionDecl> {
    let lc = name.to_lowercase();
    if let Some(f) = fn_table.get(&lc) {
        return Some(*f);
    }
    if name.contains('.') {
        // Explicit FQN that didn't match — fall through to "unresolved", same
        // as the runtime resolver.
        return None;
    }
    if !caller_ns.is_empty() {
        let key = ns_fqn(caller_ns, name);
        if let Some(f) = fn_table.get(&key) {
            return Some(*f);
        }
    }
    let ns_key = caller_ns
        .iter()
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(".");
    if let Some(imps) = imports_by_ns.get(&ns_key) {
        for imp in imps {
            let key = ns_fqn(&imp.path, name);
            if let Some(f) = fn_table.get(&key) {
                return Some(*f);
            }
        }
    }
    None
}

/// Walk every function body, route body, middleware body, and route-handler
/// reference and reject cross-namespace calls into `private function` decls.
///
/// This is the static analogue of `runner::Vm::check_visibility` (see
/// `src/runner/mod.rs::check_visibility`). The runtime check exists for
/// safety, but the AOT codegen path does not call into the interpreter, so
/// without this static pass `jwc build --native` would silently emit a
/// private-call edge as a plain Rust function call.
fn check_visibility(program: &Program) -> Result<()> {
    let fn_table = build_fn_table(program);
    let imports_by_ns = build_imports_by_ns(program);

    // Functions — caller_ns = the declaring namespace of the function.
    for function in &program.functions {
        let label = format!("Function '{}'", function.name);
        check_visibility_in_stmts(
            &function.body,
            &function.namespace,
            &fn_table,
            &imports_by_ns,
            &label,
        )?;
    }

    // Routes — caller_ns = the declaring namespace of the route. Routes from
    // a library namespace are activated by `mount`, but their bodies'
    // calls resolve against the namespace the route was DECLARED in.
    // Handler refs (`route GET "/x" -> someFn;`) are an implicit call site.
    for route in &program.routes {
        let label = format!("Route {} {}", route.method.to_ascii_uppercase(), route.path);
        if let Some(handler) = &route.handler {
            check_handler_visibility(handler, &route.namespace, &fn_table, &imports_by_ns, &label)?;
        } else {
            check_visibility_in_stmts(
                &route.body,
                &route.namespace,
                &fn_table,
                &imports_by_ns,
                &label,
            )?;
        }
    }

    // Middlewares — caller_ns = the declaring namespace of the middleware.
    for mw in &program.middlewares {
        let label = format!("Middleware '{}'", mw.name);
        check_visibility_in_stmts(&mw.body, &mw.namespace, &fn_table, &imports_by_ns, &label)?;
        if let Some(after) = &mw.after_body {
            check_visibility_in_stmts(after, &mw.namespace, &fn_table, &imports_by_ns, &label)?;
        }
    }

    // errorHandler lives at the root namespace (it's a project-level fallback).
    if let Some(handler) = &program.error_handler {
        let root: Vec<String> = Vec::new();
        check_visibility_in_stmts(
            &handler.body,
            &root,
            &fn_table,
            &imports_by_ns,
            "errorHandler",
        )?;
    }

    Ok(())
}

/// Reject `route ... -> handler;` references that point at a `private`
/// function declared in a different namespace.
fn check_handler_visibility(
    handler: &str,
    caller_ns: &[String],
    fn_table: &HashMap<String, &FunctionDecl>,
    imports_by_ns: &HashMap<String, Vec<&ImportDecl>>,
    label: &str,
) -> Result<()> {
    if let Some(callee) = resolve_callee(handler, caller_ns, fn_table, imports_by_ns) {
        emit_if_private_across_ns(callee, caller_ns, label)?;
    }
    Ok(())
}

/// Recursive visibility walker over a statement list.
fn check_visibility_in_stmts(
    stmts: &[Stmt],
    caller_ns: &[String],
    fn_table: &HashMap<String, &FunctionDecl>,
    imports_by_ns: &HashMap<String, Vec<&ImportDecl>>,
    label: &str,
) -> Result<()> {
    for stmt in stmts {
        check_visibility_in_stmt(stmt, caller_ns, fn_table, imports_by_ns, label)?;
    }
    Ok(())
}

fn check_visibility_in_stmt(
    stmt: &Stmt,
    caller_ns: &[String],
    fn_table: &HashMap<String, &FunctionDecl>,
    imports_by_ns: &HashMap<String, Vec<&ImportDecl>>,
    label: &str,
) -> Result<()> {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } => {
            check_visibility_in_expr(value, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::FieldAssign { value, .. } => {
            check_visibility_in_expr(value, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::Print(e) => check_visibility_in_expr(e, caller_ns, fn_table, imports_by_ns, label),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            check_visibility_in_expr(cond, caller_ns, fn_table, imports_by_ns, label)?;
            check_visibility_in_stmts(then_body, caller_ns, fn_table, imports_by_ns, label)?;
            if let Some(eb) = else_body {
                check_visibility_in_stmts(eb, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            check_visibility_in_expr(cond, caller_ns, fn_table, imports_by_ns, label)?;
            check_visibility_in_stmts(body, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::Break | Stmt::Continue => Ok(()),
        Stmt::Expr(e) => check_visibility_in_expr(e, caller_ns, fn_table, imports_by_ns, label),
        Stmt::Return(opt) => {
            if let Some(e) = opt {
                check_visibility_in_expr(e, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
        }
        Stmt::ValidateBody { .. } => Ok(()),
        Stmt::Try {
            body, catch_body, ..
        } => {
            check_visibility_in_stmts(body, caller_ns, fn_table, imports_by_ns, label)?;
            check_visibility_in_stmts(catch_body, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::Transaction { body } => {
            check_visibility_in_stmts(body, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::Savepoint { body, .. } => {
            check_visibility_in_stmts(body, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::ForIn { iter, body, .. } => {
            check_visibility_in_expr(iter, caller_ns, fn_table, imports_by_ns, label)?;
            check_visibility_in_stmts(body, caller_ns, fn_table, imports_by_ns, label)
        }
        Stmt::DbInsert { .. }
        | Stmt::DbUpdate { .. }
        | Stmt::DbDelete { .. }
        | Stmt::DbDeleteWhere { .. } => Ok(()),
        Stmt::DbUpdateSet { assignments, .. } => {
            for (_col, rhs) in assignments {
                check_visibility_in_expr(rhs, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
        }
    }
}

fn check_visibility_in_expr(
    expr: &Expr,
    caller_ns: &[String],
    fn_table: &HashMap<String, &FunctionDecl>,
    imports_by_ns: &HashMap<String, Vec<&ImportDecl>>,
    label: &str,
) -> Result<()> {
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_)
        | Expr::FieldGet { .. }
        | Expr::NewEntity { .. }
        | Expr::DbCount { .. }
        | Expr::DbAggregate { .. }
        | Expr::DbSelect { .. } => Ok(()),
        Expr::Call { name, args } => {
            // Builtins are not user functions; visibility doesn't apply.
            if !crate::builtins::is_builtin(name) {
                if let Some(callee) = resolve_callee(name, caller_ns, fn_table, imports_by_ns) {
                    emit_if_private_across_ns(callee, caller_ns, label)?;
                }
            }
            for a in args {
                check_visibility_in_expr(a, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
        }
        Expr::Await(inner) | Expr::Not(inner) | Expr::Neg(inner) => {
            check_visibility_in_expr(inner, caller_ns, fn_table, imports_by_ns, label)
        }
        Expr::ObjectLit(entries) => {
            for (_k, v) in entries {
                check_visibility_in_expr(v, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
        }
        Expr::ArrayLit(items) => {
            for it in items {
                check_visibility_in_expr(it, caller_ns, fn_table, imports_by_ns, label)?;
            }
            Ok(())
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
            check_visibility_in_expr(a, caller_ns, fn_table, imports_by_ns, label)?;
            check_visibility_in_expr(b, caller_ns, fn_table, imports_by_ns, label)
        }
    }
}

/// Fire the E021 diagnostic when `callee` is `Private` and lives in a
/// namespace different from `caller_ns`. Same logic as
/// `runner::check_visibility` but reported as a static error.
fn emit_if_private_across_ns(
    callee: &FunctionDecl,
    caller_ns: &[String],
    label: &str,
) -> Result<()> {
    if callee.namespace == caller_ns {
        return Ok(());
    }
    if matches!(callee.visibility, Visibility::Public) {
        return Ok(());
    }
    let callee_ns = if callee.namespace.is_empty() {
        "<root>".to_string()
    } else {
        callee.namespace.join(".")
    };
    let caller_ns_str = if caller_ns.is_empty() {
        "<root>".to_string()
    } else {
        caller_ns.join(".")
    };
    bail!(
        "error[E021]: {label}: function '{}' is private to namespace '{}' and cannot be called from '{}'",
        callee.name,
        callee_ns,
        caller_ns_str,
    );
}

/// Enforce the module-level `const` invariants: no duplicates, every value is
/// a constant expression (literals / other consts / arithmetic / comparison /
/// logical ops / unary `-`/`!` / array & object literals), and no circular
/// references (including self-reference like `const X = X + 1`).
fn validate_consts(program: &Program) -> Result<()> {
    // Declared const names, lowercased. Duplicate detection runs alongside.
    let mut const_names: HashSet<String> = HashSet::new();
    for c in &program.consts {
        let key = c.name.to_lowercase();
        if !const_names.insert(key) {
            return Err(loc_in(
                program,
                c.file_idx,
                c.offset,
                &format!("duplicate const declaration: {}", c.name),
            ));
        }
    }

    // (a) Every const value must be a constant expression.
    for c in &program.consts {
        validate_const_expr(&c.expr, &const_names)
            .map_err(|err| anyhow!("const '{}' must be a constant expression ({err})", c.name))?;
    }

    // (b) Cycle / self-reference detection over the const dependency graph.
    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    for c in &program.consts {
        let mut refs = const_expr_var_refs(&c.expr);
        refs.retain(|n| const_names.contains(n));
        deps.insert(c.name.to_lowercase(), refs);
    }
    // DFS coloring: 0 = unvisited, 1 = visiting (on stack), 2 = done.
    let mut color: HashMap<String, u8> = HashMap::new();
    for c in &program.consts {
        let key = c.name.to_lowercase();
        if color.get(&key).copied().unwrap_or(0) == 0 && const_has_cycle(&key, &deps, &mut color) {
            return Err(loc_in(
                program,
                c.file_idx,
                c.offset,
                &format!("circular const reference involving '{}'", c.name),
            ));
        }
    }

    Ok(())
}

/// Recursively verify `expr` is a constant expression. `const_names` holds the
/// lowercased names of all declared consts; a `Var` is allowed only when it
/// names one of them. Returns a short description of the offending construct on
/// failure (the caller prefixes it with the const name).
fn validate_const_expr(expr: &Expr, const_names: &HashSet<String>) -> Result<()> {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => Ok(()),
        Expr::Var(name) => {
            if const_names.contains(&name.to_lowercase()) {
                Ok(())
            } else {
                bail!("const expression may only reference other consts, not '{name}'")
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
            validate_const_expr(a, const_names)?;
            validate_const_expr(b, const_names)
        }
        Expr::Neg(inner) | Expr::Not(inner) => validate_const_expr(inner, const_names),
        Expr::ArrayLit(items) => {
            for item in items {
                validate_const_expr(item, const_names)?;
            }
            Ok(())
        }
        Expr::ObjectLit(pairs) => {
            for (_, value) in pairs {
                validate_const_expr(value, const_names)?;
            }
            Ok(())
        }
        Expr::Call { .. }
        | Expr::DbSelect { .. }
        | Expr::DbCount { .. }
        | Expr::DbAggregate { .. }
        | Expr::FieldGet { .. }
        | Expr::NewEntity { .. }
        | Expr::Await(_) => {
            bail!("function calls / DB queries / field access are not allowed")
        }
    }
}

/// Collect the lowercased names of every `Expr::Var` referenced inside `expr`,
/// recursing through the const-allowed expression shapes. Used to build the
/// const dependency graph for cycle detection.
fn const_expr_var_refs(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_const_var_refs(expr, &mut out);
    out
}

fn collect_const_var_refs(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Var(name) => out.push(name.to_lowercase()),
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
            collect_const_var_refs(a, out);
            collect_const_var_refs(b, out);
        }
        Expr::Neg(inner) | Expr::Not(inner) => collect_const_var_refs(inner, out),
        Expr::ArrayLit(items) => {
            for item in items {
                collect_const_var_refs(item, out);
            }
        }
        Expr::ObjectLit(pairs) => {
            for (_, value) in pairs {
                collect_const_var_refs(value, out);
            }
        }
        _ => {}
    }
}

/// DFS over the const dependency graph; returns true if a cycle is reachable
/// from `node`. `color` records 1 = on the current stack, 2 = fully explored.
fn const_has_cycle(
    node: &str,
    deps: &HashMap<String, Vec<String>>,
    color: &mut HashMap<String, u8>,
) -> bool {
    color.insert(node.to_string(), 1);
    if let Some(children) = deps.get(node) {
        for child in children {
            match color.get(child).copied().unwrap_or(0) {
                1 => return true,
                0 if const_has_cycle(child, deps, color) => return true,
                _ => {}
            }
        }
    }
    color.insert(node.to_string(), 2);
    false
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
        | Stmt::DbUpdateSet { .. }
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
        Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
            check_mutation_fields_in_stmts(body, bindings, entity_fields)
        }
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
        Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
            check_typed_field_access_in_stmts(body, locals, model_fields)
        }
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
        | Stmt::DbDeleteWhere { .. }
        | Stmt::DbUpdateSet { .. } => Ok(()),
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
        Stmt::Transaction { body } | Stmt::Savepoint { body, .. } => {
            check_with_relations_in_stmts(body, entity_navigations)
        }
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
                    // Dotted `with parent.child` validates the head on this
                    // entity, then the tail on the head nav's target entity.
                    let (head, tail) = match rel.split_once('.') {
                        Some((h, t)) => (h, Some(t)),
                        None => (rel.as_str(), None),
                    };
                    let head_nav = nav_map.and_then(|m| m.get(&head.to_lowercase()));
                    let head_nav = match head_nav {
                        Some(n) => n,
                        None => bail!(
                            "Entity '{}' has no navigation property '{}' (used in 'select ... with {}')",
                            entity,
                            head,
                            rel
                        ),
                    };
                    if let Some(child) = tail {
                        let target_key = head_nav.target_entity.to_lowercase();
                        let known = entity_navigations
                            .get(&target_key)
                            .map(|m| m.contains_key(&child.to_lowercase()))
                            .unwrap_or(false);
                        if !known {
                            bail!(
                                "Entity '{}' has no navigation property '{}' (used in 'select ... with {}')",
                                head_nav.target_entity,
                                child,
                                rel
                            );
                        }
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
