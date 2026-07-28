//! Statement execution — `exec_block`, `exec_stmt`, and the atomic
//! `UPDATE ... SET ... WHERE ...` helper.
//!
//! `exec_block` owns the block-scoped `let` story: every locally-bound name
//! has its outer value (and dirty-field set) shadowed on entry and restored
//! on exit so loop bodies can re-`let` the same name each iteration without
//! tripping the duplicate-check in `Stmt::Let`.
//!
//! `exec_stmt` is the giant `match Stmt` arm — print, if/while, try/catch,
//! transactions, for-in iteration, validate body, and every `db <verb>`
//! statement live here. SQL-building is delegated to [`super::sql`] so this
//! file stays focused on control flow.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use async_recursion::async_recursion;
use serde_json::{json, Value as JsonValue};
use tokio_postgres::types::ToSql;

use crate::ast::{Expr, Stmt, WhereExpr};

use super::sql::{
    boxed_params_to_refs, build_delete_sql, build_insert_sql, build_set_rhs_sql, build_update_sql,
    build_where_sql, get_var_as_json,
};
use super::validation::run_validation_rules;
use super::{
    catch_type_matches, classify_jwc_error, engine, json_to_value, value_to_json, Flow, Value, Vm,
};

impl<'a> Vm<'a> {
    #[async_recursion]
    pub(super) async fn exec_block(
        &mut self,
        stmts: &[Stmt],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Flow> {
        // Block-scoped `let`: variables declared inside this block are
        // visible only until it ends. We remember each new binding (and the
        // outer value it shadows, if any) and restore the outer state when
        // the block exits — including on early Flow exits and errors. This
        // also lets a loop body re-`let` the same name each iteration
        // without tripping the duplicate-variable check in `Stmt::Let`.
        let mut block_locals: HashMap<String, (Option<Value>, Option<HashSet<String>>)> =
            HashMap::new();
        let mut pending_err: Option<anyhow::Error> = None;
        let mut return_flow = Flow::Continue;
        for stmt in stmts {
            if let Stmt::Let { name, .. } = stmt {
                let key = name.to_lowercase();
                if block_locals.contains_key(&key) {
                    pending_err = Some(anyhow!("Duplicate variable declaration: {name}"));
                    break;
                }
                let prior_val = vars.remove(&key);
                let prior_dirty = self.dirty_fields.remove(&key);
                block_locals.insert(key, (prior_val, prior_dirty));
            }
            match self.exec_stmt(stmt, vars).await {
                Ok(flow) => {
                    if !matches!(flow, Flow::Continue) {
                        return_flow = flow;
                        break;
                    }
                }
                Err(e) => {
                    pending_err = Some(e);
                    break;
                }
            }
        }
        for (key, (prior_val, prior_dirty)) in block_locals.drain() {
            match prior_val {
                Some(v) => {
                    vars.insert(key.clone(), v);
                }
                None => {
                    vars.remove(&key);
                }
            }
            match prior_dirty {
                Some(d) => {
                    self.dirty_fields.insert(key, d);
                }
                None => {
                    self.dirty_fields.remove(&key);
                }
            }
        }
        if let Some(e) = pending_err {
            return Err(e);
        }
        Ok(return_flow)
    }

    #[async_recursion]
    pub(super) async fn exec_stmt(
        &mut self,
        stmt: &Stmt,
        vars: &mut HashMap<String, Value>,
    ) -> Result<Flow> {
        match stmt {
            Stmt::Let { name, value } => {
                let key = name.to_lowercase();
                if vars.contains_key(&key) {
                    bail!("Duplicate variable declaration: {name}");
                }
                let evaluated = self.eval_expr(value, vars).await?;
                vars.insert(key.clone(), evaluated);
                // New binding has no modified fields yet.
                self.dirty_fields.remove(&key);
                Ok(Flow::Continue)
            }
            Stmt::Assign { name, value } => {
                let key = name.to_lowercase();
                if !vars.contains_key(&key) {
                    bail!("Assignment to undefined variable: {name}");
                }
                let evaluated = self.eval_expr(value, vars).await?;
                vars.insert(key.clone(), evaluated);
                // Full rebind resets the modified-field set.
                self.dirty_fields.remove(&key);
                Ok(Flow::Continue)
            }
            Stmt::Print(expr) => {
                let value = self.eval_expr(expr, vars).await?;
                self.output.push_str(&value.as_string());
                self.output.push('\n');
                Ok(Flow::Continue)
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_value = self.eval_expr(cond, vars).await?;
                match cond_value {
                    Value::Bool(true) => self.exec_block(then_body, vars).await,
                    Value::Bool(false) => {
                        if let Some(else_body) = else_body {
                            self.exec_block(else_body, vars).await
                        } else {
                            Ok(Flow::Continue)
                        }
                    }
                    other => bail!("if condition must be bool, got {}", other.type_name()),
                }
            }
            Stmt::While { cond, body } => {
                const MAX_ITERS: usize = 100_000;
                for _ in 0..MAX_ITERS {
                    let cond_value = self.eval_expr(cond, vars).await?;
                    match cond_value {
                        Value::Bool(true) => match self.exec_block(body, vars).await? {
                            Flow::Continue => {}
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Break => return Ok(Flow::Continue),
                            Flow::ContinueLoop => continue,
                        },
                        Value::Bool(false) => return Ok(Flow::Continue),
                        other => bail!("while condition must be bool, got {}", other.type_name()),
                    }
                }
                bail!("while loop exceeded iteration limit ({MAX_ITERS})")
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::ContinueLoop),
            Stmt::Expr(expr) => {
                let _ = self.eval_expr(expr, vars).await?;
                Ok(Flow::Continue)
            }
            Stmt::Return(None) => Ok(Flow::Return(None)),
            Stmt::Return(Some(expr)) => {
                let value = self.eval_expr(expr, vars).await?;
                Ok(Flow::Return(Some(value)))
            }
            Stmt::FieldAssign { var, field, value } => {
                let key = var.to_lowercase();
                let current = vars
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| anyhow!("FieldAssign: variable '{}' not found", var))?;
                let new_val = self.eval_expr(value, vars).await?;
                match current {
                    // **Phase 1** — Record CoW mutation. If the field exists,
                    // write in place via `Arc::make_mut` on the values Arc
                    // (shape Arc stays shared). If it's a new field, the shape
                    // would have to grow — fall through to the JSON-string
                    // path below by re-serializing.
                    Value::Record {
                        field_names,
                        values,
                    } => {
                        if let Some(idx) = field_names
                            .iter()
                            .position(|f| f.as_ref() == field.as_str())
                        {
                            let mut new_values = values.clone();
                            let slot = Arc::make_mut(&mut new_values);
                            slot[idx] = new_val;
                            vars.insert(
                                key.clone(),
                                Value::Record {
                                    field_names,
                                    values: new_values,
                                },
                            );
                        } else {
                            // New field — convert to JSON and reuse the
                            // existing string-path semantics so shape grow is
                            // a single code path.
                            let mut doc = value_to_json(&Value::Record {
                                field_names,
                                values,
                            });
                            if let Some(obj) = doc.as_object_mut() {
                                obj.insert(field.clone(), value_to_json(&new_val));
                            }
                            vars.insert(key.clone(), Value::Str(doc.to_string()));
                        }
                    }
                    Value::Str(s) => {
                        let mut doc: serde_json::Value = serde_json::from_str(&s)
                            .unwrap_or(serde_json::Value::Object(Default::default()));
                        if let Some(obj) = doc.as_object_mut() {
                            obj.insert(field.clone(), value_to_json(&new_val));
                        }
                        vars.insert(key.clone(), Value::Str(doc.to_string()));
                    }
                    Value::Null => {
                        let mut doc = serde_json::Value::Object(Default::default());
                        if let Some(obj) = doc.as_object_mut() {
                            obj.insert(field.clone(), value_to_json(&new_val));
                        }
                        vars.insert(key.clone(), Value::Str(doc.to_string()));
                    }
                    other => bail!(
                        "FieldAssign: '{}' is not an object, got {}",
                        var,
                        other.type_name()
                    ),
                }
                self.dirty_fields
                    .entry(key)
                    .or_default()
                    .insert(field.clone());
                Ok(Flow::Continue)
            }
            Stmt::Try {
                body,
                catch_var,
                catch_type,
                catch_body,
            } => {
                let try_result = self.exec_block(body, vars).await;
                match try_result {
                    Ok(flow) => Ok(flow),
                    Err(e) => {
                        let kind = classify_jwc_error(&e);
                        if !catch_type_matches(catch_type.as_deref(), kind) {
                            // Annotated `catch (e: T)` clause that doesn't match
                            // this error kind — re-raise so an outer handler
                            // (or the errorHandler) gets a shot at it.
                            return Err(e);
                        }
                        let mut all_msgs: Vec<String> = e.chain().map(|c| c.to_string()).collect();
                        let message = if all_msgs.is_empty() {
                            "unknown error".to_string()
                        } else {
                            all_msgs.remove(0)
                        };
                        let err_obj = json!({
                            "type": kind,
                            "message": message,
                            "causes": all_msgs,
                        });
                        let key = catch_var.to_lowercase();
                        // Inject the error binding; restore the prior value (if any)
                        // when leaving the catch block.
                        let prior = vars.insert(key.clone(), Value::Str(err_obj.to_string()));
                        let catch_flow = self.exec_block(catch_body, vars).await;
                        match prior {
                            Some(v) => {
                                vars.insert(key, v);
                            }
                            None => {
                                vars.remove(&key);
                            }
                        }
                        catch_flow
                    }
                }
            }
            Stmt::Transaction { body } => {
                let body_clone = body.clone();
                // Run the body inside an engine-level transaction scope. The
                // body is a future capturing `self` and `vars` by mutable
                // reference, which is fine because `with_tx` polls it
                // immediately to completion.
                let outcome: Result<Flow> = engine::with_tx(|| {
                    Box::pin(async move { self.exec_block(&body_clone, vars).await })
                })
                .await;
                outcome
            }
            Stmt::Savepoint { name, body } => {
                // Sprint 4B — wire `savepoint <name> { ... }` to the
                // engine's SAVEPOINT/RELEASE/ROLLBACK helper. Outside a
                // transaction the helper bails with E017; inside, body
                // errors rollback to the savepoint without poisoning
                // the outer transaction.
                let body_clone = body.clone();
                let name_clone = name.clone();
                let outcome: Result<Flow> = engine::with_savepoint(&name_clone, || {
                    Box::pin(async move { self.exec_block(&body_clone, vars).await })
                })
                .await;
                outcome
            }
            Stmt::ForIn { var, iter, body } => {
                let iter_val = self.eval_expr(iter, vars).await?;
                // Three iterable shapes:
                //   * Value::Array — iterate the elements directly.
                //   * Value::Str holding a JSON array — parse then iterate
                //     (DB selects / body() return arrays this way).
                //   * Value::Null — zero iterations.
                let items: Vec<Value> = match iter_val {
                    Value::Array(items) => items,
                    Value::Str(s) => {
                        let parsed: JsonValue = serde_json::from_str(&s)
                            .map_err(|_| anyhow!("for-in: iter is not valid JSON"))?;
                        let arr = parsed
                            .as_array()
                            .ok_or_else(|| anyhow!("for-in: iter is not a JSON array"))?;
                        arr.iter().map(json_to_value).collect()
                    }
                    Value::Null => return Ok(Flow::Continue),
                    other => bail!("for-in: iter must be an array, got {}", other.type_name()),
                };
                let key = var.to_lowercase();
                let prior = vars.get(&key).cloned();
                let prior_dirty = self.dirty_fields.remove(&key);
                let mut early_return: Option<Option<Value>> = None;
                for item in items.into_iter() {
                    vars.insert(key.clone(), item);
                    match self.exec_block(body, vars).await? {
                        Flow::Continue | Flow::ContinueLoop => continue,
                        Flow::Break => break,
                        Flow::Return(v) => {
                            early_return = Some(v);
                            break;
                        }
                    }
                }
                match prior {
                    Some(p) => {
                        vars.insert(key.clone(), p);
                    }
                    None => {
                        vars.remove(&key);
                    }
                }
                if let Some(d) = prior_dirty {
                    self.dirty_fields.insert(key, d);
                }
                if let Some(v) = early_return {
                    return Ok(Flow::Return(v));
                }
                Ok(Flow::Continue)
            }
            Stmt::ValidateBody { fields } => {
                let body = self.request_body.clone().unwrap_or_default();
                let parsed: JsonValue = if body.trim().is_empty() {
                    JsonValue::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&body)
                        .with_context(|| "validate body: request body is not valid JSON")?
                };

                let errors = run_validation_rules(fields, &parsed);
                if !errors.is_empty() {
                    // Same envelope as every other error response; the
                    // per-field detail lives under `details`. `__jwc_status__`
                    // is the runner's internal status sentinel and is stripped
                    // before the body reaches the client.
                    let mut error_doc: serde_json::Map<String, JsonValue> = serde_json::from_str(
                        &crate::http_error::validation_failed(JsonValue::Object(errors)),
                    )
                    .expect("envelope is a JSON object");
                    error_doc.insert("__jwc_status__".into(), json!(400));
                    return Ok(Flow::Return(Some(Value::Str(
                        JsonValue::Object(error_doc).to_string(),
                    ))));
                }

                Ok(Flow::Continue)
            }
            Stmt::DbInsert {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let col_types = self
                    .models
                    .get(&table.to_lowercase())
                    .map(|m| super::sql::column_types_for_fields(&m.fields))
                    .unwrap_or_default();
                let (sql, boxed_params) = build_insert_sql(&table_name, &json_str, &col_types)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs).await?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(var.to_lowercase(), Value::Str(returned));
                }
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbUpdate {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let pk_fields = self.resolve_pk_fields(table);
                let key = var.to_lowercase();
                let dirty_snapshot = self.dirty_fields.get(&key).cloned();
                let col_types = self
                    .models
                    .get(&table.to_lowercase())
                    .map(|m| super::sql::column_types_for_fields(&m.fields))
                    .unwrap_or_default();
                let (sql, boxed_params) = build_update_sql(
                    &table_name,
                    &json_str,
                    &pk_fields,
                    dirty_snapshot.as_ref(),
                    &col_types,
                )?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs).await?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(key.clone(), Value::Str(returned));
                }
                // Persisted to DB → object is now clean.
                self.dirty_fields.remove(&key);
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbDelete {
                var,
                context_var: _,
                table,
            } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let pk_fields = self.resolve_pk_fields(table);
                let (sql, boxed_params) = build_delete_sql(&table_name, &json_str, &pk_fields)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let _ = engine::exec(&sql, &param_refs).await?;
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbDeleteWhere {
                context_var: _,
                table,
                where_clause,
            } => {
                let table_name = crate::sql::to_snake_case(table);
                let mut shape_bits: Vec<String> = Vec::new();
                let mut cache_bits: Vec<String> = Vec::new();
                let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
                let col_types = self.col_types_for(table);
                let where_sql = build_where_sql(
                    where_clause,
                    &mut params,
                    &mut shape_bits,
                    &mut cache_bits,
                    vars,
                    self,
                    None,
                    &col_types,
                )
                .await?;
                let sql = format!("DELETE FROM \"{}\" WHERE {};", table_name, where_sql);
                let param_refs = boxed_params_to_refs(&params);
                let _ = engine::exec(&sql, &param_refs).await?;
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            // The atomic `update CTX.Table set ...` body lives in its own
            // method and returns a `BoxFuture` (heap-allocated) so its
            // locals never inflate the `exec_stmt` future. `async_recursion`
            // sizes each parent frame by the union of all branches'
            // future state machines, so an inlined heavy branch (SQL
            // fragments, param vec) would shrink the stack budget left
            // for plain recursive user code (e.g. `fib(n)`).
            Stmt::DbUpdateSet {
                table,
                assignments,
                where_clause,
                ..
            } => {
                self.exec_db_update_set(table, assignments, where_clause, vars)
                    .await
            }
        }
    }

    fn exec_db_update_set<'b>(
        &'b mut self,
        table: &'b str,
        assignments: &'b [(String, Expr)],
        where_clause: &'b WhereExpr,
        vars: &'b mut HashMap<String, Value>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Flow>> + Send + 'b>> {
        Box::pin(self.exec_db_update_set_impl(table, assignments, where_clause, vars))
    }

    async fn exec_db_update_set_impl(
        &mut self,
        table: &str,
        assignments: &[(String, Expr)],
        where_clause: &WhereExpr,
        vars: &mut HashMap<String, Value>,
    ) -> Result<Flow> {
        let table_name = crate::sql::to_snake_case(table);
        let mut shape_bits: Vec<String> = Vec::new();
        let mut cache_bits: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
        let col_types = self.col_types_for(table);
        let mut set_fragments = Vec::with_capacity(assignments.len());
        for (col, rhs) in assignments {
            // Preserve the column's declared case — JWC columns are quoted
            // as-written (e.g. `columnId`), so lowercasing here would emit
            // `"columnid"` and fail to prepare against a camelCase column.
            let rhs_sql = build_set_rhs_sql(
                rhs,
                &mut params,
                vars,
                self,
                col_types.get(col).map(|s| s.as_str()),
            )
            .await?;
            set_fragments.push(format!("\"{}\" = {}", col, rhs_sql));
        }
        let set_clause = set_fragments.join(", ");
        let where_sql = build_where_sql(
            where_clause,
            &mut params,
            &mut shape_bits,
            &mut cache_bits,
            vars,
            self,
            None,
            &col_types,
        )
        .await?;
        let sql = format!(
            "UPDATE \"{}\" SET {} WHERE {};",
            table_name, set_clause, where_sql
        );
        let param_refs = boxed_params_to_refs(&params);
        let _ = engine::exec(&sql, &param_refs).await?;
        engine::invalidate_result_cache()?;
        Ok(Flow::Continue)
    }
}
