use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};

use crate::diag::SourceMap;
use crate::ast::{
    CmpExpr, CmpOp, DbContextDecl, DbSetDecl, EntityDecl, Expr, FieldDecl, Literal, Program,
    SelectStmt, SelectTarget, TypeSpec,
};
use crate::ast::FieldMods;
use crate::ast::{ClassDecl, ClassMember, FieldDeclRuntime, FunctionDecl, RouteDecl, Stmt, ValueExpr};
use crate::lexer::{Keyword, Lexer, TokenKind};

pub fn parse_program(source: &str) -> Result<Program> {
    let mut parser = Parser::new(source);
    parser.parse_program()
}

/// Parse only schema declarations (`context`/`dbcontext`/`entity`) and ignore all other
/// top-level items.
///
/// This is intended for schema/migration commands so a single `.jwc` file can also contain
/// routes/controllers/etc that are irrelevant for schema diffing.
pub fn parse_program_schema_only(source: &str) -> Result<Program> {
    let mut parser = Parser::new(source);
    parser.parse_program_schema_only()
}

pub fn validate_program(program: &Program) -> Result<()> {
    let mut fn_names = HashSet::new();
    for f in &program.functions {
        let key = f.name.to_lowercase();
        if !fn_names.insert(key) {
            bail!("Duplicate function name: {}", f.name);
        }

        let mut param_names = HashSet::new();
        for p in &f.params {
            let pk = p.to_lowercase();
            if !param_names.insert(pk) {
                bail!("Function '{}': duplicate parameter name: {}", f.name, p);
            }
        }
    }

    let mut entity_names = HashSet::new();
    for entity in &program.entities {
        let entity_key = entity.name.to_lowercase();
        if !entity_names.insert(entity_key) {
            bail!("Duplicate entity name: {}", entity.name);
        }

        let mut field_names = HashSet::new();
        let mut seen_pk: Option<String> = None;
        for field in &entity.fields {
            let field_key = field.name.to_lowercase();
            if !field_names.insert(field_key) {
                bail!("Duplicate field '{}' in entity '{}'", field.name, entity.name);
            }

            validate_type_spec(&field.ty)
                .map_err(|e| anyhow!("Entity '{}', field '{}': {e}", entity.name, field.name))?;

            if field.mods.primary_key {
                if field.mods.nullable {
                    bail!(
                        "Entity '{}', field '{}': primary key cannot be nullable",
                        entity.name,
                        field.name
                    );
                }
                if let Some(prev) = &seen_pk {
                    bail!(
                        "Entity '{}': multiple primary keys specified ('{}' and '{}')",
                        entity.name,
                        prev,
                        field.name
                    );
                }
                seen_pk = Some(field.name.clone());
            }
        }
    }

    for ctx in &program.dbcontexts {
        if ctx.name.trim().is_empty() {
            bail!("dbcontext name cannot be empty");
        }
        if ctx.driver.trim().is_empty() {
            bail!("dbcontext driver cannot be empty");
        }
        if let Some(url) = &ctx.url {
            if url.trim().is_empty() {
                bail!("dbcontext url cannot be empty");
            }
        }
    }

    // Validate dbcontext sets
    let entity_names_lower: HashSet<String> = program
        .entities
        .iter()
        .map(|e| e.name.to_lowercase())
        .collect();
    for ctx in &program.dbcontexts {
        let mut set_names = HashSet::new();
        for s in &ctx.sets {
            if s.entity.trim().is_empty() || s.name.trim().is_empty() {
                bail!("dbcontext '{}': set declarations cannot be empty", ctx.name);
            }
            let sk = s.name.to_lowercase();
            if !set_names.insert(sk) {
                bail!(
                    "dbcontext '{}': duplicate set name: {}",
                    ctx.name,
                    s.name
                );
            }
            let ek = s.entity.to_lowercase();
            if !entity_names_lower.contains(&ek) {
                bail!(
                    "dbcontext '{}': set '{}' refers to unknown entity '{}'",
                    ctx.name,
                    s.name,
                    s.entity
                );
            }
        }
    }

    validate_selects(program)?;

    let mut route_keys = HashSet::new();
    for r in &program.routes {
        let method = r.method.trim().to_lowercase();
        let path = r.path.trim().to_string();
        if method.is_empty() {
            bail!("route method cannot be empty");
        }
        if path.is_empty() {
            bail!("route path cannot be empty");
        }
        if !path.starts_with('/') {
            bail!("route path must start with '/': {}", r.path);
        }
        let key = format!("{} {}", method, path);
        if !route_keys.insert(key.clone()) {
            bail!("Duplicate route: {key}");
        }
    }

    let mut type_names = HashSet::new();
    for c in &program.classes {
        let ck = c.name.to_lowercase();
        if !type_names.insert(ck) {
            bail!("Duplicate type name (class/controller): {}", c.name);
        }
        validate_class_like("Class", &c.name, &c.members, &mut route_keys)?;
    }
    for c in &program.controllers {
        let ck = c.name.to_lowercase();
        if !type_names.insert(ck) {
            bail!("Duplicate type name (class/controller): {}", c.name);
        }
        validate_class_like("Controller", &c.name, &c.members, &mut route_keys)?;
    }

    Ok(())
}

fn validate_class_like(
    kind: &str,
    type_name: &str,
    members: &[ClassMember],
    global_route_keys: &mut HashSet<String>,
) -> Result<()> {
    let mut member_names = HashSet::new();
    let mut local_route_keys = HashSet::new();
    for m in members {
        match m {
            ClassMember::Field(f) => {
                let mk = f.name.to_lowercase();
                if !member_names.insert(mk) {
                    bail!("{} '{}': duplicate member name: {}", kind, type_name, f.name);
                }
            }
            ClassMember::Method(f) => {
                let mk = f.name.to_lowercase();
                if !member_names.insert(mk) {
                    bail!("{} '{}': duplicate member name: {}", kind, type_name, f.name);
                }

                let mut param_names = HashSet::new();
                for p in &f.params {
                    let pk = p.to_lowercase();
                    if !param_names.insert(pk) {
                        bail!("Method '{}.{}': duplicate parameter name: {}", type_name, f.name, p);
                    }
                }
            }
            ClassMember::Route(r) => {
                let method = r.method.trim().to_lowercase();
                let path = r.path.trim().to_string();
                if method.is_empty() {
                    bail!("{} '{}': route method cannot be empty", kind, type_name);
                }
                if path.is_empty() {
                    bail!("{} '{}': route path cannot be empty", kind, type_name);
                }
                if !path.starts_with('/') {
                    bail!(
                        "{} '{}': route path must start with '/': {}",
                        kind,
                        type_name,
                        r.path
                    );
                }
                let key = format!("{} {}", method, path);
                if !local_route_keys.insert(key.clone()) {
                    bail!("{} '{}': duplicate route: {key}", kind, type_name);
                }
                if !global_route_keys.insert(key.clone()) {
                    bail!("Duplicate route: {key}");
                }
            }
        }
    }
    Ok(())
}

fn validate_selects(program: &Program) -> Result<()> {
    if program.selects.is_empty() {
        return Ok(());
    }

    let mut entity_by_name: HashMap<String, &EntityDecl> = HashMap::new();
    let mut entity_by_table: HashMap<String, &EntityDecl> = HashMap::new();
    for entity in &program.entities {
        entity_by_name.insert(entity.name.to_lowercase(), entity);
        entity_by_table.insert(crate::sql::to_snake_case(&entity.name), entity);
    }

    for select in &program.selects {
        let from_key = select.from.to_lowercase();
        let entity = entity_by_name
            .get(&from_key)
            .copied()
            .or_else(|| entity_by_table.get(&from_key).copied())
            .ok_or_else(|| anyhow!("select from unknown entity/table: {}", select.from))?;

        if let SelectTarget::Entity(target_name) = &select.target {
            if target_name.to_lowercase() != entity.name.to_lowercase() {
                bail!(
                    "select target '{}' does not match from entity '{}'",
                    target_name,
                    entity.name
                );
            }
        }

        if let SelectTarget::Fields(fields) = &select.target {
            for f in fields {
                let exists = entity
                    .fields
                    .iter()
                    .any(|ef| ef.name.to_lowercase() == f.to_lowercase());
                if !exists {
                    bail!(
                        "select projection references unknown field '{}' on entity '{}'",
                        f,
                        entity.name
                    );
                }
            }
        }

        if let Some(expr) = &select.where_expr {
            validate_where_expr(entity, expr)?;
        }
    }

    Ok(())
}

fn validate_where_expr(entity: &EntityDecl, expr: &Expr) -> Result<()> {
    match expr {
        Expr::Cmp(c) => {
            let field = entity
                .fields
                .iter()
                .find(|f| f.name.to_lowercase() == c.field.to_lowercase())
                .ok_or_else(|| {
                    anyhow!(
                        "select where references unknown field '{}' on entity '{}'",
                        c.field,
                        entity.name
                    )
                })?;

            validate_cmp_op_compat(&field.ty, &c.op).map_err(|e| {
                anyhow!(
                    "select where operator mismatch for field '{}' on entity '{}': {e}",
                    field.name,
                    entity.name
                )
            })?;

            validate_where_literal_compat(&field.ty, &c.value).map_err(|e| {
                anyhow!(
                    "select where type mismatch for field '{}' on entity '{}': {e}",
                    field.name,
                    entity.name
                )
            })?;
            Ok(())
        }
        Expr::And(l, r) | Expr::Or(l, r) => {
            validate_where_expr(entity, l)?;
            validate_where_expr(entity, r)?;
            Ok(())
        }
    }
}

fn validate_cmp_op_compat(field_ty: &TypeSpec, op: &CmpOp) -> Result<()> {
    let allow_ordering = matches!(field_ty.name.as_str(), "int" | "bigint" | "decimal");

    match op {
        CmpOp::Eq => Ok(()),
        CmpOp::Gt | CmpOp::Lt | CmpOp::Gte | CmpOp::Lte => {
            if allow_ordering {
                Ok(())
            } else {
                bail!("comparison operator requires a numeric field")
            }
        }
    }
}

fn validate_where_literal_compat(field_ty: &TypeSpec, lit: &Literal) -> Result<()> {
    match field_ty.name.as_str() {
        "int" | "bigint" => match lit {
            Literal::Int(_) => Ok(()),
            _ => bail!("expected integer literal"),
        },
        "decimal" => match lit {
            Literal::Int(_) | Literal::Decimal(_) => Ok(()),
            _ => bail!("expected numeric literal"),
        },
        "bool" => match lit {
            Literal::Bool(_) => Ok(()),
            _ => bail!("expected boolean literal"),
        },
        "text" | "varchar" | "string" | "uuid" | "datetime" | "json" => match lit {
            Literal::Str(_) => Ok(()),
            _ => bail!("expected string literal"),
        },
        other => bail!("unknown field type '{other}'"),
    }
}

fn validate_type_spec(ty: &TypeSpec) -> Result<()> {
    let name = ty.name.as_str();
    match name {
        "int" => {
            if !(ty.args.is_empty() || ty.args.len() == 2) {
                bail!("int expects 0 or 2 args (min,max)");
            }
        }
        "bigint" | "uuid" | "bool" | "datetime" | "timestamp" | "json" => {
            if !ty.args.is_empty() {
                bail!("{name} does not take args");
            }
        }
        "text" | "varchar" | "string" => {
            if !(ty.args.is_empty() || ty.args.len() == 1) {
                bail!("{name} expects 0 or 1 arg (len)");
            }
        }
        "decimal" => {
            if ty.args.len() != 2 {
                bail!("decimal expects 2 args (precision,scale)");
            }
        }
        _ => {
            bail!("Unknown type '{name}'");
        }
    }

    if ty.args.iter().any(|v| *v < 0) {
        bail!("type args must be non-negative");
    }

    Ok(())
}

struct Parser<'a> {
    sm: SourceMap,
    lexer: Lexer<'a>,
    lookahead: crate::lexer::Token,
    lookahead2: crate::lexer::Token,
    lookahead3: crate::lexer::Token,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        let sm = SourceMap::new(source);
        let mut lexer = Lexer::new(source);
        let lookahead = lexer
            .next_token()
            .expect("lexer should not fail on initial token");
        let lookahead2 = lexer
            .next_token()
            .expect("lexer should not fail on second token");
        let lookahead3 = lexer
            .next_token()
            .expect("lexer should not fail on third token");
        Self {
            sm,
            lexer,
            lookahead,
            lookahead2,
            lookahead3,
        }
    }

    fn pos(&self) -> (usize, usize) {
        self.sm.line_col(self.lookahead.offset)
    }

    fn bump(&mut self) -> Result<()> {
        self.lookahead = std::mem::replace(&mut self.lookahead2, self.lookahead3.clone());
        self.lookahead3 = self.lexer.next_token()?;
        Ok(())
    }

    fn parse_program(&mut self) -> Result<Program> {
        let mut program = Program::new();
        loop {
            match &self.lookahead.kind {
                TokenKind::Eof => break,
                TokenKind::Keyword(Keyword::Context) => {
                    let (ctx, entities) = self.parse_dbcontext()?;
                    program.dbcontexts.push(ctx);
                    program.entities.extend(entities);
                }
                TokenKind::Keyword(Keyword::DbContext) => {
                    let (ctx, entities) = self.parse_dbcontext()?;
                    program.dbcontexts.push(ctx);
                    program.entities.extend(entities);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    program.entities.push(self.parse_entity()?);
                }
                TokenKind::Keyword(Keyword::Select) => {
                    program.selects.push(self.parse_select()?);
                }
                TokenKind::Keyword(Keyword::Function) => {
                    program.functions.push(self.parse_function()?);
                }
                TokenKind::Keyword(Keyword::Fn) => {
                    program.functions.push(self.parse_function()?);
                }
                TokenKind::Keyword(Keyword::Route) => {
                    program.routes.push(self.parse_route()?);
                }
                TokenKind::Keyword(Keyword::Controller) => {
                    program.controllers.push(self.parse_controller()?);
                }
                TokenKind::Keyword(Keyword::Class) => {
                    program.classes.push(self.parse_class()?);
                }
                _ => {
                    let (line, col) = self.pos();
                    return Err(anyhow!(
                        "Unexpected token at {}:{}: {:?}",
                        line,
                        col,
                        self.lookahead.kind
                    ));
                }
            }
        }
        Ok(program)
    }

    fn parse_program_schema_only(&mut self) -> Result<Program> {
        let mut program = Program::new();
        loop {
            match &self.lookahead.kind {
                TokenKind::Eof => break,
                TokenKind::Keyword(Keyword::Context) | TokenKind::Keyword(Keyword::DbContext) => {
                    let (ctx, entities) = self.parse_dbcontext()?;
                    program.dbcontexts.push(ctx);
                    program.entities.extend(entities);
                }
                TokenKind::Keyword(Keyword::Entity) => {
                    program.entities.push(self.parse_entity()?);
                }
                _ => {
                    self.skip_top_level_item()?;
                }
            }
        }
        Ok(program)
    }

    fn skip_top_level_item(&mut self) -> Result<()> {
        let mut brace_depth = 0usize;
        let mut paren_depth = 0usize;
        let mut bracket_depth = 0usize;
        let mut seen_outer_brace = false;

        loop {
            match &self.lookahead.kind {
                TokenKind::Eof => break,

                TokenKind::Symbol('{') => {
                    if brace_depth == 0 {
                        seen_outer_brace = true;
                    }
                    brace_depth += 1;
                    self.bump()?;
                }
                TokenKind::Symbol('}') => {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    }
                    self.bump()?;
                    if seen_outer_brace
                        && brace_depth == 0
                        && paren_depth == 0
                        && bracket_depth == 0
                    {
                        break;
                    }
                }
                TokenKind::Symbol('(') => {
                    paren_depth += 1;
                    self.bump()?;
                }
                TokenKind::Symbol(')') => {
                    if paren_depth > 0 {
                        paren_depth -= 1;
                    }
                    self.bump()?;
                }
                TokenKind::Symbol('[') => {
                    bracket_depth += 1;
                    self.bump()?;
                }
                TokenKind::Symbol(']') => {
                    if bracket_depth > 0 {
                        bracket_depth -= 1;
                    }
                    self.bump()?;
                }

                TokenKind::Symbol(';')
                    if !seen_outer_brace
                        && brace_depth == 0
                        && paren_depth == 0
                        && bracket_depth == 0 =>
                {
                    self.bump()?;
                    break;
                }

                _ => {
                    self.bump()?;
                }
            }
        }

        Ok(())
    }

    fn parse_controller(&mut self) -> Result<ClassDecl> {
        self.expect_keyword(Keyword::Controller)?;
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;

        let mut members: Vec<ClassMember> = Vec::new();
        while !self.is_symbol('}')? {
            match &self.lookahead.kind {
                TokenKind::Keyword(Keyword::Function) | TokenKind::Keyword(Keyword::Fn) => {
                    let f = self.parse_function()?;
                    members.push(ClassMember::Method(f));
                }
                TokenKind::Keyword(Keyword::Route) => {
                    let r = self.parse_route()?;
                    members.push(ClassMember::Route(r));
                }
                TokenKind::Keyword(Keyword::Variable)
                | TokenKind::Keyword(Keyword::Let)
                | TokenKind::Keyword(Keyword::Var) => {
                    self.bump()?;
                    let field_name = self.expect_ident()?;
                    let init = if self.is_symbol('=')? {
                        self.expect_symbol('=')?;
                        Some(self.parse_value_expr()?)
                    } else {
                        None
                    };
                    self.expect_symbol(';')?;
                    members.push(ClassMember::Field(FieldDeclRuntime {
                        name: field_name,
                        init,
                    }));
                }
                _ => {
                    let (line, col) = self.pos();
                    return Err(anyhow!(
                        "Expected controller member at {}:{} but got {:?}",
                        line,
                        col,
                        self.lookahead.kind
                    ));
                }
            }
        }

        self.expect_symbol('}')?;
        Ok(ClassDecl { name, members })
    }

    fn parse_class(&mut self) -> Result<ClassDecl> {
        self.expect_keyword(Keyword::Class)?;
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;

        let mut members: Vec<ClassMember> = Vec::new();
        while !self.is_symbol('}')? {
            match &self.lookahead.kind {
                TokenKind::Keyword(Keyword::Function) | TokenKind::Keyword(Keyword::Fn) => {
                    let f = self.parse_function()?;
                    members.push(ClassMember::Method(f));
                }
                TokenKind::Keyword(Keyword::Route) => {
                    let r = self.parse_route()?;
                    members.push(ClassMember::Route(r));
                }
                TokenKind::Keyword(Keyword::Variable)
                | TokenKind::Keyword(Keyword::Let)
                | TokenKind::Keyword(Keyword::Var) => {
                    self.bump()?;
                    let field_name = self.expect_ident()?;
                    let init = if self.is_symbol('=')? {
                        self.expect_symbol('=')?;
                        Some(self.parse_value_expr()?)
                    } else {
                        None
                    };
                    self.expect_symbol(';')?;
                    members.push(ClassMember::Field(FieldDeclRuntime {
                        name: field_name,
                        init,
                    }));
                }
                _ => {
                    let (line, col) = self.pos();
                    return Err(anyhow!(
                        "Expected class member at {}:{} but got {:?}",
                        line,
                        col,
                        self.lookahead.kind
                    ));
                }
            }
        }

        self.expect_symbol('}')?;
        Ok(ClassDecl { name, members })
    }

    fn parse_route(&mut self) -> Result<RouteDecl> {
        self.expect_keyword(Keyword::Route)?;

        let method = match &self.lookahead.kind {
            TokenKind::Ident(m) => {
                let v = m.clone();
                self.bump()?;
                v
            }
            TokenKind::Keyword(Keyword::Delete) => {
                self.bump()?;
                "delete".to_string()
            }
            _ => {
                let (line, col) = self.pos();
                return Err(anyhow!(
                    "Expected route HTTP method at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ));
            }
        };
        let path = self.expect_string()?;

        self.expect_symbol('{')?;
        let mut body = Vec::new();
        while !self.is_symbol('}')? {
            body.push(self.parse_stmt()?);
        }
        self.expect_symbol('}')?;

        Ok(RouteDecl { method, path, body })
    }

    fn parse_function(&mut self) -> Result<FunctionDecl> {
        match &self.lookahead.kind {
            TokenKind::Keyword(Keyword::Function) => {
                self.bump()?;
            }
            TokenKind::Keyword(Keyword::Fn) => {
                self.bump()?;
            }
            _ => {
                let (line, col) = self.pos();
                return Err(anyhow!(
                    "Expected function keyword at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ));
            }
        }

        let name = self.expect_ident()?;
        self.expect_symbol('(')?;
        let mut params = Vec::new();
        if !self.is_symbol(')')? {
            params.push(self.expect_ident()?);
            while self.is_symbol(',')? {
                self.expect_symbol(',')?;
                params.push(self.expect_ident()?);
            }
        }
        self.expect_symbol(')')?;
        self.expect_symbol('{')?;

        let mut body = Vec::new();
        while !self.is_symbol('}')? {
            body.push(self.parse_stmt()?);
        }

        self.expect_symbol('}')?;
        Ok(FunctionDecl { name, params, body })
    }

    fn parse_stmt(&mut self) -> Result<Stmt> {
        match &self.lookahead.kind {
            TokenKind::Keyword(Keyword::Print) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let value = self.parse_value_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol(';')?;
                Ok(Stmt::Print(value))
            }
            TokenKind::Keyword(Keyword::Variable)
            | TokenKind::Keyword(Keyword::Let)
            | TokenKind::Keyword(Keyword::Var) => {
                self.bump()?;
                let name = self.expect_ident()?;
                self.expect_symbol('=')?;
                let value = self.parse_value_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::VarDecl { name, value })
            }
            TokenKind::Ident(name) => {
                // assignment: <ident> = <expr>;
                if matches!(self.lookahead2.kind, TokenKind::Symbol('=')) {
                    let var_name = name.clone();
                    self.bump()?; // ident
                    self.expect_symbol('=')?;
                    let value = self.parse_value_expr()?;
                    self.expect_symbol(';')?;
                    return Ok(Stmt::Assign {
                        name: var_name,
                        value,
                    });
                }

                // compound assignment: <ident> <op>= <expr>;
                // supported ops: +=, -=, *=, /=, %=
                let compound_op = match (&self.lookahead2.kind, &self.lookahead3.kind) {
                    (TokenKind::Symbol(op @ ('+' | '-' | '*' | '/' | '%')), TokenKind::Symbol('=')) => {
                        Some(*op)
                    }
                    _ => None,
                };
                if let Some(op) = compound_op {
                    let var_name = name.clone();
                    self.bump()?; // ident
                    self.expect_symbol(op)?;
                    self.expect_symbol('=')?;
                    let rhs = self.parse_value_expr()?;
                    self.expect_symbol(';')?;

                    let left = ValueExpr::Var(var_name.clone());
                    let value = match op {
                        '+' => ValueExpr::Add(Box::new(left), Box::new(rhs)),
                        '-' => ValueExpr::Sub(Box::new(left), Box::new(rhs)),
                        '*' => ValueExpr::Mul(Box::new(left), Box::new(rhs)),
                        '/' => ValueExpr::Div(Box::new(left), Box::new(rhs)),
                        '%' => ValueExpr::Mod(Box::new(left), Box::new(rhs)),
                        _ => unreachable!(),
                    };

                    return Ok(Stmt::Assign {
                        name: var_name,
                        value,
                    });
                }

                // expression statement (currently mainly useful for calls): <expr>;
                let expr = self.parse_value_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Expr(expr))
            }
            TokenKind::Keyword(Keyword::This)
            | TokenKind::Keyword(Keyword::New)
            | TokenKind::Symbol('(')
            | TokenKind::Symbol('[')
            | TokenKind::Symbol('-')
            | TokenKind::Number(_)
            | TokenKind::Decimal(_)
            | TokenKind::String(_)
            | TokenKind::Keyword(Keyword::True)
            | TokenKind::Keyword(Keyword::False) => {
                let expr = self.parse_value_expr()?;
                self.expect_symbol(';')?;
                Ok(Stmt::Expr(expr))
            }
            TokenKind::Keyword(Keyword::If) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let cond = self.parse_value_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol('{')?;

                let mut then_body = Vec::new();
                while !self.is_symbol('}')? {
                    then_body.push(self.parse_stmt()?);
                }
                self.expect_symbol('}')?;

                let else_body = if matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::Else)) {
                    self.expect_keyword(Keyword::Else)?;
                    self.expect_symbol('{')?;
                    let mut else_stmts = Vec::new();
                    while !self.is_symbol('}')? {
                        else_stmts.push(self.parse_stmt()?);
                    }
                    self.expect_symbol('}')?;
                    Some(else_stmts)
                } else {
                    None
                };

                Ok(Stmt::If {
                    cond,
                    then_body,
                    else_body,
                })
            }
            TokenKind::Keyword(Keyword::While) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let cond = self.parse_value_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol('{')?;

                let mut body = Vec::new();
                while !self.is_symbol('}')? {
                    body.push(self.parse_stmt()?);
                }
                self.expect_symbol('}')?;
                Ok(Stmt::While { cond, body })
            }
            TokenKind::Keyword(Keyword::For) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let var = self.expect_ident()?;
                self.expect_keyword(Keyword::In)?;
                let start = self.parse_value_expr()?;
                // range operator '..' as two '.' symbols
                self.expect_symbol('.')?;
                self.expect_symbol('.')?;
                let end = self.parse_value_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol('{')?;

                let mut body = Vec::new();
                while !self.is_symbol('}')? {
                    body.push(self.parse_stmt()?);
                }
                self.expect_symbol('}')?;
                Ok(Stmt::ForRange {
                    var,
                    start,
                    end,
                    body,
                })
            }
            TokenKind::Keyword(Keyword::Switch) => {
                self.bump()?;
                self.expect_symbol('(')?;
                let expr = self.parse_value_expr()?;
                self.expect_symbol(')')?;
                self.expect_symbol('{')?;

                let mut cases: Vec<(Literal, Vec<Stmt>)> = Vec::new();
                let mut default: Option<Vec<Stmt>> = None;

                while !self.is_symbol('}')? {
                    match &self.lookahead.kind {
                        TokenKind::Keyword(Keyword::Case) => {
                            self.bump()?;
                            let lit = self.parse_literal()?;
                            self.expect_symbol(':')?;
                            self.expect_symbol('{')?;
                            let mut body = Vec::new();
                            while !self.is_symbol('}')? {
                                body.push(self.parse_stmt()?);
                            }
                            self.expect_symbol('}')?;
                            cases.push((lit, body));
                        }
                        TokenKind::Keyword(Keyword::Default) => {
                            self.bump()?;
                            self.expect_symbol(':')?;
                            if default.is_some() {
                                let (line, col) = self.pos();
                                return Err(anyhow!(
                                    "Duplicate default in switch at {}:{}",
                                    line,
                                    col
                                ));
                            }
                            self.expect_symbol('{')?;
                            let mut body = Vec::new();
                            while !self.is_symbol('}')? {
                                body.push(self.parse_stmt()?);
                            }
                            self.expect_symbol('}')?;
                            default = Some(body);
                        }
                        other => {
                            let (line, col) = self.pos();
                            return Err(anyhow!(
                                "Expected 'case' or 'default' in switch at {}:{} but got {:?}",
                                line,
                                col,
                                other
                            ));
                        }
                    }
                }

                self.expect_symbol('}')?;
                Ok(Stmt::Switch {
                    expr,
                    cases,
                    default,
                })
            }
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
            TokenKind::Keyword(Keyword::Return) => {
                self.bump()?;
                let value = if self.is_symbol(';')? {
                    None
                } else {
                    Some(self.parse_value_expr()?)
                };
                self.expect_symbol(';')?;
                Ok(Stmt::Return(value))
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected statement at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn parse_value_expr(&mut self) -> Result<ValueExpr> {
        self.parse_assign_expr()
    }

    fn parse_assign_expr(&mut self) -> Result<ValueExpr> {
        let left = self.parse_value_or_expr()?;

        if matches!(self.lookahead.kind, TokenKind::Symbol('='))
            && !matches!(self.lookahead2.kind, TokenKind::Symbol('='))
        {
            self.expect_symbol('=')?;
            let value = self.parse_assign_expr()?;
            match left {
                ValueExpr::Var(_) | ValueExpr::Member(_, _) => Ok(ValueExpr::Assign {
                    target: Box::new(left),
                    value: Box::new(value),
                }),
                _ => {
                    let (line, col) = self.pos();
                    Err(anyhow!("Invalid assignment target at {}:{}", line, col))
                }
            }
        } else {
            Ok(left)
        }
    }

    fn parse_value_or_expr(&mut self) -> Result<ValueExpr> {
        let mut left = self.parse_value_and_expr()?;
        while matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::Or)) {
            self.expect_keyword(Keyword::Or)?;
            let right = self.parse_value_and_expr()?;
            left = ValueExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_value_and_expr(&mut self) -> Result<ValueExpr> {
        let mut left = self.parse_value_cmp_expr()?;
        while matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::And)) {
            self.expect_keyword(Keyword::And)?;
            let right = self.parse_value_cmp_expr()?;
            left = ValueExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_value_cmp_expr(&mut self) -> Result<ValueExpr> {
        let left = self.parse_add_expr()?;

        if !matches!(
            self.lookahead.kind,
            TokenKind::Symbol('=')
                | TokenKind::Symbol('!')
                | TokenKind::Symbol('<')
                | TokenKind::Symbol('>')
        ) {
            return Ok(left);
        }

        // Comparison operators: ==, !=, <, >, <=, >=
        match &self.lookahead.kind {
            TokenKind::Symbol('=') => {
                // Only treat as comparison if it's '=='
                if !matches!(self.lookahead2.kind, TokenKind::Symbol('=')) {
                    return Ok(left);
                }
                self.expect_symbol('=')?;
                self.expect_symbol('=')?;
                let right = self.parse_add_expr()?;
                Ok(ValueExpr::Eq(Box::new(left), Box::new(right)))
            }
            TokenKind::Symbol('!') => {
                self.bump()?;
                if !self.is_symbol('=')? {
                    let (line, col) = self.pos();
                    return Err(anyhow!(
                        "Unexpected '!' at {}:{}. Use '!=' for inequality.",
                        line,
                        col
                    ));
                }
                self.expect_symbol('=')?;
                let right = self.parse_add_expr()?;
                Ok(ValueExpr::Neq(Box::new(left), Box::new(right)))
            }
            TokenKind::Symbol('<') => {
                self.bump()?;
                let is_eq = self.is_symbol('=')?;
                if is_eq {
                    self.expect_symbol('=')?;
                }
                let right = self.parse_add_expr()?;
                Ok(if is_eq {
                    ValueExpr::Lte(Box::new(left), Box::new(right))
                } else {
                    ValueExpr::Lt(Box::new(left), Box::new(right))
                })
            }
            TokenKind::Symbol('>') => {
                self.bump()?;
                let is_eq = self.is_symbol('=')?;
                if is_eq {
                    self.expect_symbol('=')?;
                }
                let right = self.parse_add_expr()?;
                Ok(if is_eq {
                    ValueExpr::Gte(Box::new(left), Box::new(right))
                } else {
                    ValueExpr::Gt(Box::new(left), Box::new(right))
                })
            }
            _ => Ok(left),
        }
    }

    fn parse_add_expr(&mut self) -> Result<ValueExpr> {
        let mut left = self.parse_mul_expr()?;
        while self.is_symbol('+')? || self.is_symbol('-')? {
            if self.is_symbol('+')? {
                self.expect_symbol('+')?;
                let right = self.parse_mul_expr()?;
                left = ValueExpr::Add(Box::new(left), Box::new(right));
            } else {
                self.expect_symbol('-')?;
                let right = self.parse_mul_expr()?;
                left = ValueExpr::Sub(Box::new(left), Box::new(right));
            }
        }
        Ok(left)
    }

    fn parse_mul_expr(&mut self) -> Result<ValueExpr> {
        let mut left = self.parse_unary_expr()?;
        while self.is_symbol('*')? || self.is_symbol('/')? || self.is_symbol('%')? {
            if self.is_symbol('*')? {
                self.expect_symbol('*')?;
                let right = self.parse_unary_expr()?;
                left = ValueExpr::Mul(Box::new(left), Box::new(right));
            } else if self.is_symbol('/')? {
                self.expect_symbol('/')?;
                let right = self.parse_unary_expr()?;
                left = ValueExpr::Div(Box::new(left), Box::new(right));
            } else {
                self.expect_symbol('%')?;
                let right = self.parse_unary_expr()?;
                left = ValueExpr::Mod(Box::new(left), Box::new(right));
            }
        }
        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> Result<ValueExpr> {
        if self.is_symbol('-')? {
            self.expect_symbol('-')?;
            let inner = self.parse_unary_expr()?;
            return Ok(ValueExpr::Neg(Box::new(inner)));
        }
        self.parse_postfix_expr()
    }

    fn parse_postfix_expr(&mut self) -> Result<ValueExpr> {
        let mut expr = self.parse_value_atom()?;

        loop {
            if self.is_symbol('[')? {
                self.expect_symbol('[')?;
                let idx = self.parse_value_expr()?;
                self.expect_symbol(']')?;
                expr = ValueExpr::Index(Box::new(expr), Box::new(idx));
                continue;
            }

            if self.is_symbol('.')? {
                // Disambiguate range operator `..` used by for-loops.
                // If the next token is also '.', do not treat this as property access.
                if matches!(self.lookahead2.kind, TokenKind::Symbol('.')) {
                    break;
                }
                self.expect_symbol('.')?;
                let name = self.expect_ident()?;

                // Keep `.length` as a builtin for arrays.
                if name == "length" && !self.is_symbol('(')? {
                    expr = ValueExpr::Length(Box::new(expr));
                    continue;
                }

                // Method call: obj.method(...)
                if self.is_symbol('(')? {
                    self.expect_symbol('(')?;
                    let mut args = Vec::new();
                    if !self.is_symbol(')')? {
                        args.push(self.parse_value_expr()?);
                        while self.is_symbol(',')? {
                            self.expect_symbol(',')?;
                            args.push(self.parse_value_expr()?);
                        }
                    }
                    self.expect_symbol(')')?;

                    expr = ValueExpr::MethodCall {
                        receiver: Box::new(expr),
                        name,
                        args,
                    };
                    continue;
                }

                expr = ValueExpr::Member(Box::new(expr), name);
                continue;
            }

            break;
        }

        Ok(expr)
    }

    fn parse_value_atom(&mut self) -> Result<ValueExpr> {
        if self.is_symbol('(')? {
            self.expect_symbol('(')?;
            let inner = self.parse_value_expr()?;
            self.expect_symbol(')')?;
            return Ok(inner);
        }

        if self.is_symbol('[')? {
            self.expect_symbol('[')?;
            let mut items = Vec::new();
            if !self.is_symbol(']')? {
                items.push(self.parse_value_expr()?);
                while self.is_symbol(',')? {
                    self.expect_symbol(',')?;
                    items.push(self.parse_value_expr()?);
                }
            }
            self.expect_symbol(']')?;
            return Ok(ValueExpr::Array(items));
        }

        match &self.lookahead.kind {
            TokenKind::Keyword(Keyword::Select) => {
                self.parse_db_select_expr()
            }
            TokenKind::Keyword(Keyword::Insert) => {
                self.parse_db_insert_expr()
            }
            TokenKind::Keyword(Keyword::Update) => {
                self.parse_db_update_expr()
            }
            TokenKind::Keyword(Keyword::Delete) => {
                self.parse_db_delete_expr()
            }
            TokenKind::Keyword(Keyword::This) => {
                self.bump()?;
                Ok(ValueExpr::This)
            }
            TokenKind::Keyword(Keyword::New) => {
                self.bump()?;
                let class_name = self.expect_ident()?;
                self.expect_symbol('(')?;
                self.expect_symbol(')')?;
                Ok(ValueExpr::New { class_name })
            }
            TokenKind::Ident(name) => {
                let v = name.clone();
                // function call if next token is '('
                if matches!(self.lookahead2.kind, TokenKind::Symbol('(')) {
                    self.bump()?; // ident
                    self.expect_symbol('(')?;
                    let mut args = Vec::new();
                    if !self.is_symbol(')')? {
                        args.push(self.parse_value_expr()?);
                        while self.is_symbol(',')? {
                            self.expect_symbol(',')?;
                            args.push(self.parse_value_expr()?);
                        }
                    }
                    self.expect_symbol(')')?;
                    Ok(ValueExpr::Call { name: v, args })
                } else {
                    self.bump()?;
                    Ok(ValueExpr::Var(v))
                }
            }
            _ => Ok(ValueExpr::Literal(self.parse_literal()?)),
        }
    }

    fn parse_db_select_expr(&mut self) -> Result<ValueExpr> {
        self.expect_keyword(Keyword::Select)?;
        let entity = self.expect_ident()?;

        let (where_field, where_value) = if matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::Where)) {
            self.expect_keyword(Keyword::Where)?;
            let field = self.expect_ident()?;
            self.expect_symbol('=')?;
            let value = self.parse_value_expr()?;
            (Some(field), Some(Box::new(value)))
        } else {
            (None, None)
        };

        Ok(ValueExpr::DbSelect {
            entity,
            where_field,
            where_value,
        })
    }

    fn parse_db_insert_expr(&mut self) -> Result<ValueExpr> {
        self.expect_keyword(Keyword::Insert)?;
        let entity = self.expect_ident()?;
        self.expect_keyword(Keyword::Set)?;

        let mut assignments: Vec<(String, ValueExpr)> = Vec::new();
        loop {
            let field = self.expect_ident()?;
            self.expect_symbol('=')?;
            let value = self.parse_value_expr()?;
            assignments.push((field, value));

            if self.is_symbol(',')? {
                self.expect_symbol(',')?;
                continue;
            }
            break;
        }

        Ok(ValueExpr::DbInsert { entity, assignments })
    }

    fn parse_db_update_expr(&mut self) -> Result<ValueExpr> {
        self.expect_keyword(Keyword::Update)?;
        let entity = self.expect_ident()?;
        self.expect_keyword(Keyword::Set)?;

        let mut assignments: Vec<(String, ValueExpr)> = Vec::new();
        loop {
            let field = self.expect_ident()?;
            self.expect_symbol('=')?;
            let value = self.parse_value_expr()?;
            assignments.push((field, value));

            if self.is_symbol(',')? {
                self.expect_symbol(',')?;
                continue;
            }
            break;
        }

        self.expect_keyword(Keyword::Where)?;
        let where_field = self.expect_ident()?;
        self.expect_symbol('=')?;
        let where_value = self.parse_value_expr()?;

        Ok(ValueExpr::DbUpdate {
            entity,
            assignments,
            where_field,
            where_value: Box::new(where_value),
        })
    }

    fn parse_db_delete_expr(&mut self) -> Result<ValueExpr> {
        self.expect_keyword(Keyword::Delete)?;
        let entity = self.expect_ident()?;
        self.expect_keyword(Keyword::Where)?;
        let where_field = self.expect_ident()?;
        self.expect_symbol('=')?;
        let where_value = self.parse_value_expr()?;

        Ok(ValueExpr::DbDelete {
            entity,
            where_field,
            where_value: Box::new(where_value),
        })
    }

    fn parse_select(&mut self) -> Result<SelectStmt> {
        self.expect_keyword(Keyword::Select)?;

        let target = if self.is_symbol('*')? {
            self.expect_symbol('*')?;
            SelectTarget::Star
        } else {
            let first = self.expect_ident()?;
            // Heuristic to preserve typed select (`select User from ...`) while enabling
            // single-field projections (`select id from ...`).
            let is_typed_entity = first
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase());

            if is_typed_entity {
                SelectTarget::Entity(first)
            } else {
                let mut fields = vec![first];
                while self.is_symbol(',')? {
                    self.expect_symbol(',')?;
                    fields.push(self.expect_ident()?);
                }
                SelectTarget::Fields(fields)
            }
        };

        self.expect_keyword(Keyword::From)?;
        let from = self.expect_ident()?;

        let where_expr = if matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::Where)) {
            self.expect_keyword(Keyword::Where)?;
            Some(self.parse_or_expr()?)
        } else {
            None
        };

        self.expect_symbol(';')?;
        Ok(SelectStmt {
            target,
            from,
            where_expr,
        })
    }

    // or has the lowest precedence
    fn parse_or_expr(&mut self) -> Result<Expr> {
        let mut left = self.parse_and_expr()?;
        while matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::Or)) {
            self.expect_keyword(Keyword::Or)?;
            let right = self.parse_and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    // and binds tighter than or
    fn parse_and_expr(&mut self) -> Result<Expr> {
        let mut left = self.parse_primary_expr()?;
        while matches!(self.lookahead.kind, TokenKind::Keyword(Keyword::And)) {
            self.expect_keyword(Keyword::And)?;
            let right = self.parse_primary_expr()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_primary_expr(&mut self) -> Result<Expr> {
        if self.is_symbol('(')? {
            self.expect_symbol('(')?;
            let inner = self.parse_or_expr()?;
            self.expect_symbol(')')?;
            return Ok(inner);
        }

        let field = self.expect_ident()?;
        let op = self.parse_cmp_op()?;
        let value = self.parse_literal()?;
        Ok(Expr::Cmp(CmpExpr { field, op, value }))
    }

    fn parse_literal(&mut self) -> Result<Literal> {
        match &self.lookahead.kind {
            TokenKind::Number(n) => {
                let v = *n;
                self.bump()?;
                Ok(Literal::Int(v))
            }
            TokenKind::Decimal(s) => {
                let v = s.clone();
                self.bump()?;
                Ok(Literal::Decimal(v))
            }
            TokenKind::String(s) => {
                let v = s.clone();
                self.bump()?;
                Ok(Literal::Str(v))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump()?;
                Ok(Literal::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump()?;
                Ok(Literal::Bool(false))
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected literal at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn parse_cmp_op(&mut self) -> Result<CmpOp> {
        // Supports: =, >, <, >=, <=
        match &self.lookahead.kind {
            TokenKind::Symbol('=') => {
                self.bump()?;
                Ok(CmpOp::Eq)
            }
            TokenKind::Symbol('>') => {
                self.bump()?;
                if self.is_symbol('=')? {
                    self.expect_symbol('=')?;
                    Ok(CmpOp::Gte)
                } else {
                    Ok(CmpOp::Gt)
                }
            }
            TokenKind::Symbol('<') => {
                self.bump()?;
                if self.is_symbol('=')? {
                    self.expect_symbol('=')?;
                    Ok(CmpOp::Lte)
                } else {
                    Ok(CmpOp::Lt)
                }
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected comparison operator at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn parse_dbcontext(&mut self) -> Result<(DbContextDecl, Vec<EntityDecl>)> {
        match &self.lookahead.kind {
            TokenKind::Keyword(Keyword::Context) | TokenKind::Keyword(Keyword::DbContext) => {
                self.bump()?;
            }
            _ => {
                let (line, col) = self.pos();
                return Err(anyhow!(
                    "Expected context/dbcontext at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ));
            }
        }
        let name = self.expect_ident()?;
        self.expect_symbol(':')?;
        let driver = self.expect_ident()?;
        let url = if let TokenKind::String(s) = &self.lookahead.kind {
            let s = s.clone();
            self.bump()?;
            Some(s)
        } else {
            None
        };

        let mut entities = Vec::new();
        let mut sets: Vec<DbSetDecl> = Vec::new();
        if self.is_symbol('{')? {
            self.expect_symbol('{')?;
            while !self.is_symbol('}')? {
                match &self.lookahead.kind {
                    TokenKind::Keyword(Keyword::Set) => {
                        self.bump()?;
                        let entity = self.expect_ident()?;
                        let set_name = self.expect_ident()?;
                        self.expect_symbol(';')?;
                        sets.push(DbSetDecl {
                            entity,
                            name: set_name,
                        });
                    }
                    TokenKind::Keyword(Keyword::Entity) => entities.push(self.parse_entity()?),
                    _ => {
                        let (line, col) = self.pos();
                        return Err(anyhow!(
                            "Only `set` and `entity` declarations are allowed inside context blocks at {}:{} (got {:?})",
                            line,
                            col,
                            self.lookahead.kind
                        ));
                    }
                }
            }
            self.expect_symbol('}')?;
        } else {
            self.expect_symbol(';')?;
        }

        Ok((DbContextDecl { name, driver, url, sets }, entities))
    }

    fn parse_entity(&mut self) -> Result<EntityDecl> {
        self.expect_keyword(Keyword::Entity)?;
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;
        let mut fields = Vec::new();
        while !self.is_symbol('}')? {
            let field_name = self.expect_ident()?;
            let ty = self.parse_type_spec()?;
            let mods = self.parse_field_mods()?;
            self.expect_symbol(';')?;
            fields.push(FieldDecl {
                name: field_name,
                ty,
                mods,
            });
        }
        self.expect_symbol('}')?;
        Ok(EntityDecl { name, fields })
    }

    fn parse_field_mods(&mut self) -> Result<FieldMods> {
        let mut mods = FieldMods::default();
        loop {
            match &self.lookahead.kind {
                TokenKind::Keyword(Keyword::Nullable) => {
                    self.bump()?;
                    mods.nullable = true;
                }
                TokenKind::Keyword(Keyword::NotNull) => {
                    self.bump()?;
                    mods.nullable = false;
                }
                TokenKind::Keyword(Keyword::Unique) => {
                    self.bump()?;
                    mods.unique = true;
                }
                TokenKind::Keyword(Keyword::Pk) => {
                    self.bump()?;
                    mods.primary_key = true;
                }
                _ => break,
            }
        }
        Ok(mods)
    }

    fn parse_type_spec(&mut self) -> Result<TypeSpec> {
        let mut name = self.expect_ident()?.to_ascii_lowercase();
        if name == "string" {
            name = "text".to_string();
        }
        let mut args = Vec::new();
        if self.is_symbol('(')? {
            self.expect_symbol('(')?;
            if !self.is_symbol(')')? {
                loop {
                    let n = self.expect_number()?;
                    args.push(n);
                    if self.is_symbol(',')? {
                        self.expect_symbol(',')?;
                        continue;
                    }
                    break;
                }
            }
            self.expect_symbol(')')?;
        }
        Ok(TypeSpec { name, args })
    }

    fn expect_keyword(&mut self, kw: Keyword) -> Result<()> {
        match &self.lookahead.kind {
            TokenKind::Keyword(k) if *k == kw => {
                self.bump()?;
                Ok(())
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected keyword {:?} at {}:{} but got {:?}",
                    kw,
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn expect_ident(&mut self) -> Result<String> {
        match &self.lookahead.kind {
            TokenKind::Ident(s) => {
                let v = s.clone();
                self.bump()?;
                Ok(v)
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected identifier at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn expect_string(&mut self) -> Result<String> {
        match &self.lookahead.kind {
            TokenKind::String(s) => {
                let out = s.clone();
                self.bump()?;
                Ok(out)
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected string literal at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn expect_number(&mut self) -> Result<i64> {
        match &self.lookahead.kind {
            TokenKind::Number(n) => {
                let v = *n;
                self.bump()?;
                Ok(v)
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected number at {}:{} but got {:?}",
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn expect_symbol(&mut self, sym: char) -> Result<()> {
        match &self.lookahead.kind {
            TokenKind::Symbol(c) if *c == sym => {
                self.bump()?;
                Ok(())
            }
            _ => {
                let (line, col) = self.pos();
                Err(anyhow!(
                    "Expected symbol '{}' at {}:{} but got {:?}",
                    sym,
                    line,
                    col,
                    self.lookahead.kind
                ))
            }
        }
    }

    fn is_symbol(&self, sym: char) -> Result<bool> {
        Ok(matches!(self.lookahead.kind, TokenKind::Symbol(c) if c == sym))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dbcontext_with_optional_url() {
        let src = r#"
            dbcontext AppDb : Postgres "postgres://localhost/test";
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.dbcontexts.len(), 1);
        assert_eq!(program.dbcontexts[0].name, "AppDb");
        assert_eq!(program.dbcontexts[0].driver, "Postgres");
        assert_eq!(
            program.dbcontexts[0].url,
            Some("postgres://localhost/test".to_string())
        );
    }

    #[test]
    fn parses_context_block_with_entities() {
        let src = r#"
            context AppDb : Postgres {
                entity User {
                    id uuid pk;
                    email text unique;
                }
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.dbcontexts.len(), 1);
        assert_eq!(program.dbcontexts[0].name, "AppDb");
        assert_eq!(program.entities.len(), 1);
        assert_eq!(program.entities[0].name, "User");
    }

    #[test]
    fn parses_context_block_with_sets() {
        let src = r#"
            entity TodoEntity { id uuid pk; }
            context AppDb : Postgres {
                set TodoEntity Todos;
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.dbcontexts.len(), 1);
        assert_eq!(program.dbcontexts[0].sets.len(), 1);
        assert_eq!(program.dbcontexts[0].sets[0].entity, "TodoEntity");
        assert_eq!(program.dbcontexts[0].sets[0].name, "Todos");
    }

    #[test]
    fn parses_entity_fields_and_args() {
        let src = r#"
            context AppContext : Postgres;

            entity User {
                id uuid;
                name text(50);
                age int(0,200);
                balance decimal(18,2);
                created_at datetime;
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.dbcontexts.len(), 1);
        assert_eq!(program.entities.len(), 1);
        assert_eq!(program.entities[0].fields.len(), 5);
        assert_eq!(program.entities[0].fields[1].ty.name, "text");
        assert_eq!(program.entities[0].fields[1].ty.args, vec![50]);
        assert_eq!(program.entities[0].fields[2].ty.args, vec![0, 200]);
    }

    #[test]
    fn parses_and_validates_typed_select() {
        let src = r#"
            entity User {
                id uuid;
                age int(0,200);
            }

            select User from user where age > 18;
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.selects.len(), 1);
    }

    #[test]
    fn parses_projection_field_list() {
        let src = r#"
            entity User { id uuid; name text; active bool; }
            select id, name from User where active = true;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.selects.len(), 1);
    }

    #[test]
    fn projection_unknown_field_fails_validation() {
        let src = r#"
            entity User { id uuid; }
            select id, missing from User;
        "#;
        let program = parse_program(src).unwrap();
        let err = validate_program(&program).unwrap_err().to_string();
        assert!(err.contains("select projection"));
    }

    #[test]
    fn select_unknown_field_fails_validation() {
        let src = r#"
            entity User { id uuid; }
            select * from User where missing = 1;
        "#;

        let program = parse_program(src).unwrap();
        let err = validate_program(&program).unwrap_err().to_string();
        assert!(err.contains("unknown field"));
    }

    #[test]
    fn parses_and_or_precedence() {
        let src = r#"
            entity User { age int; active bool; name text; }
            select * from User where name = "a" or active = true and age > 1;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.selects.len(), 1);
    }

    #[test]
    fn parses_parentheses_grouping() {
        let src = r#"
            entity User { age int; active bool; name text; }
            select * from User where (active = true or name = "a") and age > 1;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
    }

    #[test]
    fn disallows_ordering_on_text_fields() {
        let src = r#"
            entity User { name text; }
            select * from User where name > "a";
        "#;
        let program = parse_program(src).unwrap();
        let err = validate_program(&program).unwrap_err().to_string();
        assert!(err.contains("operator mismatch"));
    }

    #[test]
    fn select_where_string_literal_typechecks() {
        let src = r#"
            entity User {
                id uuid;
                name text(50);
            }
            select * from User where name = "alice";
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
    }

    #[test]
    fn select_where_bool_literal_typechecks() {
        let src = r#"
            entity Flags { active bool; }
            select * from Flags where active = true;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
    }

    #[test]
    fn select_where_type_mismatch_fails() {
        let src = r#"
            entity User { age int; }
            select * from User where age = "nope";
        "#;
        let program = parse_program(src).unwrap();
        let err = validate_program(&program).unwrap_err().to_string();
        assert!(err.contains("type mismatch"));
    }

    #[test]
    fn select_where_decimal_literal_typechecks() {
        let src = r#"
            entity Invoice { total decimal(18,2); }
            select * from Invoice where total > 12.34;
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
    }

    #[test]
    fn parse_error_includes_line_col() {
        // Missing ';' after uuid
        let src = "entity User { id uuid }";
        let err = parse_program(src).unwrap_err().to_string();
        assert!(err.contains(":"), "error should include line:col, got: {err}");
        assert!(err.contains("Expected symbol ';'"));
    }

    #[test]
    fn parses_hello_world_function() {
        let src = r#"
            function main() {
                print("Hello, world!");
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.functions.len(), 1);
        assert_eq!(program.functions[0].name, "main");
    }

    #[test]
    fn parses_route_blocks() {
        let src = r#"
            route get "/" {
                return "hi";
            }

            route get "/health" {
                return [200, "ok"];
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.routes.len(), 2);
        assert_eq!(program.routes[0].method, "get");
        assert_eq!(program.routes[0].path, "/");
    }

    #[test]
    fn parses_controller_with_routes() {
        let src = r#"
            controller TodoApi {
                route get "/" { return "ok"; }
                route get "/todos/{id}" { return id; }
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.controllers.len(), 1);
        assert_eq!(program.controllers[0].name, "TodoApi");
    }

    #[test]
    fn parses_class_new_member_and_method_call() {
        let src = r#"
            class Counter {
                let value = 0;

                route get "/counter" {
                    return this.value;
                }

                function inc() {
                    this.value = this.value + 1;
                }
            }

            function main() {
                let c = new Counter();
                c.inc();
                print(c.value);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.classes.len(), 1);
        assert_eq!(program.functions.len(), 1);
    }

    #[test]
    fn schema_only_parsing_ignores_route_bodies() {
        let src = r#"
            entity TodoEntity {
                id int pk;
                title varchar(100);
            }

            context Database : Postgres {
                set TodoEntity todos;
            }

            route get "/todos" {
                return select * from Database.todos;
            }
        "#;

        let program = parse_program_schema_only(src).unwrap();
        validate_program(&program).unwrap();
        assert_eq!(program.entities.len(), 1);
        assert_eq!(program.dbcontexts.len(), 1);
        assert_eq!(program.routes.len(), 0);
        assert_eq!(program.selects.len(), 0);
    }
}
