//! Class validation and the 400 contract (types.md §11).
//!
//! Errors are **collected**, never fail-fast, and the response body shape is
//! fixed: user code cannot produce a different `validation_failed` payload,
//! because validation is not reachable from user code (errors.md §9, E11).

use crate::exec::field_error;
use crate::symbols::{ClassSym, Symbols};
use crate::types::Ty;
use crate::value::Value;
use serde_json::Value as J;

/// Validate `input` against `class`, appending accepted fields to `out` and
/// failures to `failures`.
pub fn validate_class(
    class: &ClassSym,
    sym: &Symbols,
    input: &J,
    prefix: &str,
    out: &mut Vec<(String, Value)>,
    failures: &mut Vec<Value>,
) {
    let J::Object(map) = input else {
        failures.push(field_error(prefix, "type", None, "expected an object"));
        return;
    };

    for f in &class.fields {
        let path = if prefix.is_empty() {
            f.name.clone()
        } else {
            format!("{prefix}.{}", f.name)
        };
        // types.md §11.5 — unknown keys are dropped silently; the class is
        // the whitelist, and rejecting extras breaks every client that adds
        // a field.
        let present = map.get(&f.name);
        let required = f.rules.iter().any(|r| r.name == "required");

        let value = match present {
            None | Some(J::Null) => {
                if required {
                    failures.push(field_error(
                        &path,
                        "required",
                        None,
                        &format!("{} kerak", f.name),
                    ));
                }
                // §6.5 — absent and null are distinguishable, and absent
                // means "omit the column" downstream.
                if present.is_some() {
                    out.push((f.name.clone(), Value::Null));
                }
                continue;
            }
            Some(v) => v,
        };

        match coerce(&f.ty, value) {
            Some(v) => {
                check_rules(f, &path, value, &v, failures);
                if let (Ty::Class(name), J::Object(_)) = (&base(&f.ty), value) {
                    if let Some(inner) = sym.classes.get(name) {
                        let mut nested = Vec::new();
                        validate_class(inner, sym, value, &path, &mut nested, failures);
                        out.push((f.name.clone(), Value::Record(nested)));
                        continue;
                    }
                }
                if let (Ty::Class(name), J::Array(items)) = (&base(&f.ty), value) {
                    if let Some(inner) = sym.classes.get(name) {
                        // §11.4 — element failures accumulate with indexed
                        // paths into the same list.
                        let mut rows = Vec::new();
                        for (i, item) in items.iter().enumerate() {
                            let mut nested = Vec::new();
                            validate_class(
                                inner,
                                sym,
                                item,
                                &format!("{path}[{i}]"),
                                &mut nested,
                                failures,
                            );
                            rows.push(Value::Record(nested));
                        }
                        out.push((f.name.clone(), Value::Array(rows)));
                        continue;
                    }
                }
                out.push((f.name.clone(), v));
            }
            None => failures.push(field_error(
                &path,
                "type",
                None,
                &format!("{} tipi mos emas", f.name),
            )),
        }
    }
}

fn base(t: &Ty) -> Ty {
    match t {
        Ty::Optional(inner) | Ty::Array(inner) => base(inner),
        other => other.clone(),
    }
}

/// JSON to a `Value` of the declared type. `None` is "this JSON cannot be
/// that type".
///
/// Public because the job queue needs it: a payload is JSON on the way
/// into the table and has to come back out as the parameter types the
/// `job` declared. Reconstructing that from the JSON alone cannot work —
/// `7` is an `int` and a `bigint` and a `numeric`.
pub fn coerce(ty: &Ty, v: &J) -> Option<Value> {
    use crate::types::Scalar;
    match (&base(ty), v) {
        (_, J::Null) => Some(Value::Null),
        (Ty::Scalar(Scalar::Boolean), J::Bool(b)) => Some(Value::Bool(*b)),
        // types.md §2.3 — a bigint arrives as a JSON string *or* a number.
        (Ty::Scalar(Scalar::Bigint), J::String(s)) => s.parse().ok().map(Value::Bigint),
        (Ty::Scalar(Scalar::Bigint), J::Number(n)) => n.as_i64().map(Value::Bigint),
        (Ty::Scalar(Scalar::Int | Scalar::Smallint), J::Number(n)) => n.as_i64().map(Value::Int),
        (Ty::Scalar(Scalar::Numeric), J::String(s)) => {
            s.parse::<f64>().ok().map(|_| Value::Numeric(s.clone()))
        }
        (Ty::Scalar(Scalar::Numeric), J::Number(n)) => Some(Value::Numeric(n.to_string())),
        (Ty::Scalar(Scalar::Jsonb), other) => Some(Value::Raw(other.to_string())),
        (Ty::Scalar(_), J::String(s)) => Some(Value::Text(s.clone())),
        (Ty::Enum(_), J::String(s)) => Some(Value::Text(s.clone())),
        (Ty::Class(_), J::Object(_)) => Some(Value::Null),
        (_, J::Array(items)) => {
            let mut out = Vec::new();
            for i in items {
                out.push(coerce(&base(ty), i)?);
            }
            Some(Value::Array(out))
        }
        _ => None,
    }
}

fn check_rules(
    f: &crate::symbols::ClassFieldSym,
    path: &str,
    raw: &J,
    value: &Value,
    failures: &mut Vec<Value>,
) {
    for r in &f.rules {
        let (rule, args) = (r.name.as_str(), &r.args);
        let limit = args.first().and_then(literal_i64);
        let ok = match rule {
            "required" => true,
            "minLength" => text_len(value).is_none_or(|n| limit.is_none_or(|l| n >= l)),
            "maxLength" => text_len(value).is_none_or(|n| limit.is_none_or(|l| n <= l)),
            "minItems" => array_len(raw).is_none_or(|n| limit.is_none_or(|l| n >= l)),
            "maxItems" => array_len(raw).is_none_or(|n| limit.is_none_or(|l| n <= l)),
            // Compared as a decimal, not an integer: `amount numeric(14,2)
            // min(0)` has to reject `-1.00`, and `"-1.00".parse::<i64>()`
            // fails — which would have made the rule silently pass.
            "min" => numeric_value(value).is_none_or(|n| bound(args).is_none_or(|l| n >= l)),
            "max" => numeric_value(value).is_none_or(|n| bound(args).is_none_or(|l| n <= l)),
            "pattern" => match (args.first().and_then(literal_str), value.as_text()) {
                (Some(p), Some(s)) => regex::Regex::new(&p).map(|r| r.is_match(s)).unwrap_or(true),
                _ => true,
            },
            "transient" => true,
            _ => true,
        };
        if !ok {
            // A declared `: "…"` wins; otherwise the generated sentence.
            let message = match &r.message {
                Some(m) => m.clone(),
                None => default_message(&f.name, rule, limit),
            };
            failures.push(field_error(path, rule, limit, &message));
        }
    }
}

fn default_message(field: &str, rule: &str, limit: Option<i64>) -> String {
    match (rule, limit) {
        ("minLength", Some(n)) => format!("{field} kamida {n} belgidan iborat bo'lishi kerak"),
        ("maxLength", Some(n)) => format!("{field} ko'pi bilan {n} belgi bo'lishi kerak"),
        ("minItems", Some(n)) => format!("{field} kamida {n} ta element bo'lishi kerak"),
        ("maxItems", Some(n)) => format!("{field} ko'pi bilan {n} ta element bo'lishi kerak"),
        ("min", Some(n)) => format!("{field} kamida {n} bo'lishi kerak"),
        ("max", Some(n)) => format!("{field} ko'pi bilan {n} bo'lishi kerak"),
        ("pattern", _) => format!("{field} shakli mos emas"),
        ("required", _) => format!("{field} kerak"),
        _ => format!("{field} yaroqsiz"),
    }
}

fn numeric_value(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) | Value::Bigint(n) => Some(*n as f64),
        Value::Numeric(s) | Value::Text(s) => s.parse().ok(),
        _ => None,
    }
}

/// A numeric rule bound, which may itself be a decimal (`min(0.01)`).
fn bound(args: &[crate::ast::Expr]) -> Option<f64> {
    match args.first().map(|e| &*e.kind) {
        Some(crate::ast::ExprKind::Int(n)) | Some(crate::ast::ExprKind::Decimal(n)) => {
            n.parse().ok()
        }
        _ => None,
    }
}

fn text_len(v: &Value) -> Option<i64> {
    v.as_text().map(|s| s.chars().count() as i64)
}

fn array_len(v: &J) -> Option<i64> {
    match v {
        J::Array(items) => Some(items.len() as i64),
        _ => None,
    }
}

fn literal_i64(e: &crate::ast::Expr) -> Option<i64> {
    match &*e.kind {
        crate::ast::ExprKind::Int(n) => n.parse().ok(),
        _ => None,
    }
}

fn literal_str(e: &crate::ast::Expr) -> Option<String> {
    match &*e.kind {
        crate::ast::ExprKind::Str(s) | crate::ast::ExprKind::RawStr(s) => Some(s.clone()),
        _ => None,
    }
}
