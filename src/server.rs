use anyhow::{anyhow, bail, Result};
use std::io::Read;
use tiny_http::{Header, Response, Server, StatusCode};

use crate::ast::{ClassMember, Literal, Program, RouteDecl};

pub fn serve(program: &Program, port: u16) -> Result<()> {
    serve_with_db_url(program, port, None)
}

pub fn serve_with_db_url(program: &Program, port: u16, db_url: Option<String>) -> Result<()> {
    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow!("Failed to bind {addr}: {e}"))?;

    let mut rt = crate::runner::Runtime::new_with_db_url(program, db_url);

    // New model: controllers are auto-instantiated once per server.
    // Each controller's routes run with `this` bound to that controller instance.
    let mut controller_route_table: Vec<(&RouteDecl, u64)> = Vec::new();
    if !program.controllers.is_empty() {
        for c in &program.controllers {
            let this_id = rt.create_instance(&c.name)?;
            for m in &c.members {
                if let ClassMember::Route(r) = m {
                    controller_route_table.push((r, this_id));
                }
            }
        }
    }

    let app_this: Option<u64> = match rt.run_function("init", vec![]) {
        Ok(rr) => match rr.return_value {
            Some(Literal::Obj(id)) => {
                if !rr.output.is_empty() {
                    eprint!("{}", rr.output);
                }
                Some(id)
            }
            Some(other) => {
                bail!(
                    "init() must return an object (from new ClassName()), got {}",
                    crate::runner::literal_type_name(&other)
                )
            }
            None => bail!("init() must return an object (from new ClassName())"),
        },
        Err(e) => {
            if e.to_string().contains("Unknown function") {
                None
            } else {
                return Err(e);
            }
        }
    };

    let controller_routes: Vec<&RouteDecl> = match app_this {
        Some(id) => {
            let class_name = rt.object_class_name(id)?;
            let class = program
                .classes
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&class_name))
                .ok_or_else(|| anyhow!("init() returned unknown class instance: {class_name}"))?;
            class
                .members
                .iter()
                .filter_map(|m| match m {
                    ClassMember::Route(r) => Some(r),
                    _ => None,
                })
                .collect()
        }
        None => Vec::new(),
    };

    eprintln!("jwc: listening on http://{addr}");
    eprintln!("jwc: handler: function handle(method, path)");
    eprintln!("jwc: return text => 200, or [status:int, body:text]");
    if !controller_route_table.is_empty() {
        eprintln!("jwc: controllers: enabled (this-bound routes)");
    }
    if app_this.is_some() {
        eprintln!("jwc: init(): enabled (this-bound routes)");
    }

    for mut req in server.incoming_requests() {
        let method = req.method().as_str().to_string();
        let path = req.url().to_string();

        let body = read_request_body(&mut req)?;

        // Prefer controller routes (declared inside `controller` blocks).
        let mut route_result: Option<Result<crate::runner::RunResult>> = None;
        if !controller_route_table.is_empty() {
            let m = method.to_lowercase();
            if let Some(((route, this_id), params)) = controller_route_table
                .iter()
                .find_map(|(r, this_id)| {
                    match_route(r.method.as_str(), r.path.as_str(), &m, &path)
                        .map(|p| ((*r, *this_id), p))
                })
            {
                let mut locals = vec![
                    ("method".to_string(), Literal::Str(method.clone())),
                    ("path".to_string(), Literal::Str(path.clone())),
                    ("body".to_string(), Literal::Str(body.clone())),
                ];
                locals.extend(params);
                route_result = Some(rt.run_route(&route.body, locals, Some(this_id)));
            }
        } else if !controller_routes.is_empty() {
            // Backward-compatible: routes inside the init() object's class.
            let m = method.to_lowercase();
            if let Some((route, params)) = controller_routes
                .iter()
                .copied()
                .find_map(|r| match_route(r.method.as_str(), r.path.as_str(), &m, &path).map(|p| (r, p)))
            {
                let mut locals = vec![
                    ("method".to_string(), Literal::Str(method.clone())),
                    ("path".to_string(), Literal::Str(path.clone())),
                    ("body".to_string(), Literal::Str(body.clone())),
                ];
                locals.extend(params);
                route_result = Some(rt.run_route(&route.body, locals, app_this));
            }
        } else if !program.routes.is_empty() {
            // Otherwise use top-level routes.
            let m = method.to_lowercase();
            if let Some((route, params)) = program
                .routes
                .iter()
                .find_map(|r| match_route(r.method.as_str(), r.path.as_str(), &m, &path).map(|p| (r, p)))
            {
                let mut locals = vec![
                    ("method".to_string(), Literal::Str(method.clone())),
                    ("path".to_string(), Literal::Str(path.clone())),
                    ("body".to_string(), Literal::Str(body.clone())),
                ];
                locals.extend(params);
                route_result = Some(rt.run_route(&route.body, locals, app_this));
            }
        }

        let used_route = route_result.is_some();

        let result = match route_result {
            Some(r) => r,
            None => {
                // Legacy fallback: allow handle(method, path, body) or handle(method, path)
                let r3 = rt.run_function("handle", vec![
                    Literal::Str(method.clone()),
                    Literal::Str(path.clone()),
                    Literal::Str(body.clone()),
                ]);
                match r3 {
                    Ok(ok) => Ok(ok),
                    Err(_) => rt.run_function("handle", vec![
                        Literal::Str(method),
                        Literal::Str(path),
                    ]),
                }
            }
        };

        match result {
            Ok(run) => {
                if !run.output.is_empty() {
                    eprint!("{}", run.output);
                }

                let (status, body, content_type) = handler_return_to_http(run.return_value)?;
                let mut resp = Response::from_string(body).with_status_code(StatusCode(status));
                let ct = Header::from_bytes(
                    &b"Content-Type"[..],
                    content_type.as_bytes(),
                )
                .map_err(|_| anyhow!("Failed to create Content-Type header"))?;
                resp.add_header(ct);
                let _ = req.respond(resp);
            }
            Err(err) => {
                // If there is no handle() and no matching route, treat as 404.
                if !used_route && err.to_string().contains("Unknown function") {
                    let mut resp = Response::from_string("Not found\n")
                        .with_status_code(StatusCode(404));
                    if let Ok(ct) = Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"text/plain; charset=utf-8"[..],
                    ) {
                        resp.add_header(ct);
                    }
                    let _ = req.respond(resp);
                    continue;
                }
                // Use pretty anyhow formatting so the cause chain is visible (e.g. Postgres auth errors).
                let body = format!("Handler error:\n{err:#}\n");
                let mut resp = Response::from_string(body).with_status_code(StatusCode(500));
                if let Ok(ct) = Header::from_bytes(
                    &b"Content-Type"[..],
                    &b"text/plain; charset=utf-8"[..],
                ) {
                    resp.add_header(ct);
                }
                let _ = req.respond(resp);
            }
        }
    }

    Ok(())
}

fn read_request_body(req: &mut tiny_http::Request) -> Result<String> {
    const MAX_BODY_BYTES: usize = 1024 * 1024; // 1 MiB
    let mut buf = Vec::new();
    req.as_reader()
        .take(MAX_BODY_BYTES as u64)
        .read_to_end(&mut buf)
        .map_err(|e| anyhow!("Failed to read request body: {e}"))?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn match_route(
    route_method: &str,
    route_path: &str,
    req_method_lower: &str,
    req_path: &str,
) -> Option<Vec<(String, Literal)>> {
    if route_method.trim().to_lowercase() != req_method_lower {
        return None;
    }

    let rsegs = split_path(route_path);
    let psegs = split_path(req_path);
    if rsegs.len() != psegs.len() {
        return None;
    }

    let mut params: Vec<(String, Literal)> = Vec::new();
    for (rs, ps) in rsegs.iter().zip(psegs.iter()) {
        if let Some(name) = parse_param(rs) {
            params.push((name.to_string(), Literal::Str(ps.to_string())));
        } else if rs != ps {
            return None;
        }
    }

    Some(params)
}

fn split_path(path: &str) -> Vec<&str> {
    path.split('?')
        .next()
        .unwrap_or(path)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_param(seg: &str) -> Option<&str> {
    let s = seg.strip_prefix('{')?.strip_suffix('}')?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s)
}

fn handler_return_to_http(ret: Option<Literal>) -> Result<(u16, String, String)> {
    match ret {
        Some(Literal::Str(body)) => Ok((200, body, "text/plain; charset=utf-8".to_string())),
        Some(Literal::Array(items)) => {
            if items.len() != 2 && items.len() != 3 {
                bail!("handle() returned array of length {}, expected 2 or 3", items.len());
            }
            let status = match &items[0] {
                Literal::Int(v) => *v,
                other => bail!(
                    "handle() status must be int, got {}",
                    crate::runner::literal_type_name(other)
                ),
            };
            if status < 100 || status > 999 {
                bail!("handle() status out of range: {status}");
            }
            let body = match &items[1] {
                Literal::Str(s) => s.clone(),
                other => bail!(
                    "handle() body must be text, got {}",
                    crate::runner::literal_type_name(other)
                ),
            };

            let content_type = if items.len() == 3 {
                match &items[2] {
                    Literal::Str(s) => s.clone(),
                    other => bail!(
                        "handle() content-type must be text, got {}",
                        crate::runner::literal_type_name(other)
                    ),
                }
            } else {
                "text/plain; charset=utf-8".to_string()
            };

            Ok((status as u16, body, content_type))
        }
        Some(other) => bail!(
            "handle() must return text or [status:int, body:text] or [status:int, body:text, content_type:text], got {}",
            crate::runner::literal_type_name(&other)
        ),
        None => bail!("handle() must return a value (text or [status:int, body:text])"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_return_text_defaults_to_200() {
        let (s, b, ct) = handler_return_to_http(Some(Literal::Str("ok".into()))).unwrap();
        assert_eq!(s, 200);
        assert_eq!(b, "ok");
        assert_eq!(ct, "text/plain; charset=utf-8");
    }

    #[test]
    fn handler_return_tuple_allows_custom_status() {
        let (s, b, ct) = handler_return_to_http(Some(Literal::Array(vec![
            Literal::Int(404),
            Literal::Str("no".into()),
        ])))
        .unwrap();
        assert_eq!(s, 404);
        assert_eq!(b, "no");
        assert_eq!(ct, "text/plain; charset=utf-8");
    }

    #[test]
    fn handler_return_tuple_allows_custom_content_type() {
        let (s, b, ct) = handler_return_to_http(Some(Literal::Array(vec![
            Literal::Int(200),
            Literal::Str("{}".into()),
            Literal::Str("application/json; charset=utf-8".into()),
        ])))
        .unwrap();
        assert_eq!(s, 200);
        assert_eq!(b, "{}");
        assert_eq!(ct, "application/json; charset=utf-8");
    }

    #[test]
    fn route_match_supports_params() {
        let params = match_route("get", "/todos/{id}", "get", "/todos/123").unwrap();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].0, "id");
        assert_eq!(params[0].1, Literal::Str("123".into()));
    }

    #[test]
    fn route_match_requires_same_segment_count() {
        assert!(match_route("get", "/todos/{id}", "get", "/todos").is_none());
    }
}
