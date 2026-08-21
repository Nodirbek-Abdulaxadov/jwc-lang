//! The v1 type checker.
//!
//! Scope, deliberately: this is the **type** pass (types.md, queries.md,
//! writes.md). Routing, middleware composition and the error model have
//! their own rules and their own release; what is checked here is what the
//! lattice can decide — name resolution, `Raw` vs `Record`, `T?` and its
//! narrowing, spread, signatures, and the expression core.
//!
//! Every diagnostic carries the clause it enforces, so a rejected program
//! points at the sentence that rejected it.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::symbols::{ClassSym, Symbols};
use crate::types::{arith, comparable, orderable, Fields, Scalar, Ty};
use crate::workspace::{Loc, Workspace};
use std::collections::{HashMap, HashSet};

pub struct Checked {
    pub diags: Vec<(Loc, Diagnostic)>,
    /// What each route answers, recorded while the response builders are
    /// typed. `jwc openapi` reads it rather than re-inferring: one type
    /// engine, one answer (tooling.md §5.2).
    pub responses: Vec<RouteResponse>,
    /// `route key -> the class its body validates the request body against`,
    /// from `request.body() as C` (types.md §4.1).
    pub request_bodies: Vec<(String, String)>,
    /// The return type of every function, **inferred** where types.md §10.2
    /// does not require an annotation — which is most of them, since one is
    /// only mandatory when two returns disagree.
    ///
    /// The compiler has always known these; it just had nowhere to publish
    /// them. `jwc openapi` reads them so that a route returning
    /// `json(OrgService.get(...))` documents a shape rather than shrugging.
    pub function_returns: std::collections::BTreeMap<String, Ty>,
}

#[derive(Clone, Debug)]
pub struct RouteResponse {
    /// `GET /api/v1/orgs/{org_id}` — the declared pattern.
    pub route: String,
    pub status: u16,
    /// The payload's type. `Ty::Void` for a bodiless response.
    pub payload: Ty,
    /// The media type of the body, when the route pinned one with
    /// `content(mime, body)`. `None` is `application/json` — the default
    /// every other builder produces (routing.md §7.1).
    pub media: Option<String>,
}

pub fn check(ws: &Workspace, sym: &Symbols, model: &crate::model::SchemaModel) -> Checked {
    check_with(ws, sym, model, &Default::default())
}

/// The same pass, with the return types a previous run inferred.
///
/// One run cannot know the return type of a function it has not reached
/// yet, and reordering would only move the problem. Running twice costs a
/// second pass over the AST and gives every call site the shape its callee
/// actually produces.
pub fn check_with(
    ws: &Workspace,
    sym: &Symbols,
    model: &crate::model::SchemaModel,
    known_returns: &std::collections::BTreeMap<String, Ty>,
) -> Checked {
    let mut c = Checker {
        ws,
        sym,
        model,
        file: 0,
        diags: Vec::new(),
        responses: Vec::new(),
        request_bodies: Vec::new(),
        function_returns: Default::default(),
        known_returns: known_returns.clone(),
        route: None,
        fn_key: None,
        scopes: Vec::new(),
        params: HashMap::new(),
        query: Vec::new(),
        scoped_to: None,
        tainted: HashSet::new(),
        path_keyed: HashSet::new(),
        saw_request_path: false,
        password_hashed: HashSet::new(),
        saw_password_hash: false,
        untyped_params: HashSet::new(),
        saw_private: false,
        body: BodyKind::Free,
        loop_depth: 0,
        current_fn: None,
        returns: Vec::new(),
    };
    c.run();
    Checked {
        diags: c.diags,
        responses: c.responses,
        request_bodies: c.request_bodies,
        function_returns: c.function_returns,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BodyKind {
    Free,
    Service,
    Route,
    Middleware,
    After,
    ErrorHandler,
    Test,
    View,
}

/// One binding inside a query: `select A from …` or `left join … B on …`.
struct Binding {
    name: String,
    /// Declared table or view name.
    object: String,
    /// The projection field an `as one` join produces. The binding and the
    /// field are two names for the same row, and `orderby org.name` is
    /// written with the field because that is what the projection calls it
    /// (queries.md §5.4). `as many` is not here: a collection has no
    /// single row to reach through.
    one_field: Option<String>,
}

struct QueryScope {
    bindings: Vec<Binding>,
}

struct Checker<'a> {
    ws: &'a Workspace,
    sym: &'a Symbols,
    model: &'a crate::model::SchemaModel,
    file: usize,
    diags: Vec<(Loc, Diagnostic)>,
    responses: Vec<RouteResponse>,
    request_bodies: Vec<(String, String)>,
    function_returns: std::collections::BTreeMap<String, Ty>,
    known_returns: std::collections::BTreeMap<String, Ty>,
    /// The route being checked, as its declared pattern. `None` outside one.
    route: Option<String>,
    /// The function being checked, keyed the way a call site writes it.
    fn_key: Option<String>,
    scopes: Vec<HashMap<String, Ty>>,
    params: HashMap<String, Ty>,
    query: Vec<QueryScope>,
    /// While set, an unqualified identifier resolves against this object
    /// only. Used for projections and for a join result's own
    /// `orderby`/`limit` (queries.md §6.1, §4.6).
    scoped_to: Option<String>,
    /// Locals holding a value projected from a `private` column. Legal to
    /// read in code, never legal in a response (schema.md §3.1).
    tainted: HashSet<String>,
    /// Locals whose value came from `request.path()`, for W0602.
    path_keyed: HashSet<String>,
    saw_request_path: bool,
    /// Locals holding a `hash.password(...)` result, for W1201.
    password_hashed: HashSet<String>,
    saw_password_hash: bool,
    /// Path parameters this `routes` block declared with no type.
    untyped_params: HashSet<String>,
    /// Set while checking a projection; records that it named a private
    /// column.
    saw_private: bool,
    /// How many `for` bodies enclose the statement being checked. `break`
    /// and `continue` need one (errors.md §7.2).
    loop_depth: u32,
    body: BodyKind,
    current_fn: Option<String>,
    returns: Vec<(Ty, Span)>,
}

impl<'a> Checker<'a> {
    // ------------------------------------------------------------ plumbing

    fn err(
        &mut self,
        span: Span,
        code: &'static str,
        msg: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            Loc {
                file: self.file,
                span,
            },
            Diagnostic::error(code, span, msg).clause(clause),
        ));
    }

    fn err_note(
        &mut self,
        span: Span,
        code: &'static str,
        msg: impl Into<String>,
        note: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            Loc {
                file: self.file,
                span,
            },
            Diagnostic::error(code, span, msg).note(note).clause(clause),
        ));
    }

    fn warn(
        &mut self,
        span: Span,
        code: &'static str,
        msg: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            Loc {
                file: self.file,
                span,
            },
            Diagnostic::warning(code, span, msg).clause(clause),
        ));
    }

    /// tooling.md §5.2 — what this route answers, recorded where the type
    /// is already known. Outside a route there is nothing to attach it to.
    fn record_response(&mut self, status: u16, payload: Ty) {
        self.record_response_as(status, payload, None);
    }

    fn record_response_as(&mut self, status: u16, payload: Ty, media: Option<String>) {
        let Some(route) = &self.route else { return };
        self.responses.push(RouteResponse {
            route: route.clone(),
            status,
            payload,
            media,
        });
    }

    fn warn_note(
        &mut self,
        span: Span,
        code: &'static str,
        msg: impl Into<String>,
        note: impl Into<String>,
        clause: &'static str,
    ) {
        self.diags.push((
            Loc {
                file: self.file,
                span,
            },
            Diagnostic::warning(code, span, msg)
                .note(note)
                .clause(clause),
        ));
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, ty: Ty, span: Span) {
        if self.lookup(name).is_some() || self.params.contains_key(name) {
            self.err_note(
                span,
                "E0214",
                format!("`{name}` is already in scope"),
                "shadowing is not permitted; pick a different name",
                "names.md §5.5",
            );
        }
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), ty);
        }
    }

    /// Declare without the shadowing check — for a narrowed rebind.
    fn rebind(&mut self, name: &str, ty: Ty) {
        for s in self.scopes.iter_mut().rev() {
            if s.contains_key(name) {
                s.insert(name.to_string(), ty);
                return;
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        for s in self.scopes.iter().rev() {
            if let Some(t) = s.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn in_query(&self) -> bool {
        !self.query.is_empty()
    }

    // ------------------------------------------------------------ driver

    fn run(&mut self) {
        for fi in 0..self.ws.files.len() {
            self.file = fi;
            let decls = self.ws.files[fi].program.decls.clone();
            for d in &decls {
                self.decl(d);
            }
        }
    }

    fn decl(&mut self, d: &Decl) {
        match d {
            Decl::Function(f) => self.function(f, BodyKind::Free, None),
            Decl::Service(s) => {
                for f in &s.functions {
                    self.function(f, BodyKind::Service, Some(s.name.name.clone()));
                }
            }
            Decl::Middleware(m) => self.middleware(m),
            Decl::Routes(r) => self.routes(r),
            Decl::ErrorHandler(h) => self.error_handler(h),
            Decl::Test(t) => {
                self.body = BodyKind::Test;
                self.enter_body();
                self.push_scope();
                self.block(&t.body);
                self.pop_scope();
            }
            Decl::View(v) => self.view(v),
            Decl::Class(_) | Decl::Table(_) | Decl::Enum(_) => {}
            _ => {}
        }
    }

    fn function(&mut self, f: &FunctionDecl, kind: BodyKind, service: Option<String>) {
        self.body = kind;
        self.current_fn = Some(f.name.name.clone());
        self.fn_key = Some(match &service {
            Some(s) => format!("{s}.{}", f.name.name),
            None => f.name.name.clone(),
        });
        self.returns.clear();
        self.push_scope();

        for p in &f.params {
            // types.md §10.1 — annotations are mandatory, and the parser
            // already requires one, so this only resolves the name.
            let ty = self.resolve_type(&p.ty);
            self.declare(&p.name.name, ty, p.name.span);
        }

        // errors.md §3.3 — only a package boundary may write `raises`.
        if !f.raises.is_empty() && service.is_none() {
            self.err_note(
                f.span,
                "E1003",
                "`raises` may not be written in application code",
                "the compiler infers the raise set; a hand-written one drifts",
                "errors.md §3.3",
            );
        }

        self.block(&f.body);
        self.record_return_type();
        self.check_return_shapes(f);
        self.pop_scope();
        self.current_fn = None;
        self.fn_key = None;
    }

    /// The shape this function produces, for the next pass to hand to its
    /// callers. An annotation wins; otherwise the returns agree by §10.2, so
    /// the first of them is the answer.
    fn record_return_type(&mut self) {
        let Some(key) = self.fn_key.clone() else {
            return;
        };
        let ty = match self.sym.functions.get(&key).and_then(|f| f.returns.clone()) {
            Some(t) => t,
            None => match self.returns.first() {
                Some((t, _)) => t.clone(),
                None => return,
            },
        };
        self.function_returns.insert(key, ty);
    }

    /// types.md §10.2 — a return annotation is mandatory when two returns
    /// produce shapes that are not mutually assignable.
    fn check_return_shapes(&mut self, f: &FunctionDecl) {
        if f.returns.is_some() || self.returns.len() < 2 {
            if let Some(ann) = &f.returns {
                let want = self.resolve_type(ann);
                let returns = std::mem::take(&mut self.returns);
                for (got, span) in &returns {
                    if !got.assignable_to(&want) {
                        self.err_note(
                            *span,
                            "E0351",
                            format!("returns `{got}` where the signature says `{want}`"),
                            "the annotation and the body disagree",
                            "types.md §10.2",
                        );
                    }
                }
            }
            self.returns.clear();
            return;
        }
        let returns = std::mem::take(&mut self.returns);
        let first = returns[0].0.clone();
        for (ty, span) in returns.iter().skip(1) {
            if !ty.assignable_to(&first) && !first.assignable_to(ty) {
                self.err_note(
                    *span,
                    "E0351",
                    format!("this returns `{ty}` but an earlier return produces `{first}`"),
                    "when returns disagree the signature must say which one wins: \
                     add `-> <type>`",
                    "types.md §10.2",
                );
                break;
            }
        }
    }

    fn middleware(&mut self, m: &MiddlewareDecl) {
        self.body = BodyKind::Middleware;
        self.enter_body();
        self.params.clear();
        for b in &m.binders {
            let ty = self.resolve_type(&b.ty);
            self.params.insert(b.name.name.clone(), ty);
        }
        self.push_scope();
        self.block(&m.body);
        self.pop_scope();

        if let Some(after) = &m.after {
            self.body = BodyKind::After;
            self.enter_body();
            self.push_scope();
            self.block(after);
            self.pop_scope();
        }
        self.params.clear();
        self.body = BodyKind::Free;
    }

    fn routes(&mut self, r: &RoutesDecl) {
        let prefix_params = path_params(&r.prefix);
        for route in &r.routes {
            self.route = Some(format!(
                "{} {}",
                route.method.name.to_uppercase(),
                crate::wiring::route_pattern(&r.prefix, &route.suffix)
            ));
            self.body = BodyKind::Route;
            self.enter_body();
            self.params.clear();
            self.untyped_params = untyped_path_params(&r.prefix)
                .into_iter()
                .chain(untyped_path_params(&route.suffix))
                .collect();
            for (name, ty) in prefix_params
                .iter()
                .chain(path_params(&route.suffix).iter())
            {
                self.params.insert(name.clone(), ty.clone());
            }
            self.push_scope();
            self.block(&route.body);
            self.pop_scope();
            self.untyped_params.clear();
            self.route = None;
            // routing.md §6.4 — every path ends in a response. A body that
            // can fall off the end has no answer to send, and the runtime's
            // only recourse is a 204 nobody asked for.
            if !diverges(&route.body) {
                self.err_note(
                    route.span,
                    "E0731",
                    format!(
                        "`{} {}` has a path that does not return a response",
                        route.method.name.to_uppercase(),
                        crate::wiring::render(&crate::wiring::parse_path(&format!(
                            "{}/{}",
                            r.prefix.trim_end_matches('/'),
                            route.suffix.trim_start_matches('/')
                        )))
                    ),
                    "every path through a route body ends in `return <response>` or \
                     `throw`",
                    "routing.md §6.4",
                );
            }
        }
        self.params.clear();
        self.body = BodyKind::Free;
    }

    fn error_handler(&mut self, h: &ErrorHandlerDecl) {
        for arm in &h.arms {
            self.body = BodyKind::ErrorHandler;
            self.enter_body();
            self.push_scope();
            let ty = match &arm.error {
                Some(name) => match self.sym.errors.get(&name.name) {
                    Some(e) => Ty::Record(e.params.clone()),
                    None => {
                        self.err_note(
                            name.span,
                            "E1001",
                            format!("unknown error type `{}`", name.name),
                            "declare it: `error MyError(...) = <status>;`",
                            "errors.md §1.3",
                        );
                        Ty::Unknown
                    }
                },
                // The untyped arm catches faults, which carry a message and
                // nothing else (errors.md §4.4).
                None => Ty::Record(vec![("message".into(), Ty::text())]),
            };
            self.declare(&arm.binder.name, ty, arm.binder.span);
            self.block(&arm.body);
            self.pop_scope();
        }
        self.body = BodyKind::Free;
    }

    /// queries.md §8.1 — a view body must carry a projection and may not
    /// carry a per-query clause.
    fn view(&mut self, v: &ViewDecl) {
        self.body = BodyKind::View;
        self.enter_body();
        if v.body.projection.is_none() {
            self.err_note(
                v.span,
                "E0540",
                format!("view `{}` has no `as {{ }}` projection", v.name.name),
                "a view is a named projection — that is what makes selecting from \
                 one produce a record rather than a raw result",
                "queries.md §8.1",
            );
        }
        let mut offenders: Vec<&str> = Vec::new();
        if v.body.filter.is_some() {
            offenders.push("where");
        }
        if v.body.first {
            offenders.push("first");
        }
        if v.body.limit.is_some() {
            offenders.push("limit");
        }
        if v.body.page.is_some() {
            offenders.push("page");
        }
        if !v.body.order_by.is_empty() {
            offenders.push("orderby");
        }
        if !offenders.is_empty() {
            let list = offenders.join("`, `");
            self.err_note(
                v.span,
                "E0541",
                format!("view `{}` carries `{list}`", v.name.name),
                "those belong to the query that selects *from* the view",
                "queries.md §8.1",
            );
        }
        self.push_scope();
        self.select(&v.body, v.span);
        self.pop_scope();
        self.body = BodyKind::Free;
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self, b: &Block) {
        self.push_scope();
        for s in b {
            self.stmt(s);
        }
        self.pop_scope();
    }

    /// A body's taint sets are its own. A local named `key` in one
    /// middleware is not the `key` in the next, and carrying the set over
    /// made the second one inherit the first one's answer.
    fn enter_body(&mut self) {
        self.tainted.clear();
        self.path_keyed.clear();
        self.password_hashed.clear();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Break { span, .. } | Stmt::Continue { span, .. } => {
                if self.loop_depth == 0 {
                    let word = match s {
                        Stmt::Break { .. } => "break",
                        _ => "continue",
                    };
                    self.err_note(
                        *span,
                        "E0813",
                        format!("`{word}` outside a `for` loop"),
                        "there is no loop to leave; `return` or `throw` leaves the \
                         function",
                        "errors.md §7.2",
                    );
                }
            }
            Stmt::Let {
                name, ty, value, ..
            } => {
                self.saw_private = false;
                self.saw_request_path = false;
                let got = self.expr(value);
                if self.saw_private {
                    self.tainted.insert(name.name.clone());
                    self.saw_private = false;
                }
                if self.saw_request_path {
                    self.path_keyed.insert(name.name.clone());
                    self.saw_request_path = false;
                }
                if self.saw_password_hash {
                    self.password_hashed.insert(name.name.clone());
                    self.saw_password_hash = false;
                }
                if let Some(ann) = ty {
                    let want = self.resolve_type(ann);
                    if !got.assignable_to(&want) {
                        self.err(
                            value.span,
                            "E0352",
                            format!("`{got}` is not assignable to `{want}`"),
                            "types.md §10.3",
                        );
                    }
                    self.declare(&name.name, want, name.span);
                } else {
                    if matches!(got, Ty::Void) {
                        self.err_note(
                            value.span,
                            "E0602",
                            "this produces no value",
                            "a write with no `as { }` returns nothing — add a projection",
                            "writes.md §1.2",
                        );
                    }
                    self.declare(&name.name, got, name.span);
                }
            }
            Stmt::Assign { target, value, .. } => {
                let got = self.expr(value);
                match target {
                    AssignTarget::Local(i) => match self.lookup(&i.name) {
                        Some(want) => {
                            if !got.assignable_to(&want) {
                                self.err(
                                    value.span,
                                    "E0352",
                                    format!("`{got}` is not assignable to `{want}`"),
                                    "types.md §10.3",
                                );
                            }
                            // Assignment resets narrowing (types.md §6.6.5).
                            self.rebind(&i.name, want);
                        }
                        None => self.err_note(
                            i.span,
                            "E0211",
                            format!("unknown local `${}`", i.name),
                            "declare it with `let` first",
                            "names.md §5.5",
                        ),
                    },
                    AssignTarget::Context(_) => {
                        // middleware.md §6.4 is a v0.24 rule; the value is
                        // still type-checked above.
                    }
                }
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                ..
            } => {
                let ct = self.expr(cond);
                self.require_boolean(&ct, cond.span);

                // types.md §6.6 rule 2 — narrow inside the then-branch.
                let narrowed = narrowing_target(cond, false);
                self.push_scope();
                if let Some(name) = &narrowed {
                    if let Some(t) = self.lookup(name) {
                        self.scopes
                            .last_mut()
                            .expect("scope")
                            .insert(name.clone(), t.strip_opt());
                    }
                }
                for s in then {
                    self.stmt(s);
                }
                self.pop_scope();

                if let Some(alt) = otherwise {
                    self.block(alt);
                }

                // rule 1 — a divergent null-guard narrows after the `if`.
                if let Some(name) = narrowing_target(cond, true) {
                    if diverges(then) {
                        if let Some(t) = self.lookup(&name) {
                            self.rebind(&name, t.strip_opt());
                        }
                    }
                }
            }
            Stmt::For {
                binder,
                iterable,
                body,
                ..
            } => {
                let it = self.expr(iterable);
                let elem = match it.element() {
                    Some(e) => e.clone(),
                    None => {
                        if !matches!(it, Ty::Unknown) {
                            self.err_note(
                                iterable.span,
                                "E0372",
                                format!("`for` needs an array, found `{it}`"),
                                "iterate a class field declared `T[]`, or a query result",
                                "types.md §12.5",
                            );
                        }
                        Ty::Unknown
                    }
                };
                if elem.is_raw() {
                    self.err_note(
                        iterable.span,
                        "E0311",
                        "cannot iterate a raw result",
                        "add an `as { … }` projection to the query",
                        "types.md §5.4",
                    );
                }
                self.push_scope();
                self.declare(&binder.name, elem, binder.span);
                self.loop_depth += 1;
                for s in body {
                    self.stmt(s);
                }
                self.loop_depth -= 1;
                self.pop_scope();
            }
            Stmt::Return { value, span, .. } => {
                let ty = match value {
                    Some(v) => self.expr(v),
                    None => Ty::Void,
                };
                if self.body == BodyKind::After && value.is_some() {
                    self.err_note(
                        *span,
                        "E0810",
                        "`return <value>` inside an `after` block",
                        "an `after` block cannot produce a response; bare `return;` \
                         ends the block",
                        "middleware.md §5.3",
                    );
                }
                // routing.md §6.4 — `return $account;` is the mistake; the
                // fix is `return json($account);`. Void is left to E0731,
                // which is about the path not ending in a response at all.
                if self.body == BodyKind::Route
                    && value.is_some()
                    && !matches!(ty, Ty::Response | Ty::Unknown | Ty::Void)
                {
                    self.err_note(
                        *span,
                        "E0732",
                        format!("a route returned `{ty}`, not a response"),
                        "wrap it: `return json(...)`, `created(...)`, `noContent()`",
                        "routing.md §6.4",
                    );
                }
                // middleware.md §5.2 — `return` from a middleware is for
                // deliberately non-error responses: a redirect, a 304, a
                // 202. An error that is returned rather than thrown skips
                // the error handler, so it answers with a different body
                // than every other error in the program.
                if self.body == BodyKind::Middleware {
                    if let Some(v) = value {
                        if let Some(name) = error_builder(v) {
                            self.warn(
                                *span,
                                "W0801",
                                format!("a middleware returned `{name}(...)` instead of throwing"),
                                "middleware.md §5.2",
                            );
                        }
                    }
                }
                if self.body == BodyKind::Service && matches!(ty, Ty::Response) {
                    self.err_note(
                        *span,
                        "E0330",
                        "a service returned a response",
                        "services do not know HTTP: `throw NotFound(\"…\")` and let the \
                         error type's status decide",
                        "types.md §8",
                    );
                }
                if let Some(v) = value {
                    self.returns.push((ty, v.span));
                }
            }
            Stmt::Throw {
                error, args, span, ..
            } => {
                let Some(sym) = self.sym.errors.get(&error.name).cloned() else {
                    self.err_note(
                        error.span,
                        "E1001",
                        format!("unknown error type `{}`", error.name),
                        "declare it: `error MyError(...) = <status>;` — `throw` never \
                         invents a type",
                        "errors.md §1.3",
                    );
                    for a in args {
                        self.expr(a);
                    }
                    return;
                };
                if args.len() != sym.params.len() {
                    self.err_note(
                        *span,
                        "E1004",
                        format!(
                            "`{}` takes {} argument(s), given {}",
                            error.name,
                            sym.params.len(),
                            args.len()
                        ),
                        format!(
                            "declared as ({})",
                            sym.params
                                .iter()
                                .map(|(n, t)| format!("{n}: {t}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        "errors.md §1.1",
                    );
                }
                for (i, a) in args.iter().enumerate() {
                    let got = self.expr(a);
                    if let Some((_, want)) = sym.params.get(i) {
                        if !got.assignable_to(want) {
                            self.err(
                                a.span,
                                "E1005",
                                format!("`{got}` is not assignable to `{want}`"),
                                "errors.md §1.1",
                            );
                        }
                    }
                }
            }
            Stmt::Transaction { body, span, .. } => {
                if !matches!(self.body, BodyKind::Service | BodyKind::Test) {
                    self.err_note(
                        *span,
                        "E0621",
                        "`transaction` outside a service",
                        "a transaction spanning a route or middleware would hold a \
                         connection across the whole request",
                        "writes.md §7.4",
                    );
                }
                self.block(body);
            }
            Stmt::Assert { kind, span, .. } => match kind {
                AssertKind::Expr(e) => {
                    let t = self.expr(e);
                    self.require_boolean(&t, e.span);
                }
                AssertKind::Fails { error, body, .. } => {
                    match error {
                        Some(name) => {
                            if !self.sym.errors.contains_key(&name.name) {
                                self.err(
                                    name.span,
                                    "E1001",
                                    format!("unknown error type `{}`", name.name),
                                    "errors.md §1.3",
                                );
                            }
                        }
                        // testing.md §4.1 — an untyped `assert fails` passes
                        // when a typo makes the block raise something
                        // unrelated, which is the assertion testing itself
                        // rather than the program.
                        None => self.err_note(
                            *span,
                            "E1401",
                            "`assert fails` needs the error type it expects",
                            "write `assert fails Conflict { … }`; the predeclared \
                             types are in errors.md §1.2",
                            "testing.md §4.1",
                        ),
                    }
                    self.block(body);
                }
            },
            Stmt::Expr { expr, .. } => {
                self.expr(expr);
            }
        }
    }

    fn require_boolean(&mut self, ty: &Ty, span: Span) {
        if matches!(ty, Ty::Unknown) {
            return;
        }
        if ty.is_optional() {
            self.err_note(
                span,
                "E0320",
                format!("condition is `{ty}` and may be null"),
                "compare it: `if ($x != null)`",
                "types.md §6.4",
            );
            return;
        }
        if !matches!(ty, Ty::Scalar(Scalar::Boolean)) {
            self.err_note(
                span,
                "E0371",
                format!("condition is `{ty}`, not `boolean`"),
                "there is no truthiness: write an explicit comparison",
                "types.md §12.4",
            );
        }
    }

    fn resolve_type(&mut self, t: &TypeRef) -> Ty {
        let ty = crate::symbols::type_of(t, &self.sym.enums, &self.sym.classes);
        // `type_of` optimistically calls an unknown name a class; report it
        // here, where a span is available.
        if let Ty::Class(name) = base_of(&ty) {
            if !self.sym.classes.contains_key(&name)
                && !self.sym.views.contains_key(&name)
                && !self.sym.enums.contains_key(&name)
            {
                self.err_note(
                    t.span,
                    "E0301",
                    format!("unknown type `{name}`"),
                    "a type is a scalar (types.md §2.1), a declared `enum`, or a \
                     declared `class`",
                    "types.md §2.1",
                );
                return Ty::Unknown;
            }
        }
        ty
    }

    // ------------------------------------------------------------ expressions

    fn expr(&mut self, e: &Expr) -> Ty {
        match &*e.kind {
            // types.md §2.2 — `int` if it fits, else `bigint`. Past
            // `bigint` there is no type to give it: Postgres has none, so
            // the literal cannot round-trip and the program is wrong here
            // rather than at the first write.
            ExprKind::Int(n) => match n.parse::<i64>() {
                Ok(v) if i32::try_from(v).is_ok() => Ty::int(),
                Ok(_) => Ty::bigint(),
                Err(_) => {
                    self.err_note(
                        e.span,
                        "E0107",
                        format!("`{n}` is out of `bigint` range"),
                        "the widest integer is bigint: -9223372036854775808 … \
                         9223372036854775807",
                        "types.md §2.2",
                    );
                    Ty::bigint()
                }
            },
            ExprKind::Decimal(_) => Ty::numeric(),
            ExprKind::Str(_) | ExprKind::RawStr(_) => Ty::text(),
            ExprKind::Bool(_) => Ty::boolean(),
            ExprKind::Null => Ty::Null,

            ExprKind::Local(i) => match self.lookup(&i.name) {
                Some(t) => t,
                None => {
                    self.err_note(
                        i.span,
                        "E0211",
                        format!("unknown local `${}`", i.name),
                        "declare it with `let`, or take it as a parameter",
                        "names.md §5.3",
                    );
                    Ty::Unknown
                }
            },

            ExprKind::PathParam(i) => {
                if !matches!(
                    self.body,
                    BodyKind::Route | BodyKind::Middleware | BodyKind::After
                ) {
                    self.err_note(
                        i.span,
                        "E0220",
                        format!("`@{}` outside a route or middleware", i.name),
                        "path parameters exist only where a path does",
                        "names.md §5.2",
                    );
                    return Ty::Unknown;
                }
                match self.params.get(&i.name) {
                    Some(t) => t.clone(),
                    None => {
                        self.err_note(
                            i.span,
                            "E0801",
                            format!("`@{}` is not declared here", i.name),
                            "a route declares it in its path (`{id: bigint}`); a \
                             middleware declares it as a binder (`middleware M(@id: bigint)`)",
                            "middleware.md §2",
                        );
                        Ty::Unknown
                    }
                }
            }

            ExprKind::Name(i) => self.name(i),

            ExprKind::Field { base, field } => self.field(base, field, e.span),

            ExprKind::Index { base, index } => {
                let b = self.expr(base);
                let i = self.expr(index);
                if !matches!(i, Ty::Unknown) && i.scalar().is_none_or(|s| !s.is_numeric()) {
                    self.err(
                        index.span,
                        "E0373",
                        format!("index is `{i}`, not a number"),
                        "types.md §12.6",
                    );
                }
                if b.is_raw() {
                    self.err_note(
                        base.span,
                        "E0310",
                        "cannot index a raw result",
                        "add an `as { … }` projection to the query",
                        "types.md §5.2",
                    );
                    return Ty::Unknown;
                }
                b.element().cloned().unwrap_or(Ty::Unknown)
            }

            ExprKind::Call {
                callee,
                args,
                filter,
            } => self.call(callee, args, filter.as_ref(), e.span),

            ExprKind::Unary { op, rhs } => {
                let t = self.expr(rhs);
                match op {
                    UnaryOp::Not => {
                        self.require_boolean(&t, rhs.span);
                        Ty::boolean()
                    }
                    UnaryOp::Neg => {
                        if !matches!(t, Ty::Unknown) && t.scalar().is_none_or(|s| !s.is_numeric()) {
                            self.err(
                                rhs.span,
                                "E0370",
                                format!("cannot negate `{t}`"),
                                "types.md §12.2",
                            );
                        }
                        t
                    }
                }
            }

            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, e.span),

            ExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => {
                let c = self.expr(cond);
                self.require_boolean(&c, cond.span);
                let a = self.expr(then);
                let b = self.expr(otherwise);
                if a.assignable_to(&b) {
                    b
                } else if b.assignable_to(&a) {
                    a
                } else {
                    self.err_note(
                        e.span,
                        "E0374",
                        format!("branches produce `{a}` and `{b}`"),
                        "both arms of `? :` must produce the same type",
                        "types.md §12",
                    );
                    Ty::Unknown
                }
            }

            ExprKind::Coalesce { lhs, rhs } => {
                let a = self.expr(lhs);
                let b = self.expr(rhs);
                if !a.is_optional() && !matches!(a, Ty::Unknown) {
                    self.warn(
                        lhs.span,
                        "W0301",
                        format!("`{a}` is never null, so `??` is dead"),
                        "types.md §6.6",
                    );
                }
                let stripped = a.strip_opt();
                if b.is_optional() {
                    stripped.opt()
                } else {
                    stripped
                }
            }

            ExprKind::In { lhs, items, .. } => {
                let l = self.expr(lhs);
                for i in items {
                    let it = self.expr(i);
                    // A single array operand is the `= ANY($1)` form.
                    let elem = it.element().cloned().unwrap_or(it);
                    if !comparable(&l, &elem) {
                        self.err(
                            i.span,
                            "E0375",
                            format!("`{elem}` cannot be compared with `{l}`"),
                            "queries.md §3.3",
                        );
                    }
                }
                Ty::boolean()
            }

            ExprKind::Exists { query, .. } => {
                self.expr(query);
                Ty::boolean()
            }

            ExprKind::Object(entries) => self.object(entries, e.span),

            ExprKind::Array(items) => {
                let mut elem = Ty::Unknown;
                for i in items {
                    let t = self.expr(i);
                    if matches!(elem, Ty::Unknown) {
                        elem = t;
                    }
                }
                elem.array()
            }

            ExprKind::Select(s) => self.select(s, e.span),
            ExprKind::Insert(i) => self.insert(i, e.span),
            ExprKind::Update(u) => self.update(u, e.span),
            ExprKind::Delete(d) => self.delete(d, e.span),

            ExprKind::OrThrow { value, error, args } => {
                let t = self.expr(value);
                if !self.sym.errors.contains_key(&error.name) {
                    self.err_note(
                        error.span,
                        "E1001",
                        format!("unknown error type `{}`", error.name),
                        "declare it: `error MyError(...) = <status>;`",
                        "errors.md §1.3",
                    );
                }
                for a in args {
                    self.expr(a);
                }
                if !t.is_optional() && !matches!(t, Ty::Unknown) {
                    self.warn(
                        value.span,
                        "W1002",
                        format!("`{t}` is never null, so `or throw` never fires"),
                        "errors.md §5.2",
                    );
                }
                t.strip_opt()
            }

            ExprKind::CatchPostfix {
                value,
                error,
                binder,
                body,
            } => {
                let t = self.expr(value);
                let arm_ty = match self.sym.errors.get(&error.name) {
                    Some(sym) => Ty::Record(sym.params.clone()),
                    None => {
                        self.err(
                            error.span,
                            "E1001",
                            format!("unknown error type `{}`", error.name),
                            "errors.md §1.3",
                        );
                        Ty::Unknown
                    }
                };
                self.push_scope();
                self.declare(&binder.name, arm_ty, binder.span);
                for s in body {
                    self.stmt(s);
                }
                self.pop_scope();
                if !diverges(body) {
                    self.err_note(
                        e.span,
                        "E1020",
                        "a postfix `catch` block must diverge",
                        "end every path in `return`, `throw`, `break` or `continue` — \
                         it handles and leaves, it cannot substitute a value",
                        "errors.md §7.2",
                    );
                }
                t
            }

            ExprKind::Cast { value, ty } => {
                // `request.body() as C` is one construct: the cast is what
                // gives the body a shape, so the inner call is not evaluated
                // on its own (that is what E0720 reports).
                let is_body = matches!(&*value.kind, ExprKind::Call { callee, .. }
                    if callee_path(callee).as_deref() == Some("request.body"));
                if !is_body {
                    self.expr(value);
                }
                match self.sym.classes.get(&ty.name) {
                    Some(_) => {
                        if is_body {
                            if let Some(route) = &self.route {
                                self.request_bodies.push((route.clone(), ty.name.clone()));
                            }
                        }
                        Ty::Class(ty.name.clone())
                    }
                    None => {
                        self.err_note(
                            ty.span,
                            "E0301",
                            format!("`{}` is not a declared class", ty.name),
                            "`request.body() as C` validates against a `class` — only \
                             classes describe input (types.md §4.1)",
                            "routing.md §5.2",
                        );
                        Ty::Unknown
                    }
                }
            }

            ExprKind::WithHeaders { value, headers } => {
                let t = self.expr(value);
                let mut seen: HashSet<String> = HashSet::new();
                for h in headers {
                    if let ObjEntry::Field {
                        key, value, span, ..
                    } = h
                    {
                        if !seen.insert(key.name.to_lowercase()) {
                            self.err_note(
                                *span,
                                "E0730",
                                format!("`{}` appears twice in one `with {{ }}`", key.name),
                                "a repeated header needs `cookie(...)` or \
                                 `response.add_header(...)`",
                                "routing.md §6.2",
                            );
                        }
                        let vt = self.expr(value);
                        if !vt.assignable_to(&Ty::text()) && !matches!(vt, Ty::Unknown) {
                            self.err(
                                value.span,
                                "E0733",
                                format!("header value is `{vt}`, not text"),
                                "routing.md §6.2",
                            );
                        }
                    }
                }
                t
            }

            ExprKind::Cookie { value, args } => {
                let t = self.expr(value);
                for a in args {
                    self.expr(a);
                }
                t
            }
        }
    }

    /// A bare identifier. Inside a query it is a column and nothing else
    /// (names.md §5.3); outside it is a declaration name.
    fn name(&mut self, i: &Ident) -> Ty {
        if self.in_query() {
            return self.column(i);
        }
        // Enum / service / builtin namespaces resolve through `Field`, so a
        // bare name outside a query is either a declaration or a mistake.
        if self.sym.enums.contains_key(&i.name)
            || self.sym.classes.contains_key(&i.name)
            || self.sym.services.contains_key(&i.name)
            || self.sym.tables.contains_key(&i.name)
            || self.sym.views.contains_key(&i.name)
            || is_namespace(&i.name)
        {
            return Ty::Unknown;
        }
        if self.lookup(&i.name).is_some() {
            self.err_note(
                i.span,
                "E0211",
                format!("`{}` is a local; write `${}`", i.name, i.name),
                "every reference to a local carries `$`",
                "names.md §5.3",
            );
            return self.lookup(&i.name).unwrap_or(Ty::Unknown);
        }
        // `now()` gets its own message: it is a column default, not a call.
        if i.name == "now" {
            self.err_note(
                i.span,
                "E0302",
                "`now()` is a column default, not an application call",
                "in code write `date.now()`; `default now()` is the Postgres clock \
                 and they are different values",
                "types.md §2.4",
            );
            return Ty::Unknown;
        }
        Ty::Unknown
    }

    /// Resolve an unqualified column across the bindings in scope.
    fn column(&mut self, i: &Ident) -> Ty {
        if let Some(object) = self.scoped_to.clone() {
            return match self.column_of(&object, &i.name) {
                Some(t) => t,
                None => {
                    self.err_note(
                        i.span,
                        "E0211",
                        format!("`{}` is not a column of `{object}`", i.name),
                        "here an unqualified name is a column of this binding; \
                         qualify it (`B.col`) to reach a joined one",
                        "queries.md §6.1",
                    );
                    Ty::Unknown
                }
            };
        }
        let mut hits: Vec<(String, Ty)> = Vec::new();
        let Some(scope) = self.query.last() else {
            return Ty::Unknown;
        };
        for b in &scope.bindings {
            if let Some(ty) = self.column_of(&b.object, &i.name) {
                hits.push((b.name.clone(), ty));
            }
        }
        match hits.len() {
            1 => hits.remove(0).1,
            0 => {
                // The characteristic slip: a local written without its sigil
                // (names.md §5.3).
                if self.lookup(&i.name).is_some() || self.params.contains_key(&i.name) {
                    self.err_note(
                        i.span,
                        "E0210",
                        format!("`{}` is not a column of any binding here", i.name),
                        format!("did you mean `${}`?", i.name),
                        "names.md §5.3",
                    );
                } else if self.sym.enums.contains_key(&i.name) || is_namespace(&i.name) {
                    return Ty::Unknown;
                } else {
                    let bindings = scope
                        .bindings
                        .iter()
                        .map(|b| b.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.err_note(
                        i.span,
                        "E0211",
                        format!("`{}` is not a column of any binding here", i.name),
                        format!("bindings in scope: {bindings}"),
                        "names.md §5.3",
                    );
                }
                Ty::Unknown
            }
            _ => {
                let names = hits
                    .iter()
                    .map(|(b, _)| format!("`{b}.{}`", i.name))
                    .collect::<Vec<_>>()
                    .join(" and ");
                self.err_note(
                    i.span,
                    "E0213",
                    format!("`{}` is ambiguous", i.name),
                    format!("it resolves to {names} — qualify it"),
                    "names.md §5.4",
                );
                hits.remove(0).1
            }
        }
    }

    fn column_of(&self, object: &str, column: &str) -> Option<Ty> {
        if let Some(t) = self.sym.tables.get(object) {
            if t.is_private(column) {
                // Legal in a predicate, rejected in a projection — the
                // projection path checks separately (schema.md §3.1).
                return t.column(column).cloned();
            }
            return t.column(column).cloned();
        }
        if let Some(v) = self.sym.views.get(object) {
            return v
                .shape
                .iter()
                .find(|(n, _)| n == column)
                .map(|(_, t)| t.clone());
        }
        None
    }

    fn field(&mut self, base: &Expr, field: &Ident, span: Span) -> Ty {
        // `Enum.member`, `Service.method`, `date.now`, `App.s.T` — namespaces
        // resolve before the value path.
        if let ExprKind::Name(n) = &*base.kind {
            if let Some(e) = self.sym.enums.get(&n.name) {
                if !e.members.iter().any(|m| m == &field.name) {
                    let members = e.members.join(", ");
                    self.err_note(
                        span,
                        "E0303",
                        format!("`{}` has no member `{}`", n.name, field.name),
                        format!("members: {members}"),
                        "types.md §3.3",
                    );
                    return Ty::Unknown;
                }
                return Ty::Enum(n.name.clone());
            }
            if self.sym.services.contains_key(&n.name) || is_namespace(&n.name) {
                return Ty::Unknown;
            }
            // `B.col` where B is a query binding.
            //
            // The whole stack, innermost first: inside `exists (…)` the
            // subquery's own bindings shadow, and the outer ones stay
            // reachable — filtering a parent by its children is the point
            // of the construct (queries.md §3.5).
            if self.in_query() {
                let object = self
                    .query
                    .iter()
                    .rev()
                    .find_map(|s| s.bindings.iter().find(|b| b.name == n.name))
                    .map(|b| b.object.clone())
                    // `org.name` where `org` is the *field* an `as one`
                    // produces rather than the binding it came from. Both
                    // name the same row, and `orderby org.name` is written
                    // with the field because that is what the projection
                    // calls it (queries.md §5.4).
                    .or_else(|| {
                        self.query.iter().rev().find_map(|s| {
                            s.bindings
                                .iter()
                                .find(|b| b.one_field.as_deref() == Some(n.name.as_str()))
                                .map(|b| b.object.clone())
                        })
                    });
                if let Some(object) = object {
                    return match self.column_of(&object, &field.name) {
                        Some(t) => t,
                        None => {
                            self.err(
                                span,
                                "E0211",
                                format!("`{}` is not a column of `{}`", field.name, object),
                                "names.md §5.4",
                            );
                            Ty::Unknown
                        }
                    };
                }
            }
        }

        let bt = self.expr(base);

        // types.md §5.2 — the rule the whole lattice exists for.
        if bt.is_raw() {
            self.err_note(
                span,
                "E0310",
                format!("cannot read `{}` of a raw result", field.name),
                "add a projection naming the fields you need: \
                 `select … as { … } …`",
                "types.md §5.2",
            );
            return Ty::Unknown;
        }
        // Inside a query clause a nullable field access is SQL: a LEFT JOIN
        // column yields NULL and the database propagates it. The narrowing
        // rule is about values in application code (types.md §6.4).
        if bt.is_optional() && !self.in_query() {
            self.err_note(
                span,
                "E0320",
                format!("`{bt}` may be null"),
                "guard it first: `if ($x == null) { throw NotFound(\"…\"); }`, or \
                 write `… or throw NotFound(\"…\")` on the query",
                "types.md §6.4",
            );
            return Ty::Unknown;
        }

        // In a query clause a nullable record is a LEFT JOIN result; reading
        // through it yields SQL NULL (types.md §6.4).
        let bt = if self.in_query() { bt.strip_opt() } else { bt };

        match &bt {
            Ty::Record(fields) => match fields.iter().find(|(n, _)| n == &field.name) {
                Some((_, t)) => t.clone(),
                None => {
                    let have = fields
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.err_note(
                        span,
                        "E0312",
                        format!("no field `{}`", field.name),
                        format!("this value has: {have}"),
                        "types.md §5.3",
                    );
                    Ty::Unknown
                }
            },
            Ty::Class(name) => match self.sym.classes.get(name) {
                Some(c) => match c.fields.iter().find(|f| f.name == field.name) {
                    Some(f) => f.ty.clone(),
                    None => {
                        let have = c
                            .fields
                            .iter()
                            .map(|f| f.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.err_note(
                            span,
                            "E0312",
                            format!("`{name}` has no field `{}`", field.name),
                            format!("declared fields: {have}"),
                            "types.md §4",
                        );
                        Ty::Unknown
                    }
                },
                None => Ty::Unknown,
            },
            Ty::Unknown => Ty::Unknown,
            other => {
                self.err(
                    span,
                    "E0312",
                    format!("`{other}` has no fields"),
                    "types.md §5.3",
                );
                Ty::Unknown
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        let a = self.expr(lhs);
        let b = self.expr(rhs);
        match op {
            BinOp::And | BinOp::Or => {
                self.require_boolean(&a, lhs.span);
                self.require_boolean(&b, rhs.span);
                Ty::boolean()
            }
            BinOp::Eq | BinOp::Ne | BinOp::EqOpt => {
                if matches!(op, BinOp::EqOpt) && !b.is_optional() && !matches!(b, Ty::Unknown) {
                    self.err_note(
                        span,
                        "E0503",
                        format!("`==?` with a non-nullable right operand `{b}`"),
                        "the optional predicate exists to be dropped when the value is \
                         absent; with a non-null value it is a plain `==`",
                        "queries.md §3.2",
                    );
                }
                // builtins.md §6 — `hash.password` salts, so two calls on
                // the same input differ. A `*_hash` column compared to one
                // can never match: the login silently rejects everybody,
                // and the test that would catch it is the one nobody
                // writes because the code reads correctly.
                if let Some(col) = self.hash_column_compared_to_a_new_hash(lhs, rhs) {
                    self.warn(
                        span,
                        "W1201",
                        format!(
                            "`{col}` compared against a fresh `hash.password(...)`, which \
                             can never match"
                        ),
                        "builtins.md §6",
                    );
                }
                if !comparable(&a, &b) {
                    match self.untyped_operand(lhs, rhs) {
                        // routing.md §3.1 — the type is missing from the
                        // path, and that is a more useful thing to say
                        // than "text and bigint do not compare".
                        Some(name) => self.err_note(
                            span,
                            "E0376",
                            format!("`{a}` and `{b}` cannot be compared"),
                            format!(
                                "`@{name}` has no type in the path, so it is text: write \
                                 `{{{name}: <type>}}`"
                            ),
                            "routing.md §3.1",
                        ),
                        None => self.err(
                            span,
                            "E0376",
                            format!("`{a}` and `{b}` cannot be compared"),
                            "types.md §12.6",
                        ),
                    }
                }
                Ty::boolean()
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if a.is_optional() || b.is_optional() {
                    self.err_note(
                        span,
                        "E0320",
                        "ordering comparison on a value that may be null",
                        "guard it first, or compare against null explicitly",
                        "types.md §12.6",
                    );
                    return Ty::boolean();
                }
                if matches!(a, Ty::Enum(_)) || matches!(b, Ty::Enum(_)) {
                    self.err_note(
                        span,
                        "E0304",
                        "enums do not order",
                        "declaration order is not a documented total order, and an \
                         of-less enum has none in the database at all",
                        "types.md §3.5",
                    );
                    return Ty::boolean();
                }
                if !orderable(&a, &b) {
                    self.err(
                        span,
                        "E0376",
                        format!("`{a}` and `{b}` cannot be ordered"),
                        "types.md §12.6",
                    );
                }
                Ty::boolean()
            }
            BinOp::Like | BinOp::ILike => Ty::boolean(),
            _ => {
                if a.is_optional() || b.is_optional() {
                    self.err_note(
                        span,
                        "E0320",
                        format!("arithmetic on `{a}` and `{b}`, one of which may be null"),
                        "guard it first, or supply a default with `??`",
                        "types.md §6.4",
                    );
                    return Ty::Unknown;
                }
                match arith(op, &a, &b) {
                    Some(t) => t,
                    None => {
                        let hint = if matches!(op, BinOp::Add)
                            && (a.scalar().is_some_and(|s| s.is_text())
                                || b.scalar().is_some_and(|s| s.is_text()))
                        {
                            "`+` does not stringify: wrap the other side in \
                             `string.of(...)`"
                        } else {
                            "no overload for these operand types"
                        };
                        self.err_note(
                            span,
                            "E0370",
                            format!("`{a}` {} `{b}` is not defined", op.as_str()),
                            hint,
                            "types.md §12.1",
                        );
                        Ty::Unknown
                    }
                }
            }
        }
    }

    fn object(&mut self, entries: &[ObjEntry], _span: Span) -> Ty {
        let mut fields: Fields = Vec::new();
        for e in entries {
            match e {
                ObjEntry::Field { key, value, .. } => {
                    let t = self.expr(value);
                    // types.md §5.4 — raw may be a field value; the object is
                    // built by splicing, never by parsing.
                    fields.push((key.name.clone(), t));
                }
                ObjEntry::Spread { source, span, .. } => {
                    let t = self.spread_source(source, *span);
                    if let Some(f) = t.fields() {
                        fields.extend(f.clone());
                    }
                }
            }
        }
        Ty::Record(fields)
    }

    /// types.md §9.1 — a spread operand must have a statically known field
    /// set.
    fn spread_source(&mut self, source: &Ident, span: Span) -> Ty {
        let Some(t) = self.lookup(&source.name) else {
            self.err_note(
                span,
                "E0211",
                format!("unknown local `${}`", source.name),
                "a spread source is a local",
                "types.md §9.1",
            );
            return Ty::Unknown;
        };
        let t = t.strip_opt();
        match &t {
            Ty::Class(name) => match self.sym.classes.get(name) {
                Some(c) => Ty::Record(
                    c.fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                ),
                None => Ty::Unknown,
            },
            Ty::Record(_) => t.clone(),
            Ty::Raw => {
                self.err_note(
                    span,
                    "E0311",
                    "cannot spread a raw result",
                    "a spread needs a declared shape: project the query, or validate \
                     the body with `request.body() as C`",
                    "types.md §9.1",
                );
                Ty::Unknown
            }
            Ty::Unknown => Ty::Unknown,
            other => {
                self.err_note(
                    span,
                    "E0340",
                    format!("`{other}` has no declared shape"),
                    "spread needs a class value or a projected record",
                    "types.md §9.1",
                );
                Ty::Unknown
            }
        }
    }

    // ------------------------------------------------------------ calls

    fn call(&mut self, callee: &Expr, args: &[Expr], filter: Option<&Expr>, span: Span) -> Ty {
        // Aggregates: `count(x)`, `sum(x where pred)`.
        if let ExprKind::Name(n) = &*callee.kind {
            if is_aggregate(&n.name) {
                return self.aggregate(&n.name, args, filter, span);
            }
        }
        if let ExprKind::Field { base, field } = &*callee.kind {
            if let ExprKind::Name(ns) = &*base.kind {
                if ns.name == "count" && field.name == "distinct" {
                    return self.aggregate("count", args, filter, span);
                }
            }
        }
        if filter.is_some() {
            self.err_note(
                span,
                "E0533",
                "`where` inside a call that is not an aggregate",
                "the aggregate filter is only valid on count/sum/min/max/avg",
                "queries.md §6.3",
            );
        }

        let path = callee_path(callee);
        let arg_types: Vec<Ty> = args.iter().map(|a| self.expr(a)).collect();

        if let Some(path) = path {
            if let Some(ty) = self.builtin(&path, &arg_types, args, span) {
                return ty;
            }
            // `Service.method`
            if let Some(f) = self.sym.functions.get(&path).cloned() {
                let ty = self.user_call(&f, &arg_types, args, span);
                // An unannotated function has no declared return, but a
                // previous pass inferred one (types.md §10.2 only demands an
                // annotation when two returns disagree).
                if f.returns.is_none() {
                    if let Some(known) = self.known_returns.get(&path) {
                        return known.clone();
                    }
                }
                return ty;
            }
            if path.contains('.') {
                let (head, _) = path.split_once('.').unwrap_or((path.as_str(), ""));
                if self.sym.services.contains_key(head) {
                    self.err_note(
                        span,
                        "E0204",
                        format!("`{path}` is not a function of `{head}`"),
                        "check the spelling, or declare it in the service",
                        "names.md §6.4",
                    );
                    return Ty::Unknown;
                }
                if is_namespace(head) {
                    self.err_note(
                        span,
                        "E0204",
                        format!("`{path}` is not a builtin"),
                        "see builtins.md for the surface",
                        "builtins.md §1",
                    );
                    return Ty::Unknown;
                }
                return Ty::Unknown;
            }
            if path == "now" {
                self.err_note(
                    span,
                    "E0302",
                    "`now()` is a column default, not an application call",
                    "in code write `date.now()`",
                    "types.md §2.4",
                );
                return Ty::Unknown;
            }
            self.err_note(
                span,
                "E0204",
                format!("unknown function `{path}`"),
                "declare it at top level, or call it as `Service.method(...)`",
                "names.md §6.4",
            );
            return Ty::Unknown;
        }

        Ty::Unknown
    }

    fn user_call(
        &mut self,
        f: &crate::symbols::FunctionSym,
        arg_types: &[Ty],
        args: &[Expr],
        span: Span,
    ) -> Ty {
        if arg_types.len() != f.params.len() {
            self.err_note(
                span,
                "E0353",
                format!(
                    "`{}` takes {} argument(s), given {}",
                    f.qualified(),
                    f.params.len(),
                    arg_types.len()
                ),
                format!(
                    "declared as ({})",
                    f.params
                        .iter()
                        .map(|(n, t)| format!("{n}: {t}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "types.md §10.1",
            );
        }
        for (i, got) in arg_types.iter().enumerate() {
            if let Some((name, want)) = f.params.get(i) {
                if !got.assignable_to(want) {
                    let at = args.get(i).map(|a| a.span).unwrap_or(span);
                    self.err_note(
                        at,
                        "E0354",
                        format!("`{got}` is not assignable to `{name}: {want}`"),
                        if got.is_optional() && !want.is_optional() {
                            "guard the value or use `or throw` before passing it"
                        } else {
                            "types.md §10.3 lists the assignability rules"
                        },
                        "types.md §10.3",
                    );
                }
            }
        }
        f.returns.clone().unwrap_or(Ty::Unknown)
    }

    fn aggregate(&mut self, name: &str, args: &[Expr], filter: Option<&Expr>, span: Span) -> Ty {
        if !self.in_query() {
            self.err_note(
                span,
                "E0530",
                format!("`{name}(...)` outside a query"),
                "SQL aggregates are only valid inside a projection",
                "queries.md §6.2",
            );
        }
        let arg = args.first().map(|a| self.expr(a)).unwrap_or(Ty::Unknown);
        if let Some(f) = filter {
            let t = self.expr(f);
            self.require_boolean(&t, f.span);
        }
        match name {
            // count never returns null (types.md §6.3).
            "count" => Ty::int(),
            // `sum` widens and `avg` is always numeric — the same
            // widening Postgres does, and for the same reason: a sum that
            // kept its operand's width would overflow on exactly the data
            // that makes a sum worth asking for (types.md §6.3).
            "sum" => widen_sum(&arg.strip_opt()).opt(),
            "avg" => match arg.strip_opt() {
                Ty::Scalar(s) if s.numeric_rank().is_some() => Ty::numeric().opt(),
                other => other.opt(),
            },
            "min" | "max" => arg.strip_opt().opt(),
            _ => Ty::Unknown,
        }
    }

    /// Builtin signatures (builtins.md). Returns `None` when the path is not
    /// a builtin, so the caller can try user functions.
    fn builtin(&mut self, path: &str, args: &[Ty], exprs: &[Expr], span: Span) -> Option<Ty> {
        let arity = |c: &mut Self, want: usize| {
            if args.len() != want {
                c.err_note(
                    span,
                    "E0205",
                    format!("`{path}` takes {want} argument(s), given {}", args.len()),
                    "see builtins.md",
                    "builtins.md §1.2",
                );
            }
        };
        let a0 = args.first().cloned().unwrap_or(Ty::Unknown);

        Some(match path {
            // --- responses (routing.md §6.1)
            "json" | "created" | "accepted" | "badRequest" => {
                arity(self, 1);
                let status = match path {
                    "created" => 201,
                    "accepted" => 202,
                    "badRequest" => 400,
                    _ => 200,
                };
                self.record_response(status, a0.clone());
                self.reject_private_response(exprs.first(), path, span);
                // types.md §6.4 — `json(x)` with `x : T?` answers 200 null
                // where it means 404.
                if a0.is_optional() {
                    self.err_note(
                        exprs.first().map(|e| e.span).unwrap_or(span),
                        "E0320",
                        format!("`{path}(...)` on `{a0}`, which may be null"),
                        "a route that answers 200 null usually means 404: use \
                         `… or throw NotFound(\"…\")` on the query",
                        "types.md §6.4",
                    );
                }
                Ty::Response
            }
            "noContent" | "internalError" => {
                arity(self, 0);
                self.record_response(if path == "noContent" { 204 } else { 500 }, Ty::Void);
                Ty::Response
            }
            "unauthorized" | "forbidden" | "notFound" | "conflict" | "tooManyRequests" => {
                arity(self, 1);
                let status = match path {
                    "unauthorized" => 401,
                    "forbidden" => 403,
                    "notFound" => 404,
                    "conflict" => 409,
                    _ => 429,
                };
                // `{"error": m}` — routing.md §6.1, the same envelope a
                // declared error produces.
                self.record_response(status, Ty::Unknown);
                Ty::Response
            }
            "statusCode" => {
                arity(self, 2);
                if let Some(n) = exprs.first().and_then(literal_status) {
                    self.record_response(n, args.get(1).cloned().unwrap_or(Ty::Unknown));
                }
                Ty::Response
            }
            "redirect" => {
                arity(self, 2);
                if let Some(n) = exprs.first().and_then(literal_status) {
                    self.record_response(n, Ty::Void);
                }
                Ty::Response
            }
            // routing.md §6.5 — the one body that is not JSON. The media
            // type is a literal so `jwc openapi` can name it and so a
            // runtime value can never decide how a body is framed.
            "content" => {
                arity(self, 2);
                let mime = match exprs.first().map(|e| &*e.kind) {
                    Some(ExprKind::Str(m)) | Some(ExprKind::RawStr(m)) => Some(m.clone()),
                    _ => {
                        self.err_note(
                            exprs.first().map(|e| e.span).unwrap_or(span),
                            "E0735",
                            "`content(...)` media type must be a string literal",
                            "the media type decides how the body is framed, so it \
                             cannot depend on a runtime value",
                            "routing.md §6.5",
                        );
                        None
                    }
                };
                // A record here means the author reached for `content` where
                // `json` was meant; JSON-encoding it silently would produce a
                // body that disagrees with the declared type.
                let body = args.get(1).cloned().unwrap_or(Ty::Unknown);
                let body_is_text = matches!(body, Ty::Scalar(sc) if sc.is_text());
                if !matches!(body, Ty::Unknown) && !body_is_text {
                    self.err_note(
                        exprs.get(1).map(|e| e.span).unwrap_or(span),
                        "E0736",
                        format!("`content(...)` body is `{body}`, not `text`"),
                        "a `content` body is sent verbatim: build the string first, \
                         or use `json(...)` for a structured body",
                        "routing.md §6.5",
                    );
                }
                if let Some(m) = &mime {
                    self.record_response_as(200, Ty::text(), Some(normalize_media(m)));
                }
                Ty::Response
            }
            "cookie" => Ty::Response,
            "serve" => {
                arity(self, 1);
                Ty::Void
            }

            // --- env and coercions (types.md §7.2)
            "env" => {
                arity(self, 1);
                Ty::text().opt()
            }
            "int" => {
                arity(self, 1);
                Ty::int()
            }
            "bigint" => {
                arity(self, 1);
                Ty::bigint()
            }
            "numeric" => {
                arity(self, 1);
                Ty::numeric()
            }
            "boolean" => {
                arity(self, 1);
                Ty::boolean()
            }
            "uuid" => {
                arity(self, 1);
                Ty::Scalar(Scalar::Uuid)
            }
            "timestamptz" => {
                arity(self, 1);
                Ty::timestamptz()
            }
            // `enum(E, x)` takes a type name (builtins.md §2).
            "enum" => {
                arity(self, 2);
                match exprs.first().map(|e| &*e.kind) {
                    Some(ExprKind::Name(n)) if self.sym.enums.contains_key(&n.name) => {
                        Ty::Enum(n.name.clone()).opt()
                    }
                    _ => {
                        self.err_note(
                            span,
                            "E0301",
                            "`enum(...)` needs an enum type as its first argument",
                            "write `enum(InvoiceStatus, request.query(\"status\"))`",
                            "builtins.md §2",
                        );
                        Ty::Unknown
                    }
                }
            }

            // --- date (builtins.md §3)
            // --- debug (tooling.md §3)
            //
            // The one builtin that accepts a `Raw`, because the one place a
            // raw result's shape can be inspected is where the shape is in
            // question (types.md §5.1). It returns its argument unchanged so
            // it can be wrapped around a subexpression without restructuring
            // the code around it.
            "debug.dump" => {
                arity(self, 1);
                self.warn_note(
                    span,
                    "W1301",
                    "`debug.dump` in the program",
                    "it prints only under `jwc serve --dev` and is a no-op \
                     elsewhere, but a call left in is a call nobody meant to \
                     ship — delete it, or run `jwc check` without \
                     `--deny-warnings`",
                    "tooling.md §3.4",
                );
                a0
            }

            "date.now" => {
                arity(self, 0);
                Ty::timestamptz()
            }
            "date.today" => {
                arity(self, 0);
                Ty::Scalar(Scalar::Date)
            }
            "date.days" | "date.hours" | "date.minutes" | "date.seconds" => {
                arity(self, 1);
                Ty::interval()
            }
            "date.add" => {
                arity(self, 2);
                Ty::timestamptz()
            }
            "date.parse" => {
                arity(self, 1);
                Ty::timestamptz().opt()
            }
            "date.format" => {
                arity(self, 2);
                Ty::text()
            }

            // --- string (builtins.md §4)
            "string.of" => {
                arity(self, 1);
                Ty::text()
            }
            "string.len" => {
                arity(self, 1);
                Ty::int()
            }
            "string.lower" | "string.upper" | "string.trim" => {
                arity(self, 1);
                Ty::text()
            }
            "string.replace" => {
                arity(self, 3);
                Ty::text()
            }
            "string.starts_with" | "string.ends_with" | "string.contains" => {
                arity(self, 2);
                Ty::boolean()
            }
            "string.split" => {
                arity(self, 2);
                Ty::text().array()
            }
            "string.split_csv" => {
                arity(self, 1);
                Ty::text().array()
            }
            "string.join" => {
                arity(self, 2);
                Ty::text()
            }
            "string.pad_left" | "string.pad_right" => {
                arity(self, 3);
                Ty::text()
            }
            "string.slice" => {
                arity(self, 3);
                Ty::text()
            }
            "string.matches" => {
                arity(self, 2);
                Ty::boolean()
            }
            "string.strip_prefix" => {
                arity(self, 2);
                Ty::text()
            }

            // --- array (builtins.md §5) — the lambda replacement
            "array.len" => {
                arity(self, 1);
                Ty::int()
            }
            "array.is_empty" => {
                arity(self, 1);
                Ty::boolean()
            }
            "array.sum" => {
                arity(self, 2);
                self.check_field_name(&a0, exprs.get(1), span);
                Ty::numeric()
            }
            "array.sum_product" => {
                arity(self, 3);
                self.check_field_name(&a0, exprs.get(1), span);
                self.check_field_name(&a0, exprs.get(2), span);
                Ty::numeric()
            }
            "array.min" | "array.max" => {
                arity(self, 2);
                self.check_field_name(&a0, exprs.get(1), span);
                Ty::numeric().opt()
            }
            "array.pluck" => {
                arity(self, 2);
                self.check_field_name(&a0, exprs.get(1), span)
                    .unwrap_or(Ty::Unknown)
                    .array()
            }
            "array.contains" => {
                arity(self, 2);
                Ty::boolean()
            }
            "array.first" | "array.last" => {
                arity(self, 1);
                a0.element().cloned().unwrap_or(Ty::Unknown).opt()
            }
            "array.sorted" => {
                arity(self, 2);
                self.check_field_name(&a0, exprs.get(1), span);
                a0
            }

            // --- hash / jwt / crypto (builtins.md §6)
            "hash.password" => {
                arity(self, 1);
                self.saw_password_hash = true;
                Ty::text()
            }
            "hash.verify" | "hash.hmac_verify" => Ty::boolean(),
            "hash.sha256" => {
                arity(self, 1);
                Ty::text()
            }
            "hash.hmac_sha256" => {
                arity(self, 2);
                Ty::text()
            }
            // The comparison that does not leak how far it got. Two
            // hex digests are not secret, but a hand-written `==` on a
            // *token* is, and having the name here means the safe form is
            // reachable without importing anything.
            "crypto.constant_time_eq" => {
                arity(self, 2);
                Ty::boolean()
            }
            "crypto.token" => {
                arity(self, 1);
                Ty::text()
            }
            "jwt.sign" => {
                arity(self, 3);
                Ty::text()
            }
            "jwt.verify" => {
                arity(self, 2);
                Ty::Record(vec![
                    ("sub".into(), Ty::text()),
                    ("exp".into(), Ty::bigint()),
                    ("iat".into(), Ty::bigint()),
                ])
                .opt()
            }

            // --- request / response / context (builtins.md §7)
            "request.raw_body" => {
                arity(self, 0);
                Ty::text()
            }
            "request.header" | "request.query" => {
                arity(self, 1);
                Ty::text().opt()
            }
            "request.query_all" => {
                arity(self, 1);
                Ty::text().array()
            }
            "request.path" => {
                arity(self, 0);
                self.saw_request_path = true;
                Ty::text()
            }
            "request.method" | "request.route" | "request.id" => {
                arity(self, 0);
                Ty::text()
            }
            "request.peer_ip" | "request.client_ip" => {
                arity(self, 0);
                Ty::inet()
            }
            "request.body" => {
                // routing.md §5.2 — always `as C`. The cast wraps this call,
                // so reaching here means the cast is missing.
                self.err_note(
                    span,
                    "E0720",
                    "`request.body()` without `as <Class>`",
                    "the body has no declared shape until it is validated: write \
                     `request.body() as Register`",
                    "routing.md §5.2",
                );
                Ty::Unknown
            }
            "response.status" => {
                arity(self, 0);
                if self.body != BodyKind::After {
                    self.err_note(
                        span,
                        "E0734",
                        "`response.status()` outside an `after` block",
                        "there is no response yet",
                        "middleware.md §5.1",
                    );
                }
                Ty::int()
            }
            // The other half of what an `after` block exists to observe.
            // Without it the only honest thing a telemetry row could say
            // about latency was nothing — jwc-shortener wrote a hardcoded
            // zero into every one of 1.48M rows, and every percentile
            // derived from them was a zero.
            "response.duration_ms" | "response.duration_us" => {
                arity(self, 0);
                if self.body != BodyKind::After {
                    self.err_note(
                        span,
                        "E0734",
                        format!("`{path}()` outside an `after` block"),
                        "the request is not finished yet",
                        "middleware.md §5.1",
                    );
                }
                Ty::bigint()
            }
            "response.set_header" | "response.add_header" => {
                arity(self, 2);
                if self.body != BodyKind::After {
                    self.err_note(
                        span,
                        "E0734",
                        "response headers may only be set in an `after` block",
                        "on the response path use `with { ... }`",
                        "routing.md §6.3",
                    );
                }
                Ty::Void
            }

            // --- packages (builtins.md §8). The surface a package exports
            // is declared by the package; until package manifests carry it
            // (ROADMAP v0.28.0) the two the sample depends on are pinned
            // here rather than left unchecked.
            "redis.get" => {
                arity(self, 1);
                Ty::text().opt()
            }
            "redis.set" => {
                arity(self, 3);
                Ty::boolean()
            }
            "redis.del" => {
                arity(self, 1);
                Ty::int()
            }
            "redis.incr" => {
                arity(self, 1);
                Ty::bigint()
            }
            "redis.expire" => {
                arity(self, 2);
                Ty::boolean()
            }
            "redis.rate_limit" => {
                arity(self, 3);
                // routing.md §5.4 — `request.route()` exists so a rate-limit
                // key has bounded cardinality. `request.path()` gives every
                // id its own bucket, so a caller walking ids is never
                // limited and the store fills with one-hit keys: a
                // self-DoS with the rate limiter switched on.
                if let Some(k) = exprs.first() {
                    if self.reads_request_path(k) {
                        self.warn(
                            k.span,
                            "W0602",
                            "a rate-limit key built from `request.path()`",
                            "routing.md §5.4",
                        );
                    }
                }
                Ty::boolean()
            }
            "redis.enabled" => {
                arity(self, 0);
                Ty::boolean()
            }
            "mail.send" => {
                arity(self, 3);
                Ty::Void
            }

            // --- raw escape hatch (writes.md §6)
            "raw" => {
                self.check_raw(exprs, span);
                Ty::Raw.array()
            }

            _ => return None,
        })
    }

    /// `array.sum(xs, "field")` — the field name is a literal and is checked
    /// against the element type (builtins.md §5).
    fn check_field_name(&mut self, subject: &Ty, arg: Option<&Expr>, span: Span) -> Option<Ty> {
        let name = match arg.map(|a| &*a.kind) {
            Some(ExprKind::Str(s)) => s.clone(),
            Some(_) => {
                self.err_note(
                    arg.map(|a| a.span).unwrap_or(span),
                    "E0206",
                    "a field name must be a string literal",
                    "there are no lambdas: `array.sum($lines, \"quantity\")`",
                    "builtins.md §5",
                );
                return None;
            }
            None => return None,
        };
        let elem = subject.element()?;
        let fields = match elem {
            Ty::Class(c) => self.sym.classes.get(c).map(class_fields)?,
            Ty::Record(f) => f.clone(),
            _ => return None,
        };
        match fields.iter().find(|(n, _)| *n == name) {
            Some((_, t)) => Some(t.clone()),
            None => {
                let have = fields
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.err_note(
                    arg.map(|a| a.span).unwrap_or(span),
                    "E0301",
                    format!("no field `{name}` on the element type"),
                    format!("fields: {have}"),
                    "builtins.md §5",
                );
                None
            }
        }
    }

    // ------------------------------------------------------------ queries

    fn select(&mut self, s: &SelectExpr, span: Span) -> Ty {
        let object = match self.resolve_source(&s.source) {
            Some(o) => o,
            None => return Ty::Unknown,
        };

        let mut bindings = vec![Binding {
            name: s.binder.name.clone(),
            object: object.clone(),
            one_field: None,
        }];

        for j in &s.joins {
            let Some(obj) = self.resolve_source(&j.table) else {
                continue;
            };
            if bindings.iter().any(|b| b.name == j.binder.name) {
                self.err_note(
                    j.binder.span,
                    "E0212",
                    format!("`{}` is already a binding in this query", j.binder.name),
                    "give the second one an alias: `left join App.s.T other on …`",
                    "names.md §5.4",
                );
            }
            bindings.push(Binding {
                name: j.binder.name.clone(),
                object: obj,
                one_field: j
                    .result
                    .as_ref()
                    .and_then(|r| (r.cardinality == Cardinality::One).then(|| r.name.name.clone())),
            });
        }

        self.query.push(QueryScope { bindings });

        for j in &s.joins {
            let t = self.expr(&j.on);
            self.require_boolean(&t, j.on.span);
            if let Some(r) = &j.result {
                // The child's own ordering and bound are about the child
                // (queries.md §4.6), so only that binding is in scope.
                let outer = self.scoped_to.take();
                self.scoped_to = self.sym.by_path.get(&j.table.text()).cloned();
                for k in &r.order_by {
                    self.expr(&k.expr);
                }
                if let Some(l) = &r.limit {
                    self.expr(l);
                }
                self.scoped_to = outer;
            }
        }
        if let Some(f) = &s.filter {
            let t = self.expr(f);
            self.require_boolean(&t, f.span);
            self.check_tautology(f);
        }
        for g in &s.group_by {
            self.expr(g);
        }
        if let Some(h) = &s.having {
            let t = self.expr(h);
            self.require_boolean(&t, h.span);
        }

        let shape = s
            .projection
            .as_ref()
            .map(|p| self.projection(p, &object, s));
        for k in &s.order_by {
            // queries.md §5.4 — a collection has no single value to sort
            // by. Left to the generic resolver this reported "not a column
            // of any binding", which points the reader at the wrong fix:
            // the name is right, it just is not orderable.
            if let ExprKind::Name(n) = &*k.expr.kind {
                if collection_field(s, &n.name) {
                    self.err_note(
                        n.span,
                        "E0521",
                        format!("`{}` is a collection", n.name),
                        "there is no single value to sort by; order the collection \
                         inside its own join result, or sort by a scalar field",
                        "queries.md §5.4",
                    );
                    continue;
                }
            }
            self.expr(&k.expr);
        }
        if let Some(l) = &s.limit {
            self.expr(l);
        }
        if let Some(p) = &s.page {
            if let Some(a) = &p.after {
                self.expr(a);
            }
            self.expr(&p.size);
            if let Some(m) = &p.max {
                self.expr(m);
            }
            if s.order_by.is_empty() {
                self.err_note(
                    p.span,
                    "E0550",
                    "`page` without an `orderby`",
                    "keyset pagination needs a total order: `orderby issued_at desc, id desc`",
                    "queries.md §9.2",
                );
            }
        }

        // The join attachment tree (queries.md §4.4). Its diagnostics are
        // about how the joins relate to each other, which is why they live
        // in the planner rather than here.
        let mut planned = crate::query::plan(s, self.sym);
        let diags = std::mem::take(&mut planned.diags);
        let plan_ok = !diags
            .iter()
            .any(|d| d.severity == crate::diag::Severity::Error);
        for d in diags {
            self.diags.push((
                Loc {
                    file: self.file,
                    span: d.span,
                },
                d,
            ));
        }
        if plan_ok {
            self.check_pushdown(s, &planned, span);
        }

        self.check_aggregates(s);
        self.check_unbounded(s);
        if s.first {
            self.check_first_determinism(s, &object, span);
        }

        self.query.pop();

        let row = match shape {
            Some(fields) => Ty::Record(fields),
            None => {
                // A view is a named projection, so selecting from one yields
                // a record even without `as { }` (types.md §5.3).
                match self.sym.views.get(&object) {
                    Some(v) => Ty::Record(v.shape.clone()),
                    None => Ty::Raw,
                }
            }
        };

        if s.first {
            // A whole-table aggregate answers exactly one row — `count`
            // of an empty table is 0, not no row — so `first` on it is not
            // optional. Typing it `T?` forced an `or throw` on a branch
            // that cannot be taken, which is how a real null check learns
            // to be ignored.
            if s.group_by.is_empty() && s.joins.is_empty() && is_whole_table_aggregate(s) {
                row
            } else {
                row.opt()
            }
        } else if s.page.is_some() {
            // queries.md §9.3 — the envelope, with `items` keeping whatever
            // the query produced.
            Ty::Record(vec![
                ("items".into(), row.array()),
                ("next".into(), Ty::text().opt()),
                ("has_more".into(), Ty::boolean()),
            ])
        } else {
            row.array()
        }
    }

    fn resolve_source(&mut self, q: &QualifiedTable) -> Option<String> {
        let path = q.text();
        match self.sym.by_path.get(&path) {
            Some(name) => Some(name.clone()),
            None => {
                self.err_note(
                    q.span,
                    "E0502",
                    format!("`{path}` is not a declared table or view"),
                    "sources are fully qualified: `App.<schema>.<Object>`",
                    "queries.md §2.1",
                );
                None
            }
        }
    }

    fn projection(&mut self, p: &ObjectShape, object: &str, s: &SelectExpr) -> Fields {
        let mut out: Fields = Vec::new();
        for f in &p.fields {
            match f {
                ProjField::Column(i) => {
                    // A bare projection column is a column of the **driving**
                    // binding. Joined tables reach the projection through
                    // their `as one` / `as many` nested shape, so resolving
                    // across every binding would make `id` ambiguous in every
                    // joined query (queries.md §6.1).
                    let ty = match self.column_of(object, &i.name) {
                        Some(t) => t,
                        None => {
                            self.err_note(
                                i.span,
                                "E0211",
                                format!("`{}` is not a column of `{object}`", i.name),
                                "a bare projection field names a column of the query's \
                                 own binding; a joined table's columns go in its nested \
                                 shape",
                                "queries.md §6.1",
                            );
                            Ty::Unknown
                        }
                    };
                    self.reject_private(object, &i.name, i.span);
                    out.push((i.name.clone(), ty));
                }
                ProjField::Expr { alias, value, .. } => {
                    // `org_id: id` — an alias of a driving column. Bare names
                    // here are columns of the driving binding, same as
                    // `ProjField::Column`; an aggregate over a joined table
                    // qualifies (`count(I.id)`).
                    let outer = self.scoped_to.take();
                    if matches!(&*value.kind, ExprKind::Name(_)) {
                        self.scoped_to = Some(object.to_string());
                    }
                    let ty = self.expr(value);
                    self.scoped_to = outer;
                    if let ExprKind::Name(n) = &*value.kind {
                        self.reject_private(object, &n.name, value.span);
                    }
                    out.push((alias.name.clone(), ty));
                }
                ProjField::Nested { alias, shape, span } => {
                    let join = s
                        .joins
                        .iter()
                        .find(|j| j.result.as_ref().is_some_and(|r| r.name.name == alias.name));
                    let Some(join) = join else {
                        self.err_note(
                            *span,
                            "E0534",
                            format!("`{}` is not a join result", alias.name),
                            "a nested projection names an `as one` / `as many` binding",
                            "queries.md §6.1",
                        );
                        out.push((alias.name.clone(), Ty::Unknown));
                        continue;
                    };
                    let obj = self
                        .sym
                        .by_path
                        .get(&join.table.text())
                        .cloned()
                        .unwrap_or_default();
                    let inner = self.nested_projection(shape, &obj);
                    let rec = Ty::Record(inner);
                    let result = join.result.as_ref().expect("checked above");
                    let ty = match result.cardinality {
                        // `as many` is an array, empty rather than null;
                        // `left join … as one` may not match (types.md §6.3).
                        Cardinality::Many => rec.array(),
                        Cardinality::One if join.kind == JoinKind::Left => rec.opt(),
                        Cardinality::One => rec,
                        // `as group` produces no field, so a projection
                        // naming it is E0534 above.
                        Cardinality::Group => Ty::Unknown,
                    };
                    out.push((alias.name.clone(), ty));
                }
            }
        }
        out
    }

    fn nested_projection(&mut self, shape: &ObjectShape, object: &str) -> Fields {
        shape
            .fields
            .iter()
            .map(|f| match f {
                ProjField::Column(i) => {
                    let ty = self.column_of(object, &i.name).unwrap_or_else(|| {
                        self.err(
                            i.span,
                            "E0211",
                            format!("`{}` is not a column of `{object}`", i.name),
                            "queries.md §6.1",
                        );
                        Ty::Unknown
                    });
                    self.reject_private(object, &i.name, i.span);
                    (i.name.clone(), ty)
                }
                ProjField::Expr { alias, .. } => (alias.name.clone(), Ty::Unknown),
                ProjField::Nested { alias, .. } => (alias.name.clone(), Ty::Unknown),
            })
            .collect()
    }

    /// schema.md §3.1 — a `private` column never reaches a response.
    ///
    /// It **may** be projected into a local: `hash.verify` needs the stored
    /// hash, and the hash lives in the database. What is forbidden is the
    /// value escaping, so the projection marks the value and the response
    /// builders reject it. In a `view` — which exists only to be returned —
    /// it is rejected outright.
    fn reject_private(&mut self, object: &str, column: &str, span: Span) {
        let private = self
            .sym
            .tables
            .get(object)
            .is_some_and(|t| t.is_private(column));
        if !private {
            return;
        }
        if self.body == BodyKind::View {
            self.err_note(
                span,
                "E0410",
                format!("`{column}` is `private` and this is a view"),
                "a view is an output shape; a private column can never appear in one",
                "schema.md §3.3",
            );
            return;
        }
        self.saw_private = true;
    }

    /// The other half of the private rule: a value carrying a private column
    /// may not be handed to a response builder.
    fn reject_private_response(&mut self, arg: Option<&Expr>, path: &str, span: Span) {
        let Some(arg) = arg else { return };
        let name = match &*arg.kind {
            ExprKind::Local(i) => i.name.clone(),
            _ => return,
        };
        if self.tainted.contains(&name) {
            self.err_note(
                span,
                "E0410",
                format!("`{path}(${name})` would send a `private` column"),
                "project only the fields the response needs: a private column may be \
                 read in code, never returned",
                "schema.md §3.1",
            );
        }
    }

    /// queries.md §6.2 — aggregates need a `group by`, and every
    /// non-aggregate projection field must be in it.
    fn check_aggregates(&mut self, s: &SelectExpr) {
        let Some(p) = &s.projection else { return };
        let mut aggregated = Vec::new();
        let mut plain = Vec::new();
        for f in &p.fields {
            match f {
                ProjField::Expr { alias, value, span } => {
                    if contains_aggregate(value) {
                        aggregated.push(alias.name.clone());
                    } else {
                        plain.push((alias.name.clone(), *span));
                    }
                }
                ProjField::Column(i) => plain.push((i.name.clone(), i.span)),
                ProjField::Nested { alias, span, .. } => plain.push((alias.name.clone(), *span)),
            }
        }
        if aggregated.is_empty() {
            return;
        }
        if s.group_by.is_empty() {
            // queries.md §6.2 allows the whole-table aggregate: "a query
            // that has a `group by`, **or that has exactly one binding and
            // no non-aggregate projection fields**". Only the first half
            // was implemented, so `as { total: count(A.id) }` — the way
            // you ask a table how many rows it has — was rejected.
            //
            // A join is excluded because a bare join fans out and the
            // count would silently be of the joined rows (§6.2, W0502).
            if plain.is_empty() && s.joins.is_empty() {
                return;
            }
            self.err_note(
                p.span,
                "E0530",
                "aggregates without a `group by`",
                "an aggregate projection needs `group by` naming every non-aggregate \
                 field",
                "queries.md §6.2",
            );
            return;
        }
        let grouped: HashSet<String> = s
            .group_by
            .iter()
            .filter_map(|g| match &*g.kind {
                ExprKind::Name(n) => Some(n.name.clone()),
                ExprKind::Field { field, .. } => Some(field.name.clone()),
                _ => None,
            })
            .collect();
        // A projection alias of a grouped column counts as grouped.
        //
        // `group by` above collects the *column* name from either spelling —
        // bare `status` or qualified `C.name` — so the alias map has to read
        // both too. It used to read only the bare one, which made
        // `group by T.column_id, C.name` + `as { column_name: C.name }` an
        // E0531 against a column that was plainly grouped: the alias mapped
        // to nothing, so `column_name` was looked up as if it were the
        // column. A projection that aliased a qualified column to its own
        // name worked by coincidence and one that renamed it did not.
        let aliases: HashMap<String, String> = p
            .fields
            .iter()
            .filter_map(|f| match f {
                ProjField::Expr { alias, value, .. } => match &*value.kind {
                    ExprKind::Name(n) => Some((alias.name.clone(), n.name.clone())),
                    ExprKind::Field { field, .. } => Some((alias.name.clone(), field.name.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        for (name, span) in plain {
            let source = aliases.get(&name).cloned().unwrap_or(name.clone());
            if !grouped.contains(&source) && !grouped.contains(&name) {
                self.err_note(
                    span,
                    "E0531",
                    format!("`{name}` is neither aggregated nor grouped"),
                    "add it to `group by`, or wrap it in an aggregate",
                    "queries.md §6.2",
                );
            }
        }

        self.check_fan_out(s);
    }

    /// queries.md §8.3 — a bounded page over a query that carries a
    /// collection must be provably pushed down.
    ///
    /// The emitter is the only thing that knows whether it can be, so it is
    /// asked rather than re-implemented here. Every other reason emission
    /// gives up is a missing feature, not a wrong program, and stays
    /// silent.
    fn check_pushdown(&mut self, s: &SelectExpr, plan: &crate::query::Plan, span: Span) {
        let mut c = crate::query_sql::Compiler::new(self.model);
        if c.compile(s, plan).is_some() {
            return;
        }
        if c.gap_code() != Some("E0542") {
            return;
        }
        self.err_note(
            span,
            "E0542",
            c.gap().to_string(),
            "queries.md §8.3",
            "queries.md §8.3",
        );
    }

    /// writes.md §6 — the one unchecked boundary, and the rules that keep
    /// it from being an interpolation hole.
    fn check_raw(&mut self, exprs: &[Expr], span: Span) {
        let Some(first) = exprs.first() else {
            self.err_note(
                span,
                "E0610",
                "`raw()` needs a SQL string",
                "the SQL is a literal so the placeholders can be counted; a computed                  string would be interpolation by another name",
                "writes.md §6.1",
            );
            return;
        };
        let ExprKind::Str(sql) = &*first.kind else {
            self.err_note(
                first.span,
                "E0610",
                "`raw()`'s SQL must be a literal",
                "a computed string cannot be checked, and checking it is the only                  thing standing between this and interpolation",
                "writes.md §6.1",
            );
            return;
        };
        let holes = sql.matches("{}").count();
        let args = exprs.len() - 1;
        if holes != args {
            self.err_note(
                span,
                "E0610",
                format!("`raw()` has {holes} placeholder(s) and {args} argument(s)"),
                "each `{}` binds one argument, in order",
                "writes.md §6.1",
            );
        }
        // A view is a snapshotted object; a hand-written body cannot be
        // diffed against the next one (writes.md §6.2).
        if self.body == BodyKind::View {
            self.err_note(
                span,
                "E0611",
                "`raw()` inside a `view`",
                "a view is snapshotted and diffed by the migration generator, which                  needs a body it can read",
                "writes.md §6.2",
            );
        }
    }

    /// queries.md §4.6 — a collection with no bound.
    ///
    /// A warning, not an error: "how many members can an org have" is the
    /// author's question, not the compiler's. But an unbounded collection
    /// inside a row is the shape that works on the test data and takes the
    /// process down on the one org with fifty thousand members, and it is
    /// invisible in the source unless something says so.
    fn check_unbounded(&mut self, s: &SelectExpr) {
        for j in &s.joins {
            let Some(r) = &j.result else { continue };
            if r.cardinality == Cardinality::Many && r.limit.is_none() {
                self.warn(
                    r.span,
                    "W0501",
                    format!("`{}` is a collection with no `limit`", r.name.name),
                    "queries.md §4.6",
                );
            }
        }
    }

    /// queries.md §6.2 — two bare joins fan out, so a plain `count` over
    /// either one counts the other's rows as well.
    ///
    /// A customer with 3 orders and 2 notes reports 6 orders. Nothing
    /// fails; the number is just wrong, and it is wrong in proportion to
    /// the other collection, so it looks plausible on small data and
    /// diverges on real data.
    fn check_fan_out(&mut self, s: &SelectExpr) {
        let bare = s
            .joins
            .iter()
            .filter(|j| {
                j.result
                    .as_ref()
                    .is_some_and(|r| r.cardinality == Cardinality::Group)
            })
            .count();
        if bare < 2 {
            return;
        }
        let Some(p) = &s.projection else { return };
        for f in &p.fields {
            let ProjField::Expr { value, .. } = f else {
                continue;
            };
            for span in plain_counts(value) {
                self.warn(
                    span,
                    "W0502",
                    format!("`count` under {bare} bare joins counts the other join's rows too"),
                    "queries.md §6.2",
                );
            }
        }
    }

    /// queries.md §5.2 — `first` needs a deterministic result.
    fn check_first_determinism(&mut self, s: &SelectExpr, object: &str, span: Span) {
        if !s.order_by.is_empty() {
            return;
        }
        // A whole-table aggregate returns exactly one row, so `first` on it
        // is already deterministic and there is no ordering to add — the
        // rule is about which of several rows you get.
        if s.group_by.is_empty() && s.joins.is_empty() && is_whole_table_aggregate(s) {
            return;
        }
        let Some(filter) = &s.filter else {
            self.err_note(
                span,
                "E0520",
                "`first` with no `where` and no `orderby`",
                "`first` must be deterministic: add an `orderby`",
                "queries.md §5.2",
            );
            return;
        };
        let equalities = equality_columns(filter);
        if self.covers_unique(object, &equalities, filter) {
            return;
        }
        self.err_note(
            span,
            "E0520",
            "`first` over a predicate that may match more than one row",
            "add an `orderby`, or constrain a primary key or unique constraint by \
             equality — an arbitrary row whose identity can change is not a result",
            "queries.md §5.2",
        );
    }

    fn covers_unique(&self, object: &str, equalities: &HashSet<String>, filter: &Expr) -> bool {
        // Views inherit the driving table's keys through their projection
        // (queries.md §5.2.1).
        let (table, mapped): (Option<&crate::symbols::TableSym>, HashSet<String>) =
            match self.sym.views.get(object) {
                Some(v) => {
                    let mapped = equalities
                        .iter()
                        .filter_map(|e| v.inherited.get(e).cloned())
                        .collect();
                    (self.sym.tables.get(&v.driving_table), mapped)
                }
                None => (self.sym.tables.get(object), equalities.clone()),
            };
        let Some(t) = table else { return false };

        if t.unique_sets
            .iter()
            .any(|set| set.iter().all(|c| mapped.contains(c)))
        {
            return true;
        }
        // A partial unique counts when the query's predicate implies its
        // own. Implication is conjunct containment over the **same**
        // canonicaliser the DDL uses (schema.md §4.3) — a second spelling
        // here would disagree with the index that was actually created.
        let table_name = t.declared.clone();
        let Some(model_table) = self
            .model
            .tables
            .iter()
            .find(|mt| mt.declared == table_name)
        else {
            return false;
        };
        let enums: std::collections::BTreeMap<String, crate::model::EnumObj> = self
            .model
            .enums
            .iter()
            .map(|e| (e.declared.clone(), e.clone()))
            .collect();
        let conjuncts: Vec<String> = split_conjuncts(filter)
            .iter()
            .map(|c| crate::model::canonical_expr(c, &model_table.columns, &enums))
            .collect();
        t.partial_uniques.iter().any(|(cols, pred)| {
            cols.iter().all(|c| mapped.contains(c))
                && pred
                    .split(" AND ")
                    .map(|p| p.trim().trim_start_matches('(').trim_end_matches(')'))
                    .all(|p| conjuncts.iter().any(|c| c == p))
        })
    }

    /// `where org_id == org_id` is now impossible to write by accident, but
    /// it is still possible to write on purpose (names.md §5.3).
    fn check_tautology(&mut self, e: &Expr) {
        if let ExprKind::Binary { op, lhs, rhs } = &*e.kind {
            if matches!(op, BinOp::Eq) {
                if let (ExprKind::Name(a), ExprKind::Name(b)) = (&*lhs.kind, &*rhs.kind) {
                    if a.name == b.name {
                        self.warn(
                            e.span,
                            "W0104",
                            format!("`{} == {}` is always true", a.name, b.name),
                            "names.md §5.3",
                        );
                    }
                }
            }
            if matches!(op, BinOp::And | BinOp::Or) {
                self.check_tautology(lhs);
                self.check_tautology(rhs);
            }
        }
    }

    // ------------------------------------------------------------ writes

    fn insert(&mut self, i: &InsertExpr, span: Span) -> Ty {
        let Some(object) = self.resolve_source(&i.table) else {
            return Ty::Unknown;
        };
        if self.sym.views.contains_key(&object) {
            self.err_note(
                i.table.span,
                "E0601",
                "cannot write to a view",
                "a view is a projection; write to the table underneath",
                "writes.md §1.1",
            );
            return Ty::Unknown;
        }

        self.query.push(QueryScope {
            bindings: vec![Binding {
                name: object.clone(),
                object: object.clone(),
                one_field: None,
            }],
        });
        let before = self.diags.len();
        let written = self.write_entries(&i.values, &object);
        // Only report missing columns when the entries themselves were
        // understood — otherwise one bad spread produces two diagnostics
        // and the second points nowhere useful.
        if self.diags.len() == before {
            self.check_required_columns(&object, &written, span);
        }

        let shape = i.projection.as_ref().map(|p| {
            let s = SelectExpr {
                binder: Ident::new(object.clone(), span),
                source: i.table.clone(),
                joins: vec![],
                filter: None,
                group_by: vec![],
                having: None,
                projection: None,
                order_by: vec![],
                limit: None,
                page: None,
                first: false,
                span,
            };
            self.projection(p, &object, &s)
        });
        self.query.pop();

        if let Some(c) = &i.conflict {
            self.check_conflict_target(c, &object);
        }

        match shape {
            Some(f) => {
                let rec = Ty::Record(f);
                // `on conflict do nothing` may produce no row (writes.md §2.3).
                match &i.conflict {
                    Some(c) if matches!(c.action, ConflictAction::DoNothing) => rec.opt(),
                    _ => rec,
                }
            }
            None => Ty::Void,
        }
    }

    /// A `*_hash` column on one side and a fresh password hash on the
    /// other.
    fn hash_column_compared_to_a_new_hash(&self, lhs: &Expr, rhs: &Expr) -> Option<String> {
        let hash_name = |e: &Expr| -> Option<String> {
            let name = match &*e.kind {
                ExprKind::Name(n) => n.name.clone(),
                ExprKind::Field { field, .. } => field.name.clone(),
                _ => return None,
            };
            name.ends_with("_hash").then_some(name)
        };
        let fresh = |e: &Expr| -> bool {
            match &*e.kind {
                ExprKind::Local(n) => self.password_hashed.contains(&n.name),
                ExprKind::Call { callee, .. } => {
                    matches!(&*callee.kind, ExprKind::Field { base, field }
                        if field.name == "password"
                            && matches!(&*base.kind, ExprKind::Name(b) if b.name == "hash"))
                }
                _ => false,
            }
        };
        match (hash_name(lhs), fresh(rhs), hash_name(rhs), fresh(lhs)) {
            (Some(c), true, _, _) | (_, _, Some(c), true) => Some(c),
            _ => None,
        }
    }

    /// The name of an untyped path parameter on either side of a
    /// comparison, if there is one.
    fn untyped_operand(&self, lhs: &Expr, rhs: &Expr) -> Option<String> {
        [lhs, rhs].into_iter().find_map(|e| match &*e.kind {
            ExprKind::PathParam(n) if self.untyped_params.contains(&n.name) => Some(n.name.clone()),
            _ => None,
        })
    }

    /// True when an expression reaches `request.path()`, directly or
    /// through a local it was assigned to.
    fn reads_request_path(&self, e: &Expr) -> bool {
        match &*e.kind {
            ExprKind::Local(n) => self.path_keyed.contains(&n.name),
            ExprKind::Call { callee, .. } => {
                matches!(&*callee.kind, ExprKind::Field { base, field }
                    if field.name == "path"
                        && matches!(&*base.kind, ExprKind::Name(b) if b.name == "request"))
            }
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
                self.reads_request_path(lhs) || self.reads_request_path(rhs)
            }
            _ => false,
        }
    }

    /// writes.md §2.4–§2.5 — `on conflict`'s target must be a real unique
    /// constraint.
    ///
    /// Postgres rejects a target it cannot match to an index, so this is a
    /// runtime 500 otherwise — on the concurrent path, which is the one
    /// that is hardest to reach in testing and the reason `on conflict` is
    /// there at all.
    fn check_conflict_target(&mut self, c: &ConflictClause, object: &str) {
        let Some(t) = self.sym.tables.get(object) else {
            return;
        };
        if c.columns.is_empty() {
            // §2.5 — omitting the list is legal only when there is exactly
            // one unique constraint to mean.
            let total = t.unique_sets.len() + t.partial_uniques.len();
            if total != 1 {
                self.err_note(
                    c.span,
                    "E0604",
                    format!(
                        "`on conflict` with no columns on a table with {total} unique \
                         constraint(s)"
                    ),
                    "name the columns: `on conflict (provider_ref) …`",
                    "writes.md §2.5",
                );
            }
            return;
        }
        let want: Vec<String> = c.columns.iter().map(|i| i.name.clone()).collect();
        let matches =
            |set: &Vec<String>| set.len() == want.len() && set.iter().all(|c| want.contains(c));
        if t.unique_sets.iter().any(matches)
            || t.partial_uniques.iter().any(|(cols, _)| matches(cols))
        {
            return;
        }
        self.err_note(
            c.span,
            "E0603",
            format!(
                "`({})` is not a unique constraint on `{object}`",
                want.join(", ")
            ),
            "`on conflict` needs an index to match against: declare \
             `unique (…)` on exactly these columns",
            "writes.md §2.4",
        );
    }

    fn update(&mut self, u: &UpdateExpr, span: Span) -> Ty {
        let Some(object) = self.resolve_source(&u.table) else {
            return Ty::Unknown;
        };
        if self.sym.views.contains_key(&object) {
            self.err_note(
                u.table.span,
                "E0601",
                "cannot write to a view",
                "a view is a projection; write to the table underneath",
                "writes.md §1.1",
            );
            return Ty::Unknown;
        }
        if u.filter.is_none() {
            self.err_note(
                span,
                "E0605",
                "`update` with no `where`",
                "there is no accidental whole-table update; write `where true` to \
                 mean it",
                "writes.md §3.4",
            );
        }

        self.query.push(QueryScope {
            bindings: vec![Binding {
                name: object.clone(),
                object: object.clone(),
                one_field: None,
            }],
        });
        for item in &u.sets {
            match item {
                SetItem::Set {
                    column,
                    value,
                    span,
                    ..
                } => {
                    let got = self.expr(value);
                    self.check_write_column(&object, &column.name, &got, *span);
                }
                SetItem::Spread {
                    source,
                    except,
                    span,
                } => {
                    self.check_spread(&object, source, except, *span);
                }
            }
        }
        if let Some(f) = &u.filter {
            let t = self.expr(f);
            self.require_boolean(&t, f.span);
        }
        let shape = u.projection.as_ref().map(|p| {
            let s = SelectExpr {
                binder: Ident::new(object.clone(), span),
                source: u.table.clone(),
                joins: vec![],
                filter: u.filter.clone(),
                group_by: vec![],
                having: None,
                projection: None,
                order_by: u.order_by.clone(),
                limit: None,
                page: None,
                first: u.first,
                span,
            };
            self.projection(p, &object, &s)
        });
        if u.first {
            let probe = SelectExpr {
                binder: Ident::new(object.clone(), span),
                source: u.table.clone(),
                joins: vec![],
                filter: u.filter.clone(),
                group_by: vec![],
                having: None,
                projection: None,
                order_by: u.order_by.clone(),
                limit: None,
                page: None,
                first: true,
                span,
            };
            self.check_first_determinism(&probe, &object, span);
        }
        self.query.pop();

        match shape {
            Some(f) => {
                let rec = Ty::Record(f);
                if u.first {
                    rec.opt()
                } else {
                    rec.array()
                }
            }
            None => Ty::Void,
        }
    }

    fn delete(&mut self, d: &DeleteExpr, span: Span) -> Ty {
        let Some(object) = self.resolve_source(&d.table) else {
            return Ty::Unknown;
        };
        if self.sym.views.contains_key(&object) {
            self.err_note(
                d.table.span,
                "E0601",
                "cannot write to a view",
                "a view is a projection; write to the table underneath",
                "writes.md §1.1",
            );
            return Ty::Unknown;
        }
        if d.filter.is_none() {
            self.err_note(
                span,
                "E0605",
                "`delete` with no `where`",
                "there is no accidental whole-table delete; write `where true` to \
                 mean it",
                "writes.md §5.3",
            );
        }
        self.query.push(QueryScope {
            bindings: vec![Binding {
                name: object.clone(),
                object: object.clone(),
                one_field: None,
            }],
        });
        if let Some(f) = &d.filter {
            let t = self.expr(f);
            self.require_boolean(&t, f.span);
        }
        let shape = d.projection.as_ref().map(|p| {
            let s = SelectExpr {
                binder: Ident::new(object.clone(), span),
                source: d.table.clone(),
                joins: vec![],
                filter: d.filter.clone(),
                group_by: vec![],
                having: None,
                projection: None,
                order_by: d.order_by.clone(),
                limit: None,
                page: None,
                first: d.first,
                span,
            };
            self.projection(p, &object, &s)
        });
        if d.first {
            let probe = SelectExpr {
                binder: Ident::new(object.clone(), span),
                source: d.table.clone(),
                joins: vec![],
                filter: d.filter.clone(),
                group_by: vec![],
                having: None,
                projection: None,
                order_by: d.order_by.clone(),
                limit: None,
                page: None,
                first: true,
                span,
            };
            self.check_first_determinism(&probe, &object, span);
        }
        self.query.pop();

        match shape {
            Some(f) => {
                let rec = Ty::Record(f);
                if d.first {
                    rec.opt()
                } else {
                    rec.array()
                }
            }
            None => Ty::Void,
        }
    }

    /// Returns the set of columns the entries write.
    fn write_entries(&mut self, entries: &[ObjEntry], object: &str) -> HashSet<String> {
        let mut written = HashSet::new();
        for e in entries {
            match e {
                ObjEntry::Field {
                    key, value, span, ..
                } => {
                    let got = self.expr(value);
                    self.check_write_column(object, &key.name, &got, *span);
                    written.insert(key.name.clone());
                }
                ObjEntry::Spread {
                    source,
                    except,
                    span,
                } => {
                    for name in self.check_spread(object, source, except, *span) {
                        written.insert(name);
                    }
                }
            }
        }
        written
    }

    fn check_write_column(&mut self, object: &str, column: &str, got: &Ty, span: Span) {
        let Some(t) = self.sym.tables.get(object) else {
            return;
        };
        let Some(want) = t.column(column).cloned() else {
            let have = t
                .columns
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.err_note(
                span,
                "E0602",
                format!("`{column}` is not a column of `{object}`"),
                format!("columns: {have}"),
                "writes.md §1.3",
            );
            return;
        };
        if !got.assignable_to(&want) {
            self.err_note(
                span,
                "E0606",
                format!("`{got}` is not assignable to `{column}: {want}`"),
                if got.is_optional() && !want.is_optional() {
                    "the column is NOT NULL; guard the value or use `=?`"
                } else {
                    "types.md §10.3 lists the assignability rules"
                },
                "types.md §10.3",
            );
        }
    }

    /// types.md §9.3, §9.4 — every non-transient field needs a column, and
    /// `private` / `server` columns are unreachable through a spread.
    fn check_spread(
        &mut self,
        object: &str,
        source: &Ident,
        except: &[Ident],
        span: Span,
    ) -> Vec<String> {
        let ty = self.spread_source(source, span);
        let Some(fields) = ty.fields().cloned() else {
            return Vec::new();
        };
        let excluded: HashSet<&str> = except.iter().map(|i| i.name.as_str()).collect();
        for ex in except {
            if !fields.iter().any(|(n, _)| n == &ex.name) {
                let have = fields
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.err_note(
                    ex.span,
                    "E0341",
                    format!("`{}` is not a field of `${}`", ex.name, source.name),
                    format!("fields: {have}"),
                    "types.md §9.1",
                );
            }
        }

        // `transient` marks a class field with no column (types.md §9.3),
        // so it drops out of the spread rather than failing it.
        let transient: HashSet<String> = match self.lookup(&source.name).map(|t| t.strip_opt()) {
            Some(Ty::Class(c)) => self
                .sym
                .classes
                .get(&c)
                .map(|c| {
                    c.fields
                        .iter()
                        .filter(|f| f.transient)
                        .map(|f| f.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            _ => HashSet::new(),
        };

        let Some(table) = self.sym.tables.get(object).cloned() else {
            return Vec::new();
        };
        let mut written = Vec::new();
        for (name, fty) in &fields {
            if excluded.contains(name.as_str()) || transient.contains(name) {
                continue;
            }
            if table.is_private(name) || table.is_server(name) {
                self.err_note(
                    span,
                    "E0342",
                    format!(
                        "`{name}` is a `{}` column",
                        if table.is_private(name) {
                            "private"
                        } else {
                            "server"
                        }
                    ),
                    "mass assignment is closed at the language level: name it \
                     explicitly, or drop it with `except (…)`",
                    "types.md §9.4",
                );
                continue;
            }
            match table.column(name) {
                Some(want) => {
                    if !fty.assignable_to(want) && !fty.strip_opt().assignable_to(want) {
                        self.err_note(
                            span,
                            "E0606",
                            format!("`${}.{name}` is `{fty}`, column is `{want}`", source.name),
                            "the spread's field type and the column type must match",
                            "types.md §9.3",
                        );
                    }
                    written.push(name.clone());
                }
                None => {
                    let have = table
                        .columns
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.err_note(
                        span,
                        "E0305",
                        format!("`{name}` has no column in `{object}`"),
                        format!(
                            "mark the field `transient`, or drop it at the site with \
                             `except ({name})` — columns: {have}"
                        ),
                        "types.md §9.3",
                    );
                }
            }
        }
        written
    }

    /// A NOT NULL column with no default must be written (types.md §9.5).
    fn check_required_columns(&mut self, object: &str, written: &HashSet<String>, span: Span) {
        let Some(t) = self.sym.tables.get(object).cloned() else {
            return;
        };
        let model_table = self.model_table(object);
        let mut missing = Vec::new();
        for (name, ty) in &t.columns {
            if written.contains(name) || ty.is_optional() {
                continue;
            }
            let has_default = model_table
                .as_ref()
                .and_then(|m| m.iter().find(|(n, _, _)| n == name))
                .map(|(_, d, ident)| *d || *ident)
                .unwrap_or(false);
            if !has_default {
                missing.push(name.clone());
            }
        }
        if !missing.is_empty() {
            self.err_note(
                span,
                "E0343",
                format!("`{object}` needs {}", missing.join(", ")),
                "these columns are NOT NULL with no default, so the insert cannot \
                 leave them unset",
                "types.md §9.5",
            );
        }
    }

    /// (column, has_default, is_identity) — read from the schema model via
    /// the symbol table's owner.
    fn model_table(&self, object: &str) -> Option<Vec<(String, bool, bool)>> {
        self.sym
            .tables
            .get(object)
            .map(|_| self.sym.table_defaults(object))
    }
}

// ---------------------------------------------------------------- helpers

fn class_fields(c: &ClassSym) -> Fields {
    c.fields
        .iter()
        .map(|f| (f.name.clone(), f.ty.clone()))
        .collect()
}

fn base_of(t: &Ty) -> Ty {
    match t {
        Ty::Optional(inner) | Ty::Array(inner) => base_of(inner),
        other => other.clone(),
    }
}

fn is_namespace(name: &str) -> bool {
    matches!(
        name,
        "date"
            | "string"
            | "array"
            | "hash"
            | "jwt"
            | "crypto"
            | "request"
            | "response"
            | "context"
            | "redis"
            | "mail"
            | "count"
            | "App"
    )
}

/// types.md §6.3 — `sum` moves one step up the numeric ladder and stops at
/// `numeric`, which is unbounded.
fn widen_sum(t: &Ty) -> Ty {
    match t {
        Ty::Scalar(Scalar::Smallint) | Ty::Scalar(Scalar::Int) => Ty::bigint(),
        Ty::Scalar(Scalar::Bigint) | Ty::Scalar(Scalar::Numeric) => Ty::numeric(),
        other => other.clone(),
    }
}

/// Spans of every non-distinct `count(...)` in an expression.
fn plain_counts(e: &Expr) -> Vec<Span> {
    let mut out = Vec::new();
    walk_counts(e, &mut out);
    out
}

fn walk_counts(e: &Expr, out: &mut Vec<Span>) {
    if let ExprKind::Call {
        callee,
        args,
        filter,
    } = &*e.kind
    {
        if matches!(&*callee.kind, ExprKind::Name(n) if n.name == "count") {
            out.push(e.span);
        }
        for a in args {
            walk_counts(a, out);
        }
        if let Some(f) = filter {
            walk_counts(f, out);
        }
        return;
    }
    match &*e.kind {
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            walk_counts(lhs, out);
            walk_counts(rhs, out);
        }
        ExprKind::Unary { rhs, .. } => walk_counts(rhs, out),
        ExprKind::Ternary {
            cond,
            then,
            otherwise,
        } => {
            walk_counts(cond, out);
            walk_counts(then, out);
            walk_counts(otherwise, out);
        }
        _ => {}
    }
}

/// True when a name is a projection field this query produces as a
/// collection — either a join result written here, or a view column the
/// source already carries.
fn collection_field(s: &SelectExpr, name: &str) -> bool {
    s.joins.iter().any(|j| {
        j.result
            .as_ref()
            .is_some_and(|r| r.cardinality == Cardinality::Many && r.name.name == name)
    })
}

/// The response builders that carry a 4xx/5xx status.
///
/// `statusCode(n, …)` is included when `n` is a literal in that range —
/// a computed status is the author saying they know what they are doing.
fn error_builder(e: &Expr) -> Option<String> {
    let ExprKind::Call { callee, args, .. } = &*e.kind else {
        return None;
    };
    let ExprKind::Name(n) = &*callee.kind else {
        return None;
    };
    match n.name.as_str() {
        "badRequest" | "unauthorized" | "forbidden" | "notFound" | "conflict"
        | "tooManyRequests" | "internalError" => Some(n.name.clone()),
        "statusCode" => match args.first().map(|a| &*a.kind) {
            Some(ExprKind::Int(v)) if v.parse::<u16>().is_ok_and(|s| s >= 400) => {
                Some(format!("statusCode({v}"))
            }
            _ => None,
        },
        _ => None,
    }
}

fn is_aggregate(name: &str) -> bool {
    matches!(name, "count" | "sum" | "min" | "max" | "avg")
}

fn contains_aggregate(e: &Expr) -> bool {
    match &*e.kind {
        ExprKind::Call { callee, args, .. } => {
            let hit = match &*callee.kind {
                ExprKind::Name(n) => is_aggregate(&n.name),
                ExprKind::Field { base, field } => {
                    matches!(&*base.kind, ExprKind::Name(n) if n.name == "count")
                        && field.name == "distinct"
                }
                _ => false,
            };
            hit || args.iter().any(contains_aggregate)
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            contains_aggregate(lhs) || contains_aggregate(rhs)
        }
        ExprKind::Unary { rhs, .. } => contains_aggregate(rhs),
        _ => false,
    }
}

/// Columns constrained by equality against something that is not a column.
fn equality_columns(e: &Expr) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_equalities(e, &mut out);
    out
}

fn collect_equalities(e: &Expr, out: &mut HashSet<String>) {
    let ExprKind::Binary { op, lhs, rhs } = &*e.kind else {
        return;
    };
    match op {
        BinOp::And => {
            collect_equalities(lhs, out);
            collect_equalities(rhs, out);
        }
        BinOp::Eq => {
            // `col == <not a column, not null>` pins one value.
            if let ExprKind::Name(n) = &*lhs.kind {
                if !matches!(&*rhs.kind, ExprKind::Name(_) | ExprKind::Null) {
                    out.insert(n.name.clone());
                }
            }
            if let ExprKind::Field { field, .. } = &*lhs.kind {
                if !matches!(&*rhs.kind, ExprKind::Null) {
                    out.insert(field.name.clone());
                }
            }
        }
        _ => {}
    }
}

/// Top-level `and` conjuncts of a predicate.
fn split_conjuncts(e: &Expr) -> Vec<Expr> {
    let mut out = Vec::new();
    fn walk(e: &Expr, out: &mut Vec<Expr>) {
        if let ExprKind::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } = &*e.kind
        {
            walk(lhs, out);
            walk(rhs, out);
        } else {
            out.push(e.clone());
        }
    }
    walk(e, &mut out);
    out
}

/// `if (x == null)` / `if (x != null)` — the guard forms narrowing
/// recognises (types.md §6.6).
fn narrowing_target(cond: &Expr, want_is_null: bool) -> Option<String> {
    let ExprKind::Binary { op, lhs, rhs } = &*cond.kind else {
        return None;
    };
    let is_null_test = match op {
        BinOp::Eq => true,
        BinOp::Ne => false,
        _ => return None,
    };
    if is_null_test != want_is_null {
        return None;
    }
    let name = |e: &Expr| match &*e.kind {
        ExprKind::Local(i) => Some(i.name.clone()),
        _ => None,
    };
    match (name(lhs), &*rhs.kind) {
        (Some(n), ExprKind::Null) => Some(n),
        _ => match (&*lhs.kind, name(rhs)) {
            (ExprKind::Null, Some(n)) => Some(n),
            _ => None,
        },
    }
}

/// Every path through the block ends in `return`, `throw`, `break` or
/// `continue` (types.md §6.6).
/// A projection that is entirely aggregates — `as { total: count(x) }`.
/// Such a query answers exactly one row whatever the table holds.
fn is_whole_table_aggregate(s: &SelectExpr) -> bool {
    let Some(p) = &s.projection else { return false };
    !p.fields.is_empty()
        && p.fields.iter().all(|f| match f {
            ProjField::Expr { value, .. } => contains_aggregate(value),
            _ => false,
        })
}

fn diverges(b: &Block) -> bool {
    b.iter().any(|s| match s {
        Stmt::Return { .. } | Stmt::Throw { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {
            true
        }
        Stmt::If {
            then, otherwise, ..
        } => otherwise
            .as_ref()
            .is_some_and(|alt| diverges(then) && diverges(alt)),
        Stmt::Transaction { body, .. } => diverges(body),
        _ => false,
    })
}

/// Path parameters declared in a `routes` prefix or `route` suffix
/// (routing.md §3.1).
/// The `{name}` slots written with no `: type`.
fn untyped_path_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        if !inner.contains(':') {
            out.push(inner.trim().to_string());
        }
        rest = &rest[open + close + 1..];
    }
    out
}

/// A literal status in `statusCode(n, v)` / `redirect(n, url)`. A computed
/// one has no single answer to document, and is left out rather than
/// guessed.
/// `text/*` without a charset is the one ambiguity worth closing at the
/// language rather than leaving to each caller: a browser that guesses the
/// encoding of an HTML page guesses wrong on the first non-ASCII byte.
/// Everything else is passed through exactly as written.
pub fn normalize_media(mime: &str) -> String {
    let m = mime.trim();
    if m.starts_with("text/") && !m.to_ascii_lowercase().contains("charset=") {
        format!("{m}; charset=utf-8")
    } else {
        m.to_string()
    }
}

fn literal_status(e: &Expr) -> Option<u16> {
    match &*e.kind {
        // The lexer keeps integer literals as text so a `bigint` never
        // passes through an `i64` on the way in.
        ExprKind::Int(n) => n.parse().ok(),
        _ => None,
    }
}

fn path_params(path: &str) -> Vec<(String, Ty)> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        let (name, ty) = match inner.split_once(':') {
            Some((n, t)) => (
                n.trim().to_string(),
                Scalar::from_name(t.trim())
                    .map(Ty::Scalar)
                    .unwrap_or(Ty::text()),
            ),
            // routing.md §3.1 — an untyped parameter defaults to text.
            None => (inner.trim().to_string(), Ty::text()),
        };
        out.push((name, ty));
        rest = &rest[open + close + 1..];
    }
    out
}

fn callee_path(e: &Expr) -> Option<String> {
    match &*e.kind {
        ExprKind::Name(i) => Some(i.name.clone()),
        ExprKind::Field { base, field } => {
            let b = callee_path(base)?;
            Some(format!("{b}.{}", field.name))
        }
        _ => None,
    }
}

use crate::token::Span;
