//! `jwc openapi` — an OpenAPI 3.1 document, derived and never authored.
//!
//! Every part of it already exists in the compiler: the route table
//! (routing.md §5), typed path parameters (§3.1), the `class` a route
//! validates its body against (types.md §4.1), the type of each response
//! builder's payload, and the raise set that decides which non-2xx statuses
//! a route can produce (errors.md §3, §4.3). This module arranges them; it
//! infers nothing of its own.
//!
//! ## Two rules that make the document truthful
//!
//! **A `Raw` response has no schema.** It is emitted as
//! `application/json` with no `schema` at all, because the compiler did not
//! check that shape either (types.md §5.1). Writing a plausible object there
//! would be the document asserting something the type system refused to.
//!
//! **Scalars map to their wire form, not their Postgres form** (types.md
//! §2.3). `bigint` and `numeric` are `{"type": "string"}` because that is
//! what the runtime sends — JavaScript loses digits above 2^53, and no float
//! ever touches money.

use crate::check::{Checked, RouteResponse};
use crate::symbols::{ClassSym, Symbols};
use crate::types::{Scalar, Ty};
use crate::wiring::{ResolvedRoute, Wired};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub struct Input<'a> {
    pub title: String,
    pub version: String,
    pub sym: &'a Symbols,
    pub wired: &'a Wired,
    pub checked: &'a Checked,
    /// Per route, the declared errors it can raise (errors.md §3).
    pub raises: BTreeMap<String, Vec<String>>,
}

pub fn document(input: &Input) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    let mut routes: Vec<&ResolvedRoute> = input.wired.routes.iter().collect();
    routes.sort_by(|a, b| (&a.pattern, &a.method).cmp(&(&b.pattern, &b.method)));

    for r in routes {
        let key = format!("{} {}", r.method, r.pattern);
        let mut op: Map<String, Value> = Map::new();
        op.insert("operationId".into(), json!(operation_id(r)));

        if !r.params.is_empty() {
            let params: Vec<Value> = r
                .params
                .iter()
                .map(|(name, ty)| {
                    json!({
                        "name": name,
                        "in": "path",
                        "required": true,
                        "schema": scalar_schema(ty),
                    })
                })
                .collect();
            op.insert("parameters".into(), json!(params));
        }

        if let Some((_, class)) = input
            .checked
            .request_bodies
            .iter()
            .find(|(route, _)| route == &key)
        {
            used.insert(class.clone());
            op.insert(
                "requestBody".into(),
                json!({
                    "required": true,
                    "content": { "application/json": { "schema": reference(class) } },
                }),
            );
        }

        let mut responses: Map<String, Value> = Map::new();
        for resp in input
            .checked
            .responses
            .iter()
            .filter(|x| x.route == key)
        {
            let (body, mut named) = response_body(input.sym, resp);
            used.append(&mut named);
            responses.insert(resp.status.to_string(), body);
        }

        // Everything the route can raise, whether or not an `errorHandler`
        // arm names it: a declared error's default status is what makes an
        // arm optional (errors.md §4.3), so the status is known either way.
        for name in input.raises.get(&key).into_iter().flatten() {
            let Some(e) = input.sym.errors.get(name) else {
                continue;
            };
            responses
                .entry(e.status.to_string())
                .or_insert_with(|| error_response(name));
        }
        if responses.is_empty() {
            responses.insert("200".into(), json!({ "description": "OK" }));
        }
        op.insert("responses".into(), Value::Object(responses));

        if !r.chain.is_empty() {
            // Not an OpenAPI concept, but the chain is what decides whether
            // a call needs a token, and a reader of the document has no
            // other way to find out.
            op.insert(
                "x-jwc-middleware".into(),
                json!(r.chain.iter().collect::<Vec<_>>()),
            );
        }

        let entry = paths
            .entry(r.pattern.clone())
            .or_insert_with(|| json!({}));
        if let Some(o) = entry.as_object_mut() {
            o.insert(r.method.to_lowercase(), Value::Object(op));
        }
    }

    // Schemas are emitted for what the paths actually reference, plus
    // whatever those reference in turn.
    let mut schemas: Map<String, Value> = Map::new();
    let mut queue: Vec<String> = used.into_iter().collect();
    while let Some(name) = queue.pop() {
        if schemas.contains_key(&name) {
            continue;
        }
        let Some(class) = input.sym.classes.get(&name) else {
            continue;
        };
        let (schema, refs) = class_schema(input.sym, class);
        schemas.insert(name, schema);
        queue.extend(refs);
    }

    let mut doc = Map::new();
    doc.insert("openapi".into(), json!("3.1.0"));
    doc.insert(
        "info".into(),
        json!({ "title": input.title, "version": input.version }),
    );
    doc.insert("paths".into(), Value::Object(paths));
    if !schemas.is_empty() {
        doc.insert("components".into(), json!({ "schemas": schemas }));
    }
    Value::Object(doc)
}

/// `getApiV1OrgsOrgIdInvoices` — stable across runs, and unique because the
/// route table already refuses two routes of the same shape (routing §7).
fn operation_id(r: &ResolvedRoute) -> String {
    let mut out = r.method.to_lowercase();
    let mut upper = true;
    for c in r.pattern.chars() {
        if c == '/' || c == '{' || c == '}' || c == '-' || c == '_' {
            upper = true;
            continue;
        }
        if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn error_response(name: &str) -> Value {
    json!({
        "description": name,
        "content": { "application/json": { "schema": {
            "type": "object",
            "properties": {
                "error": { "type": "string" },
                "message": { "type": "string" },
            },
        } } },
    })
}

/// The `content` for one recorded response, plus any class it names.
fn response_body(sym: &Symbols, r: &RouteResponse) -> (Value, BTreeSet<String>) {
    let mut named = BTreeSet::new();
    if matches!(r.payload, Ty::Void) {
        return (json!({ "description": describe(r.status) }), named);
    }
    match schema(sym, &r.payload, &mut named) {
        // tooling.md §5.3 — a `Raw` response is emitted with no schema.
        // The compiler did not check that shape either.
        None => (
            json!({
                "description": describe(r.status),
                "content": { "application/json": {} },
            }),
            named,
        ),
        Some(s) => (
            json!({
                "description": describe(r.status),
                "content": { "application/json": { "schema": s } },
            }),
            named,
        ),
    }
}

fn describe(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Response",
    }
}

/// `None` when the type carries no checked shape — `Raw`, and the error
/// recovery type.
fn schema(sym: &Symbols, ty: &Ty, named: &mut BTreeSet<String>) -> Option<Value> {
    match ty {
        Ty::Raw | Ty::Unknown | Ty::Null | Ty::Response | Ty::Void => None,
        Ty::Optional(inner) => schema(sym, inner, named),
        Ty::Array(inner) => Some(match schema(sym, inner, named) {
            Some(items) => json!({ "type": "array", "items": items }),
            None => json!({ "type": "array" }),
        }),
        Ty::Class(name) => {
            named.insert(name.clone());
            Some(reference(name))
        }
        Ty::Enum(name) => Some(match sym.enums.get(name) {
            Some(e) => json!({ "type": "string", "enum": e.members }),
            None => json!({ "type": "string" }),
        }),
        Ty::Scalar(s) => Some(scalar(*s)),
        Ty::Record(fields) => {
            let mut props = Map::new();
            let mut required = Vec::new();
            for (name, ft) in fields.iter() {
                if !ft.is_optional() {
                    required.push(name.clone());
                }
                props.insert(
                    name.clone(),
                    schema(sym, ft, named).unwrap_or_else(|| json!({})),
                );
            }
            let mut out = Map::new();
            out.insert("type".into(), json!("object"));
            out.insert("properties".into(), Value::Object(props));
            if !required.is_empty() {
                out.insert("required".into(), json!(required));
            }
            Some(Value::Object(out))
        }
    }
}

fn class_schema(sym: &Symbols, class: &ClassSym) -> (Value, Vec<String>) {
    let mut props = Map::new();
    let mut required = Vec::new();
    let refs = Vec::new();
    for f in &class.fields {
        // `transient` is validated and never stored (types.md §4.3), but it
        // is still part of the request body, which is what this schema
        // describes.
        if !f.ty.is_optional() {
            required.push(f.name.clone());
        }
        props.insert(f.name.clone(), class_field_schema(sym, f));
    }
    let mut out = Map::new();
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        out.insert("required".into(), json!(required));
    }
    (Value::Object(out), refs)
}

/// A class field carries its validation rules, and several of them have an
/// exact JSON Schema spelling. Emitting them makes the document able to
/// reject what the server would reject.
fn class_field_schema(sym: &Symbols, f: &crate::symbols::ClassFieldSym) -> Value {
    let base = match &f.ty {
        Ty::Optional(inner) => &**inner,
        other => other,
    };
    let mut out = match base {
        Ty::Scalar(s) => scalar(*s),
        // An enum's members are the whole point of the type: a document
        // that says `string` lets a caller send `superadmin` and find out
        // from a 400.
        Ty::Enum(name) => match sym.enums.get(name) {
            Some(e) => json!({ "type": "string", "enum": e.members }),
            None => json!({ "type": "string" }),
        },
        Ty::Array(_) => json!({ "type": "array" }),
        _ => json!({}),
    };
    let Some(o) = out.as_object_mut() else {
        return out;
    };
    for (rule, args) in &f.rules {
        let n = args.first().and_then(number_literal);
        match (rule.as_str(), n) {
            ("minLength", Some(v)) => {
                o.insert("minLength".into(), v);
            }
            ("maxLength", Some(v)) => {
                o.insert("maxLength".into(), v);
            }
            ("min", Some(v)) => {
                o.insert("minimum".into(), v);
            }
            ("max", Some(v)) => {
                o.insert("maximum".into(), v);
            }
            ("pattern", _) => {
                if let Some(p) = args.first().and_then(string_literal) {
                    o.insert("pattern".into(), json!(p));
                }
            }
            _ => {}
        }
    }
    out
}

/// JSON Schema's `minLength` is an integer and its `minimum` is a number.
/// `2.0` where `2` was written is valid JSON and wrong-looking in every
/// generated client.
fn number_literal(e: &crate::ast::Expr) -> Option<Value> {
    match &*e.kind {
        crate::ast::ExprKind::Int(n) => n.parse::<i64>().ok().map(|v| json!(v)),
        crate::ast::ExprKind::Decimal(n) => n.parse::<f64>().ok().map(|v| json!(v)),
        _ => None,
    }
}

fn string_literal(e: &crate::ast::Expr) -> Option<String> {
    match &*e.kind {
        // `pattern(r"…")` is a raw string; the regex is its text.
        crate::ast::ExprKind::Str(s) | crate::ast::ExprKind::RawStr(s) => Some(s.clone()),
        _ => None,
    }
}

/// A path parameter's declared type, which routing.md §3.1 restricts to a
/// small set.
fn scalar_schema(name: &str) -> Value {
    match Scalar::from_name(name) {
        Some(s) => scalar(s),
        None => json!({ "type": "string" }),
    }
}

/// types.md §2.3 — the wire form. `bigint` and `numeric` are JSON strings.
fn scalar(s: Scalar) -> Value {
    match s {
        Scalar::Smallint | Scalar::Int => json!({ "type": "integer" }),
        Scalar::Bigint => json!({ "type": "string", "format": "int64" }),
        Scalar::Numeric => json!({ "type": "string", "format": "decimal" }),
        Scalar::Boolean => json!({ "type": "boolean" }),
        Scalar::Varchar | Scalar::Text => json!({ "type": "string" }),
        Scalar::Timestamptz => json!({ "type": "string", "format": "date-time" }),
        Scalar::Date => json!({ "type": "string", "format": "date" }),
        Scalar::Time => json!({ "type": "string", "format": "time" }),
        Scalar::Interval => json!({ "type": "string" }),
        Scalar::Uuid => json!({ "type": "string", "format": "uuid" }),
        Scalar::Jsonb => json!({}),
        Scalar::Inet => json!({ "type": "string" }),
        Scalar::Bytea => json!({ "type": "string", "format": "byte" }),
    }
}
