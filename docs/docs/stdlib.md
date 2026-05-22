---
title: Standard library
sidebar_position: 5
---

# Standard library

Everything below ships in the `jwc` binary — no imports needed.

## HTTP responses

| Helper | Effect |
|---|---|
| `json(value)` | wraps value in a 200 response |
| `created(value)` | wraps value in a 201 response |
| `noContent()` | 204, empty body |
| `notFound()` | 404 |
| `unauthorized()` | 401 |
| `forbidden()` | 403 |
| `internalError(msg)` | 500 with the message |

## HTTP client

```jwc
async function loadUsers() {
    let res = await http_get("https://api.example.com/users");
    let posted = await http_post(
        "https://api.example.com/users",
        "{\"name\":\"Najim\"}",
        "{\"x-api-key\":\"abc\"}"
    );
    let parsed = await fetch_json("https://api.example.com/users");
    return parsed;
}
```

- `http_get` / `http_post` return `{ "status": N, "body": <JSON or string> }`.
- Third arg of `http_post` is an optional JSON object of headers.
- `fetch_json(url)` does `http_get` + `json_parse` and returns the decoded
  value directly.
- All three are async — backed by `reqwest` + `rustls`, they yield while
  the request is in flight.

## Async helpers

```jwc
async function pause() {
    await sleep_ms(250);   // non-blocking; yields to the tokio scheduler
}
```

`sleep_ms(ms)` is the canonical async delay — use it instead of any
blocking sleep so concurrent requests aren't serialised on the same
worker.

## JWT (HS256)

```jwc
let token = jwt_sign({ sub: u.id, exp: 9999999999 }, env("JWT_SECRET"));
let claims = jwt_verify(token, env("JWT_SECRET"));  // throws on mismatch
```

## Password hashing (Argon2id)

```jwc
let hash = hash_password("hunter2");
let ok   = verify_password("hunter2", hash);  // → true
```

## Cache (in-memory, TTL)

```jwc
cache_set("session:" + token, user_id, 60);   // 60s TTL, 0 = forever
let cached = cache_get("session:" + token);   // null when missing/expired
cache_del("session:" + token);
cache_clear();
```

## Email (SMTP via lettre + rustls)

```jwc
send_email("user@example.com", "Welcome", "<p>Hi!</p>");
```

Env: `JWC_SMTP_HOST`, `JWC_SMTP_PORT` (default 587),
`JWC_SMTP_USER`, `JWC_SMTP_PASSWORD`, `JWC_SMTP_FROM`,
`JWC_SMTP_TLS` (`starttls` | `tls` | `none`, default `starttls`).

## Background jobs

```jwc
function sendWelcome(payload_json) {
    let user = json_parse(payload_json);
    send_email(user.email, "Welcome", "<p>Hi " + user.name + "</p>");
}

register_job_handler("welcome", "sendWelcome");

route POST "register" {
    let req = body();
    // ... insert user ...
    enqueue("welcome", json_stringify({ name: req.name, email: req.email }));
    return created({ ok: true });
}
```

- Worker pool size from `JWC_QUEUE_WORKERS` (default 2).
- `register_job_handler(name, fn)` — handler is validated at compile time.
- `enqueue(name, payload_json)` returns immediately.
- `job_count()` reports pending size.

## WebSocket

```jwc
route WS "/chat/{room}" {
    while (true) {
        let msg = ws_recv();          // blocks; null on disconnect
        if (msg == null) { break; }
        ws_send(json_stringify({
            room: path_param("room"),
            echo: msg
        }));
    }
}
```

`ws_close()` tears the socket down from the handler side.

## Strings

```jwc
lower("HI")                  // "hi"
upper("hi")                  // "HI"
trim("  hi  ")               // "hi"
replace("a-b-c", "-", "/")   // "a/b/c"
split("a,b,c", ",")          // "[\"a\",\"b\",\"c\"]"
contains("hello", "ell")     // true
starts_with("hello", "he")   // true
ends_with("hello", "lo")     // true
length("hello")              // 5
```

## Arrays

`length(xs)` returns element count; `first(xs)` / `last(xs)` return
endpoints (or `null` for empty); `contains(xs, item)` works on
JSON-array elements **and** JSON-object keys.

## JSON

```jwc
let v   = json_parse("{\"a\":1}");
let str = json_stringify({ a: 1 });
```

## Time

```jwc
let now_iso = now();             // "2026-05-19T12:00:00.000Z"
let secs    = unix_timestamp();   // 1747654800
```

## Misc

```jwc
let id = uuid();                  // RFC 4122 string
let raw_body = body();            // raw HTTP body as a string
let h = header("authorization");  // case-insensitive
let q = query_param("page", "1"); // 2nd arg = default
let p = path_param("id");
setContext("userId", "u-42");
let v = context("userId");
```
