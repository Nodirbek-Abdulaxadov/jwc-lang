use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{
    DbContextDecl, DbOrderBy, DbWhere, Expr, FieldDecl, FieldReference, FunctionDecl,
    MiddlewareDecl, ModelDecl, ModelKind, OnDeleteAction, Program, RouteDecl, SortDir, Stmt,
    TypeSpec, TypedParam, ValidateField, ValidateRule, WhereExpr,
};
use crate::diag::SourceMap;
use crate::lexer::{Keyword, Lexer, TemplatePart, Token, TokenKind};

pub fn parse_program(source: &str) -> Result<Program> {
    let mut parser = Parser::new(source)?;
    parser.parse_program()
}

pub fn validate_program(program: &Program) -> Result<()> {
    let mut ctx_names = HashSet::new();
    let mut ctx_drivers: HashMap<String, String> = HashMap::new();
    for ctx in &program.dbcontexts {
        let key = ctx.name.to_lowercase();
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
    for model in &program.models {
        let model_key = model.name.to_lowercase();
        if !model_names.insert(model_key) {
            bail!("Duplicate model name: {}", model.name);
        }

        if model.kind != ModelKind::Entity {
            continue;
        }
        let key = model.name.to_lowercase();
        if !entity_names.insert(key) {
            bail!("Duplicate entity name: {}", model.name);
        }

        let resolved_context = resolve_entity_context_name(program, model, &ctx_names)?;
        let resolved_context_lc = resolved_context.as_ref().map(|v| v.to_lowercase());
        entity_contexts.insert(model.name.to_lowercase(), resolved_context_lc.clone());
        if let Some(ctx_name) = resolved_context_lc {
            db_tables.insert((ctx_name.clone(), model.name.to_lowercase()));
            db_tables.insert((ctx_name.clone(), to_snake_case(&model.name).to_lowercase()));

            let fields: Vec<String> = model
                .fields
                .iter()
                .map(|f| f.name.to_lowercase())
                .collect();
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
                bail!("Duplicate field '{}' in entity '{}'", field.name, model.name);
            }

            validate_type_spec_for_driver(&field.ty, &resolved_driver)
                .map_err(|err| anyhow!("Entity '{}', field '{}': {err}", model.name, field.name))?;
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
        let key = function.name.to_lowercase();
        if !fn_names.insert(key) {
            bail!("Duplicate function name: {}", function.name);
        }

        let mut param_names = HashSet::new();
        for param in &function.params {
            let param_key = param.name.to_lowercase();
            if !param_names.insert(param_key) {
                bail!("Function '{}': duplicate parameter '{}'", function.name, param.name);
            }
        }
    }

    let mut route_keys = HashSet::new();
    for route in &program.routes {
        let method = route.method.to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE" | "PATCH") {
            bail!("Unsupported route method: {}", route.method);
        }

        let key = format!("{} {}", method, route.path);
        if !route_keys.insert(key) {
            bail!("Duplicate route: {} {}", method, route.path);
        }

        if route.handler.is_some() && !route.body.is_empty() {
            bail!("Route cannot define both handler and inline body");
        }
        if route.handler.is_none() && route.body.is_empty() {
            bail!("Route must define either handler or inline body");
        }

        if let Some(handler) = &route.handler {
            let handler_key = handler.to_lowercase();
            if !fn_names.contains(&handler_key) {
                bail!("Route handler '{}' is not defined as a function", handler);
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
        let key = mw.name.to_lowercase();
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

    for route in &program.routes {
        for mw_ref in &route.middlewares {
            if !mw_names.contains(&mw_ref.to_lowercase()) {
                bail!(
                    "Route {} {} references unknown middleware '{}'",
                    route.method,
                    route.path,
                    mw_ref
                );
            }
        }
    }

    Ok(())
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
                bail!("validate body block has no fields");
            }
            Ok(())
        }
        Stmt::Try {
            body,
            catch_body,
            ..
        } => {
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
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Var(_) => Ok(()),
        Expr::Call { args, .. } => {
            for arg in args {
                validate_expr(arg, ctx_names, entity_contexts, db_tables, entity_fields_by_table)?;
            }
            Ok(())
        }
        Expr::FieldGet { .. } | Expr::NewEntity { .. } => Ok(()),
        Expr::DbSelect {
            entity,
            context_var,
            table,
            where_clause,
            order_by,
            limit,
            offset,
            first: _,
        } => {
            let ctx_key = validate_context_exists(context_var, ctx_names)?;

            if entity != "*" {
                let entity_key = entity.to_lowercase();
                let expected_ctx = entity_contexts.get(&entity_key).ok_or_else(|| {
                    anyhow!("Unknown entity '{}' used in select expression", entity)
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

            // Compile-time column existence check for WHERE / ORDER BY.
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
        Expr::Await(inner) => validate_expr(
            inner,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
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
            validate_expr(l, ctx_names, entity_contexts, db_tables, entity_fields_by_table)?;
            validate_expr(r, ctx_names, entity_contexts, db_tables, entity_fields_by_table)
        }
        Expr::Neg(inner) => validate_expr(
            inner,
            ctx_names,
            entity_contexts,
            db_tables,
            entity_fields_by_table,
        ),
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
        WhereExpr::And(l, r) | WhereExpr::Or(l, r) => {
            validate_where_expr(l, ctx_names, entity_contexts, db_tables, entity_fields_by_table)?;
            validate_where_expr(r, ctx_names, entity_contexts, db_tables, entity_fields_by_table)
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
                "Entity '{}' references unknown dbcontext '{}'",
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
                "Entity '{}' references unknown dbcontext '{}'",
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
        "bigint" | "bool" | "uuid" | "datetime" | "json" => {
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
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self> {
        let mut lexer = Lexer::new(source);
        let current = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            source_map: SourceMap::new(source),
        })
    }

    fn parse_program(&mut self) -> Result<Program> {
        let mut program = Program::default();

        while !matches!(self.current.kind, TokenKind::Eof) {
            match &self.current.kind {
                TokenKind::Keyword(Keyword::Import) => {
                    self.parse_import_stmt()?;
                }
                TokenKind::Keyword(Keyword::Namespace) => {
                    self.parse_namespace_stmt()?;
                }
                TokenKind::Keyword(Keyword::DbContext) => {
                    program.dbcontexts.push(self.parse_dbcontext_decl()?);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    program.models.push(self.parse_model_decl(ModelKind::Entity)?);
                }
                TokenKind::Keyword(Keyword::Class) => {
                    program.models.push(self.parse_model_decl(ModelKind::Class)?);
                }
                TokenKind::Keyword(Keyword::Route) => {
                    program.routes.push(self.parse_route_decl()?);
                }
                TokenKind::Keyword(Keyword::Function) => {
                    program.functions.push(self.parse_function_decl(None)?);
                }
                TokenKind::Keyword(Keyword::Async) => {
                    self.bump()?;
                    if !matches!(self.current.kind, TokenKind::Keyword(Keyword::Function)) {
                        return Err(self.error_here("expected 'function' after 'async'"));
                    }
                    let mut fn_decl = self.parse_function_decl(None)?;
                    fn_decl.is_async = true;
                    program.functions.push(fn_decl);
                }
                TokenKind::Keyword(Keyword::Dome) => {
                    self.parse_dome_decl(&mut program)?;
                }
                TokenKind::Keyword(Keyword::Middleware) => {
                    program.middlewares.push(self.parse_middleware_decl()?);
                }
                _ => {
                    return Err(self.error_here(
                        "expected import, namespace, dbcontext, entity/class, route, function, middleware, or dome",
                    ));
                }
            }
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

        Ok(DbContextDecl { name, driver })
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

    fn parse_import_stmt(&mut self) -> Result<()> {
        self.expect_keyword(Keyword::Import)?;
        self.parse_qualified_name()?;
        self.expect_symbol(';')?;
        Ok(())
    }

    fn parse_namespace_stmt(&mut self) -> Result<()> {
        self.expect_keyword(Keyword::Namespace)?;
        self.parse_qualified_name()?;
        self.expect_symbol(';')?;
        Ok(())
    }

    fn parse_qualified_name(&mut self) -> Result<String> {
        let mut parts = vec![self.expect_ident("expected identifier")?];
        while self.check_symbol('.') {
            self.expect_symbol('.')?;
            parts.push(self.expect_ident("expected identifier after '.'")?);
        }
        Ok(parts.join("."))
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
        while !self.check_symbol('}') {
            let field_name = self.expect_ident("expected field name")?;
            let ty = self.parse_type_spec()?;
            let mut is_nullable = false;
            let mut is_primary_key = false;
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
                references,
            });
        }

        self.expect_symbol('}')?;
        Ok(ModelDecl {
            kind,
            name,
            context_name,
            fields,
        })
    }

    fn parse_dome_decl(&mut self, program: &mut Program) -> Result<()> {
        self.expect_keyword(Keyword::Dome)?;
        let dome_name = self.expect_ident("expected dome name")?;
        self.expect_symbol('{')?;

        while !self.check_symbol('}') {
            match &self.current.kind {
                TokenKind::Keyword(Keyword::Function) => {
                    program.functions.push(self.parse_function_decl(Some(&dome_name))?);
                }
                _ => {
                    return Err(self.error_here("expected function declaration inside dome block"))
                }
            }
        }

        self.expect_symbol('}')?;
        Ok(())
    }

    fn parse_route_decl(&mut self) -> Result<RouteDecl> {
        self.expect_keyword(Keyword::Route)?;
        let method = self.expect_ident("expected HTTP method (GET/POST/PUT/DELETE/PATCH)")?;
        let path = self.expect_string("expected route path string")?;

        // Optional `use M1[, M2, ...]` middleware list
        let middlewares = if self.current.kind == TokenKind::Keyword(Keyword::Use) {
            self.bump()?;
            let mut names = vec![self.expect_ident("expected middleware name after 'use'")?];
            while self.check_symbol(',') {
                self.expect_symbol(',')?;
                names.push(self.expect_ident("expected middleware name after ','")?);
            }
            names
        } else {
            Vec::new()
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
            });
        }

        let body = self.parse_block()?;
        Ok(RouteDecl {
            method,
            path,
            handler: None,
            body,
            middlewares,
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
        Ok(MiddlewareDecl { name, body })
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
                Ok(Stmt::DbInsert { var, context_var: ctx, table })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("update") => {
                self.bump()?;
                let var = self.expect_ident("expected variable name after 'update'")?;
                let kw = self.expect_ident("expected 'in'")?;
                if !kw.eq_ignore_ascii_case("in") {
                    return Err(self.error_here("expected 'in' in update statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbUpdate { var, context_var: ctx, table })
            }
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("delete") => {
                self.bump()?;
                let var = self.expect_ident("expected variable name after 'delete'")?;
                let kw = self.expect_ident("expected 'from'")?;
                if !kw.eq_ignore_ascii_case("from") {
                    return Err(self.error_here("expected 'from' in delete statement"));
                }
                let (ctx, table) = self.parse_db_ref()?;
                self.expect_symbol(';')?;
                Ok(Stmt::DbDelete { var, context_var: ctx, table })
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
            Some(self.parse_block()?)
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
            _ => {
                return Err(
                    self.error_here(&format!("expected numeric argument for {rule_name}"))
                )
            }
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
            _ => Err(self.error_here("expected expression")),
        }
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

    /// Parse `select [Entity|*] from CTX.TABLE
    ///        [where FIELD OP EXPR]
    ///        [orderby FIELD [asc|desc]]
    ///        [limit N] [offset N] [first]`
    fn parse_select_expr(&mut self) -> Result<Expr> {
        // entity name or `*`
        let entity = if self.check_symbol('*') {
            self.bump()?;
            "*".to_string()
        } else {
            self.expect_ident("expected entity name or '*' after 'select'")?
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
        let op = self.parse_cmp_op()?;
        let rhs = if self.check_symbol('@') {
            self.bump()?;
            let param = self.expect_ident("expected parameter name after '@'")?;
            Expr::Var(param)
        } else {
            self.parse_expr()?
        };
        Ok(WhereExpr::Atom(DbWhere { field, op, rhs }))
    }

    /// Accepts `@param`, integer literal, or any expression — runtime ensures
    /// the value is an integer when binding to LIMIT/OFFSET.
    fn parse_db_int_arg(&mut self, clause: &str) -> Result<Expr> {
        if self.check_symbol('@') {
            self.bump()?;
            let name = self.expect_ident(&format!("expected parameter name after '@' in {clause}"))?;
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
                    crate::ast::Expr::DbSelect { entity, table, first, .. } => {
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
                crate::ast::Expr::DbSelect { where_clause, first, .. } => {
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

    #[test]
    fn runner_type_mismatch_returns_error() {
        let src = r#"
            function takesInt(x: int) { print(x); }
            function main() { takesInt(true); }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let result = crate::runner::run_main(&program);
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
        assert!(err.contains("expected import, namespace, dbcontext, entity/class"));
    }
}
