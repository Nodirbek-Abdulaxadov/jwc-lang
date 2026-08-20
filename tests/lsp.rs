//! The language server, driven over its real stdio protocol.
//!
//! A scripted session against the built binary: nothing here reaches into
//! the library, because what is being pinned is the wire — the framing, the
//! capabilities, and the shape of each reply (tooling.md §6).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn start(root: &Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jwc"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn jwc lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut c = Client {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let caps = c.request(
            "initialize",
            json!({ "rootUri": format!("file://{}", root.display()) }),
        );
        assert_eq!(caps["capabilities"]["hoverProvider"], true, "{caps}");
        assert_eq!(caps["capabilities"]["definitionProvider"], true);
        assert_eq!(
            caps["capabilities"]["completionProvider"]["triggerCharacters"][0],
            "."
        );
        c
    }

    fn send(&mut self, msg: &Value) {
        let body = serde_json::to_vec(msg).expect("encode");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("write");
        self.stdin.write_all(&body).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        loop {
            let msg = self.read();
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return msg["result"].clone();
            }
            // A notification (diagnostics) arriving between the request and
            // its reply is normal and is not the answer.
        }
    }

    /// The next notification with this method, skipping anything else.
    fn wait_for(&mut self, method: &str) -> Value {
        loop {
            let msg = self.read();
            if msg.get("method").and_then(|m| m.as_str()) == Some(method) {
                return msg["params"].clone();
            }
        }
    }

    fn read(&mut self) -> Value {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read header");
            assert!(n > 0, "the server closed the connection");
            let t = line.trim_end_matches(['\r', '\n']);
            if t.is_empty() {
                break;
            }
            if let Some(rest) = t.strip_prefix("Content-Length:") {
                length = rest.trim().parse().expect("length");
            }
        }
        let mut buf = vec![0u8; length];
        self.stdout.read_exact(&mut buf).expect("read body");
        serde_json::from_slice(&buf).expect("decode")
    }

    fn open(&mut self, path: &Path) -> String {
        let text = std::fs::read_to_string(path).expect("read");
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": format!("file://{}", path.display()),
                "languageId": "jwc",
                "version": 1,
                "text": text,
            }}),
        );
        text
    }

    fn at(&mut self, path: &Path, method: &str, line: usize, character: usize) -> Value {
        self.request(
            method,
            json!({
                "textDocument": { "uri": format!("file://{}", path.display()) },
                "position": { "line": line, "character": character },
            }),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.notify("exit", json!({}));
        let _ = self.child.wait();
    }
}

/// The 0-based line holding the first occurrence of `needle`, and the
/// 0-based UTF-16 column just inside it.
fn find(text: &str, needle: &str) -> (usize, usize) {
    for (i, l) in text.lines().enumerate() {
        if let Some(col) = l.find(needle) {
            let prefix: usize = l[..col].chars().map(|c| c.len_utf16()).sum();
            return (i, prefix + 1);
        }
    }
    panic!("`{needle}` is not in the document");
}

#[test]
fn the_server_answers_over_its_own_protocol() {
    let root = repo_root().join("docs/spec/v1/sample");
    let mut c = Client::start(&root);

    let billing = root.join("src/services/billing.jwc");
    let text = c.open(&billing);
    let published = c.wait_for("textDocument/publishDiagnostics");
    // The sample is clean, so the interesting assertion is that a clean
    // file publishes an empty list rather than nothing at all — a client
    // that never receives one never clears the previous run's squiggles.
    assert_eq!(
        published["diagnostics"].as_array().map(|a| a.len()),
        Some(0),
        "{published}"
    );

    // Hover over a query is the SQL. Same compiler as `jwc explain`, so the
    // same answer (tooling.md §6.4).
    let (line, col) = find(&text, "select I from App.billing.Invoices");
    let hover = c.at(&billing, "textDocument/hover", line, col);
    let sql = hover["contents"]["value"].as_str().unwrap_or_default();
    assert!(sql.starts_with("```sql"), "{hover}");
    assert!(sql.contains("billing.invoice"), "{sql}");
    assert!(hover["range"]["start"]["line"].is_number(), "{hover}");

    // Hover over a name is what that name is.
    let (line, col) = find(&text, "InvoiceCreate");
    let hover = c.at(&billing, "textDocument/hover", line, col);
    let v = hover["contents"]["value"].as_str().unwrap_or_default();
    assert!(
        v.contains("**class**") && v.contains("InvoiceCreate"),
        "{hover}"
    );

    // Go to definition lands on the declaration, in whichever file it is.
    let def = c.at(&billing, "textDocument/definition", line, col);
    assert!(
        def["uri"].as_str().unwrap_or_default().ends_with(".jwc"),
        "{def}"
    );
    assert!(def["range"]["start"]["line"].is_number(), "{def}");

    // Signature help reads the typed service boundary (types.md §1), at a
    // real call site.
    let routes = root.join("src/routes/billing.jwc");
    let route_text = c.open(&routes);
    c.wait_for("textDocument/publishDiagnostics");
    let (line, col) = find(&route_text, "BillingService.subscribe(");
    let sig = c.at(&routes, "textDocument/signatureHelp", line, col + 25);
    let label = sig["signatures"][0]["label"].as_str().unwrap_or_default();
    assert!(
        label.starts_with("BillingService.subscribe(org_id: bigint"),
        "{sig}"
    );
    assert_eq!(sig["activeParameter"], 0, "{sig}");

    // One comma along is the second parameter.
    let sig = c.at(&routes, "textDocument/signatureHelp", line, col + 36);
    assert_eq!(sig["activeParameter"], 1, "{sig}");
}

#[test]
fn completion_offers_members_after_a_dot_and_names_otherwise() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a.jwc");
    std::fs::write(
        &path,
        "namespace c;\n\
         database App : Postgres;\n\
         schema s of App;\n\
         enum Plan of App.s { free, pro }\n\
         table Orgs of App.s {\n\
         \x20   id     bigint primary key identity;\n\
         \x20   slug   varchar(40);\n\
         \x20   secret varchar(40) private;\n\
         }\n\
         service Svc {\n\
         \x20   function one(id: bigint) -> text { return \"x\"; }\n\
         }\n\
         -- completion probes\n\
         -- Orgs.\n\
         -- Svc.\n\
         -- date.\n",
    )
    .expect("write");
    let root = dir.path().to_path_buf();
    let mut c = Client::start(&root);
    let text = c.open(&path);
    c.wait_for("textDocument/publishDiagnostics");

    let members = |c: &mut Client, needle: &str| -> Vec<String> {
        let (line, _) = find(&text, needle);
        let col = text.lines().nth(line).expect("line").len();
        let items = c.at(&path, "textDocument/completion", line, col);
        items
            .as_array()
            .expect("array")
            .iter()
            .map(|i| i["label"].as_str().unwrap_or_default().to_string())
            .collect()
    };

    let cols = members(&mut c, "-- Orgs.");
    assert!(cols.contains(&"slug".to_string()), "{cols:?}");
    // schema.md §3.1 — a `private` column is never in a response, so it is
    // not offered where a projection is being written.
    assert!(!cols.contains(&"secret".to_string()), "{cols:?}");

    let fns = members(&mut c, "-- Svc.");
    assert_eq!(fns, vec!["one".to_string()], "{fns:?}");

    let dates = members(&mut c, "-- date.");
    assert!(dates.contains(&"now".to_string()), "{dates:?}");

    // At statement position, the visible names.
    let (line, _) = find(&text, "-- completion probes");
    let items = c.at(&path, "textDocument/completion", line, 3);
    let labels: Vec<String> = items
        .as_array()
        .expect("array")
        .iter()
        .map(|i| i["label"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(labels.contains(&"Orgs".to_string()), "{labels:?}");
    assert!(labels.contains(&"Plan".to_string()), "{labels:?}");
    assert!(labels.contains(&"NotFound".to_string()), "{labels:?}");
}

#[test]
fn an_unsaved_edit_is_what_gets_checked() {
    // The editor's buffer is not the file. A server that read the file
    // would report the last save's diagnostics under the next edit.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a.jwc");
    let good = "namespace c;\n\
                database App : Postgres;\n\
                schema s of App;\n\
                table T of App.s { id bigint primary key identity; }\n";
    std::fs::write(&path, good).expect("write");

    let root = dir.path().to_path_buf();
    let mut c = Client::start(&root);
    c.open(&path);
    let clean = c.wait_for("textDocument/publishDiagnostics");
    assert_eq!(clean["diagnostics"].as_array().map(|a| a.len()), Some(0));

    // Same file on disk; a broken buffer in the editor.
    c.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": format!("file://{}", path.display()), "version": 2 },
            "contentChanges": [{ "text": format!("{good}table T of App.s {{ id bigint; }}\n") }],
        }),
    );
    let after = c.wait_for("textDocument/publishDiagnostics");
    let diags = after["diagnostics"].as_array().expect("array");
    assert!(!diags.is_empty(), "the unsaved edit was not checked");
    assert_eq!(diags[0]["source"], "jwc");
    assert!(diags[0]["code"].is_string(), "{after}");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), good);
}
