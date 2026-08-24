---
sidebar_position: 5
title: WebSockets
description: "Declaring a socket, the three handlers, and what middleware does before the upgrade."
---

# WebSockets

A `socket` sits inside a `routes` block, beside the HTTP routes, and shares
the prefix and the `use` chain.

```jwc no-compile
routes "/live" use RequireAuth {
    socket "rooms/{room: text}" {
        on open {
            socket.send("joined " + @room);
        }

        on message (text) {
            socket.send("echo: " + $text);
        }

        on close {
            -- runs however the connection ended
        }
    }
}
```

All three blocks are optional; a `socket` with none of them does not
compile, because it would accept the upgrade and then do nothing.

## The runtime owns the loop

There is no `while` in JWC, and a socket handler does not need one. You do
not write a receive loop — you say what happens at the three moments a
connection has, and the runtime drives it.

That is not only a syntax convenience. A hand-written receive loop that
forgets to break holds a task for the life of the process, and that is the
single most common WebSocket bug there is.

## Middleware runs before the upgrade

This is the reason `use` on a socket is worth anything:

```jwc no-compile
middleware RequireAuth provides account_id: bigint {
    let header = request.header("Authorization") or throw Unauthorized("token kerak");
    -- …
}
```

A client with no token gets **`401` with that message**, as an ordinary
HTTP response. It does not get a `101` followed by an immediate close it
has to guess about.

Whatever the chain puts in `context` is readable in all three handlers and
persists for the connection. Locals do not: each handler runs on its own
scope, and `context` is the state that is meant to survive.

## `socket.send` and `socket.close`

Both are legal only inside one of the three handlers — anywhere else is a
compile error, not a runtime fault.

Both **queue**. The connection writes what a handler produced once that
handler returns, so:

```jwc no-compile
on message (text) {
    if ($text == "bye") {
        socket.close();
    }
    socket.send("this never goes");   -- the close came first
}
```

A handler that raises ends the connection: there is no response to put an
error in, and closing is the only signal the protocol has.

## What the wire does

| | |
|---|---|
| The upgrade | a `GET`, so `route GET` on the same path is a duplicate and does not compile |
| A plain `GET` at a socket path | `400` — the path exists, the request is wrong |
| A binary frame | closes the connection; `on message (m)` binds `text` |
| A text frame with no `on message` | dropped — a peer that speaks first is not an error |
| `after` blocks | do not run: they observe a response, and the response was the `101` |

## Tooling

`jwc routes` prints sockets as `WS`:

```
WS      /live/rooms/{room}  RequireAuth
GET     /live/health        -
```

`jwc openapi` cannot describe a WebSocket — OpenAPI has no notion of one —
so sockets are listed under `x-jwc-sockets` rather than emitted as a `GET`
that answers 200, which is a lie a client generator would act on.

Both backends run sockets: `jwc serve` and `jwc build`. They are held to
the same answers as every other route.

## Server-Sent Events

Not implemented. 0.9 parsed and validated `route SSE "…"` end to end and
dispatched it to a stub, so a program could declare one, pass every check
and serve nothing — which is worse than not having it. Use a `socket`, or
long-polling.
