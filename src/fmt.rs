//! Source-level formatter for `.jwc` files.
//!
//! Phase 8F: the formatter is a two-tier dispatcher.
//!
//! 1. **AST renderer** ([`format_program`]). When the source parses cleanly
//!    *and* contains no comments, we round-trip through the AST and emit a
//!    canonical, opinionated rendering. This is the "real" formatter — it
//!    asserts a single house style for braces, semicolons, indentation,
//!    spacing, and declaration order.
//! 2. **Line-based normaliser** ([`format_source_line_based`]). When the
//!    source contains comments (which would be lost on the AST round-trip)
//!    or fails to parse, we fall back to a comment-preserving whitespace
//!    normaliser: tabs → 4 spaces, strip trailing whitespace, collapse
//!    runs of 3+ blank lines, single trailing newline.
//!
//! Both tiers are **idempotent**: `format(format(src)) == format(src)`.
//!
//! ## Why the comment gate
//!
//! The AST renderer can't recover lexical comments because the parser drops
//! them. Re-emitting from `Program` would silently delete every `// ...` and
//! `/* ... */` block. We err on the side of "treat as has-comments" — a
//! false positive (e.g. `"//"` inside a string literal) is safe; it just
//! routes through the line-based normaliser. A false negative (real comment
//! we missed) would drop code, so the heuristic only consults coarse
//! delimiter presence.
//!
//! ## What the AST renderer covers
//!
//! Every variant in [`crate::ast::Program`], [`Stmt`](crate::ast::Stmt),
//! and [`Expr`](crate::ast::Expr) — declarations
//! (`dbcontext`, `entity`/`class`, `route`, `function`, `middleware`,
//! `const`, `mount`, `using`), every statement form (control flow, try,
//! transaction, savepoint, validate, DB CRUD), and the full expression
//! ladder (literals, calls, arithmetic, comparison, boolean, `select`,
//! `await`, object/array literals).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::ast::{
    AggregateKind, ConstDecl, DbContextDecl, DbOrderBy, DbWhere, ErrorHandlerDecl, Expr, FieldDecl,
    FieldReference, FunctionDecl, ImportDecl, MiddlewareDecl, ModelDecl, ModelKind, MountDecl,
    NavigationField, NavigationKind, OnDeleteAction, Program, RouteDecl, RouteProtocol, SortDir,
    Stmt, TypeSpec, TypedParam, ValidateField, ValidateRule, Visibility, WhereExpr,
};

const INDENT: &str = "    ";

/// Apply formatting to `src`. Routes through the AST renderer when the
/// source is comment-free and parses cleanly; otherwise falls back to the
/// line-based normaliser so comments are preserved.
pub fn format_source(src: &str) -> String {
    if has_comments(src) {
        // Comments would be lost on the AST round-trip. Fall back to the
        // line-based normaliser which preserves them.
        return format_source_line_based(src);
    }
    match crate::parser::parse_program(src) {
        Ok(program) => format_program(&program),
        Err(_) => format_source_line_based(src),
    }
}

/// Coarse heuristic — quick scan for `//` or `/*` anywhere in `src`. We
/// deliberately err on the side of "treat as has-comments" (false positives
/// inside string literals are safe; they just route through line-based).
pub fn has_comments(src: &str) -> bool {
    src.contains("//") || src.contains("/*")
}

/// Line-based whitespace normaliser — preserves comments.
///
/// Rules applied to every `.jwc` file routed through this tier:
///
/// 1. Tabs are expanded to four spaces (consistent with the rest of the
///    codebase, which uses spaces).
/// 2. Trailing whitespace at end of each line is stripped.
/// 3. Three or more consecutive blank lines collapse to two.
/// 4. The file ends with exactly one trailing newline.
///
/// These four together are *idempotent*.
pub fn format_source_line_based(src: &str) -> String {
    // Pass 1 — normalise per line: tabs → 4 spaces, strip trailing whitespace.
    let normalised: Vec<String> = src
        .split('\n')
        .map(|line| {
            let expanded = line.replace('\t', "    ");
            expanded.trim_end_matches([' ', '\r']).to_string()
        })
        .collect();

    // Pass 2 — collapse runs of 3+ empty lines down to 2. We walk the
    // sequence and track how many empties we've emitted in the current run.
    let mut out: Vec<String> = Vec::with_capacity(normalised.len());
    let mut empty_run = 0usize;
    for line in normalised {
        if line.is_empty() {
            empty_run += 1;
            if empty_run <= 2 {
                out.push(line);
            }
        } else {
            empty_run = 0;
            out.push(line);
        }
    }

    // Pass 3 — exactly one trailing newline. After Pass 2 the source might
    // end with N empties; trim them all off and reattach a single `\n`.
    while out.last().map(|s| s.is_empty()).unwrap_or(false) {
        out.pop();
    }
    let mut result = out.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result
}

/// Return `true` when `src` is already in canonical form.
pub fn is_formatted(src: &str) -> bool {
    format_source(src) == src
}

/// Walk `root` (a file or a directory) and collect every `.jwc` file path
/// reachable from it, breadth-first. Skips the conventional build caches
/// (`.jwc-build/`, `target/`, `node_modules/`) so we don't reformat
/// generated Rust scratch or vendor JS.
pub fn collect_jwc_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::metadata(&path).with_context(|| format!("stat: {}", path.display()))?;
        if meta.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("jwc") {
                out.push(path);
            }
            continue;
        }
        if !meta.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&path).with_context(|| format!("read_dir: {}", path.display()))? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if matches!(name, ".jwc-build" | "target" | "node_modules" | ".git") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("jwc") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Outcome of running the formatter on a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOutcome {
    /// Already canonical — no change needed.
    Unchanged,
    /// Was rewritten (write mode) or *would* be rewritten (check mode).
    Changed,
}

/// Format (or in check mode, diff) a single file. On `check=true` the file
/// is left untouched and the return value tells the caller whether it was
/// canonical. On `check=false` the rewritten content is written back.
pub fn format_file(path: &Path, check: bool) -> Result<FormatOutcome> {
    let src = fs::read_to_string(path).with_context(|| format!("read: {}", path.display()))?;
    let formatted = format_source(&src);
    if formatted == src {
        return Ok(FormatOutcome::Unchanged);
    }
    if !check {
        fs::write(path, &formatted).with_context(|| format!("write: {}", path.display()))?;
    }
    Ok(FormatOutcome::Changed)
}

// ----------------------------------------------------------------------
// AST renderer
// ----------------------------------------------------------------------

/// Render a parsed [`Program`] as canonical `.jwc` source.
///
/// Declaration order:
/// 1. `using` imports (one per declared namespace)
/// 2. `const` bindings
/// 3. `dbcontext` declarations
/// 4. `entity` / `class` models
/// 5. `mount` declarations
/// 6. `middleware` declarations
/// 7. `function` declarations
/// 8. `route` declarations
/// 9. `error` handler (if any)
///
/// Within each group, the source order from the merged [`Program`] is
/// preserved.
pub fn format_program(program: &Program) -> String {
    let mut w = Writer::new();

    let mut printed_block = false;

    // 1. Imports.
    if !program.imports.is_empty() {
        for imp in &program.imports {
            render_import(&mut w, imp);
        }
        printed_block = true;
    }

    // 2. Consts.
    if !program.consts.is_empty() {
        if printed_block {
            w.blank();
        }
        for c in &program.consts {
            render_const(&mut w, c);
        }
        printed_block = true;
    }

    // 3. DbContexts.
    if !program.dbcontexts.is_empty() {
        if printed_block {
            w.blank();
        }
        for d in &program.dbcontexts {
            render_dbcontext(&mut w, d);
        }
        printed_block = true;
    }

    // 4. Models.
    if !program.models.is_empty() {
        if printed_block {
            w.blank();
        }
        for (i, m) in program.models.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            render_model(&mut w, m);
        }
        printed_block = true;
    }

    // 5. Mounts.
    if !program.mounts.is_empty() {
        if printed_block {
            w.blank();
        }
        for m in &program.mounts {
            render_mount(&mut w, m);
        }
        printed_block = true;
    }

    // 6. Middlewares.
    if !program.middlewares.is_empty() {
        if printed_block {
            w.blank();
        }
        for (i, m) in program.middlewares.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            render_middleware(&mut w, m);
        }
        printed_block = true;
    }

    // 7. Functions.
    if !program.functions.is_empty() {
        if printed_block {
            w.blank();
        }
        for (i, f) in program.functions.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            render_function(&mut w, f);
        }
        printed_block = true;
    }

    // 8. Routes.
    if !program.routes.is_empty() {
        if printed_block {
            w.blank();
        }
        for (i, r) in program.routes.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            render_route(&mut w, r);
        }
        printed_block = true;
    }

    // 9. Error handler.
    if let Some(eh) = &program.error_handler {
        if printed_block {
            w.blank();
        }
        render_error_handler(&mut w, eh);
    }

    w.into_string()
}

/// Indent-aware line buffer. Owns the running output, the current indent
/// depth, and a small "did we just emit a blank line?" flag so multiple
/// `blank()` calls collapse to one.
struct Writer {
    out: String,
    indent: usize,
    last_was_blank: bool,
    just_started: bool,
}

impl Writer {
    fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
            last_was_blank: false,
            just_started: true,
        }
    }

    fn push_indent(&mut self) {
        self.indent += 1;
    }

    fn pop_indent(&mut self) {
        debug_assert!(self.indent > 0);
        self.indent -= 1;
    }

    /// Emit `text` as one line, prefixed by the current indentation.
    fn line(&mut self, text: &str) {
        if text.is_empty() {
            self.blank();
            return;
        }
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
        self.out.push_str(text);
        self.out.push('\n');
        self.last_was_blank = false;
        self.just_started = false;
    }

    /// Emit a blank separator line. No-op at file start; collapses runs.
    fn blank(&mut self) {
        if self.just_started || self.last_was_blank {
            return;
        }
        self.out.push('\n');
        self.last_was_blank = true;
    }

    fn into_string(mut self) -> String {
        // Ensure single trailing newline, no leading or trailing blanks.
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out
    }
}

fn render_import(w: &mut Writer, imp: &ImportDecl) {
    w.line(&format!("using {};", imp.path.join(".")));
}

fn render_const(w: &mut Writer, c: &ConstDecl) {
    w.line(&format!("const {} = {};", c.name, render_expr(&c.expr)));
}

fn render_dbcontext(w: &mut Writer, d: &DbContextDecl) {
    let driver = if d.driver.is_empty() {
        String::new()
    } else {
        format!(" {}", d.driver)
    };
    w.line(&format!("dbcontext {}{};", d.name, driver));
}

fn render_mount(w: &mut Writer, m: &MountDecl) {
    let target = m.target.join(".");
    match &m.prefix {
        Some(p) => w.line(&format!("mount {} at \"{}\";", target, escape_string(p))),
        None => w.line(&format!("mount {};", target)),
    }
}

fn render_model(w: &mut Writer, m: &ModelDecl) {
    let vis = vis_prefix(m.visibility);
    let kw = match m.kind {
        ModelKind::Entity => "entity",
        ModelKind::Class => "class",
    };
    let ctx = m
        .context_name
        .as_ref()
        .map(|n| format!(" of {}", n))
        .unwrap_or_default();
    w.line(&format!("{}{} {}{} {{", vis, kw, m.name, ctx));
    w.push_indent();
    for f in &m.fields {
        render_field(w, f);
    }
    for n in &m.navigations {
        render_navigation(w, n);
    }
    w.pop_indent();
    w.line("}");
}

fn render_field(w: &mut Writer, f: &FieldDecl) {
    let mut parts: Vec<String> = Vec::new();
    parts.push(f.name.clone());
    parts.push(render_type(&f.ty));
    if f.is_primary_key {
        parts.push("pk".to_string());
    }
    if f.is_auto_increment {
        parts.push("autoincrement".to_string());
    }
    if f.is_unique {
        parts.push("unique".to_string());
    }
    if f.is_nullable {
        parts.push("nullable".to_string());
    }
    if let Some(r) = &f.references {
        parts.push(render_field_reference(r));
    }
    w.line(&format!("{};", parts.join(" ")));
}

fn render_field_reference(r: &FieldReference) -> String {
    let on_delete = match r.on_delete {
        OnDeleteAction::NoAction => "",
        OnDeleteAction::Cascade => " on delete cascade",
        OnDeleteAction::Restrict => " on delete restrict",
        OnDeleteAction::SetNull => " on delete set null",
    };
    format!("references {}.{}{}", r.entity, r.column, on_delete)
}

fn render_navigation(w: &mut Writer, n: &NavigationField) {
    let ty = match n.kind {
        NavigationKind::OneToMany => format!("List<{}>", n.target_entity),
        NavigationKind::OneToOne => n.target_entity.clone(),
    };
    w.line(&format!(
        "{}: {} via {}.{};",
        n.name, ty, n.target_entity, n.target_field
    ));
}

fn render_type(t: &TypeSpec) -> String {
    if t.args.is_empty() {
        t.name.clone()
    } else {
        let args: Vec<String> = t.args.iter().map(|a| a.to_string()).collect();
        format!("{}({})", t.name, args.join(", "))
    }
}

fn render_middleware(w: &mut Writer, m: &MiddlewareDecl) {
    let vis = vis_prefix(m.visibility);
    w.line(&format!("{}middleware {} {{", vis, m.name));
    w.push_indent();
    render_block(w, &m.body);
    if let Some(after) = &m.after_body {
        w.pop_indent();
        w.line("} after {");
        w.push_indent();
        render_block(w, after);
    }
    w.pop_indent();
    w.line("}");
}

fn render_function(w: &mut Writer, f: &FunctionDecl) {
    let vis = vis_prefix(f.visibility);
    let async_kw = if f.is_async { "async " } else { "" };
    let params: Vec<String> = f.params.iter().map(render_typed_param).collect();
    let ret = f
        .return_type
        .as_ref()
        .map(|t| format!(": {}", t))
        .unwrap_or_default();
    w.line(&format!(
        "{}{}function {}({}){} {{",
        vis,
        async_kw,
        f.name,
        params.join(", "),
        ret
    ));
    w.push_indent();
    render_block(w, &f.body);
    w.pop_indent();
    w.line("}");
}

fn render_typed_param(p: &TypedParam) -> String {
    match &p.ty {
        Some(t) => format!("{}: {}", p.name, t),
        None => p.name.clone(),
    }
}

fn render_route(w: &mut Writer, r: &RouteDecl) {
    let kw = match r.protocol {
        RouteProtocol::Http => format!("route {}", r.method),
        RouteProtocol::Ws => "route WS".to_string(),
        RouteProtocol::Sse => "route SSE".to_string(),
    };
    let middlewares = if r.middlewares.is_empty() {
        String::new()
    } else {
        format!(" use {}", r.middlewares.join(", "))
    };
    let handler = match &r.handler {
        Some(h) => format!(" => {}", h),
        None => String::new(),
    };
    if r.body.is_empty() && r.handler.is_some() {
        w.line(&format!(
            "{} \"{}\"{}{};",
            kw,
            escape_string(&r.path),
            middlewares,
            handler
        ));
        return;
    }
    w.line(&format!(
        "{} \"{}\"{} {{",
        kw,
        escape_string(&r.path),
        middlewares
    ));
    w.push_indent();
    render_block(w, &r.body);
    w.pop_indent();
    w.line("}");
}

fn render_error_handler(w: &mut Writer, eh: &ErrorHandlerDecl) {
    w.line(&format!("error catch ({}) {{", eh.catch_var));
    w.push_indent();
    render_block(w, &eh.body);
    w.pop_indent();
    w.line("}");
}

fn render_block(w: &mut Writer, stmts: &[Stmt]) {
    for s in stmts {
        render_stmt(w, s);
    }
}

fn render_stmt(w: &mut Writer, s: &Stmt) {
    match s {
        Stmt::Let { name, value } => {
            w.line(&format!("let {} = {};", name, render_expr(value)));
        }
        Stmt::Assign { name, value } => {
            w.line(&format!("{} = {};", name, render_expr(value)));
        }
        Stmt::FieldAssign { var, field, value } => {
            w.line(&format!("{}.{} = {};", var, field, render_expr(value)));
        }
        Stmt::Print(e) => {
            w.line(&format!("print({});", render_expr(e)));
        }
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            w.line(&format!("if ({}) {{", render_expr(cond)));
            w.push_indent();
            render_block(w, then_body);
            w.pop_indent();
            if let Some(eb) = else_body {
                w.line("} else {");
                w.push_indent();
                render_block(w, eb);
                w.pop_indent();
            }
            w.line("}");
        }
        Stmt::While { cond, body } => {
            w.line(&format!("while ({}) {{", render_expr(cond)));
            w.push_indent();
            render_block(w, body);
            w.pop_indent();
            w.line("}");
        }
        Stmt::Break => w.line("break;"),
        Stmt::Continue => w.line("continue;"),
        Stmt::Expr(e) => w.line(&format!("{};", render_expr(e))),
        Stmt::Return(opt) => match opt {
            Some(e) => w.line(&format!("return {};", render_expr(e))),
            None => w.line("return;"),
        },
        Stmt::ValidateBody { fields } => {
            w.line("validate body {");
            w.push_indent();
            for f in fields {
                w.line(&render_validate_field(f));
            }
            w.pop_indent();
            w.line("}");
        }
        Stmt::Try {
            body,
            catch_var,
            catch_type,
            catch_body,
        } => {
            w.line("try {");
            w.push_indent();
            render_block(w, body);
            w.pop_indent();
            let head = match catch_type {
                Some(t) => format!("}} catch ({}: {}) {{", catch_var, t),
                None => format!("}} catch ({}) {{", catch_var),
            };
            w.line(&head);
            w.push_indent();
            render_block(w, catch_body);
            w.pop_indent();
            w.line("}");
        }
        Stmt::Transaction { body } => {
            w.line("transaction {");
            w.push_indent();
            render_block(w, body);
            w.pop_indent();
            w.line("}");
        }
        Stmt::Savepoint { name, body } => {
            w.line(&format!("savepoint {} {{", name));
            w.push_indent();
            render_block(w, body);
            w.pop_indent();
            w.line("}");
        }
        Stmt::ForIn { var, iter, body } => {
            w.line(&format!("for {} in {} {{", var, render_expr(iter)));
            w.push_indent();
            render_block(w, body);
            w.pop_indent();
            w.line("}");
        }
        Stmt::DbInsert {
            var,
            context_var,
            table,
        } => {
            w.line(&format!("insert {} into {}.{};", var, context_var, table));
        }
        Stmt::DbUpdate {
            var,
            context_var,
            table,
        } => {
            w.line(&format!("update {} in {}.{};", var, context_var, table));
        }
        Stmt::DbDelete {
            var,
            context_var,
            table,
        } => {
            w.line(&format!("delete {} from {}.{};", var, context_var, table));
        }
        Stmt::DbDeleteWhere {
            context_var,
            table,
            where_clause,
        } => {
            w.line(&format!(
                "delete from {}.{} where {};",
                context_var,
                table,
                render_where(where_clause)
            ));
        }
        Stmt::DbUpdateSet {
            context_var,
            table,
            assignments,
            where_clause,
        } => {
            let sets: Vec<String> = assignments
                .iter()
                .map(|(k, v)| format!("{} = {}", k, render_expr(v)))
                .collect();
            w.line(&format!(
                "update {}.{} set {} where {};",
                context_var,
                table,
                sets.join(", "),
                render_where(where_clause)
            ));
        }
    }
}

fn render_validate_field(f: &ValidateField) -> String {
    let rules: Vec<String> = f.rules.iter().map(render_validate_rule).collect();
    format!("{}: {};", f.name, rules.join(", "))
}

fn render_validate_rule(r: &ValidateRule) -> String {
    match r {
        ValidateRule::Required => "required".to_string(),
        ValidateRule::MinLength(n) => format!("min_length({})", n),
        ValidateRule::MaxLength(n) => format!("max_length({})", n),
        ValidateRule::Min(n) => format!("min({})", n),
        ValidateRule::Max(n) => format!("max({})", n),
        ValidateRule::Pattern(p) => format!("pattern(\"{}\")", escape_string(p)),
    }
}

fn render_where(w: &WhereExpr) -> String {
    match w {
        WhereExpr::Atom(a) => render_db_where(a),
        WhereExpr::InList { field, values } => {
            let vs: Vec<String> = values.iter().map(render_expr).collect();
            format!("{} in ({})", field, vs.join(", "))
        }
        WhereExpr::Between { field, low, high } => {
            format!(
                "{} between {} and {}",
                field,
                render_expr(low),
                render_expr(high)
            )
        }
        WhereExpr::And(a, b) => format!("({} and {})", render_where(a), render_where(b)),
        WhereExpr::Or(a, b) => format!("({} or {})", render_where(a), render_where(b)),
    }
}

fn render_db_where(a: &DbWhere) -> String {
    format!("{} {} {}", a.field, a.op, render_expr(&a.rhs))
}

fn render_order_by(o: &DbOrderBy) -> String {
    let dir = match o.dir {
        SortDir::Asc => "asc",
        SortDir::Desc => "desc",
    };
    format!("{} {}", o.field, dir)
}

fn render_expr(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Float(s) => s.clone(),
        Expr::Str(s) => format!("\"{}\"", escape_string(s)),
        Expr::Bool(b) => b.to_string(),
        Expr::Null => "null".to_string(),
        Expr::Var(name) => name.clone(),
        Expr::Call { name, args } => {
            let parts: Vec<String> = args.iter().map(render_expr).collect();
            format!("{}({})", name, parts.join(", "))
        }
        Expr::FieldGet { var, field } => format!("{}.{}", var, field),
        Expr::NewEntity { entity } => format!("new {}()", entity),
        Expr::DbCount {
            context_var,
            table,
            where_clause,
        } => {
            let mut s = format!("select count(*) from {}.{}", context_var, table);
            if let Some(wc) = where_clause {
                s.push_str(" where ");
                s.push_str(&render_where(wc));
            }
            s
        }
        Expr::DbAggregate {
            kind,
            field,
            context_var,
            table,
            where_clause,
        } => {
            let kw = match kind {
                AggregateKind::Sum => "sum",
                AggregateKind::Avg => "avg",
                AggregateKind::Min => "min",
                AggregateKind::Max => "max",
            };
            let mut s = format!("select {}({}) from {}.{}", kw, field, context_var, table);
            if let Some(wc) = where_clause {
                s.push_str(" where ");
                s.push_str(&render_where(wc));
            }
            s
        }
        Expr::DbSelect {
            entity,
            context_var,
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
        } => {
            let mut s = String::from("select ");
            s.push_str(entity);
            if !projection.is_empty() {
                s.push_str(" { ");
                s.push_str(&projection.join(", "));
                s.push_str(" }");
            }
            if !with_relations.is_empty() {
                s.push_str(" with ");
                s.push_str(&with_relations.join(", "));
            }
            s.push_str(&format!(" from {}.{}", context_var, table));
            if let Some(wc) = where_clause {
                s.push_str(" where ");
                s.push_str(&render_where(wc));
            }
            if !group_by.is_empty() {
                s.push_str(" group by ");
                s.push_str(&group_by.join(", "));
            }
            if let Some(h) = having {
                s.push_str(" having ");
                s.push_str(&render_where(h));
            }
            if let Some(o) = order_by {
                s.push_str(" orderby ");
                s.push_str(&render_order_by(o));
            }
            if let Some(l) = limit {
                s.push_str(&format!(" limit {}", render_expr(l)));
            }
            if let Some(o) = offset {
                s.push_str(&format!(" offset {}", render_expr(o)));
            }
            if *first {
                s.push_str(" first");
            }
            s
        }
        Expr::Await(inner) => format!("await {}", render_expr(inner)),
        Expr::Not(inner) => format!("!{}", paren_expr(inner)),
        Expr::ObjectLit(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}: {}", k, render_expr(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Expr::ArrayLit(items) => {
            let parts: Vec<String> = items.iter().map(render_expr).collect();
            format!("[{}]", parts.join(", "))
        }
        Expr::Add(a, b) => bin(a, "+", b),
        Expr::Sub(a, b) => bin(a, "-", b),
        Expr::Mul(a, b) => bin(a, "*", b),
        Expr::Div(a, b) => bin(a, "/", b),
        Expr::Mod(a, b) => bin(a, "%", b),
        Expr::Neg(inner) => format!("-{}", paren_expr(inner)),
        Expr::Eq(a, b) => bin(a, "==", b),
        Expr::Neq(a, b) => bin(a, "!=", b),
        Expr::Lt(a, b) => bin(a, "<", b),
        Expr::Lte(a, b) => bin(a, "<=", b),
        Expr::Gt(a, b) => bin(a, ">", b),
        Expr::Gte(a, b) => bin(a, ">=", b),
        Expr::And(a, b) => bin(a, "and", b),
        Expr::Or(a, b) => bin(a, "or", b),
    }
}

fn bin(a: &Expr, op: &str, b: &Expr) -> String {
    format!("{} {} {}", paren_expr(a), op, paren_expr(b))
}

/// Wrap composite expressions in parentheses so reparsing yields the same
/// shape. Atoms and call-like forms pass through bare.
fn paren_expr(e: &Expr) -> String {
    match e {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_)
        | Expr::Call { .. }
        | Expr::FieldGet { .. }
        | Expr::NewEntity { .. }
        | Expr::ObjectLit(_)
        | Expr::ArrayLit(_) => render_expr(e),
        _ => format!("({})", render_expr(e)),
    }
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn vis_prefix(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "pub ",
        Visibility::Private => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_whitespace_line_based() {
        // Has `//` so routes through line-based.
        let src = "// hdr\nfunction foo() {  \n    return 1;   \n}\n";
        let want = "// hdr\nfunction foo() {\n    return 1;\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn expands_tabs_to_four_spaces_line_based() {
        let src = "// hdr\nfunction foo() {\n\treturn 1;\n}\n";
        let want = "// hdr\nfunction foo() {\n    return 1;\n}\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn collapses_triple_blank_lines_line_based() {
        let src = "// a\n\n\n\nb\n";
        let want = "// a\n\n\nb\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn enforces_single_trailing_newline_line_based() {
        assert_eq!(format_source("// a\n\n\n"), "// a\n");
        assert_eq!(format_source("// a"), "// a\n");
        assert_eq!(format_source(""), "");
    }

    #[test]
    fn line_based_is_idempotent_on_commented_sources() {
        let inputs = [
            "// header\nfunction foo() {\n  return 1;\n}\n",
            "// a\n\nb\n",
            "// a\t\tb\n",
            "// trailing  \n",
            "// \n\n\n\nonly blanks above\n",
        ];
        for input in inputs {
            let once = format_source(input);
            let twice = format_source(&once);
            assert_eq!(once, twice, "format() must be idempotent on: {input:?}");
        }
    }

    #[test]
    fn is_formatted_recognises_canonical_input() {
        // Comment-free: routes through the AST tier when it parses.
        let canonical = "function foo() {\n    return 1;\n}\n";
        assert!(is_formatted(canonical));
    }

    #[test]
    fn handles_crlf_line_endings_line_based() {
        let src = "// hdr\na\r\nb\r\n";
        let want = "// hdr\na\nb\n";
        assert_eq!(format_source(src), want);
    }

    #[test]
    fn has_comments_detects_line_and_block() {
        assert!(has_comments("// hi\n"));
        assert!(has_comments("/* hi */\n"));
        assert!(!has_comments("function foo() {}\n"));
    }

    #[test]
    fn ast_tier_renders_simple_function() {
        let src = "function main() {\n    return 1;\n}\n";
        let out = format_source(src);
        assert!(out.contains("function main()"));
        assert!(out.contains("return 1;"));
    }

    #[test]
    fn ast_tier_idempotent_on_simple_program() {
        let src = "function main() {\n    print(2 + 3);\n}\n";
        let once = format_source(src);
        let twice = format_source(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn unparseable_input_routes_to_line_based() {
        // `@@@` is not valid jwc — must fall back, not panic.
        let src = "@@@ broken !!!\n";
        let out = format_source(src);
        assert_eq!(out, "@@@ broken !!!\n");
    }
}
