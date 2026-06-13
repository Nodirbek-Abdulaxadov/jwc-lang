---
sidebar_position: 3
---

# HTTP client

Async — only callable from `async` functions / route bodies.

| Built-in | Returns | Notes |
|---|---|---|
| `await http_get(url)` | `string` | response body as text |
| `await http_post(url, body)` | `string` | body is sent as `application/json` |
| `await http_post(url, body, headers)` | `string` | headers is a JSON object `{ "X-Foo": "Bar" }` |
| `await fetch_json(url)` | `any` | shortcut: `json_parse(await http_get(url))` |
| `await sleep_ms(ms)` | `void` | yields to the runtime |

```jwc
async function fetchUser(id: int): User? {
    let raw = await http_get("https://api.example.com/users/" + id);
    return json_parse(raw);
}

async function postEvent(payload: json) {
    await http_post(
        "https://logs.example.com/ingest",
        json_stringify({ event: payload }),
        { "X-Source": "jwc" }
    );
}
```

## TLS

`rustls` is bundled — no system OpenSSL dependency. HTTPS works out of the box.

## SSRF allowlist (`JWC_HTTP_ALLOWLIST`)

In production, set `JWC_HTTP_ALLOWLIST` to lock outbound HTTP to a
known set of hosts:

```
JWC_HTTP_ALLOWLIST=api.stripe.com,api.sendgrid.com
```

When the var is non-empty, any `http_get` / `http_post` / `fetch_json`
to a host that isn't in the list throws `HttpError` before the socket
opens — closes the classic SSRF gap where a handler does
`http_get(body().url)` and an attacker passes an internal address.

Empty (default) ⇒ no restriction — fine for dev. Pin it down for any
externally-facing deployment. See [security/](../security/index.md#ssrf-allowlist-jwc_http_allowlist).

## Errors

HTTP non-2xx + network failures throw `HttpError`:

```jwc
try {
    let data = await fetch_json("https://flaky.example.com/api");
    return ok(data);
} catch (e: HttpError) {
    return internalError({ error: "upstream unavailable" });
}
```

## Timeouts

No per-call timeout flag yet — wrap in `sleep_ms` + custom logic, or set a global tokio timeout via the runtime. Issue tracking it on the [roadmap](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/ROADMAP.md).
