//! Routing, middleware composition, and the error model.
//!
//! Everything here is decidable statically, and all of it is the kind of
//! thing that used to be discovered at runtime: which route wins, which
//! middleware ran first, whether a `context` key was ever set, and which
//! errors can reach the boundary.
//!
//! Implements routing.md §3–§4, middleware.md §2–§6, and errors.md
//! E1–E14 minus the parts the checker already covers.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::symbols::Symbols;
use crate::token::Span;
use crate::workspace::{Loc, Workspace};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub struct Wired {
    pub routes: Vec<ResolvedRoute>,
    pub diags: Vec<(Loc, Diagnostic)>,
}

#[derive(Clone, Debug)]
pub struct ResolvedRoute {
    pub method: String,
    /// The declared pattern, e.g. `/api/v1/orgs/{org_id}` — what
    /// `request.route()` returns (routing.md §5.4).
    pub pattern: String,
    pub segments: Vec<Segment>,
    /// Path parameters, prefix then suffix (routing.md §3.3).
    pub params: Vec<(String, String)>,
    /// Middleware in execution order: block list, then route list
    /// (middleware.md §4.1).
    pub chain: Vec<String>,
    /// `after` blocks, in reverse chain order.
    pub after: Vec<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Param { name: String, ty: String },
}

impl Segment {
    /// Two routes collide when their shapes match: same literals in the
    /// same places, parameters wherever the other has one. Parameter
    /// *names* are irrelevant — `/orgs/{a}` and `/orgs/{b}` are one route.
    fn same_shape(&self, other: &Segment) -> bool {
        match (self, other) {
            (Segment::Literal(a), Segment::Literal(b)) => a == b,
            (Segment::Param { .. }, Segment::Param { .. }) => true,
            _ => false,
        }
    }
}

pub fn wire(ws: &Workspace, sym: &Symbols) -> Wired {
    let mut w = Wiring {
        ws,
        sym,
        diags: Vec::new(),
        routes: Vec::new(),
    };
    w.collect_routes();
    w.check_duplicates();
    w.check_slot_agreement();
    w.check_middleware();
    w.check_context();
    w.check_error_model();
    w.check_cursor_secret();
    Wired {
        routes: w.routes,
        diags: w.diags,
    }
}

struct Wiring<'a> {
    ws: &'a Workspace,
    sym: &'a Symbols,
    diags: Vec<(Loc, Diagnostic)>,
    routes: Vec<ResolvedRoute>,
}

impl<'a> Wiring<'a> {
    fn err(
        &mut self,
        loc: Loc,
        code: &'static str,
        msg: impl Into<String>,
        note: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            loc,
            Diagnostic::error(code, loc.span, msg)
                .note(note)
                .clause(clause),
        ));
    }

    /// config.md §3.2 — an unknown `server { }` key.
    ///
    /// This exists for the same reason `E1202` does on `init()`: a
    /// misspelled key is otherwise **silent**. The setting keeps its
    /// default and the deployment runs with a value nobody chose. Two of
    /// them are worse than a wrong number — `trusted_proxie` leaves the
    /// proxy list empty, so `client_ip()` quietly reports the proxy's
    /// address for every request and a rate limiter keyed on it becomes
    /// one shared bucket; `max_body_byte` leaves the limit at 1 MB after
    /// someone deliberately narrowed it.
    fn check_server_keys(&mut self, sv: &crate::ast::ServerDecl, fi: usize) {
        use crate::ast::ServerEntry;
        const KEYS: [&str; 9] = [
            "max_body_bytes",
            "request_timeout",
            "header_timeout",
            "max_page_size",
            "strict_slash",
            "cursor_secret",
            "trusted_proxies",
            "shutdown_grace",
            "bind",
        ];
        const GROUPS: [&str; 2] = ["cors", "tls"];
        const CORS: [&str; 5] = ["origins", "methods", "headers", "credentials", "max_age"];
        const TLS: [&str; 2] = ["cert", "key"];

        let unknown = |this: &mut Self, name: &str, span, what: &str, known: &[&str]| {
            this.err(
                Loc { file: fi, span },
                "E1206",
                format!("unknown {what} `{name}`"),
                format!("the keys are: {}", known.join(", ")),
                "config.md §3.2",
            );
        };

        for e in &sv.entries {
            match e {
                ServerEntry::Set(a) => {
                    if !KEYS.contains(&a.key.name.as_str()) {
                        unknown(self, &a.key.name, a.key.span, "`server { }` key", &KEYS);
                    }
                }
                ServerEntry::Group {
                    name,
                    entries,
                    span,
                } => {
                    if !GROUPS.contains(&name.name.as_str()) {
                        unknown(self, &name.name, *span, "`server { }` block", &GROUPS);
                        continue;
                    }
                    let known: &[&str] = if name.name == "cors" { &CORS } else { &TLS };
                    let what = format!("`{}` key", name.name);
                    for a in entries {
                        if !known.contains(&a.key.name.as_str()) {
                            unknown(self, &a.key.name, a.key.span, &what, known);
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ routes

    fn collect_routes(&mut self) {
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let Decl::Routes(block) = d else { continue };
                let block_uses: Vec<String> = block.uses.iter().map(|i| i.name.clone()).collect();
                for r in &block.routes {
                    let raw = join_path(&block.prefix, &r.suffix);
                    let segments = parse_path(&raw);
                    // routing.md §5.4 — the declared pattern, without the
                    // type annotation: `/api/v1/orgs/{org_id}`. This is what
                    // `request.route()` returns, so it is also the shape a
                    // rate-limit key is bucketed by.
                    let pattern = render(&segments);
                    let params = segments
                        .iter()
                        .filter_map(|s| match s {
                            Segment::Param { name, ty } => Some((name.clone(), ty.clone())),
                            _ => None,
                        })
                        .collect();
                    let mut chain = block_uses.clone();
                    chain.extend(r.uses.iter().map(|i| i.name.clone()));
                    let after = chain
                        .iter()
                        .rev()
                        .filter(|m| self.sym.middleware.get(*m).is_some_and(|s| s.has_after))
                        .cloned()
                        .collect();
                    self.routes.push(ResolvedRoute {
                        method: r.method.name.clone(),
                        pattern,
                        segments,
                        params,
                        chain,
                        after,
                        loc: Loc {
                            file: fi,
                            span: r.span,
                        },
                    });
                }
            }
        }
    }

    /// routing.md §4.1 — a duplicate `(method, resolved path)` is a hard
    /// error naming both sites. Last-wins does not exist: registration
    /// order is a file ordering, and file ordering is not a language
    /// feature.
    fn check_duplicates(&mut self) {
        let routes = self.routes.clone();
        let mut problems = Vec::new();
        for (i, a) in routes.iter().enumerate() {
            for b in routes.iter().take(i) {
                if a.method != b.method || a.segments.len() != b.segments.len() {
                    continue;
                }
                if a.segments
                    .iter()
                    .zip(&b.segments)
                    .all(|(x, y)| x.same_shape(y))
                {
                    problems.push((
                        a.loc,
                        format!("duplicate route `{} {}`", a.method, a.pattern),
                        format!(
                            "already declared at {} as `{}` — parameter names do not \
                             distinguish two routes",
                            self.ws.file_line(b.loc),
                            b.pattern
                        ),
                    ));
                }
            }
        }
        for (loc, msg, note) in problems {
            self.err(loc, "E0710", msg, note, "routing.md §4.1");
        }
    }

    /// routing.md §3.4 — one URL slot, one name and one type, everywhere.
    /// Otherwise a middleware reading `@org_id` means two different things
    /// depending on which block invoked it.
    fn check_slot_agreement(&mut self) {
        // key: (segment index, the literal prefix up to it) -> (name, type)
        let mut slots: BTreeMap<(usize, String), (String, String, Loc)> = BTreeMap::new();
        let routes = self.routes.clone();
        let mut problems = Vec::new();
        for r in &routes {
            let mut prefix = String::new();
            for (i, seg) in r.segments.iter().enumerate() {
                match seg {
                    Segment::Literal(l) => {
                        prefix.push('/');
                        prefix.push_str(l);
                    }
                    Segment::Param { name, ty } => {
                        let key = (i, prefix.clone());
                        match slots.get(&key) {
                            Some((n, t, other)) if n != name || t != ty => {
                                problems.push((
                                    r.loc,
                                    format!(
                                        "path slot {i} is `{{{name}: {ty}}}` here and \
                                         `{{{n}: {t}}}` at {}",
                                        self.ws.file_line(*other)
                                    ),
                                ));
                            }
                            None => {
                                slots.insert(key, (name.clone(), ty.clone(), r.loc));
                            }
                            _ => {}
                        }
                        prefix.push_str("/{}");
                    }
                }
            }
        }
        for (loc, msg) in problems {
            self.err(
                loc,
                "E0701",
                msg,
                "a middleware reading this parameter would mean two different things \
                 depending on which block invoked it",
                "routing.md §3.4",
            );
        }
    }

    /// middleware.md §2, §3, §4.1.
    fn check_middleware(&mut self) {
        let routes = self.routes.clone();
        let mut problems: Vec<(Loc, &'static str, String, String, &'static str)> = Vec::new();

        for r in &routes {
            // §4.1 — a name appearing in both lists is an error: run twice,
            // or reorder, is not guessable.
            let mut seen = HashSet::new();
            for m in &r.chain {
                if !seen.insert(m.clone()) {
                    problems.push((
                        r.loc,
                        "E0804",
                        format!("`{m}` appears twice in this route's chain"),
                        "route-level `use` appends to the block's list; a name in both \
                         is ambiguous between running twice and reordering"
                            .into(),
                        "middleware.md §4.1",
                    ));
                }
            }

            let declared: HashMap<&str, &str> = r
                .params
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();

            for (i, m) in r.chain.iter().enumerate() {
                let Some(sym) = self.sym.middleware.get(m) else {
                    problems.push((
                        r.loc,
                        "E0805",
                        format!("`{m}` is not a declared middleware"),
                        "declare it, or remove it from `use`".into(),
                        "middleware.md §1",
                    ));
                    continue;
                };

                // §2.2 — every declared binder must exist at the attachment
                // site, with a matching type.
                for (name, ty) in &sym.binders {
                    let want = ty.to_string();
                    match declared.get(name.as_str()) {
                        None => problems.push((
                            r.loc,
                            "E0802",
                            format!(
                                "`{m}` declares `@{name}: {want}` but `{} {}` has no \
                                 such path parameter",
                                r.method, r.pattern
                            ),
                            format!(
                                "add it to the path (`{{{name}: {want}}}`) or attach \
                                 `{m}` somewhere that has it"
                            ),
                            "middleware.md §2.2",
                        )),
                        Some(have) if *have != want => problems.push((
                            r.loc,
                            "E0802",
                            format!(
                                "`{m}` declares `@{name}: {want}` but this route \
                                 declares `{{{name}: {have}}}`"
                            ),
                            "the types must match exactly".into(),
                            "middleware.md §2.2",
                        )),
                        _ => {}
                    }
                }

                // §3.1 — a declared dependency must actually run earlier.
                for req in &sym.requires {
                    let position = r.chain.iter().position(|x| x == req);
                    match position {
                        Some(p) if p < i => {}
                        Some(_) => problems.push((
                            r.loc,
                            "E0803",
                            format!("`{m}` requires `{req}`, which runs after it"),
                            format!("chain: {}", r.chain.join(" → ")),
                            "middleware.md §3.1",
                        )),
                        None => problems.push((
                            r.loc,
                            "E0803",
                            format!("`{m}` requires `{req}`, which is not in this chain"),
                            format!(
                                "chain: {} — `requires` is checked, not satisfied \
                                 automatically",
                                r.chain.join(" → ")
                            ),
                            "middleware.md §3.2",
                        )),
                    }
                }
            }
        }
        for (loc, code, msg, note, clause) in problems {
            self.err(loc, code, msg, note, clause);
        }
    }

    /// middleware.md §6.2 — `context.k` must be provided by a middleware
    /// that provably runs earlier in **every** chain that reaches the
    /// reader.
    fn check_context(&mut self) {
        // What each middleware provides.
        let provides: HashMap<&str, HashSet<&str>> = self
            .sym
            .middleware
            .iter()
            .map(|(n, m)| {
                (
                    n.as_str(),
                    m.provides.iter().map(|(k, _)| k.as_str()).collect(),
                )
            })
            .collect();

        let mut problems = Vec::new();

        // Routes: everything before the handler is available.
        let routes = self.routes.clone();
        for r in &routes {
            let mut available: HashSet<String> = HashSet::new();
            for m in &r.chain {
                if let Some(p) = provides.get(m.as_str()) {
                    available.extend(p.iter().map(|s| s.to_string()));
                }
            }
            let body = self.route_body(r);
            if let Some(body) = body {
                for (key, span, optional) in context_reads(&body) {
                    if optional || available.contains(&key) {
                        continue;
                    }
                    problems.push((
                        Loc {
                            file: r.loc.file,
                            span,
                        },
                        format!("`context.{key}` is not provided on this route"),
                        format!(
                            "chain: {} — add a middleware that declares \
                             `provides {key}: <type>`, or read it as `context.{key}?`",
                            if r.chain.is_empty() {
                                "(none)".to_string()
                            } else {
                                r.chain.join(" → ")
                            }
                        ),
                    ));
                }
            }
        }

        // Middleware: only what its own `requires` guarantees.
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let Decl::Middleware(m) = d else { continue };
                let Some(sym) = self.sym.middleware.get(&m.name.name) else {
                    continue;
                };
                let mut available: HashSet<String> = HashSet::new();
                for req in &sym.requires {
                    if let Some(p) = provides.get(req.as_str()) {
                        available.extend(p.iter().map(|s| s.to_string()));
                    }
                }
                let own: HashSet<&str> = sym.provides.iter().map(|(k, _)| k.as_str()).collect();

                let mut body = m.body.clone();
                if let Some(a) = &m.after {
                    body.extend(a.clone());
                }
                for (key, span, optional) in context_reads(&body) {
                    if optional || available.contains(&key) || own.contains(key.as_str()) {
                        continue;
                    }
                    problems.push((
                        Loc { file: fi, span },
                        format!(
                            "`{}` reads `context.{key}` without declaring a dependency",
                            m.name.name
                        ),
                        format!(
                            "add `requires <Middleware>` naming the one that declares \
                             `provides {key}: <type>` — an undeclared dependency is \
                             what made the sample's admin gate order-dependent"
                        ),
                    ));
                }

                // §6.4 — writing needs a `provides` declaration.
                for (key, span) in context_writes(&m.body) {
                    if !own.contains(key.as_str()) {
                        problems.push((
                            Loc { file: fi, span },
                            format!(
                                "`{}` sets `context.{key}` without declaring it",
                                m.name.name
                            ),
                            format!("add `provides {key}: <type>` to the declaration"),
                        ));
                    }
                }
            }
        }

        for (loc, msg, note) in problems {
            let code = if msg.contains("sets ") {
                "E0821"
            } else {
                "E0820"
            };
            self.err(loc, code, msg, note, "middleware.md §6");
        }
    }

    fn route_body(&self, r: &ResolvedRoute) -> Option<Block> {
        let file = self.ws.files.get(r.loc.file)?;
        for d in &file.program.decls {
            if let Decl::Routes(block) = d {
                for route in &block.routes {
                    if route.span.start == r.loc.span.start {
                        return Some(route.body.clone());
                    }
                }
            }
        }
        None
    }

    // ------------------------------------------------------------ errors

    /// errors.md §3.1 — the raise set, as a fixed point over the static
    /// call graph.
    fn raise_sets(&self) -> HashMap<String, BTreeSet<String>> {
        // Direct throws, `or throw`s, and constraint promotions per
        // function, plus the callees it reaches.
        let mut direct: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut calls: HashMap<String, BTreeSet<String>> = HashMap::new();

        for file in &self.ws.files {
            for d in &file.program.decls {
                match d {
                    Decl::Function(f) => {
                        let key = f.name.name.clone();
                        direct.insert(key.clone(), self.direct_raises(&f.body));
                        calls.insert(key, callees(&f.body));
                    }
                    Decl::Service(s) => {
                        for f in &s.functions {
                            let key = format!("{}.{}", s.name.name, f.name.name);
                            direct.insert(key.clone(), self.direct_raises(&f.body));
                            calls.insert(key, callees(&f.body));
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut sets = direct.clone();
        loop {
            let mut changed = false;
            let keys: Vec<String> = sets.keys().cloned().collect();
            for k in keys {
                let mut acc = sets.get(&k).cloned().unwrap_or_default();
                if let Some(cs) = calls.get(&k) {
                    for c in cs {
                        if let Some(inner) = sets.get(c) {
                            for e in inner {
                                if acc.insert(e.clone()) {
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                sets.insert(k, acc);
            }
            if !changed {
                break;
            }
        }
        sets
    }

    /// Own `throw`s, `or throw`s, and the constraints of tables this body
    /// writes — minus anything a postfix `catch` swallows (errors.md §3.1,
    /// §6.4).
    fn direct_raises(&self, b: &Block) -> BTreeSet<String> {
        direct_raises(b, self.sym)
    }

    fn check_error_model(&mut self) {
        let sets = self.raise_sets();

        // E7 — an `after` block's raise set must be empty. There is no
        // outer handler left: the response is already decided.
        let mut problems: Vec<(Loc, &'static str, String, String, &'static str)> = Vec::new();
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let Decl::Middleware(m) = d else { continue };
                let Some(after) = &m.after else { continue };
                let after_span = after.first().map(stmt_span).unwrap_or(m.span);
                let mut raises = self.direct_raises(after);
                for c in callees(after) {
                    if let Some(inner) = sets.get(&c) {
                        raises.extend(inner.iter().cloned());
                    }
                }
                if !raises.is_empty() {
                    problems.push((
                        Loc {
                            file: fi,
                            span: after_span,
                        },
                        "E0811",
                        format!(
                            "`{}`'s `after` block can raise {}",
                            m.name.name,
                            raises.iter().cloned().collect::<Vec<_>>().join(", ")
                        ),
                        "an `after` block runs once the response is decided, so there \
                         is no handler left. Wrap the fallible statement in a postfix \
                         `catch` that returns"
                            .into(),
                        "middleware.md §5.5",
                    ));
                }
            }
        }

        // E3/E5/E6 — the boundary.
        let mut boundary: BTreeSet<String> = BTreeSet::new();
        for r in self.routes.clone() {
            if let Some(body) = self.route_body(&r) {
                boundary.extend(self.direct_raises(&body));
                for c in callees(&body) {
                    if let Some(inner) = sets.get(&c) {
                        boundary.extend(inner.iter().cloned());
                    }
                }
            }
            for m in &r.chain {
                if let Some(decl) = self.middleware_decl(m) {
                    boundary.extend(self.direct_raises(&decl));
                    for c in callees(&decl) {
                        if let Some(inner) = sets.get(&c) {
                            boundary.extend(inner.iter().cloned());
                        }
                    }
                }
            }
        }

        // errors.md §3.3 — a declared `raises` is a public contract, so it
        // must cover what the function can actually raise. A declaration
        // that is short is worse than none: a caller reads it and handles
        // less than arrives.
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let functions: Vec<(String, &FunctionDecl)> = match d {
                    Decl::Function(f) => vec![(f.name.name.clone(), f)],
                    Decl::Service(sv) => sv
                        .functions
                        .iter()
                        .map(|f| (format!("{}.{}", sv.name.name, f.name.name), f))
                        .collect(),
                    _ => continue,
                };
                for (key, f) in functions {
                    if f.raises.is_empty() {
                        continue;
                    }
                    let declared: BTreeSet<String> =
                        f.raises.iter().map(|i| i.name.clone()).collect();
                    let inferred = sets.get(&key).cloned().unwrap_or_default();
                    let missing: Vec<String> = inferred.difference(&declared).cloned().collect();
                    if !missing.is_empty() {
                        self.err(
                            Loc {
                                file: fi,
                                span: f.span,
                            },
                            "E1002",
                            format!(
                                "`{key}` raises `{}`, which its `raises` does not declare",
                                missing.join("`, `")
                            ),
                            "a declared raise set is a public contract: a caller reads it \
                             and handles exactly what it names",
                            "errors.md §3.3",
                        );
                    }
                }
            }
        }

        // errors.md §4.1 — exactly one `errorHandler` per program. Two
        // would make which one answers depend on file order.
        let mut handlers: Vec<Loc> = Vec::new();
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                if let Decl::ErrorHandler(h) = d {
                    handlers.push(Loc {
                        file: fi,
                        span: h.span,
                    });
                }
            }
        }
        // config.md §3 — one `server` block, for the same reason.
        let mut servers: Vec<Loc> = Vec::new();
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                if let Decl::Server(sv) = d {
                    servers.push(Loc {
                        file: fi,
                        span: sv.span,
                    });
                }
            }
        }
        for loc in servers.iter().skip(1) {
            self.err(
                *loc,
                "E1204",
                "a second `server` block",
                "one per program: with two, which settings apply depends on the order \
                 the files happened to load in",
                "config.md §3",
            );
        }
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                if let Decl::Server(sv) = d {
                    self.check_server_keys(sv, fi);
                }
            }
        }

        for loc in handlers.iter().skip(1) {
            self.err(
                *loc,
                "E1010",
                "a second `errorHandler`",
                "exactly one per program: with two, which one answers depends on the \
                 order the files happened to load in",
                "errors.md §4.1",
            );
        }

        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let Decl::ErrorHandler(h) = d else { continue };
                let mut seen_untyped = false;
                for arm in &h.arms {
                    let loc = Loc {
                        file: fi,
                        span: arm.span,
                    };
                    // E6 — every arm must answer.
                    if !returns_a_response(&arm.body) {
                        problems.push((
                            loc,
                            "E1011",
                            "this `errorHandler` arm does not return a response".into(),
                            "every path through an arm must end in `return <response>;`".into(),
                            "errors.md §4.6",
                        ));
                    }
                    match &arm.error {
                        // E5 — an arm for a type nobody raises.
                        Some(name) => {
                            if !boundary.contains(&name.name) {
                                self.diags.push((
                                    loc,
                                    Diagnostic::warning(
                                        "W1001",
                                        arm.span,
                                        format!("nothing raises `{}`", name.name),
                                    )
                                    .note(
                                        "the arm is unreachable — a warning, not an \
                                         error, because a package upgrade can \
                                         legitimately remove a raise",
                                    )
                                    .clause("errors.md §4.5"),
                                ));
                            }
                        }
                        None => seen_untyped = true,
                    }
                }
                let _ = seen_untyped;
            }
        }

        // E13 — a nested transaction, over the call graph.
        let transactional = self.transactional_functions();
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                let functions: Vec<(&FunctionDecl, String)> = match d {
                    Decl::Function(f) => vec![(f, f.name.name.clone())],
                    Decl::Service(s) => s
                        .functions
                        .iter()
                        .map(|f| (f, format!("{}.{}", s.name.name, f.name.name)))
                        .collect(),
                    _ => vec![],
                };
                for (f, _key) in functions {
                    walk_block(&f.body, &mut |stmt| {
                        if let Stmt::Transaction { body, span, .. } = stmt {
                            for c in callees(body) {
                                if transactional.contains(&c) {
                                    problems.push((
                                        Loc {
                                            file: fi,
                                            span: *span,
                                        },
                                        "E0620",
                                        format!("`{c}` opens a transaction of its own"),
                                        "nested transactions are detected over the call \
                                         graph, not discovered at request time"
                                            .into(),
                                        "writes.md §7.3",
                                    ));
                                }
                            }
                        }
                    });
                }
            }
        }

        for (loc, code, msg, note, clause) in problems {
            self.err(loc, code, msg, note, clause);
        }
    }

    fn middleware_decl(&self, name: &str) -> Option<Block> {
        for file in &self.ws.files {
            for d in &file.program.decls {
                if let Decl::Middleware(m) = d {
                    if m.name.name == name {
                        let mut b = m.body.clone();
                        if let Some(a) = &m.after {
                            b.extend(a.clone());
                        }
                        return Some(b);
                    }
                }
            }
        }
        None
    }

    /// Functions whose body opens a `transaction`, directly or through a
    /// callee.
    fn transactional_functions(&self) -> HashSet<String> {
        let mut direct = HashSet::new();
        let mut calls: HashMap<String, BTreeSet<String>> = HashMap::new();
        for file in &self.ws.files {
            for d in &file.program.decls {
                let functions: Vec<(&FunctionDecl, String)> = match d {
                    Decl::Function(f) => vec![(f, f.name.name.clone())],
                    Decl::Service(s) => s
                        .functions
                        .iter()
                        .map(|f| (f, format!("{}.{}", s.name.name, f.name.name)))
                        .collect(),
                    _ => vec![],
                };
                for (f, key) in functions {
                    let mut has = false;
                    walk_block(&f.body, &mut |s| {
                        if matches!(s, Stmt::Transaction { .. }) {
                            has = true;
                        }
                    });
                    if has {
                        direct.insert(key.clone());
                    }
                    calls.insert(key, callees(&f.body));
                }
            }
        }
        loop {
            let mut changed = false;
            let keys: Vec<String> = calls.keys().cloned().collect();
            for k in keys {
                if direct.contains(&k) {
                    continue;
                }
                if calls
                    .get(&k)
                    .is_some_and(|cs| cs.iter().any(|c| direct.contains(c)))
                {
                    direct.insert(k);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        direct
    }
}

// ---------------------------------------------------------------- helpers

fn join_path(prefix: &str, suffix: &str) -> String {
    let p = prefix.trim_end_matches('/');
    let s = suffix.trim_start_matches('/');
    if s.is_empty() {
        if p.is_empty() {
            "/".to_string()
        } else {
            p.to_string()
        }
    } else {
        format!("{p}/{s}")
    }
}

impl Wiring<'_> {
    /// config.md §3 — a `page` query needs `server { cursor_secret }`.
    ///
    /// Checked at compile time so the runtime never has to decide what an
    /// unsigned cursor means. Unsigned, a cursor is a client-supplied
    /// predicate: a caller could hand back any ordering tuple and read
    /// rows the query's own `where` was meant to keep from them.
    fn check_cursor_secret(&mut self) {
        let mut paging: Option<Loc> = None;
        let mut has_secret = false;
        let mut server: Option<Loc> = None;
        for (fi, file) in self.ws.files.iter().enumerate() {
            for d in &file.program.decls {
                if let Decl::Server(s) = d {
                    server = Some(Loc {
                        file: fi,
                        span: s.span,
                    });
                    for e in &s.entries {
                        if let ServerEntry::Set(a) = e {
                            if a.key.name == "cursor_secret" {
                                has_secret = true;
                            }
                        }
                    }
                }
            }
            if paging.is_none() {
                for site in crate::query_sql::sites(&file.program) {
                    if site.select.page.is_some() {
                        paging = Some(Loc {
                            file: fi,
                            span: site.select.span,
                        });
                        break;
                    }
                }
            }
        }
        let Some(loc) = paging else { return };
        if has_secret {
            return;
        }
        self.err(
            server.unwrap_or(loc),
            "E1205",
            "`page` is used but `server { cursor_secret }` is not set",
            "a cursor is a client-supplied predicate; unsigned, it is a second \
             filter nobody checked",
            "config.md §3",
        );
    }
}

pub fn render(segments: &[Segment]) -> String {
    if segments.is_empty() {
        return "/".to_string();
    }
    let mut out = String::new();
    for s in segments {
        out.push('/');
        match s {
            Segment::Literal(l) => out.push_str(l),
            Segment::Param { name, .. } => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
    }
    out
}

/// The declared pattern for a route, from its block prefix and its own
/// suffix: `("/api/v1/auth", "register")` is `/api/v1/auth/register`.
///
/// This is the string `request.route()` returns and the one
/// `jwc explain --route` takes (routing.md §5.4). Concatenating the two raw
/// strings does not produce it: a suffix has no leading slash, and a
/// parameter still carries its type annotation.
pub fn route_pattern(prefix: &str, suffix: &str) -> String {
    render(&parse_path(&format!("{prefix}/{suffix}")))
}

pub fn parse_path(path: &str) -> Vec<Segment> {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                let inner = &s[1..s.len() - 1];
                match inner.split_once(':') {
                    Some((n, t)) => Segment::Param {
                        name: n.trim().to_string(),
                        ty: t.trim().to_string(),
                    },
                    // routing.md §3.1 — an untyped parameter is `text`.
                    None => Segment::Param {
                        name: inner.trim().to_string(),
                        ty: "text".to_string(),
                    },
                }
            } else {
                Segment::Literal(s.to_string())
            }
        })
        .collect()
}

fn stmt_span(s: &Stmt) -> Span {
    match s {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::If { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Return { span, .. }
        | Stmt::Throw { span, .. }
        | Stmt::Transaction { span, .. }
        | Stmt::Assert { span, .. }
        | Stmt::Expr { span, .. } => *span,
    }
}

fn walk_block(b: &Block, f: &mut impl FnMut(&Stmt)) {
    for s in b {
        f(s);
        match s {
            Stmt::If {
                then, otherwise, ..
            } => {
                walk_block(then, f);
                if let Some(alt) = otherwise {
                    walk_block(alt, f);
                }
            }
            Stmt::For { body, .. } | Stmt::Transaction { body, .. } => walk_block(body, f),
            Stmt::Assert {
                kind: AssertKind::Fails { body, .. },
                ..
            } => walk_block(body, f),
            _ => {}
        }
    }
}

fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &*e.kind {
        ExprKind::Field { base, .. } => walk_expr(base, f),
        ExprKind::Index { base, index } => {
            walk_expr(base, f);
            walk_expr(index, f);
        }
        ExprKind::Call {
            callee,
            args,
            filter,
        } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(a, f);
            }
            if let Some(x) = filter {
                walk_expr(x, f);
            }
        }
        ExprKind::Unary { rhs, .. } => walk_expr(rhs, f),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(otherwise, f);
        }
        ExprKind::In { lhs, items, .. } => {
            walk_expr(lhs, f);
            for i in items {
                walk_expr(i, f);
            }
        }
        ExprKind::Exists { query, .. } => walk_expr(query, f),
        ExprKind::Object(entries) => {
            for en in entries {
                if let ObjEntry::Field { value, .. } = en {
                    walk_expr(value, f);
                }
            }
        }
        ExprKind::Array(items) => {
            for i in items {
                walk_expr(i, f);
            }
        }
        ExprKind::OrThrow { value, args, .. } => {
            walk_expr(value, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        ExprKind::CatchPostfix { value, .. } => walk_expr(value, f),
        ExprKind::Cast { value, .. } | ExprKind::WithHeaders { value, .. } => walk_expr(value, f),
        ExprKind::Cookie { value, args } => {
            walk_expr(value, f);
            for a in args {
                walk_expr(a, f);
            }
        }
        _ => {}
    }
}

fn each_expr(b: &Block, f: &mut impl FnMut(&Expr)) {
    walk_block(b, &mut |s| match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::Expr { expr: value, .. } => {
            walk_expr(value, f)
        }
        Stmt::Return { value: Some(v), .. } => walk_expr(v, f),
        Stmt::If { cond, .. } => walk_expr(cond, f),
        Stmt::For { iterable, .. } => walk_expr(iterable, f),
        Stmt::Throw { args, .. } => {
            for a in args {
                walk_expr(a, f);
            }
        }
        Stmt::Assert {
            kind: AssertKind::Expr(e),
            ..
        } => walk_expr(e, f),
        _ => {}
    });
}

/// Errors an expression can raise: `or throw`, and constraint promotion for
/// every table it writes (errors.md §3.1, §6.4). A postfix `catch` removes
/// what it swallows.
fn collect_expr_raises(e: &Expr, sym: &Symbols, out: &mut BTreeSet<String>) {
    let mut caught: HashSet<String> = HashSet::new();
    walk_expr(e, &mut |x| {
        if let ExprKind::CatchPostfix { error, .. } = &*x.kind {
            caught.insert(error.name.clone());
        }
    });
    let mut found = BTreeSet::new();
    walk_expr(e, &mut |x| match &*x.kind {
        ExprKind::OrThrow { error, .. } => {
            found.insert(error.name.clone());
        }
        ExprKind::Insert(i) => {
            // `on conflict do nothing` is exactly the construct that stops a
            // unique violation being raised (writes.md §2.3).
            let suppressed = i.conflict.is_some();
            promote(&i.table, sym, suppressed, &mut found);
        }
        ExprKind::Update(u) => promote(&u.table, sym, false, &mut found),
        ExprKind::Delete(d) => promote(&d.table, sym, false, &mut found),
        _ => {}
    });
    for f in found {
        if !caught.contains(&f) {
            out.insert(f);
        }
    }
}

/// errors.md §6.1 — a constraint carrying a message raises a declared
/// error; a message-less one is a fault and is not tracked.
fn promote(
    table: &QualifiedTable,
    sym: &Symbols,
    suppress_unique: bool,
    out: &mut BTreeSet<String>,
) {
    let Some(name) = sym.by_path.get(&table.text()) else {
        return;
    };
    let Some(t) = sym.tables.get(name) else {
        return;
    };
    if !suppress_unique && t.has_messaged_unique {
        out.insert("Conflict".to_string());
    }
    if t.has_messaged_check {
        out.insert("BadRequest".to_string());
    }
    if t.has_foreign_key {
        out.insert("BadRequest".to_string());
    }
}

/// Own `throw`s, `or throw`s, and the constraints of the tables this body
/// writes — minus anything a postfix `catch` swallows (errors.md §3.1,
/// §6.4).
pub fn direct_raises(b: &Block, sym: &Symbols) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_block(b, &mut |s| match s {
        Stmt::Throw { error, .. } => {
            out.insert(error.name.clone());
        }
        Stmt::Expr { expr, .. } | Stmt::Let { value: expr, .. } => {
            collect_expr_raises(expr, sym, &mut out);
        }
        Stmt::Return {
            value: Some(expr), ..
        } => collect_expr_raises(expr, sym, &mut out),
        _ => {}
    });
    out
}

/// Everything a block can raise, transitively over the call graph.
///
/// The same set `errorHandler` exhaustiveness reads (errors.md §3), exposed
/// because `jwc openapi` answers "which non-2xx responses can this route
/// produce" with exactly it — nothing is discovered at runtime (§6.4).
pub fn raises_from(
    sym: &Symbols,
    bodies: &std::collections::BTreeMap<String, &Block>,
    start: &Block,
) -> BTreeSet<String> {
    let mut out = direct_raises(start, sym);
    for f in reachable_from(bodies, start) {
        if let Some(b) = bodies.get(&f) {
            out.extend(direct_raises(b, sym));
        }
    }
    out
}

/// The body of every named function, keyed the way a call site writes it:
/// `AuthService.login` for a service function, `main` for a bare one.
pub fn function_bodies(ws: &Workspace) -> std::collections::BTreeMap<String, &Block> {
    let mut out = std::collections::BTreeMap::new();
    for file in &ws.files {
        for d in &file.program.decls {
            match d {
                Decl::Function(f) => {
                    out.insert(f.name.name.clone(), &f.body);
                }
                Decl::Service(s) => {
                    for f in &s.functions {
                        out.insert(format!("{}.{}", s.name.name, f.name.name), &f.body);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Every function reachable from `start`, transitively.
///
/// Exact rather than approximate: there are no function values (types §1),
/// so a call site names its callee literally and the graph has no unknown
/// edges. This is the same reachability the raise sets are computed over
/// (errors §3), and `jwc explain --route` reads it to answer "which queries
/// can a request to this route issue" (tooling §1.3).
pub fn reachable_from(
    bodies: &std::collections::BTreeMap<String, &Block>,
    start: &Block,
) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = callees(start).into_iter().collect();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(b) = bodies.get(&name) {
            stack.extend(callees(b));
        }
    }
    seen
}

/// Every table a block writes, as the qualified path the source wrote —
/// `App.org.Orgs`.
///
/// The set a route's constraint report is built from: a write is the only
/// way a constraint can be violated, so the tables reachable from a route
/// bound the statuses that route can produce (errors.md §6.4).
pub fn writes_in(b: &Block) -> BTreeSet<(String, WriteKind)> {
    let mut out = BTreeSet::new();
    each_expr(b, &mut |e| match &*e.kind {
        ExprKind::Insert(i) => {
            out.insert((i.table.text(), WriteKind::Insert));
        }
        ExprKind::Update(u) => {
            out.insert((u.table.text(), WriteKind::Update));
        }
        ExprKind::Delete(d) => {
            out.insert((d.table.text(), WriteKind::Delete));
        }
        _ => {}
    });
    out
}

/// Which statement wrote a table. It decides which constraints can actually
/// raise: an `insert` or `update` can violate the target's own uniques,
/// checks and foreign keys; a `delete` can violate none of them, and can
/// only trip a foreign key **pointing at** the row it removes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum WriteKind {
    Insert,
    Update,
    Delete,
}

/// Every name called anywhere in a block, qualified as written.
pub fn callees(b: &Block) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    each_expr(b, &mut |e| {
        if let ExprKind::Call { callee, .. } = &*e.kind {
            if let Some(p) = path_of(callee) {
                out.insert(p);
            }
        }
    });
    out
}

/// `AuthService.login` from the callee expression of a call.
pub fn path_of(e: &Expr) -> Option<String> {
    match &*e.kind {
        ExprKind::Name(i) => Some(i.name.clone()),
        ExprKind::Field { base, field } => Some(format!("{}.{}", path_of(base)?, field.name)),
        _ => None,
    }
}

/// `context.k` reads, with whether the `?` form was used.
fn context_reads(b: &Block) -> Vec<(String, Span, bool)> {
    let mut out = Vec::new();
    each_expr(b, &mut |e| {
        if let ExprKind::Field { base, field } = &*e.kind {
            if matches!(&*base.kind, ExprKind::Name(n) if n.name == "context") {
                let optional = field.name.ends_with('?');
                out.push((
                    field.name.trim_end_matches('?').to_string(),
                    e.span,
                    optional,
                ));
            }
        }
    });
    out
}

fn context_writes(b: &Block) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    walk_block(b, &mut |s| {
        if let Stmt::Assign {
            target: AssignTarget::Context(k),
            span,
            ..
        } = s
        {
            out.push((k.name.clone(), *span));
        }
    });
    out
}

fn returns_a_response(b: &Block) -> bool {
    let mut ok = false;
    walk_block(b, &mut |s| {
        if let Stmt::Return { value: Some(_), .. } = s {
            ok = true;
        }
    });
    ok
}
