//! Declaration parsers: dbcontext, entity/class, route, function, middleware,
//! const, dome, mount, group, import, namespace.
//!
//! Every method here is attached to the `Parser` state struct that lives in
//! `mod.rs`. They share the lexer/lookahead scaffolding (`bump`, `expect_*`,
//! `check_*`) defined on `Parser` in the parent module.

use anyhow::Result;

use crate::ast::{
    ConstDecl, DbContextDecl, FieldDecl, FieldReference, FunctionDecl, MiddlewareDecl, ModelDecl,
    ModelKind, NavJoin, NavOrder, NavigationField, NavigationKind, OnDeleteAction, Program,
    RouteDecl, RouteProtocol, SortDir, TypeSpec, TypedParam, Visibility,
};
use crate::lexer::{Keyword, TokenKind};

use super::{visibility_from, GroupFrame, Parser};

impl<'a> Parser<'a> {
    pub(super) fn parse_dbcontext_decl(&mut self) -> Result<DbContextDecl> {
        let offset = self.current.offset;
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
            offset,
            file_idx: 0,
        })
    }

    /// `const NAME = <expr>;` — a module-level immutable binding. Consts have
    /// no visibility modifier; the constant-expression restriction is enforced
    /// in `validate_program`, not here.
    pub(super) fn parse_const_decl(&mut self) -> Result<ConstDecl> {
        let offset = self.current.offset;
        self.bump()?; // consume `const`
        let name = self.expect_ident("expected const name after 'const'")?;
        self.expect_symbol('=')?;
        let expr = self.parse_expr()?;
        self.expect_symbol(';')?;
        Ok(ConstDecl {
            name,
            expr,
            offset,
            file_idx: 0,
        })
    }

    pub(super) fn skip_braced_block(&mut self) -> Result<()> {
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
    pub(super) fn parse_import_stmt(&mut self, program: &mut Program) -> Result<()> {
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
    pub(super) fn parse_namespace_stmt(&mut self, program: &Program) -> Result<()> {
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
    pub(super) fn parse_mount_stmt(&mut self) -> Result<crate::ast::MountDecl> {
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
    pub(super) fn parse_group_block(&mut self, program: &mut Program) -> Result<()> {
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

    pub(super) fn parse_qualified_name(&mut self) -> Result<String> {
        let parts = self.parse_qualified_path()?;
        Ok(parts.join("."))
    }

    /// Parses a dot-separated identifier sequence into its parts.
    pub(super) fn parse_qualified_path(&mut self) -> Result<Vec<String>> {
        let mut parts = vec![self.expect_ident("expected identifier")?];
        while self.check_symbol('.') {
            self.expect_symbol('.')?;
            parts.push(self.expect_ident("expected identifier after '.'")?);
        }
        Ok(parts)
    }

    pub(super) fn parse_model_decl(&mut self, kind: ModelKind) -> Result<ModelDecl> {
        let offset = self.current.offset;
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
            let mut is_unique = false;
            let mut references: Option<FieldReference> = None;

            loop {
                match self.current.kind.clone() {
                    TokenKind::Ident(v) if v.eq_ignore_ascii_case("nullable") => {
                        is_nullable = true;
                        self.bump()?;
                    }
                    TokenKind::Ident(v) if v.eq_ignore_ascii_case("unique") => {
                        is_unique = true;
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
                is_unique,
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
            offset,
            file_idx: 0,
        })
    }

    /// Parse the rest of `name: List<Target> via Target.col;` or
    /// `name: Target via Target.col;` after the `:` has been consumed.
    pub(super) fn parse_navigation_remainder(
        &mut self,
        field_name: &str,
    ) -> Result<NavigationField> {
        let head = self.expect_ident("expected target type after ':'")?;
        let (is_list, target_entity) = if head.eq_ignore_ascii_case("List") {
            self.expect_symbol('<')?;
            let inner = self.expect_ident("expected entity name inside List<...>")?;
            self.expect_symbol('>')?;
            (true, inner)
        } else {
            (false, head)
        };

        // optional column subset: `{ col, col, ... }`
        let projection = if self.check_symbol('{') {
            self.expect_symbol('{')?;
            let mut cols = Vec::new();
            if !self.check_symbol('}') {
                cols.push(self.expect_ident("expected column name in navigation projection")?);
                while self.check_symbol(',') {
                    self.expect_symbol(',')?;
                    cols.push(self.expect_ident("expected column name in navigation projection")?);
                }
            }
            self.expect_symbol('}')?;
            cols
        } else {
            Vec::new()
        };

        let via_kw = self.expect_ident("expected 'via' in navigation declaration")?;
        if !via_kw.eq_ignore_ascii_case("via") {
            return Err(self.error_here("expected 'via' in navigation declaration"));
        }

        // `via JoinTable(near, far)` → many-to-many through a link table.
        // `via Target.col`  (dotted)  → the target holds the FK (has-many / has-one).
        // `via local_col`   (bare)    → this entity holds the FK (belongs-to).
        let first = self.expect_ident("expected column, entity, or join table after 'via'")?;
        let (kind, target_field, join) = if self.check_symbol('(') {
            self.expect_symbol('(')?;
            let near = self.expect_ident("expected near column in 'via JoinTable(near, far)'")?;
            self.expect_symbol(',')?;
            let far = self.expect_ident("expected far column in 'via JoinTable(near, far)'")?;
            self.expect_symbol(')')?;
            if !is_list {
                return Err(self.error_here(
                    "many-to-many navigation 'via JoinTable(near, far)' must be declared as List<...>",
                ));
            }
            (
                NavigationKind::ManyToMany,
                far.clone(),
                Some(NavJoin {
                    table: first,
                    near_col: near,
                    far_col: far,
                }),
            )
        } else if self.check_symbol('.') {
            self.expect_symbol('.')?;
            let col = self.expect_ident("expected target column name after '.'")?;
            if !first.eq_ignore_ascii_case(&target_entity) {
                return Err(self.error_here(
                    "navigation 'via Target.col' must reference the same entity declared on the left side",
                ));
            }
            let kind = if is_list {
                NavigationKind::OneToMany
            } else {
                NavigationKind::OneToOne
            };
            (kind, col, None)
        } else {
            if is_list {
                return Err(self.error_here(
                    "List<...> navigation needs 'via Target.fk' (the target holds the FK) or 'via JoinTable(near, far)' (many-to-many); a bare column is a belongs-to (single) relation",
                ));
            }
            (NavigationKind::BelongsTo, first, None)
        };

        // optional `orderby <target col> [asc|desc]` — orders the materialised
        // collection (`json_agg(... ORDER BY ...)`). Bare column or `Target.col`.
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
            Some(NavOrder { col: field, dir })
        } else {
            None
        };
        self.expect_symbol(';')?;

        Ok(NavigationField {
            name: field_name.to_string(),
            kind,
            target_entity,
            target_field,
            projection,
            join,
            order_by,
        })
    }

    pub(super) fn parse_dome_decl(&mut self, program: &mut Program, is_pub: bool) -> Result<()> {
        self.expect_keyword(Keyword::Dome)?;
        let dome_name = self.expect_ident("expected dome name")?;
        self.expect_symbol('{')?;

        while !self.check_symbol('}') {
            // Per-member modifiers, mirroring the top-level declaration loop.
            // Business logic lives in domes, so the modifiers that work on a
            // top-level `function` have to work here too — otherwise `async`
            // is only available in the one place domain code isn't written,
            // and the editor's "Async Function" snippet expands into code the
            // parser rejects.
            let mut member_vis: Option<bool> = None;
            if matches!(self.current.kind, TokenKind::Keyword(Keyword::Public)) {
                self.bump()?;
                member_vis = Some(true);
            } else if matches!(self.current.kind, TokenKind::Keyword(Keyword::Private)) {
                self.bump()?;
                member_vis = Some(false);
            }

            let is_async = if matches!(self.current.kind, TokenKind::Keyword(Keyword::Async)) {
                self.bump()?;
                true
            } else {
                false
            };

            match &self.current.kind {
                TokenKind::Keyword(Keyword::Function) => {
                    let mut decl = self.parse_function_decl(Some(&dome_name))?;
                    decl.namespace = self.current_namespace.clone();
                    // An explicit modifier on the member wins over the dome's.
                    decl.visibility = visibility_from(member_vis.unwrap_or(is_pub));
                    decl.is_async = is_async;
                    program.functions.push(decl);
                }
                _ if is_async => {
                    return Err(self.error_here("expected 'function' after 'async'"));
                }
                _ => return Err(self.error_here("expected function declaration inside dome block")),
            }
        }

        self.expect_symbol('}')?;
        Ok(())
    }

    pub(super) fn parse_route_decl(&mut self) -> Result<RouteDecl> {
        let offset = self.current.offset;
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
                offset,
                file_idx: 0,
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
            offset,
            file_idx: 0,
        })
    }

    pub(super) fn parse_field_reference(&mut self) -> Result<FieldReference> {
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

    pub(super) fn parse_middleware_decl(&mut self) -> Result<MiddlewareDecl> {
        let offset = self.current.offset;
        self.expect_keyword(Keyword::Middleware)?;
        let name = self.expect_ident("expected middleware name")?;
        let body = self.parse_block()?;
        // Optional `after { ... }` response-phase block immediately
        // following the main body. Closes the dogfooding gap where
        // request-phase middleware couldn't read the response.
        let after_body = if self.check_ident_eq("after") {
            self.bump()?;
            Some(self.parse_block()?)
        } else {
            None
        };
        Ok(MiddlewareDecl {
            name,
            body,
            after_body,
            namespace: Vec::new(),
            visibility: Visibility::Private,
            offset,
            file_idx: 0,
        })
    }

    pub(super) fn parse_type_spec(&mut self) -> Result<TypeSpec> {
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

    pub(super) fn parse_signed_number(&mut self, msg: &str) -> Result<i64> {
        let sign = if self.check_symbol('-') {
            self.expect_symbol('-')?;
            -1
        } else {
            1
        };
        let number = self.expect_number(msg)?;
        Ok(sign * number)
    }

    pub(super) fn parse_function_decl(&mut self, dome_name: Option<&str>) -> Result<FunctionDecl> {
        let offset = self.current.offset;
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
            offset,
            file_idx: 0,
        })
    }

    /// Parse a single parameter: `name` or `name: TypeRef`
    pub(super) fn parse_typed_param(&mut self) -> Result<TypedParam> {
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
    pub(super) fn parse_type_ref(&mut self) -> Result<String> {
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
}
