---
sidebar_position: 1
description: "Every JWC built-in function with its arguments, return type and whether it is supported under jwc build --native. Generated from the compiler's own table."
---

# Built-in functions

> **Auto-generated** from `src/builtins.rs::BUILTIN_DEFS`. To regenerate after adding
> or editing a builtin, run:
>
> ```bash
> cargo run --bin gen_builtins_doc > docs/docs/reference/builtins.md
> ```
>
> `tests/builtins_doc_sync.rs` compares this file against the registry byte for
> byte. Run it after adding a builtin — the workflow's test job does not.

Columns:

- **Args** — `min..max` argument count enforced by the runtime. `*` = variadic.
- **Native** — ✅ if `jwc build --native` accepts the call; — if interpreter-only.
- **Aliases** — additional names the interpreter dispatches case-insensitively.

## Strings

| Name | Aliases | Args | Native |
|---|---|---|---|
| `length` | — | 1 | ✅ |
| `lower` | — | 1 | ✅ |
| `upper` | — | 1 | ✅ |
| `trim` | — | 1 | ✅ |
| `contains` | — | 2 | ✅ |
| `starts_with` | — | 2 | ✅ |
| `ends_with` | — | 2 | ✅ |
| `replace` | — | 3 | ✅ |
| `split` | — | 2 | ✅ |
| `substring` | — | 3 | ✅ |
| `take` | — | 2 | ✅ |
| `first` | — | 1 | ✅ |
| `last` | — | 1 | ✅ |
| `len` | — | 1 | — |

## JSON

| Name | Aliases | Args | Native |
|---|---|---|---|
| `json_parse` | — | 1 | ✅ |
| `json_stringify` | — | 1 | ✅ |
| `set_json_field` | — | 3 | — |

## HTTP request

| Name | Aliases | Args | Native |
|---|---|---|---|
| `path_param` | — | 1 | ✅ |
| `query_param` | — | 1..2 | ✅ |
| `body` | — | 0 | ✅ |
| `header` | — | 1 | ✅ |
| `client_ip` | — | 0 | ✅ |
| `request_id` | — | 0 | ✅ |
| `response_status` | — | 0 | ✅ |
| `response_duration_ms` | — | 0 | ✅ |
| `request_path` | — | 0 | ✅ |
| `request_method` | — | 0 | ✅ |
| `request_body` | — | 0 | — |

## HTTP response

| Name | Aliases | Args | Native |
|---|---|---|---|
| `json` | — | 1 | ✅ |
| `json_unchecked` | — | 1 | ✅ |
| `text` | — | 1 | ✅ |
| `html` | — | 1 | ✅ |
| `response` | `raw` | 2 | ✅ |
| `ok` | — | 0..1 | ✅ |
| `created` | — | 1 | ✅ |
| `not_found` | — | 0..1 | ✅ |
| `no_content` | — | 0 | ✅ |
| `unauthorized` | — | 0..1 | ✅ |
| `forbidden` | — | 0..1 | ✅ |
| `internal_error` | — | 0..1 | ✅ |
| `status_code` | — | 1..2 | ✅ |
| `notFound` | — | 0..1 | ✅ |
| `noContent` | — | 0 | ✅ |
| `internalError` | — | 0..1 | ✅ |
| `statusCode` | — | 1..2 | ✅ |
| `badRequest` | — | 0..1 | ✅ |
| `bad_request` | — | 0..1 | ✅ |

## Database

| Name | Aliases | Args | Native |
|---|---|---|---|
| `setConnectionString` | — | 0..1 | ✅ |
| `raw_sql` | — | 1..2 | ✅ |
| `db_query` | — | 1 | — |
| `set_connection_string` | — | 0..1 | — |

## WebSocket

| Name | Aliases | Args | Native |
|---|---|---|---|
| `ws_send` | — | 1 | ✅ |
| `ws_recv` | — | 0 | ✅ |
| `ws_close` | — | 0 | ✅ |

## Async I/O

| Name | Aliases | Args | Native |
|---|---|---|---|
| `sleep_ms` | — | 1 | ✅ |
| `http_get` | — | 1..2 | ✅ |
| `fetch_json` | — | 1 | ✅ |
| `http_post` | — | 1..3 | — |

## Console I/O

| Name | Aliases | Args | Native |
|---|---|---|---|
| `console.write` | — | 1 | ✅ |
| `console.error` | — | 1 | ✅ |
| `console.read` | — | 0 | ✅ |

## Files + directories

| Name | Aliases | Args | Native |
|---|---|---|---|
| `file.read` | — | 1 | ✅ |
| `file.write` | — | 2 | ✅ |
| `file.append` | — | 2 | ✅ |
| `file.exists` | — | 1 | ✅ |
| `file.delete` | — | 1 | ✅ |
| `file.copy` | — | 2 | ✅ |
| `file.move` | — | 2 | ✅ |
| `file.size` | — | 1 | ✅ |
| `file.lines` | — | 1 | ✅ |
| `directory.list` | — | 1 | ✅ |
| `directory.create` | — | 1 | ✅ |
| `directory.exists` | — | 1 | ✅ |
| `directory.delete` | — | 1 | ✅ |

## Environment + coercion

| Name | Aliases | Args | Native |
|---|---|---|---|
| `env` | — | 1 | ✅ |
| `int` | — | 1 | ✅ |
| `random_int` | — | 1..2 | ✅ |
| `serve` | — | 0..1 | ✅ |

## Time + identifiers

| Name | Aliases | Args | Native |
|---|---|---|---|
| `now` | — | 0 | ✅ |
| `uuid` | — | 0 | ✅ |
| `unix_timestamp` | — | 0 | ✅ |

## Cache (in-memory)

| Name | Aliases | Args | Native |
|---|---|---|---|
| `cache_get` | — | 1 | ✅ |
| `cache_set` | — | 3 | ✅ |
| `cache_del` | — | 1 | ✅ |
| `cache_clear` | — | 0 | ✅ |

## Arrays

| Name | Aliases | Args | Native |
|---|---|---|---|
| `range` | — | 1..3 | ✅ |
| `push` | `append` | 2 | ✅ |
| `join` | — | 2 | ✅ |

## Hashing + crypto

| Name | Aliases | Args | Native |
|---|---|---|---|
| `sha256` | — | 1 | ✅ |
| `sha1` | — | 1 | ✅ |
| `md5` | — | 1 | ✅ |
| `hmac_sha256` | — | 2 | ✅ |
| `jwt_sign` | — | 2 | ✅ |
| `jwt_verify` | — | 2 | ✅ |
| `hash_password` | — | 1 | ✅ |
| `verify_password` | — | 2 | ✅ |

## Email

| Name | Aliases | Args | Native |
|---|---|---|---|
| `send_email` | — | 3 | — |

## Request context

| Name | Aliases | Args | Native |
|---|---|---|---|
| `dispatch` | — | 2 | — |
| `context` | — | 1 | — |
| `setContext` | `set_context` | 2 | — |

## Background jobs

| Name | Aliases | Args | Native |
|---|---|---|---|
| `register_job_handler` | — | 2 | — |
| `enqueue` | — | 2 | — |
| `enqueue_urgent` | — | 2 | — |
| `job_count` | — | 0 | — |
| `dlq_count` | — | 0 | — |
| `dlq_drain` | — | 0 | — |

## Notes

- The native AOT whitelist is the set of every `Name` + every `Alias` where
  **Native** is ✅. Interpreter-only builtins still run under `jwc run` but are
  rejected at `jwc build --native` time — preferred over silent miscompilation.
- For per-builtin contract (semantics, error modes, examples), see
  [`docs/spec/builtins.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/docs/spec/builtins.md).
- New builtin? Edit `src/builtins.rs::BUILTIN_DEFS`, regenerate this file via
  the command at the top, then add the runtime + AOT impls per the module docs.
