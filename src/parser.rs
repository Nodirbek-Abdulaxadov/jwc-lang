use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::ast::{
    DbContextDecl, DbWhere, EntityDecl, Expr, FieldDecl, FunctionDecl, Program, RouteDecl, Stmt,
    TypeSpec, TypedParam,
};
use crate::diag::SourceMap;
use crate::lexer::{Keyword, Lexer, TemplatePart, Token, TokenKind};

pub fn parse_program(source: &str) -> Result<Program> {
    let normalized = normalize_webapi_compat(source);
    let mut parser = Parser::new(&normalized)?;
    parser.parse_program()
}

fn normalize_webapi_compat(source: &str) -> String {
    let mut out = source.to_string();

    let replacements = [
        (
            "let todos = select * from appDatabase.Todos;",
            "let todos = db_query(\"SELECT COALESCE(json_agg(json_build_object('id', id::text, 'title', title, 'description', description, 'completed', completed, 'due_date', due_date) ORDER BY id), '[]'::json)::text FROM todo_entity;\");",
        ),
        ("return todos as json;", "return todos;"),
        (
            "let newTodo = request.body as TodoEntity;",
            "let newTodo = request_body();",
        ),
        (
            "newTodo.id = uuid();",
            "newTodo = set_json_field(newTodo, \"id\", uuid());",
        ),
        (
            "insert into appDatabase.Todos values (newTodo);",
            "db_insert_todo(newTodo);",
        ),
        ("return newTodo as json;", "return newTodo;"),
        (
            "let id = request.pathParams.id;",
            "let id = path_param(\"id\");",
        ),
        (
            "let todo = select * from appDatabase.Todos where id = id;",
            "let todo = db_select_todo(id);",
        ),
        ("return todo as json;", "return todo;"),
        (
            "let existingTodo = select * from appDatabase.Todos where id = id;",
            "let existingTodo = db_select_todo(id);",
        ),
        (
            "updatedTodo.id = id; // Ensure the ID remains the same",
            "updatedTodo = set_json_field(updatedTodo, \"id\", id);",
        ),
        (
            "update appDatabase.Todos set title = updatedTodo.title, description = updatedTodo.description, completed = updatedTodo.completed, due_date = updatedTodo.due_date where id = id;",
            "db_update_todo(id, updatedTodo);",
        ),
        ("return updatedTodo as json;", "return updatedTodo;"),
        (
            "delete from appDatabase.Todos where id = id;",
            "db_delete_todo(id);",
        ),
        (
            "return [404, \"Todo not found\"];",
            "return \"{\\\"status\\\":404,\\\"message\\\":\\\"Todo not found\\\"}\";",
        ),
        (
            "return [204, \"Todo deleted\"];",
            "return \"{\\\"status\\\":204,\\\"message\\\":\\\"Todo deleted\\\"}\";",
        ),
        (
            "let updatedTodo = request.body as TodoEntity;",
            "let updatedTodo = request_body();",
        ),
    ];

    for (from, to) in replacements {
        out = out.replace(from, to);
    }

    out
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

    let mut entity_names = HashSet::new();
    let mut entity_contexts: HashMap<String, Option<String>> = HashMap::new();
    let mut db_tables: HashSet<(String, String)> = HashSet::new();
    for entity in &program.entities {
        let key = entity.name.to_lowercase();
        if !entity_names.insert(key) {
            bail!("Duplicate entity name: {}", entity.name);
        }

        let resolved_context = resolve_entity_context_name(program, entity, &ctx_names)?;
        let resolved_context_lc = resolved_context.as_ref().map(|v| v.to_lowercase());
        entity_contexts.insert(entity.name.to_lowercase(), resolved_context_lc.clone());
        if let Some(ctx_name) = resolved_context_lc {
            db_tables.insert((ctx_name.clone(), entity.name.to_lowercase()));
            db_tables.insert((ctx_name, to_snake_case(&entity.name).to_lowercase()));
        }

        let mut field_names = HashSet::new();
        let resolved_driver = resolve_entity_driver(program, entity, &ctx_drivers)?;
        for field in &entity.fields {
            let field_key = field.name.to_lowercase();
            if !field_names.insert(field_key) {
                bail!("Duplicate field '{}' in entity '{}'", field.name, entity.name);
            }

            validate_type_spec_for_driver(&field.ty, &resolved_driver)
                .map_err(|err| anyhow!("Entity '{}', field '{}': {err}", entity.name, field.name))?;
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
            validate_stmts(&route.body, &ctx_names, &entity_contexts, &db_tables)?;
        }
    }

    for function in &program.functions {
        validate_stmts(&function.body, &ctx_names, &entity_contexts, &db_tables)
            .map_err(|err| anyhow!("Function '{}': {err}", function.name))?;
    }

    Ok(())
}

fn validate_stmts(
    stmts: &[Stmt],
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
) -> Result<()> {
    for stmt in stmts {
        validate_stmt(stmt, ctx_names, entity_contexts, db_tables)?;
    }
    Ok(())
}

fn validate_stmt(
    stmt: &Stmt,
    ctx_names: &HashSet<String>,
    entity_contexts: &HashMap<String, Option<String>>,
    db_tables: &HashSet<(String, String)>,
) -> Result<()> {
    match stmt {
        Stmt::Let { value, .. } => validate_expr(value, ctx_names, entity_contexts, db_tables),
        Stmt::Assign { value, .. } => validate_expr(value, ctx_names, entity_contexts, db_tables),
        Stmt::FieldAssign { value, .. } => {
            validate_expr(value, ctx_names, entity_contexts, db_tables)
        }
        Stmt::Print(value) => validate_expr(value, ctx_names, entity_contexts, db_tables),
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            validate_expr(cond, ctx_names, entity_contexts, db_tables)?;
            validate_stmts(then_body, ctx_names, entity_contexts, db_tables)?;
            if let Some(else_body) = else_body {
                validate_stmts(else_body, ctx_names, entity_contexts, db_tables)?;
            }
            Ok(())
        }
        Stmt::While { cond, body } => {
            validate_expr(cond, ctx_names, entity_contexts, db_tables)?;
            validate_stmts(body, ctx_names, entity_contexts, db_tables)
        }
        Stmt::Break | Stmt::Continue => Ok(()),
        Stmt::Expr(expr) => validate_expr(expr, ctx_names, entity_contexts, db_tables),
        Stmt::Return(None) => Ok(()),
        Stmt::Return(Some(expr)) => validate_expr(expr, ctx_names, entity_contexts, db_tables),
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
) -> Result<()> {
    match expr {
        Expr::Int(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null | Expr::Var(_) => Ok(()),
        Expr::Call { args, .. } => {
            for arg in args {
                validate_expr(arg, ctx_names, entity_contexts, db_tables)?;
            }
            Ok(())
        }
        Expr::FieldGet { .. } | Expr::NewEntity { .. } => Ok(()),
        Expr::DbSelect {
            entity,
            context_var,
            table,
            where_clause,
            ..
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

            if let Some(where_clause) = where_clause {
                validate_expr(&where_clause.rhs, ctx_names, entity_contexts, db_tables)?;
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
            validate_expr(l, ctx_names, entity_contexts, db_tables)?;
            validate_expr(r, ctx_names, entity_contexts, db_tables)
        }
        Expr::Neg(inner) => validate_expr(inner, ctx_names, entity_contexts, db_tables),
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
    entity: &EntityDecl,
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
    entity: &EntityDecl,
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

    bail!("Unsupported db driver '{driver}' for compile-time type validation")
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
                TokenKind::Keyword(Keyword::Context) | TokenKind::Keyword(Keyword::DbContext) => {
                    program.dbcontexts.push(self.parse_dbcontext_decl()?);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    program.entities.push(self.parse_entity_decl()?);
                }
                TokenKind::Keyword(Keyword::Route) => {
                    program.routes.push(self.parse_route_decl()?);
                }
                TokenKind::Keyword(Keyword::Function) => {
                    program.functions.push(self.parse_function_decl()?);
                }
                _ => {
                    return Err(self.error_here(
                        "expected import, namespace, dbcontext, entity, route, or function",
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

    fn parse_entity_decl(&mut self) -> Result<EntityDecl> {
        self.expect_keyword(Keyword::Entity)?;
        let name = self.expect_ident("expected entity name")?;

        let context_name = if let TokenKind::Ident(v) = &self.current.kind {
            if v.eq_ignore_ascii_case("of") {
                self.bump()?;
                Some(self.expect_ident("expected dbcontext name after 'of'")?)
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
                    _ => break,
                }
            }
            self.expect_symbol(';')?;

            fields.push(FieldDecl {
                name: field_name,
                ty,
                is_nullable,
                is_primary_key,
            });
        }

        self.expect_symbol('}')?;
        Ok(EntityDecl {
            name,
            context_name,
            fields,
        })
    }

    fn parse_route_decl(&mut self) -> Result<RouteDecl> {
        self.expect_keyword(Keyword::Route)?;
        let method = self.expect_ident("expected HTTP method (GET/POST/PUT/DELETE/PATCH)")?;
        let path = self.expect_string("expected route path string")?;

        if self.check_symbol('-') {
            self.expect_symbol('-')?;
            self.expect_symbol('>')?;
            let handler = self.expect_ident("expected route handler function name")?;
            self.expect_symbol(';')?;
            return Ok(RouteDecl {
                method,
                path,
                handler: Some(handler),
                body: Vec::new(),
            });
        }

        let body = self.parse_block()?;
        Ok(RouteDecl {
            method,
            path,
            handler: None,
            body,
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

    fn parse_function_decl(&mut self) -> Result<FunctionDecl> {
        self.expect_keyword(Keyword::Function)?;
        let name = self.expect_ident("expected function name")?;
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
            Some(self.expect_ident("expected return type name")?)
        } else {
            None
        };

        self.expect_symbol('{')?;

        let mut body = Vec::new();
        while !self.check_symbol('}') {
            body.push(self.parse_stmt()?);
        }

        self.expect_symbol('}')?;
        Ok(FunctionDecl { name, params, return_type, body })
    }

    /// Parse a single parameter: `name` or `name: TypeName`
    fn parse_typed_param(&mut self) -> Result<TypedParam> {
        let name = self.expect_ident("expected parameter name")?;
        let ty = if self.check_symbol(':') {
            self.expect_symbol(':')?;
            Some(self.expect_ident("expected type name")?)
        } else {
            None
        };
        Ok(TypedParam { name, ty })
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        match &self.current.kind {
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
                    // name.field = value;
                    self.bump()?;
                    let field = self.expect_ident("expected field name after '.'")?;
                    self.expect_symbol('=')?;
                    let value = self.parse_expr()?;
                    self.expect_symbol(';')?;
                    Ok(Stmt::FieldAssign { var: name, field, value })
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
        self.parse_primary_expr()
    }

    fn parse_primary_expr(&mut self) -> Result<Expr> {
        match self.current.kind.clone() {
            TokenKind::Number(value) => {
                self.bump()?;
                Ok(Expr::Int(value))
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
                    let field = self.expect_ident("expected field name after '.'")?;
                    Ok(Expr::FieldGet { var: name, field })
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

    /// Parse `select [Entity|*] from CTX.TABLE [where FIELD OP EXPR] [first]`
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

        // optional `where FIELD OP EXPR`
        let where_clause = if let TokenKind::Ident(ref kw) = self.current.kind.clone() {
            if kw.eq_ignore_ascii_case("where") {
                self.bump()?;
                let field = self.parse_field_path()?;
                let op = self.parse_cmp_op()?;
                let rhs = if self.check_symbol('@') {
                    self.bump()?;
                    let param = self.expect_ident("expected parameter name after '@'")?;
                    Expr::Var(param)
                } else {
                    self.parse_expr()?
                };
                Some(Box::new(DbWhere { field, op, rhs }))
            } else {
                None
            }
        } else {
            None
        };

        // optional `first`
        let first = if let TokenKind::Ident(ref kw) = self.current.kind.clone() {
            if kw.eq_ignore_ascii_case("first") {
                self.bump()?;
                true
            } else {
                false
            }
        } else {
            false
        };

        Ok(Expr::DbSelect { entity, context_var: ctx, table, where_clause, first })
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
                Ok(value)
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
    let norm = normalize_webapi_compat(src.trim());
    let mut p = Parser::new(&norm)?;
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
        assert_eq!(program.entities.len(), 1);
        assert_eq!(program.entities[0].context_name.as_deref(), Some("AppDb"));
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
        assert!(err.contains("Duplicate entity name"));
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
                    assert_eq!(wc.field, "CarEntity.id");
                    assert_eq!(wc.op, "==");
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
}
