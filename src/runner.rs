use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use crate::ast::{ClassDecl, ClassMember, EntityDecl, FunctionDecl, Literal, Program, Stmt, TypeSpec, ValueExpr};

pub struct RunResult {
    pub output: String,
    pub return_value: Option<Literal>,
}

#[derive(Debug, Clone)]
struct ObjectInstance {
    class_name: String,
    fields: HashMap<String, Literal>,
}

struct Vm<'a> {
    fns: HashMap<String, &'a FunctionDecl>,
    classes: HashMap<String, &'a ClassDecl>,
    entities: HashMap<String, &'a EntityDecl>,
    heap: HashMap<u64, ObjectInstance>,
    next_obj_id: u64,
    db_url: Option<String>,
}

impl<'a> Vm<'a> {
    fn new(program: &'a Program, db_url: Option<String>) -> Self {
        let mut fns: HashMap<String, &'a FunctionDecl> = HashMap::new();
        for f in &program.functions {
            fns.insert(f.name.to_lowercase(), f);
        }

        let mut classes: HashMap<String, &'a ClassDecl> = HashMap::new();
        for c in &program.classes {
            classes.insert(c.name.to_lowercase(), c);
        }
        for c in &program.controllers {
            classes.insert(c.name.to_lowercase(), c);
        }

        let mut entities: HashMap<String, &'a EntityDecl> = HashMap::new();
        for e in &program.entities {
            entities.insert(e.name.to_lowercase(), e);
        }

        Self {
            fns,
            classes,
            entities,
            heap: HashMap::new(),
            next_obj_id: 1,
            db_url,
        }
    }

    fn alloc_object(&mut self, obj: ObjectInstance) -> u64 {
        let id = self.next_obj_id;
        self.next_obj_id += 1;
        self.heap.insert(id, obj);
        id
    }

    fn get_object(&self, id: u64) -> Result<&ObjectInstance> {
        self.heap
            .get(&id)
            .ok_or_else(|| anyhow!("Invalid object reference: {id}"))
    }

    fn get_object_mut(&mut self, id: u64) -> Result<&mut ObjectInstance> {
        self.heap
            .get_mut(&id)
            .ok_or_else(|| anyhow!("Invalid object reference: {id}"))
    }
}

pub struct Runtime<'a> {
    vm: Vm<'a>,
}

impl<'a> Runtime<'a> {
    pub fn new(program: &'a Program) -> Self {
        Self {
            vm: Vm::new(program, None),
        }
    }

    pub fn new_with_db_url(program: &'a Program, db_url: Option<String>) -> Self {
        Self {
            vm: Vm::new(program, db_url),
        }
    }

    pub fn object_class_name(&self, id: u64) -> Result<String> {
        Ok(self.vm.get_object(id)?.class_name.clone())
    }

    pub fn create_instance(&mut self, class_name: &str) -> Result<u64> {
        let class = self
            .vm
            .classes
            .get(&class_name.to_lowercase())
            .copied()
            .ok_or_else(|| anyhow!("Unknown class/controller: {class_name}"))?;

        let mut fields: HashMap<String, Literal> = HashMap::new();
        let mut vars: HashMap<String, Literal> = HashMap::new();
        let mut out = String::new();
        for m in &class.members {
            if let ClassMember::Field(f) = m {
                let v = if let Some(init) = &f.init {
                    eval_value_expr(&mut self.vm, init, &mut vars, &mut out, 0, None)?
                } else {
                    Literal::Int(0)
                };
                fields.insert(f.name.to_lowercase(), v);
            }
        }

        let id = self.vm.alloc_object(ObjectInstance {
            class_name: class.name.clone(),
            fields,
        });
        Ok(id)
    }

    pub fn run_function(&mut self, name: &str, args: Vec<Literal>) -> Result<RunResult> {
        let f = self
            .vm
            .fns
            .get(&name.to_lowercase())
            .copied()
            .ok_or_else(|| anyhow!("Unknown function: {name}"))?;

        if f.params.len() != args.len() {
            bail!(
                "Function '{}' expects {} args but got {}",
                f.name,
                f.params.len(),
                args.len()
            );
        }

        let mut frame: HashMap<String, Literal> = HashMap::new();
        for (param, arg) in f.params.iter().zip(args.into_iter()) {
            frame.insert(param.to_lowercase(), arg);
        }

        let mut out = String::new();
        let flow = exec_block(&mut self.vm, &f.body, &mut frame, &mut out, 0, None)?;
        let ret = match flow {
            Flow::Continue => None,
            Flow::Return(v) => v,
            Flow::Break => bail!("Runtime error: 'break' used outside of a loop"),
            Flow::ContinueLoop => bail!("Runtime error: 'continue' used outside of a loop"),
        };

        Ok(RunResult {
            output: out,
            return_value: ret,
        })
    }

    pub fn run_route(
        &mut self,
        body: &[Stmt],
        locals: Vec<(String, Literal)>,
        this_obj: Option<u64>,
    ) -> Result<RunResult> {
        let mut frame: HashMap<String, Literal> = HashMap::new();
        for (name, value) in locals {
            frame.insert(name.to_lowercase(), value);
        }

        let mut out = String::new();
        let flow = exec_block(&mut self.vm, body, &mut frame, &mut out, 0, this_obj)?;
        let ret = match flow {
            Flow::Continue => None,
            Flow::Return(v) => v,
            Flow::Break => bail!("Runtime error: 'break' used outside of a loop"),
            Flow::ContinueLoop => bail!("Runtime error: 'continue' used outside of a loop"),
        };

        Ok(RunResult {
            output: out,
            return_value: ret,
        })
    }
}

pub fn run_main(program: &Program) -> Result<String> {
    let rr = run_function(program, "main", vec![])?;
    Ok(rr.output)
}

pub fn run_function(program: &Program, name: &str, args: Vec<Literal>) -> Result<RunResult> {
    let mut rt = Runtime::new(program);
    rt.run_function(name, args)
}

pub fn run_route(
    program: &Program,
    body: &[Stmt],
    locals: Vec<(String, Literal)>,
) -> Result<RunResult> {
    let mut rt = Runtime::new(program);
    rt.run_route(body, locals, None)
}

enum Flow {
    Continue,
    Return(Option<Literal>),
    Break,
    ContinueLoop,
}

fn exec_block(
    vm: &mut Vm,
    stmts: &[Stmt],
    vars: &mut HashMap<String, Literal>,
    out: &mut String,
    depth: usize,
    this_obj: Option<u64>,
) -> Result<Flow> {
    for stmt in stmts {
        match exec_stmt(vm, stmt, vars, out, depth, this_obj)? {
            Flow::Continue => {}
            Flow::Return(v) => return Ok(Flow::Return(v)),
            Flow::Break => return Ok(Flow::Break),
            Flow::ContinueLoop => return Ok(Flow::ContinueLoop),
        }
    }
    Ok(Flow::Continue)
}

fn exec_stmt(
    vm: &mut Vm,
    stmt: &Stmt,
    vars: &mut HashMap<String, Literal>,
    out: &mut String,
    depth: usize,
    this_obj: Option<u64>,
) -> Result<Flow> {
    match stmt {
        Stmt::VarDecl { name, value } => {
            let key = name.to_lowercase();
            if vars.contains_key(&key) {
                bail!("Duplicate variable declaration: {name}");
            }
            let v = eval_value_expr(vm, value, vars, out, depth, this_obj)?;
            vars.insert(key, v);
            Ok(Flow::Continue)
        }
        Stmt::Assign { name, value } => {
            let key = name.to_lowercase();
            if !vars.contains_key(&key) {
                bail!("Assignment to undefined variable: {name}");
            }
            let v = eval_value_expr(vm, value, vars, out, depth, this_obj)?;
            vars.insert(key, v);
            Ok(Flow::Continue)
        }
        Stmt::AssignMember {
            receiver,
            field,
            value,
        } => {
            let recv = eval_value_expr(vm, receiver, vars, out, depth, this_obj)?;
            let v = eval_value_expr(vm, value, vars, out, depth, this_obj)?;
            assign_member(vm, recv, field, v)?;
            Ok(Flow::Continue)
        }
        Stmt::Print(expr) => {
            let lit = eval_value_expr(vm, expr, vars, out, depth, this_obj)?;
            out.push_str(&literal_to_string(&lit));
            out.push('\n');
            Ok(Flow::Continue)
        }
        Stmt::Expr(expr) => {
            let _ = eval_value_expr(vm, expr, vars, out, depth, this_obj)?;
            Ok(Flow::Continue)
        }
        Stmt::If {
            cond,
            then_body,
            else_body,
        } => {
            let cv = eval_value_expr(vm, cond, vars, out, depth, this_obj)?;
            match cv {
                Literal::Bool(true) => exec_block(vm, then_body, vars, out, depth, this_obj),
                Literal::Bool(false) => {
                    if let Some(else_stmts) = else_body {
                        exec_block(vm, else_stmts, vars, out, depth, this_obj)
                    } else {
                        Ok(Flow::Continue)
                    }
                }
                other => bail!(
                    "Type error: if condition must be bool, got {}",
                    literal_type_name(&other)
                ),
            }
        }
        Stmt::While { cond, body } => {
            const MAX_ITERS: usize = 100_000;
            for _ in 0..MAX_ITERS {
                let cv = eval_value_expr(vm, cond, vars, out, depth, this_obj)?;
                match cv {
                    Literal::Bool(true) => {
                        match exec_block(vm, body, vars, out, depth, this_obj)? {
                            Flow::Continue => {}
                            Flow::Return(v) => return Ok(Flow::Return(v)),
                            Flow::Break => return Ok(Flow::Continue),
                            Flow::ContinueLoop => continue,
                        }
                    }
                    Literal::Bool(false) => return Ok(Flow::Continue),
                    other => {
                        bail!(
                            "Type error: while condition must be bool, got {}",
                            literal_type_name(&other)
                        )
                    }
                }
            }
            bail!("Runtime error: while loop exceeded iteration limit ({MAX_ITERS})")
        }
        Stmt::ForRange { var, start, end, body } => {
            let var_key = var.to_lowercase();
            if vars.contains_key(&var_key) {
                bail!("for-loop variable already defined: {var}");
            }

            let s = eval_value_expr(vm, start, vars, out, depth, this_obj)?;
            let e = eval_value_expr(vm, end, vars, out, depth, this_obj)?;
            let (start_i, end_i) = match (s, e) {
                (Literal::Int(a), Literal::Int(b)) => (a, b),
                (l, r) => bail!(
                    "Type error: for range expects int bounds, got {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            };

            const MAX_ITERS: usize = 100_000;
            let mut iters: usize = 0;
            let mut i = start_i;
            while i < end_i {
                iters += 1;
                if iters > MAX_ITERS {
                    bail!("Runtime error: for loop exceeded iteration limit ({MAX_ITERS})")
                }

                vars.insert(var_key.clone(), Literal::Int(i));
                match exec_block(vm, body, vars, out, depth, this_obj)? {
                    Flow::Continue => {}
                    Flow::Return(v) => {
                        vars.remove(&var_key);
                        return Ok(Flow::Return(v));
                    }
                    Flow::Break => break,
                    Flow::ContinueLoop => {
                        i += 1;
                        continue;
                    }
                }
                i += 1;
            }

            vars.remove(&var_key);
            Ok(Flow::Continue)
        }
        Stmt::Switch {
            expr,
            cases,
            default,
        } => {
            let sv = eval_value_expr(vm, expr, vars, out, depth, this_obj)?;

            // Enforce that case literal types match the switch value type.
            for (lit, _) in cases {
                if literal_type_name(&sv) != literal_type_name(lit) {
                    bail!(
                        "Type error: switch cases must match switch value type (expected {}, got {})",
                        literal_type_name(&sv),
                        literal_type_name(lit)
                    );
                }
            }

            let mut selected: Option<&[Stmt]> = None;
            for (lit, body) in cases {
                if lit == &sv {
                    selected = Some(body);
                    break;
                }
            }

            let selected_body: Option<&[Stmt]> = selected.or_else(|| default.as_deref());
            if let Some(body) = selected_body {
                match exec_block(vm, body, vars, out, depth, this_obj)? {
                    Flow::Continue => Ok(Flow::Continue),
                    Flow::Return(v) => Ok(Flow::Return(v)),
                    Flow::Break => Ok(Flow::Continue),
                    Flow::ContinueLoop => Ok(Flow::ContinueLoop),
                }
            } else {
                Ok(Flow::Continue)
            }
        }
        Stmt::Break => Ok(Flow::Break),
        Stmt::Continue => Ok(Flow::ContinueLoop),
        Stmt::Return(value) => {
            let v = if let Some(expr) = value {
                Some(eval_value_expr(vm, expr, vars, out, depth, this_obj)?)
            } else {
                None
            };
            Ok(Flow::Return(v))
        }
    }
}

fn eval_value_expr(
    vm: &mut Vm,
    expr: &ValueExpr,
    vars: &mut HashMap<String, Literal>,
    out: &mut String,
    depth: usize,
    this_obj: Option<u64>,
) -> Result<Literal> {
    match expr {
        ValueExpr::Literal(l) => Ok(l.clone()),
        ValueExpr::Var(name) => {
            let key = name.to_lowercase();
            vars.get(&key)
                .cloned()
                .ok_or_else(|| anyhow!("Undefined variable: {name}"))
        }
        ValueExpr::This => match this_obj {
            Some(id) => Ok(Literal::Obj(id)),
            None => bail!("Runtime error: 'this' used outside of a method"),
        },
        ValueExpr::New { class_name } => {
            let class = vm
                .classes
                .get(&class_name.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown class: {class_name}"))?;

            let mut fields: HashMap<String, Literal> = HashMap::new();
            for m in &class.members {
                if let ClassMember::Field(f) = m {
                    let v = if let Some(init) = &f.init {
                        eval_value_expr(vm, init, vars, out, depth, None)?
                    } else {
                        Literal::Int(0)
                    };
                    fields.insert(f.name.to_lowercase(), v);
                }
            }

            let id = vm.alloc_object(ObjectInstance {
                class_name: class.name.clone(),
                fields,
            });
            Ok(Literal::Obj(id))
        }
        ValueExpr::Member(receiver, field) => {
            let recv = eval_value_expr(vm, receiver, vars, out, depth, this_obj)?;
            read_member(vm, recv, field)
        }
        ValueExpr::MethodCall {
            receiver,
            name,
            args,
        } => {
            let recv = eval_value_expr(vm, receiver, vars, out, depth, this_obj)?;
            let obj_id = match recv {
                Literal::Obj(id) => id,
                other => bail!(
                    "Type error: method call receiver must be object, got {}",
                    literal_type_name(&other)
                ),
            };

            const MAX_DEPTH: usize = 256;
            if depth >= MAX_DEPTH {
                bail!("Runtime error: call stack depth exceeded ({MAX_DEPTH})");
            }

            let class_name = vm.get_object(obj_id)?.class_name.clone();
            let class = vm
                .classes
                .get(&class_name.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown class: {class_name}"))?;

            let method = class
                .members
                .iter()
                .find_map(|m| match m {
                    ClassMember::Method(f) if f.name.eq_ignore_ascii_case(name) => Some(f),
                    _ => None,
                })
                .ok_or_else(|| anyhow!("Unknown method '{}' on class '{}'", name, class.name))?;

            if method.params.len() != args.len() {
                bail!(
                    "Method '{}.{}' expects {} args but got {}",
                    class.name,
                    method.name,
                    method.params.len(),
                    args.len()
                );
            }

            let mut frame: HashMap<String, Literal> = HashMap::new();
            for (param, arg_expr) in method.params.iter().zip(args.iter()) {
                let v = eval_value_expr(vm, arg_expr, vars, out, depth, this_obj)?;
                frame.insert(param.to_lowercase(), v);
            }

            match exec_block(vm, &method.body, &mut frame, out, depth + 1, Some(obj_id))? {
                Flow::Continue => bail!("Method '{}.{}' did not return a value", class.name, method.name),
                Flow::Return(Some(v)) => Ok(v),
                Flow::Return(None) => bail!("Method '{}.{}' returned no value", class.name, method.name),
                Flow::Break => bail!("Runtime error: 'break' used outside of a loop"),
                Flow::ContinueLoop => bail!("Runtime error: 'continue' used outside of a loop"),
            }
        }
        ValueExpr::Assign { target, value } => {
            let v = eval_value_expr(vm, value, vars, out, depth, this_obj)?;
            match &**target {
                ValueExpr::Var(name) => {
                    let key = name.to_lowercase();
                    if !vars.contains_key(&key) {
                        bail!("Assignment to undefined variable: {name}");
                    }
                    vars.insert(key, v.clone());
                    Ok(v)
                }
                ValueExpr::Member(receiver, field) => {
                    let recv = eval_value_expr(vm, receiver, vars, out, depth, this_obj)?;
                    assign_member(vm, recv, field, v.clone())?;
                    Ok(v)
                }
                _ => bail!("Invalid assignment target"),
            }
        }
        ValueExpr::Call { name, args } => {
            const MAX_DEPTH: usize = 256;
            if depth >= MAX_DEPTH {
                bail!("Runtime error: call stack depth exceeded ({MAX_DEPTH})");
            }

            let builtin = name.to_lowercase();

            // ASP.NET Core-like IActionResult helpers:
            // - Ok([body])
            // - Json(body)
            // - Content(body[, contentType])
            // - Created(body) | Created(location, body)
            // - NotFound([body])
            // - BadRequest([body])
            // - NoContent()
            // - StatusCode(code[, body[, contentType]])
            if builtin == "ok"
                || builtin == "json"
                || builtin == "content"
                || builtin == "created"
                || builtin == "notfound"
                || builtin == "badrequest"
                || builtin == "nocontent"
                || builtin == "statuscode"
            {
                return match builtin.as_str() {
                    "ok" => {
                        if args.len() > 1 {
                            bail!("Ok expects 0 or 1 args");
                        }
                        let body = if args.is_empty() {
                            String::new()
                        } else {
                            let lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                            literal_to_string(&lit)
                        };
                        Ok(action_result(200, body, "text/plain; charset=utf-8"))
                    }
                    "json" => {
                        if args.len() != 1 {
                            bail!("Json expects 1 arg");
                        }
                        let lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                        let body = literal_to_json_http_body(&lit);
                        Ok(action_result(
                            200,
                            body,
                            "application/json; charset=utf-8",
                        ))
                    }
                    "content" => {
                        if args.is_empty() || args.len() > 2 {
                            bail!("Content expects 1 or 2 args");
                        }
                        let body_lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                        let body = literal_to_string(&body_lit);
                        let ct = if args.len() == 2 {
                            let ct_lit = eval_value_expr(vm, &args[1], vars, out, depth, this_obj)?;
                            match ct_lit {
                                Literal::Str(s) => s,
                                other => bail!(
                                    "Content arg2 must be text content-type, got {}",
                                    literal_type_name(&other)
                                ),
                            }
                        } else {
                            "text/plain; charset=utf-8".to_string()
                        };
                        Ok(action_result(200, body, &ct))
                    }
                    "created" => {
                        if args.is_empty() || args.len() > 2 {
                            bail!("Created expects 1 or 2 args");
                        }
                        let body_idx = if args.len() == 2 { 1 } else { 0 };
                        let body_lit = eval_value_expr(vm, &args[body_idx], vars, out, depth, this_obj)?;
                        Ok(action_result(
                            201,
                            literal_to_string(&body_lit),
                            "application/json; charset=utf-8",
                        ))
                    }
                    "notfound" => {
                        if args.len() > 1 {
                            bail!("NotFound expects 0 or 1 args");
                        }
                        let body = if args.is_empty() {
                            "{\"error\":\"not found\"}".to_string()
                        } else {
                            let lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                            match lit {
                                Literal::Str(s) if !looks_like_json_payload(&s) => {
                                    format!("{{\"error\":\"{}\"}}", escape_json_str(&s))
                                }
                                _ => literal_to_json_http_body(&lit),
                            }
                        };
                        Ok(action_result(404, body, "application/json; charset=utf-8"))
                    }
                    "badrequest" => {
                        if args.len() > 1 {
                            bail!("BadRequest expects 0 or 1 args");
                        }
                        let body = if args.is_empty() {
                            "{\"error\":\"bad request\"}".to_string()
                        } else {
                            let lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                            match lit {
                                Literal::Str(s) if !looks_like_json_payload(&s) => {
                                    format!("{{\"error\":\"{}\"}}", escape_json_str(&s))
                                }
                                _ => literal_to_json_http_body(&lit),
                            }
                        };
                        Ok(action_result(400, body, "application/json; charset=utf-8"))
                    }
                    "nocontent" => {
                        if !args.is_empty() {
                            bail!("NoContent expects 0 args");
                        }
                        Ok(action_result(204, String::new(), "text/plain; charset=utf-8"))
                    }
                    "statuscode" => {
                        if args.is_empty() || args.len() > 3 {
                            bail!("StatusCode expects 1 to 3 args");
                        }
                        let code_lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                        let code = match code_lit {
                            Literal::Int(v) => v,
                            other => bail!(
                                "StatusCode arg1 must be int, got {}",
                                literal_type_name(&other)
                            ),
                        };
                        if code < 100 || code > 999 {
                            bail!("StatusCode value out of range: {code}");
                        }
                        let body = if args.len() >= 2 {
                            let lit = eval_value_expr(vm, &args[1], vars, out, depth, this_obj)?;
                            literal_to_string(&lit)
                        } else {
                            String::new()
                        };
                        let ct = if args.len() == 3 {
                            let ct_lit = eval_value_expr(vm, &args[2], vars, out, depth, this_obj)?;
                            match ct_lit {
                                Literal::Str(s) => s,
                                other => bail!(
                                    "StatusCode arg3 must be text content-type, got {}",
                                    literal_type_name(&other)
                                ),
                            }
                        } else {
                            "text/plain; charset=utf-8".to_string()
                        };
                        Ok(action_result(code as u16, body, &ct))
                    }
                    _ => unreachable!(),
                };
            }

            // HTTP response helper:
            // - response(body)
            // - response(status, body)
            // - response(body, "json"|"text")
            // - response(status, body, "json"|"text")
            // Returns [status:int, body:text, content_type:text]
            if builtin == "response" {
                if args.is_empty() || args.len() > 3 {
                    bail!("response expects 1 to 3 args");
                }

                let (status, body_expr, format_expr): (i64, &ValueExpr, Option<&ValueExpr>) =
                    match args.len() {
                        1 => (200, &args[0], None),
                        2 => {
                            let first = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                            match first {
                                Literal::Int(s) => (s, &args[1], None),
                                _ => (200, &args[0], Some(&args[1])),
                            }
                        }
                        3 => {
                            let first = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                            let status = match first {
                                Literal::Int(s) => s,
                                other => bail!(
                                    "response arg1 must be status int when 3 args are provided, got {}",
                                    literal_type_name(&other)
                                ),
                            };
                            (status, &args[1], Some(&args[2]))
                        }
                        _ => unreachable!(),
                    };

                if status < 100 || status > 999 {
                    bail!("response status out of range: {status}");
                }

                let body_lit = eval_value_expr(vm, body_expr, vars, out, depth, this_obj)?;
                let body_text = literal_to_string(&body_lit);

                let content_type = if let Some(fmt_expr) = format_expr {
                    let f = eval_value_expr(vm, fmt_expr, vars, out, depth, this_obj)?;
                    let f = match f {
                        Literal::Str(s) => s.to_ascii_lowercase(),
                        other => bail!(
                            "response format must be text ('json' or 'text'), got {}",
                            literal_type_name(&other)
                        ),
                    };

                    match f.as_str() {
                        "json" | "application/json" => "application/json; charset=utf-8".to_string(),
                        "text" | "text/plain" => "text/plain; charset=utf-8".to_string(),
                        _ => bail!("Unsupported response format '{}'. Use 'text' or 'json'", f),
                    }
                } else {
                    "text/plain; charset=utf-8".to_string()
                };

                return Ok(Literal::Array(vec![
                    Literal::Int(status),
                    Literal::Str(body_text),
                    Literal::Str(content_type),
                ]));
            }

            if builtin == "tojson" || builtin == "jsonserialize" {
                if args.len() != 1 {
                    bail!("{} expects 1 arg", name);
                }
                let lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                return Ok(Literal::Str(literal_to_json(&lit)));
            }

            // Built-in Postgres helpers:
            // - pgExec(sql: text, params?: array) -> int
            // - pgQuery(sql: text, params?: array) -> text (JSON array)
            // - pgQueryOne(sql: text, params?: array) -> text (JSON object or "null")
            if builtin == "pgexec" || builtin == "pgquery" || builtin == "pgqueryone" {
                let db_url = vm
                    .db_url
                    .clone()
                    .ok_or_else(|| anyhow!("No DB configured. Pass --db-url / config.json, or set JWC_DATABASE_URL"))?;

                if args.is_empty() || args.len() > 2 {
                    bail!("{} expects 1 or 2 args", name);
                }

                let sql_lit = eval_value_expr(vm, &args[0], vars, out, depth, this_obj)?;
                let sql = match sql_lit {
                    Literal::Str(s) => s,
                    other => bail!(
                        "{} arg1 must be text SQL, got {}",
                        name,
                        literal_type_name(&other)
                    ),
                };

                let params: Vec<Literal> = if args.len() == 2 {
                    let pv = eval_value_expr(vm, &args[1], vars, out, depth, this_obj)?;
                    match pv {
                        Literal::Array(items) => items,
                        other => bail!(
                            "{} arg2 must be array, got {}",
                            name,
                            literal_type_name(&other)
                        ),
                    }
                } else {
                    Vec::new()
                };

                return match builtin.as_str() {
                    "pgexec" => {
                        let n = crate::db::exec_postgres(&db_url, &sql, &params)?;
                        Ok(Literal::Int(n))
                    }
                    "pgquery" => {
                        let json = crate::db::query_postgres_json(&db_url, &sql, &params)?;
                        Ok(Literal::Str(json))
                    }
                    "pgqueryone" => {
                        let json = crate::db::query_postgres_one_json(&db_url, &sql, &params)?;
                        Ok(Literal::Str(json.unwrap_or_else(|| "null".to_string())))
                    }
                    _ => unreachable!(),
                };
            }

            let f = vm
                .fns
                .get(&name.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown function: {name}"))?;

            if f.params.len() != args.len() {
                bail!(
                    "Function '{}' expects {} args but got {}",
                    f.name,
                    f.params.len(),
                    args.len()
                );
            }

            let mut frame: HashMap<String, Literal> = HashMap::new();
            for (param, arg_expr) in f.params.iter().zip(args.iter()) {
                let v = eval_value_expr(vm, arg_expr, vars, out, depth, this_obj)?;
                frame.insert(param.to_lowercase(), v);
            }

            match exec_block(vm, &f.body, &mut frame, out, depth + 1, None)? {
                Flow::Continue => bail!("Function '{}' did not return a value", f.name),
                Flow::Return(Some(v)) => Ok(v),
                Flow::Return(None) => bail!("Function '{}' returned no value", f.name),
                Flow::Break => bail!("Runtime error: 'break' used outside of a loop"),
                Flow::ContinueLoop => bail!("Runtime error: 'continue' used outside of a loop"),
            }
        }
        ValueExpr::Array(items) => {
            let mut out_items: Vec<Literal> = Vec::with_capacity(items.len());
            for it in items {
                out_items.push(eval_value_expr(vm, it, vars, out, depth, this_obj)?);
            }
            Ok(Literal::Array(out_items))
        }
        ValueExpr::Index(target, index) => {
            let tv = eval_value_expr(vm, target, vars, out, depth, this_obj)?;
            let iv = eval_value_expr(vm, index, vars, out, depth, this_obj)?;
            let idx = match iv {
                Literal::Int(i) => i,
                other => bail!(
                    "Type error: array index must be int, got {}",
                    literal_type_name(&other)
                ),
            };
            let arr = match tv {
                Literal::Array(a) => a,
                other => bail!(
                    "Type error: indexing expects array, got {}",
                    literal_type_name(&other)
                ),
            };

            if idx < 0 {
                bail!("Runtime error: array index out of bounds ({idx})");
            }
            let ui = idx as usize;
            arr.get(ui)
                .cloned()
                .ok_or_else(|| anyhow!("Runtime error: array index out of bounds ({idx})"))
        }
        ValueExpr::Length(target) => {
            let tv = eval_value_expr(vm, target, vars, out, depth, this_obj)?;
            match tv {
                Literal::Array(a) => Ok(Literal::Int(a.len() as i64)),
                other => bail!(
                    "Type error: '.length' expects array, got {}",
                    literal_type_name(&other)
                ),
            }
        }
        ValueExpr::Add(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            match (av, bv) {
                (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Int(x + y)),
                (Literal::Str(x), Literal::Str(y)) => Ok(Literal::Str(format!("{x}{y}"))),
                (l, r) => bail!(
                    "Type error: cannot apply '+' to {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            }
        }
        ValueExpr::Sub(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            match (av, bv) {
                (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Int(x - y)),
                (l, r) => bail!(
                    "Type error: cannot apply '-' to {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            }
        }
        ValueExpr::Mul(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            match (av, bv) {
                (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Int(x * y)),
                (l, r) => bail!(
                    "Type error: cannot apply '*' to {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            }
        }
        ValueExpr::Div(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            match (av, bv) {
                (Literal::Int(_), Literal::Int(0)) => bail!("Runtime error: division by zero"),
                (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Int(x / y)),
                (l, r) => bail!(
                    "Type error: cannot apply '/' to {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            }
        }
        ValueExpr::Mod(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            match (av, bv) {
                (Literal::Int(_), Literal::Int(0)) => bail!("Runtime error: modulo by zero"),
                (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Int(x % y)),
                (l, r) => bail!(
                    "Type error: cannot apply '%' to {} and {}",
                    literal_type_name(&l),
                    literal_type_name(&r)
                ),
            }
        }
        ValueExpr::Neg(a) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            match av {
                Literal::Int(x) => Ok(Literal::Int(-x)),
                other => bail!(
                    "Type error: unary '-' expects int, got {}",
                    literal_type_name(&other)
                ),
            }
        }
        ValueExpr::And(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            match av {
                Literal::Bool(false) => Ok(Literal::Bool(false)),
                Literal::Bool(true) => {
                    let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
                    match bv {
                        Literal::Bool(v) => Ok(Literal::Bool(v)),
                        other => bail!(
                            "Type error: 'and' expects bool operands, got bool and {}",
                            literal_type_name(&other)
                        ),
                    }
                }
                other => bail!(
                    "Type error: 'and' expects bool operands, got {}",
                    literal_type_name(&other)
                ),
            }
        }
        ValueExpr::Or(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            match av {
                Literal::Bool(true) => Ok(Literal::Bool(true)),
                Literal::Bool(false) => {
                    let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
                    match bv {
                        Literal::Bool(v) => Ok(Literal::Bool(v)),
                        other => bail!(
                            "Type error: 'or' expects bool operands, got bool and {}",
                            literal_type_name(&other)
                        ),
                    }
                }
                other => bail!(
                    "Type error: 'or' expects bool operands, got {}",
                    literal_type_name(&other)
                ),
            }
        }
        ValueExpr::Eq(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            Ok(Literal::Bool(literal_eq(&av, &bv)?))
        }
        ValueExpr::Neq(a, b) => {
            let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
            let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
            Ok(Literal::Bool(!literal_eq(&av, &bv)?))
        }
        ValueExpr::Lt(a, b) => cmp_int(vm, a, b, vars, out, depth, this_obj, |x, y| x < y),
        ValueExpr::Gt(a, b) => cmp_int(vm, a, b, vars, out, depth, this_obj, |x, y| x > y),
        ValueExpr::Lte(a, b) => cmp_int(vm, a, b, vars, out, depth, this_obj, |x, y| x <= y),
        ValueExpr::Gte(a, b) => cmp_int(vm, a, b, vars, out, depth, this_obj, |x, y| x >= y),
        ValueExpr::DbSelect {
            entity,
            where_field,
            where_value,
        } => {
            let db_url = vm
                .db_url
                .clone()
                .ok_or_else(|| anyhow!("No DB configured. Pass --db-url / config.json, or set JWC_DATABASE_URL"))?;

            let ent = vm
                .entities
                .get(&entity.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown entity in select expression: {entity}"))?;
            let table = crate::sql::to_snake_case(&ent.name);

            if let (Some(field), Some(value_expr)) = (where_field, where_value) {
                let f = ent
                    .fields
                    .iter()
                    .find(|x| x.name.eq_ignore_ascii_case(field))
                    .ok_or_else(|| anyhow!("Unknown field '{}' on entity '{}'", field, ent.name))?;
                let raw = eval_value_expr(vm, value_expr, vars, out, depth, this_obj)?;
                let v = coerce_literal_for_type(raw, &f.ty)?;
                let cast = sql_cast_suffix(&f.ty);
                let sql = format!(
                    "SELECT * FROM \"{}\" WHERE \"{}\" = $1{}",
                    table, f.name, cast
                );
                let row = crate::db::query_postgres_one_json(&db_url, &sql, &[v])?;
                Ok(Literal::Str(row.unwrap_or_else(|| "null".to_string())))
            } else {
                let sql = format!("SELECT * FROM \"{}\" ORDER BY 1", table);
                let rows = crate::db::query_postgres_json(&db_url, &sql, &[])?;
                Ok(Literal::Str(rows))
            }
        }
        ValueExpr::DbInsert { entity, assignments } => {
            let db_url = vm
                .db_url
                .clone()
                .ok_or_else(|| anyhow!("No DB configured. Pass --db-url / config.json, or set JWC_DATABASE_URL"))?;

            let ent = vm
                .entities
                .get(&entity.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown entity in insert expression: {entity}"))?;
            let table = crate::sql::to_snake_case(&ent.name);

            let mut cols: Vec<String> = Vec::new();
            let mut vals_sql: Vec<String> = Vec::new();
            let mut params: Vec<Literal> = Vec::new();

            for (field, expr) in assignments {
                let f = ent
                    .fields
                    .iter()
                    .find(|x| x.name.eq_ignore_ascii_case(field))
                    .ok_or_else(|| anyhow!("Unknown field '{}' on entity '{}'", field, ent.name))?;
                let raw = eval_value_expr(vm, expr, vars, out, depth, this_obj)?;
                let v = coerce_literal_for_type(raw, &f.ty)?;
                params.push(v);
                let idx = params.len();
                cols.push(format!("\"{}\"", f.name));
                vals_sql.push(format!("${}{}", idx, sql_cast_suffix(&f.ty)));
            }

            let has_pk = ent
                .fields
                .iter()
                .find(|f| f.mods.primary_key)
                .map(|f| f.name.to_lowercase());
            let has_pk_in_assignments = has_pk
                .as_ref()
                .map(|pk| assignments.iter().any(|(n, _)| n.eq_ignore_ascii_case(pk)))
                .unwrap_or(false);

            if !has_pk_in_assignments {
                if let Some(pk_field) = ent.fields.iter().find(|f| f.mods.primary_key) {
                    if pk_field.ty.name == "int" {
                        cols.push(format!("\"{}\"", pk_field.name));
                        vals_sql.push(format!(
                            "(SELECT COALESCE(MAX(\"{}\"), 0) + 1 FROM \"{}\")",
                            pk_field.name, table
                        ));
                    }
                }
            }

            let sql = format!(
                "INSERT INTO \"{}\" ({}) VALUES ({}) RETURNING *",
                table,
                cols.join(", "),
                vals_sql.join(", ")
            );
            let row = crate::db::query_postgres_one_json(&db_url, &sql, &params)?;
            Ok(Literal::Str(row.unwrap_or_else(|| "null".to_string())))
        }
        ValueExpr::DbUpdate {
            entity,
            assignments,
            where_field,
            where_value,
        } => {
            let db_url = vm
                .db_url
                .clone()
                .ok_or_else(|| anyhow!("No DB configured. Pass --db-url / config.json, or set JWC_DATABASE_URL"))?;

            let ent = vm
                .entities
                .get(&entity.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown entity in update expression: {entity}"))?;
            let table = crate::sql::to_snake_case(&ent.name);

            let mut set_parts: Vec<String> = Vec::new();
            let mut params: Vec<Literal> = Vec::new();

            for (field, expr) in assignments {
                let f = ent
                    .fields
                    .iter()
                    .find(|x| x.name.eq_ignore_ascii_case(field))
                    .ok_or_else(|| anyhow!("Unknown field '{}' on entity '{}'", field, ent.name))?;
                let raw = eval_value_expr(vm, expr, vars, out, depth, this_obj)?;
                let v = coerce_literal_for_type(raw, &f.ty)?;
                params.push(v);
                let idx = params.len();
                set_parts.push(format!(
                    "\"{}\" = ${}{}",
                    f.name,
                    idx,
                    sql_cast_suffix(&f.ty)
                ));
            }

            let wf = ent
                .fields
                .iter()
                .find(|x| x.name.eq_ignore_ascii_case(where_field))
                .ok_or_else(|| anyhow!("Unknown where field '{}' on entity '{}'", where_field, ent.name))?;
            let raw_wv = eval_value_expr(vm, where_value, vars, out, depth, this_obj)?;
            let wv = coerce_literal_for_type(raw_wv, &wf.ty)?;
            params.push(wv);
            let widx = params.len();

            let sql = format!(
                "UPDATE \"{}\" SET {} WHERE \"{}\" = ${}{} RETURNING *",
                table,
                set_parts.join(", "),
                wf.name,
                widx,
                sql_cast_suffix(&wf.ty)
            );
            let row = crate::db::query_postgres_one_json(&db_url, &sql, &params)?;
            Ok(Literal::Str(row.unwrap_or_else(|| "null".to_string())))
        }
        ValueExpr::DbDelete {
            entity,
            where_field,
            where_value,
        } => {
            let db_url = vm
                .db_url
                .clone()
                .ok_or_else(|| anyhow!("No DB configured. Pass --db-url / config.json, or set JWC_DATABASE_URL"))?;

            let ent = vm
                .entities
                .get(&entity.to_lowercase())
                .copied()
                .ok_or_else(|| anyhow!("Unknown entity in delete expression: {entity}"))?;
            let table = crate::sql::to_snake_case(&ent.name);

            let wf = ent
                .fields
                .iter()
                .find(|x| x.name.eq_ignore_ascii_case(where_field))
                .ok_or_else(|| anyhow!("Unknown where field '{}' on entity '{}'", where_field, ent.name))?;
            let raw_wv = eval_value_expr(vm, where_value, vars, out, depth, this_obj)?;
            let wv = coerce_literal_for_type(raw_wv, &wf.ty)?;

            let sql = format!(
                "DELETE FROM \"{}\" WHERE \"{}\" = $1{} RETURNING *",
                table,
                wf.name,
                sql_cast_suffix(&wf.ty)
            );
            let row = crate::db::query_postgres_one_json(&db_url, &sql, &[wv])?;
            Ok(Literal::Str(row.unwrap_or_else(|| "null".to_string())))
        }
    }
}

fn sql_cast_suffix(ty: &TypeSpec) -> &'static str {
    match ty.name.as_str() {
        "int" => "::int",
        "bigint" => "::bigint",
        "bool" => "::boolean",
        _ => "",
    }
}

fn action_result(status: u16, body: String, content_type: &str) -> Literal {
    Literal::Array(vec![
        Literal::Int(status as i64),
        Literal::Str(body),
        Literal::Str(content_type.to_string()),
    ])
}

fn coerce_literal_for_type(value: Literal, ty: &TypeSpec) -> Result<Literal> {
    match ty.name.as_str() {
        "int" | "bigint" => match value {
            Literal::Int(_) => Ok(value),
            Literal::Str(s) => {
                let parsed = s.parse::<i64>().map_err(|_| {
                    anyhow!("Cannot convert '{}' to integer for field type '{}'", s, ty.name)
                })?;
                Ok(Literal::Int(parsed))
            }
            other => bail!(
                "Cannot use {} where integer is expected",
                literal_type_name(&other)
            ),
        },
        "bool" => match value {
            Literal::Bool(_) => Ok(value),
            Literal::Str(s) => {
                let lowered = s.to_ascii_lowercase();
                match lowered.as_str() {
                    "true" => Ok(Literal::Bool(true)),
                    "false" => Ok(Literal::Bool(false)),
                    _ => bail!("Cannot convert '{}' to bool", s),
                }
            }
            other => bail!(
                "Cannot use {} where bool is expected",
                literal_type_name(&other)
            ),
        },
        _ => Ok(value),
    }
}

fn cmp_int<F>(
    vm: &mut Vm,
    a: &ValueExpr,
    b: &ValueExpr,
    vars: &mut HashMap<String, Literal>,
    out: &mut String,
    depth: usize,
    this_obj: Option<u64>,
    f: F,
) -> Result<Literal>
where
    F: FnOnce(i64, i64) -> bool,
{
    let av = eval_value_expr(vm, a, vars, out, depth, this_obj)?;
    let bv = eval_value_expr(vm, b, vars, out, depth, this_obj)?;
    match (av, bv) {
        (Literal::Int(x), Literal::Int(y)) => Ok(Literal::Bool(f(x, y))),
        (l, r) => bail!(
            "Type error: ordering comparison expects int operands, got {} and {}",
            literal_type_name(&l),
            literal_type_name(&r)
        ),
    }
}

fn read_member(vm: &mut Vm, recv: Literal, field: &str) -> Result<Literal> {
    match recv {
        Literal::Obj(id) => {
            let obj = vm.get_object(id)?;
            let key = field.to_lowercase();
            obj.fields
                .get(&key)
                .cloned()
                .ok_or_else(|| anyhow!("Unknown field '{}' on class '{}'", field, obj.class_name))
        }
        other => bail!(
            "Type error: member access receiver must be object, got {}",
            literal_type_name(&other)
        ),
    }
}

fn assign_member(vm: &mut Vm, recv: Literal, field: &str, value: Literal) -> Result<()> {
    match recv {
        Literal::Obj(id) => {
            let obj = vm.get_object_mut(id)?;
            let key = field.to_lowercase();
            if !obj.fields.contains_key(&key) {
                bail!("Unknown field '{}' on class '{}'", field, obj.class_name);
            }
            obj.fields.insert(key, value);
            Ok(())
        }
        other => bail!(
            "Type error: member assignment receiver must be object, got {}",
            literal_type_name(&other)
        ),
    }
}

fn literal_eq(a: &Literal, b: &Literal) -> Result<bool> {
    match (a, b) {
        (Literal::Int(x), Literal::Int(y)) => Ok(x == y),
        (Literal::Bool(x), Literal::Bool(y)) => Ok(x == y),
        (Literal::Str(x), Literal::Str(y)) => Ok(x == y),
        (Literal::Decimal(_), _) | (_, Literal::Decimal(_)) => {
            bail!("Type error: decimal equality is not supported yet")
        }
        (Literal::Array(_), _) | (_, Literal::Array(_)) => {
            bail!("Type error: array equality is not supported yet")
        }
        (l, r) => bail!(
            "Type error: cannot compare {} and {} with '=='",
            literal_type_name(l),
            literal_type_name(r)
        ),
    }
}

pub(crate) fn literal_type_name(lit: &Literal) -> &'static str {
    match lit {
        Literal::Int(_) => "int",
        Literal::Decimal(_) => "decimal",
        Literal::Str(_) => "text",
        Literal::Bool(_) => "bool",
        Literal::Array(_) => "array",
        Literal::Obj(_) => "object",
    }
}

fn literal_to_string(lit: &Literal) -> String {
    match lit {
        Literal::Int(v) => v.to_string(),
        Literal::Decimal(s) => s.clone(),
        Literal::Str(s) => s.clone(),
        Literal::Bool(true) => "true".to_string(),
        Literal::Bool(false) => "false".to_string(),
        Literal::Array(items) => {
            let inner = items
                .iter()
                .map(literal_to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Literal::Obj(id) => format!("<object #{id}>"),
    }
}

fn literal_to_json_http_body(lit: &Literal) -> String {
    match lit {
        Literal::Str(s) if looks_like_json_payload(s) => s.clone(),
        _ => literal_to_json(lit),
    }
}

fn looks_like_json_payload(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t == "null" || t == "true" || t == "false" {
        return true;
    }
    if t.starts_with('{') || t.starts_with('[') {
        return true;
    }
    t.parse::<i64>().is_ok()
}

fn literal_to_json(lit: &Literal) -> String {
    match lit {
        Literal::Int(v) => v.to_string(),
        Literal::Decimal(s) => s.clone(),
        Literal::Str(s) => format!("\"{}\"", escape_json_str(s)),
        Literal::Bool(true) => "true".to_string(),
        Literal::Bool(false) => "false".to_string(),
        Literal::Array(items) => {
            let inner = items
                .iter()
                .map(literal_to_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Literal::Obj(id) => format!("\"<object #{id}>\""),
    }
}

fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_main_and_collects_print_output() {
        let src = r#"
            function main() {
                let msg = "Hello";
                print(msg);
                print(123);
                print(true);
                print(1 + 2 + 3);
                print(("a" + "b") + "c");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();

        let out = run_main(&program).unwrap();
        assert_eq!(out, "Hello\n123\ntrue\n6\nabc\n");
    }

    #[test]
    fn can_execute_route_body_with_method_and_path() {
        let src = r#"
            route get "/echo" {
                return method + ":" + path;
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();

        let route = program.routes.iter().find(|r| r.path == "/echo").unwrap();
        let rr = run_route(
            &program,
            &route.body,
            vec![
                ("method".into(), Literal::Str("GET".into())),
                ("path".into(), Literal::Str("/echo".into())),
            ],
        )
        .unwrap();

        assert_eq!(rr.output, "");
        assert_eq!(rr.return_value, Some(Literal::Str("GET:/echo".into())));
    }

    #[test]
    fn if_executes_then_block_when_true() {
        let src = r#"
            function main() {
                if (true) {
                    print("yes");
                }
                print("done");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "yes\ndone\n");
    }

    #[test]
    fn if_skips_then_block_when_false() {
        let src = r#"
            function main() {
                if (false) {
                    print("no");
                }
                print("ok");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "ok\n");
    }

    #[test]
    fn if_else_executes_else_block() {
        let src = r#"
            function main() {
                if (false) {
                    print("then");
                } else {
                    print("else");
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "else\n");
    }

    #[test]
    fn comparisons_and_boolean_ops_work() {
        let src = r#"
            function main() {
                if (1 + 1 == 2 and 3 > 2 or false) {
                    print("ok");
                } else {
                    print("no");
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "ok\n");
    }

    #[test]
    fn assignment_updates_variable() {
        let src = r#"
            function main() {
                let x = 1;
                x = x + 2;
                print(x);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "3\n");
    }

    #[test]
    fn while_loop_runs_until_condition_false() {
        let src = r#"
            function main() {
                let i = 0;
                while (i < 3) {
                    print(i);
                    i = i + 1;
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "0\n1\n2\n");
    }

    #[test]
    fn return_exits_early() {
        let src = r#"
            function main() {
                print("a");
                return;
                print("b");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "a\n");
    }

    #[test]
    fn function_call_with_params_and_return_value() {
        let src = r#"
            function add(a, b) {
                return a + b;
            }

            function main() {
                let x = add(10, 20);
                print(x);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "30\n");
    }

    #[test]
    fn call_as_statement_allows_side_effects() {
        let src = r#"
            function greet() {
                print("hi");
                return 1;
            }

            function main() {
                greet();
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "hi\n");
    }

    #[test]
    fn if_condition_must_be_bool() {
        let src = r#"
            function main() {
                if (1) { print("no"); }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let err = run_main(&program).unwrap_err().to_string();
        assert!(err.contains("if condition must be bool"));
    }

    #[test]
    fn tojson_serializes_primitives_and_arrays() {
        let src = r#"
            function main() {
                print(ToJson([1, true, "hi"]));
            }
        "#;
        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "[1,true,\"hi\"]\n");
    }

    #[test]
    fn json_helper_keeps_json_payload_text() {
        let src = r#"
            route get "/x" {
                return Json("{\"ok\":true}");
            }
        "#;
        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let route = program.routes.iter().find(|r| r.path == "/x").unwrap();
        let rr = run_route(
            &program,
            &route.body,
            vec![
                ("method".into(), Literal::Str("GET".into())),
                ("path".into(), Literal::Str("/x".into())),
            ],
        )
        .unwrap();
        assert_eq!(rr.return_value, Some(Literal::Array(vec![
            Literal::Int(200),
            Literal::Str("{\"ok\":true}".into()),
            Literal::Str("application/json; charset=utf-8".into()),
        ])));
    }

    #[test]
    fn notfound_wraps_plain_text_into_json_error() {
        let src = r#"
            route get "/x" {
                return NotFound("todo not found");
            }
        "#;
        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let route = program.routes.iter().find(|r| r.path == "/x").unwrap();
        let rr = run_route(
            &program,
            &route.body,
            vec![
                ("method".into(), Literal::Str("GET".into())),
                ("path".into(), Literal::Str("/x".into())),
            ],
        )
        .unwrap();
        assert_eq!(rr.return_value, Some(Literal::Array(vec![
            Literal::Int(404),
            Literal::Str("{\"error\":\"todo not found\"}".into()),
            Literal::Str("application/json; charset=utf-8".into()),
        ])));
    }

    #[test]
    fn for_range_loops_from_start_to_end_exclusive() {
        let src = r#"
            function main() {
                for (i in 0..3) {
                    print(i);
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "0\n1\n2\n");
    }

    #[test]
    fn break_exits_for_loop_early() {
        let src = r#"
            function main() {
                for (i in 0..10) {
                    print(i);
                    break;
                }
                print("done");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "0\ndone\n");
    }

    #[test]
    fn continue_skips_to_next_iteration() {
        let src = r#"
            function main() {
                for (i in 0..4) {
                    if (i == 2) {
                        continue;
                    }
                    print(i);
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "0\n1\n3\n");
    }

    #[test]
    fn arithmetic_precedence_mul_before_add() {
        let src = r#"
            function main() {
                print(1 + 2 * 3);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "7\n");
    }

    #[test]
    fn arithmetic_parentheses_override_precedence() {
        let src = r#"
            function main() {
                print((1 + 2) * 3);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "9\n");
    }

    #[test]
    fn unary_minus_works() {
        let src = r#"
            function main() {
                print(-1 + 2);
                print(-(1 + 2));
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "1\n-3\n");
    }

    #[test]
    fn compound_assignments_work() {
        let src = r#"
            function main() {
                let x = 1;
                x += 2;
                x *= 3;
                x -= 4;
                x /= 2;
                x %= 3;
                print(x);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "2\n");
    }

    #[test]
    fn switch_selects_matching_case() {
        let src = r#"
            function main() {
                let x = 2;
                switch (x) {
                    case 1: { print("one"); }
                    case 2: { print("two"); }
                    default: { print("other"); }
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "two\n");
    }

    #[test]
    fn switch_uses_default_when_no_case_matches() {
        let src = r#"
            function main() {
                let x = 99;
                switch (x) {
                    case 1: { print("one"); }
                    case 2: { print("two"); }
                    default: { print("other"); }
                }
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "other\n");
    }

    #[test]
    fn break_exits_switch_but_not_program() {
        let src = r#"
            function main() {
                let x = 1;
                switch (x) {
                    case 1: {
                        print("a");
                        break;
                        print("b");
                    }
                    default: { print("no"); }
                }
                print("done");
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "a\ndone\n");
    }

    #[test]
    fn arrays_support_indexing_and_length() {
        let src = r#"
            function main() {
                let a = [10, 20, 30];
                print(a.length);
                print(a[0]);
                print(a[2]);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "3\n10\n30\n");
    }

    #[test]
    fn arrays_can_be_nested_and_printed() {
        let src = r#"
            function main() {
                let a = [1, 2];
                let b = [a, [3, 4]];
                print(b);
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "[[1, 2], [3, 4]]\n");
    }

    #[test]
    fn classes_support_new_this_fields_and_methods() {
        let src = r#"
            class Counter {
                let value = 0;

                function inc() {
                    this.value = this.value + 1;
                    return this.value;
                }
            }

            function main() {
                let c = new Counter();
                print(c.inc());
                print(c.inc());
            }
        "#;

        let program = crate::parser::parse_program(src).unwrap();
        crate::parser::validate_program(&program).unwrap();
        let out = run_main(&program).unwrap();
        assert_eq!(out, "1\n2\n");
    }
}
