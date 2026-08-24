//! `jwc swagger` — a browsable API reference over the OpenAPI document.
//!
//! # What the 0.9 command actually was
//!
//! `src/swagger.rs` (661 lines) and `src/cmd/swagger.rs` went at the
//! v0.25.0 cutover, and I called that a loss. It was not, quite: the old
//! module was a **second OpenAPI generator**, and the command wrote its
//! output to `openapi.json`. `jwc openapi --out openapi.json` does that
//! today from `src/openapi.rs`, so restoring the old code verbatim would
//! reintroduce two generators to keep in step by hand — the mistake the
//! native backend avoids by calling `query_sql` instead of reimplementing
//! it.
//!
//! What never existed, in either version, is the thing people mean when
//! they type `jwc swagger`: somewhere to *read* the API. That is what this
//! is. The document comes from `cmd::openapi_document`, so there is still
//! exactly one generator.
//!
//! # Self-contained on purpose
//!
//! No CDN, no vendored `swagger-ui-dist`. A page that fetches its
//! renderer from unpkg is blank on an air-gapped box and pins a
//! third-party script into a developer's browser session; vendoring the
//! real Swagger UI would put ~1.5 MB of JavaScript into every `jwc`
//! binary. The page below is HTML and CSS the generator writes, so
//! `--out api.html` produces one file you can open, commit or publish.

use anyhow::Result;
use serde_json::Value;
use std::fmt::Write as _;

/// The HTTP methods a path item may carry, in the order they are shown.
const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A one-line rendering of a JSON Schema node: `string`, `integer`,
/// `Note[]`, `#/components/schemas/Note` → `Note`.
fn type_of(schema: &Value) -> String {
    if let Some(r) = str_at(schema, "$ref") {
        return r.rsplit('/').next().unwrap_or(r).to_string();
    }
    let nullable = schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|xs| xs.iter().any(|x| str_at(x, "type") == Some("null")));
    if let Some(xs) = schema.get("anyOf").and_then(Value::as_array) {
        if let Some(inner) = xs.iter().find(|x| str_at(x, "type") != Some("null")) {
            return format!("{}{}", type_of(inner), if nullable { "?" } else { "" });
        }
    }
    match str_at(schema, "type") {
        Some("array") => match schema.get("items") {
            Some(items) => format!("{}[]", type_of(items)),
            None => "array".into(),
        },
        Some(t) => match str_at(schema, "format") {
            Some(f) => format!("{t} ({f})"),
            None => t.to_string(),
        },
        None => "object".into(),
    }
}

fn schema_rows(schema: &Value, out: &mut String) {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|xs| xs.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    out.push_str("<table class=\"fields\"><thead><tr><th>Field</th><th>Type</th><th></th></tr></thead><tbody>");
    for (name, sub) in props {
        let req = if required.contains(&name.as_str()) {
            "<span class=\"req\">required</span>"
        } else {
            ""
        };
        let _ = write!(
            out,
            "<tr><td><code>{}</code></td><td class=\"ty\">{}</td><td>{req}</td></tr>",
            esc(name),
            esc(&type_of(sub))
        );
    }
    out.push_str("</tbody></table>");
}

/// Render the whole document as one self-contained HTML page.
pub fn render(doc: &Value) -> String {
    let title = doc
        .pointer("/info/title")
        .and_then(Value::as_str)
        .unwrap_or("API");
    let version = doc
        .pointer("/info/version")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut nav = String::new();
    let mut body = String::new();
    let mut n = 0usize;

    if let Some(paths) = doc.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            for method in METHODS {
                let Some(op) = item.get(*method) else {
                    continue;
                };
                n += 1;
                let id = format!("op-{n}");
                let summary = str_at(op, "summary").unwrap_or("");
                let up = method.to_uppercase();

                let _ = write!(
                    nav,
                    "<a href=\"#{id}\"><span class=\"m m-{method}\">{up}</span>{}</a>",
                    esc(path)
                );

                let _ = write!(
                    body,
                    "<section id=\"{id}\"><h2><span class=\"m m-{method}\">{up}</span><code>{}</code></h2>",
                    esc(path)
                );
                if !summary.is_empty() {
                    let _ = write!(body, "<p class=\"summary\">{}</p>", esc(summary));
                }
                if let Some(d) = str_at(op, "description") {
                    let _ = write!(body, "<p class=\"desc\">{}</p>", esc(d));
                }

                render_parameters(op, &mut body);
                render_request_body(op, doc, &mut body);
                render_responses(op, &mut body);
                body.push_str("</section>");
            }
        }
    }

    render_schemas(doc, &mut body);

    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{t} — API</title><style>{css}</style></head><body>\
         <header><h1>{t}</h1><p class=\"v\">{v}{sep}{n} operation{plural}</p>\
         <p class=\"v\"><a href=\"openapi.json\">openapi.json</a></p></header>\
         <div class=\"wrap\"><nav>{nav}</nav><main>{body}</main></div></body></html>\n",
        t = esc(title),
        v = esc(version),
        sep = if version.is_empty() { "" } else { " · " },
        plural = if n == 1 { "" } else { "s" },
        css = CSS,
    )
}

fn render_parameters(op: &Value, out: &mut String) {
    let Some(params) = op.get("parameters").and_then(Value::as_array) else {
        return;
    };
    if params.is_empty() {
        return;
    }
    out.push_str("<h3>Parameters</h3><table class=\"fields\"><thead><tr><th>Name</th><th>In</th><th>Type</th><th></th></tr></thead><tbody>");
    for p in params {
        let req = if p.get("required").and_then(Value::as_bool) == Some(true) {
            "<span class=\"req\">required</span>"
        } else {
            ""
        };
        let _ = write!(
            out,
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"ty\">{}</td><td>{req}</td></tr>",
            esc(str_at(p, "name").unwrap_or("")),
            esc(str_at(p, "in").unwrap_or("")),
            esc(&type_of(p.get("schema").unwrap_or(&Value::Null)))
        );
    }
    out.push_str("</tbody></table>");
}

fn render_request_body(op: &Value, doc: &Value, out: &mut String) {
    let Some(schema) = op.pointer("/requestBody/content/application~1json/schema") else {
        return;
    };
    out.push_str("<h3>Request body</h3>");
    let resolved = resolve(schema, doc);
    if let Some(name) =
        str_at(schema, "$ref").map(|r| r.rsplit('/').next().unwrap_or(r).to_string())
    {
        let _ = write!(out, "<p class=\"ref\"><code>{}</code></p>", esc(&name));
    }
    schema_rows(&resolved, out);
}

fn render_responses(op: &Value, out: &mut String) {
    let Some(responses) = op.get("responses").and_then(Value::as_object) else {
        return;
    };
    out.push_str("<h3>Responses</h3><table class=\"fields\"><thead><tr><th>Status</th><th></th></tr></thead><tbody>");
    for (code, r) in responses {
        let cls = match code.chars().next() {
            Some('2') => "ok",
            Some('4') => "warn",
            Some('5') => "bad",
            _ => "",
        };
        let _ = write!(
            out,
            "<tr><td><span class=\"st {cls}\">{}</span></td><td>{}</td></tr>",
            esc(code),
            esc(str_at(r, "description").unwrap_or(""))
        );
    }
    out.push_str("</tbody></table>");
}

fn render_schemas(doc: &Value, out: &mut String) {
    let Some(schemas) = doc
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    else {
        return;
    };
    if schemas.is_empty() {
        return;
    }
    out.push_str("<section id=\"schemas\"><h2>Schemas</h2>");
    for (name, schema) in schemas {
        let _ = write!(out, "<h3 id=\"schema-{}\">{}</h3>", esc(name), esc(name));
        schema_rows(schema, out);
    }
    out.push_str("</section>");
}

/// One level of `$ref` resolution — enough for the shapes `openapi.rs`
/// emits, which never nest a `$ref` inside a `$ref`.
fn resolve(schema: &Value, doc: &Value) -> Value {
    match str_at(schema, "$ref") {
        Some(r) => {
            let pointer = r.trim_start_matches('#');
            doc.pointer(pointer).cloned().unwrap_or(Value::Null)
        }
        None => schema.clone(),
    }
}

const CSS: &str = "\
:root{--bg:#fff;--fg:#1b1b1f;--mut:#5b5b66;--line:#e3e3e8;--card:#fafafc;--acc:#3358d4}\
@media (prefers-color-scheme:dark){:root{--bg:#16161a;--fg:#eceef2;--mut:#9a9aa6;--line:#2a2a31;--card:#1d1d22;--acc:#7d97f4}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 ui-sans-serif,system-ui,-apple-system,Segoe UI,Roboto,sans-serif}\
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.92em}\
header{padding:28px 32px;border-bottom:1px solid var(--line)}\
header h1{margin:0 0 4px;font-size:22px}\
.v{margin:0;color:var(--mut);font-size:13px}\
.v a{color:var(--acc)}\
.wrap{display:flex;align-items:flex-start}\
nav{position:sticky;top:0;flex:0 0 300px;max-height:100vh;overflow:auto;padding:20px 12px;border-right:1px solid var(--line)}\
nav a{display:flex;gap:8px;align-items:center;padding:5px 8px;border-radius:6px;color:var(--fg);text-decoration:none;font-size:13px}\
nav a:hover{background:var(--card)}\
main{flex:1;min-width:0;padding:20px 32px 80px}\
section{padding:18px 0;border-bottom:1px solid var(--line)}\
h2{display:flex;gap:10px;align-items:center;font-size:16px;margin:0 0 8px}\
h3{font-size:13px;text-transform:uppercase;letter-spacing:.04em;color:var(--mut);margin:18px 0 6px}\
.summary{margin:0 0 4px}\
.desc,.ref{margin:0 0 8px;color:var(--mut);font-size:13px}\
.m{display:inline-block;min-width:58px;text-align:center;padding:2px 6px;border-radius:5px;font-size:11px;font-weight:700;letter-spacing:.03em;color:#fff}\
.m-get{background:#2f7d4f}.m-post{background:#3358d4}.m-put{background:#8a6d1f}\
.m-patch{background:#8a6d1f}.m-delete{background:#b03535}.m-head,.m-options{background:#5b5b66}\
table.fields{width:100%;border-collapse:collapse;margin:2px 0 6px}\
table.fields th{text-align:left;font-size:11px;text-transform:uppercase;letter-spacing:.04em;color:var(--mut);border-bottom:1px solid var(--line);padding:5px 8px}\
table.fields td{padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}\
.ty{color:var(--mut);font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:12px}\
.req{font-size:11px;color:#b03535}\
.st{display:inline-block;min-width:36px;padding:1px 6px;border-radius:4px;font-size:12px;font-weight:600;background:var(--card)}\
.st.ok{color:#2f7d4f}.st.warn{color:#8a6d1f}.st.bad{color:#b03535}\
@media(max-width:820px){.wrap{display:block}nav{position:static;max-height:none;width:100%;border-right:0;border-bottom:1px solid var(--line)}main{padding:16px}}\
";

/// Serve the page at `/` and the document at `/openapi.json`.
///
/// Bound to loopback: this is a developer's local reference, and a
/// listener on `0.0.0.0` would put an unauthenticated description of every
/// endpoint on whatever network the machine is attached to.
pub async fn serve(doc: Value, port: u16) -> Result<()> {
    use axum::response::{Html, IntoResponse};
    use axum::routing::get;

    let html = render(&doc);
    let json = format!("{}\n", serde_json::to_string_pretty(&doc)?);

    let app = axum::Router::new()
        .route("/", get(move || async move { Html(html) }))
        .route(
            "/openapi.json",
            get(move || async move {
                ([("content-type", "application/json; charset=utf-8")], json).into_response()
            }),
        );

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("API reference on http://{addr}  (Ctrl-C to stop)");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "openapi": "3.1.0",
            "info": { "title": "Shop", "version": "1.0.0" },
            "paths": {
                "/notes/{id}": {
                    "get": {
                        "summary": "One note",
                        "parameters": [
                            { "name": "id", "in": "path", "required": true,
                              "schema": { "type": "integer", "format": "int64" } }
                        ],
                        "responses": {
                            "200": { "description": "ok" },
                            "404": { "description": "bunday eslatma yo'q" }
                        }
                    }
                },
                "/notes": {
                    "post": {
                        "requestBody": { "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/NoteCreate" } } } },
                        "responses": { "201": { "description": "created" } }
                    }
                }
            },
            "components": { "schemas": { "NoteCreate": {
                "type": "object",
                "required": ["title"],
                "properties": {
                    "title": { "type": "string" },
                    "tags":  { "type": "array", "items": { "type": "string" } },
                    "body":  { "anyOf": [{ "type": "string" }, { "type": "null" }] }
                }
            } } }
        })
    }

    #[test]
    fn every_operation_reaches_the_page() {
        let html = render(&doc());
        assert!(html.contains("GET"), "{html}");
        assert!(html.contains("POST"), "{html}");
        assert!(html.contains("/notes/{id}"), "{html}");
        assert!(html.contains("One note"), "{html}");
        assert!(html.contains("2 operations"), "{html}");
    }

    #[test]
    fn a_ref_body_is_expanded_rather_than_printed_as_a_pointer() {
        let html = render(&doc());
        assert!(html.contains("NoteCreate"), "{html}");
        // The fields of the referenced schema, not the `$ref` string.
        assert!(html.contains("title"), "{html}");
        assert!(html.contains("required"), "{html}");
        assert!(
            !html.contains("#/components/schemas"),
            "a raw pointer leaked into the page: {html}"
        );
    }

    #[test]
    fn types_render_readably() {
        assert_eq!(type_of(&json!({ "type": "string" })), "string");
        assert_eq!(
            type_of(&json!({ "type": "integer", "format": "int64" })),
            "integer (int64)"
        );
        assert_eq!(
            type_of(&json!({ "type": "array", "items": { "type": "string" } })),
            "string[]"
        );
        assert_eq!(
            type_of(&json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })),
            "string?"
        );
        assert_eq!(
            type_of(&json!({ "$ref": "#/components/schemas/Note" })),
            "Note"
        );
    }

    /// The page is one file. A CDN reference would leave it blank offline,
    /// which is where a developer most often reads it.
    #[test]
    fn the_page_fetches_nothing() {
        let html = render(&doc());
        for needle in ["http://", "https://", "<script", "src="] {
            assert!(
                !html.contains(needle),
                "`{needle}` in a page that must be self-contained"
            );
        }
        // …except the sibling document, which this command also serves.
        assert!(html.contains("href=\"openapi.json\""), "{html}");
    }

    /// Summaries and descriptions come from user source, so they are
    /// attacker-influenced in exactly the way a doc page is not expected
    /// to be — but they are still program text, and program text with a
    /// `<` in it must not become markup.
    #[test]
    fn text_from_the_program_is_escaped() {
        let mut d = doc();
        d["paths"]["/notes"]["post"]["summary"] =
            json!("<img src=x onerror=alert(1)> & \"quoted\"");
        let html = render(&d);
        assert!(!html.contains("<img src=x"), "{html}");
        assert!(html.contains("&lt;img src=x"), "{html}");
        assert!(html.contains("&amp;"), "{html}");
    }
}
