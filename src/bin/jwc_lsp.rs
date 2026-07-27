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
//!   * textDocument/definition — jumps to the declaration of a top-level
//!     entity / class / function / middleware / dbcontext, resolved via a
//!     per-document symbol index.
//!   * textDocument/rename — renames a top-level symbol across the document.
//!     References are located by textual scan with word-boundary guards,
//!     skipping comments and string literals.
//!   * textDocument/completion — context-aware: typed-catch error kinds,
//!     `use <mw>` middleware names, and in-scope user / builtin identifiers.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::{Error as LspError, Result as LspResult};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use jwc::ast::{ModelKind, Program};
use jwc::builtins::BUILTIN_DEFS;
use jwc::lint::lint_program;
use jwc::parser::{parse_program, validate_program};

/// Local mirror of `runner::JWC_ERROR_KINDS` (which is `pub(crate)` in the
/// library). Kept in sync manually; the smoke test in `tests/lsp_smoke.rs`
/// asserts a subset round-trip so a divergence here breaks CI loudly.
const JWC_ERROR_KINDS: &[&str] = &[
    "Error",
    "DbError",
    "DbError.UniqueViolation",
    "DbError.ForeignKeyViolation",
    "DbError.NotNullViolation",
    "DbError.CheckViolation",
    "DbError.SerializationFailure",
    "DbError.DeadlockDetected",
    "DbError.ConnectionFailure",
    "HttpError",
    "HttpError.NotFound",
    "HttpError.Unauthorized",
    "HttpError.Forbidden",
    "HttpError.BadGateway",
    "ValidationError",
    "TimeoutError",
    "JwtError",
    "JwtError.InvalidSignature",
    "JwtError.Expired",
];

/// One declaration site captured during indexing. Cross-file resolution maps
/// `file_idx` back to a `Program::sources[file_idx].label` and then to a URI;
/// for the single-document LSP path the active document URI is reused.
#[derive(Debug, Clone)]
struct SymbolEntry {
    name: String,
    /// Kind of declaration; surfaced via the symbol index for callers that
    /// want to distinguish a function from an entity without re-parsing.
    #[allow(dead_code)]
    kind: SymbolKindTag,
    /// Index into `Program::sources` for the file this decl came from.
    #[allow(dead_code)]
    file_idx: usize,
    /// Project-relative label of the declaring file (`"Data/Ctx.jwc"`), taken
    /// from `Program::sources[file_idx].label`. Empty when the program has no
    /// source table (single-file analysis), meaning "the focused document".
    file_label: String,
    /// The name token's range **within its own file**, resolved at index time
    /// while that file's text is in hand. Query-time handlers can't recompute
    /// it: they only hold the focused document's text, which is the wrong
    /// text for a symbol declared in a sibling file.
    range: Range,
    /// Byte offset of the declaration's name token inside its file's text.
    /// Superseded by `range` for LSP responses; retained as the byte-level
    /// view the index tests assert against and the natural basis for a future
    /// reference search.
    #[allow(dead_code)]
    offset_start: usize,
    /// `offset_start + name.len()`.
    #[allow(dead_code)]
    offset_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolKindTag {
    Function,
    Entity,
    Class,
    Middleware,
    DbContext,
}

impl SymbolKindTag {
    #[allow(dead_code)]
    fn keyword(self) -> &'static str {
        match self {
            SymbolKindTag::Function => "function",
            SymbolKindTag::Entity => "entity",
            SymbolKindTag::Class => "class",
            SymbolKindTag::Middleware => "middleware",
            SymbolKindTag::DbContext => "dbcontext",
        }
    }
}

/// Per-document index — symbol name (case-sensitive) → declarations.
#[derive(Debug, Default, Clone)]
struct SymbolIndex {
    entries: HashMap<String, Vec<SymbolEntry>>,
    /// Middleware names declared in the program, for `use <mw>` completion.
    middlewares: Vec<String>,
    /// User-defined function names, for in-body identifier completion.
    user_functions: Vec<String>,
}

#[derive(Debug)]
struct Backend {
    client: Client,
    /// Latest text we've seen for each open document, keyed by URI.
    documents: Arc<RwLock<HashMap<Url, String>>>,
    /// Per-document symbol index, rebuilt on every analyze.
    indices: Arc<RwLock<HashMap<Url, SymbolIndex>>>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build the merged program for the project `uri` belongs to, with every
    /// open editor buffer substituted for its on-disk contents.
    ///
    /// A JWC project is one flat namespace — entities, middleware and dome
    /// functions routinely live in sibling files. Analysing a file on its own
    /// reports each of those cross-file references as unknown, which is what
    /// made a healthy project show a screenful of errors that the CLI never
    /// produced.
    ///
    /// Returns `None` for a file with no `jwcproj.json` above it, where the
    /// single-file view is the correct one.
    async fn project_program(&self, uri: &Url) -> Option<Program> {
        let path = uri.to_file_path().ok()?;
        let root = jwc::project::find_project_root(&path).ok()?;

        // Unsaved edits in any open tab must win over what's on disk,
        // otherwise diagnostics lag a save behind.
        let docs = self.documents.read().await;
        let mut overrides = HashMap::new();
        for (doc_uri, text) in docs.iter() {
            if let Ok(p) = doc_uri.to_file_path() {
                overrides.insert(p, text.clone());
            }
        }
        drop(docs);

        jwc::project::merge_project_sources(&root, &overrides).ok()
    }

    /// Parse + validate + lint, and translate the results into LSP
    /// `Diagnostic` values.
    ///
    /// `project` is the merged program for the whole project when the file
    /// belongs to one; validation and lint run against it so cross-file
    /// references resolve. `file_label` is the file's project-relative path,
    /// used to keep another file's lint warnings from being republished here.
    fn compute_diagnostics(
        source: &str,
        project: Option<&Program>,
        file_label: Option<&str>,
    ) -> Vec<Diagnostic> {
        let mut diags = Vec::new();

        // The focused file is always parsed on its own so a syntax error
        // carries this file's line and column.
        let own_program = match parse_program(source) {
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

        let analysed = project.unwrap_or(&own_program);

        if let Err(err) = validate_program(analysed) {
            let msg = err.to_string();
            // Validating the merged project surfaces errors that belong to a
            // sibling file. Those carry an `at <file>:<line>:<col>` marker —
            // publish them against that file only, otherwise a single real
            // error is repeated on every file in the project. Errors with no
            // marker can't be attributed, so they stay on the current file
            // rather than being dropped.
            match located_diagnostic(&msg) {
                Some((label, range)) => {
                    if file_label.is_none_or(|f| f == label) {
                        diags.push(validate_diagnostic(msg, range));
                    }
                }
                None => diags.push(validate_diagnostic(msg, zero_range())),
            }
            return diags;
        }

        for w in lint_program(analysed) {
            // Lint ran over the merged project, so a warning about a symbol
            // declared elsewhere belongs on that file, not this one.
            if let (Some(label), Some(warned_on)) = (file_label, w.source_label(analysed)) {
                if label != warned_on {
                    continue;
                }
            }
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

    /// Resolve a `Program::sources` label to a file URI, relative to the
    /// project that `current` belongs to. Returns `None` for an empty label
    /// (single-file analysis) or when the file sits outside a project, in
    /// which case the caller falls back to the focused document.
    fn uri_for_label(current: &Url, label: &str) -> Option<Url> {
        if label.is_empty() {
            return None;
        }
        let path = current.to_file_path().ok()?;
        let root = jwc::project::find_project_root(&path).ok()?;
        Url::from_file_path(root.join(label)).ok()
    }

    /// The file's project-relative label (`"Features/Wallets/Routes.jwc"`),
    /// matching the labels `merge_project_sources` stamps on each source.
    fn project_label(uri: &Url) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        let root = jwc::project::find_project_root(&path).ok()?;
        Some(
            path.strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/"),
        )
    }

    async fn analyze_and_publish(&self, uri: Url, text: String, version: Option<i32>) {
        // Record the buffer first so `project_program` merges the text the
        // user is looking at, not the last-saved copy.
        {
            let mut docs = self.documents.write().await;
            docs.insert(uri.clone(), text.clone());
        }

        let project = self.project_program(&uri).await;
        let label = Self::project_label(&uri);
        let diags = Self::compute_diagnostics(&text, project.as_ref(), label.as_deref());
        // Build a symbol index from a successful parse. If parsing fails we
        // clear the index so stale lookups can't return phantom hits.
        //
        // The index is seeded from the merged project so hover, go-to-
        // definition and completion resolve symbols declared in sibling
        // files — the same root cause as the false diagnostics above.
        let index = match (&project, parse_program(&text)) {
            (Some(p), Ok(_)) => build_symbol_index(p, &text),
            (None, Ok(own)) => build_symbol_index(&own, &text),
            (_, Err(_)) => SymbolIndex::default(),
        };
        {
            let mut idx = self.indices.write().await;
            idx.insert(uri.clone(), index);
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
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        ":".to_string(),
                        " ".to_string(),
                    ]),
                    all_commit_characters: None,
                    work_done_progress_options: Default::default(),
                    completion_item: None,
                    resolve_provider: Some(false),
                }),
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

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let text = match self.documents.read().await.get(&uri).cloned() {
            Some(t) => t,
            None => return Ok(None),
        };
        let symbols = collect_document_symbols(&text, &uri);
        if symbols.is_empty() {
            Ok(None)
        } else {
            Ok(Some(DocumentSymbolResponse::Flat(symbols)))
        }
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

        // Prefer the merged project: hovering `AppDbContext.Wallet` in a
        // service file has to find the entity even though it's declared in
        // another file. Falls back to a re-parse of this document when the
        // file isn't part of a project. parse_program is fast enough for
        // interactive use and avoids caching an AST that may be stale.
        let program = match self.project_program(&uri).await {
            Some(p) => p,
            None => match parse_program(&text) {
                Ok(p) => p,
                Err(_) => return Ok(None),
            },
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

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
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

        let index = match self.indices.read().await.get(&uri).cloned() {
            Some(i) => i,
            None => return Ok(None),
        };

        // Case-insensitive resolution to match the rest of the LSP.
        let entry = index
            .entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&ident))
            .and_then(|(_, v)| v.first().cloned());
        let entry = match entry {
            Some(e) => e,
            None => return Ok(None),
        };

        // The declaration may live in a sibling file. `entry.range` was
        // resolved against that file's own text at index time, so it is used
        // as-is rather than recomputed against the focused document.
        let target_uri = Self::uri_for_label(&uri, &entry.file_label).unwrap_or(uri);

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range: entry.range,
        })))
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;

        // Validate the new identifier shape — reject early so the editor
        // surfaces a useful error message instead of corrupting the source.
        let ident_re =
            Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").map_err(|_| LspError::internal_error())?;
        if !ident_re.is_match(&new_name) {
            return Err(invalid_request(format!(
                "'{new_name}' is not a valid identifier"
            )));
        }

        let text = match self.documents.read().await.get(&uri).cloned() {
            Some(t) => t,
            None => return Ok(None),
        };

        let old_name = match identifier_at(&text, position) {
            Some(name) => name,
            None => return Ok(None),
        };
        if old_name == new_name {
            return Ok(None);
        }

        let index = match self.indices.read().await.get(&uri).cloned() {
            Some(i) => i,
            None => return Ok(None),
        };

        // Must resolve to a known top-level symbol — refuse to rename random
        // free identifiers.
        let resolved = index
            .entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&old_name))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if resolved.is_empty() {
            return Err(invalid_request(format!(
                "'{old_name}' is not a renamable top-level symbol"
            )));
        }

        // Collision check: refuse to rename onto an existing top-level name.
        if index
            .entries
            .keys()
            .any(|k| k.eq_ignore_ascii_case(&new_name))
        {
            return Err(invalid_request(format!(
                "'{new_name}' already names another top-level declaration"
            )));
        }

        // Rename every file in the project, not just this one. A JWC project
        // is a single flat namespace, so an entity declared in one file is
        // referenced from many others; rewriting only the focused document
        // would leave every other reference pointing at a name that no longer
        // exists — a silent break the user wouldn't see until the next build.
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        match self.project_program(&uri).await {
            Some(program) => {
                for source in &program.sources {
                    let edits = collect_rename_edits(&source.text, &old_name, &new_name);
                    if edits.is_empty() {
                        continue;
                    }
                    match Self::uri_for_label(&uri, &source.label) {
                        Some(target) => {
                            changes.insert(target, edits);
                        }
                        // Unlabelled source — can only be the focused buffer.
                        None => {
                            changes.insert(uri.clone(), edits);
                        }
                    }
                }
            }
            None => {
                let edits = collect_rename_edits(&text, &old_name, &new_name);
                if !edits.is_empty() {
                    changes.insert(uri.clone(), edits);
                }
            }
        }

        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let text = match self.documents.read().await.get(&uri).cloned() {
            Some(t) => t,
            None => {
                return Ok(Some(CompletionResponse::Array(default_completion_items(
                    &[],
                ))))
            }
        };
        let index = self
            .indices
            .read()
            .await
            .get(&uri)
            .cloned()
            .unwrap_or_default();

        let items = completion_items_for(&text, position, &index);
        Ok(Some(CompletionResponse::Array(items)))
    }
}

/// Convert a 1-based `(line, col)` mention in a parser error message into an
/// LSP `Range`. Returns `None` if the message doesn't carry one.
/// Pull the `at <file>:<line>:<col>` marker `validate.rs::loc_in` renders for
/// a multi-file program, returning the file label and the range it points at.
///
/// `None` for the single-file `at line X, col Y` shape and for errors raised
/// without a location — those can't be attributed to a sibling file.
fn located_diagnostic(msg: &str) -> Option<(String, Range)> {
    static RE_SRC: &str = r"at ([^\s:]+\.jwc):(\d+):(\d+)";
    let re = Regex::new(RE_SRC).ok()?;
    let caps = re.captures(msg)?;
    let label = caps.get(1)?.as_str().to_string();
    let line: u32 = caps.get(2)?.as_str().parse().ok()?;
    let col: u32 = caps.get(3)?.as_str().parse().ok()?;
    let line = line.saturating_sub(1);
    let col = col.saturating_sub(1);
    Some((
        label,
        Range {
            start: Position {
                line,
                character: col,
            },
            end: Position {
                line,
                character: col + 1,
            },
        },
    ))
}

fn validate_diagnostic(message: String, range: Range) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String("validate".into())),
        code_description: None,
        source: Some("jwc".into()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

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

    if let Some(model) = program.models.iter().find(|m| m.name.to_lowercase() == key) {
        let kind = match model.kind {
            ModelKind::Entity => "entity",
            ModelKind::Class => "class",
        };
        let field_word = if model.fields.len() == 1 {
            "field"
        } else {
            "fields"
        };
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
        let prefix = if func.is_async {
            "async function"
        } else {
            "function"
        };
        return Some(format!("{prefix} {}({params}){ret}", func.name));
    }

    None
}

/// Regex-scan `source` for top-level declarations the editor's outline
/// view should surface. Returns a flat `Vec<SymbolInformation>` matching
/// `DocumentSymbolResponse::Flat`.
///
/// Intentionally regex-based rather than AST-based: the AST doesn't carry
/// source positions per declaration today, and the outline only needs
/// "first line of the declaration" granularity. A token-stream-aware
/// upgrade is tracked alongside the LSP power sprint (Sprint 3).
fn collect_document_symbols(source: &str, uri: &Url) -> Vec<SymbolInformation> {
    // Each pattern captures the symbol name (or path, for routes) and the
    // SymbolKind to report. Anchored at the start of the line because JWC
    // top-level forms don't get indented.
    let patterns: &[(&str, SymbolKind)] = &[
        (
            r#"^\s*entity\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            SymbolKind::CLASS,
        ),
        (
            r#"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            SymbolKind::STRUCT,
        ),
        (
            r#"^\s*(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            SymbolKind::FUNCTION,
        ),
        (
            r#"^\s*middleware\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            SymbolKind::INTERFACE,
        ),
        (
            r#"^\s*dbcontext\s+([A-Za-z_][A-Za-z0-9_]*)"#,
            SymbolKind::NAMESPACE,
        ),
        (
            r#"^\s*route\s+(?:GET|POST|PUT|DELETE|PATCH|WS)\s+"([^"]+)""#,
            SymbolKind::METHOD,
        ),
    ];

    let regexes: Vec<(Regex, SymbolKind)> = patterns
        .iter()
        .filter_map(|(p, k)| Regex::new(p).ok().map(|r| (r, *k)))
        .collect();

    let mut out: Vec<SymbolInformation> = Vec::new();
    for (line_idx, line) in source.lines().enumerate() {
        for (re, kind) in &regexes {
            let Some(caps) = re.captures(line) else {
                continue;
            };
            let Some(name_match) = caps.get(1) else {
                continue;
            };
            let start_col = name_match.start() as u32;
            let end_col = name_match.end() as u32;
            let range = Range {
                start: Position {
                    line: line_idx as u32,
                    character: start_col,
                },
                end: Position {
                    line: line_idx as u32,
                    character: end_col,
                },
            };
            #[allow(deprecated)]
            out.push(SymbolInformation {
                name: name_match.as_str().to_string(),
                kind: *kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: uri.clone(),
                    range,
                },
                container_name: None,
            });
            break; // First match per line wins.
        }
    }
    out
}

/// Locate the source range of a top-level declaration name. `kind` is one of
/// `"function"`, `"entity"`, `"class"`, or `"middleware"`. Retained as a
/// regex-only fallback for callers that lack a parsed Program; `goto_definition`
/// now resolves via the symbol index instead.
#[allow(dead_code)]
fn find_decl_name_range(source: &str, kind: &str, name: &str) -> Option<Range> {
    let escaped = regex::escape(name);
    let pattern = match kind {
        "function" => format!(r#"^\s*(?:pub\s+)?(?:async\s+)?function\s+({})\b"#, escaped),
        "entity" => format!(r#"^\s*(?:pub\s+)?entity\s+({})\b"#, escaped),
        "class" => format!(r#"^\s*(?:pub\s+)?class\s+({})\b"#, escaped),
        "middleware" => format!(r#"^\s*(?:pub\s+)?middleware\s+({})\b"#, escaped),
        _ => return None,
    };
    let re = Regex::new(&pattern).ok()?;
    for (line_idx, line) in source.lines().enumerate() {
        if let Some(caps) = re.captures(line) {
            let m = caps.get(1)?;
            return Some(Range {
                start: Position {
                    line: line_idx as u32,
                    character: m.start() as u32,
                },
                end: Position {
                    line: line_idx as u32,
                    character: m.end() as u32,
                },
            });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Symbol indexing
// ---------------------------------------------------------------------------

/// Walk a parsed `Program` once and stamp each top-level declaration with the
/// source offset of its name token. We don't have a `name_offset` on the AST
/// nodes (intentionally — Phase 8D doesn't touch `ast.rs`), so we scan
/// forward from the keyword offset to the next identifier, which is always
/// the declaration name in JWC's grammar.
fn build_symbol_index(program: &Program, source: &str) -> SymbolIndex {
    let mut index = SymbolIndex::default();

    let push = |entry: SymbolEntry, idx: &mut SymbolIndex| {
        idx.entries
            .entry(entry.name.clone())
            .or_default()
            .push(entry);
    };

    // A declaration's `offset` indexes into the text of the file it came
    // from, which is only `source` for a single-file program. Once the
    // program is a merged project, resolving every offset against the
    // focused file's text would hand back spans pointing at unrelated code.
    // `Program::sources` carries each file's verbatim text, so use that when
    // it's populated.
    let file_text = |file_idx: usize| -> &str {
        program
            .sources
            .get(file_idx)
            .map(|s| s.text.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(source)
    };
    let file_label = |file_idx: usize| -> String {
        program
            .sources
            .get(file_idx)
            .map(|s| s.label.clone())
            .unwrap_or_default()
    };

    for f in &program.functions {
        let text = file_text(f.file_idx);
        if let Some((s, e)) = name_span_after_keyword(text, f.offset, &f.name) {
            if let Some(range) = offsets_to_range(text, s, e) {
                push(
                    SymbolEntry {
                        name: f.name.clone(),
                        kind: SymbolKindTag::Function,
                        file_idx: f.file_idx,
                        file_label: file_label(f.file_idx),
                        range,
                        offset_start: s,
                        offset_end: e,
                    },
                    &mut index,
                );
            }
        }
        index.user_functions.push(f.name.clone());
    }
    for m in &program.models {
        let kind = match m.kind {
            ModelKind::Entity => SymbolKindTag::Entity,
            ModelKind::Class => SymbolKindTag::Class,
        };
        let text = file_text(m.file_idx);
        if let Some((s, e)) = name_span_after_keyword(text, m.offset, &m.name) {
            if let Some(range) = offsets_to_range(text, s, e) {
                push(
                    SymbolEntry {
                        name: m.name.clone(),
                        kind,
                        file_idx: m.file_idx,
                        file_label: file_label(m.file_idx),
                        range,
                        offset_start: s,
                        offset_end: e,
                    },
                    &mut index,
                );
            }
        }
    }
    for mw in &program.middlewares {
        let text = file_text(mw.file_idx);
        if let Some((s, e)) = name_span_after_keyword(text, mw.offset, &mw.name) {
            if let Some(range) = offsets_to_range(text, s, e) {
                push(
                    SymbolEntry {
                        name: mw.name.clone(),
                        kind: SymbolKindTag::Middleware,
                        file_idx: mw.file_idx,
                        file_label: file_label(mw.file_idx),
                        range,
                        offset_start: s,
                        offset_end: e,
                    },
                    &mut index,
                );
            }
        }
        index.middlewares.push(mw.name.clone());
    }
    for db in &program.dbcontexts {
        let text = file_text(db.file_idx);
        if let Some((s, e)) = name_span_after_keyword(text, db.offset, &db.name) {
            if let Some(range) = offsets_to_range(text, s, e) {
                push(
                    SymbolEntry {
                        name: db.name.clone(),
                        kind: SymbolKindTag::DbContext,
                        file_idx: db.file_idx,
                        file_label: file_label(db.file_idx),
                        range,
                        offset_start: s,
                        offset_end: e,
                    },
                    &mut index,
                );
            }
        }
    }

    index
}

/// Starting at the `keyword` byte offset (e.g. the `f` of `function`), skip
/// the keyword + any modifier (`async`, `pub`) and any whitespace, then
/// return the byte span of the first matching `name` occurrence.
///
/// Returns `None` if the offset is out of bounds or the name isn't present
/// at the expected location (e.g. for synthetic AST nodes with offset = 0
/// that don't correspond to source text).
fn name_span_after_keyword(source: &str, kw_offset: usize, name: &str) -> Option<(usize, usize)> {
    if name.is_empty() || kw_offset > source.len() {
        return None;
    }
    let slice = source.get(kw_offset..)?;
    // Match the name with word boundaries; search inside the next ~256 bytes
    // (declarations don't put arbitrary text between `function` and the name
    // in real JWC code).
    let window_end = slice.len().min(256);
    let window = &slice[..window_end];
    let pat = format!(r"\b{}\b", regex::escape(name));
    let re = Regex::new(&pat).ok()?;
    let m = re.find(window)?;
    Some((kw_offset + m.start(), kw_offset + m.end()))
}

/// Convert a byte-offset span in `source` to an LSP `Range`.
fn offsets_to_range(source: &str, start: usize, end: usize) -> Option<Range> {
    let start_pos = offset_to_position(source, start)?;
    let end_pos = offset_to_position(source, end)?;
    Some(Range {
        start: start_pos,
        end: end_pos,
    })
}

fn offset_to_position(source: &str, offset: usize) -> Option<Position> {
    if offset > source.len() {
        return None;
    }
    let prefix = &source[..offset];
    let line = prefix.matches('\n').count() as u32;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = source[line_start..offset].chars().count() as u32;
    Some(Position { line, character })
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

/// Collect `TextEdit`s for every occurrence of `old_name` (whole-word) in
/// `source`, skipping line comments (`//`), block comments (`/* */`), and
/// double-quoted string literals. Used by `textDocument/rename`.
fn collect_rename_edits(source: &str, old_name: &str, new_name: &str) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;

    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let old_bytes = old_name.as_bytes();
    let old_len = old_bytes.len();

    while i < n {
        let b = bytes[i];

        // Line comment: skip to end of line.
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment: skip until "*/".
        if b == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // String literal: skip with escape handling.
        if b == b'"' {
            i += 1;
            while i < n && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            i = (i + 1).min(n);
            continue;
        }

        if is_ident(b) {
            // Read full identifier; only act on exact matches with word
            // boundaries on both sides.
            let start = i;
            while i < n && is_ident(bytes[i]) {
                i += 1;
            }
            let end = i;
            let prev_is_ident = start > 0 && is_ident(bytes[start - 1]);
            if !prev_is_ident && end - start == old_len && &bytes[start..end] == old_bytes {
                if let Some(range) = offsets_to_range(source, start, end) {
                    edits.push(TextEdit {
                        range,
                        new_text: new_name.to_string(),
                    });
                }
            }
            continue;
        }

        i += 1;
    }

    edits
}

/// Build a `LspError::invalid_request` carrying a custom message. tower-lsp's
/// `LspError` doesn't expose a constructor, but we can clone the default and
/// patch the `message` field.
fn invalid_request(msg: String) -> LspError {
    let mut e = LspError::invalid_request();
    e.message = msg.into();
    e
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

const JWC_KEYWORDS: &[&str] = &[
    "function",
    "route",
    "entity",
    "middleware",
    "dbcontext",
    "try",
    "catch",
    "transaction",
    "for",
    "if",
    "else",
    "return",
    "let",
    "validate",
    "body",
    "where",
    "group",
    "having",
    "orderby",
    "limit",
    "offset",
    "first",
    "with",
    "from",
    "into",
    "in",
    "using",
    "mount",
    "pub",
    "async",
    "await",
];

/// Context-aware completion. The current line is sliced up to `position.character`
/// and matched against four patterns; the first hit wins. Contexts shipped this
/// sprint:
///
///   1. `catch (var: <prefix>` → `JWC_ERROR_KINDS`.
///   2. `use <prefix>` → declared middleware names.
///   3. In-body identifier prefix → user functions + builtin defs + keywords.
///
/// TODO[lsp]: where-context completion (`where Entity.<col>`) is deferred —
/// the AST doesn't expose entity field offsets in a way we can resolve from
/// the cursor alone yet, and the validator's column check is the source of
/// truth that should drive it.
fn completion_items_for(
    text: &str,
    position: Position,
    index: &SymbolIndex,
) -> Vec<CompletionItem> {
    let line = text.lines().nth(position.line as usize).unwrap_or("");
    let col = (position.character as usize).min(line.len());
    let prefix = &line[..col];

    // 1. Typed-catch context: `catch (e: <prefix>` — match the `:` separator.
    static CATCH_RE_SRC: &str = r"catch\s*\(\s*[A-Za-z_][A-Za-z0-9_]*\s*:\s*[A-Za-z0-9_.]*$";
    if Regex::new(CATCH_RE_SRC)
        .ok()
        .map(|r| r.is_match(prefix))
        .unwrap_or(false)
    {
        return JWC_ERROR_KINDS
            .iter()
            .map(|k| CompletionItem {
                label: (*k).to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                detail: Some("JWC error kind".into()),
                ..Default::default()
            })
            .collect();
    }

    // 2. Middleware `use` context: `use <prefix>` (route- or group-level).
    static USE_RE_SRC: &str = r"\buse\s+[A-Za-z0-9_,\s]*$";
    if Regex::new(USE_RE_SRC)
        .ok()
        .map(|r| r.is_match(prefix))
        .unwrap_or(false)
        && !index.middlewares.is_empty()
    {
        return index
            .middlewares
            .iter()
            .map(|name| CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::INTERFACE),
                detail: Some("middleware".into()),
                ..Default::default()
            })
            .collect();
    }

    // 3. Default: keywords + builtins + user functions.
    let extras: Vec<CompletionItem> = index
        .user_functions
        .iter()
        .map(|name| CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some("user function".into()),
            ..Default::default()
        })
        .collect();
    default_completion_items(&extras)
}

/// Keywords + builtin functions + any caller-supplied items, deduplicated by
/// label so the user-function pass doesn't shadow builtins.
fn default_completion_items(extras: &[CompletionItem]) -> Vec<CompletionItem> {
    let mut items = Vec::with_capacity(JWC_KEYWORDS.len() + BUILTIN_DEFS.len() + extras.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for kw in JWC_KEYWORDS {
        if seen.insert((*kw).to_string()) {
            items.push(CompletionItem {
                label: (*kw).to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }
    for def in BUILTIN_DEFS {
        if seen.insert(def.name.to_string()) {
            items.push(CompletionItem {
                label: def.name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("builtin".into()),
                ..Default::default()
            });
        }
    }
    for ex in extras {
        if seen.insert(ex.label.clone()) {
            items.push(ex.clone());
        }
    }
    items
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
    fn collects_top_level_symbols_in_source_order() {
        let src = "\
dbcontext AppDb { driver = \"postgres\"; }
entity User { id: int pk; name: string; }
class UserCreateDto { name: string; }
function getUser(id: int): User { return null; }
middleware AuthMw { return 1; }
route GET \"/users\" { return json(\"ok\"); }
route POST \"/users\" -> getUser;
async function doStuff() { return null; }
";
        let uri = Url::parse("file:///tmp/x.jwc").unwrap();
        let syms = collect_document_symbols(src, &uri);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "AppDb",
                "User",
                "UserCreateDto",
                "getUser",
                "AuthMw",
                "/users",
                "/users",
                "doStuff",
            ]
        );
        // Same URI propagated on every entry.
        for sym in &syms {
            assert_eq!(sym.location.uri, uri);
        }
        // Line numbers ascend monotonically.
        let lines: Vec<u32> = syms.iter().map(|s| s.location.range.start.line).collect();
        assert!(
            lines.windows(2).all(|w| w[0] <= w[1]),
            "symbol lines not monotonic: {lines:?}"
        );
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
        let diags = Backend::compute_diagnostics(src, None, None);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    /// Multi-file validation errors carry `at <file>:<line>:<col>`, which is
    /// how a project-wide error gets published against the one file it
    /// belongs to instead of every file in the project.
    #[test]
    fn located_diagnostic_extracts_file_and_position() {
        let msg = "Route GET /stats references unknown middleware 'Ghost' \
                   at Features/Stats/StatsRoutes.jwc:3:1";
        let (label, range) = located_diagnostic(msg).expect("should parse a located error");
        assert_eq!(label, "Features/Stats/StatsRoutes.jwc");
        assert_eq!(range.start.line, 2, "1-based line 3 -> 0-based 2");
        assert_eq!(range.start.character, 0);
    }

    /// Single-file errors use `at line X, col Y` and carry no file, so they
    /// must not be mistaken for another file's problem and filtered away.
    #[test]
    fn located_diagnostic_ignores_single_file_shape() {
        assert!(located_diagnostic("expected ';' at line 8, col 35").is_none());
        assert!(located_diagnostic("Unknown entity 'Wallet'").is_none());
    }

    /// A project-wide error belonging to another file is dropped here; the
    /// file it names publishes it instead.
    #[test]
    fn diagnostics_skip_errors_owned_by_another_file() {
        // Parses fine on its own; the project error points elsewhere.
        let src = "route GET \"/x\" { return json(\"ok\"); }\n";
        let mut project = Program::default();
        jwc::project::merge_program(
            &mut project,
            jwc::parser::parse_program_with_label(
                "route GET \"/y\" use Ghost { return json(\"ok\"); }\n",
                "Other.jwc",
            )
            .unwrap(),
        )
        .unwrap();

        let diags = Backend::compute_diagnostics(src, Some(&project), Some("Mine.jwc"));
        assert!(
            diags.is_empty(),
            "Other.jwc's error must not surface on Mine.jwc, got: {diags:?}"
        );

        let owned = Backend::compute_diagnostics(src, Some(&project), Some("Other.jwc"));
        assert_eq!(owned.len(), 1, "the owning file still reports it");
    }

    #[test]
    fn lint_warnings_become_warning_diagnostics() {
        let src = r#"
            function helper() { return 1; }
            function main() { print("hi"); }
        "#;
        let diags = Backend::compute_diagnostics(src, None, None);
        assert!(diags.iter().any(
            |d| d.severity == Some(DiagnosticSeverity::WARNING) && d.message.contains("helper")
        ));
    }

    // -- Phase 8D smoke tests -----------------------------------------------

    /// Build a symbol index from `src` for the assertions below.
    fn index(src: &str) -> SymbolIndex {
        let program = parse_program(src).unwrap();
        build_symbol_index(&program, src)
    }

    #[test]
    fn definition_jumps_to_function_in_same_file() {
        let src = "function helper(): int { return 1; }\nfunction main() { return helper(); }\n";
        let idx = index(src);
        let entry = idx
            .entries
            .get("helper")
            .and_then(|v| v.first())
            .cloned()
            .expect("helper indexed");
        assert_eq!(entry.kind, SymbolKindTag::Function);

        // The decl-name span lands on `helper` in the first line.
        let range = offsets_to_range(src, entry.offset_start, entry.offset_end).unwrap();
        assert_eq!(range.start.line, 0);
        let snippet = &src[entry.offset_start..entry.offset_end];
        assert_eq!(snippet, "helper");
    }

    #[test]
    fn rename_renames_all_references_within_function_scope() {
        let src = "function helper(): int { return 1; }\nfunction main() { return helper() + helper(); }\n";
        let edits = collect_rename_edits(src, "helper", "renamed");
        // One decl + two call sites = three edits.
        assert_eq!(edits.len(), 3, "expected 3 edits, got: {edits:?}");
        // None of them straddle a non-identifier boundary — every edit covers
        // exactly the six bytes of the old name.
        for e in &edits {
            assert_eq!(e.new_text, "renamed");
        }
    }

    #[test]
    fn rename_skips_matches_inside_strings_and_comments() {
        let src = "function helper() { return 1; }\n// helper in comment\nfunction main() { let s = \"helper\"; return helper(); }\n";
        let edits = collect_rename_edits(src, "helper", "renamed");
        // Decl + body call = 2 (the comment and string occurrences are skipped).
        assert_eq!(edits.len(), 2, "expected 2 edits, got: {edits:?}");
    }

    #[test]
    fn completion_inside_catch_returns_error_kinds() {
        let src = "function f() { try { return 1; } catch (e: ) { return 2; } }\n";
        // Cursor sits right after the `:` (column = position of the space after `:`).
        let col = src.lines().next().unwrap().find("e: ").unwrap() + 3; // after "e: "
        let pos = Position {
            line: 0,
            character: col as u32,
        };
        let idx = SymbolIndex::default();
        let items = completion_items_for(src, pos, &idx);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"DbError"), "labels: {labels:?}");
        assert!(labels.contains(&"HttpError.NotFound"), "labels: {labels:?}");
        // The catch context must NOT emit JWC keywords like `function`.
        assert!(!labels.contains(&"function"), "labels: {labels:?}");
    }

    #[test]
    fn completion_inside_use_returns_middleware_names() {
        // The completion path runs against in-memory text + a prebuilt index;
        // the text doesn't need to parse cleanly for the prefix-matcher.
        let src = "middleware Auth { return 1; }\nroute GET \"/x\" use Auth, ";
        let idx = SymbolIndex {
            middlewares: vec!["Auth".into(), "Log".into()],
            ..Default::default()
        };
        let line_idx = 1u32;
        let line = src.lines().nth(line_idx as usize).unwrap();
        let col = line.len();
        let pos = Position {
            line: line_idx,
            character: col as u32,
        };
        let items = completion_items_for(src, pos, &idx);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Auth"), "labels: {labels:?}");
        assert!(labels.contains(&"Log"), "labels: {labels:?}");
        // Middleware-only context must not bleed keywords through.
        assert!(!labels.contains(&"function"), "labels: {labels:?}");
    }

    #[test]
    fn completion_default_includes_keywords_and_builtins() {
        // Source doesn't need to parse — completion_items_for just slices the
        // current line up to the cursor for prefix matching.
        let src = "function f() { ret";
        let idx = SymbolIndex {
            user_functions: vec!["f".into()],
            ..Default::default()
        };
        let pos = Position {
            line: 0,
            character: src.len() as u32,
        };
        let items = completion_items_for(src, pos, &idx);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"return"), "labels missing 'return'");
        assert!(labels.contains(&"json"), "labels missing 'json'");
        assert!(labels.contains(&"f"), "labels missing user fn 'f'");
    }
}
