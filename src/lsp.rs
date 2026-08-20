//! `jwc lsp` — the language server, over stdio.
//!
//! JSON-RPC is hand-written for the same reason the lexer and parser are:
//! the protocol is a length-prefixed header and a JSON object, and a
//! dependency that owns the event loop also owns when the compiler runs.
//!
//! ## One compiler, one answer
//!
//! Every request re-runs the pipeline over the open documents. There is no
//! incremental cache and no half-built model held between calls — a
//! language server that caches one is a language server that reports a
//! diagnostic `jwc check` does not (tooling.md §6.3). The pipeline is fast
//! enough that the whole test suite runs it hundreds of times a second.
//!
//! Hover over a query prints the same SQL `jwc explain` prints for that
//! site, because it calls the same compiler (§6.4).

use crate::diag::{Diagnostic, Severity};
use crate::workspace::{Loc, Workspace};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;
// `read_exact` arrives with `BufRead`, which requires `Read`.
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut server = Server::default();

    while let Some(msg) = read_message(&mut input)? {
        let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
            continue;
        };
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                server.root = root_of(&params);
                reply(&mut output, id, server.capabilities())?;
            }
            "shutdown" => reply(&mut output, id, Value::Null)?,
            "exit" => break,
            "textDocument/didOpen" | "textDocument/didChange" => {
                if let Some((path, text)) = server.apply_change(method, &params) {
                    let diags = server.diagnostics(&path);
                    notify(
                        &mut output,
                        "textDocument/publishDiagnostics",
                        json!({ "uri": to_uri(&path), "diagnostics": diags }),
                    )?;
                    let _ = text;
                }
            }
            "textDocument/didClose" => {
                if let Some(path) = document_path(&params) {
                    server.docs.remove(&path);
                }
            }
            "textDocument/hover" => {
                let r = server.hover(&params);
                reply(&mut output, id, r)?;
            }
            "textDocument/definition" => {
                let r = server.definition(&params);
                reply(&mut output, id, r)?;
            }
            "textDocument/completion" => {
                let r = server.completion(&params);
                reply(&mut output, id, r)?;
            }
            "textDocument/signatureHelp" => {
                let r = server.signature_help(&params);
                reply(&mut output, id, r)?;
            }
            _ => {
                // A request with an id must be answered even when it is not
                // supported, or the client waits forever.
                if id.is_some() {
                    reply(&mut output, id, Value::Null)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct Server {
    pub root: PathBuf,
    /// The editor's text, which by definition is not the file yet.
    pub docs: BTreeMap<PathBuf, String>,
}

impl Server {
    fn capabilities(&self) -> Value {
        json!({
            "capabilities": {
                // Full sync: the documents are small and a range update is
                // one more thing to get wrong for no measurable gain.
                "textDocumentSync": 1,
                "hoverProvider": true,
                "definitionProvider": true,
                "completionProvider": { "triggerCharacters": ["."] },
                "signatureHelpProvider": { "triggerCharacters": ["(", ","] },
            },
            "serverInfo": { "name": "jwc", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn apply_change(&mut self, method: &str, params: &Value) -> Option<(PathBuf, String)> {
        let path = document_path(params)?;
        let text = if method == "textDocument/didOpen" {
            params["textDocument"]["text"].as_str()?.to_string()
        } else {
            // Full sync, so the last change carries the whole document.
            params["contentChanges"]
                .as_array()?
                .last()?
                .get("text")?
                .as_str()?
                .to_string()
        };
        self.docs.insert(path.clone(), text.clone());
        Some((path, text))
    }

    /// The project root: the `initialize` rootUri, or the directory of the
    /// first open document.
    fn analysis_root(&self) -> PathBuf {
        if self.root.as_os_str().is_empty() {
            self.docs
                .keys()
                .next()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."))
        } else {
            self.root.clone()
        }
    }

    pub fn analyze(&self) -> Option<Analysis> {
        let ws = Workspace::load_with(self.analysis_root(), &self.docs).ok()?;
        let built = crate::model::build(&ws);
        let sym = crate::symbols::build(&ws, &built.model);
        let checked = crate::check::check(&ws, &sym, &built.model);
        let wired = crate::wiring::wire(&ws, &sym);
        let mut imports = crate::imports::check(&ws, &ws.packages);
        imports.extend(crate::imports::case_convention(&ws));
        imports.extend(crate::packages::check(&ws, &sym));
        Some(Analysis {
            diags: built
                .diags
                .into_iter()
                .chain(sym.diags.clone())
                .chain(checked.diags)
                .chain(wired.diags)
                .chain(imports)
                .collect(),
            ws,
            model: built.model,
            sym,
        })
    }

    /// Everything `jwc check` would report for one file.
    pub fn diagnostics(&self, path: &Path) -> Vec<Value> {
        let Some(a) = self.analyze() else {
            return Vec::new();
        };
        let Some(index) = a.file_index(path) else {
            return Vec::new();
        };
        let text = &a.ws.files[index].source.text;
        let mut out = Vec::new();

        // Parse diagnostics live on the file; everything later carries a
        // `Loc` naming which file it belongs to.
        for d in &a.ws.files[index].diags {
            out.push(lsp_diagnostic(text, d));
        }
        for (loc, d) in &a.diags {
            if loc.file == index {
                out.push(lsp_diagnostic(text, d));
            }
        }
        out
    }

    fn hover(&self, params: &Value) -> Value {
        let Some((a, index, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let text = &a.ws.files[index].source.text;

        // A query first: it is the answer worth having, and it is the one
        // `jwc explain` gives for the same site (tooling.md §6.4).
        if let Some((span, sql)) = a.sql_at(index, offset) {
            return json!({
                "contents": { "kind": "markdown", "value": format!("```sql\n{sql}\n```") },
                "range": range_of(text, span.start, span.end),
            });
        }
        let Some((word, start, end)) = word_at(text, offset) else {
            return Value::Null;
        };
        let Some(summary) = a.describe(&word) else {
            return Value::Null;
        };
        json!({
            "contents": { "kind": "markdown", "value": summary },
            "range": range_of(text, start as u32, end as u32),
        })
    }

    fn definition(&self, params: &Value) -> Value {
        let Some((a, index, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let text = &a.ws.files[index].source.text;
        let Some((word, _, _)) = word_at(text, offset) else {
            return Value::Null;
        };
        let Some(loc) = a.declaration(&word) else {
            return Value::Null;
        };
        let Some(file) = a.ws.files.get(loc.file) else {
            return Value::Null;
        };
        json!({
            "uri": to_uri(&file.source.path),
            "range": range_of(&file.source.text, loc.span.start, loc.span.end),
        })
    }

    fn completion(&self, params: &Value) -> Value {
        let Some((a, index, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let text = &a.ws.files[index].source.text;
        let items = match base_before_dot(text, offset) {
            Some(base) => a.members(&base),
            None => a.visible_names(),
        };
        json!(items
            .into_iter()
            .map(|(label, kind, detail)| json!({
                "label": label,
                "kind": kind,
                "detail": detail,
            }))
            .collect::<Vec<_>>())
    }

    fn signature_help(&self, params: &Value) -> Value {
        let Some((a, index, offset)) = self.locate(params) else {
            return Value::Null;
        };
        let text = &a.ws.files[index].source.text;
        let Some((callee, active)) = enclosing_call(text, offset) else {
            return Value::Null;
        };
        // A call written inside its own service omits the service name.
        // A suffix match is exact when only one function answers to it, and
        // ambiguity is not worth guessing through.
        let f = match a.sym.functions.get(&callee) {
            Some(f) => f,
            None => {
                let suffix = format!(".{callee}");
                let mut hits = a.sym.functions.iter().filter(|(k, _)| k.ends_with(&suffix));
                match (hits.next(), hits.next()) {
                    (Some((_, f)), None) => f,
                    _ => return Value::Null,
                }
            }
        };
        let params_text: Vec<String> = f.params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
        let label = format!(
            "{}({}){}",
            f.qualified(),
            params_text.join(", "),
            match &f.returns {
                Some(t) => format!(" -> {t}"),
                None => String::new(),
            }
        );
        json!({
            "signatures": [{
                "label": label,
                "parameters": params_text
                    .iter()
                    .map(|p| json!({ "label": p }))
                    .collect::<Vec<_>>(),
            }],
            "activeSignature": 0,
            "activeParameter": active,
        })
    }

    /// The analysis, the file's index in it, and the byte offset the
    /// position names.
    fn locate(&self, params: &Value) -> Option<(Analysis, usize, usize)> {
        let path = document_path(params)?;
        let a = self.analyze()?;
        let index = a.file_index(&path)?;
        let line = params["position"]["line"].as_u64()? as usize;
        let character = params["position"]["character"].as_u64()? as usize;
        let offset = offset_at(&a.ws.files[index].source.text, line, character)?;
        Some((a, index, offset))
    }
}

pub struct Analysis {
    pub ws: Workspace,
    pub model: crate::model::SchemaModel,
    pub sym: crate::symbols::Symbols,
    pub diags: Vec<(Loc, Diagnostic)>,
}

impl Analysis {
    pub fn file_index(&self, path: &Path) -> Option<usize> {
        self.ws.files.iter().position(|f| f.source.path == path)
    }

    /// The generated SQL for the query under `offset`, and its span.
    ///
    /// The innermost enclosing site wins, so hovering inside a subquery
    /// answers about the subquery.
    pub fn sql_at(&self, index: usize, offset: usize) -> Option<(crate::token::Span, String)> {
        let file = self.ws.files.get(index)?;
        let mut best: Option<&crate::query_sql::Site> = None;
        let sites = crate::query_sql::sites(&file.program);
        for site in &sites {
            let span = site.select.span;
            if offset < span.start as usize || offset > span.end as usize {
                continue;
            }
            if best.is_none_or(|b| span.end - span.start < b.select.span.end - b.select.span.start)
            {
                best = Some(site);
            }
        }
        let site = best?;
        let plan = crate::query::plan(site.select, &self.sym);
        if let Some(d) = plan.diags.iter().find(|d| d.severity == Severity::Error) {
            return Some((site.select.span, format!("-- {} {}", d.code, d.message)));
        }
        let mut c = crate::query_sql::Compiler::new(&self.model);
        let sql = match c.compile(site.select, &plan) {
            Some(compiled) => compiled.sql,
            None => format!("-- not compilable: {}", c.gap()),
        };
        Some((site.select.span, sql))
    }

    /// A markdown summary of a declared name.
    pub fn describe(&self, name: &str) -> Option<String> {
        if let Some(t) = self.sym.tables.get(name) {
            let cols: Vec<String> = t
                .columns
                .iter()
                .map(|(n, ty)| format!("    {n} {ty}"))
                .collect();
            return Some(format!(
                "**table** `{}` of `{}`\n```\n{}\n```",
                t.declared,
                t.schema,
                cols.join("\n")
            ));
        }
        if let Some(v) = self.sym.views.get(name) {
            return Some(format!(
                "**view** `{}` of `{}` over `{}`",
                v.declared, v.schema, v.driving_table
            ));
        }
        if let Some(c) = self.sym.classes.get(name) {
            let fields: Vec<String> = c
                .fields
                .iter()
                .map(|f| format!("    {} {}", f.name, f.ty))
                .collect();
            return Some(format!(
                "**class** `{}`\n```\n{}\n```",
                c.declared,
                fields.join("\n")
            ));
        }
        if let Some(e) = self.sym.enums.get(name) {
            return Some(format!(
                "**enum** `{}` — {}",
                e.declared,
                e.members.join(", ")
            ));
        }
        if let Some(e) = self.sym.errors.get(name) {
            let params: Vec<String> = e.params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
            return Some(format!(
                "**error** `{}({})` → {}{}",
                e.declared,
                params.join(", "),
                e.status,
                if e.predeclared { " (predeclared)" } else { "" }
            ));
        }
        if let Some(f) = self.sym.functions.get(name) {
            return Some(function_summary(f));
        }
        if let Some(m) = self.sym.middleware.get(name) {
            let provides: Vec<String> = m
                .provides
                .iter()
                .map(|(n, t)| format!("{n}: {t}"))
                .collect();
            return Some(format!(
                "**middleware** `{}`{}{}",
                m.name,
                if m.requires.is_empty() {
                    String::new()
                } else {
                    format!(" requires {}", m.requires.join(", "))
                },
                if provides.is_empty() {
                    String::new()
                } else {
                    format!(" provides {}", provides.join(", "))
                }
            ));
        }
        if let Some(names) = self.sym.services.get(name) {
            return Some(format!("**service** `{name}` — {}", names.join(", ")));
        }
        None
    }

    pub fn declaration(&self, name: &str) -> Option<Loc> {
        if let Some(t) = self.sym.tables.get(name) {
            return Some(t.loc);
        }
        if let Some(v) = self.sym.views.get(name) {
            return Some(v.loc);
        }
        if let Some(c) = self.sym.classes.get(name) {
            return Some(c.loc);
        }
        if let Some(e) = self.sym.enums.get(name) {
            return Some(e.loc);
        }
        if let Some(e) = self.sym.errors.get(name) {
            // A predeclared error has no declaration to jump to
            // (errors.md §1.2).
            return (!e.predeclared).then_some(e.loc);
        }
        if let Some(f) = self.sym.functions.get(name) {
            return Some(f.loc);
        }
        if let Some(m) = self.sym.middleware.get(name) {
            return Some(m.loc);
        }
        None
    }

    /// What `<base>.` offers. Tables and views answer with their columns;
    /// a service with its functions; a builtin namespace with its surface.
    pub fn members(&self, base: &str) -> Vec<(String, u32, String)> {
        const FIELD: u32 = 5;
        const METHOD: u32 = 2;
        const FUNCTION: u32 = 3;

        if let Some(t) = self.sym.tables.get(base) {
            return t
                .columns
                .iter()
                .filter(|(n, _)| !t.is_private(n))
                .map(|(n, ty)| (n.clone(), FIELD, ty.to_string()))
                .collect();
        }
        if let Some(v) = self.sym.views.get(base) {
            return v
                .shape
                .iter()
                .map(|(n, ty)| (n.clone(), FIELD, ty.to_string()))
                .collect();
        }
        if let Some(c) = self.sym.classes.get(base) {
            return c
                .fields
                .iter()
                .map(|f| (f.name.clone(), FIELD, f.ty.to_string()))
                .collect();
        }
        if let Some(e) = self.sym.enums.get(base) {
            return e
                .members
                .iter()
                .map(|m| (m.clone(), FIELD, base.to_string()))
                .collect();
        }
        if let Some(names) = self.sym.services.get(base) {
            return names
                .iter()
                .filter_map(|n| self.sym.functions.get(&format!("{base}.{n}")))
                .map(|f| (f.name.clone(), METHOD, function_summary(f)))
                .collect();
        }
        namespace_members(base)
            .iter()
            .map(|(n, d)| ((*n).to_string(), FUNCTION, (*d).to_string()))
            .collect()
    }

    /// Everything nameable at statement position.
    pub fn visible_names(&self) -> Vec<(String, u32, String)> {
        const CLASS: u32 = 7;
        const ENUM: u32 = 13;
        const MODULE: u32 = 9;
        const FUNCTION: u32 = 3;
        let mut out: Vec<(String, u32, String)> = Vec::new();
        for (name, t) in &self.sym.tables {
            out.push((name.clone(), CLASS, format!("table of {}", t.schema)));
        }
        for (name, v) in &self.sym.views {
            out.push((name.clone(), CLASS, format!("view of {}", v.schema)));
        }
        for name in self.sym.classes.keys() {
            out.push((name.clone(), CLASS, "class".into()));
        }
        for name in self.sym.enums.keys() {
            out.push((name.clone(), ENUM, "enum".into()));
        }
        for (name, e) in &self.sym.errors {
            out.push((name.clone(), CLASS, format!("error → {}", e.status)));
        }
        for name in self.sym.services.keys() {
            out.push((name.clone(), MODULE, "service".into()));
        }
        for (name, f) in &self.sym.functions {
            if f.service.is_none() {
                out.push((name.clone(), FUNCTION, function_summary(f)));
            }
        }
        for name in self.sym.middleware.keys() {
            out.push((name.clone(), MODULE, "middleware".into()));
        }
        for ns in NAMESPACES {
            out.push(((*ns).to_string(), MODULE, "builtin namespace".into()));
        }
        out.sort();
        out
    }
}

fn function_summary(f: &crate::symbols::FunctionSym) -> String {
    let params: Vec<String> = f.params.iter().map(|(n, t)| format!("{n}: {t}")).collect();
    format!(
        "**function** `{}({})`{}{}",
        f.qualified(),
        params.join(", "),
        match &f.returns {
            Some(t) => format!(" -> {t}"),
            None => String::new(),
        },
        if f.raises.is_empty() {
            String::new()
        } else {
            format!(" raises {}", f.raises.join(", "))
        }
    )
}

const NAMESPACES: &[&str] = &[
    "date", "string", "json", "hash", "crypto", "jwt", "request", "response", "context", "debug",
];

/// The builtin surface of a namespace (builtins.md). Hand-listed rather
/// than derived, because the checker's arms are a `match` and not a table —
/// and a wrong completion is cheaper to fix than a wrong dispatch.
fn namespace_members(base: &str) -> &'static [(&'static str, &'static str)] {
    match base {
        "date" => &[
            ("now", "() -> timestamptz"),
            ("today", "() -> date"),
            ("days", "(n) -> interval"),
            ("hours", "(n) -> interval"),
            ("minutes", "(n) -> interval"),
            ("seconds", "(n) -> interval"),
            ("add", "(t, i) -> timestamptz"),
            ("parse", "(s) -> timestamptz"),
            ("format", "(t, f) -> text"),
        ],
        "string" => &[
            ("of", "(x) -> text"),
            ("len", "(s) -> int"),
            ("lower", "(s) -> text"),
            ("upper", "(s) -> text"),
            ("trim", "(s) -> text"),
            ("split", "(s, sep) -> text[]"),
            ("join", "(xs, sep) -> text"),
            ("replace", "(s, a, b) -> text"),
            ("starts_with", "(s, p) -> boolean"),
            ("contains", "(s, p) -> boolean"),
        ],
        "debug" => &[("dump", "(x) -> the type of x — `jwc serve --dev` only")],
        "request" => &[
            ("body", "() -> validated with `as C`"),
            ("header", "(k) -> text?"),
            ("query", "(k) -> text?"),
            ("query_all", "(k) -> text[]"),
            ("method", "() -> text"),
            ("path", "() -> text"),
            ("route", "() -> text — the declared pattern"),
            ("peer_ip", "() -> inet"),
            ("client_ip", "() -> inet"),
            ("id", "() -> text"),
        ],
        "response" => &[
            ("status", "() -> int — `after` only"),
            ("set_header", "(k, v) -> Void — `after` only"),
            ("add_header", "(k, v) -> Void — `after` only"),
        ],
        _ => &[],
    }
}

// ── text positions ─────────────────────────────────────────────────────

/// LSP counts characters in **UTF-16 code units**, and the sample is full
/// of Uzbek text where that differs from both bytes and chars.
fn offset_at(text: &str, line: usize, character: usize) -> Option<usize> {
    let mut offset = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let mut units = 0usize;
            for (byte, c) in l.char_indices() {
                if units >= character {
                    return Some(offset + byte);
                }
                units += c.len_utf16();
            }
            return Some(offset + l.trim_end_matches('\n').len());
        }
        offset += l.len();
    }
    Some(text.len())
}

fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut start = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if start + l.len() > offset {
            line = i;
            break;
        }
        start += l.len();
        line = i + 1;
    }
    let character = text[start..offset.min(text.len())]
        .chars()
        .map(|c| c.len_utf16())
        .sum();
    (line, character)
}

fn range_of(text: &str, start: u32, end: u32) -> Value {
    let (sl, sc) = position_of(text, start as usize);
    let (el, ec) = position_of(text, end as usize);
    json!({
        "start": { "line": sl, "character": sc },
        "end": { "line": el, "character": ec },
    })
}

fn lsp_diagnostic(text: &str, d: &Diagnostic) -> Value {
    let mut message = d.message.clone();
    if let Some(n) = &d.note {
        message.push_str("\n\n");
        message.push_str(n);
    }
    if let Some(c) = d.clause {
        message.push_str(&format!("\n\nspec: {c}"));
    }
    json!({
        "range": range_of(text, d.span.start, d.span.end),
        "severity": if d.severity == Severity::Error { 1 } else { 2 },
        "code": d.code,
        "source": "jwc",
        "message": message,
    })
}

/// The identifier under `offset`, and its bounds.
fn word_at(text: &str, offset: usize) -> Option<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if offset > bytes.len() {
        return None;
    }
    let mut start = offset.min(bytes.len().saturating_sub(1));
    if start < bytes.len() && !ident(bytes[start]) && start > 0 {
        start -= 1;
    }
    if start >= bytes.len() || !ident(bytes[start]) {
        return None;
    }
    while start > 0 && ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = start;
    while end < bytes.len() && ident(bytes[end]) {
        end += 1;
    }
    Some((text[start..end].to_string(), start, end))
}

/// The name immediately before a `.` at `offset`, for member completion.
fn base_before_dot(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = offset.min(bytes.len());
    // Skip the partial identifier the user is typing.
    while i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    let dot = i - 1;
    let (word, _, _) = word_at(text, dot.checked_sub(1)?)?;
    Some(word)
}

/// The callee of the call `offset` sits inside, and which argument it is on.
fn enclosing_call(text: &str, offset: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut i = offset.min(bytes.len());
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b',' if depth == 0 => commas += 1,
            b'(' => {
                if depth == 0 {
                    // The dotted path immediately before the paren.
                    let mut start = i;
                    while start > 0 {
                        let b = bytes[start - 1];
                        if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
                            start -= 1;
                        } else {
                            break;
                        }
                    }
                    if start == i {
                        return None;
                    }
                    return Some((text[start..i].to_string(), commas));
                }
                depth -= 1;
            }
            b'\n' if depth == 0 && commas == 0 && i > 0 && bytes[i - 1] == b'}' => return None,
            _ => {}
        }
    }
    None
}

// ── the wire ───────────────────────────────────────────────────────────

fn read_message(input: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok();
        }
    }
    let Some(n) = length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; n];
    input.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf).ok())
}

fn write_message(out: &mut impl Write, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    out.flush()?;
    Ok(())
}

fn reply(out: &mut impl Write, id: Option<Value>, result: Value) -> Result<()> {
    let Some(id) = id else { return Ok(()) };
    write_message(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    )
}

fn notify(out: &mut impl Write, method: &str, params: Value) -> Result<()> {
    write_message(
        out,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

fn root_of(params: &Value) -> PathBuf {
    params["rootUri"]
        .as_str()
        .and_then(from_uri)
        .or_else(|| params["rootPath"].as_str().map(PathBuf::from))
        .unwrap_or_default()
}

fn document_path(params: &Value) -> Option<PathBuf> {
    from_uri(params["textDocument"]["uri"].as_str()?)
}

pub fn to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

pub fn from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `%20` and friends. Nothing else in a path needs decoding for this to
    // work on the platforms the compiler builds for.
    let mut out = String::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Some(PathBuf::from(out))
}
