//! `validate body { ... }` rule engine.
//!
//! The runner's `Stmt::ValidateBody` arm (in `exec.rs`) parses the request
//! body once and hands it to [`run_validation_rules`], which walks every
//! declared rule and returns `{ field: first_failing_rule_message }`. An
//! empty map means the body satisfied every rule. The handler short-circuits
//! with a 400 response when the map is non-empty — that decision lives in
//! the caller, not here.

use serde_json::Value as JsonValue;

use crate::ast::{ValidateField, ValidateRule};

/// Run the rules from a `validate body { ... }` block against a parsed JSON body.
/// Returns a map `{ field: "first failing rule" }`. Empty map means all rules passed.
pub(super) fn run_validation_rules(
    fields: &[ValidateField],
    body: &JsonValue,
) -> serde_json::Map<String, JsonValue> {
    let mut errors = serde_json::Map::new();

    for field in fields {
        let value = body.get(&field.name);

        for rule in &field.rules {
            if let Some(msg) = check_rule(rule, value) {
                errors.insert(field.name.clone(), JsonValue::String(msg));
                break;
            }
        }
    }

    errors
}

fn check_rule(rule: &ValidateRule, value: Option<&JsonValue>) -> Option<String> {
    match rule {
        ValidateRule::Required => match value {
            None | Some(JsonValue::Null) => Some("required".to_string()),
            _ => None,
        },
        ValidateRule::MinLength(n) => match value {
            Some(JsonValue::String(s)) => {
                if (s.chars().count() as i64) < *n {
                    Some(format!("minLength({n})"))
                } else {
                    None
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("minLength({n}): not a string")),
        },
        ValidateRule::MaxLength(n) => match value {
            Some(JsonValue::String(s)) => {
                if (s.chars().count() as i64) > *n {
                    Some(format!("maxLength({n})"))
                } else {
                    None
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("maxLength({n}): not a string")),
        },
        ValidateRule::Min(bound) => check_numeric_bound(value, bound, true),
        ValidateRule::Max(bound) => check_numeric_bound(value, bound, false),
        ValidateRule::Pattern(regex_src) => match value {
            Some(JsonValue::String(s)) => {
                let re = match regex::Regex::new(regex_src) {
                    Ok(re) => re,
                    Err(_) => return Some(format!("pattern({regex_src}): invalid regex")),
                };
                if re.is_match(s) {
                    None
                } else {
                    Some(format!("pattern({regex_src})"))
                }
            }
            None | Some(JsonValue::Null) => None,
            _ => Some(format!("pattern({regex_src}): not a string")),
        },
    }
}

fn check_numeric_bound(value: Option<&JsonValue>, bound: &str, is_min: bool) -> Option<String> {
    let bound_num: f64 = match bound.parse() {
        Ok(v) => v,
        Err(_) => return Some(format!("invalid numeric bound '{bound}'")),
    };
    let label = if is_min { "min" } else { "max" };

    match value {
        Some(JsonValue::Number(n)) => {
            let v = n.as_f64().unwrap_or(0.0);
            let ok = if is_min {
                v >= bound_num
            } else {
                v <= bound_num
            };
            if ok {
                None
            } else {
                Some(format!("{label}({bound})"))
            }
        }
        None | Some(JsonValue::Null) => None,
        _ => Some(format!("{label}({bound}): not a number")),
    }
}
