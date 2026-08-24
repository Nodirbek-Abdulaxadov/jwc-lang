//! Canonical printer for the v1 AST.
//!
//! `jwc v1 fmt` re-prints from the AST rather than editing tokens, so the
//! output is a fixed point by construction: anything the printer would
//! normalise is already normalised on the second pass. The test that matters
//! is `fmt(fmt(x)) == fmt(x)` over the corpus and the sample.
//!
//! Doc comments (`---`) and line comments (`--`) survive: the parser hangs
//! them on the AST node that follows them (`Attached`), and the printer
//! emits them back in place. Trailing comments on the same line as code are
//! not preserved — they become leading comments of the next item.

use crate::ast::*;

const INDENT: &str = "    ";

pub fn format_program(p: &Program) -> String {
    let mut w = Writer::default();
    for (i, d) in p.decls.iter().enumerate() {
        if i > 0 {
            let solo = matches!(d, Decl::Import(_) | Decl::Namespace(_))
                && matches!(p.decls[i - 1], Decl::Import(_) | Decl::Namespace(_));
            // Consecutive imports stay together; everything else gets air.
            if solo && !d.attached().blank_before {
                // no blank line
            } else {
                w.blank();
            }
        }
        w.decl(d);
    }
    let mut out = w.out;
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[derive(Default)]
struct Writer {
    out: String,
    depth: usize,
}

impl Writer {
    fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn attached(&mut self, at: &Attached) {
        for c in &at.comments {
            let s = if c.is_empty() {
                "--".to_string()
            } else {
                format!("-- {c}")
            };
            self.line(&s);
        }
        for d in &at.docs {
            let s = if d.is_empty() {
                "---".to_string()
            } else {
                format!("--- {d}")
            };
            self.line(&s);
        }
    }

    // ------------------------------------------------------------ decls

    fn decl(&mut self, d: &Decl) {
        self.attached(d.attached());
        match d {
            Decl::Namespace(n) => self.line(&format!("namespace {};", n.name.text())),
            Decl::Import(n) => self.line(&format!("import {};", n.name.text())),
            Decl::Database(n) => self.database(n),
            Decl::Schema(n) => {
                let phys = n
                    .physical
                    .as_ref()
                    .map(|p| format!(" as {}", quote(p)))
                    .unwrap_or_default();
                self.line(&format!(
                    "schema {} of {}{phys};",
                    n.name.name, n.database.name
                ));
            }
            Decl::Table(n) => self.table(n),
            Decl::View(n) => self.view(n),
            Decl::Enum(n) => self.enum_decl(n),
            Decl::Class(n) => self.class(n),
            Decl::Error(n) => self.error_decl(n),
            Decl::Service(n) => self.service(n),
            Decl::Middleware(n) => self.middleware(n),
            Decl::Job(n) => self.job(n),
            Decl::Routes(n) => self.routes(n),
            Decl::ErrorHandler(n) => self.error_handler(n),
            Decl::Server(n) => self.server(n),
            Decl::Function(n) => self.function(n),
            Decl::Test(n) => {
                self.line(&format!("test {} {{", quote(&n.name)));
                self.depth += 1;
                self.block(&n.body);
                self.depth -= 1;
                self.line("}");
            }
        }
    }

    fn database(&mut self, n: &DatabaseDecl) {
        if n.init.is_empty() {
            self.line(&format!("database {} : {};", n.name.name, n.driver.name));
            return;
        }
        self.line(&format!("database {} : {} {{", n.name.name, n.driver.name));
        self.depth += 1;
        self.line("init() {");
        self.depth += 1;
        let pad = n.init.iter().map(|a| a.key.name.len()).max().unwrap_or(0);
        for a in &n.init {
            self.line(&format!(
                "{:pad$} = {};",
                a.key.name,
                expr(&a.value),
                pad = pad
            ));
        }
        self.depth -= 1;
        self.line("}");
        self.depth -= 1;
        self.line("}");
    }

    fn table(&mut self, n: &TableDecl) {
        let phys = n
            .physical
            .as_ref()
            .map(|p| format!(" as {}", quote(p)))
            .unwrap_or_default();
        let was = n
            .was
            .as_ref()
            .map(|p| format!(" was {}", quote(p)))
            .unwrap_or_default();
        self.line(&format!(
            "table {} of {}.{}{phys}{was} {{",
            n.name.name, n.schema.database.name, n.schema.schema.name
        ));
        self.depth += 1;

        // Column names are padded to a common width inside one table. This
        // is deterministic (it depends only on the declarations), so the
        // output stays a fixed point.
        let pad = n
            .columns
            .iter()
            .map(|c| c.name.name.len())
            .max()
            .unwrap_or(0);
        for c in &n.columns {
            self.attached(&c.at);
            let mut s = format!("{:pad$} {}", c.name.name, type_ref(&c.ty), pad = pad);
            let mut prev_attr = false;
            for m in &c.modifiers {
                let attr = is_attribute(m);
                s.push_str(if attr && prev_attr { ", " } else { " " });
                s.push_str(&modifier(m));
                prev_attr = attr;
            }
            s.push(';');
            self.line(&s);
        }

        if !n.constraints.is_empty() {
            self.blank();
        }
        for c in &n.constraints {
            self.attached(c.attached());
            self.constraint(c);
        }
        if !n.indexes.is_empty() {
            self.blank();
        }
        for ix in &n.indexes {
            self.attached(&ix.at);
            let cols = ix
                .columns
                .iter()
                .map(index_column)
                .collect::<Vec<_>>()
                .join(", ");
            let pred = ix
                .predicate
                .as_ref()
                .map(|p| format!(" where {}", expr(p)))
                .unwrap_or_default();
            let using = ix
                .method
                .as_ref()
                .map(|m| format!(" using {}", m.name))
                .unwrap_or_default();
            self.line(&format!("index on ({cols}){pred}{using};"));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn constraint(&mut self, c: &TableConstraint) {
        match c {
            TableConstraint::PrimaryKey { columns, .. } => {
                self.line(&format!("primary key ({});", names(columns)));
            }
            TableConstraint::ForeignKey {
                columns,
                target,
                target_columns,
                on_delete,
                on_update,
                ..
            } => {
                let mut s = format!(
                    "foreign key ({}) references {} ({})",
                    names(columns),
                    target.text(),
                    names(target_columns)
                );
                if let Some(a) = on_delete {
                    s.push_str(&format!(" on delete {}", action_text(*a)));
                }
                if let Some(a) = on_update {
                    s.push_str(&format!(" on update {}", action_text(*a)));
                }
                s.push(';');
                self.line(&s);
            }
            TableConstraint::Unique {
                columns,
                predicate,
                message,
                ..
            } => {
                let mut s = format!("unique ({})", names(columns));
                if let Some(p) = predicate {
                    s.push_str(&format!(" where {}", expr(p)));
                }
                if let Some(m) = message {
                    s.push_str(&format!(" : {}", quote(m)));
                }
                s.push(';');
                self.line(&s);
            }
            TableConstraint::Check {
                expr: e, message, ..
            } => {
                let mut s = format!("check ({})", expr(e));
                if let Some(m) = message {
                    s.push_str(&format!(" : {}", quote(m)));
                }
                s.push(';');
                self.line(&s);
            }
        }
    }

    fn view(&mut self, n: &ViewDecl) {
        let phys = n
            .physical
            .as_ref()
            .map(|p| format!(" as {}", quote(p)))
            .unwrap_or_default();
        self.line(&format!(
            "view {} of {}.{}{phys} {{",
            n.name.name, n.schema.database.name, n.schema.schema.name
        ));
        self.depth += 1;
        self.select(&n.body, false);
        self.depth -= 1;
        self.line("}");
    }

    fn enum_decl(&mut self, n: &EnumDecl) {
        let of = n
            .schema
            .as_ref()
            .map(|s| format!(" of {}.{}", s.database.name, s.schema.name))
            .unwrap_or_default();
        let phys = n
            .physical
            .as_ref()
            .map(|p| format!(" as {}", quote(p)))
            .unwrap_or_default();
        self.line(&format!(
            "enum {}{of}{phys} {{ {} }}",
            n.name.name,
            names(&n.members)
        ));
    }

    fn class(&mut self, n: &ClassDecl) {
        self.line(&format!("class {} {{", n.name.name));
        self.depth += 1;
        let pad = n
            .fields
            .iter()
            .map(|f| f.name.name.len())
            .max()
            .unwrap_or(0);
        for f in &n.fields {
            self.attached(&f.at);
            let mut s = format!("{:pad$} {}", f.name.name, type_ref(&f.ty), pad = pad);
            let mut parts: Vec<String> = Vec::new();
            if f.transient {
                parts.push("transient".into());
            }
            for r in &f.rules {
                parts.push(rule_call(r));
            }
            if !parts.is_empty() {
                s.push(' ');
                s.push_str(&parts.join(", "));
            }
            s.push(';');
            self.line(&s);
        }
        self.depth -= 1;
        self.line("}");
    }

    fn error_decl(&mut self, n: &ErrorDecl) {
        let params = if n.params.is_empty() {
            String::new()
        } else {
            format!("({})", params_text(&n.params))
        };
        let msg = n
            .message
            .as_ref()
            .map(|m| format!(" : {}", quote(m)))
            .unwrap_or_default();
        self.line(&format!(
            "error {}{params} = {}{msg};",
            n.name.name, n.status
        ));
    }

    fn service(&mut self, n: &ServiceDecl) {
        self.line(&format!("service {} {{", n.name.name));
        self.depth += 1;
        for (i, f) in n.functions.iter().enumerate() {
            if i > 0 {
                self.blank();
            }
            self.attached(&f.at);
            self.function(f);
        }
        self.depth -= 1;
        self.line("}");
    }

    /// Does **not** print `n.at` — the caller does, because a function is
    /// reached both as a top-level declaration (via `decl`) and as a service
    /// member (via `service`).
    fn function(&mut self, n: &FunctionDecl) {
        let ret = n
            .returns
            .as_ref()
            .map(|t| format!(" -> {}", type_ref(t)))
            .unwrap_or_default();
        let raises = if n.raises.is_empty() {
            String::new()
        } else {
            format!(" raises ({})", names(&n.raises))
        };
        self.line(&format!(
            "function {}({}){ret}{raises} {{",
            n.name.name,
            params_text(&n.params)
        ));
        self.depth += 1;
        self.block(&n.body);
        self.depth -= 1;
        self.line("}");
    }

    fn middleware(&mut self, n: &MiddlewareDecl) {
        let mut head = format!("middleware {}", n.name.name);
        if !n.binders.is_empty() {
            let bs = n
                .binders
                .iter()
                .map(|b| format!("@{}: {}", b.name.name, type_ref(&b.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            head.push_str(&format!("({bs})"));
        }
        let requires = if n.requires.is_empty() {
            String::new()
        } else {
            format!("requires {}", names(&n.requires))
        };
        let provides = if n.provides.is_empty() {
            String::new()
        } else {
            let ps = n
                .provides
                .iter()
                .map(|p| format!("{}: {}", p.name.name, type_ref(&p.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("provides {ps}")
        };

        // One line while it fits; otherwise `requires` and `provides` get
        // their own continuation lines, which is how the declaration reads
        // in middleware.md §1.
        let inline = [head.as_str(), requires.as_str(), provides.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if inline.len() + 2 <= 88 {
            self.line(&format!("{inline} {{"));
        } else {
            self.line(&head);
            self.depth += 1;
            if !requires.is_empty() {
                self.line(&requires);
            }
            if !provides.is_empty() {
                self.line(&provides);
            }
            self.depth -= 1;
            self.line("{");
        }
        self.depth += 1;
        self.block(&n.body);
        if let Some(after) = &n.after {
            if !n.body.is_empty() {
                self.blank();
            }
            self.line("after {");
            self.depth += 1;
            self.block(after);
            self.depth -= 1;
            self.line("}");
        }
        self.depth -= 1;
        self.line("}");
    }

    fn job(&mut self, n: &JobDecl) {
        let mut head = format!("job {}({})", n.name.name, params_text(&n.params));
        if let Some(r) = n.retries {
            head.push_str(&format!(" retries {r}"));
        }
        if let Some(b) = &n.backoff {
            head.push_str(&format!(" backoff {}", quote(b)));
        }
        self.line(&format!("{head} {{"));
        self.depth += 1;
        self.block(&n.body);
        self.depth -= 1;
        self.line("}");
    }

    fn routes(&mut self, n: &RoutesDecl) {
        let uses = if n.uses.is_empty() {
            String::new()
        } else {
            format!(" use {}", names(&n.uses))
        };
        self.line(&format!("routes {}{uses} {{", quote(&n.prefix)));
        self.depth += 1;
        for (i, r) in n.routes.iter().enumerate() {
            if i > 0 {
                self.blank();
            }
            self.attached(&r.at);
            let ruses = if r.uses.is_empty() {
                String::new()
            } else {
                format!(" use {}", names(&r.uses))
            };
            self.line(&format!(
                "route {} {}{ruses} {{",
                r.method.name,
                quote(&r.suffix)
            ));
            self.depth += 1;
            self.block(&r.body);
            self.depth -= 1;
            self.line("}");
        }
        for (i, sk) in n.sockets.iter().enumerate() {
            if i > 0 || !n.routes.is_empty() {
                self.blank();
            }
            self.attached(&sk.at);
            let suses = if sk.uses.is_empty() {
                String::new()
            } else {
                format!(" use {}", names(&sk.uses))
            };
            self.line(&format!("socket {}{suses} {{", quote(&sk.suffix)));
            self.depth += 1;
            let mut first = true;
            if let Some(b) = &sk.on_open {
                self.line("on open {");
                self.depth += 1;
                self.block(b);
                self.depth -= 1;
                self.line("}");
                first = false;
            }
            if let Some((binder, b)) = &sk.on_message {
                if !first {
                    self.blank();
                }
                self.line(&format!("on message ({}) {{", binder.name));
                self.depth += 1;
                self.block(b);
                self.depth -= 1;
                self.line("}");
                first = false;
            }
            if let Some(b) = &sk.on_close {
                if !first {
                    self.blank();
                }
                self.line("on close {");
                self.depth += 1;
                self.block(b);
                self.depth -= 1;
                self.line("}");
            }
            self.depth -= 1;
            self.line("}");
        }
        self.depth -= 1;
        self.line("}");
    }

    fn error_handler(&mut self, n: &ErrorHandlerDecl) {
        self.line(&format!("errorHandler ({}) {{", n.binder.name));
        self.depth += 1;
        for a in &n.arms {
            let ty = a
                .error
                .as_ref()
                .map(|e| format!("{} ", e.name))
                .unwrap_or_default();
            self.line(&format!("catch {ty}({}) {{", a.binder.name));
            self.depth += 1;
            self.block(&a.body);
            self.depth -= 1;
            self.line("}");
        }
        self.depth -= 1;
        self.line("}");
    }

    fn server(&mut self, n: &ServerDecl) {
        self.line("server {");
        self.depth += 1;
        let pad = n
            .entries
            .iter()
            .filter_map(|e| match e {
                ServerEntry::Set(a) => Some(a.key.name.len()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        for e in &n.entries {
            match e {
                ServerEntry::Set(a) => self.line(&format!(
                    "{:pad$} = {};",
                    a.key.name,
                    expr(&a.value),
                    pad = pad
                )),
                ServerEntry::Group { name, entries, .. } => {
                    self.blank();
                    self.line(&format!("{} {{", name.name));
                    self.depth += 1;
                    let ipad = entries.iter().map(|a| a.key.name.len()).max().unwrap_or(0);
                    for a in entries {
                        self.line(&format!(
                            "{:ipad$} = {};",
                            a.key.name,
                            expr(&a.value),
                            ipad = ipad
                        ));
                    }
                    self.depth -= 1;
                    self.line("}");
                }
            }
        }
        self.depth -= 1;
        self.line("}");
    }

    // ------------------------------------------------------------ stmts

    fn block(&mut self, b: &Block) {
        for (i, s) in b.iter().enumerate() {
            if i > 0 && stmt_attached(s).blank_before {
                self.blank();
            }
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        self.attached(stmt_attached(s));
        match s {
            Stmt::Break { .. } => self.line("break;"),
            Stmt::Continue { .. } => self.line("continue;"),
            Stmt::Let {
                name, ty, value, ..
            } => {
                let t = ty
                    .as_ref()
                    .map(|t| format!(": {}", type_ref(t)))
                    .unwrap_or_default();
                self.assigned(&format!("let {}{t} = ", name.name), value, ";");
            }
            Stmt::Assign { target, value, .. } => {
                let t = match target {
                    AssignTarget::Local(i) => format!("${}", i.name),
                    AssignTarget::Context(i) => format!("context.{}", i.name),
                };
                self.assigned(&format!("{t} = "), value, ";");
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.line(&format!("if ({}) {{", expr(cond)));
                self.depth += 1;
                self.block(then);
                self.depth -= 1;
                match otherwise {
                    None => self.line("}"),
                    Some(alt) => {
                        // `else if` is printed as a nested block. It round-trips
                        // (an `if` inside an `else` block parses back the same
                        // way), which is what idempotency needs.
                        self.line("} else {");
                        self.depth += 1;
                        self.block(alt);
                        self.depth -= 1;
                        self.line("}");
                    }
                }
            }
            Stmt::For {
                binder,
                iterable,
                body,
                ..
            } => {
                self.line(&format!("for ({} in {}) {{", binder.name, expr(iterable)));
                self.depth += 1;
                self.block(body);
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Return { value, .. } => match value {
                None => self.line("return;"),
                Some(v) => self.assigned("return ", v, ";"),
            },
            Stmt::Throw { error, args, .. } => {
                let a = args.iter().map(expr).collect::<Vec<_>>().join(", ");
                if args.is_empty() {
                    self.line(&format!("throw {};", error.name));
                } else {
                    self.line(&format!("throw {}({a});", error.name));
                }
            }
            Stmt::Dispatch { job, args, .. } => {
                let list = args
                    .iter()
                    .map(|(n, v)| format!("{}: {}", n.name, expr(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.line(&format!("dispatch {}({list});", job.name));
            }
            Stmt::Transaction { body, .. } => {
                self.line("transaction {");
                self.depth += 1;
                self.block(body);
                self.depth -= 1;
                self.line("}");
            }
            Stmt::Assert { kind, .. } => match kind {
                AssertKind::Expr(e) => self.line(&format!("assert {};", expr(e))),
                AssertKind::Fails {
                    error,
                    body,
                    message,
                    ..
                } => {
                    let t = error
                        .as_ref()
                        .map(|e| format!("{} ", e.name))
                        .unwrap_or_default();
                    self.line(&format!("assert fails {t}{{"));
                    self.depth += 1;
                    self.block(body);
                    self.depth -= 1;
                    match message {
                        Some(m) => self.line(&format!("}} with {};", quote(m))),
                        None => self.line("};"),
                    }
                }
            },
            Stmt::Expr { expr: e, .. } => self.assigned("", e, ";"),
        }
    }

    /// Prints `prefix<expr>suffix`, breaking a query across lines.
    ///
    /// Queries are the only multi-line expression form. `or throw` and a
    /// postfix `catch` are peeled off first and become part of the suffix,
    /// so `let x = select … first or throw NotFound("…");` breaks at its
    /// clauses instead of running to 200 columns.
    fn assigned(&mut self, prefix: &str, e: &Expr, suffix: &str) {
        match &*e.kind {
            ExprKind::OrThrow { value, error, args } => {
                let a = args.iter().map(expr).collect::<Vec<_>>().join(", ");
                let tail = if args.is_empty() {
                    format!(" or throw {}{suffix}", error.name)
                } else {
                    format!(" or throw {}({a}){suffix}", error.name)
                };
                self.assigned(prefix, value, &tail);
            }
            ExprKind::CatchPostfix {
                value,
                error,
                binder,
                body,
            } => {
                self.assigned(
                    prefix,
                    value,
                    &format!(" catch {} ({}) {{", error.name, binder.name),
                );
                self.depth += 1;
                self.block(body);
                self.depth -= 1;
                self.line(&format!("}}{suffix}"));
            }
            ExprKind::Select(q) => {
                self.line(&format!(
                    "{prefix}select {} from {}",
                    q.binder.name,
                    q.source.text()
                ));
                self.depth += 1;
                self.select_tail(q, suffix);
                self.depth -= 1;
            }
            ExprKind::Insert(i) => self.insert_stmt(prefix, i, suffix),
            ExprKind::Update(u) => self.update_stmt(prefix, u, suffix),
            ExprKind::Delete(d) => self.delete_stmt(prefix, d, suffix),
            _ => {
                let body = expr(e);
                let one_line = format!("{prefix}{body}{suffix}");
                // `or throw` on a non-query value: break at the boundary
                // rather than run past the margin.
                if one_line.len() + self.depth * INDENT.len() > 92 {
                    if let Some(cut) = suffix.find(" or throw ") {
                        self.line(&format!("{prefix}{body}"));
                        self.depth += 1;
                        self.line(suffix[cut + 1..].trim_end_matches('\n'));
                        self.depth -= 1;
                        return;
                    }
                }
                self.line(&one_line);
            }
        }
    }

    fn insert_stmt(&mut self, prefix: &str, i: &InsertExpr, suffix: &str) {
        let inline = obj_entries_text(&i.values);
        let head = format!("{prefix}insert into {}", i.table.text());
        let mut tail: Vec<String> = Vec::new();
        if let Some(c) = &i.conflict {
            let cols = if c.columns.is_empty() {
                String::new()
            } else {
                format!(" ({})", names(&c.columns))
            };
            tail.push(match &c.action {
                ConflictAction::DoNothing => format!("on conflict{cols} do nothing"),
                ConflictAction::DoUpdate(sets) => {
                    format!("on conflict{cols} do update set {}", set_items_text(sets))
                }
            });
        }

        if inline.len() + head.len() <= 76 && tail.is_empty() && i.projection.is_none() {
            self.line(&format!("{head} {{ {inline} }}{suffix}"));
            return;
        }

        self.line(&format!("{head} {{"));
        self.depth += 1;
        let pad = i
            .values
            .iter()
            .filter_map(|e| match e {
                ObjEntry::Field {
                    key, assign: true, ..
                } => Some(key.name.len()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        for (n, entry) in i.values.iter().enumerate() {
            let comma = if n + 1 < i.values.len() { "," } else { "" };
            self.line(&format!("{}{comma}", obj_entry_text_padded(entry, pad)));
        }
        self.depth -= 1;

        // The RETURNING list rides on the closing brace when it fits, which
        // is how the specification's own sample writes it:
        //     } as { id, email, display_name, created_at };
        if tail.is_empty() {
            match &i.projection {
                None => self.line(&format!("}}{suffix}")),
                Some(p) => {
                    let one_line = format!("}} as {}{suffix}", shape_text(p));
                    if one_line.len() + self.depth * INDENT.len() <= 88 {
                        self.line(&one_line);
                    } else {
                        self.line("}");
                        self.depth += 1;
                        self.shape_with_suffix("as ", p, suffix);
                        self.depth -= 1;
                    }
                }
            }
            return;
        }

        self.line("}");
        self.depth += 1;
        for (n, t) in tail.iter().enumerate() {
            let last = n + 1 == tail.len() && i.projection.is_none();
            self.line(&format!("{t}{}", if last { suffix } else { "" }));
        }
        if let Some(p) = &i.projection {
            self.shape_with_suffix("as ", p, suffix);
        }
        self.depth -= 1;
    }

    fn update_stmt(&mut self, prefix: &str, u: &UpdateExpr, suffix: &str) {
        self.line(&format!("{prefix}update {}", u.table.text()));
        self.depth += 1;
        let sets = set_items_text(&u.sets);
        if sets.len() <= 72 {
            self.line(&format!("set {sets}"));
        } else {
            self.line("set");
            self.depth += 1;
            for (n, it) in u.sets.iter().enumerate() {
                let comma = if n + 1 < u.sets.len() { "," } else { "" };
                self.line(&format!("{}{comma}", set_item_text(it)));
            }
            self.depth -= 1;
        }
        self.write_filter_projection_tail(
            u.filter.as_ref(),
            u.projection.as_ref(),
            &u.order_by,
            u.first,
            suffix,
        );
        self.depth -= 1;
    }

    fn delete_stmt(&mut self, prefix: &str, d: &DeleteExpr, suffix: &str) {
        self.line(&format!("{prefix}delete from {}", d.table.text()));
        self.depth += 1;
        self.write_filter_projection_tail(
            d.filter.as_ref(),
            d.projection.as_ref(),
            &d.order_by,
            d.first,
            suffix,
        );
        self.depth -= 1;
    }

    fn write_filter_projection_tail(
        &mut self,
        filter: Option<&Expr>,
        projection: Option<&ObjectShape>,
        order_by: &[SortKey],
        first: bool,
        suffix: &str,
    ) {
        let mut trailing: Vec<String> = Vec::new();
        if !order_by.is_empty() {
            trailing.push(format!("orderby {}", sort_keys(order_by)));
        }
        if first {
            trailing.push("first".into());
        }

        if let Some(f) = filter {
            let last = projection.is_none() && trailing.is_empty();
            self.line(&format!(
                "where {}{}",
                expr(f),
                if last { suffix } else { "" }
            ));
        }
        if let Some(p) = projection {
            let last = trailing.is_empty();
            self.shape_with_suffix("as ", p, if last { suffix } else { "" });
        }
        for (n, t) in trailing.iter().enumerate() {
            let last = n + 1 == trailing.len();
            self.line(&format!("{t}{}", if last { suffix } else { "" }));
        }
    }

    fn shape_with_suffix(&mut self, prefix: &str, shape: &ObjectShape, suffix: &str) {
        self.shape(prefix, shape);
        if !suffix.is_empty() {
            let trimmed = self.out.trim_end_matches('\n').to_string();
            self.out = format!("{trimmed}{suffix}\n");
        }
    }

    fn select(&mut self, s: &SelectExpr, _nested: bool) {
        self.line(&format!(
            "select {} from {}",
            s.binder.name,
            s.source.text()
        ));
        self.depth += 1;
        self.select_tail(s, "");
        self.depth -= 1;
    }

    fn select_tail(&mut self, s: &SelectExpr, suffix: &str) {
        let mut lines: Vec<String> = Vec::new();
        for j in &s.joins {
            lines.push(join_text(j));
        }
        if let Some(f) = &s.filter {
            lines.push(format!("where {}", expr(f)));
        }
        if !s.group_by.is_empty() {
            lines.push(format!(
                "group by {}",
                s.group_by.iter().map(expr).collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some(h) = &s.having {
            lines.push(format!("having {}", expr(h)));
        }

        let mut tail: Vec<String> = Vec::new();
        if !s.order_by.is_empty() {
            tail.push(format!("orderby {}", sort_keys(&s.order_by)));
        }
        if let Some(p) = &s.page {
            tail.push(page_text(p));
        } else if let Some(l) = &s.limit {
            tail.push(format!("limit {}", expr(l)));
        }
        if s.first {
            tail.push("first".into());
        }

        let n_lines = lines.len();
        for (i, l) in lines.iter().enumerate() {
            let last = i + 1 == n_lines && s.projection.is_none() && tail.is_empty();
            self.line(&format!("{l}{}", if last { suffix } else { "" }));
        }
        if let Some(p) = &s.projection {
            let last = tail.is_empty();
            self.shape_with_suffix("as ", p, if last { suffix } else { "" });
        }
        for (i, t) in tail.iter().enumerate() {
            let last = i + 1 == tail.len();
            self.line(&format!("{t}{}", if last { suffix } else { "" }));
        }
        if lines.is_empty() && s.projection.is_none() && tail.is_empty() && !suffix.is_empty() {
            let trimmed = self.out.trim_end_matches('\n').to_string();
            self.out = format!("{trimmed}{suffix}\n");
        }
    }

    fn shape(&mut self, prefix: &str, shape: &ObjectShape) {
        if shape
            .fields
            .iter()
            .all(|f| matches!(f, ProjField::Column(_)))
            && shape_inline_len(shape) <= 72
        {
            let inner = shape
                .fields
                .iter()
                .map(proj_field_text)
                .collect::<Vec<_>>()
                .join(", ");
            self.line(&format!("{prefix}{{ {inner} }}"));
            return;
        }
        self.line(&format!("{prefix}{{"));
        self.depth += 1;
        for (i, f) in shape.fields.iter().enumerate() {
            let comma = if i + 1 < shape.fields.len() { "," } else { "" };
            match f {
                ProjField::Nested {
                    alias,
                    shape: inner,
                    ..
                } => {
                    self.shape(&format!("{}: ", alias.name), inner);
                    let trimmed = self.out.trim_end_matches('\n').to_string();
                    self.out = format!("{trimmed}{comma}\n");
                }
                _ => self.line(&format!("{}{comma}", proj_field_text(f))),
            }
        }
        self.depth -= 1;
        self.line("}");
    }
}

fn stmt_attached(s: &Stmt) -> &Attached {
    match s {
        Stmt::Dispatch { at, .. }
        | Stmt::Break { at, .. }
        | Stmt::Continue { at, .. }
        | Stmt::Let { at, .. }
        | Stmt::Assign { at, .. }
        | Stmt::If { at, .. }
        | Stmt::For { at, .. }
        | Stmt::Return { at, .. }
        | Stmt::Throw { at, .. }
        | Stmt::Transaction { at, .. }
        | Stmt::Assert { at, .. }
        | Stmt::Expr { at, .. } => at,
    }
}

fn is_attribute(m: &ColumnModifier) -> bool {
    matches!(
        m,
        ColumnModifier::Private(_)
            | ColumnModifier::Server(_)
            | ColumnModifier::Unique { .. }
            | ColumnModifier::Rule(_)
    )
}

fn modifier(m: &ColumnModifier) -> String {
    match m {
        ColumnModifier::PrimaryKey(_) => "primary key".into(),
        ColumnModifier::Identity(_) => "identity".into(),
        ColumnModifier::Unique { message, .. } => match message {
            Some(msg) => format!("unique : {}", quote(msg)),
            None => "unique".into(),
        },
        ColumnModifier::Private(_) => "private".into(),
        ColumnModifier::Server(_) => "server".into(),
        ColumnModifier::Default(e, _) => format!("default {}", expr(e)),
        ColumnModifier::OnUpdate(e, _) => format!("on update {}", expr(e)),
        ColumnModifier::Physical(p, _) => format!("as {}", quote(p)),
        ColumnModifier::Was(p, _) => format!("was {}", quote(p)),
        ColumnModifier::Rule(r) => rule_call(r),
    }
}

fn rule_call(r: &RuleCall) -> String {
    let mut s = if r.args.is_empty() {
        r.name.name.clone()
    } else {
        format!(
            "{}({})",
            r.name.name,
            r.args.iter().map(expr).collect::<Vec<_>>().join(", ")
        )
    };
    if let Some(m) = &r.message {
        s.push_str(&format!(" : {}", quote(m)));
    }
    s
}

fn index_column(c: &IndexColumn) -> String {
    let mut s = c.name.name.clone();
    if c.desc {
        s.push_str(" desc");
    }
    match c.nulls {
        Some(NullsOrder::First) => s.push_str(" nulls first"),
        Some(NullsOrder::Last) => s.push_str(" nulls last"),
        None => {}
    }
    s
}

fn action_text(a: RefAction) -> &'static str {
    match a {
        RefAction::Cascade => "cascade",
        RefAction::Restrict => "restrict",
        RefAction::NoAction => "no action",
        RefAction::SetNull => "set null",
        RefAction::SetDefault => "set default",
    }
}

fn names(v: &[Ident]) -> String {
    v.iter()
        .map(|i| i.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn params_text(p: &[Param]) -> String {
    p.iter()
        .map(|x| {
            let d = x
                .default
                .as_ref()
                .map(|e| format!(" = {}", expr(e)))
                .unwrap_or_default();
            format!("{}: {}{d}", x.name.name, type_ref(&x.ty))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn type_ref(t: &TypeRef) -> String {
    let mut s = match &t.kind {
        TypeKind::Scalar { name, args } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!(
                    "{name}({})",
                    args.iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        TypeKind::Record(fields) => format!(
            "{{ {} }}",
            fields
                .iter()
                .map(|(n, ty)| format!("{}: {}", n.name, type_ref(ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeKind::Named(d) => d.text(),
    };
    if t.optional {
        s.push('?');
    }
    for i in 0..t.array_depth as usize {
        s.push_str("[]");
        if t.array_optional.get(i).copied().unwrap_or(false) {
            s.push('?');
        }
    }
    s
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn raw_quote(s: &str) -> String {
    format!("r\"{}\"", s.replace('"', "\\\""))
}

fn join_text(j: &JoinClause) -> String {
    let kind = match j.kind {
        JoinKind::Left => "left",
        JoinKind::Inner => "inner",
    };
    let alias = if j.binder.name == j.table.object.name {
        String::new()
    } else {
        format!(" {}", j.binder.name)
    };
    let mut s = format!("{kind} join {}{alias} on {}", j.table.text(), expr(&j.on));
    if let Some(f) = &j.filter {
        s.push_str(&format!(" where {}", expr(f)));
    }
    if let Some(r) = &j.result {
        match r.cardinality {
            Cardinality::Group => {
                s.push_str(" as group");
                return s;
            }
            Cardinality::One => s.push_str(&format!(" as one {}", r.name.name)),
            Cardinality::Many => s.push_str(&format!(" as many {}", r.name.name)),
        }
        if let Some(u) = &r.under {
            s.push_str(&format!(" under {}", u.name));
        }
        if !r.order_by.is_empty() {
            s.push_str(&format!(" orderby {}", sort_keys(&r.order_by)));
        }
        if let Some(l) = &r.limit {
            s.push_str(&format!(" limit {}", expr(l)));
        }
    }
    s
}

fn sort_keys(keys: &[SortKey]) -> String {
    keys.iter()
        .map(|k| {
            let mut s = expr(&k.expr);
            if k.desc {
                s.push_str(" desc");
            } else {
                s.push_str(" asc");
            }
            match k.nulls {
                Some(NullsOrder::First) => s.push_str(" nulls first"),
                Some(NullsOrder::Last) => s.push_str(" nulls last"),
                None => {}
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn page_text(p: &PageClause) -> String {
    let mut s = "page".to_string();
    if let Some(a) = &p.after {
        s.push_str(&format!(" after {}", expr(a)));
    }
    s.push_str(&format!(" size {}", expr(&p.size)));
    if let Some(m) = &p.max {
        s.push_str(&format!(" max {}", expr(m)));
    }
    s
}

fn shape_inline_len(s: &ObjectShape) -> usize {
    s.fields.iter().map(|f| proj_field_text(f).len() + 2).sum()
}

fn proj_field_text(f: &ProjField) -> String {
    match f {
        ProjField::Column(i) => i.name.clone(),
        ProjField::Expr { alias, value, .. } => format!("{}: {}", alias.name, expr(value)),
        ProjField::Nested { alias, shape, .. } => format!(
            "{}: {{ {} }}",
            alias.name,
            shape
                .fields
                .iter()
                .map(proj_field_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn shape_text(s: &ObjectShape) -> String {
    format!(
        "{{ {} }}",
        s.fields
            .iter()
            .map(proj_field_text)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn obj_entries_text(entries: &[ObjEntry]) -> String {
    entries
        .iter()
        .map(|e| match e {
            ObjEntry::Field {
                key, value, assign, ..
            } => {
                let k = if key.name.contains('-') || key.name.contains(' ') {
                    quote(&key.name)
                } else {
                    key.name.clone()
                };
                format!("{k}{} {}", if *assign { " =" } else { ":" }, expr(value))
            }
            ObjEntry::Spread { source, except, .. } => {
                if except.is_empty() {
                    format!("...${}", source.name)
                } else {
                    format!("...${} except ({})", source.name, names(except))
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn obj_entry_text_padded(e: &ObjEntry, pad: usize) -> String {
    match e {
        ObjEntry::Field {
            key,
            value,
            assign: true,
            ..
        } => format!("{:pad$} = {}", key.name, expr(value), pad = pad),
        other => obj_entries_text(std::slice::from_ref(other)),
    }
}

fn set_item_text(i: &SetItem) -> String {
    set_items_text(std::slice::from_ref(i))
}

fn set_items_text(items: &[SetItem]) -> String {
    items
        .iter()
        .map(|i| match i {
            SetItem::Set {
                column,
                value,
                optional,
                ..
            } => format!(
                "{} {} {}",
                column.name,
                if *optional { "=?" } else { "=" },
                expr(value)
            ),
            SetItem::Spread { source, except, .. } => {
                if except.is_empty() {
                    format!("...${}", source.name)
                } else {
                    format!("...${} except ({})", source.name, names(except))
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Precedence for parenthesisation. Higher binds tighter.
fn prec(e: &ExprKind) -> u8 {
    match e {
        ExprKind::Coalesce { .. } => 1,
        ExprKind::Ternary { .. } => 2,
        ExprKind::Binary { op: BinOp::Or, .. } => 3,
        ExprKind::Binary { op: BinOp::And, .. } => 4,
        ExprKind::Unary {
            op: UnaryOp::Not, ..
        } => 5,
        ExprKind::Binary { op, .. } if is_compare(*op) => 6,
        ExprKind::In { .. } | ExprKind::Exists { .. } => 6,
        ExprKind::Binary {
            op: BinOp::Add | BinOp::Sub,
            ..
        } => 7,
        ExprKind::Binary {
            op: BinOp::Mul | BinOp::Div | BinOp::Rem,
            ..
        } => 8,
        ExprKind::Unary {
            op: UnaryOp::Neg, ..
        } => 9,
        _ => 10,
    }
}

fn is_compare(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq
            | BinOp::Ne
            | BinOp::EqOpt
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Like
            | BinOp::ILike
    )
}

fn wrap(e: &Expr, parent: u8) -> String {
    let s = expr(e);
    if prec(&e.kind) < parent {
        format!("({s})")
    } else {
        s
    }
}

pub fn expr(e: &Expr) -> String {
    match &*e.kind {
        ExprKind::Int(n) | ExprKind::Decimal(n) => n.clone(),
        ExprKind::Str(s) => quote(s),
        ExprKind::RawStr(s) => raw_quote(s),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Null => "null".into(),
        ExprKind::Name(i) => i.name.clone(),
        ExprKind::Local(i) => format!("${}", i.name),
        ExprKind::PathParam(i) => format!("@{}", i.name),
        ExprKind::Field { base, field } => format!("{}.{}", wrap(base, 10), field.name),
        ExprKind::Index { base, index } => format!("{}[{}]", wrap(base, 10), expr(index)),
        ExprKind::Call {
            callee,
            args,
            filter,
        } => {
            let a = args.iter().map(expr).collect::<Vec<_>>().join(", ");
            let f = filter
                .as_ref()
                .map(|x| format!(" where {}", expr(x)))
                .unwrap_or_default();
            format!("{}({a}{f})", wrap(callee, 10))
        }
        ExprKind::Unary { op, rhs } => match op {
            UnaryOp::Not => format!("!{}", wrap(rhs, 5)),
            UnaryOp::Neg => format!("-{}", wrap(rhs, 9)),
        },
        ExprKind::Binary { op, lhs, rhs } => {
            let p = prec(&e.kind);
            format!("{} {} {}", wrap(lhs, p), op.as_str(), wrap(rhs, p + 1))
        }
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => format!(
            "{} ? {} : {}",
            wrap(cond, 3),
            wrap(then, 2),
            wrap(otherwise, 2)
        ),
        ExprKind::Coalesce { lhs, rhs } => format!("{} ?? {}", wrap(lhs, 2), wrap(rhs, 2)),
        ExprKind::In {
            lhs,
            items,
            negated,
        } => format!(
            "{} {}in ({})",
            wrap(lhs, 7),
            if *negated { "not " } else { "" },
            items.iter().map(expr).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::Exists { query, negated } => format!(
            "{}exists ({})",
            if *negated { "not " } else { "" },
            expr(query)
        ),
        ExprKind::Object(entries) => {
            if entries.is_empty() {
                "{ }".into()
            } else {
                format!("{{ {} }}", obj_entries_text(entries))
            }
        }
        ExprKind::Array(items) => format!(
            "[{}]",
            items.iter().map(expr).collect::<Vec<_>>().join(", ")
        ),
        ExprKind::Select(s) => select_inline(s),
        ExprKind::Insert(i) => {
            let mut out = format!(
                "insert into {} {{ {} }}",
                i.table.text(),
                obj_entries_text(&i.values)
            );
            if let Some(c) = &i.conflict {
                let cols = if c.columns.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", names(&c.columns))
                };
                match &c.action {
                    ConflictAction::DoNothing => {
                        out.push_str(&format!(" on conflict{cols} do nothing"))
                    }
                    ConflictAction::DoUpdate(sets) => out.push_str(&format!(
                        " on conflict{cols} do update set {}",
                        set_items_text(sets)
                    )),
                }
            }
            if let Some(p) = &i.projection {
                out.push_str(&format!(" as {}", shape_text(p)));
            }
            out
        }
        ExprKind::Update(u) => {
            let mut out = format!("update {} set {}", u.table.text(), set_items_text(&u.sets));
            if let Some(f) = &u.filter {
                out.push_str(&format!(" where {}", expr(f)));
            }
            if let Some(p) = &u.projection {
                out.push_str(&format!(" as {}", shape_text(p)));
            }
            if !u.order_by.is_empty() {
                out.push_str(&format!(" orderby {}", sort_keys(&u.order_by)));
            }
            if u.first {
                out.push_str(" first");
            }
            out
        }
        ExprKind::Delete(d) => {
            let mut out = format!("delete from {}", d.table.text());
            if let Some(f) = &d.filter {
                out.push_str(&format!(" where {}", expr(f)));
            }
            if let Some(p) = &d.projection {
                out.push_str(&format!(" as {}", shape_text(p)));
            }
            if !d.order_by.is_empty() {
                out.push_str(&format!(" orderby {}", sort_keys(&d.order_by)));
            }
            if d.first {
                out.push_str(" first");
            }
            out
        }
        ExprKind::OrThrow { value, error, args } => {
            let a = args.iter().map(expr).collect::<Vec<_>>().join(", ");
            if args.is_empty() {
                format!("{} or throw {}", expr(value), error.name)
            } else {
                format!("{} or throw {}({a})", expr(value), error.name)
            }
        }
        ExprKind::CatchPostfix {
            value,
            error,
            binder,
            body,
        } => {
            let mut w = Writer {
                out: String::new(),
                depth: 1,
            };
            w.block(body);
            format!(
                "{} catch {} ({}) {{\n{}}}",
                expr(value),
                error.name,
                binder.name,
                w.out
            )
        }
        ExprKind::Cast { value, ty } => format!("{} as {}", wrap(value, 10), ty.name),
        ExprKind::WithHeaders { value, headers } => {
            format!("{} with {{ {} }}", expr(value), obj_entries_text(headers))
        }
        ExprKind::Cookie { value, args } => format!(
            "{} cookie({})",
            expr(value),
            args.iter().map(expr).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn select_inline(s: &SelectExpr) -> String {
    let mut out = format!("select {} from {}", s.binder.name, s.source.text());
    for j in &s.joins {
        out.push(' ');
        out.push_str(&join_text(j));
    }
    if let Some(f) = &s.filter {
        out.push_str(&format!(" where {}", expr(f)));
    }
    if !s.group_by.is_empty() {
        out.push_str(&format!(
            " group by {}",
            s.group_by.iter().map(expr).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(h) = &s.having {
        out.push_str(&format!(" having {}", expr(h)));
    }
    if let Some(p) = &s.projection {
        out.push_str(&format!(" as {}", shape_text(p)));
    }
    if !s.order_by.is_empty() {
        out.push_str(&format!(" orderby {}", sort_keys(&s.order_by)));
    }
    if let Some(p) = &s.page {
        out.push(' ');
        out.push_str(&page_text(p));
    } else if let Some(l) = &s.limit {
        out.push_str(&format!(" limit {}", expr(l)));
    }
    if s.first {
        out.push_str(" first");
    }
    out
}
