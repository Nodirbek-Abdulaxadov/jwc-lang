//! SQL builders shared between `Stmt::Db*` (insert/update/delete) and
//! `Expr::DbSelect` / `Expr::DbAggregate` / `Expr::DbCount`.
//!
//! These are free functions, not `Vm` methods — the higher-level evaluator
//! arms in `runner/exec.rs` and `runner/eval.rs` call them with a `&mut Vm`
//! so they can recurse back into expression evaluation for parameter values
//! (e.g. an `expr` on the RHS of `where`). Keeping them outside `impl Vm`
//! keeps the borrow story tidy: each builder takes the `params` vec by `&mut`
//! and only touches `Vm` through the passed-in reference.

use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Context, Result};
use async_recursion::async_recursion;
use tokio_postgres::types::ToSql;

use crate::ast::{AggregateKind, DbOrderBy, Expr, ModelDecl, NavigationKind, SortDir, WhereExpr};

use super::util::{looks_like_datetime, looks_like_uuid};
use super::{format_float, value_to_json, Value, Vm};

/// Extract the column name from a field path like `"Entity.field"` → `"field"`
pub(super) fn field_path_to_col(path: &str) -> String {
    if let Some(pos) = path.rfind('.') {
        path[pos + 1..].to_string()
    } else {
        path.to_string()
    }
}

/// Normalize JWC comparison operators to SQL operators
pub(super) fn normalize_sql_op(op: &str) -> &str {
    match op {
        "==" | "=" => "=",
        "!=" => "!=",
        "<" => "<",
        "<=" => "<=",
        ">" => ">",
        ">=" => ">=",
        "like" => "LIKE",
        "ilike" => "ILIKE",
        _ => "=",
    }
}

/// Get a variable value as a JSON object string for an `insert` / `update` /
/// `delete <var>` statement.
///
/// A row loaded by `select ... first` is a `Value::Record`, and one built by
/// `new Entity()` (after field assignments) may be a `Value::Record` or a
/// JSON-string `Value::Str`. Both — plus a `Value::Array` of rows — are valid
/// object inputs here: render them through the shared `value_to_json`
/// serializer so the canonical `let x = select...; x.f = ..; update x in`
/// pattern works regardless of which representation the row arrived as.
pub(super) fn get_var_as_json(var: &str, vars: &HashMap<String, Value>) -> Result<String> {
    match vars.get(&var.to_lowercase()) {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(v @ (Value::Record { .. } | Value::Array(_))) => Ok(v.as_string()),
        Some(other) => bail!("'{}' must be a JSON object, got {}", var, other.type_name()),
        None => bail!("variable '{}' not found", var),
    }
}

/// Build `INSERT INTO "table" (...) VALUES (...) RETURNING *` from a JSON object string
/// Uses a CTE so all columns (including SERIAL id) are returned.
pub(super) fn build_insert_sql(
    table: &str,
    json_str: &str,
    col_types: &HashMap<String, String>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "insert: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("insert: value must be a JSON object"))?;
    if obj.is_empty() {
        bail!("insert: object has no fields to insert");
    }
    // Keep all provided fields, including explicit primary keys.
    // Some schemas (including example projects) use int PK without identity/default.
    let mut filtered: Vec<(&String, &serde_json::Value)> = obj.iter().collect();
    filtered.sort_by(|a, b| a.0.cmp(b.0));

    let fields: Vec<String> = filtered.iter().map(|(k, _)| format!("\"{}\"", k)).collect();
    let placeholders: Vec<String> = (1..=filtered.len()).map(|i| format!("${}", i)).collect();
    let params: Vec<Box<dyn ToSql + Sync + Send>> = filtered
        .iter()
        .map(|(k, v)| json_value_to_sql_param_typed(v, col_types.get(*k).map(|s| s.as_str())))
        .collect();
    Ok((format!(
        "WITH _ins AS (INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING *) SELECT row_to_json(t)::text FROM _ins t;",
        table,
        fields.join(", "),
        placeholders.join(", "),
    ), params))
}

/// Build `UPDATE "table" SET ... WHERE <pk-cols> = ... RETURNING *;` from a
/// JSON object string. PK columns are excluded from the SET clause and used in
/// the WHERE filter; with composite PKs all of them are required in the JSON.
///
/// `dirty_fields` — when `Some`, only those columns are included in SET (the
/// natural "modified-since-load" semantics). When `None`, every non-PK field
/// from the JSON is updated (used for objects materialised entirely from code).
pub(super) fn build_update_sql(
    table: &str,
    json_str: &str,
    pk_fields: &[String],
    dirty_fields: Option<&HashSet<String>>,
    col_types: &HashMap<String, String>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "update: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("update: value must be a JSON object"))?;

    if pk_fields.is_empty() {
        bail!("update: table '{}' has no primary key declared", table);
    }
    let mut pk_values: Vec<&serde_json::Value> = Vec::with_capacity(pk_fields.len());
    for pk in pk_fields {
        let v = obj.get(pk).ok_or_else(|| {
            anyhow!(
                "update: object must have field '{}' for the primary-key WHERE clause",
                pk
            )
        })?;
        pk_values.push(v);
    }

    let pk_set: std::collections::HashSet<String> =
        pk_fields.iter().map(|p| p.to_lowercase()).collect();
    let mut updates: Vec<(&String, &serde_json::Value)> = obj
        .iter()
        .filter(|(k, _)| !pk_set.contains(&k.to_lowercase()))
        .filter(|(k, _)| match dirty_fields {
            None => true,
            Some(d) => d.contains(*k) || d.contains(&k.to_lowercase()),
        })
        .collect();
    updates.sort_by(|a, b| a.0.cmp(b.0));

    if updates.is_empty() {
        if dirty_fields.is_some() {
            bail!("update: no fields have been modified since the object was loaded");
        }
        bail!("update: no fields to update (only primary key present in object)");
    }

    let sets: Vec<String> = updates
        .iter()
        .enumerate()
        .map(|(idx, (k, _))| format!("\"{}\" = ${}", k, idx + 1))
        .collect();

    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = updates
        .iter()
        .map(|(k, v)| json_value_to_sql_param_typed(v, col_types.get(*k).map(|s| s.as_str())))
        .collect();
    let mut where_parts = Vec::with_capacity(pk_fields.len());
    for (idx, pk) in pk_fields.iter().enumerate() {
        params.push(json_value_to_sql_param_typed(
            pk_values[idx],
            col_types.get(pk).map(|s| s.as_str()),
        ));
        where_parts.push(format!("\"{}\" = ${}", pk, params.len()));
    }

    Ok((
        format!(
            "WITH _upd AS (UPDATE \"{}\" SET {} WHERE {} RETURNING *) SELECT row_to_json(t)::text FROM _upd t;",
            table,
            sets.join(", "),
            where_parts.join(" AND "),
        ),
        params,
    ))
}

/// Build `DELETE FROM "table" WHERE <pk-cols> = $N [AND ...];` from a JSON
/// object string. Composite primary keys are supported when all pk fields are
/// present in the JSON.
pub(super) fn build_delete_sql(
    table: &str,
    json_str: &str,
    pk_fields: &[String],
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "delete: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("delete: value must be a JSON object"))?;

    if pk_fields.is_empty() {
        bail!("delete: table '{}' has no primary key declared", table);
    }
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::with_capacity(pk_fields.len());
    let mut where_parts = Vec::with_capacity(pk_fields.len());
    for pk in pk_fields {
        let v = obj.get(pk).ok_or_else(|| {
            anyhow!(
                "delete: object must have field '{}' for the primary-key WHERE clause",
                pk
            )
        })?;
        params.push(json_value_to_sql_param(v));
        where_parts.push(format!("\"{}\" = ${}", pk, params.len()));
    }

    Ok((
        format!(
            "DELETE FROM \"{}\" WHERE {};",
            table,
            where_parts.join(" AND ")
        ),
        params,
    ))
}

pub(super) fn json_value_to_sql_param(val: &serde_json::Value) -> Box<dyn ToSql + Sync + Send> {
    match val {
        serde_json::Value::Null => Box::new(Option::<String>::None),
        serde_json::Value::Bool(b) => Box::new(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if (i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                    Box::new(i as i32)
                } else {
                    Box::new(i)
                }
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        serde_json::Value::String(s) => string_to_sql_param(s),
        other => Box::new(other.to_string()),
    }
}

/// Schema-aware variant of [`json_value_to_sql_param`]: bind a value according
/// to the **declared column type** rather than guessing from the value's shape.
///
/// This fixes two value-shape-vs-schema mismatches:
/// - a datetime-looking *string* destined for a `varchar`/`text` column was
///   bound as `timestamptz` (Postgres rejected it) — now it binds as text;
/// - a JSON *object* destined for a `jsonb` column was bound as text (rejected)
///   — now it binds as real `jsonb`.
///
/// `target` is the lower-cased Postgres type from the entity schema (e.g.
/// `"varchar(120)"`, `"timestamptz"`, `"jsonb"`, `"uuid"`). When `None` (column
/// not found / ad-hoc table) it falls back to the shape-based binder.
pub(super) fn json_value_to_sql_param_typed(
    val: &serde_json::Value,
    target: Option<&str>,
) -> Box<dyn ToSql + Sync + Send> {
    if let Some(t) = target {
        // jsonb / json columns take the value verbatim as JSON (serde_json's
        // ToSql maps to jsonb). NULL falls through to the generic binder.
        if t.contains("json") && !val.is_null() {
            return Box::new(val.clone());
        }
        if let serde_json::Value::String(s) = val {
            if is_text_column_type(t) {
                // Plain text: never reinterpret an ISO-date-looking string as a
                // timestamp just because of its shape.
                return Box::new(s.clone());
            }
            if is_timestamp_column_type(t) {
                if let Some(ts) = parse_rfc3339(s) {
                    return Box::new(ts);
                }
            }
            if t.contains("uuid") {
                if let Ok(u) = uuid::Uuid::parse_str(s) {
                    return Box::new(u);
                }
            }
        }
    }
    json_value_to_sql_param(val)
}

/// Build a `column-name → lower-cased Postgres type` map for an entity, used to
/// drive schema-aware parameter binding in insert / update. Columns whose type
/// can't be mapped are simply omitted (the binder falls back to shape-based).
pub(super) fn column_types_for_fields(fields: &[crate::ast::FieldDecl]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for f in fields {
        if let Ok((ty, _)) = crate::sql::map_type_postgres(&f.ty, &f.name) {
            map.insert(f.name.clone(), ty.to_ascii_lowercase());
        }
    }
    map
}

fn is_text_column_type(t: &str) -> bool {
    t.starts_with("varchar") || t.starts_with("char") || t == "text" || t == "citext"
}

fn is_timestamp_column_type(t: &str) -> bool {
    t.starts_with("timestamp") || t == "date" || t.starts_with("time")
}

/// Postgres rejects plain `String` against `uuid` / `timestamptz` columns —
/// no auto-cast. Try to recognise these shapes from the literal value and
/// box them as the proper Rust type so postgres-types accepts the bind.
/// Falls back to `String` for anything that doesn't match.
pub(super) fn string_to_sql_param(s: &str) -> Box<dyn ToSql + Sync + Send> {
    if looks_like_uuid(s) {
        if let Ok(u) = uuid::Uuid::parse_str(s) {
            return Box::new(u);
        }
    }
    if looks_like_datetime(s) {
        if let Some(ts) = parse_rfc3339(s) {
            return Box::new(ts);
        }
    }
    Box::new(s.to_string())
}

/// Accept the most common ISO 8601 / RFC 3339 shapes. Strict chrono parsing
/// is intentionally permissive: trailing `Z`, sub-second precision, and an
/// optional timezone offset all parse cleanly.
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // Date-only forms like "2026-05-19" — promote to midnight UTC.
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = d.and_hms_opt(0, 0, 0)?;
        return Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            dt,
            chrono::Utc,
        ));
    }
    None
}

pub(super) fn boxed_params_to_refs(
    params: &[Box<dyn ToSql + Sync + Send>],
) -> Vec<&(dyn ToSql + Sync)> {
    params
        .iter()
        .map(|p| p.as_ref() as &(dyn ToSql + Sync))
        .collect()
}

pub(super) fn value_to_sql_param(val: &Value) -> Box<dyn ToSql + Sync + Send> {
    match val {
        Value::Int(n) => {
            if (i32::MIN as i64..=i32::MAX as i64).contains(n) {
                Box::new(*n as i32)
            } else {
                Box::new(*n)
            }
        }
        Value::Str(s) => string_to_sql_param(s),
        Value::Float(n) => Box::new(*n),
        Value::Bool(b) => Box::new(*b),
        Value::Null | Value::Void => Box::new(Option::<String>::None),
        // No native array param type yet — bind the JSON text. The DB column is
        // expected to be json/jsonb/text; richer array binding lands later.
        Value::Array(_) | Value::Record { .. } => {
            string_to_sql_param(&value_to_json(val).to_string())
        }
    }
}

pub(super) fn value_to_cache_fragment(val: &Value) -> String {
    match val {
        Value::Int(n) => format!("int:{n}"),
        Value::Float(n) => format!("float:{}", format_float(*n)),
        Value::Str(s) => format!("str:{s}"),
        Value::Bool(b) => format!("bool:{b}"),
        Value::Null => "null".to_string(),
        Value::Void => "void".to_string(),
        Value::Array(_) => format!("arr:{}", value_to_json(val)),
        Value::Record { .. } => format!("rec:{}", value_to_json(val)),
    }
}

// Wide signature is intentional — this is the shared SELECT builder used by
// the runtime path. Splitting into a builder struct is tracked under the
// runner.rs modularisation sprint (Sprint 7).
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_select_sql(
    table_name: String,
    where_clause: Option<&WhereExpr>,
    order_by: Option<&DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    first: bool,
    nav_subqueries: &[NavigationSubquery],
    projection: &[String],
    group_by: &[String],
    having: Option<&WhereExpr>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>, String, String)> {
    let mut sql_where = String::new();
    let mut shape_bits: Vec<String> = Vec::new();
    let mut cache_bits: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

    if let Some(wc) = where_clause {
        let where_sql =
            build_where_sql(wc, &mut params, &mut shape_bits, &mut cache_bits, vars, vm).await?;
        sql_where = format!(" WHERE {}", where_sql);
    }

    // GROUP BY + HAVING — emitted between WHERE and ORDER BY. The shape /
    // cache keys include the column list so two different groupings don't
    // collide in the prepared-statement cache.
    let mut sql_group = String::new();
    if !group_by.is_empty() {
        let cols: Vec<String> = group_by
            .iter()
            .map(|f| format!("\"{}\"", field_path_to_col(f)))
            .collect();
        sql_group = format!(" GROUP BY {}", cols.join(", "));
        shape_bits.push(format!("group:{}", cols.join(",")));
        cache_bits.push(format!("group:{}", cols.join(",")));
    }

    let mut sql_having = String::new();
    if let Some(hv) = having {
        let having_sql =
            build_where_sql(hv, &mut params, &mut shape_bits, &mut cache_bits, vars, vm).await?;
        sql_having = format!(" HAVING {}", having_sql);
    }

    let mut sql_order = String::new();
    if let Some(ob) = order_by {
        let col = field_path_to_col(&ob.field);
        let dir = match ob.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        };
        sql_order = format!(" ORDER BY \"{}\" {}", col, dir);
        shape_bits.push(format!("orderby:{col}:{dir}"));
        cache_bits.push(format!("orderby:{col}:{dir}"));
    }

    let mut sql_limit_offset = String::new();

    // LIMIT / OFFSET are *bound parameters*, not baked literals. Baking the
    // value as a literal collides in the SQL-compile cache (the shape key only
    // recorded `limit:literal`, not the value), so the first compiled
    // LIMIT/OFFSET would stick and later dynamic values were silently ignored.
    // A `$N` placeholder keeps the shape constant and the value per-call.
    if let Some(limit_expr) = limit {
        let v = vm.eval_expr(limit_expr, vars).await?;
        let n = value_to_positive_int(&v, "limit")?;
        params.push(Box::new(n));
        sql_limit_offset.push_str(&format!(" LIMIT ${}", params.len()));
        shape_bits.push("limit:param".to_string());
        cache_bits.push(format!("limit:{n}"));
    } else if first {
        sql_limit_offset.push_str(" LIMIT 1");
    }

    if let Some(offset_expr) = offset {
        let v = vm.eval_expr(offset_expr, vars).await?;
        let n = value_to_positive_int(&v, "offset")?;
        params.push(Box::new(n));
        sql_limit_offset.push_str(&format!(" OFFSET ${}", params.len()));
        shape_bits.push("offset:param".to_string());
        cache_bits.push(format!("offset:{n}"));
    }

    // Build the inner SELECT column list.
    //
    // - No `{ ... }` projection and no `with` clause → SELECT * (cheapest).
    // - Explicit `{ col1, col2 }` → only those columns from the source table.
    // - `with rel` → always include the named columns (or `t.*`) plus a
    //   correlated json subquery per relation.
    let base_cols: Vec<String> = if projection.is_empty() {
        Vec::new()
    } else {
        projection.iter().map(|c| format!("t.\"{}\"", c)).collect()
    };
    let inner_projection = if nav_subqueries.is_empty() && base_cols.is_empty() {
        "*".to_string()
    } else {
        let mut bits: Vec<String> = if base_cols.is_empty() {
            vec!["t.*".to_string()]
        } else {
            base_cols.clone()
        };
        if !projection.is_empty() {
            shape_bits.push(format!("project:{}", projection.join(",")));
            cache_bits.push(format!("project:{}", projection.join(",")));
        }
        for nav in nav_subqueries {
            bits.push(format!("{} AS \"{}\"", nav.sql_fragment("t"), nav.alias()));
            shape_bits.push(format!("with:{}", nav.alias()));
            cache_bits.push(format!("with:{}", nav.alias()));
        }
        bits.join(", ")
    };

    let inner_sql = format!(
        "SELECT {} FROM \"{}\" t{}{}{}{}{}",
        inner_projection, table_name, sql_where, sql_group, sql_having, sql_order, sql_limit_offset
    );

    let sql = if first {
        format!(
            "SELECT row_to_json(r)::text FROM ({}) r;",
            inner_sql.trim_end()
        )
    } else {
        format!(
            "SELECT COALESCE(json_agg(row_to_json(r)), '[]')::text FROM ({}) r;",
            inner_sql.trim_end()
        )
    };

    let shape_suffix = if shape_bits.is_empty() {
        "no_clauses".to_string()
    } else {
        shape_bits.join("|")
    };
    let cache_suffix = if cache_bits.is_empty() {
        "no_clauses".to_string()
    } else {
        cache_bits.join("|")
    };
    let shape_key = format!("select|table:{table_name}|first:{first}|{shape_suffix}");
    let cache_key = format!("result|table:{table_name}|first:{first}|{cache_suffix}");

    Ok((sql, params, shape_key, cache_key))
}

pub(super) struct NavigationSubquery {
    name: String,
    kind: NavigationKind,
    target_table: String,
    /// Column on the target (`c`) side of the join.
    join_target_col: String,
    /// Column on the source (`t`) side of the join.
    join_source_col: String,
    /// Column subset (empty = whole row).
    projection: Vec<String>,
    /// `(join_table, near_col, far_col)` for a ManyToMany nav; `None` otherwise.
    join: Option<(String, String, String)>,
    /// Optional `(column, direction)` to order the materialised collection.
    order_by: Option<(String, SortDir)>,
}

fn sort_dir_sql(dir: SortDir) -> &'static str {
    match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    }
}

/// The per-row JSON value for a nav subquery: the whole row, or a
/// `json_build_object` of the projected columns (hides the rest).
fn nav_json_value(projection: &[String]) -> String {
    if projection.is_empty() {
        "row_to_json(c)".to_string()
    } else {
        let pairs: Vec<String> = projection
            .iter()
            .map(|col| format!("'{}', c.\"{}\"", col, col))
            .collect();
        format!("json_build_object({})", pairs.join(", "))
    }
}

impl NavigationSubquery {
    fn alias(&self) -> &str {
        &self.name
    }

    fn sql_fragment(&self, source_alias: &str) -> String {
        let val = nav_json_value(&self.projection);
        let order = match &self.order_by {
            Some((col, dir)) => format!(" ORDER BY c.\"{}\" {}", col, sort_dir_sql(*dir)),
            None => String::new(),
        };
        match self.kind {
            NavigationKind::OneToMany => format!(
                "COALESCE((SELECT json_agg({}{}) FROM \"{}\" c WHERE c.\"{}\" = {}.\"{}\"), '[]'::json)",
                val, order, self.target_table, self.join_target_col, source_alias, self.join_source_col
            ),
            // OneToOne (target holds FK) and BelongsTo (source holds FK) both
            // materialise a single nested object; only the join direction —
            // captured in join_target_col / join_source_col — differs.
            NavigationKind::OneToOne | NavigationKind::BelongsTo => format!(
                "(SELECT {} FROM \"{}\" c WHERE c.\"{}\" = {}.\"{}\"{} LIMIT 1)",
                val, self.target_table, self.join_target_col, source_alias, self.join_source_col, order
            ),
            // ManyToMany: target rows reached through the link table.
            // join_target_col = target PK, join_source_col = source PK.
            NavigationKind::ManyToMany => {
                let (jt, near, far) = self
                    .join
                    .as_ref()
                    .expect("ManyToMany navigation must carry join-table coordinates");
                format!(
                    "COALESCE((SELECT json_agg({}{}) FROM \"{}\" c JOIN \"{}\" j ON j.\"{}\" = c.\"{}\" WHERE j.\"{}\" = {}.\"{}\"), '[]'::json)",
                    val, order, self.target_table, jt, far, self.join_target_col, near, source_alias, self.join_source_col
                )
            }
        }
    }
}

pub(super) fn build_navigation_subqueries(
    entity: &str,
    _source_table_name: &str,
    requested: &[String],
    models: &HashMap<String, &ModelDecl>,
    pk_by_table: &HashMap<String, Vec<String>>,
) -> Result<Vec<NavigationSubquery>> {
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    if entity == "*" {
        bail!("'with' clause requires a named entity, not '*'");
    }
    let entity_key = entity.to_lowercase();
    let model = models
        .get(&entity_key)
        .ok_or_else(|| anyhow!("unknown entity '{}' for 'with' clause", entity))?;

    let source_pk_col = pk_by_table
        .get(&entity_key)
        .and_then(|v| v.first().cloned())
        .unwrap_or_else(|| "id".to_string());

    let mut out = Vec::with_capacity(requested.len());
    for rel in requested {
        let rel_key = rel.to_lowercase();
        let nav = model
            .navigations
            .iter()
            .find(|n| n.name.to_lowercase() == rel_key)
            .ok_or_else(|| anyhow!("entity '{}' has no navigation '{}'", entity, rel))?;
        let target_pk = pk_by_table
            .get(&nav.target_entity.to_lowercase())
            .and_then(|v| v.first().cloned())
            .unwrap_or_else(|| "id".to_string());
        let (join_target_col, join_source_col) = match nav.kind {
            // belongs-to: this entity holds the FK → join target PK = source FK
            NavigationKind::BelongsTo => (target_pk, nav.target_field.clone()),
            // m2m: join via link table on both PKs.
            NavigationKind::ManyToMany => (target_pk, source_pk_col.clone()),
            // has-many / has-one: target holds the FK → join target FK = source PK
            _ => (nav.target_field.clone(), source_pk_col.clone()),
        };
        let join = nav.join.as_ref().map(|j| {
            (
                crate::sql::to_snake_case(&j.table),
                j.near_col.clone(),
                j.far_col.clone(),
            )
        });
        out.push(NavigationSubquery {
            name: nav.name.clone(),
            kind: nav.kind,
            target_table: crate::sql::to_snake_case(&nav.target_entity),
            join_target_col,
            join_source_col,
            projection: nav.projection.clone(),
            join,
            order_by: nav
                .order_by
                .as_ref()
                .map(|o| (field_path_to_col(&o.col), o.dir)),
        });
    }
    Ok(out)
}

#[async_recursion]
pub(super) async fn build_where_sql(
    expr: &WhereExpr,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    shape: &mut Vec<String>,
    cache: &mut Vec<String>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<String> {
    match expr {
        WhereExpr::Atom(wc) => {
            let col = field_path_to_col(&wc.field);
            let op = normalize_sql_op(&wc.op);
            let rhs_val = vm.eval_expr(&wc.rhs, vars).await?;

            Ok(match rhs_val {
                Value::Null | Value::Void => {
                    if op == "!=" {
                        shape.push(format!("where:{col}:is_not_null"));
                        cache.push(format!("where:{col}:is_not_null"));
                        format!("\"{}\" IS NOT NULL", col)
                    } else {
                        shape.push(format!("where:{col}:is_null"));
                        cache.push(format!("where:{col}:is_null"));
                        format!("\"{}\" IS NULL", col)
                    }
                }
                other => {
                    params.push(value_to_sql_param(&other));
                    let idx = params.len();
                    shape.push(format!("where:{col}:{op}:param"));
                    cache.push(format!(
                        "where:{col}:{op}:{}",
                        value_to_cache_fragment(&other)
                    ));
                    format!("\"{}\" {} ${}", col, op, idx)
                }
            })
        }
        WhereExpr::Between { field, low, high } => {
            let col = field_path_to_col(field);
            let low_v = vm.eval_expr(low, vars).await?;
            let high_v = vm.eval_expr(high, vars).await?;
            params.push(value_to_sql_param(&low_v));
            let low_idx = params.len();
            params.push(value_to_sql_param(&high_v));
            let high_idx = params.len();
            shape.push(format!("where:{col}:between"));
            cache.push(format!(
                "where:{col}:between:{}..{}",
                value_to_cache_fragment(&low_v),
                value_to_cache_fragment(&high_v)
            ));
            Ok(format!(
                "\"{}\" BETWEEN ${} AND ${}",
                col, low_idx, high_idx
            ))
        }
        WhereExpr::InList { field, values } => {
            let col = field_path_to_col(field);
            if values.is_empty() {
                bail!("WHERE 'in (...)' must have at least one value");
            }
            let mut placeholders = Vec::with_capacity(values.len());
            let mut cache_parts = Vec::with_capacity(values.len());
            for value_expr in values {
                let v = vm.eval_expr(value_expr, vars).await?;
                params.push(value_to_sql_param(&v));
                placeholders.push(format!("${}", params.len()));
                cache_parts.push(value_to_cache_fragment(&v));
            }
            shape.push(format!("where:{col}:in({})", values.len()));
            cache.push(format!("where:{col}:in[{}]", cache_parts.join(",")));
            Ok(format!("\"{}\" IN ({})", col, placeholders.join(", ")))
        }
        WhereExpr::And(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm).await?;
            shape.push("AND".to_string());
            cache.push("AND".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm).await?;
            Ok(format!("({} AND {})", ls, rs))
        }
        WhereExpr::Or(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm).await?;
            shape.push("OR".to_string());
            cache.push("OR".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm).await?;
            Ok(format!("({} OR {})", ls, rs))
        }
    }
}

/// Compile the RHS of one `col = <expr>` pair in an atomic UPDATE into a
/// SQL fragment. Bare identifiers that name a column on the entity stay
/// inline (`"hits"`); arithmetic ops fold recursively; anything else is
/// evaluated host-side once and bound as `$N`.
///
/// The function is split in two halves so it stays sync at the recursive
/// callsite — `async_recursion` here would inflate every parent future's
/// state machine and shrink the stack budget for plain recursive user
/// code (the conformance `fib(n)` case overflowed in debug before this
/// split). Step 1 walks the expression and collects every fall-through
/// `Expr` that needs host evaluation. Step 2 awaits the evals (flat list,
/// no recursion). Step 3 builds the SQL string with the pre-evaluated
/// values plugged in.
pub(super) async fn build_set_rhs_sql(
    expr: &Expr,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<String> {
    let mut fallbacks: Vec<&Expr> = Vec::new();
    collect_set_rhs_fallbacks(expr, vars, &mut fallbacks);
    // Evaluate fall-through exprs in source order so `$N` indices line up
    // with the order `build_set_rhs_sql_sync` reads them.
    let mut values: Vec<Value> = Vec::with_capacity(fallbacks.len());
    for f in &fallbacks {
        values.push(vm.eval_expr(f, vars).await?);
    }
    let mut value_iter = values.into_iter();
    Ok(build_set_rhs_sql_sync(expr, params, vars, &mut value_iter))
}

/// Walk the RHS tree and append every `Expr` that needs host-side
/// evaluation. Bare column references and the arithmetic spine are
/// SQL-inline and contribute nothing here.
fn collect_set_rhs_fallbacks<'a>(
    expr: &'a Expr,
    vars: &HashMap<String, Value>,
    out: &mut Vec<&'a Expr>,
) {
    match expr {
        Expr::Var(name) => {
            if vars.contains_key(&name.to_lowercase()) {
                out.push(expr);
            }
            // Otherwise it's a column reference — no host eval needed.
        }
        Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) => {
            collect_set_rhs_fallbacks(a, vars, out);
            collect_set_rhs_fallbacks(b, vars, out);
        }
        _ => out.push(expr),
    }
}

/// Build the SQL fragment, consuming pre-evaluated values for each
/// fall-through Expr in left-to-right order.
fn build_set_rhs_sql_sync(
    expr: &Expr,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    vars: &HashMap<String, Value>,
    values: &mut std::vec::IntoIter<Value>,
) -> String {
    match expr {
        Expr::Var(name) => {
            let key = name.to_lowercase();
            if vars.contains_key(&key) {
                let v = values
                    .next()
                    .expect("fallback collector must produce one value per Var-shadow");
                params.push(value_to_sql_param(&v));
                return format!("${}", params.len());
            }
            format!("\"{}\"", key)
        }
        Expr::Add(a, b) => {
            let ls = build_set_rhs_sql_sync(a, params, vars, values);
            let rs = build_set_rhs_sql_sync(b, params, vars, values);
            format!("({} + {})", ls, rs)
        }
        Expr::Sub(a, b) => {
            let ls = build_set_rhs_sql_sync(a, params, vars, values);
            let rs = build_set_rhs_sql_sync(b, params, vars, values);
            format!("({} - {})", ls, rs)
        }
        Expr::Mul(a, b) => {
            let ls = build_set_rhs_sql_sync(a, params, vars, values);
            let rs = build_set_rhs_sql_sync(b, params, vars, values);
            format!("({} * {})", ls, rs)
        }
        Expr::Div(a, b) => {
            let ls = build_set_rhs_sql_sync(a, params, vars, values);
            let rs = build_set_rhs_sql_sync(b, params, vars, values);
            format!("({} / {})", ls, rs)
        }
        _ => {
            let v = values
                .next()
                .expect("fallback collector must produce one value per non-Var leaf");
            params.push(value_to_sql_param(&v));
            format!("${}", params.len())
        }
    }
}

pub(super) fn value_to_positive_int(value: &Value, clause: &str) -> Result<i64> {
    match value {
        Value::Int(n) if *n >= 0 => Ok(*n),
        Value::Int(n) => bail!("{clause} must be non-negative, got {n}"),
        Value::Str(s) => s
            .parse::<i64>()
            .map_err(|_| anyhow!("{clause} must be integer, got string '{s}'"))
            .and_then(|n| {
                if n >= 0 {
                    Ok(n)
                } else {
                    Err(anyhow!("{clause} must be non-negative, got {n}"))
                }
            }),
        other => bail!("{clause} must be integer, got {}", other.type_name()),
    }
}

/// `Expr::DbAggregate` / `Expr::DbCount` use the same shape. Surface
/// `AggregateKind` here so eval.rs can stay agnostic of the SQL operators.
pub(super) fn aggregate_sql_op(kind: AggregateKind, col: &str) -> (String, &'static str) {
    let agg_sql = match kind {
        AggregateKind::Sum => format!("SUM(\"{}\")", col),
        AggregateKind::Avg => format!("AVG(\"{}\")", col),
        AggregateKind::Min => format!("MIN(\"{}\")", col),
        AggregateKind::Max => format!("MAX(\"{}\")", col),
    };
    let tag = match kind {
        AggregateKind::Sum => "sum",
        AggregateKind::Avg => "avg",
        AggregateKind::Min => "min",
        AggregateKind::Max => "max",
    };
    (agg_sql, tag)
}
