---
sidebar_position: 5
---

# WebSockets

```jwc
route WS "/chat/{room}" {
    let room = path_param("room");
    ws_send("welcome to " + room);
    while (true) {
        let msg = ws_recv();
        if (msg == null) { break; }   // client disconnected
        ws_send("echo: " + msg);
    }
    ws_close();
}
```

The handler runs once per connection, on a dedicated tokio task. All three built-ins block the task (not the runtime) until a frame is available / sent.

## Built-ins

| Built-in | Effect |
|---|---|
| `ws_send(s)` | Send a text frame |
| `ws_recv()` | Block until next text frame; `null` on close |
| `ws_close()` | Send a close frame and end the handler |

Binary frames + per-client backpressure controls land in a future sprint; v1 is text-only.

## Authentication

```jwc no-compile
middleware AuthWS {
    let token = query_param("token");
    if (token == null) { return unauthorized(); }
    let claims = jwt_verify(token, env("JWT_SECRET"));
    if (claims == null) { return unauthorized(); }
    setContext("user_id", claims.sub);
}

route WS "/chat/{room}" use AuthWS {
    let uid = context("user_id");
    ...
}
```

Token in the query string is the browser-friendly form (the WS handshake API doesn't let you set arbitrary headers).

## Broadcast / fanout

No built-in pub/sub yet. For now, push from a `ws_send` loop driven by the [background queue](./queue). Native pub/sub (`sse_broadcast` / `ws_broadcast`) lands with the SSE work.
