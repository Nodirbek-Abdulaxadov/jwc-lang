use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use postgres::types::ToSql;
use serde_json::{json, Value as JsonValue};

use crate::ast::{Expr, FunctionDecl, Program, RouteDecl, Stmt, TypedParam};
use crate::engine;

#[derive(Debug)]
pub struct RunMainResult {
    pub output: String,
    /// If `serve(port)` was called in `main()`, contains the port to listen on.
    pub serve_port: Option<u16>,
}

pub fn run_main(program: &Program) -> Result<RunMainResult> {
    let mut vm = Vm::new(program);
    let _ = vm.call_function("main", Vec::new())?;
    Ok(RunMainResult {
        output: vm.output,
        serve_port: vm.serve_requested,
    })
}

/// Dispatch a single HTTP request to the matching route and return (status_code, body).
pub fn run_request(
    program: &Program,
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<(u16, String)> {
    let mut vm = Vm::new(program);
    vm.request_body = body;
    vm.dispatch_route(method, path)
}

struct Vm<'a> {
    functions: HashMap<String, &'a FunctionDecl>,
    /// Entity schema map — used for compile-time checks and future typed queries
    #[allow(dead_code)]
    entities: HashMap<String, &'a crate::ast::EntityDecl>,
    routes: Vec<&'a RouteDecl>,
    current_path_params: Option<HashMap<String, String>>,
    output: String,
    depth: usize,
    /// Body of the current HTTP request (set by run_request)
    request_body: Option<String>,
    /// Set when `serve(port)` is called from main()
    serve_requested: Option<u16>,
}

impl<'a> Vm<'a> {
    fn new(program: &'a Program) -> Self {
        let mut functions = HashMap::new();
        for function in &program.functions {
            functions.insert(function.name.to_lowercase(), function);
        }

        let mut entities = HashMap::new();
        for entity in &program.entities {
            entities.insert(entity.name.to_lowercase(), entity);
        }

        let routes = program.routes.iter().collect();

        Self {
            functions,
            entities,
            routes,
            current_path_params: None,
            output: String::new(),
            depth: 0,
            request_body: None,
            serve_requested: None,
        }
    }

    fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Option<Value>> {
        const MAX_DEPTH: usize = 256;
        if self.depth >= MAX_DEPTH {
            bail!("Call stack depth exceeded ({MAX_DEPTH})");
        }

        let function = self
            .functions
            .get(&name.to_lowercase())
            .copied()
            .ok_or_else(|| anyhow!("Unknown function: {name}"))?;

        if function.params.len() != args.len() {
            bail!(
                "Function '{}' expects {} args but got {}",
                function.name,
                function.params.len(),
                args.len()
            );
        }

        self.depth += 1;

        let mut vars = HashMap::new();
        for (param, value) in function.params.iter().zip(args.into_iter()) {
            let value = check_param_type(param, value)?;
            vars.insert(param.name.to_lowercase(), value);
        }

        let flow = self.exec_block(&function.body, &mut vars)?;
        self.depth -= 1;

        match flow {
            Flow::Continue => Ok(None),
            Flow::Return(v) => Ok(v),
            Flow::Break => bail!("'break' used outside loop"),
            Flow::ContinueLoop => bail!("'continue' used outside loop"),
        }
    }

    fn exec_block(&mut self, stmts: &[Stmt], vars: &mut HashMap<String, Value>) -> Result<Flow> {
        for stmt in stmts {
            let flow = self.exec_stmt(stmt, vars)?;
            if !matches!(flow, Flow::Continue) {
                return Ok(flow);
            }
        }
        Ok(Flow::Continue)
    }

    fn exec_stmt(&mut self, stmt: &Stmt, vars: &mut HashMap<String, Value>) -> Result<Flow> {
        match stmt {
            Stmt::Let { name, value } => {
                let key = name.to_lowercase();
                if vars.contains_key(&key) {
                    bail!("Duplicate variable declaration: {name}");
                }
                let evaluated = self.eval_expr(value, vars)?;
                vars.insert(key, evaluated);
                Ok(Flow::Continue)
            }
            Stmt::Assign { name, value } => {
                let key = name.to_lowercase();
                if !vars.contains_key(&key) {
                    bail!("Assignment to undefined variable: {name}");
                }
                let evaluated = self.eval_expr(value, vars)?;
                vars.insert(key, evaluated);
                Ok(Flow::Continue)
            }
            Stmt::Print(expr) => {
                let value = self.eval_expr(expr, vars)?;
                self.output.push_str(&value.as_string());
                self.output.push('\n');
                Ok(Flow::Continue)
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond_value = self.eval_expr(cond, vars)?;
                match cond_value {
                    Value::Bool(true) => self.exec_block(then_body, vars),
                    Value::Bool(false) => {
                        if let Some(else_body) = else_body {
                            self.exec_block(else_body, vars)
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
                    let cond_value = self.eval_expr(cond, vars)?;
                    match cond_value {
                        Value::Bool(true) => match self.exec_block(body, vars)? {
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
                let _ = self.eval_expr(expr, vars)?;
                Ok(Flow::Continue)
            }
            Stmt::Return(None) => Ok(Flow::Return(None)),
            Stmt::Return(Some(expr)) => {
                let value = self.eval_expr(expr, vars)?;
                Ok(Flow::Return(Some(value)))
            }
            Stmt::FieldAssign { var, field, value } => {
                let key = var.to_lowercase();
                let current = vars
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| anyhow!("FieldAssign: variable '{}' not found", var))?;
                let new_val = self.eval_expr(value, vars)?;
                let json_str = match current {
                    Value::Str(s) => s,
                    Value::Null => "{}".to_string(),
                    other => bail!("FieldAssign: '{}' is not an object, got {}", var, other.type_name()),
                };
                let mut doc: serde_json::Value = serde_json::from_str(&json_str)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                if let Some(obj) = doc.as_object_mut() {
                    obj.insert(field.clone(), value_to_json(&new_val));
                }
                vars.insert(key, Value::Str(doc.to_string()));
                Ok(Flow::Continue)
            }
            Stmt::DbInsert { var, context_var: _, table } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let (sql, boxed_params) = build_insert_sql(&table_name, &json_str)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs)?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(var.to_lowercase(), Value::Str(returned));
                }
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbUpdate { var, context_var: _, table } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let (sql, boxed_params) = build_update_sql(&table_name, &json_str)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let returned = engine::query_text(&sql, &param_refs)?;
                if !returned.is_empty() && returned != "null" {
                    vars.insert(var.to_lowercase(), Value::Str(returned));
                }
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
            Stmt::DbDelete { var, context_var: _, table } => {
                let json_str = get_var_as_json(var, vars)?;
                let table_name = crate::sql::to_snake_case(table);
                let doc: serde_json::Value = serde_json::from_str(&json_str)
                    .with_context(|| "delete: value is not valid JSON")?;
                let pk_val = doc
                    .get("id")
                    .ok_or_else(|| anyhow!("delete: object must have an 'id' field"))?;
                let sql = format!("DELETE FROM \"{}\" WHERE \"id\" = $1;", table_name);
                let boxed_params = vec![json_value_to_sql_param(pk_val)];
                let param_refs = boxed_params_to_refs(&boxed_params);
                let _ = engine::exec(&sql, &param_refs)?;
                engine::invalidate_result_cache()?;
                Ok(Flow::Continue)
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr, vars: &mut HashMap<String, Value>) -> Result<Value> {
        match expr {
            Expr::Int(v) => Ok(Value::Int(*v)),
            Expr::Str(v) => Ok(Value::Str(v.clone())),
            Expr::Bool(v) => Ok(Value::Bool(*v)),
            Expr::Null => Ok(Value::Null),
            Expr::Var(name) => vars
                .get(&name.to_lowercase())
                .cloned()
                .ok_or_else(|| anyhow!("Undefined variable: {name}")),
            Expr::NewEntity { entity: _ } => Ok(Value::Str("{}".to_string())),
            Expr::FieldGet { var, field } => {
                let obj_val = vars
                    .get(&var.to_lowercase())
                    .cloned()
                    .ok_or_else(|| anyhow!("Undefined variable: {var}"))?;
                match obj_val {
                    Value::Str(s) => {
                        let doc: serde_json::Value = serde_json::from_str(&s)
                            .with_context(|| format!("FieldGet: '{}' is not valid JSON", var))?;
                        match doc.get(field.as_str()) {
                            Some(serde_json::Value::String(s)) => Ok(Value::Str(s.clone())),
                            Some(serde_json::Value::Number(n)) => {
                                Ok(Value::Int(n.as_i64().unwrap_or(0)))
                            }
                            Some(serde_json::Value::Bool(b)) => Ok(Value::Bool(*b)),
                            Some(serde_json::Value::Null) | None => Ok(Value::Null),
                            Some(v) => Ok(Value::Str(v.to_string())),
                        }
                    }
                    Value::Null => Ok(Value::Null),
                    other => bail!(
                        "FieldGet: '{}' is not a JSON object, got {}",
                        var,
                        other.type_name()
                    ),
                }
            }
            Expr::DbSelect { entity: _, context_var: _, table, where_clause, first } => {
                let table_name = crate::sql::to_snake_case(table);
                let (sql, boxed_params, shape_key, cache_key) =
                    build_select_sql(table_name, where_clause.as_deref(), *first, vars, self)?;
                let param_refs = boxed_params_to_refs(&boxed_params);
                let compiled_sql = engine::get_or_compile_sql(&shape_key, || Ok(sql.clone()))?;
                let result =
                    engine::query_text_with_optional_cache(&cache_key, &compiled_sql, &param_refs)?;
                if result == "null" || result.is_empty() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Str(result))
                }
            }
            Expr::Call { name, args } => {
                if name.eq_ignore_ascii_case("dispatch") {
                    return self.eval_dispatch_call(args, vars);
                }

                if name.eq_ignore_ascii_case("path_param") {
                    return self.eval_path_param_call(args, vars);
                }

                if name.eq_ignore_ascii_case("db_query") {
                    return self.eval_db_query_call(args, vars);
                }

                if name.eq_ignore_ascii_case("request_body") {
                    return self.eval_request_body_call(args, vars);
                }

                if name.eq_ignore_ascii_case("uuid") {
                    return self.eval_uuid_call(args, vars);
                }

                if name.eq_ignore_ascii_case("set_json_field") {
                    return self.eval_set_json_field_call(args, vars);
                }

                if name.eq_ignore_ascii_case("body") {
                    return self.eval_request_body_call(args, vars);
                }

                // ── `serve(port?)` — starts HTTP server from main() ───────
                if name.eq_ignore_ascii_case("serve") {
                    let port: u16 = if let Some(arg) = args.first() {
                        match self.eval_expr(arg, vars)? {
                            Value::Int(n) if n > 0 && n <= 65535 => n as u16,
                            Value::Int(n) => bail!("serve(): invalid port {n}"),
                            other => bail!(
                                "serve(port): port must be int, got {}",
                                other.type_name()
                            ),
                        }
                    } else {
                        8080
                    };
                    self.serve_requested = Some(port);
                    return Ok(Value::Void);
                }

                // ── `env("VAR_NAME")` — read an environment variable ──────
                if name.eq_ignore_ascii_case("env") {
                    if args.len() != 1 {
                        bail!("env(name) expects exactly 1 arg");
                    }
                    let var_name = self.eval_expr(&args[0], vars)?;
                    let var_name = match var_name {
                        Value::Str(s) => s,
                        other => bail!("env(name): name must be string, got {}", other.type_name()),
                    };
                    let val = std::env::var(&var_name).unwrap_or_default();
                    return Ok(Value::Str(val));
                }

                // ── `setConnectionString(url)` — set DATABASE_URL for this process ──
                if name.eq_ignore_ascii_case("setConnectionString")
                    || name.eq_ignore_ascii_case("set_connection_string")
                {
                    if args.len() != 1 {
                        bail!("setConnectionString(url) expects exactly 1 arg");
                    }
                    let url = self.eval_expr(&args[0], vars)?;
                    let url = match url {
                        Value::Str(s) => s,
                        other => bail!(
                            "setConnectionString(url): url must be string, got {}",
                            other.type_name()
                        ),
                    };
                    // SAFETY: only called from single-threaded main startup
                    std::env::set_var("DATABASE_URL", &url);
                    return Ok(Value::Void);
                }

                // ── HTTP response helpers ──────────────────────────────────
                if name.eq_ignore_ascii_case("json") {
                    if args.len() != 1 {
                        bail!("json(val) expects exactly 1 arg");
                    }
                    let val = self.eval_expr(&args[0], vars)?;
                    return Ok(Value::Str(val.as_string()));
                }

                if name.eq_ignore_ascii_case("created") {
                    if args.len() != 1 {
                        bail!("created(val) expects exactly 1 arg");
                    }
                    let val = self.eval_expr(&args[0], vars)?;
                    let s = val.as_string();
                    let result = if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&s) {
                        match doc.as_object_mut() {
                            Some(obj) => {
                                obj.insert("status".into(), json!(201));
                                doc.to_string()
                            }
                            None => format!(r#"{{"status":201,"data":{s}}}"#),
                        }
                    } else {
                        format!(r#"{{"status":201,"data":{s:?}}}"#)
                    };
                    return Ok(Value::Str(result));
                }

                if name.eq_ignore_ascii_case("notFound") {
                    return Ok(Value::Str(
                        r#"{"status":404,"error":"Not Found"}"#.to_string(),
                    ));
                }

                if name.eq_ignore_ascii_case("noContent") {
                    return Ok(Value::Str(r#"{"status":204}"#.to_string()));
                }

                if name.eq_ignore_ascii_case("internalError") {
                    let msg = if let Some(arg) = args.first() {
                        self.eval_expr(arg, vars)?.as_string()
                    } else {
                        "Internal Server Error".to_string()
                    };
                    let escaped = msg.replace('"', "\\\"");
                    return Ok(Value::Str(format!(
                        r#"{{"status":500,"error":"{escaped}"}}"#
                    )));
                }
                // ──────────────────────────────────────────────────────────

                if name.eq_ignore_ascii_case("db_insert_todo") {
                    return self.eval_db_insert_todo_call(args, vars);
                }

                if name.eq_ignore_ascii_case("db_select_todo") {
                    return self.eval_db_select_todo_call(args, vars);
                }

                if name.eq_ignore_ascii_case("db_update_todo") {
                    return self.eval_db_update_todo_call(args, vars);
                }

                if name.eq_ignore_ascii_case("db_delete_todo") {
                    return self.eval_db_delete_todo_call(args, vars);
                }

                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval_expr(arg, vars)?);
                }
                Ok(self.call_function(name, values)?.unwrap_or(Value::Void))
            }
            Expr::Add(left, right) => {
                let left = self.eval_expr(left, vars)?;
                let right = self.eval_expr(right, vars)?;
                match (left, right) {
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                    (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
                    (Value::Str(a), b) => Ok(Value::Str(format!("{a}{}", b.as_string()))),
                    (a, Value::Str(b)) => Ok(Value::Str(format!("{}{b}", a.as_string()))),
                    (a, b) => bail!("Unsupported '+' for {} and {}", a.type_name(), b.type_name()),
                }
            }
            Expr::Sub(left, right) => self.eval_int_bin(left, right, vars, |a, b| a - b),
            Expr::Mul(left, right) => self.eval_int_bin(left, right, vars, |a, b| a * b),
            Expr::Div(left, right) => {
                let l = self.eval_expr(left, vars)?;
                let r = self.eval_expr(right, vars)?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("division by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
                    (a, b) => bail!("Unsupported '/' for {} and {}", a.type_name(), b.type_name()),
                }
            }
            Expr::Mod(left, right) => {
                let l = self.eval_expr(left, vars)?;
                let r = self.eval_expr(right, vars)?;
                match (l, r) {
                    (Value::Int(_), Value::Int(0)) => bail!("modulo by zero"),
                    (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
                    (a, b) => bail!("Unsupported '%' for {} and {}", a.type_name(), b.type_name()),
                }
            }
            Expr::Neg(inner) => {
                let value = self.eval_expr(inner, vars)?;
                match value {
                    Value::Int(v) => Ok(Value::Int(-v)),
                    other => bail!("Unsupported unary '-' for {}", other.type_name()),
                }
            }
            Expr::Eq(left, right) => {
                let l = self.eval_expr(left, vars)?;
                let r = self.eval_expr(right, vars)?;
                Ok(Value::Bool(l == r))
            }
            Expr::Neq(left, right) => {
                let l = self.eval_expr(left, vars)?;
                let r = self.eval_expr(right, vars)?;
                Ok(Value::Bool(l != r))
            }
            Expr::Lt(left, right) => self.eval_int_cmp(left, right, vars, |a, b| a < b),
            Expr::Lte(left, right) => self.eval_int_cmp(left, right, vars, |a, b| a <= b),
            Expr::Gt(left, right) => self.eval_int_cmp(left, right, vars, |a, b| a > b),
            Expr::Gte(left, right) => self.eval_int_cmp(left, right, vars, |a, b| a >= b),
            Expr::And(left, right) => {
                let l = self.eval_expr(left, vars)?;
                match l {
                    Value::Bool(false) => Ok(Value::Bool(false)),
                    Value::Bool(true) => {
                        let r = self.eval_expr(right, vars)?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'and' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'and' expects bool, got {}", other.type_name()),
                }
            }
            Expr::Or(left, right) => {
                let l = self.eval_expr(left, vars)?;
                match l {
                    Value::Bool(true) => Ok(Value::Bool(true)),
                    Value::Bool(false) => {
                        let r = self.eval_expr(right, vars)?;
                        match r {
                            Value::Bool(v) => Ok(Value::Bool(v)),
                            other => bail!("'or' expects bool, got {}", other.type_name()),
                        }
                    }
                    other => bail!("'or' expects bool, got {}", other.type_name()),
                }
            }
        }
    }

    fn eval_int_bin<F>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        func: F,
    ) -> Result<Value>
    where
        F: FnOnce(i64, i64) -> i64,
    {
        let l = self.eval_expr(left, vars)?;
        let r = self.eval_expr(right, vars)?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(func(a, b))),
            (a, b) => bail!("Unsupported numeric op for {} and {}", a.type_name(), b.type_name()),
        }
    }

    fn eval_int_cmp<F>(
        &mut self,
        left: &Expr,
        right: &Expr,
        vars: &mut HashMap<String, Value>,
        func: F,
    ) -> Result<Value>
    where
        F: FnOnce(i64, i64) -> bool,
    {
        let l = self.eval_expr(left, vars)?;
        let r = self.eval_expr(right, vars)?;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(func(a, b))),
            (a, b) => bail!("Unsupported comparison for {} and {}", a.type_name(), b.type_name()),
        }
    }

    /// Dispatch a single HTTP request directly (used by the real HTTP server).
    /// Returns (http_status_code, response_body).
    pub fn dispatch_route(&mut self, method: &str, path: &str) -> Result<(u16, String)> {
        // Find matching route index and collect params (avoid holding borrow across mut calls)
        let mut found_idx: Option<usize> = None;
        let mut found_params: HashMap<String, String> = HashMap::new();

        for (i, route) in self.routes.iter().enumerate() {
            if !route.method.eq_ignore_ascii_case(method) {
                continue;
            }
            if let Some(params) = match_route_pattern(&route.path, path) {
                found_idx = Some(i);
                found_params = params;
                break;
            }
        }

        let Some(idx) = found_idx else {
            return Ok((
                404,
                format!(
                    "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{method}\",\"path\":\"{path}\"}}"
                ),
            ));
        };

        // Clone what we need so we can mutably borrow self below
        let handler: Option<String> = self.routes[idx].handler.clone();
        let body_stmts: Vec<Stmt> = self.routes[idx].body.clone();

        let previous = self.current_path_params.take();
        self.current_path_params = Some(found_params);

        let response_str = if let Some(ref handler_name) = handler {
            self.call_function(handler_name, Vec::new())?
                .map(|v| v.as_string())
        } else {
            let mut route_vars = HashMap::new();
            let flow = self.exec_block(&body_stmts, &mut route_vars)?;
            match flow {
                Flow::Return(Some(v)) => Some(v.as_string()),
                Flow::Return(None) => Some("null".to_string()),
                _ => {
                    if !self.output.is_empty() {
                        let out = self.output.trim_end_matches('\n').to_string();
                        self.output.clear();
                        Some(out)
                    } else {
                        None
                    }
                }
            }
        };

        self.current_path_params = previous;

        let body = response_str.unwrap_or_else(|| "null".to_string());

        // Derive HTTP status from a "status" field in JSON, default 200.
        // Then strip the internal "status" field before sending to client.
        let (status, clean_body) =
            if let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&body) {
                let code = doc
                    .get("status")
                    .and_then(|s| s.as_u64())
                    .and_then(|s| u16::try_from(s).ok())
                    .filter(|s| *s >= 100 && *s < 600)
                    .unwrap_or(200);
                // Strip the "status" key from the response body
                if let Some(obj) = doc.as_object_mut() {
                    obj.remove("status");
                }
                let body_out = if code == 204 {
                    String::new()
                } else {
                    doc.to_string()
                };
                (code, body_out)
            } else {
                (200, body)
            };

        Ok((status, clean_body))
    }

    fn eval_dispatch_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("dispatch(method, path) expects exactly 2 args");
        }

        let method = self.eval_expr(&args[0], vars)?;
        let path = self.eval_expr(&args[1], vars)?;

        let method = match method {
            Value::Str(v) => v.to_ascii_uppercase(),
            other => bail!(
                "dispatch(method, path): method must be string, got {}",
                other.type_name()
            ),
        };

        let path = match path {
            Value::Str(v) => v,
            other => bail!(
                "dispatch(method, path): path must be string, got {}",
                other.type_name()
            ),
        };

        for route in &self.routes {
            if !method.eq_ignore_ascii_case(&route.method) {
                continue;
            }

            let Some(params) = match_route_pattern(&route.path, &path) else {
                continue;
            };

            let previous = self.current_path_params.take();
            self.current_path_params = Some(params);

            if let Some(handler) = &route.handler {
                let result = self.call_function(handler, Vec::new())?;
                if let Some(value) = result {
                    self.output.push_str(&value.as_string());
                    self.output.push('\n');
                }
            } else {
                let mut route_vars = HashMap::new();
                let flow = self.exec_block(&route.body, &mut route_vars)?;
                match flow {
                    Flow::Break | Flow::ContinueLoop => {
                        self.current_path_params = previous;
                        bail!("break/continue cannot be used at route top-level");
                    }
                    Flow::Return(Some(value)) => {
                        self.output.push_str(&value.as_string());
                        self.output.push('\n');
                    }
                    Flow::Return(None) => {
                        self.output.push_str("null\n");
                    }
                    Flow::Continue => {}
                }
            }

            self.current_path_params = previous;
            return Ok(Value::Bool(true));
        }

        self.output.push_str(&format!(
            "{{\"status\":404,\"error\":\"Not Found\",\"method\":\"{}\",\"path\":\"{}\"}}\n",
            method, path
        ));
        Ok(Value::Bool(false))
    }

    fn eval_path_param_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("path_param(name) expects exactly 1 arg");
        }

        let name = self.eval_expr(&args[0], vars)?;
        let name = match name {
            Value::Str(v) => v,
            other => bail!("path_param(name): name must be string, got {}", other.type_name()),
        };

        let params = self
            .current_path_params
            .as_ref()
            .ok_or_else(|| anyhow!("path_param() can only be used inside route execution"))?;

        match params.get(&name) {
            Some(v) => Ok(Value::Str(v.clone())),
            None => Ok(Value::Null),
        }
    }

    fn eval_db_query_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_query(sql) expects exactly 1 arg");
        }

        let sql = self.eval_expr(&args[0], vars)?;
        let sql = match sql {
            Value::Str(v) => v,
            other => bail!("db_query(sql): sql must be string, got {}", other.type_name()),
        };

        let database_url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("JWC_DATABASE_URL"))
            .map_err(|_| anyhow!("DATABASE_URL (or JWC_DATABASE_URL) is required for db_query"))?;

        engine::init_engine(&database_url)?;
        let value = engine::query_text(&sql, &[])?;
        if value.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(value))
        }
    }

    fn eval_request_body_call(
        &mut self,
        args: &[Expr],
        _vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if !args.is_empty() {
            bail!("request_body() expects no args");
        }

        // Prefer the body injected by run_request(), fall back to env var for legacy use
        let body = self
            .request_body
            .clone()
            .or_else(|| std::env::var("JWC_REQUEST_BODY").ok())
            .unwrap_or_else(|| "null".to_string());
        Ok(Value::Str(body))
    }

    fn eval_uuid_call(&mut self, args: &[Expr], _vars: &mut HashMap<String, Value>) -> Result<Value> {
        if !args.is_empty() {
            bail!("uuid() expects no args");
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| anyhow!("System clock error"))?
            .as_nanos();
        let hex = format!("{:032x}", nanos);
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        );
        Ok(Value::Str(uuid))
    }

    fn eval_set_json_field_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 3 {
            bail!("set_json_field(obj_json, field, value) expects 3 args");
        }

        let source = match self.eval_expr(&args[0], vars)? {
            Value::Str(v) => v,
            other => bail!("set_json_field: first arg must be string json, got {}", other.type_name()),
        };
        let field = match self.eval_expr(&args[1], vars)? {
            Value::Str(v) => v,
            other => bail!("set_json_field: field must be string, got {}", other.type_name()),
        };
        let value = self.eval_expr(&args[2], vars)?;

        let mut json_value: JsonValue = serde_json::from_str(&source)
            .with_context(|| "set_json_field: invalid json in first arg")?;
        let object = json_value
            .as_object_mut()
            .ok_or_else(|| anyhow!("set_json_field: first arg must be a json object"))?;

        object.insert(field, value_to_json(&value));

        Ok(Value::Str(json_value.to_string()))
    }

    fn eval_db_insert_todo_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_insert_todo(todo_json) expects 1 arg");
        }

        let raw = match self.eval_expr(&args[0], vars)? {
            Value::Str(v) => v,
            other => bail!("db_insert_todo: arg must be string json, got {}", other.type_name()),
        };
        let doc: JsonValue = serde_json::from_str(&raw).with_context(|| "db_insert_todo: invalid json")?;

        let id = json_get_string(&doc, "id")?;
        let title = json_get_string(&doc, "title")?;
        let description = json_get_opt_string(&doc, "description");
        let completed = doc.get("completed").and_then(|v| v.as_bool()).unwrap_or(false);
        let due_date = json_get_opt_string(&doc, "due_date");

        let sql = "INSERT INTO todo_entity (id, title, description, completed, due_date) VALUES ($1::uuid, $2, $3, $4, $5::timestamptz) ON CONFLICT (id) DO UPDATE SET title = EXCLUDED.title, description = EXCLUDED.description, completed = EXCLUDED.completed, due_date = EXCLUDED.due_date;";
        let boxed_params: Vec<Box<dyn ToSql + Sync>> = vec![
            Box::new(id),
            Box::new(title),
            Box::new(description),
            Box::new(completed),
            Box::new(due_date),
        ];
        let param_refs = boxed_params_to_refs(&boxed_params);

        let _ = engine::exec(sql, &param_refs)?;
        engine::invalidate_result_cache()?;
        Ok(Value::Null)
    }

    fn eval_db_select_todo_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_select_todo(id) expects 1 arg");
        }

        let id = match self.eval_expr(&args[0], vars)? {
            Value::Str(v) => v,
            other => bail!("db_select_todo: id must be string, got {}", other.type_name()),
        };

        let sql = "SELECT COALESCE((SELECT json_build_object('id', id::text, 'title', title, 'description', description, 'completed', completed, 'due_date', due_date)::text FROM todo_entity WHERE id::text = $1 LIMIT 1), 'null');";
        let boxed_params: Vec<Box<dyn ToSql + Sync>> = vec![Box::new(id)];
        let param_refs = boxed_params_to_refs(&boxed_params);
        let out = engine::query_text_with_optional_cache(
            "todo.select.by_id",
            sql,
            &param_refs,
        )?;
        if out == "null" || out.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(Value::Str(out))
        }
    }

    fn eval_db_update_todo_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 2 {
            bail!("db_update_todo(id, todo_json) expects 2 args");
        }

        let id = match self.eval_expr(&args[0], vars)? {
            Value::Str(v) => v,
            other => bail!("db_update_todo: id must be string, got {}", other.type_name()),
        };
        let raw = match self.eval_expr(&args[1], vars)? {
            Value::Str(v) => v,
            other => bail!("db_update_todo: todo must be string json, got {}", other.type_name()),
        };
        let doc: JsonValue = serde_json::from_str(&raw).with_context(|| "db_update_todo: invalid json")?;

        let title = json_get_string(&doc, "title")?;
        let description = json_get_opt_string(&doc, "description");
        let completed = doc.get("completed").and_then(|v| v.as_bool()).unwrap_or(false);
        let due_date = json_get_opt_string(&doc, "due_date");

        let sql = "UPDATE todo_entity SET title = $1, description = $2, completed = $3, due_date = $4::timestamptz WHERE id::text = $5;";
        let boxed_params: Vec<Box<dyn ToSql + Sync>> = vec![
            Box::new(title),
            Box::new(description),
            Box::new(completed),
            Box::new(due_date),
            Box::new(id),
        ];
        let param_refs = boxed_params_to_refs(&boxed_params);
        let _ = engine::exec(sql, &param_refs)?;
        engine::invalidate_result_cache()?;
        Ok(Value::Null)
    }

    fn eval_db_delete_todo_call(
        &mut self,
        args: &[Expr],
        vars: &mut HashMap<String, Value>,
    ) -> Result<Value> {
        if args.len() != 1 {
            bail!("db_delete_todo(id) expects 1 arg");
        }

        let id = match self.eval_expr(&args[0], vars)? {
            Value::Str(v) => v,
            other => bail!("db_delete_todo: id must be string, got {}", other.type_name()),
        };

        let sql = "DELETE FROM todo_entity WHERE id::text = $1;";
        let boxed_params: Vec<Box<dyn ToSql + Sync>> = vec![Box::new(id)];
        let param_refs = boxed_params_to_refs(&boxed_params);
        let _ = engine::exec(sql, &param_refs)?;
        engine::invalidate_result_cache()?;
        Ok(Value::Null)
    }
}

/// Extract the column name from a field path like `"Entity.field"` → `"field"`
fn field_path_to_col(path: &str) -> String {
    if let Some(pos) = path.rfind('.') {
        path[pos + 1..].to_string()
    } else {
        path.to_string()
    }
}

/// Normalize JWC comparison operators to SQL operators
fn normalize_sql_op(op: &str) -> &str {
    match op {
        "==" | "=" => "=",
        "!=" => "!=",
        "<" => "<",
        "<=" => "<=",
        ">" => ">",
        ">=" => ">=",
        _ => "=",
    }
}

/// Get a variable value as a JSON string, or error
fn get_var_as_json(var: &str, vars: &HashMap<String, Value>) -> Result<String> {
    match vars.get(&var.to_lowercase()).cloned() {
        Some(Value::Str(s)) => Ok(s),
        Some(other) => bail!("'{}' must be a JSON object, got {}", var, other.type_name()),
        None => bail!("variable '{}' not found", var),
    }
}

/// Build `INSERT INTO "table" (...) VALUES (...) RETURNING *` from a JSON object string
/// Uses a CTE so all columns (including SERIAL id) are returned.
fn build_insert_sql(table: &str, json_str: &str) -> Result<(String, Vec<Box<dyn ToSql + Sync>>)> {
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
    let params: Vec<Box<dyn ToSql + Sync>> =
        filtered.iter().map(|(_, v)| json_value_to_sql_param(v)).collect();
    Ok((format!(
        "WITH _ins AS (INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING *) SELECT row_to_json(t)::text FROM _ins t;",
        table,
        fields.join(", "),
        placeholders.join(", "),
    ), params))
}

/// Build `UPDATE "table" SET ... WHERE "id" = ... RETURNING *;` from a JSON object string
fn build_update_sql(table: &str, json_str: &str) -> Result<(String, Vec<Box<dyn ToSql + Sync>>)> {
    let doc: serde_json::Value =
        serde_json::from_str(json_str).with_context(|| "update: value is not valid JSON")?;
    let obj = doc
        .as_object()
        .ok_or_else(|| anyhow!("update: value must be a JSON object"))?;

    let pk_val = obj
        .get("id")
        .ok_or_else(|| anyhow!("update: object must have an 'id' field for the WHERE clause"))?;

    let mut updates: Vec<(&String, &serde_json::Value)> = obj
        .iter()
        .filter(|(k, _)| *k != "id")
        .collect();

    updates.sort_by(|a, b| a.0.cmp(b.0));

    let sets: Vec<String> = updates
        .iter()
        .enumerate()
        .map(|(idx, (k, _))| format!("\"{}\" = ${}", k, idx + 1))
        .collect();

    if sets.is_empty() {
        bail!("update: no fields to update (only 'id' present in object)");
    }

    let mut params: Vec<Box<dyn ToSql + Sync>> = updates
        .iter()
        .map(|(_, v)| json_value_to_sql_param(v))
        .collect();
    params.push(json_value_to_sql_param(pk_val));

    Ok((format!(
        "WITH _upd AS (UPDATE \"{}\" SET {} WHERE \"id\" = {} RETURNING *) SELECT row_to_json(t)::text FROM _upd t;",
        table,
        sets.join(", "),
        format!("${}", params.len()),
    ), params))
}

fn json_value_to_sql_param(val: &serde_json::Value) -> Box<dyn ToSql + Sync> {
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
        serde_json::Value::String(s) => Box::new(s.clone()),
        other => Box::new(other.to_string()),
    }
}

fn boxed_params_to_refs(params: &[Box<dyn ToSql + Sync>]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect()
}

fn value_to_sql_param(val: &Value) -> Box<dyn ToSql + Sync> {
    match val {
        Value::Int(n) => {
            if (i32::MIN as i64..=i32::MAX as i64).contains(n) {
                Box::new(*n as i32)
            } else {
                Box::new(*n)
            }
        }
        Value::Str(s) => Box::new(s.clone()),
        Value::Bool(b) => Box::new(*b),
        Value::Null | Value::Void => Box::new(Option::<String>::None),
    }
}

fn value_to_cache_fragment(val: &Value) -> String {
    match val {
        Value::Int(n) => format!("int:{n}"),
        Value::Str(s) => format!("str:{s}"),
        Value::Bool(b) => format!("bool:{b}"),
        Value::Null => "null".to_string(),
        Value::Void => "void".to_string(),
    }
}

fn build_select_sql(
    table_name: String,
    where_clause: Option<&crate::ast::DbWhere>,
    first: bool,
    vars: &mut HashMap<String, Value>,
    vm: &mut Vm,
) -> Result<(String, Vec<Box<dyn ToSql + Sync>>, String, String)> {
    let mut sql_where = String::new();
    let mut shape_bits = String::new();
    let mut cache_bits = String::new();
    let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::new();

    if let Some(wc) = where_clause {
        let col = field_path_to_col(&wc.field);
        let op = normalize_sql_op(&wc.op);
        let rhs_val = vm.eval_expr(&wc.rhs, vars)?;

        match rhs_val {
            Value::Null | Value::Void => {
                if op == "!=" {
                    sql_where = format!(" WHERE \"{}\" IS NOT NULL", col);
                    shape_bits = format!("where:{col}:is_not_null");
                    cache_bits = shape_bits.clone();
                } else {
                    sql_where = format!(" WHERE \"{}\" IS NULL", col);
                    shape_bits = format!("where:{col}:is_null");
                    cache_bits = shape_bits.clone();
                }
            }
            other => {
                sql_where = format!(" WHERE \"{}\" {} $1", col, op);
                shape_bits = format!("where:{col}:{op}:param");
                cache_bits = format!("{shape_bits}:{}", value_to_cache_fragment(&other));
                params.push(value_to_sql_param(&other));
            }
        }
    }

    let sql = if first {
        format!(
            "SELECT row_to_json(t)::text FROM (SELECT * FROM \"{}\"{} LIMIT 1) t;",
            table_name, sql_where
        )
    } else {
        format!(
            "SELECT COALESCE(json_agg(row_to_json(t)), '[]')::text FROM (SELECT * FROM \"{}\"{}) t;",
            table_name, sql_where
        )
    };

    let shape_key = format!(
        "select|table:{table_name}|first:{first}|{}",
        if shape_bits.is_empty() {
            "no_where".to_string()
        } else {
            shape_bits
        }
    );
    let cache_key = format!(
        "result|table:{table_name}|first:{first}|{}",
        if cache_bits.is_empty() {
            "no_where".to_string()
        } else {
            cache_bits
        }
    );

    Ok((sql, params, shape_key, cache_key))
}

fn value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Int(v) => json!(v),
        Value::Str(v) => json!(v),
        Value::Bool(v) => json!(v),
        Value::Null | Value::Void => JsonValue::Null,
    }
}

fn json_get_string(doc: &JsonValue, field: &str) -> Result<String> {
    doc.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("Missing or invalid string field: {field}"))
}

fn json_get_opt_string(doc: &JsonValue, field: &str) -> Option<String> {
    doc.get(field).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn match_route_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_segments: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let path_segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if pattern_segments.len() != path_segments.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (p, v) in pattern_segments.iter().zip(path_segments.iter()) {
        if p.starts_with('{') && p.ends_with('}') && p.len() > 2 {
            let key = p.trim_start_matches('{').trim_end_matches('}').to_string();
            params.insert(key, (*v).to_string());
            continue;
        }

        if p != v {
            return None;
        }
    }

    Some(params)
}

enum Flow {
    Continue,
    Return(Option<Value>),
    Break,
    ContinueLoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Int(i64),
    Str(String),
    Bool(bool),
    Null,
    Void,
}

impl Value {
    fn as_string(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Str(v) => v.clone(),
            Value::Bool(v) => v.to_string(),
            Value::Null => "null".to_string(),
            Value::Void => String::new(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Str(_) => "string",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Void => "void",
        }
    }
}

/// Runtime type guard for typed function parameters.
/// If the param has no type annotation, the value passes through unchanged.
/// For known primitive types a coercion is attempted before raising an error.
fn check_param_type(param: &TypedParam, value: Value) -> Result<Value> {
    let ty = match &param.ty {
        None => return Ok(value),
        Some(t) => t.as_str(),
    };

    match ty {
        "string" | "str" => match &value {
            Value::Str(_) => Ok(value),
            Value::Int(n) => Ok(Value::Str(n.to_string())),
            _ => bail!(
                "Type error: parameter '{}' expects string, got {}",
                param.name,
                value.type_name()
            ),
        },
        "int" | "integer" | "number" => match &value {
            Value::Int(_) => Ok(value),
            Value::Str(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| anyhow!(
                    "Type error: parameter '{}' expects int, got string \"{}\"",
                    param.name, s
                )),
            _ => bail!(
                "Type error: parameter '{}' expects int, got {}",
                param.name,
                value.type_name()
            ),
        },
        "bool" | "boolean" => match &value {
            Value::Bool(_) => Ok(value),
            _ => bail!(
                "Type error: parameter '{}' expects bool, got {}",
                param.name,
                value.type_name()
            ),
        },
        // Entity types and unknown types — pass through at runtime (checked by validator later)
        _ => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_program, validate_program};

    #[test]
    fn runs_main_and_prints_output() {
        let src = r#"
            function main() {
                let name = "JWC";
                print("Hello " + name);
                print(1 + 2 * 3);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "Hello JWC\n7\n");
    }

    #[test]
    fn supports_function_call_and_return() {
        let src = r#"
            function add(a, b) {
                return a + b;
            }

            function main() {
                let x = add(20, 22);
                print(x);
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "42\n");
    }

    #[test]
    fn supports_if_while_break_continue() {
        let src = r#"
            function main() {
                let i = 0;
                while (i < 6) {
                    i = i + 1;
                    if (i == 2) {
                        continue;
                    }
                    if (i == 5) {
                        break;
                    }
                    print(i);
                }
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "1\n3\n4\n");
    }

    #[test]
    fn supports_logical_ops() {
        let src = r#"
            function main() {
                if (true and (1 < 2) or false) {
                    print("ok");
                } else {
                    print("bad");
                }
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "ok\n");
    }

    #[test]
    fn supports_declarative_routes_with_dispatch() {
        let src = r#"
            route GET "/health" {
                print("GET /health -> 200 OK");
            }

            function main() {
                dispatch("GET", "/health");
                dispatch("GET", "/unknown");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(
            out.output,
            "GET /health -> 200 OK\n{\"status\":404,\"error\":\"Not Found\",\"method\":\"GET\",\"path\":\"/unknown\"}\n"
        );
    }

    #[test]
    fn supports_route_path_params() {
        let src = r#"
            route GET "/todos/{id}" {
                let id = path_param("id");
                print("todo=" + id);
            }

            function main() {
                dispatch("GET", "/todos/42");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "todo=42\n");
    }

    #[test]
    fn dispatch_outputs_json_from_route_return() {
        let src = r#"
            route GET "/todos" {
                return "{\"items\":[]}";
            }

            function main() {
                dispatch("GET", "/todos");
            }
        "#;

        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "{\"items\":[]}\n");
    }

    #[test]
    fn supports_new_entity_and_field_ops() {
        let src = r#"
            function main() {
                let car = new CarEntity();
                car.model = "Tesla";
                car.year = 2024;
                let m = car.model;
                let y = car.year;
                print(m);
                print(y);
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out.output, "Tesla\n2024\n");
    }

    #[test]
    fn run_request_dispatches_route() {
        let src = r#"
            route GET "/ping" {
                return "{\"ok\":true}";
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, body) = run_request(&program, "GET", "/ping", None).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
    }

    #[test]
    fn run_request_returns_404_for_unknown_route() {
        let src = r#"
            route GET "/ping" {
                return "pong";
            }
        "#;
        let program = parse_program(src).unwrap();
        validate_program(&program).unwrap();
        let (status, _body) = run_request(&program, "GET", "/missing", None).unwrap();
        assert_eq!(status, 404);
    }
}
