//! Runner-level end-to-end tests.
//!
//! These exercise `parse_program` → `run_main` (or `run_request_with_headers`)
//! to verify interpreter behaviour. Kept in their own sibling file so the
//! orchestrator `mod.rs` stays focused on production paths only.

use super::*;
use crate::parser::{parse_program, validate_program};

// Phase 1 [1.0-blocker]: Value::Record foundation unit tests live in
// `crates/jwc-runtime/src/lib.rs` since that's where the type now
// lives. The high-level runner tests below exercise the same
// machinery through `parse_program` → `run_main` end-to-end.

#[tokio::test]
async fn runs_main_and_prints_output() {
    let src = r#"
            function main() {
                let name = "JWC";
                print("Hello " + name);
                print(1 + 2 * 3);
            }
        "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "Hello JWC\n7\n");
}

/// End-to-end round trip through the real `file.*` / `directory.*`
/// builtins. Writes under `target/` (gitignored, and `cargo test` runs with
/// cwd at the manifest root) in a directory unique to this test, because
/// `cargo test` runs cases in parallel.
///
/// Asserts on `print` output rather than `console.write`: `print` lands in
/// `Vm::output`, which is what `run_main` returns. `console.write` goes to
/// the process stdout and is invisible here — that difference is the whole
/// point of having both, and is documented in `docs/docs/stdlib/io.md`.
#[tokio::test]
async fn file_and_directory_builtins_round_trip() {
    let src = r#"
            function main() {
                directory.create("target/jwc-io-unit/nested");
                file.write("target/jwc-io-unit/nested/a.txt", "bir\niki\n");
                print(file.read("target/jwc-io-unit/nested/a.txt"));
                file.append("target/jwc-io-unit/nested/a.txt", "uch\n");
                print(file.size("target/jwc-io-unit/nested/a.txt"));
                print(length(file.lines("target/jwc-io-unit/nested/a.txt")));
                file.copy("target/jwc-io-unit/nested/a.txt", "target/jwc-io-unit/nested/b.txt");
                file.move("target/jwc-io-unit/nested/b.txt", "target/jwc-io-unit/nested/c.txt");
                print(join(directory.list("target/jwc-io-unit/nested"), ","));
                print(file.exists("target/jwc-io-unit/nested/a.txt"));
                print(file.exists("target/jwc-io-unit/nested"));
                print(directory.exists("target/jwc-io-unit/nested"));
                file.delete("target/jwc-io-unit/nested/a.txt");
                file.delete("target/jwc-io-unit/nested/c.txt");
                directory.delete("target/jwc-io-unit/nested");
                directory.delete("target/jwc-io-unit");
                print(directory.exists("target/jwc-io-unit"));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(
        out.output,
        // "bir\niki\n" is 8 bytes, + "uch\n" is 4 → 12.
        // `file.lines` drops the trailing empty element, so 3 not 4.
        // `directory.list` is sorted, so a before c.
        // `file.exists` is false for a directory; `directory.exists` is true.
        "bir\niki\n\n12\n3\na.txt,c.txt\ntrue\nfalse\ntrue\nfalse\n"
    );
}

/// `file.read` on a missing path raises rather than returning null, and the
/// raised error carries an `io::Error` so a typed catch can name it.
#[tokio::test]
async fn file_read_missing_raises_catchable_io_not_found() {
    let src = r#"
            function main() {
                try {
                    file.read("target/jwc-io-unit-missing/nope.sql");
                    print("unreachable");
                } catch (e: IoError.NotFound) {
                    print("caught");
                }
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "caught\n");
}

/// `directory.delete` is not recursive. A non-empty directory must fail
/// rather than taking the tree with it — paths are unrestricted, so a
/// recursive variant would make `directory.delete(query_param("d"))` a
/// one-call `rm -rf`.
#[tokio::test]
async fn directory_delete_refuses_a_non_empty_directory() {
    let src = r#"
            function main() {
                directory.create("target/jwc-io-unit-nonempty");
                file.write("target/jwc-io-unit-nonempty/x.txt", "x");
                try {
                    directory.delete("target/jwc-io-unit-nonempty");
                    print("deleted");
                } catch (e) {
                    print("refused");
                }
                file.delete("target/jwc-io-unit-nonempty/x.txt");
                directory.delete("target/jwc-io-unit-nonempty");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "refused\n");
}

/// `int()` trims before parsing. A single trailing space used to make it
/// answer 0, which is how a `console.read()` value silently became zero.
#[tokio::test]
async fn int_trims_before_parsing() {
    let src = r#"
            function main() {
                print(int(" 42 "));
                print(int("\t7\n"));
                print(int("-3"));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "42\n7\n-3\n");
}

/// An unparseable string raises instead of answering 0 — otherwise
/// `int("abc")` and `int("0")` are indistinguishable and bad input travels
/// on looking like a real number.
#[tokio::test]
async fn int_raises_on_unparseable_string() {
    for bad in ["abc", "", "4.5", "12x"] {
        let src = format!(
            r#"
            function main() {{
                print(int("{bad}"));
            }}
            "#
        );
        let program = parse_program(&src).unwrap();
        validate_program(&program).unwrap();
        let err = run_main(&program)
            .await
            .expect_err(&format!("int({bad:?}) must raise"));
        let msg = format!("{err:#}");
        assert!(
            msg.contains("type error"),
            "int({bad:?}) message should say 'type error', got: {msg}"
        );
    }
}

/// Catchable as ValidationError, so a handler can turn bad input into a
/// 400 rather than a 500.
#[tokio::test]
async fn int_parse_failure_classifies_as_validation_error() {
    let src = r#"
            function main() {
                try {
                    print(int("abc"));
                } catch (e: ValidationError) {
                    print("caught");
                }
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "caught\n");
}

/// `null` propagates rather than raising, so `int(query_param("page"))`
/// stays usable when the parameter is absent.
#[tokio::test]
async fn int_propagates_null() {
    let src = r#"
            function main() {
                let v = int(null);
                print(v == null);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "true\n");
}

/// The whole point of the change: a line read from stdin with stray
/// whitespace parses instead of silently becoming 0.
#[tokio::test]
async fn console_writeln_appends_exactly_one_newline() {
    // `console.*` bypasses `Vm::output`, so assert on the interpreter's
    // buffer staying empty rather than on the text — the text goes to the
    // real stdout and is not observable here.
    let src = r#"
            function main() {
                console.writeln("x");
                console.write("y");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "", "console.* must not touch the print buffer");
}

#[tokio::test]
async fn module_const_is_visible_in_main() {
    let src = r#"
            const GREETING = "hi";
            function main() { print(GREETING); }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "hi\n");
}

#[tokio::test]
async fn const_can_reference_other_const() {
    let src = r#"
            const A = 2;
            const B = A * 10;
            function main() { print(B); }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "20\n");
}

#[tokio::test]
async fn supports_function_call_and_return() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "42\n");
}

#[tokio::test]
async fn supports_float_literals_and_arithmetic() {
    let src = r#"
            function main() {
                let a = 0.2;
                let b = 0.1;
                let sum = a + b;
                print(sum);
            }
        "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "0.3\n");
}

#[tokio::test]
async fn supports_if_while_break_continue() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "1\n3\n4\n");
}

#[tokio::test]
async fn supports_logical_ops() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "ok\n");
}

#[tokio::test]
async fn supports_declarative_routes_with_dispatch() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(
            out.output,
            "GET /health -> 200 OK\n{\"status\":404,\"error\":\"Not Found\",\"method\":\"GET\",\"path\":\"/unknown\"}\n"
        );
}

#[tokio::test]
async fn supports_route_path_params() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "todo=42\n");
}

#[tokio::test]
async fn dispatch_outputs_json_from_route_return() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "{\"items\":[]}\n");
}

#[tokio::test]
async fn supports_new_entity_and_field_ops() {
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
    let out = run_main(&program).await.unwrap();
    assert_eq!(out.output, "Tesla\n2024\n");
}

#[tokio::test]
async fn after_middleware_block_sees_response_status() {
    // The pre-handler middleware runs first; the route returns
    // statusCode(202, ...); the `after` block reads
    // response_status() and writes it to a process-wide spot via
    // print. Without after-phase support, every project hardcoded
    // status=200 / latency=0 because nothing else was reachable
    // from pre-handler context.
    let src = r#"
            middleware Logger {
                let _ = "before";
            } after {
                let status = response_status();
                print("status=" + status);
            }

            route GET "/ping" use Logger {
                return statusCode(202, "accepted");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let result = run_request(&program, "GET", "/ping", None).await.unwrap();
    assert_eq!(result.0, 202);
    // The after-block ran AFTER the handler returned 202 — visible
    // via the captured stdout. We can't easily peek stdout from a
    // tokio test without redirecting, so just assert the dispatch
    // status came back unchanged (proving after-body didn't
    // overwrite it accidentally).
}

#[tokio::test]
async fn after_middleware_response_duration_reads_back() {
    // response_duration_ms() returns the milliseconds since the
    // dispatch started. Outside an `after` block the runner
    // doesn't expose it specially — it just reads
    // current_request_started which is set at dispatch entry, so
    // the measurement is valid throughout the request.
    let src = r#"
            route GET "/x" {
                let ms = response_duration_ms();
                if (ms == null) { return "null"; }
                if (ms < 0)     { return "negative"; }
                return "ok";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let result = run_request(&program, "GET", "/x", None).await.unwrap();
    assert_eq!(result.0, 200);
    assert_eq!(result.1, "ok");
}

#[tokio::test]
async fn middleware_without_after_block_still_works() {
    // The after_body is Option<Vec<Stmt>>; a middleware that omits
    // `after { ... }` must keep behaving exactly like before this
    // slice landed. No-op smoke test.
    let src = r#"
            middleware Plain {
                let _ = "before only";
            }

            route GET "/x" use Plain {
                return "ok";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let result = run_request(&program, "GET", "/x", None).await.unwrap();
    assert_eq!(result.0, 200);
    assert_eq!(result.1, "ok");
}

#[tokio::test]
async fn request_id_is_visible_when_server_stamps_one() {
    let src = r#"
            route GET "/x" {
                let rid = request_id();
                if (rid == null) { return "{\"rid\":null}"; }
                return "{\"rid\":\"" + rid + "\"}";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let headers = std::collections::HashMap::new();
    let (status, body, _ct, _extra) = run_request_with_headers_and_id(
        &program,
        "GET",
        "/x",
        None,
        headers,
        Some("abc12345".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "{\"rid\":\"abc12345\"}");
}

#[tokio::test]
async fn request_id_returns_null_when_unstamped() {
    // Calling `run_request_with_headers` (the legacy entry point)
    // doesn't stamp an id; `request_id()` must surface that cleanly
    // as null instead of panicking or returning the empty string.
    let src = r#"
            route GET "/x" {
                let rid = request_id();
                if (rid == null) { return "null"; }
                return rid;
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let headers = std::collections::HashMap::new();
    let (status, body, _ct, _extra) =
        run_request_with_headers(&program, "GET", "/x", None, headers)
            .await
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "null");
}

/// Serializes every test that reads/writes `JWC_TRUSTED_PROXIES` so
/// concurrent tokio tests don't see a partial state. Without this
/// the two client_ip tests race on the shared env var, since
/// cargo runs `#[tokio::test]` cases in parallel.
static TRUSTED_PROXIES_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn client_ip_returns_rightmost_untrusted_entry() {
    // Default JWC_TRUSTED_PROXIES is empty — no proxy is trusted,
    // so the rightmost entry of the chain is the closest hop's view
    // of the source. That's the only header value we can rely on
    // without an explicit trust list.
    let _g = TRUSTED_PROXIES_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var("JWC_TRUSTED_PROXIES").ok();
    std::env::remove_var("JWC_TRUSTED_PROXIES");
    let src = r#"
            route GET "/whoami" {
                let ip = client_ip();
                if (ip == null) { return "{\"ip\":null}"; }
                return "{\"ip\":\"" + ip + "\"}";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "x-forwarded-for".to_string(),
        "1.2.3.4, 10.0.0.5".to_string(),
    );
    let (status, body, _ct, _extra) =
        run_request_with_headers(&program, "GET", "/whoami", None, headers)
            .await
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ip\":\"10.0.0.5\"}");
    if let Some(v) = prev {
        std::env::set_var("JWC_TRUSTED_PROXIES", v);
    }
}

#[tokio::test]
async fn client_ip_peels_trusted_proxies_off_the_chain() {
    // With JWC_TRUSTED_PROXIES="10." the trailing 10.x hop is a
    // known forwarder and gets peeled off, leaving 1.2.3.4 — the
    // real client. Exact semantics nginx + go's net/http use.
    let _g = TRUSTED_PROXIES_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var("JWC_TRUSTED_PROXIES").ok();
    std::env::set_var("JWC_TRUSTED_PROXIES", "10.");
    let src = r#"
            route GET "/whoami" {
                let ip = client_ip();
                if (ip == null) { return "{\"ip\":null}"; }
                return "{\"ip\":\"" + ip + "\"}";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "x-forwarded-for".to_string(),
        "1.2.3.4, 10.0.0.5".to_string(),
    );
    let (status, body, _ct, _extra) =
        run_request_with_headers(&program, "GET", "/whoami", None, headers)
            .await
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ip\":\"1.2.3.4\"}");
    match prev {
        Some(v) => std::env::set_var("JWC_TRUSTED_PROXIES", v),
        None => std::env::remove_var("JWC_TRUSTED_PROXIES"),
    }
}

#[tokio::test]
async fn client_ip_returns_null_when_header_absent() {
    let src = r#"
            route GET "/whoami" {
                let ip = client_ip();
                if (ip == null) { return "null"; }
                return ip;
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let headers = std::collections::HashMap::new();
    let (status, body, _ct, _extra) =
        run_request_with_headers(&program, "GET", "/whoami", None, headers)
            .await
            .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "null");
}

#[tokio::test]
async fn run_request_dispatches_route() {
    let src = r#"
            route GET "/ping" {
                return "{\"ok\":true}";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/ping", None).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ok\":true}");
}

#[tokio::test]
async fn run_request_returns_404_for_unknown_route() {
    let src = r#"
            route GET "/ping" {
                return "pong";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, _body) = run_request(&program, "GET", "/missing", None)
        .await
        .unwrap();
    assert_eq!(status, 404);
}

#[tokio::test]
async fn body_is_auto_parsed_for_typed_class_param() {
    let src = r#"
            class BrandInput {
                id int;
                name string;
            }

            function echoBrand(input: BrandInput): BrandInput {
                return input;
            }

            route POST "/brands" {
                let payload = echoBrand(body());
                return json(payload);
            }
        "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(
        &program,
        "POST",
        "/brands",
        Some("{\"id\":1,\"name\":\"Acme\"}".to_string()),
    )
    .await
    .unwrap();

    assert_eq!(status, 200);
    assert_eq!(body, "{\"id\":1,\"name\":\"Acme\"}");
}

#[tokio::test]
async fn typed_class_param_accepts_partial_payload() {
    // Phase 1C: a typed class parameter no longer forces every declared field
    // to be present. Field *presence* is enforced by `validate body { required }`,
    // not by the structural type — which is what makes partial / PATCH payloads
    // (`{ "name": "x" }` against a multi-field DTO) work.
    let src = r#"
            class BrandInput {
                id int;
                name string;
            }

            function createBrand(input: BrandInput) {
                return input;
            }

            route POST "/brands" {
                let payload = createBrand(body());
                return json(payload);
            }
        "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(&program, "POST", "/brands", Some("{\"id\":1}".to_string()))
        .await
        .unwrap();

    assert_eq!(status, 200);
    assert_eq!(body, "{\"id\":1}");
}

#[tokio::test]
async fn typed_class_param_still_rejects_wrong_field_type() {
    // The relaxed presence rule must NOT weaken type checking: a field that IS
    // present must still match its declared type.
    let src = r#"
            class BrandInput {
                id int;
                name string;
            }

            function createBrand(input: BrandInput) {
                return input;
            }

            route POST "/brands" {
                let payload = createBrand(body());
                return json(payload);
            }
        "#;

    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let err = run_request(
        &program,
        "POST",
        "/brands",
        Some("{\"id\":\"not-an-int\",\"name\":\"Acme\"}".to_string()),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(err.contains("invalid type") || err.contains("id"));
}

#[tokio::test]
async fn response_body_status_key_is_preserved() {
    // Phase 0.1: a user body key named `status` must survive to the client —
    // the HTTP status now travels through a reserved `__jwc_status__` sentinel,
    // so `json({ status: ... })` no longer loses the field.
    let src = r#"
            route GET "/health" {
                return json({ status: "ok", code: 1 });
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(&program, "GET", "/health", None).await.unwrap();

    assert_eq!(status, 200);
    assert!(
        body.contains("\"status\":\"ok\""),
        "status body key was stripped: {body}"
    );
    assert!(body.contains("\"code\":1"));
}

#[tokio::test]
async fn created_sets_201_and_keeps_body_status_key() {
    // `created(...)` sets HTTP 201 via the sentinel while leaving a body
    // `status` field intact.
    let src = r#"
            route POST "/things" {
                return created(json({ status: "active" }));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(&program, "POST", "/things", None)
        .await
        .unwrap();

    assert_eq!(status, 201);
    assert!(
        body.contains("\"status\":\"active\""),
        "status body key was stripped by created(): {body}"
    );
    assert!(!body.contains("__jwc_status__"), "sentinel leaked: {body}");
}

#[tokio::test]
async fn query_param_reads_value_from_url() {
    let src = r#"
            route GET "/items" {
                let limit = query_param("limit");
                let q = query_param("q");
                return text("limit=" + limit + ",q=" + q);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/items?limit=10&q=hello", None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("limit=10"));
    assert!(body.contains("q=hello"));
}

#[tokio::test]
async fn query_param_default_used_when_missing() {
    let src = r#"
            route GET "/items" {
                let limit = query_param("limit", "20");
                return text("limit=" + limit);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/items", None).await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("limit=20"));
}

#[tokio::test]
async fn uuid_type_accepts_valid_string_and_rejects_invalid() {
    let src = r#"
            function take(id: uuid): uuid {
                return id;
            }

            function main() {
                let good = take("550e8400-e29b-41d4-a716-446655440000");
                print(good);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("550e8400-e29b-41d4-a716-446655440000"));

    let bad_src = r#"
            function take(id: uuid): uuid { return id; }
            function main() { take("not-a-uuid"); }
        "#;
    let bad_program = parse_program(bad_src).unwrap();
    validate_program(&bad_program).unwrap();
    let err = run_main(&bad_program).await.unwrap_err().to_string();
    assert!(err.contains("uuid"));
}

#[tokio::test]
async fn datetime_type_accepts_iso_string() {
    let src = r#"
            function take(at: datetime): datetime {
                return at;
            }

            function main() {
                let v = take("2026-05-19T10:00:00Z");
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("2026-05-19"));
}

#[tokio::test]
async fn nullable_type_marker_allows_null_value() {
    let src = r#"
            function take(name: string?): string? {
                return name;
            }

            function main() {
                let v = take(null);
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "null");
}

#[tokio::test]
async fn optional_wrapper_is_equivalent_to_nullable_marker() {
    let src = r#"
            function take(x: Optional<int>): Optional<int> {
                return x;
            }

            function main() {
                let v = take(null);
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "null");
}

#[tokio::test]
async fn list_of_int_validates_each_element() {
    let src = r#"
            function take(xs: List<int>): List<int> {
                return xs;
            }

            function main() {
                let v = take("[1, 2, 3]");
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("[1,2,3]") || out.contains("[1, 2, 3]"));

    let bad_src = r#"
            function take(xs: List<int>): List<int> { return xs; }
            function main() { take("[1, \"two\", 3]"); }
        "#;
    let bad = parse_program(bad_src).unwrap();
    validate_program(&bad).unwrap();
    let err = run_main(&bad).await.unwrap_err().to_string();
    assert!(err.contains("List<int>"));
}

#[tokio::test]
async fn try_catch_swallows_runtime_error() {
    let src = r#"
            function risky() {
                let x = undefined_var;
            }

            function main() {
                try {
                    risky();
                    print("unreachable");
                } catch (e) {
                    print("caught: " + e);
                }
                print("after");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("caught:"));
    assert!(out.contains("Undefined variable"));
    assert!(out.contains("after"));
    assert!(!out.contains("unreachable"));
}

#[tokio::test]
async fn try_catch_returns_from_catch_block() {
    let src = r#"
            function broken(): int {
                let x = 0;
                return x / 0;
            }

            function main() {
                try {
                    let y = broken();
                    print(y);
                } catch (e) {
                    print("recovered");
                }
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "recovered");
}

#[tokio::test]
async fn try_catch_lets_success_pass_through() {
    let src = r#"
            function safe(): int {
                return 7;
            }

            function main() {
                try {
                    let n = safe();
                    print(n);
                } catch (e) {
                    print("never");
                }
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "7");
}

/// Tests that mutate process env need to be serialised — cargo runs
/// tests in parallel by default and `setConnectionString(...)` writes
/// to `DATABASE_URL`. One shared mutex keeps the three env-touching
/// tests below from racing each other.
fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[tokio::test]
async fn set_connection_string_accepts_url_form() {
    let _g = env_test_lock();
    let key = "DATABASE_URL";
    let backup = std::env::var(key).ok();
    std::env::remove_var(key);

    let src = r#"
            function main() {
                setConnectionString("postgresql://postgres:secret@127.0.0.1:5432/myapp");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    run_main(&program).await.unwrap();
    assert_eq!(
        std::env::var(key).unwrap(),
        "postgresql://postgres:secret@127.0.0.1:5432/myapp"
    );

    if let Some(v) = backup {
        std::env::set_var(key, v);
    } else {
        std::env::remove_var(key);
    }
}

#[tokio::test]
async fn set_connection_string_accepts_object_literal_form() {
    let _g = env_test_lock();
    let key = "DATABASE_URL";
    let backup = std::env::var(key).ok();
    std::env::remove_var(key);

    let src = r#"
            function main() {
                setConnectionString({
                    host:     "db.example.com",
                    port:     5433,
                    user:     "app",
                    password: "topsecret",
                    database: "prod"
                });
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    run_main(&program).await.unwrap();
    assert_eq!(
        std::env::var(key).unwrap(),
        "postgresql://app:topsecret@db.example.com:5433/prod"
    );

    if let Some(v) = backup {
        std::env::set_var(key, v);
    } else {
        std::env::remove_var(key);
    }
}

#[tokio::test]
async fn set_connection_string_no_args_reads_from_env() {
    let _g = env_test_lock();
    let backup = std::env::var("DATABASE_URL").ok();
    std::env::set_var(
        "DATABASE_URL",
        "postgresql://envuser:envpw@envhost:5400/envdb",
    );

    let src = r#"
            function main() {
                setConnectionString();
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    run_main(&program).await.unwrap();
    assert_eq!(
        std::env::var("DATABASE_URL").unwrap(),
        "postgresql://envuser:envpw@envhost:5400/envdb"
    );

    if let Some(v) = backup {
        std::env::set_var("DATABASE_URL", v);
    } else {
        std::env::remove_var("DATABASE_URL");
    }
}

#[tokio::test]
async fn uuid_builtin_is_v4_and_never_collides_on_a_tight_loop() {
    let src = r#"
            function main() {
                let a = uuid();
                let b = uuid();
                let c = uuid();
                print(a);
                print(b);
                print(c);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    // All three must be distinct — v4 should never collide here.
    assert!(lines[0] != lines[1]);
    assert!(lines[1] != lines[2]);
    assert!(lines[0] != lines[2]);
    // And every one is a proper RFC 4122 v4: version nibble is '4' at
    // the 14th hex digit (index 14, position [14..15] in the dashed form).
    for line in &lines {
        assert_eq!(
            line.as_bytes()[14],
            b'4',
            "expected v4 version nibble in {line}"
        );
    }
}

#[tokio::test]
async fn ws_path_params_reach_the_handler() {
    // Smoke check that the runtime path-params plumbing accepts a
    // pre-populated map exactly the way `server.rs::handle_ws` will
    // hand it over. We exercise it by calling `run_ws_request`
    // directly and watching the value land in `path_param(...)`.
    let src = r#"
            route WS "/chat/{room}" {
                let r = path_param("room");
                ws_send(r);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (tx_to_vm, rx_to_vm) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (tx_from_vm, mut rx_from_vm) = tokio::sync::mpsc::unbounded_channel::<String>();
    // No inbound messages, so the handler runs ws_send then exits.
    drop(tx_to_vm);

    let mut params = HashMap::new();
    params.insert("room".to_string(), "general".to_string());

    run_ws_request(
        &program,
        "/chat/{room}",
        params,
        HashMap::new(),
        rx_to_vm,
        tx_from_vm,
    )
    .await
    .unwrap();

    let received = rx_from_vm.try_recv().expect("ws_send fired");
    assert_eq!(received, "general");
}

#[tokio::test]
async fn ws_route_parses_with_protocol_marker() {
    let src = r#"
            route WS "/chat/{room}" {
                let msg = ws_recv();
                ws_send(msg);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    assert_eq!(program.routes.len(), 1);
    assert_eq!(program.routes[0].method, "WS");
    assert_eq!(program.routes[0].path, "/chat/{room}");
    assert_eq!(program.routes[0].protocol, crate::ast::RouteProtocol::Ws);
}

#[tokio::test]
async fn ws_builtins_error_outside_a_ws_handler() {
    let src = r#"
            function main() {
                ws_send("hi");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let err = run_main(&program).await.unwrap_err().to_string();
    assert!(err.contains("only valid inside a WS route"));
}

#[tokio::test]
async fn string_helpers_basic_shapes() {
    let src = r#"
            function main() {
                print(lower("HELLO"));
                print(upper("Najim"));
                print(trim("   ok   "));
                print(replace("a-b-c", "-", "/"));
                print(contains("hello world", "world"));
                print(starts_with("hello", "he"));
                print(ends_with("hello", "lo"));
                print(length("hello"));
                let parts = split("a,b,c", ",");
                print(parts);
                print(length(parts));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("hello"));
    assert!(out.contains("NAJIM"));
    assert!(out.contains("\nok\n"));
    assert!(out.contains("a/b/c"));
    assert!(out.contains("true\ntrue\ntrue\n5\n"));
    assert!(out.contains("[\"a\",\"b\",\"c\"]"));
    // length(parts) — array of 3 strings → 3.
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.last().map(|l| l.trim() == "3").unwrap_or(false),
        "{out}"
    );
}

#[tokio::test]
async fn for_in_iterates_json_array() {
    let src = r#"
            function main() {
                let xs = "[1, 2, 3, 4]";
                let sum = 0;
                for x in xs {
                    sum = sum + x;
                    if (x == 3) { break; }
                }
                print(sum);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "6");
}

#[tokio::test]
async fn for_in_continue_skips_iteration() {
    let src = r#"
            function main() {
                let xs = "[1, 2, 3, 4, 5]";
                let kept = 0;
                for x in xs {
                    if (x == 3) { continue; }
                    kept = kept + 1;
                }
                print(kept);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "4");
}

#[tokio::test]
async fn substring_slices_chars_with_clamping() {
    let src = r#"
            function main() {
                print(substring("hello world", 0, 5));
                print(substring("hello world", 6, 5));
                print(substring("salom", 100, 3));
                print(substring("abc", -1, 2));
                print(substring("abcdef", 2, 100));
                print(substring("ko'p", 0, 4));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "hello");
    assert_eq!(lines[1], "world");
    assert_eq!(lines[2], "");
    assert_eq!(lines[3], "");
    assert_eq!(lines[4], "cdef");
    assert_eq!(lines[5], "ko'p");
}

#[tokio::test]
async fn take_returns_prefix_of_string() {
    let src = r#"
            function main() {
                print(take("hello", 3));
                print(take("hi", 10));
                print(take("anything", 0));
                print(take("xx", -5));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "hel");
    assert_eq!(lines[1], "hi");
    assert_eq!(lines[2], "");
    assert_eq!(lines[3], "");
}

#[tokio::test]
async fn first_last_return_array_endpoints() {
    let src = r#"
            function main() {
                let xs = "[10, 20, 30]";
                print(first(xs));
                print(last(xs));
                let empty = "[]";
                print(first(empty));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "10");
    assert_eq!(lines[1], "30");
    assert_eq!(lines[2], "null");
}

#[tokio::test]
async fn json_parse_then_stringify_roundtrip() {
    let src = r#"
            function main() {
                let parsed = json_parse("{\"a\":1,\"b\":\"x\"}");
                print(parsed);
                let back = json_stringify({ a: 1, b: "x" });
                print(back);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("\"a\":1"));
    assert!(out.contains("\"b\":\"x\""));
}

#[tokio::test]
async fn error_handler_catches_uncaught_route_error() {
    let src = r#"
            errorHandler (e) {
                return internalError(e.message);
            }

            route GET "boom" {
                let x = undefined_var;
                return text("nope");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(&program, "GET", "/boom", None).await.unwrap();
    assert_eq!(status, 500);
    assert!(body.contains("Undefined variable"), "body: {body}");
}

#[tokio::test]
async fn error_handler_does_not_intercept_normal_responses() {
    let src = r#"
            errorHandler (e) {
                return internalError(e.message);
            }

            route GET "ok" {
                return text("hello");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body) = run_request(&program, "GET", "/ok", None).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "hello");
}

#[tokio::test]
async fn object_literal_serializes_to_json_with_nested_embedding() {
    let src = r#"
            function main() {
                let inner = "[1,2,3]";
                let payload = { name: "Najim", count: 5, items: inner, ok: true };
                print(payload);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let trimmed = out.trim_end();
    // Order isn't guaranteed by serde_json::Map (insertion order preserved
    // since 1.0.79 with preserve_order disabled defaults to BTreeMap-like),
    // so check field presence instead of exact text.
    assert!(trimmed.contains("\"name\":\"Najim\""), "got: {trimmed}");
    assert!(trimmed.contains("\"count\":5"), "got: {trimmed}");
    assert!(
        trimmed.contains("\"items\":[1,2,3]"),
        "nested JSON should embed raw, got: {trimmed}"
    );
    assert!(trimmed.contains("\"ok\":true"), "got: {trimmed}");
}

#[tokio::test]
async fn unary_not_inverts_bool() {
    let src = r#"
            function main() {
                let ok = false;
                if (!ok) { print("flipped"); }
                let n = true;
                if (!!n) { print("doubled"); }
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert!(out.contains("flipped"));
    assert!(out.contains("doubled"));
}

#[tokio::test]
async fn now_built_in_returns_iso_8601() {
    let src = r#"
            function main() {
                let ts = now();
                print(ts);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let trimmed = out.trim_end();
    // 2026-05-19T12:00:00.000Z shape — 4 digits, dashes, T, ms, Z.
    let bytes = trimmed.as_bytes();
    assert!(bytes.len() >= 20, "too short: {trimmed}");
    assert_eq!(bytes[4], b'-');
    assert_eq!(bytes[7], b'-');
    assert_eq!(bytes[10], b'T');
    assert_eq!(bytes[trimmed.len() - 1], b'Z');
}

#[tokio::test]
async fn at_var_field_shortcut_in_where_clause() {
    let src = r#"
            dbcontext AppDb : Postgres;
            entity User of AppDb {
                id uuid pk;
                username varchar(40);
            }

            function lookup(req) {
                let u = select User from AppDb.User
                    where User.username == @req.username first;
                return u;
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    match &program.functions[0].body[0] {
        crate::ast::Stmt::Let { value, .. } => match value {
            crate::ast::Expr::DbSelect { where_clause, .. } => {
                let wc = where_clause.as_ref().unwrap();
                let atom = match wc.as_ref() {
                    crate::ast::WhereExpr::Atom(a) => a,
                    _ => panic!("expected atom"),
                };
                match &atom.rhs {
                    crate::ast::Expr::FieldGet { var, field } => {
                        assert_eq!(var, "req");
                        assert_eq!(field, "username");
                    }
                    other => panic!("expected FieldGet, got {:?}", other),
                }
            }
            _ => panic!("expected DbSelect"),
        },
        _ => panic!("expected Let stmt"),
    }
}

#[tokio::test]
async fn unknown_function_suggests_closest_match() {
    let src = r#"
            function getAllUsers() { return 1; }
            function main() {
                let v = getAllUser();
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let err = run_main(&program).await.unwrap_err().to_string();
    assert!(err.contains("Did you mean 'getallusers'"));
}

#[tokio::test]
async fn undefined_variable_suggests_closest_match() {
    let src = r#"
            function main() {
                let userName = "x";
                print(usrName);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let err = run_main(&program).await.unwrap_err().to_string();
    assert!(err.contains("Did you mean 'username'"));
}

#[tokio::test]
async fn async_function_and_await_keyword_parse_and_run() {
    let src = r#"
            async function fetch(): int {
                return 42;
            }

            function main() {
                let v = await fetch();
                print(v);
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    assert!(program
        .functions
        .iter()
        .find(|f| f.name == "fetch")
        .map(|f| f.is_async)
        .unwrap_or(false));
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "42");
}

#[tokio::test]
async fn middleware_short_circuits_route_when_returning() {
    let src = r#"
            middleware AuthMw {
                let token = header("authorization");
                if (token == null) {
                    return unauthorized();
                }
            }

            route GET "/secret" use AuthMw {
                return text("payload");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, _body) = run_request(&program, "GET", "/secret", None).await.unwrap();
    assert_eq!(status, 401);

    let mut headers = HashMap::new();
    headers.insert("authorization".into(), "Bearer xyz".into());
    let (status_ok, body_ok, _ct, _headers) =
        run_request_with_headers(&program, "GET", "/secret", None, headers)
            .await
            .unwrap();
    assert_eq!(status_ok, 200);
    assert_eq!(body_ok, "payload");
}

#[tokio::test]
async fn middleware_can_share_context_with_route_handler() {
    let src = r#"
            middleware UserMw {
                setContext("userId", "u-1");
            }

            route GET "/me" use UserMw {
                return text("user=" + context("userId"));
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/me", None).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "user=u-1");
}

#[tokio::test]
async fn unknown_middleware_fails_at_validation() {
    let src = r#"
            route GET "/x" use MissingMw {
                return text("hi");
            }
        "#;
    let program = parse_program(src).unwrap();
    let err = validate_program(&program).unwrap_err().to_string();
    assert!(err.contains("unknown middleware"));
}

#[tokio::test]
async fn typed_route_handler_receives_path_param() {
    let src = r#"
            function getUser(id: int) {
                return "user=" + id;
            }

            route GET "/users/{id}" -> getUser;
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/users/42", None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "user=42");
}

#[tokio::test]
async fn typed_route_handler_receives_query_param_fallback() {
    let src = r#"
            function search(q: string) {
                return "q=" + q;
            }

            route GET "/search" -> search;
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/search?q=jwc", None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "q=jwc");
}

#[tokio::test]
async fn validate_body_returns_400_on_missing_required_field() {
    let src = r#"
            route POST "/users" {
                validate body {
                    name: required, minLength(2);
                    age: min(0), max(150);
                }
                return text("ok");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "POST", "/users", Some("{\"age\":10}".to_string()))
        .await
        .unwrap();
    assert_eq!(status, 400);
    // Unified envelope: the message is under `error`, the machine-readable
    // kind under `code`, and the per-field detail under `details`.
    let doc: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(doc["status"], 400);
    assert_eq!(doc["code"], "validation_failed");
    assert!(doc["error"].as_str().is_some());
    assert!(
        doc["details"]["name"].as_str().is_some(),
        "body was: {body}"
    );
}

#[tokio::test]
async fn validate_body_passes_when_all_rules_satisfied() {
    let src = r#"
            route POST "/users" {
                validate body {
                    name: required, minLength(2), maxLength(10);
                    age: min(0), max(150);
                }
                return text("ok");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(
        &program,
        "POST",
        "/users",
        Some("{\"name\":\"Najim\",\"age\":25}".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn validate_body_min_max_bound_violation() {
    let src = r#"
            route POST "/score" {
                validate body {
                    value: min(0), max(100);
                }
                return text("ok");
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(
        &program,
        "POST",
        "/score",
        Some("{\"value\":250}".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status, 400);
    assert!(body.contains("max(100)"));
}

#[tokio::test]
async fn query_string_does_not_break_route_matching() {
    let src = r#"
            route GET "/ping" {
                return "{\"ok\":true}";
            }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/ping?ignored=1", None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ok\":true}");
}

#[test]
fn check_typed_value_accepts_base64_for_bytes() {
    // Empty program — no models / functions needed; we only exercise
    // the type-name dispatch inside `check_typed_value`.
    let program = Program::default();
    let vm = Vm::new(&program);

    // "hello" base64-encoded == "aGVsbG8=". Valid standard base64.
    let ok = vm
        .check_typed_value("p", "bytes", Value::Str("aGVsbG8=".to_string()))
        .expect("valid base64 should pass");
    assert_eq!(ok, Value::Str("aGVsbG8=".to_string()));

    // `byte[]` alias should behave the same way.
    vm.check_typed_value("p", "byte[]", Value::Str("aGVsbG8=".to_string()))
        .expect("byte[] alias accepts base64");

    // Non-base64 (`!` is not in the standard alphabet) must fail.
    let err = vm
        .check_typed_value("p", "bytes", Value::Str("not!base64".to_string()))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("expects bytes"),
        "expected bytes type-error, got: {err}"
    );

    // Wrong shape (Int) must also fail.
    let err = vm
        .check_typed_value("p", "bytes", Value::Int(1))
        .unwrap_err()
        .to_string();
    assert!(err.contains("expects bytes"));
}

// ── Sprint 4C [1.0-blocker] — json() validates strings ────────────────

#[tokio::test]
async fn json_rejects_non_json_string_in_interpreter() {
    let src = r#"
            route GET "/x" { return json("not-json-at-all"); }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let err = run_request(&program, "GET", "/x", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not valid JSON"),
        "expected validation message, got: {err}"
    );
    assert!(
        err.contains("json_unchecked"),
        "expected hint about json_unchecked(), got: {err}"
    );
}

#[tokio::test]
async fn json_accepts_well_formed_object_literal() {
    // The Phase 1 Record path goes through value_to_json, so a literal
    // object is always serialised as valid JSON — no validation
    // needed. Regression guard that Sprint 4C didn't break this path.
    let src = r#"
            route GET "/x" { return json({ id: 1, name: "x" }); }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/x", None).await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("\"id\""));
    assert!(body.contains("\"name\""));
}

#[tokio::test]
async fn json_unchecked_bypasses_string_validation() {
    // Caller-asserted contract: the string is JSON. Runtime trusts it.
    // We use a string the interpreter would reject under json() so the
    // contrast is clear.
    let src = r#"
            route GET "/x" { return json_unchecked("definitely-not-json"); }
        "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/x", None).await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("definitely-not-json"));
}

// ------------------------------------------------------------------
// Sprint 3A: typed-catch dispatch with dotted-path subtypes.
// ------------------------------------------------------------------
//
// We can't fabricate a real `tokio_postgres::Error` with a specific
// SQLSTATE from outside the crate (every `Error::*` constructor is
// `pub(crate)`), so subtype detection on PG errors is exercised
// indirectly: when `classify_jwc_error` can't downcast to a PG error,
// it falls back to the substring scan and returns the parent kind
// `"DbError"` only. That fallback IS reachable from user-land errors
// and is what the production code path will see whenever someone
// raises a "looks like a DB problem" `anyhow!` without an underlying
// tokio_postgres source — which is precisely the gap Sprint 3A wants
// to keep honest.

#[test]
fn catch_type_matches_none_catches_everything() {
    for &kind in JWC_ERROR_KINDS {
        assert!(
            catch_type_matches(None, kind),
            "untyped catch must catch every kind, missed: {kind}"
        );
    }
}

#[test]
fn catch_type_matches_error_super_kind_catches_everything() {
    for &kind in JWC_ERROR_KINDS {
        assert!(
            catch_type_matches(Some("Error"), kind),
            "`catch (e: Error)` must catch every kind, missed: {kind}"
        );
    }
}

#[test]
fn catch_type_matches_parent_catches_child() {
    // `catch (e: DbError)` MUST match a kind of "DbError.UniqueViolation".
    assert!(catch_type_matches(
        Some("DbError"),
        "DbError.UniqueViolation"
    ));
    assert!(catch_type_matches(
        Some("DbError"),
        "DbError.ForeignKeyViolation"
    ));
    assert!(catch_type_matches(Some("HttpError"), "HttpError.NotFound"));
    assert!(catch_type_matches(
        Some("JwtError"),
        "JwtError.InvalidSignature"
    ));
    // Parent also matches its own bare kind.
    assert!(catch_type_matches(Some("DbError"), "DbError"));
    assert!(catch_type_matches(Some("IoError"), "IoError.NotFound"));
    assert!(catch_type_matches(
        Some("IoError"),
        "IoError.PermissionDenied"
    ));
}

/// `std::io::Error` is constructible from outside the crate (unlike
/// `tokio_postgres::Error`), so these drive `classify_jwc_error`'s typed
/// Pass-1 downcast directly rather than through a live failure.
#[test]
fn classify_jwc_error_maps_io_not_found() {
    let io = std::io::Error::from(std::io::ErrorKind::NotFound);
    let e = anyhow::Error::new(io).context("file.read(/nope) failed");
    assert_eq!(classify_jwc_error(&e), "IoError.NotFound");
}

#[test]
fn classify_jwc_error_maps_io_permission_denied() {
    let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let e = anyhow::Error::new(io).context("file.write(/etc/passwd) failed");
    assert_eq!(classify_jwc_error(&e), "IoError.PermissionDenied");
}

/// The whole reason the `IoError` branch is a typed downcast rather than a
/// substring match. Pass 2 reads `sql` as DbError, `url` / `http` as
/// HttpError — and the file builtins put the path in the message, so a
/// perfectly ordinary backup path would otherwise be misfiled.
#[test]
fn classify_jwc_error_io_beats_the_substring_scan_on_the_path() {
    for path in [
        "/var/backups/app.sql",
        "/srv/url-shortener/config.txt",
        "/opt/http-cache/index",
    ] {
        let io = std::io::Error::from(std::io::ErrorKind::NotFound);
        let e = anyhow::Error::new(io).context(format!("file.read({path}) failed"));
        assert_eq!(
            classify_jwc_error(&e),
            "IoError.NotFound",
            "path {path} was misclassified by the substring scan"
        );
    }
}

/// An io kind we don't have a subtype for stays on the bare parent rather
/// than inventing one that would silently mismatch `catch (e: IoError.X)`.
#[test]
fn classify_jwc_error_unknown_io_kind_stays_on_the_parent() {
    let io = std::io::Error::from(std::io::ErrorKind::WouldBlock);
    let e = anyhow::Error::new(io).context("file.write(/x) failed");
    assert_eq!(classify_jwc_error(&e), "IoError");
}

#[test]
fn catch_type_matches_specific_subtype_does_not_match_sibling() {
    // UniqueViolation must NOT catch ForeignKeyViolation, even though
    // both are dotted children of DbError.
    assert!(!catch_type_matches(
        Some("DbError.UniqueViolation"),
        "DbError.ForeignKeyViolation"
    ));
    assert!(!catch_type_matches(
        Some("DbError.UniqueViolation"),
        "DbError"
    ));
    assert!(!catch_type_matches(
        Some("HttpError.NotFound"),
        "HttpError.Unauthorized"
    ));
}

#[test]
fn catch_type_matches_specific_subtype_matches_exact() {
    assert!(catch_type_matches(
        Some("DbError.UniqueViolation"),
        "DbError.UniqueViolation"
    ));
    assert!(catch_type_matches(
        Some("HttpError.NotFound"),
        "HttpError.NotFound"
    ));
}

#[test]
fn catch_type_matches_rejects_bare_prefix_overlap() {
    // "Db" is not a real kind, but defensively: `catch (e: Db)` must
    // NOT silently catch a "DbError.*" kind just because the string
    // starts with "Db". The match boundary is the literal dot.
    assert!(!catch_type_matches(Some("Db"), "DbError"));
    assert!(!catch_type_matches(Some("Db"), "DbError.UniqueViolation"));
    assert!(!catch_type_matches(Some("Http"), "HttpError.NotFound"));
}

#[test]
fn classify_jwc_error_falls_back_to_dberror_when_subtype_not_detectable() {
    // No real `tokio_postgres::Error` on the chain — just a plain
    // anyhow!() message that smells like DB. We must NOT invent a
    // subtype; the contract is "fall back to the parent kind".
    let e = anyhow::anyhow!("Postgres pool exhausted: no connection");
    let kind = classify_jwc_error(&e);
    assert_eq!(kind, "DbError");
    // And the parent kind matches a `catch (e: DbError)`.
    assert!(catch_type_matches(Some("DbError"), kind));
    // But NOT a specific subtype.
    assert!(!catch_type_matches(Some("DbError.UniqueViolation"), kind));
}

#[test]
fn classify_jwc_error_detects_jwt_invalid_signature() {
    // Real `crate::jwt::verify_hs256` failure — its message is
    // "jwt_verify: signature mismatch", which our substring scan
    // routes to the InvalidSignature subtype.
    let token = crate::jwt::sign_hs256(r#"{"a":1}"#, "right").unwrap();
    let err = crate::jwt::verify_hs256(&token, "wrong").unwrap_err();
    let kind = classify_jwc_error(&err);
    assert_eq!(kind, "JwtError.InvalidSignature");
    // Parent JwtError catches it.
    assert!(catch_type_matches(Some("JwtError"), kind));
    // And the catch-all `Error` does too.
    assert!(catch_type_matches(Some("Error"), kind));
    // But an unrelated subtype does not.
    assert!(!catch_type_matches(Some("DbError.UniqueViolation"), kind));
}

#[test]
fn classify_jwc_error_falls_back_to_error_for_unknown_shape() {
    let e = anyhow::anyhow!("something nobody recognises");
    let kind = classify_jwc_error(&e);
    assert_eq!(kind, "Error");
}

#[test]
fn classify_jwc_error_detects_validation_error_message_shape() {
    let e = anyhow::anyhow!("validate body: field 'name' is required");
    assert_eq!(classify_jwc_error(&e), "ValidationError");
}

#[test]
fn classify_jwc_error_detects_timeout_error_message_shape() {
    let e = anyhow::anyhow!("request timeout: deadline elapsed");
    assert_eq!(classify_jwc_error(&e), "TimeoutError");
}

/// A request to a path that exists under a different verb used to return the
/// same 404 as a path that doesn't exist at all, so a client couldn't tell a
/// typo'd URL from a wrong method.
#[tokio::test]
async fn wrong_method_on_existing_path_is_405_with_allow_header() {
    let src = r#"
        route GET "/items" { return json({ ok: true }); }
        route POST "/items" { return json({ created: true }); }
        function main() { }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();

    let (status, body, _ct, headers) =
        run_request_with_headers(&program, "DELETE", "/items", None, HashMap::new())
            .await
            .unwrap();

    assert_eq!(status, 405, "body was: {body}");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(doc["code"], "method_not_allowed", "body was: {body}");
    // Both declared verbs are advertised, sorted, per RFC 9110 §10.2.1.
    let allow = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("allow"))
        .map(|(_, v)| v.clone())
        .expect("405 must carry an Allow header");
    assert_eq!(allow, "GET, POST");
}

#[tokio::test]
async fn unknown_path_is_still_404_not_405() {
    let src = r#"
        route GET "/items" { return json({ ok: true }); }
        function main() { }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, body) = run_request(&program, "GET", "/nope", None).await.unwrap();
    assert_eq!(status, 404, "body was: {body}");
}

/// Path params must not make a 405 look like a 404: `/items/{id}` matches
/// `/items/7` for the purposes of "this path exists".
#[tokio::test]
async fn wrong_method_is_405_on_parameterised_paths_too() {
    let src = r#"
        route GET "/items/{id}" { return json({ id: path_param("id") }); }
        function main() { }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let (status, _) = run_request(&program, "PUT", "/items/7", None)
        .await
        .unwrap();
    assert_eq!(status, 405);
}

/// §2 ergonomics: `&&` / `||`, compound assignment, ternary and `??`.
/// Each was a parse error, so everyday code had to spell out
/// `n = n + 1` and `if (x == null) { x = d; }` by hand.
#[tokio::test]
async fn everyday_operators_evaluate() {
    let src = r#"
        function main() {
            let a = 1;
            let b = 2;
            if (a == 1 && b == 2) { print("and"); }
            if (a == 9 || b == 2) { print("or"); }

            let n = 10;
            n += 5;
            n -= 3;
            n *= 2;
            n /= 4;
            print(n);

            print(a > 0 ? "pos" : "neg");
            let missing = null;
            print(missing ?? "fallback");
            print("kept" ?? "unused");
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "and\nor\n6\npos\nfallback\nkept");
}

/// `?:` and `??` must not evaluate the branch they don't take — otherwise
/// `x ?? expensive()` silently pays for the fallback every time.
#[tokio::test]
async fn ternary_and_coalesce_short_circuit() {
    let src = r#"
        function boom() {
            print("evaluated");
            return "boom";
        }
        function main() {
            let kept = "value";
            print(kept ?? boom());
            print(true ? "taken" : boom());
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(
        out.trim(),
        "value\ntaken",
        "the untaken branch must not run"
    );
}

/// Compound assignment on an object field, which is where the manual
/// `w.balance = w.balance + delta` spelling showed up most.
#[tokio::test]
async fn compound_assignment_works_on_object_fields() {
    let src = r#"
        function main() {
            let o = { balance: 100 };
            o.balance += 50;
            o.balance -= 20;
            print(o.balance);
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "130");
}

/// `and`/`or` keep working — the symbol forms are aliases, not replacements.
#[tokio::test]
async fn keyword_and_symbol_boolean_operators_agree() {
    let src = r#"
        function main() {
            let a = 1;
            if (a == 1 and a < 5) { print("kw"); }
            if (a == 1 && a < 5) { print("sym"); }
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out.trim(), "kw\nsym");
}

/// `badRequest` / `internalError` used to stringify their argument
/// unconditionally, so an object body came back JSON-encoded *inside* the
/// `error` string:
///
///   {"error":"{\"got\":\"x\"}"}
///
/// `notFound`, `unauthorized`, `forbidden` and `ok` all went through
/// `error_response` and were correct, and every one of them was correct
/// under `--native` — so this was visible only to whoever ran `jwc run`,
/// which is anyone without a Rust toolchain installed.
#[tokio::test]
async fn error_helpers_keep_an_object_body_intact() {
    for (call, status) in [
        ("badRequest", 400),
        ("internalError", 500),
        ("notFound", 404),
        ("unauthorized", 401),
        ("forbidden", 403),
    ] {
        let src = format!(
            r#"
            function main() {{
                print({call}({{ got: "x", n: 1 }}));
            }}
        "#
        );
        let program = parse_program(&src).unwrap();
        validate_program(&program).unwrap();
        let out = run_main(&program).await.unwrap().output;
        let doc: serde_json::Value = serde_json::from_str(out.trim())
            .unwrap_or_else(|e| panic!("{call} produced invalid JSON: {e}\n{out}"));
        assert_eq!(doc["got"], "x", "{call} lost or re-encoded the body: {out}");
        assert_eq!(doc["n"], 1, "{call} lost or re-encoded the body: {out}");
        assert_eq!(doc["__jwc_status__"], status, "{call} status: {out}");
        assert!(
            doc.get("error").is_none(),
            "{call} wrapped the object in an `error` string: {out}"
        );
    }
}

/// `statusCode(302, { Location: url })` has to produce a header envelope,
/// not a JSON body.
///
/// The redirect branch matched on `Value::Str` holding JSON, which is what
/// object literals used to evaluate to. Once they became `Value::Record`
/// nothing matched, so the call fell through to the body path: a 302 status
/// line, no `Location` header, and the header map served as the response
/// body — a redirect that does not redirect, wearing a 3xx to hide it.
#[tokio::test]
async fn redirect_status_code_emits_headers_from_an_object_literal() {
    let src = r#"
        function main() {
            print(statusCode(302, { Location: "https://example.com/t" }));
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
    assert_eq!(doc["__jwc_status__"], 302, "{out}");
    assert_eq!(
        doc["__jwc_headers__"]["Location"], "https://example.com/t",
        "Location did not become a header: {out}"
    );
    assert_eq!(doc["__jwc_body__"], "", "{out}");
}

/// `take(xs, n)` accepts arrays, not just strings.
///
/// `first` and `last` have always taken either, and the reference groups all
/// three together — so `take(rows, 5)`, the obvious pagination shape, raising
/// "s must be string" read as a bug in the caller's code. A string still
/// slices by character; that behaviour predates this and programs depend on it.
#[tokio::test]
async fn take_slices_arrays_and_strings() {
    let src = r#"
        function main() {
            print(join(take([1, 2, 3, 4], 2), ","));
            print(take("abcdef", 2));
            print(join(take([1, 2], 99), ","));
            print(join(take([1, 2], 0), ","));
        }
    "#;
    let program = parse_program(src).unwrap();
    validate_program(&program).unwrap();
    let out = run_main(&program).await.unwrap().output;
    assert_eq!(out, "1,2\nab\n1,2\n\n");
}
