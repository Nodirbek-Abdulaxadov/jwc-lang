//! AST-walk helpers used by [`super::validate::validate_program`].
//!
//! Split out of `validate.rs` so each file stays under the per-file budget.
//! Covers the body-walking pass (`validate_stmts` / `validate_stmt` /
//! `validate_expr` / `validate_where_expr` / `check_where_columns`), the
//! field-suggestion utilities (`suggest_column`, `simple_levenshtein`,
//! `strip_entity_prefix`), the table-name + dbcontext lookups
//! (`validate_context_exists`, `validate_table_in_context`,
//! `table_matches_entity`, `to_snake_case`, `lookup_table_fields`), and the
//! per-driver type-spec validators (`resolve_entity_driver`,
//! `resolve_entity_context_name`, `validate_type_spec_for_driver`,
//! `validate_type_spec_postgres`).

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{Expr, ModelDecl, Program, Stmt, TypeSpec, WhereExpr};

pub(super) fn validate_stmts(
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
        Stmt::DbUpdateSet {
            context_var,
            table,
            assignments,
            where_clause,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;
            validate_table_in_context(&ctx_key, table, db_tables)?;
            let fields = lookup_table_fields(&ctx_key, table, entity_fields_by_table);
            // Each `col = expr` LHS must be a real column on the entity.
            // Otherwise the SQL would surface as `column "..." does not
            // exist` at request time instead of compile time. We also
            // require the `set` list to be non-empty (the parser refuses
            // a trailing comma but does not currently enforce ≥1 pair).
            if assignments.is_empty() {
                bail!(
                    "error[E012]: atomic 'update {context_var}.{table} set ...' must list at least one column"
                );
            }
            if let Some(fields) = fields {
                for (col, _) in assignments {
                    let needle = col.to_lowercase();
                    if !fields.iter().any(|f| f == &needle) {
                        bail!(
                            "Unknown column '{col}' on '{context_var}.{table}' in atomic update{}",
                            suggest_column(col, fields),
                        );
                    }
                }
                check_where_columns(where_clause, fields, context_var, table)?;
            }
            // Walk RHS expressions so nested DB calls / typed errors surface.
            for (_, rhs) in assignments {
                validate_expr(
                    rhs,
                    ctx_names,
                    entity_contexts,
                    db_tables,
                    entity_fields_by_table,
                )?;
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
                        "Unknown column '{}' in aggregate over {}.{}{}",
                        col,
                        context_var,
                        table,
                        suggest_column(&col, fields),
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
                            "Unknown column '{}' in ORDER BY of {}.{}{}",
                            col,
                            context_var,
                            table,
                            suggest_column(&col, fields),
                        );
                    }
                }
                for col in projection {
                    if !fields.iter().any(|f| f.eq_ignore_ascii_case(col)) {
                        bail!(
                            "Unknown column '{}' in projection of {}.{}{}",
                            col,
                            context_var,
                            table,
                            suggest_column(col, fields),
                        );
                    }
                }
                for grp in group_by {
                    let col = strip_entity_prefix(grp);
                    if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                        bail!(
                            "error[E004]: Unknown column '{}' in GROUP BY of {}.{}{}",
                            col,
                            context_var,
                            table,
                            suggest_column(&col, fields),
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
                    "Unknown column '{}' in WHERE of {}.{}{}",
                    col,
                    context_var,
                    table,
                    suggest_column(&col, fields),
                );
            }
            Ok(())
        }
        WhereExpr::InList { field, .. } | WhereExpr::Between { field, .. } => {
            let col = strip_entity_prefix(field);
            if !fields.iter().any(|f| f.eq_ignore_ascii_case(&col)) {
                bail!(
                    "Unknown column '{}' in WHERE of {}.{}{}",
                    col,
                    context_var,
                    table,
                    suggest_column(&col, fields),
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

/// Append ` — did you mean 'X'?` to the validator's `Unknown column …`
/// errors when an existing entity field is close enough by Levenshtein
/// distance. Returns an empty string when no candidate is close,
/// keeping the bare error legible. Threshold of `max(2, len/3)`
/// mirrors `runner::closest_match` so the two paths suggest under the
/// same noise floor.
fn suggest_column(target: &str, fields: &[String]) -> String {
    let target_lc = target.to_ascii_lowercase();
    let threshold = std::cmp::max(2, target_lc.chars().count() / 3);
    let mut best: Option<(usize, &String)> = None;
    for f in fields {
        if f.eq_ignore_ascii_case(target) {
            continue;
        }
        let dist = simple_levenshtein(&target_lc, &f.to_ascii_lowercase());
        if dist > threshold {
            continue;
        }
        match best {
            Some((d, _)) if d <= dist => {}
            _ => best = Some((dist, f)),
        }
    }
    match best {
        Some((_, s)) => format!(" — did you mean '{}'?", s),
        None => String::new(),
    }
}

/// Plain Levenshtein. Duplicated from `runner::levenshtein` to avoid a
/// `parser → runner` dependency for one helper.
fn simple_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
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

pub(super) fn table_matches_entity(table: &str, entity: &str) -> bool {
    if table.eq_ignore_ascii_case(entity) {
        return true;
    }
    to_snake_case(table).eq_ignore_ascii_case(&to_snake_case(entity))
}

pub(super) fn to_snake_case(input: &str) -> String {
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

pub(super) fn resolve_entity_driver(
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

pub(super) fn resolve_entity_context_name(
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

pub(super) fn validate_type_spec_for_driver(ty: &TypeSpec, driver: &str) -> Result<()> {
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
