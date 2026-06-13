//! `jwc-runtime` — the runtime `Value` model shared by the JWC interpreter
//! and (eventually) the native AOT.
//!
//! Phase 1 [1.0-blocker]: extracted out of `jwc::runner` so the native AOT
//! and the interpreter can converge on one in-memory representation without
//! the interpreter crate being a build-time dependency of the AOT codegen.
//!
//! Scope is intentionally narrow — only items that are **runtime-pure** live
//! here. Anything that reaches into `Vm`, postgres types, or the route
//! dispatcher (e.g. `value_to_sql_param`, `content_type_response`) stays in
//! `jwc::runner` because moving it would drag those deps across the
//! boundary.

use std::sync::Arc;

use serde_json::{json, Value as JsonValue};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Void,
    /// In-language array literal (`[1, "two", true]`). Elements may be
    /// heterogeneous. Renders as compact JSON via `as_string()`.
    Array(Vec<Value>),
    /// **Phase 1 [1.0-blocker]** — compile-time-shape object value.
    ///
    /// `field_names` carries the ordered field-name layout (one allocation
    /// shared via `Arc` across every Record built with the same schema —
    /// DB rows, monomorphized entity instances, statically-typed object
    /// literals). `values` carries the field values in the SAME order as
    /// `field_names`. Field access is `field_names.iter().position(...)`
    /// then `values[i]` — O(N) for small N (typical entity has 2-10
    /// fields, linear scan beats hashing); both vecs are wrapped in `Arc`
    /// so cloning a Record is a refcount bump.
    ///
    /// This is the **typed fast path**. Dynamic objects (`json_parse(s)`
    /// output, object literals with computed keys) still travel as
    /// `Value::Str(json_string)`. The runner decides per-site which to
    /// produce; `value_to_json` / `as_string` render both identically.
    Record {
        field_names: Arc<Vec<Arc<str>>>,
        values: Arc<Vec<Value>>,
    },
}

impl Value {
    pub fn as_string(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::Str(v) => v.clone(),
            Value::Bool(v) => v.to_string(),
            Value::Null => "null".to_string(),
            Value::Void => String::new(),
            // Arrays render as compact JSON (`[1,"two",true]`), reusing the
            // serde_json serializer over a Value→JsonValue conversion.
            Value::Array(_) | Value::Record { .. } => value_to_json(self).to_string(),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "double",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Void => "void",
            Value::Array(_) => "array",
            Value::Record { .. } => "object",
        }
    }

    /// Build a `Value::Record` from an ordered list of `(name, value)`
    /// pairs. Field name strings get interned into the per-record `Arc`
    /// here; for hot paths that build many Records with the same schema
    /// (e.g. a 1000-row select), prefer `Value::record_with_shape` so
    /// the `field_names` Arc is shared across all rows.
    pub fn record_from_pairs(pairs: Vec<(String, Value)>) -> Value {
        let mut names: Vec<Arc<str>> = Vec::with_capacity(pairs.len());
        let mut values: Vec<Value> = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            names.push(Arc::from(k));
            values.push(v);
        }
        Value::Record {
            field_names: Arc::new(names),
            values: Arc::new(values),
        }
    }

    /// Build a `Value::Record` reusing a pre-interned `field_names` Arc
    /// — the shape-deduping path. `values.len()` must equal
    /// `field_names.len()`; mismatch is a codegen bug, so debug builds
    /// assert and release builds silently truncate to `min(len_a, len_b)`
    /// rather than panic in production.
    pub fn record_with_shape(field_names: Arc<Vec<Arc<str>>>, values: Vec<Value>) -> Value {
        debug_assert_eq!(field_names.len(), values.len(), "Record shape/value arity mismatch");
        Value::Record {
            field_names,
            values: Arc::new(values),
        }
    }

    /// O(N) field lookup by name. Returns `None` for non-Record values
    /// (callers should match on the variant first when they need the
    /// distinction); returns `None` for an unknown field name on a
    /// genuine Record. The linear scan beats hashing for the small N
    /// (2-10 fields) of typical entity / object-literal shapes.
    pub fn record_field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Record { field_names, values } => field_names
                .iter()
                .position(|f| f.as_ref() == name)
                .map(|i| &values[i]),
            _ => None,
        }
    }
}

pub fn format_float(value: f64) -> String {
    let mut s = format!("{value:.15}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Convert a `serde_json::Value` into the runtime `Value` tree.
///
/// **Phase 1 [1.0-blocker]** — JSON objects materialise as `Value::Record`
/// rather than a single top-level shape with JSON-string leaves. Field-name
/// `Arc<str>`s are allocated per call (no shape interning for dynamic JSON);
/// the per-record `Arc<Vec<...>>` wrappers keep clones cheap downstream.
pub fn json_to_value(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Str(n.to_string())
            }
        }
        JsonValue::String(s) => Value::Str(s.clone()),
        // JSON arrays map to the in-language array Value (recursively); JSON
        // objects map to Value::Record, also recursively via json_to_value.
        JsonValue::Array(items) => Value::Array(items.iter().map(json_to_value).collect()),
        JsonValue::Object(map) => {
            let pairs: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect();
            Value::record_from_pairs(pairs)
        }
    }
}

/// JSON-encode a runtime Value, embedding nested JSON shapes raw.
///
/// `Value::Str` is the language's universal carrier for both plain strings
/// AND nested objects/arrays returned by `select` / `body()` / `cache_get`.
/// When the string parses as a JSON object/array, embed it as-is so an
/// object literal like `{ items: posts }` produces `{"items": [...]}`
/// rather than the double-encoded `{"items": "[...]"}`.
pub fn value_to_json_smart(value: &Value) -> JsonValue {
    if let Value::Str(s) = value {
        if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
            if parsed.is_object() || parsed.is_array() {
                return parsed;
            }
        }
    }
    value_to_json(value)
}

pub fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Int(v) => json!(v),
        Value::Float(v) => json!(v),
        Value::Str(v) => json!(v),
        Value::Bool(v) => json!(v),
        Value::Null | Value::Void => JsonValue::Null,
        // Arrays serialize to a JSON array; each element goes through the same
        // smart serializer so a `Value::Str` carrying nested JSON embeds raw
        // rather than double-encoded.
        Value::Array(items) => JsonValue::Array(items.iter().map(value_to_json_smart).collect()),
        // **Phase 1** — typed-shape object. Walk the shape's field-name
        // list in order, pairing each name with its value through the
        // smart serializer (so a nested `Value::Str` carrying JSON embeds
        // raw rather than double-encoded, same as Array). Field-name Arcs
        // are cheap to deref; the output `serde_json::Map` is an alloc per
        // serialize call (unavoidable on this path until we switch the
        // entire response pipeline to streaming writes).
        Value::Record { field_names, values } => {
            let mut map = serde_json::Map::with_capacity(field_names.len());
            for (name, val) in field_names.iter().zip(values.iter()) {
                map.insert(name.as_ref().to_string(), value_to_json_smart(val));
            }
            JsonValue::Object(map)
        }
    }
}

/// Convert a `SELECT` result string from `engine::query_text_with_optional_cache`
/// into the typed `Value` tree.
///
/// **Phase 1 [1.0-blocker]** — the eager-parse fast path for DB rows.
/// Before this, every `select` returned `Value::Str(json)` and downstream
/// code (ForIn loop, FieldGet, response wrap) paid `serde_json::from_str`
/// + `json_to_value` PER ACCESS. Now we parse once here and emit:
///
/// - JSON null / empty → `Value::Null`
/// - JSON array of homogeneous-shape objects → `Value::Array` of
///   `Value::Record` sharing **one** `field_names` Arc across all rows
///   (the headline /json-large win — 1000 rows = 1 allocation for the
///   schema layout plus per-row `values` Vecs).
/// - JSON object → single `Value::Record` (the `first` form).
/// - Anything else → fall back through `json_to_value`.
///
/// Shape derivation: the first row's keys define the shape. Subsequent
/// rows look up each field in the shape's order, missing keys become
/// `Value::Null`. SQL `SELECT` always returns the same projected columns
/// across rows, so the fast path is the common case; the per-row
/// `into_iter` walk handles any extra keys gracefully without dropping
/// to the slow path.
pub fn materialize_select_result(result: &str) -> Value {
    if result == "null" || result.is_empty() {
        return Value::Null;
    }
    let parsed: JsonValue = match serde_json::from_str(result) {
        Ok(v) => v,
        // Not valid JSON — keep the original string so user code can
        // still see (and debug) whatever the engine returned.
        Err(_) => return Value::Str(result.to_string()),
    };
    match parsed {
        JsonValue::Array(rows) => {
            // Empty array — preserve the array shape (callers may iterate).
            if rows.is_empty() {
                return Value::Array(Vec::new());
            }
            // Derive the shared shape from the first row, then materialise
            // every row against it. Non-object first row → element-wise
            // json_to_value (heterogeneous payloads keep the dynamic path).
            let first_obj = match &rows[0] {
                JsonValue::Object(m) => m,
                _ => {
                    return Value::Array(rows.iter().map(json_to_value).collect());
                }
            };
            let shape: Arc<Vec<Arc<str>>> = Arc::new(
                first_obj.keys().map(|k| Arc::from(k.as_str())).collect(),
            );
            let mut out: Vec<Value> = Vec::with_capacity(rows.len());
            for row in rows {
                match row {
                    JsonValue::Object(mut m) => {
                        let mut vals: Vec<Value> = Vec::with_capacity(shape.len());
                        for field in shape.iter() {
                            let v = m.remove(field.as_ref()).unwrap_or(JsonValue::Null);
                            vals.push(json_to_value(&v));
                        }
                        out.push(Value::record_with_shape(Arc::clone(&shape), vals));
                    }
                    other => out.push(json_to_value(&other)),
                }
            }
            Value::Array(out)
        }
        // `first` form returns one object — emit a single Record.
        // json_to_value already turns Object→Record (Stage 2B wiring),
        // so this hands off cleanly.
        obj @ JsonValue::Object(_) => json_to_value(&obj),
        other => json_to_value(&other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Phase 1 [1.0-blocker]: Value::Record foundation ----

    #[test]
    fn record_constructs_from_pairs_and_renders_as_json() {
        let r = Value::record_from_pairs(vec![
            ("id".to_string(), Value::Int(7)),
            ("name".to_string(), Value::Str("Brand".to_string())),
            ("active".to_string(), Value::Bool(true)),
        ]);
        // type_name reports `object`, matching the dynamic-V::Object surface.
        assert_eq!(r.type_name(), "object");
        // as_string round-trips through value_to_json — `serde_json::Map`
        // is a `BTreeMap` (no `preserve_order` feature), so output keys
        // come out alphabetical. This matches what every other JSON
        // path in JWC emits today (DB rows, object literals via Str).
        let s = r.as_string();
        assert_eq!(s, r#"{"active":true,"id":7,"name":"Brand"}"#);
    }

    #[test]
    fn record_field_lookup_is_position_based() {
        let r = Value::record_from_pairs(vec![
            ("first".to_string(), Value::Int(10)),
            ("second".to_string(), Value::Int(20)),
            ("third".to_string(), Value::Int(30)),
        ]);
        assert_eq!(r.record_field("first"), Some(&Value::Int(10)));
        assert_eq!(r.record_field("second"), Some(&Value::Int(20)));
        assert_eq!(r.record_field("third"), Some(&Value::Int(30)));
        assert_eq!(r.record_field("missing"), None);
        // Non-Record values return None — callers must match the variant
        // when they need to distinguish "this isn't a record" from "this
        // record has no such field".
        assert_eq!(Value::Int(5).record_field("anything"), None);
    }

    #[test]
    fn record_with_shape_shares_the_field_name_arc() {
        // Build one shape once, then build two Records that reuse it. The
        // headline /json-large win comes from this Arc being shared
        // across 1000 rows instead of 1000 separate Vec<String> allocs.
        let shape: Arc<Vec<Arc<str>>> = Arc::new(vec![Arc::from("id"), Arc::from("name")]);
        let r1 = Value::record_with_shape(
            Arc::clone(&shape),
            vec![Value::Int(1), Value::Str("alpha".to_string())],
        );
        let r2 = Value::record_with_shape(
            Arc::clone(&shape),
            vec![Value::Int(2), Value::Str("beta".to_string())],
        );
        // Both records' field_names point at the same allocation.
        if let (
            Value::Record { field_names: f1, .. },
            Value::Record { field_names: f2, .. },
        ) = (&r1, &r2)
        {
            assert!(Arc::ptr_eq(f1, f2), "field_names Arc should be shared");
        } else {
            unreachable!("record_with_shape must produce Value::Record");
        }
        assert_eq!(r1.as_string(), r#"{"id":1,"name":"alpha"}"#);
        assert_eq!(r2.as_string(), r#"{"id":2,"name":"beta"}"#);
    }

    #[test]
    fn materialize_select_result_shares_shape_across_rows() {
        // The /json-large win: 1000 rows from one select share ONE
        // field_names Arc, not 1000 separate Vec<String> allocations.
        let payload = r#"[{"id":1,"name":"a"},{"id":2,"name":"b"},{"id":3,"name":"c"}]"#;
        let v = materialize_select_result(payload);
        match v {
            Value::Array(rows) => {
                assert_eq!(rows.len(), 3);
                let first_arc = match &rows[0] {
                    Value::Record { field_names, .. } => Arc::clone(field_names),
                    _ => panic!("expected Record"),
                };
                for row in rows.iter().skip(1) {
                    match row {
                        Value::Record { field_names, .. } => {
                            assert!(
                                Arc::ptr_eq(&first_arc, field_names),
                                "all rows must share the same field_names Arc",
                            );
                        }
                        _ => panic!("expected Record"),
                    }
                }
                assert_eq!(rows[0].record_field("id"), Some(&Value::Int(1)));
                assert_eq!(
                    rows[2].record_field("name"),
                    Some(&Value::Str("c".to_string())),
                );
            }
            other => panic!("expected Value::Array, got {:?}", other),
        }
    }

    #[test]
    fn materialize_select_result_handles_null_and_empty() {
        assert!(matches!(materialize_select_result("null"), Value::Null));
        assert!(matches!(materialize_select_result(""), Value::Null));
        // Empty array stays an empty array — callers may iterate.
        match materialize_select_result("[]") {
            Value::Array(v) => assert!(v.is_empty()),
            other => panic!("expected empty Array, got {:?}", other),
        }
    }

    #[test]
    fn materialize_select_result_single_object_emits_record() {
        // `select first ...` returns one object — materialise to Record.
        let v = materialize_select_result(r#"{"id":42,"name":"first"}"#);
        match v {
            Value::Record { .. } => {
                assert_eq!(v.record_field("id"), Some(&Value::Int(42)));
                assert_eq!(
                    v.record_field("name"),
                    Some(&Value::Str("first".to_string())),
                );
            }
            other => panic!("expected Record, got {:?}", other),
        }
    }

    #[test]
    fn materialize_select_result_falls_back_on_bad_json() {
        // Anything the engine returns that isn't JSON falls back to Str,
        // so callers still see the engine's raw output rather than Null.
        let v = materialize_select_result("not-json-at-all");
        assert!(matches!(v, Value::Str(ref s) if s == "not-json-at-all"));
    }

    #[test]
    fn record_nested_embed_does_not_double_encode() {
        // `value_to_json_smart` recognises JSON-string carriers — so a
        // Record containing a Value::Str of JSON should embed raw, not
        // get re-quoted. Mirrors the same behaviour Array already has.
        let inner_json = r#"{"x":1}"#.to_string();
        let outer = Value::record_from_pairs(vec![
            ("inner".to_string(), Value::Str(inner_json)),
            ("count".to_string(), Value::Int(1)),
        ]);
        // Keys come out alphabetical via serde_json's BTreeMap-backed Map.
        assert_eq!(outer.as_string(), r#"{"count":1,"inner":{"x":1}}"#);
    }
}
