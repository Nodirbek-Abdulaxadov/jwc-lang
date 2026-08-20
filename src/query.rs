//! The query plan: bindings, the join attachment tree, and its validation.
//!
//! A query is a **tree**, not a list. `OrgWithMembers` joins members to the
//! org and accounts to the members; the projection nests the same way. Which
//! join hangs off which was previously positional and undeclared (N12) —
//! `as one account` landed inside `as many members` only because its `on`
//! clause happened to mention `Members`. Here it is resolved explicitly and
//! an ambiguity is an error with `under <alias>` as the fix.
//!
//! SQL emission reads this tree (25.b onward); nothing here touches SQL.

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::symbols::Symbols;
use crate::token::Span;
use std::collections::BTreeSet;

/// The root of a query plan.
pub struct Plan {
    pub root: Node,
    /// Joins that only feed filtering and aggregates (queries.md §6.2).
    pub groups: Vec<GroupJoin>,
    pub diags: Vec<Diagnostic>,
}

pub struct Node {
    /// The binding name — the alias if one was written, else the table's
    /// declared name. It is the **only** name for this binding: the table
    /// name is not a second one (queries.md §4.2).
    pub alias: String,
    /// Declared table or view name.
    pub object: String,
    /// How this node relates to its parent. `None` on the root.
    pub link: Option<Link>,
    pub children: Vec<Node>,
    pub span: Span,
}

pub struct Link {
    pub kind: JoinKind,
    pub cardinality: Cardinality,
    /// The projection field this node produces.
    pub field: String,
    pub on: Expr,
    /// A `where` on the join — filters the child collection, not the
    /// driving rows.
    pub filter: Option<Expr>,
    pub order_by: Vec<SortKey>,
    pub limit: Option<Expr>,
}

pub struct GroupJoin {
    pub alias: String,
    pub object: String,
    pub kind: JoinKind,
    pub on: Expr,
    pub filter: Option<Expr>,
    pub span: Span,
}

impl Node {
    pub fn find(&self, alias: &str) -> Option<&Node> {
        if self.alias == alias {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(alias))
    }

    /// `under` may name either the binding alias (`M`) or the projection
    /// field it produces (`members`). The two are different names for the
    /// same node and a reader thinks in whichever the surrounding code uses,
    /// so both resolve. Alias wins on a collision.
    pub fn resolve(&self, name: &str) -> Option<&Node> {
        self.find(name).or_else(|| self.find_by_field(name))
    }

    fn find_by_field(&self, field: &str) -> Option<&Node> {
        if self.link.as_ref().is_some_and(|l| l.field == field) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find_by_field(field))
    }

    /// Every binding in the subtree, depth-first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }

    /// True when this node or any descendant is a collection. A `many`
    /// anywhere is what forces the two-stage pushdown (queries.md §8.3).
    pub fn has_many(&self) -> bool {
        self.children.iter().any(|c| {
            matches!(
                c.link.as_ref().map(|l| l.cardinality),
                Some(Cardinality::Many)
            ) || c.has_many()
        })
    }
}

/// Build the plan, reporting every attachment problem.
pub fn plan(select: &SelectExpr, sym: &Symbols) -> Plan {
    let mut diags = Vec::new();

    let root_object = sym
        .by_path
        .get(&select.source.text())
        .cloned()
        .unwrap_or_else(|| select.source.object.name.clone());

    let mut root = Node {
        alias: select.binder.name.clone(),
        object: root_object,
        link: None,
        children: Vec::new(),
        span: select.binder.span,
    };
    let mut groups = Vec::new();

    // Joins are attached in written order, so a join may hang off one that
    // came before it — which is how `account` reaches `members`.
    for j in &select.joins {
        let Some(result) = &j.result else {
            // queries.md §4.3 — a join always states what it produces.
            // "I forgot the projection" and "I meant to aggregate" used to
            // be the same syntax.
            diags.push(
                Diagnostic::error("E0535", j.span, "this join does not say what it produces")
                    .note(
                        "write `as one <name>`, `as many <name> orderby …`, or `as group` \
                     when the join exists only to feed aggregates",
                    )
                    .clause("queries.md §4.3"),
            );
            continue;
        };
        let object = sym
            .by_path
            .get(&j.table.text())
            .cloned()
            .unwrap_or_else(|| j.table.object.name.clone());

        if result.cardinality == Cardinality::Group {
            groups.push(GroupJoin {
                alias: j.binder.name.clone(),
                object,
                kind: j.kind,
                on: j.on.clone(),
                filter: j.filter.clone(),
                span: j.span,
            });
            continue;
        }

        // queries.md §5.3 — a collection with no ordering returns its
        // elements in whatever order the plan produced, and that changes.
        if result.cardinality == Cardinality::Many && result.order_by.is_empty() {
            diags.push(
                Diagnostic::error(
                    "E0536",
                    result.span,
                    format!("`as many {}` has no `orderby`", result.name.name),
                )
                .note(
                    "a collection needs a stated order: without one the elements come \
                     back in whatever order the plan produced, and that changes with \
                     the data",
                )
                .clause("queries.md §4.6"),
            );
        }

        let parent = match &result.under {
            // An explicit parent is taken as written; it only has to exist.
            Some(u) => {
                if let Some(node) = root.resolve(&u.name) {
                    node.alias.clone()
                } else {
                    diags.push(
                        Diagnostic::error(
                            "E0511",
                            u.span,
                            format!("`under {}` names no binding in this query", u.name),
                        )
                        .note("name the binding alias, or the field its join produces")
                        .clause("queries.md §4.4"),
                    );
                    root.alias.clone()
                }
            }
            None => {
                let mut referenced = referenced_bindings(&j.on);
                referenced.remove(&j.binder.name);
                let mut candidates: Vec<String> = referenced
                    .into_iter()
                    .filter(|r| root.resolve(r).is_some())
                    .collect();
                candidates.sort();
                match candidates.len() {
                    1 => candidates.remove(0),
                    0 => root.alias.clone(),
                    _ => {
                        diags.push(
                            Diagnostic::error(
                                "E0510",
                                j.span,
                                format!(
                                    "`{}` could attach to {}",
                                    result.name.name,
                                    candidates
                                        .iter()
                                        .map(|c| format!("`{c}`"))
                                        .collect::<Vec<_>>()
                                        .join(" or ")
                                ),
                            )
                            .note(format!(
                                "the projection tree follows the attachment, so it has \
                                 to be stated: `as {} {} under {}`",
                                cardinality_word(result.cardinality),
                                result.name.name,
                                candidates[0]
                            ))
                            .clause("queries.md §4.4"),
                        );
                        candidates.remove(0)
                    }
                }
            }
        };

        let node = Node {
            alias: j.binder.name.clone(),
            object,
            link: Some(Link {
                kind: j.kind,
                cardinality: result.cardinality,
                field: result.name.name.clone(),
                on: j.on.clone(),
                filter: j.filter.clone(),
                order_by: result.order_by.clone(),
                limit: result.limit.clone(),
            }),
            children: Vec::new(),
            span: j.span,
        };

        if !attach(&mut root, &parent, node) {
            diags.push(
                Diagnostic::error(
                    "E0511",
                    j.span,
                    format!("`{parent}` is not a binding in this query"),
                )
                .clause("queries.md §4.4"),
            );
        }
    }

    // queries.md §6.2 — aggregation and a collection in one query is a real
    // design question, and a silently multiplied `count` is not an answer.
    if !groups.is_empty() && root.has_many() {
        diags.push(
            Diagnostic::error(
                "E0532",
                select.span,
                "a query cannot both aggregate and carry an `as many` collection",
            )
            .note(
                "split it in two: one query for the aggregate, one for the children \
                 (DEFERRED-12)",
            )
            .clause("queries.md §6.2"),
        );
    }

    Plan {
        root,
        groups,
        diags,
    }
}

fn cardinality_word(c: Cardinality) -> &'static str {
    match c {
        Cardinality::One => "one",
        Cardinality::Many => "many",
        Cardinality::Group => "group",
    }
}

/// Attach `child` under the binding named `parent`. The path is found
/// immutably first, then walked once mutably — a recursive `&mut` search
/// would have to give the child back on every miss.
fn attach(root: &mut Node, parent: &str, child: Node) -> bool {
    let Some(path) = path_to(root, parent) else {
        return false;
    };
    let mut node = root;
    for i in path {
        node = &mut node.children[i];
    }
    node.children.push(child);
    true
}

fn path_to(node: &Node, alias: &str) -> Option<Vec<usize>> {
    if node.alias == alias {
        return Some(Vec::new());
    }
    for (i, c) in node.children.iter().enumerate() {
        if let Some(mut rest) = path_to(c, alias) {
            let mut path = vec![i];
            path.append(&mut rest);
            return Some(path);
        }
    }
    None
}

/// Binding names an `on` clause references through `Alias.column`.
fn referenced_bindings(e: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(e, &mut out);
    out
}

fn walk(e: &Expr, out: &mut BTreeSet<String>) {
    match &*e.kind {
        ExprKind::Field { base, .. } => {
            if let ExprKind::Name(n) = &*base.kind {
                out.insert(n.name.clone());
            } else {
                walk(base, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Coalesce { lhs, rhs } => {
            walk(lhs, out);
            walk(rhs, out);
        }
        ExprKind::Unary { rhs, .. } => walk(rhs, out),
        ExprKind::Call { args, .. } => {
            for a in args {
                walk(a, out);
            }
        }
        ExprKind::In { lhs, items, .. } => {
            walk(lhs, out);
            for i in items {
                walk(i, out);
            }
        }
        _ => {}
    }
}
