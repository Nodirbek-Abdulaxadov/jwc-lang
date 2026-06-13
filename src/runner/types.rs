//! Runtime type-checking for typed parameters, function return values,
//! and JSON payloads coerced into model objects.
//!
//! These methods all hang off `Vm` (they need `self.models` to walk model
//! shapes), so each `impl Vm` block here defines a few related methods.
//! Nothing here is async — coercion is pure CPU work.

use anyhow::{anyhow, bail, Result};
use serde_json::Value as JsonValue;

use crate::ast::{ModelDecl, TypedParam};

use super::util::{looks_like_base64, looks_like_datetime, looks_like_uuid, strip_generic_wrapper};
use super::{format_float, Value, Vm};

impl<'a> Vm<'a> {
    pub(super) fn check_param_type(&self, param: &TypedParam, value: Value) -> Result<Value> {
        let ty = match &param.ty {
            None => return Ok(value),
            Some(t) => t,
        };
        self.check_typed_value(&format!("parameter '{}'", param.name), ty, value)
    }

    pub(super) fn check_typed_value(
        &self,
        subject: &str,
        ty: &str,
        value: Value,
    ) -> Result<Value> {
        // Strip trailing `?` nullable marker
        let (base, nullable_marker) = match ty.strip_suffix('?') {
            Some(stripped) => (stripped, true),
            None => (ty, false),
        };

        // Desugar `Optional<T>` → same nullable semantics as `T?`
        let (base, nullable) = if let Some(inner) = strip_generic_wrapper(base, "Optional") {
            (inner.to_string(), true)
        } else {
            (base.to_string(), nullable_marker)
        };

        if nullable {
            if matches!(value, Value::Null) {
                return Ok(Value::Null);
            }
            if let Value::Str(s) = &value {
                if s == "null" {
                    return Ok(Value::Null);
                }
            }
        }

        // List<T> — JSON array where each element matches T
        if let Some(elem_ty) = strip_generic_wrapper(&base, "List") {
            return self.check_list_value(subject, elem_ty, value);
        }

        match base.as_str() {
            "string" | "str" => match &value {
                Value::Str(_) => Ok(value),
                Value::Int(n) => Ok(Value::Str(n.to_string())),
                Value::Float(n) => Ok(Value::Str(format_float(*n))),
                _ => bail!(
                    "Type error: {subject} expects string, got {}",
                    value.type_name()
                ),
            },
            "int" | "integer" | "number" | "bigint" => match &value {
                Value::Int(_) => Ok(value),
                Value::Float(n) if n.fract() == 0.0 => Ok(Value::Int(*n as i64)),
                Value::Str(s) => s.parse::<i64>().map(Value::Int).map_err(|_| {
                    anyhow!("Type error: {subject} expects int, got string \"{}\"", s)
                }),
                _ => bail!(
                    "Type error: {subject} expects int, got {}",
                    value.type_name()
                ),
            },
            "double" | "float" => match &value {
                Value::Float(_) => Ok(value),
                Value::Int(n) => Ok(Value::Float(*n as f64)),
                Value::Str(s) => s.parse::<f64>().map(Value::Float).map_err(|_| {
                    anyhow!("Type error: {subject} expects double, got string \"{}\"", s)
                }),
                _ => bail!(
                    "Type error: {subject} expects double, got {}",
                    value.type_name()
                ),
            },
            "bool" | "boolean" => match &value {
                Value::Bool(_) => Ok(value),
                _ => bail!(
                    "Type error: {subject} expects bool, got {}",
                    value.type_name()
                ),
            },
            "uuid" => match &value {
                Value::Str(s) if looks_like_uuid(s) => Ok(value),
                Value::Str(s) => bail!("Type error: {subject} expects uuid, got string \"{s}\""),
                _ => bail!(
                    "Type error: {subject} expects uuid, got {}",
                    value.type_name()
                ),
            },
            "datetime" | "timestamp" => match &value {
                Value::Str(s) if looks_like_datetime(s) => Ok(value),
                Value::Str(s) => {
                    bail!("Type error: {subject} expects datetime (ISO 8601), got string \"{s}\"")
                }
                _ => bail!(
                    "Type error: {subject} expects datetime, got {}",
                    value.type_name()
                ),
            },
            "decimal" => match &value {
                Value::Float(_) | Value::Int(_) => Ok(value),
                Value::Str(s) if s.parse::<f64>().is_ok() => Ok(Value::Str(s.clone())),
                Value::Str(s) => bail!("Type error: {subject} expects decimal, got string \"{s}\""),
                _ => bail!(
                    "Type error: {subject} expects decimal, got {}",
                    value.type_name()
                ),
            },
            "json" => match &value {
                Value::Str(s) => {
                    if serde_json::from_str::<JsonValue>(s).is_ok() {
                        Ok(value)
                    } else {
                        bail!("Type error: {subject} expects json, got non-json string")
                    }
                }
                Value::Null => Ok(value),
                _ => bail!(
                    "Type error: {subject} expects json, got {}",
                    value.type_name()
                ),
            },
            // `bytes` / `byte[]` cross the JSON boundary as a base64
            // string. We validate the charset / padding shape via
            // `base64::decode` so callers fail fast on garbage; the
            // decoded bytes are intentionally discarded — a real
            // `Value::Bytes` variant lands in a follow-up sprint.
            "bytes" | "byte[]" => match &value {
                Value::Str(s) if looks_like_base64(s) => Ok(value),
                Value::Str(s) => {
                    bail!("Type error: {subject} expects bytes (base64 string), got \"{s}\"")
                }
                _ => bail!(
                    "Type error: {subject} expects bytes (base64 string), got {}",
                    value.type_name()
                ),
            },
            model_ty => {
                let model = self.models.get(&model_ty.to_lowercase());
                if model.is_none() {
                    return Ok(value);
                }

                match value {
                    Value::Null => Ok(Value::Null),
                    Value::Str(raw) => {
                        let parsed: JsonValue = serde_json::from_str(&raw).map_err(|_| {
                            anyhow!("Type error: {subject} expects {model_ty}, got non-json string")
                        })?;

                        self.validate_json_against_model(
                            subject,
                            model.expect("INVARIANT: model.is_none() returned early above (line 157), so model is Some here"),
                            &parsed,
                        )?;
                        Ok(Value::Str(parsed.to_string()))
                    }
                    other => bail!(
                        "Type error: {subject} expects {}, got {}",
                        model_ty,
                        other.type_name()
                    ),
                }
            }
        }
    }

    pub(super) fn check_list_value(
        &self,
        subject: &str,
        elem_ty: &str,
        value: Value,
    ) -> Result<Value> {
        let raw = match value {
            Value::Str(s) => s,
            Value::Null => bail!("Type error: {subject} expects List<{elem_ty}>, got null"),
            other => bail!(
                "Type error: {subject} expects List<{elem_ty}>, got {}",
                other.type_name()
            ),
        };

        let parsed: JsonValue = serde_json::from_str(&raw)
            .map_err(|_| anyhow!("Type error: {subject} expects List<{elem_ty}>, got non-json"))?;

        let arr = parsed.as_array().ok_or_else(|| {
            anyhow!("Type error: {subject} expects List<{elem_ty}>, got non-array")
        })?;

        for (i, item) in arr.iter().enumerate() {
            if !self.json_value_matches_type(item, elem_ty) {
                bail!(
                    "Type error: {subject} expects List<{elem_ty}>, item at index {i} is not {elem_ty}"
                );
            }
        }

        Ok(Value::Str(parsed.to_string()))
    }

    pub(super) fn validate_json_against_model(
        &self,
        subject: &str,
        model: &ModelDecl,
        value: &JsonValue,
    ) -> Result<()> {
        if let Some(arr) = value.as_array() {
            for item in arr {
                self.validate_model_object(subject, model, item)?;
            }
            return Ok(());
        }

        self.validate_model_object(subject, model, value)
    }

    fn validate_model_object(
        &self,
        subject: &str,
        model: &ModelDecl,
        value: &JsonValue,
    ) -> Result<()> {
        let Some(obj) = value.as_object() else {
            bail!(
                "Type error: {subject} expects {}, got non-object json",
                model.name
            );
        };

        for field in &model.fields {
            let Some(v) = obj.get(&field.name) else {
                if field.is_nullable {
                    continue;
                }
                bail!(
                    "Type error: {subject} expects field '{}' for {}",
                    field.name,
                    model.name
                );
            };

            if v.is_null() {
                if !field.is_nullable {
                    bail!(
                        "Type error: {subject} field '{}' cannot be null for {}",
                        field.name,
                        model.name
                    );
                }
                continue;
            }

            if !self.json_value_matches_type(v, &field.ty.name) {
                bail!(
                    "Type error: {subject} field '{}' has invalid type for {}",
                    field.name,
                    model.name
                );
            }
        }

        Ok(())
    }

    pub(super) fn json_value_matches_type(&self, value: &JsonValue, type_name: &str) -> bool {
        let (base, nullable) = match type_name.strip_suffix('?') {
            Some(stripped) => (stripped, true),
            None => (type_name, false),
        };

        if nullable && value.is_null() {
            return true;
        }

        match base.to_ascii_lowercase().as_str() {
            "string" | "str" | "text" | "varchar" => value.is_string(),
            "uuid" => value.as_str().map(looks_like_uuid).unwrap_or(false),
            "datetime" | "timestamp" => value.as_str().map(looks_like_datetime).unwrap_or(false),
            "int" | "integer" | "number" | "bigint" => {
                value.as_i64().is_some() || value.as_u64().is_some()
            }
            "double" | "float" | "decimal" => value.is_number(),
            "bool" | "boolean" => value.is_boolean(),
            "json" => value.is_object() || value.is_array(),
            // bytes / byte[]: payloads cross JSON as base64 strings. We
            // accept any string here and leave decoding to user code (or
            // a future `decode_base64()` helper). Length / charset
            // validation is intentionally lax for now — Phase 2.1 v2 will
            // tighten this when a real `bytes` Value variant lands.
            "bytes" | "byte[]" => value.is_string(),
            other => {
                if let Some(model) = self.models.get(other) {
                    return self
                        .validate_json_against_model("nested model", model, value)
                        .is_ok();
                }
                true
            }
        }
    }
}
