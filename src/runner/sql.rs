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

use crate::ast::{
    AggProj, AggregateKind, AliasedCol, DbOrderBy, Expr, JoinClause, ModelDecl, NavigationKind,
    SortDir, WhereExpr,
};

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

/// Bind a runtime array as a real Postgres array param (for `col = ANY($N)`).
/// Element type is inferred from the (homogeneous) first element: int → int4[]
/// (or int8[] when a value exceeds i32), string → text[]. Empty → int4[].
pub(super) fn array_to_sql_param(items: &[Value]) -> Result<Box<dyn ToSql + Sync + Send>> {
    match items.first() {
        None => Ok(Box::new(Vec::<i32>::new())),
        Some(Value::Int(_)) => {
            if !items.iter().all(|x| matches!(x, Value::Int(_))) {
                bail!("in (<list>): array must be all-int or all-string");
            }
            let fits_i32 = items.iter().all(
                |x| matches!(x, Value::Int(n) if (i32::MIN as i64..=i32::MAX as i64).contains(n)),
            );
            if fits_i32 {
                let v: Vec<i32> = items
                    .iter()
                    .map(|x| match x {
                        Value::Int(n) => *n as i32,
                        _ => 0,
                    })
                    .collect();
                Ok(Box::new(v))
            } else {
                let v: Vec<i64> = items
                    .iter()
                    .map(|x| match x {
                        Value::Int(n) => *n,
                        _ => 0,
                    })
                    .collect();
                Ok(Box::new(v))
            }
        }
        Some(Value::Str(_)) => {
            if !items.iter().all(|x| matches!(x, Value::Str(_))) {
                bail!("in (<list>): array must be all-int or all-string");
            }
            let v: Vec<String> = items
                .iter()
                .map(|x| match x {
                    Value::Str(s) => s.clone(),
                    _ => String::new(),
                })
                .collect();
            Ok(Box::new(v))
        }
        Some(other) => bail!(
            "in (<list>): unsupported array element type {}",
            other.type_name()
        ),
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
    main_entity: &str,
    where_clause: Option<&WhereExpr>,
    order_by: Option<&DbOrderBy>,
    limit: Option<&Expr>,
    offset: Option<&Expr>,
    first: bool,
    nav_subqueries: &[NavigationSubquery],
    projection: &[String],
    aggregates: &[AggProj],
    aliased_cols: &[AliasedCol],
    joins: &[JoinClause],
    group_by: &[String],
    having: Option<&WhereExpr>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
) -> Result<(String, Vec<Box<dyn ToSql + Sync + Send>>, String, String)> {
    let mut sql_where = String::new();
    let mut shape_bits: Vec<String> = Vec::new();
    let mut cache_bits: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();

    // Alias map for JOIN queries: main entity → "t", each join → "j{i}". When
    // there are no joins, `alias_opt` is None and every column stays unqualified
    // (byte-identical to the historical single-table SQL).
    let mut aliases: HashMap<String, String> = HashMap::new();
    aliases.insert(main_entity.to_lowercase(), "t".to_string());
    for (i, j) in joins.iter().enumerate() {
        aliases.insert(j.entity.to_lowercase(), format!("j{}", i));
    }
    let alias_opt = if joins.is_empty() {
        None
    } else {
        Some(&aliases)
    };

    if let Some(wc) = where_clause {
        let where_sql = build_where_sql(
            wc,
            &mut params,
            &mut shape_bits,
            &mut cache_bits,
            vars,
            vm,
            alias_opt,
        )
        .await?;
        sql_where = format!(" WHERE {}", where_sql);
    }

    // GROUP BY + HAVING — emitted between WHERE and ORDER BY. The shape /
    // cache keys include the column list so two different groupings don't
    // collide in the prepared-statement cache.
    let mut sql_group = String::new();
    if !group_by.is_empty() {
        let key_cols: Vec<String> = group_by.iter().map(|f| field_path_to_col(f)).collect();
        let cols: Vec<String> = group_by
            .iter()
            .map(|f| where_col_sql(f, alias_opt))
            .collect();
        sql_group = format!(" GROUP BY {}", cols.join(", "));
        shape_bits.push(format!("group:{}", key_cols.join(",")));
        cache_bits.push(format!("group:{}", key_cols.join(",")));
    }

    let mut sql_having = String::new();
    if let Some(hv) = having {
        let having_sql = build_where_sql(
            hv,
            &mut params,
            &mut shape_bits,
            &mut cache_bits,
            vars,
            vm,
            alias_opt,
        )
        .await?;
        sql_having = format!(" HAVING {}", having_sql);
    }

    let mut sql_order = String::new();
    if let Some(ob) = order_by {
        let col = field_path_to_col(&ob.field);
        let dir = match ob.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        };
        sql_order = format!(" ORDER BY {} {}", where_col_sql(&ob.field, alias_opt), dir);
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
    // Explicit projection (named columns and/or aliased aggregates) suppresses
    // the `t.*` / `*` fallback — required for grouped aggregation, where a bare
    // `SELECT *` under GROUP BY is invalid SQL.
    let explicit = !projection.is_empty() || !aggregates.is_empty() || !aliased_cols.is_empty();
    let mut bits: Vec<String> = Vec::new();
    if explicit {
        for c in projection {
            bits.push(format!("t.\"{}\"", c));
        }
        for ac in aliased_cols {
            bits.push(format!(
                "{} AS \"{}\"",
                where_col_sql(&ac.field, alias_opt),
                ac.alias
            ));
        }
        for agg in aggregates {
            bits.push(agg_select_sql(agg, alias_opt));
        }
    } else if !nav_subqueries.is_empty() {
        bits.push("t.*".to_string());
    }
    for nav in nav_subqueries {
        bits.push(format!(
            "{} AS \"{}\"",
            nav.sql_fragment("t", 0),
            nav.alias()
        ));
    }
    if !projection.is_empty() {
        shape_bits.push(format!("project:{}", projection.join(",")));
        cache_bits.push(format!("project:{}", projection.join(",")));
    }
    for ac in aliased_cols {
        shape_bits.push(format!("acol:{}:{}", ac.alias, ac.field));
        cache_bits.push(format!("acol:{}:{}", ac.alias, ac.field));
    }
    for agg in aggregates {
        shape_bits.push(format!("agg:{}:{:?}:{}", agg.alias, agg.kind, agg.col));
        cache_bits.push(format!("agg:{}:{:?}:{}", agg.alias, agg.kind, agg.col));
    }
    for nav in nav_subqueries {
        // Flat navs keep the historical `with:<name>` key (no cache regression);
        // a nested nav appends its children so `with boards` and
        // `with boards.columns` never share a prepared-statement / result entry.
        let mut key = format!("with:{}", nav.alias());
        if !nav.children.is_empty() {
            let kids: Vec<&str> = nav.children.iter().map(|c| c.alias()).collect();
            key.push_str(&format!(".{}", kids.join("+")));
        }
        shape_bits.push(key.clone());
        cache_bits.push(key);
    }
    let inner_projection = if bits.is_empty() {
        "*".to_string()
    } else {
        bits.join(", ")
    };

    // FROM with optional JOINs — each joined entity aliased as j{i}.
    let mut from_sql = format!("\"{}\" t", table_name);
    for (i, j) in joins.iter().enumerate() {
        let jt = crate::sql::to_snake_case(&j.entity);
        from_sql.push_str(&format!(
            " JOIN \"{}\" j{} ON {} = {}",
            jt,
            i,
            where_col_sql(&j.left, Some(&aliases)),
            where_col_sql(&j.right, Some(&aliases)),
        ));
        shape_bits.push(format!("join:{}", jt));
        cache_bits.push(format!("join:{}", jt));
    }

    let inner_sql = format!(
        "SELECT {} FROM {}{}{}{}{}{}",
        inner_projection, from_sql, sql_where, sql_group, sql_having, sql_order, sql_limit_offset
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

pub(crate) struct NavigationSubquery {
    name: String,
    kind: NavigationKind,
    target_table: String,
    /// Entity (model) name of the target — used to resolve nested children.
    target_entity: String,
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
    /// Nested `with parent.child` navs loaded one level deeper, correlated to
    /// this nav's own rows. Empty for a flat (single-level) nav, in which case
    /// the emitted SQL stays byte-for-byte identical to the pre-nesting form.
    children: Vec<NavigationSubquery>,
}

fn sort_dir_sql(dir: SortDir) -> &'static str {
    match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    }
}

/// A `<fn>(...) AS "alias"` term for an aliased aggregate projection.
pub(crate) fn agg_select_sql(agg: &AggProj, aliases: Option<&HashMap<String, String>>) -> String {
    let func = match agg.kind {
        AggregateKind::Count => return format!("count(*) AS \"{}\"", agg.alias),
        AggregateKind::Sum => "sum",
        AggregateKind::Avg => "avg",
        AggregateKind::Min => "min",
        AggregateKind::Max => "max",
    };
    let col_sql = match aliases {
        None => format!("t.\"{}\"", field_path_to_col(&agg.col)),
        Some(m) => where_col_sql(&agg.col, Some(m)),
    };
    format!("{}({}) AS \"{}\"", func, col_sql, agg.alias)
}

impl NavigationSubquery {
    pub(crate) fn alias(&self) -> &str {
        &self.name
    }

    /// The per-row JSON value for this nav's target rows (aliased `row_alias`):
    /// the whole row, or a `json_build_object` of the projected columns. When
    /// the nav has nested children the row is widened to `jsonb` and each child
    /// key is merged in with `||` via a correlated sub-subquery one level
    /// deeper. Without children this is the legacy `row_to_json` /
    /// `json_build_object` form, byte-for-byte.
    fn json_value(&self, row_alias: &str, depth: usize) -> String {
        let base = if self.projection.is_empty() {
            if self.children.is_empty() {
                format!("row_to_json({})", row_alias)
            } else {
                format!("to_jsonb({})", row_alias)
            }
        } else {
            let builder = if self.children.is_empty() {
                "json_build_object"
            } else {
                "jsonb_build_object"
            };
            let pairs: Vec<String> = self
                .projection
                .iter()
                .map(|col| format!("'{}', {}.\"{}\"", col, row_alias, col))
                .collect();
            format!("{}({})", builder, pairs.join(", "))
        };
        if self.children.is_empty() {
            return base;
        }
        let mut merged = base;
        for child in &self.children {
            merged = format!(
                "{} || jsonb_build_object('{}', {})",
                merged,
                child.alias(),
                child.sql_fragment(row_alias, depth + 1)
            );
        }
        format!("({})", merged)
    }

    /// Render the correlated subquery for this nav. `source_alias` is the outer
    /// row this nav joins back to (`t` at the top level, the parent nav's row
    /// alias when nested). Row/link aliases are depth-derived so nested levels
    /// never collide; depth 0 keeps the historical `c` / `j` aliases so flat
    /// navs emit unchanged SQL.
    pub(crate) fn sql_fragment(&self, source_alias: &str, depth: usize) -> String {
        let row_alias = if depth == 0 {
            "c".to_string()
        } else {
            format!("c{}", depth)
        };
        let val = self.json_value(&row_alias, depth);
        let order = match &self.order_by {
            Some((col, dir)) => {
                format!(" ORDER BY {}.\"{}\" {}", row_alias, col, sort_dir_sql(*dir))
            }
            None => String::new(),
        };
        match self.kind {
            NavigationKind::OneToMany => format!(
                "COALESCE((SELECT json_agg({}{}) FROM \"{}\" {} WHERE {}.\"{}\" = {}.\"{}\"), '[]'::json)",
                val, order, self.target_table, row_alias, row_alias, self.join_target_col, source_alias, self.join_source_col
            ),
            // OneToOne (target holds FK) and BelongsTo (source holds FK) both
            // materialise a single nested object; only the join direction —
            // captured in join_target_col / join_source_col — differs.
            NavigationKind::OneToOne | NavigationKind::BelongsTo => format!(
                "(SELECT {} FROM \"{}\" {} WHERE {}.\"{}\" = {}.\"{}\"{} LIMIT 1)",
                val, self.target_table, row_alias, row_alias, self.join_target_col, source_alias, self.join_source_col, order
            ),
            // ManyToMany: target rows reached through the link table.
            // join_target_col = target PK, join_source_col = source PK.
            NavigationKind::ManyToMany => {
                let (jt, near, far) = self
                    .join
                    .as_ref()
                    .expect("ManyToMany navigation must carry join-table coordinates");
                let link_alias = if depth == 0 {
                    "j".to_string()
                } else {
                    format!("j{}", depth)
                };
                format!(
                    "COALESCE((SELECT json_agg({}{}) FROM \"{}\" {} JOIN \"{}\" {} ON {}.\"{}\" = {}.\"{}\" WHERE {}.\"{}\" = {}.\"{}\"), '[]'::json)",
                    val, order, self.target_table, row_alias, jt, link_alias, link_alias, far, row_alias, self.join_target_col, link_alias, near, source_alias, self.join_source_col
                )
            }
        }
    }
}

/// Build a single (flat) nav subquery for `rel` on `source_entity`. Nested
/// children are attached by the caller. Shared by the top-level list and the
/// recursive `with parent.child` resolution.
fn build_one_nav(
    source_entity: &str,
    rel: &str,
    models: &HashMap<String, &ModelDecl>,
    pk_by_table: &HashMap<String, Vec<String>>,
) -> Result<NavigationSubquery> {
    let entity_key = source_entity.to_lowercase();
    let model = models
        .get(&entity_key)
        .ok_or_else(|| anyhow!("unknown entity '{}' for 'with' clause", source_entity))?;
    let source_pk_col = pk_by_table
        .get(&entity_key)
        .and_then(|v| v.first().cloned())
        .unwrap_or_else(|| "id".to_string());
    let rel_key = rel.to_lowercase();
    let nav = model
        .navigations
        .iter()
        .find(|n| n.name.to_lowercase() == rel_key)
        .ok_or_else(|| anyhow!("entity '{}' has no navigation '{}'", source_entity, rel))?;
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
    Ok(NavigationSubquery {
        name: nav.name.clone(),
        kind: nav.kind,
        target_table: crate::sql::to_snake_case(&nav.target_entity),
        target_entity: nav.target_entity.clone(),
        join_target_col,
        join_source_col,
        projection: nav.projection.clone(),
        join,
        order_by: nav
            .order_by
            .as_ref()
            .map(|o| (field_path_to_col(&o.col), o.dir)),
        children: Vec::new(),
    })
}

pub(crate) fn build_navigation_subqueries(
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

    // Group dotted entries (`with boards.columns, boards.x`) by their top-level
    // nav, preserving first-appearance order. Each head becomes one nav; its
    // tails become nested children resolved against the head's target entity.
    let mut order: Vec<String> = Vec::new();
    let mut child_names: HashMap<String, Vec<String>> = HashMap::new();
    for rel in requested {
        let (head, tail) = match rel.split_once('.') {
            Some((h, t)) => (h.to_string(), Some(t.to_string())),
            None => (rel.clone(), None),
        };
        if !child_names.contains_key(&head) {
            order.push(head.clone());
            child_names.insert(head.clone(), Vec::new());
        }
        if let Some(t) = tail {
            child_names
                .get_mut(&head)
                .expect("head key just inserted")
                .push(t);
        }
    }

    let mut out = Vec::with_capacity(order.len());
    for head in &order {
        let mut nav = build_one_nav(entity, head, models, pk_by_table)?;
        let child_entity = nav.target_entity.clone();
        for child in &child_names[head] {
            nav.children
                .push(build_one_nav(&child_entity, child, models, pk_by_table)?);
        }
        out.push(nav);
    }
    Ok(out)
}

/// The SQL column reference for a WHERE field. Without an alias map (single
/// table) it's the bare quoted column — byte-identical to the historical
/// output. With a map (JOIN query) it's qualified by the entity's table alias:
/// `Entity.col` → `<alias>."col"`, bare/unknown → the main alias `t`.
pub(crate) fn where_col_sql(field: &str, aliases: Option<&HashMap<String, String>>) -> String {
    match aliases {
        None => format!("\"{}\"", field_path_to_col(field)),
        Some(map) => {
            if let Some((ent, c)) = field.split_once('.') {
                if let Some(a) = map.get(&ent.to_lowercase()) {
                    return format!("{}.\"{}\"", a, c);
                }
            }
            format!("t.\"{}\"", field_path_to_col(field))
        }
    }
}

#[async_recursion]
pub(super) async fn build_where_sql(
    expr: &WhereExpr,
    params: &mut Vec<Box<dyn ToSql + Sync + Send>>,
    shape: &mut Vec<String>,
    cache: &mut Vec<String>,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm<'_>,
    aliases: Option<&HashMap<String, String>>,
) -> Result<String> {
    match expr {
        WhereExpr::Atom(wc) => {
            let col = field_path_to_col(&wc.field);
            let csql = where_col_sql(&wc.field, aliases);
            // `==?` / `like?` etc. — an optional predicate: if the value is "" or
            // null at runtime, the term is dropped (no value → no filter). Lets a
            // single static query serve optional filters without in-code branching.
            let optional = wc.op.ends_with('?');
            let raw_op = wc.op.trim_end_matches('?');
            let rhs_val = vm.eval_expr(&wc.rhs, vars).await?;
            if optional
                && (matches!(rhs_val, Value::Null | Value::Void)
                    || matches!(&rhs_val, Value::Str(s) if s.is_empty()))
            {
                shape.push(format!("where:{col}:opt_skip"));
                cache.push(format!("where:{col}:opt_skip"));
                return Ok("TRUE".to_string());
            }
            let op = normalize_sql_op(raw_op);

            Ok(match rhs_val {
                Value::Null | Value::Void => {
                    if op == "!=" {
                        shape.push(format!("where:{col}:is_not_null"));
                        cache.push(format!("where:{col}:is_not_null"));
                        format!("{} IS NOT NULL", csql)
                    } else {
                        shape.push(format!("where:{col}:is_null"));
                        cache.push(format!("where:{col}:is_null"));
                        format!("{} IS NULL", csql)
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
                    format!("{} {} ${}", csql, op, idx)
                }
            })
        }
        WhereExpr::Between { field, low, high } => {
            let col = field_path_to_col(field);
            let csql = where_col_sql(field, aliases);
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
            Ok(format!("{} BETWEEN ${} AND ${}", csql, low_idx, high_idx))
        }
        WhereExpr::InList { field, values } => {
            let col = field_path_to_col(field);
            let csql = where_col_sql(field, aliases);
            if values.is_empty() {
                bail!("WHERE 'in (...)' must have at least one value");
            }
            // Dynamic-length list: `where X.id in (@ids)` where @ids is a runtime
            // array → bind the whole array as one param and emit `= ANY($N)`.
            // A single scalar keeps the `IN ($N)` shape.
            if values.len() == 1 {
                let v = vm.eval_expr(&values[0], vars).await?;
                if let Value::Array(items) = &v {
                    params.push(array_to_sql_param(items)?);
                    shape.push(format!("where:{col}:any"));
                    cache.push(format!("where:{col}:any[{}]", value_to_cache_fragment(&v)));
                    return Ok(format!("{} = ANY(${})", csql, params.len()));
                }
                params.push(value_to_sql_param(&v));
                shape.push(format!("where:{col}:in(1)"));
                cache.push(format!("where:{col}:in[{}]", value_to_cache_fragment(&v)));
                return Ok(format!("{} IN (${})", csql, params.len()));
            }
            // Fixed-arity literal/param list → IN ($1, $2, ...).
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
            Ok(format!("{} IN ({})", csql, placeholders.join(", ")))
        }
        WhereExpr::And(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm, aliases).await?;
            shape.push("AND".to_string());
            cache.push("AND".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm, aliases).await?;
            Ok(format!("({} AND {})", ls, rs))
        }
        WhereExpr::Or(l, r) => {
            let ls = build_where_sql(l, params, shape, cache, vars, vm, aliases).await?;
            shape.push("OR".to_string());
            cache.push("OR".to_string());
            let rs = build_where_sql(r, params, shape, cache, vars, vm, aliases).await?;
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
        // Count only appears in aliased grouped projections (handled in
        // build_select_sql); the scalar DbCount path uses count(*) directly.
        AggregateKind::Count => "COUNT(*)".to_string(),
    };
    let tag = match kind {
        AggregateKind::Sum => "sum",
        AggregateKind::Avg => "avg",
        AggregateKind::Min => "min",
        AggregateKind::Max => "max",
        AggregateKind::Count => "count",
    };
    (agg_sql, tag)
}

#[cfg(test)]
mod nav_sql_tests {
    use super::*;
    use crate::parser::parse_program;

    const SCHEMA: &str = r#"
        dbcontext AppDb : Postgres;
        entity Project of AppDb {
            id int pk;
            name varchar(64);
            boards: List<Board> via Board.projectId;
        }
        entity Board of AppDb {
            id int pk;
            projectId int;
            lanes: List<Lane> via Lane.boardId;
        }
        entity Lane of AppDb {
            id int pk;
            boardId int;
            name varchar(64);
        }
    "#;

    /// Parse `SCHEMA` and build the `(models, pk_by_table)` maps the nav
    /// builder expects — both keyed by lowercased entity name.
    fn maps() -> (
        HashMap<String, &'static ModelDecl>,
        HashMap<String, Vec<String>>,
    ) {
        // Leak the parsed program so the borrowed `&ModelDecl`s outlive this
        // helper — fine for a test, keeps the call sites readable.
        let program = Box::leak(Box::new(parse_program(SCHEMA).expect("schema parses")));
        let models = program
            .models
            .iter()
            .map(|m| (m.name.to_lowercase(), m))
            .collect();
        let mut pk_by_table = HashMap::new();
        for m in &program.models {
            let pks = m
                .fields
                .iter()
                .filter(|f| f.is_primary_key)
                .map(|f| f.name.clone())
                .collect();
            pk_by_table.insert(m.name.to_lowercase(), pks);
        }
        (models, pk_by_table)
    }

    #[test]
    fn flat_nav_sql_is_unchanged_legacy_form() {
        let (models, pk) = maps();
        let navs = build_navigation_subqueries(
            "Project",
            "project",
            &["boards".to_string()],
            &models,
            &pk,
        )
        .expect("flat nav builds");
        assert_eq!(navs.len(), 1);
        assert!(navs[0].children.is_empty());
        let sql = navs[0].sql_fragment("t", 0);
        // Byte-for-byte the historical single-level form: row_to_json + json,
        // no jsonb widening anywhere.
        assert_eq!(
            sql,
            "COALESCE((SELECT json_agg(row_to_json(c)) FROM \"board\" c WHERE c.\"projectId\" = t.\"id\"), '[]'::json)"
        );
    }

    #[test]
    fn nested_nav_merges_child_one_level_deeper() {
        let (models, pk) = maps();
        let navs = build_navigation_subqueries(
            "Project",
            "project",
            &["boards.lanes".to_string()],
            &models,
            &pk,
        )
        .expect("nested nav builds");
        assert_eq!(navs.len(), 1, "one head nav");
        assert_eq!(navs[0].children.len(), 1, "one nested child");
        assert_eq!(navs[0].children[0].alias(), "lanes");
        let sql = navs[0].sql_fragment("t", 0);
        // Parent row widened to jsonb so the child key can be merged in.
        assert!(sql.contains("to_jsonb(c)"), "parent widened: {sql}");
        assert!(
            sql.contains("|| jsonb_build_object('lanes',"),
            "child merged as a key: {sql}"
        );
        // Child correlates to the parent row alias `c` via a fresh `c1` alias.
        assert!(
            sql.contains("FROM \"lane\" c1 WHERE c1.\"boardId\" = c.\"id\""),
            "child correlated one level deeper: {sql}"
        );
        // Outer board still correlates to the top-level `t`.
        assert!(
            sql.contains("FROM \"board\" c WHERE c.\"projectId\" = t.\"id\""),
            "parent correlated to outer query: {sql}"
        );
    }

    #[test]
    fn dotted_entries_merge_under_one_head() {
        // `boards.lanes, boards` collapses to a single boards nav carrying the
        // lanes child — not two colliding boards keys.
        let (models, pk) = maps();
        let navs = build_navigation_subqueries(
            "Project",
            "project",
            &["boards.lanes".to_string(), "boards".to_string()],
            &models,
            &pk,
        )
        .expect("builds");
        assert_eq!(navs.len(), 1);
        assert_eq!(navs[0].children.len(), 1);
    }
}
