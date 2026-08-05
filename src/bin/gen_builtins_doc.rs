//! Generate `docs/docs/reference/builtins.md` from `jwc::builtins::BUILTIN_DEFS`.
//!
//! Run:
//!     cargo run --bin gen_builtins_doc > docs/docs/reference/builtins.md
//!
//! Or as part of CI:
//!     cargo run --bin gen_builtins_doc | diff -q docs/docs/reference/builtins.md -
//!
//! The generated markdown is grouped by category (string helpers, HTTP
//! request, response helpers, etc.) and emits a table per group with
//! columns: `Name | Aliases | Args | Native | Description`. The
//! `Description` column is intentionally left as a placeholder
//! (`—`) when no doc string is attached to the def; human-curated prose
//! for those rows lives in `docs/spec/builtins.md` until every def carries
//! an inline doc string.
//!
//! Note: a def matching NO group predicate below is silently dropped from
//! the output, and `builtins_doc_sync` still passes because generator and
//! checked-in file agree on the omission. Adding a `BuiltinDef` row is not
//! enough — put the name in a `GROUPS` predicate too.
//!
//! Why a binary instead of a `build.rs` step:
//!   1. The output is checked-in so docs/* reads work offline.
//!   2. `cargo run --bin gen_builtins_doc` is the *manual* regenerate
//!      command, run by docs maintainers after touching `src/builtins.rs`.
//!   3. A unit test (`tests/builtins_doc_sync.rs`) asserts the checked-in
//!      file matches the current generator output. It is NOT in the
//!      workflow's test list, so run it yourself after touching the
//!      registry.

use jwc::builtins::{BuiltinDef, BUILTIN_DEFS};

// When this file is reused as a module by `tests/builtins_doc_sync.rs` via
// `#[path]`, `main` is unused — it's only the binary entry point. The
// sync test imports `render_builtins_doc` directly.
#[allow(dead_code)]
fn main() {
    print!("{}", render_builtins_doc(BUILTIN_DEFS));
}

/// Render the full builtins reference markdown from the registry.
///
/// Pure function — takes the static defs and emits the doc body. Easy to
/// snapshot-test against the on-disk file (see
/// `tests/builtins_doc_sync.rs`).
pub fn render_builtins_doc(defs: &[BuiltinDef]) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("sidebar_position: 1\n");
    // Docusaurus derives the meta description from the first line of body
    // text when the frontmatter carries none, which for this page is the
    // "Auto-generated" admonition. Emit a written one, and emit it here so
    // regenerating the doc doesn't drop it.
    out.push_str(
        "description: \"Every JWC built-in function with its arguments, return type and \
         whether it is supported under jwc build --native. Generated from the compiler's \
         own table.\"\n",
    );
    out.push_str("---\n\n");
    out.push_str("# Built-in functions\n\n");
    out.push_str(
        "> **Auto-generated** from `src/builtins.rs::BUILTIN_DEFS`. To regenerate after adding\n",
    );
    out.push_str("> or editing a builtin, run:\n");
    out.push_str(">\n");
    out.push_str("> ```bash\n");
    out.push_str("> cargo run --bin gen_builtins_doc > docs/docs/reference/builtins.md\n");
    out.push_str("> ```\n");
    out.push_str(">\n");
    out.push_str(
        "> `tests/builtins_doc_sync.rs` compares this file against the registry byte for\n",
    );
    out.push_str("> byte. Run it after adding a builtin — the workflow's test job does not.\n\n");

    out.push_str("Columns:\n\n");
    out.push_str(
        "- **Args** — `min..max` argument count enforced by the runtime. `*` = variadic.\n",
    );
    out.push_str(
        "- **Native** — ✅ if `jwc build --native` accepts the call; — if interpreter-only.\n",
    );
    out.push_str(
        "- **Aliases** — additional names the interpreter dispatches case-insensitively.\n\n",
    );

    // The defs already flow in category order in src/builtins.rs (string
    // helpers → HTTP request → response helpers → DB → WS → async I/O →
    // env → time → cache → raw SQL → arrays → crypto → interpreter-only).
    // Re-derive groups from the source comment markers via a static slice
    // so the generator order matches the file's section order.
    for group in GROUPS {
        out.push_str(&format!("## {}\n\n", group.title));
        out.push_str("| Name | Aliases | Args | Native |\n");
        out.push_str("|---|---|---|---|\n");
        for def in defs.iter().filter(|d| (group.predicate)(d)) {
            let aliases = if def.aliases.is_empty() {
                "—".to_string()
            } else {
                def.aliases
                    .iter()
                    .map(|a| format!("`{a}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let args = match def.max_args {
                Some(max) if max == def.min_args => format!("{}", def.min_args),
                Some(max) => format!("{}..{}", def.min_args, max),
                None => format!("{}..*", def.min_args),
            };
            let native = if def.native { "✅" } else { "—" };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                def.name, aliases, args, native
            ));
        }
        out.push('\n');
    }

    out.push_str("## Notes\n\n");
    out.push_str("- The native AOT whitelist is the set of every `Name` + every `Alias` where\n");
    out.push_str(
        "  **Native** is ✅. Interpreter-only builtins still run under `jwc run` but are\n",
    );
    out.push_str(
        "  rejected at `jwc build --native` time — preferred over silent miscompilation.\n",
    );
    out.push_str("- For per-builtin contract (semantics, error modes, examples), see\n");
    out.push_str("  [`docs/spec/builtins.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/docs/spec/builtins.md).\n");
    out.push_str("- New builtin? Edit `src/builtins.rs::BUILTIN_DEFS`, regenerate this file via\n");
    out.push_str(
        "  the command at the top, then add the runtime + AOT impls per the module docs.\n",
    );

    out
}

struct Group {
    title: &'static str,
    predicate: fn(&BuiltinDef) -> bool,
}

// Section membership is name-based — keeps the doc generator a pure
// function of the defs slice (no comment-parsing). When a new builtin
// lands, slot it into an existing predicate or add a new Group row.
const GROUPS: &[Group] = &[
    Group {
        title: "Strings",
        predicate: |d| {
            matches!(
                d.name,
                "length"
                    | "len"
                    | "lower"
                    | "upper"
                    | "trim"
                    | "contains"
                    | "starts_with"
                    | "ends_with"
                    | "replace"
                    | "split"
                    | "substring"
                    | "take"
                    | "first"
                    | "last"
            )
        },
    },
    Group {
        title: "JSON",
        predicate: |d| matches!(d.name, "json_parse" | "json_stringify" | "set_json_field"),
    },
    Group {
        title: "HTTP request",
        predicate: |d| {
            matches!(
                d.name,
                "path_param"
                    | "query_param"
                    | "body"
                    | "request_body"
                    | "header"
                    | "client_ip"
                    | "request_id"
                    | "request_path"
                    | "request_method"
                    | "response_status"
                    | "response_duration_ms"
            )
        },
    },
    Group {
        title: "HTTP response",
        predicate: |d| {
            matches!(
                d.name,
                "json"
                    | "json_unchecked"
                    | "text"
                    | "html"
                    | "response"
                    | "ok"
                    | "created"
                    | "not_found"
                    | "notFound"
                    | "no_content"
                    | "noContent"
                    | "unauthorized"
                    | "forbidden"
                    | "bad_request"
                    | "badRequest"
                    | "internal_error"
                    | "internalError"
                    | "status_code"
                    | "statusCode"
            )
        },
    },
    Group {
        title: "Database",
        predicate: |d| {
            matches!(
                d.name,
                "setConnectionString" | "set_connection_string" | "raw_sql" | "db_query"
            )
        },
    },
    Group {
        title: "WebSocket",
        predicate: |d| matches!(d.name, "ws_send" | "ws_recv" | "ws_close"),
    },
    Group {
        title: "Async I/O",
        predicate: |d| matches!(d.name, "sleep_ms" | "http_get" | "http_post" | "fetch_json"),
    },
    Group {
        title: "Console I/O",
        predicate: |d| matches!(d.name, "console.write" | "console.error" | "console.read"),
    },
    Group {
        title: "Files + directories",
        predicate: |d| {
            matches!(
                d.name,
                "file.read"
                    | "file.write"
                    | "file.append"
                    | "file.exists"
                    | "file.delete"
                    | "file.copy"
                    | "file.move"
                    | "file.size"
                    | "file.lines"
                    | "directory.list"
                    | "directory.create"
                    | "directory.exists"
                    | "directory.delete"
            )
        },
    },
    Group {
        title: "Environment + coercion",
        // `serve` and `random_int` live here because nothing else fits and a
        // def matching no group's predicate is silently dropped from the
        // generated doc — both were invisible until this was noticed.
        predicate: |d| matches!(d.name, "env" | "int" | "serve" | "random_int"),
    },
    Group {
        title: "Time + identifiers",
        predicate: |d| matches!(d.name, "now" | "uuid" | "unix_timestamp"),
    },
    Group {
        title: "Cache (in-memory)",
        predicate: |d| {
            matches!(
                d.name,
                "cache_get" | "cache_set" | "cache_del" | "cache_clear"
            )
        },
    },
    Group {
        title: "Arrays",
        predicate: |d| matches!(d.name, "range" | "push" | "join"),
    },
    Group {
        title: "Hashing + crypto",
        predicate: |d| {
            matches!(
                d.name,
                "sha256"
                    | "sha1"
                    | "md5"
                    | "hmac_sha256"
                    | "hash_password"
                    | "verify_password"
                    | "jwt_sign"
                    | "jwt_verify"
            )
        },
    },
    Group {
        title: "Email",
        predicate: |d| matches!(d.name, "send_email"),
    },
    Group {
        title: "Request context",
        predicate: |d| {
            matches!(
                d.name,
                "context" | "setContext" | "set_context" | "dispatch"
            )
        },
    },
    Group {
        title: "Background jobs",
        predicate: |d| {
            matches!(
                d.name,
                "register_job_handler"
                    | "enqueue"
                    | "enqueue_urgent"
                    | "job_count"
                    | "dlq_count"
                    | "dlq_drain"
            )
        },
    },
];
