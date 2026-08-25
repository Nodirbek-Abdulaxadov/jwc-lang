//! Runtime values and their wire form.
//!
//! The wire rules of types.md §2.1 live here and nowhere else, so the raw
//! path (which casts in SQL) and the record path (which serialises here)
//! cannot drift: `bigint` and `numeric` are JSON **strings** on both.

use serde_json::{Map, Number, Value as J};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// `int` / `smallint` — a JSON number.
    Int(i64),
    /// `bigint` — a JSON string, because JavaScript loses digits above 2^53.
    Bigint(i64),
    /// Exact decimal, carried as text so no float ever touches money.
    Numeric(String),
    Text(String),
    /// RFC 3339, UTC.
    Timestamptz(String),
    Interval(String),
    /// A pre-serialised JSON fragment from Postgres. Never parsed
    /// (types.md §5.1).
    Raw(String),
    Record(Vec<(String, Value)>),
    Array(Vec<Value>),
    /// A response under construction. It is a value so `created(json(x))`
    /// composes: `json` produces one and `created` re-statuses it
    /// (routing.md §6.1).
    Response {
        status: u16,
        /// Already-serialised JSON, or empty for a bodiless response.
        body: String,
        headers: Vec<(String, String)>,
    },
}

impl Value {
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(n) | Value::Bigint(n) => Some(*n),
            Value::Numeric(s) | Value::Text(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) | Value::Numeric(s) | Value::Timestamptz(s) | Value::Interval(s) => {
                Some(s)
            }
            _ => None,
        }
    }

    /// The text a bind parameter carries. `None` binds SQL NULL.
    /// Postgres array literal form (`{a,b}`) — a bound parameter cast to
    /// `T[]` needs that, not JSON.
    fn array_literal(items: &[Value]) -> String {
        let parts: Vec<String> = items
            .iter()
            .map(|v| match v {
                Value::Null => "NULL".to_string(),
                other => {
                    let t = other.to_bind().unwrap_or_default();
                    format!("\"{}\"", t.replace('\\', "\\\\").replace('"', "\\\""))
                }
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    }

    pub fn to_bind(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Bool(b) => Some(b.to_string()),
            Value::Int(n) | Value::Bigint(n) => Some(n.to_string()),
            Value::Numeric(s)
            | Value::Text(s)
            | Value::Timestamptz(s)
            | Value::Interval(s)
            | Value::Raw(s) => Some(s.clone()),
            Value::Array(items) => Some(Value::array_literal(items)),
            Value::Record(_) => Some(self.to_json().to_string()),
            Value::Response { body, .. } => Some(body.clone()),
        }
    }

    pub fn to_json(&self) -> J {
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(n) => J::Number(Number::from(*n)),
            // types.md §2.3
            Value::Bigint(n) => J::String(n.to_string()),
            Value::Numeric(s) => J::String(s.clone()),
            Value::Text(s) | Value::Timestamptz(s) | Value::Interval(s) => J::String(s.clone()),
            Value::Raw(s) => serde_json::from_str(s).unwrap_or(J::Null),
            Value::Record(fields) => {
                let mut m = Map::new();
                for (k, v) in fields {
                    m.insert(k.clone(), v.to_json());
                }
                J::Object(m)
            }
            Value::Array(items) => J::Array(items.iter().map(|v| v.to_json()).collect()),
            Value::Response { body, .. } => serde_json::from_str(body).unwrap_or(J::Null),
        }
    }

    /// Serialise for a response. A `Raw` fragment is spliced verbatim — the
    /// text goes into the buffer without a parse (types.md §5.4).
    pub fn write_json(&self, out: &mut String) {
        match self {
            Value::Raw(s) => out.push_str(s),
            Value::Record(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&J::String(k.clone()).to_string());
                    out.push(':');
                    v.write_json(out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write_json(out);
                }
                out.push(']');
            }
            Value::Response { body, .. } => out.push_str(body),
            other => out.push_str(&other.to_json().to_string()),
        }
    }

    /// A one-line rendering for `debug.dump` (tooling.md §3).
    ///
    /// A `Raw` prints as the text Postgres produced, unparsed — which is
    /// exactly the thing that cannot be seen any other way, and the reason
    /// this builtin exists.
    pub fn debug_text(&self) -> String {
        match self {
            Value::Raw(t) => format!("raw {t}"),
            Value::Response { status, body, .. } => format!("response {status} {body}"),
            other => {
                let mut out = String::new();
                other.write_json(&mut out);
                out
            }
        }
    }

    /// What `console.*` writes (builtins.md §7b).
    ///
    /// A text value prints as its characters, not as a JSON string —
    /// `console.write("hi")` puts `hi` on the terminal, not `"hi"`. That
    /// is the one difference from [`debug_text`], which quotes because a
    /// dump is for reading a value's shape rather than its content.
    /// Everything else renders the same way, so `console.write(42)` and
    /// `console.write($row)` both work.
    pub fn display_text(&self) -> String {
        match self {
            Value::Text(s) | Value::Numeric(s) => s.clone(),
            Value::Null => String::new(),
            other => other.debug_text(),
        }
    }

    pub fn field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Record(fields) => fields.iter().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Parse a JSON fragment into a record, applying the declared wire
    /// form. Used on the record path, where the projection is known.
    pub fn from_json(j: &J) -> Value {
        match j {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Numeric(n.to_string()),
            },
            J::String(s) => Value::Text(s.clone()),
            J::Array(items) => Value::Array(items.iter().map(Value::from_json).collect()),
            J::Object(m) => Value::Record(
                m.iter()
                    .map(|(k, v)| (k.clone(), Value::from_json(v)))
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigint_is_a_string_and_int_is_a_number() {
        assert_eq!(
            Value::Bigint(9007199254740993).to_json().to_string(),
            "\"9007199254740993\""
        );
        assert_eq!(Value::Int(42).to_json().to_string(), "42");
    }

    #[test]
    fn money_never_touches_a_float() {
        assert_eq!(
            Value::Numeric("10.00".into()).to_json().to_string(),
            "\"10.00\""
        );
    }

    #[test]
    fn raw_is_spliced_verbatim() {
        let env = Value::Record(vec![
            ("items".into(), Value::Raw("[{\"id\":\"1\"}]".into())),
            ("next".into(), Value::Null),
        ]);
        let mut out = String::new();
        env.write_json(&mut out);
        assert_eq!(out, "{\"items\":[{\"id\":\"1\"}],\"next\":null}");
    }

    #[test]
    fn a_bigint_survives_the_round_trip_the_wire_rule_exists_for() {
        // 9007199254740993 is the smallest odd integer f64 cannot hold.
        let v = Value::Bigint(9007199254740993);
        let mut out = String::new();
        v.write_json(&mut out);
        assert_eq!(out, "\"9007199254740993\"");
        assert_eq!(
            out.trim_matches('"').parse::<i64>().ok(),
            Some(9007199254740993)
        );
    }
}
