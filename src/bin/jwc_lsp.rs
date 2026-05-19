//! JWC Language Server.
//!
//! Speaks LSP over stdio. On every textDocument open/change/save it runs
//! `jwc::parser::parse_program` + `jwc::parser::validate_program` +
//! `jwc::lint::lint_program` and pushes the diagnostics to the editor.
//!
//! Capabilities advertised:
//!   * textDocument/didOpen, didChange (full sync), didSave
//!   * textDocument/hover  — surfaces a one-line summary when the cursor is on
//!     a top-level `entity` / `class` / `function` identifier.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use jwc::ast::{ModelKind, Program};
use jwc::lint::lint_program;
use jwc::parser::{parse_program, validate_program};

#[derive(Debug)]
struct Backend {
    client: Client,
    /// Latest text we've seen for each open document, keyed by URI.
    documents: Arc<RwLock<HashMap<Url, String>>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Parse + validate + lint the given source and translate the results into
    /// LSP `Diagnostic` values.
    fn compute_diagnostics(source: &str) -> Vec<Diagnostic> {
        let mut diags = Vec::new();

        let program = match parse_program(source) {
            Ok(p) => p,
            Err(err) => {
                let msg = err.to_string();
                let range = extract_line_col(&msg).unwrap_or_else(zero_range);
                diags.push(Diagnostic {
                    range,
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: Some(NumberOrString::String("parse".into())),
                    code_description: None,
                    source: Some("jwc".into()),
                    message: msg,
                    related_information: None,
                    tags: None,
                    data: None,
                });
                return diags;
            }
        };

        if let Err(err) = validate_program(&program) {
            // validate_program errors don't carry line/col, so anchor them at
            // the top of the file.
            diags.push(Diagnostic {
                range: zero_range(),
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("validate".into())),
                code_description: None,
                source: Some("jwc".into()),
                message: err.to_string(),
                related_information: None,
                tags: None,
                data: None,
            });
            return diags;
        }

        for w in lint_program(&program) {
            diags.push(Diagnostic {
                range: zero_range(),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String(w.code.to_string())),
                code_description: None,
                source: Some("jwc".into()),
                message: w.message,
                related_information: None,
                tags: None,
                data: None,
            });
        }

        diags
    }

    async fn analyze_and_publish(&self, uri: Url, text: String, version: Option<i32>) {
        let diags = Self::compute_diagnostics(&text);
        {
            let mut docs = self.documents.write().await;
            docs.insert(uri.clone(), text);
        }
        self.client.publish_diagnostics(uri, diags, version).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "jwc-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "jwc-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = Some(params.text_document.version);
        self.analyze_and_publish(uri, text, version).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = Some(params.text_document.version);
        // We advertise FULL sync, so each change carries the entire document
        // in `content_changes[0].text`.
        let text = params
            .content_changes
            .into_iter()
            .last()
            .map(|c| c.text)
            .unwrap_or_default();
        self.analyze_and_publish(uri, text, version).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        // `text` only arrives when client opts in via SaveOptions; fall back to
        // whatever we cached during the last didChange.
        let text = match params.text {
            Some(t) => t,
            None => match self.documents.read().await.get(&uri) {
                Some(t) => t.clone(),
                None => return,
            },
        };
        self.analyze_and_publish(uri, text, None).await;
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let text = match self.documents.read().await.get(&uri).cloned() {
            Some(t) => t,
            None => return Ok(None),
        };

        let ident = match identifier_at(&text, position) {
            Some(name) => name,
            None => return Ok(None),
        };

        // Re-parse on demand. parse_program is fast enough for interactive use
        // and avoids us having to cache an AST that may be stale on errors.
        let program = match parse_program(&text) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        let summary = match hover_summary(&program, &ident) {
            Some(s) => s,
            None => return Ok(None),
        };

        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(summary)),
            range: None,
        }))
    }
}

/// Convert a 1-based `(line, col)` mention in a parser error message into an
/// LSP `Range`. Returns `None` if the message doesn't carry one.
fn extract_line_col(msg: &str) -> Option<Range> {
    // Compiled lazily on first call; cheap to keep static.
    static RE_SRC: &str = r"at line (\d+), col (\d+)";
    let re = Regex::new(RE_SRC).ok()?;
    let caps = re.captures(msg)?;
    let line: u32 = caps.get(1)?.as_str().parse().ok()?;
    let col: u32 = caps.get(2)?.as_str().parse().ok()?;
    let line = line.saturating_sub(1);
    let col = col.saturating_sub(1);
    Some(Range {
        start: Position {
            line,
            character: col,
        },
        end: Position {
            line,
            character: col + 1,
        },
    })
}

fn zero_range() -> Range {
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: 0,
        },
    }
}

/// Return the identifier (a-z, A-Z, 0-9, _) that contains `position` in the
/// given text, or `None` if the cursor isn't on one.
fn identifier_at(text: &str, position: Position) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let bytes = line.as_bytes();
    let col = position.character as usize;
    if col > bytes.len() {
        return None;
    }

    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    // Cursor can sit either on a character or just past the end of one; nudge
    // back one byte when we're at the boundary to still hit the trailing char.
    let mut anchor = col;
    if anchor == bytes.len() || !is_ident(bytes[anchor]) {
        if anchor == 0 {
            return None;
        }
        if !is_ident(bytes[anchor - 1]) {
            return None;
        }
        anchor -= 1;
    }

    let mut start = anchor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = anchor;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(line[start..end].to_string())
}

/// Find a top-level model/function with `name` (case-insensitive) and render a
/// short single-line summary suitable for hover.
fn hover_summary(program: &Program, name: &str) -> Option<String> {
    let key = name.to_lowercase();

    if let Some(model) = program
        .models
        .iter()
        .find(|m| m.name.to_lowercase() == key)
    {
        let kind = match model.kind {
            ModelKind::Entity => "entity",
            ModelKind::Class => "class",
        };
        let field_word = if model.fields.len() == 1 { "field" } else { "fields" };
        let summary = match (&model.context_name, model.kind == ModelKind::Entity) {
            (Some(ctx), true) => format!(
                "{kind} {} of {ctx} ({} {field_word})",
                model.name,
                model.fields.len()
            ),
            _ => format!(
                "{kind} {} ({} {field_word})",
                model.name,
                model.fields.len()
            ),
        };
        return Some(summary);
    }

    if let Some(func) = program
        .functions
        .iter()
        .find(|f| f.name.to_lowercase() == key)
    {
        let params = func
            .params
            .iter()
            .map(|p| match &p.ty {
                Some(ty) => format!("{}: {}", p.name, ty),
                None => p.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let ret = match &func.return_type {
            Some(ty) => format!(": {ty}"),
            None => String::new(),
        };
        let prefix = if func.is_async { "async function" } else { "function" };
        return Some(format!("{prefix} {}({params}){ret}", func.name));
    }

    None
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_col_from_error_message() {
        let r = extract_line_col("Unexpected token at line 3, col 7").unwrap();
        assert_eq!(r.start.line, 2);
        assert_eq!(r.start.character, 6);
    }

    #[test]
    fn ignores_messages_without_position() {
        assert!(extract_line_col("Duplicate entity name: User").is_none());
    }

    #[test]
    fn picks_identifier_under_cursor() {
        let src = "entity User { id: int }";
        let pos = Position {
            line: 0,
            character: 9,
        };
        assert_eq!(identifier_at(src, pos).as_deref(), Some("User"));
    }

    #[test]
    fn cursor_in_pure_whitespace_returns_none() {
        // Column 7 is the second of two spaces between "entity" and "User"
        // — neither side is an ident byte from that anchor.
        let src = "entity  User";
        let pos = Position {
            line: 0,
            character: 7,
        };
        assert!(identifier_at(src, pos).is_none());
    }

    #[test]
    fn hover_summary_for_entity_with_context() {
        let src = r#"
            dbcontext AppDbContext : Postgres;
            entity User of AppDbContext {
                id int pk;
                email varchar(255);
                name varchar(255);
            }
        "#;
        let program = parse_program(src).unwrap();
        let s = hover_summary(&program, "user").unwrap();
        assert!(s.contains("entity User"), "summary: {s}");
        assert!(s.contains("AppDbContext"), "summary: {s}");
        assert!(s.contains("3 fields"), "summary: {s}");
    }

    #[test]
    fn hover_summary_for_function_with_return_type() {
        let src = r#"
            function getUser(id: int): User {
                return 1;
            }
        "#;
        let program = parse_program(src).unwrap();
        let s = hover_summary(&program, "getUser").unwrap();
        assert_eq!(s, "function getUser(id: int): User");
    }

    #[test]
    fn diagnostics_for_parse_error_have_position() {
        let src = "entity {";
        let diags = Backend::compute_diagnostics(src);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn lint_warnings_become_warning_diagnostics() {
        let src = r#"
            function helper() { return 1; }
            function main() { print("hi"); }
        "#;
        let diags = Backend::compute_diagnostics(src);
        assert!(diags
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::WARNING)
                && d.message.contains("helper")));
    }
}
