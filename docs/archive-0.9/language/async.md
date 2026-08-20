---
sidebar_position: 5
description: "JWC runs on a real async runtime (tokio). How async functions and await work, and why every HTTP request gets its own task."
---

# Async runtime

JWC has a real async runtime (tokio under the hood). Every HTTP request gets its own task; `await` actually yields.

## When to use `async`

Any function that does I/O — database, HTTP client, sleep, WebSocket — should be `async`. Routes do **not** need to be marked `async`; the server already spawns each handler on a tokio task.

```jwc
async function fetchAndSave(): User {
    let raw = await http_get("https://api.example.com/me");
    let parsed = json_parse(raw);
    insert parsed into AppDb.User;
    return parsed;
}
```

## Sleeping

```jwc
await sleep_ms(250);   // yields to the runtime; doesn't block a thread
```

## Awaiting multiple things

There's no `Promise.all` style helper today. Use a `for` loop with sequential `await`; for true parallelism, enqueue jobs via the [background queue](../backend/queue).

## DB calls are async

`select` / `insert` / `update` / `delete` and the `transaction` block are all async — the underlying `tokio-postgres` driver yields while the DB processes. You don't write `await` on SQL statements; it's implicit.

## HTTP server concurrency

`server.rs` uses `tokio::spawn` per request — `JWC_SERVER_WORKERS` from the legacy worker-pool model is no longer honoured. Scale tuning belongs in the tokio runtime config now (planned: `JWC_TOKIO_WORKERS` env).

## Async in the native binary

`jwc build --native` produces a `#[tokio::main(flavor = multi_thread)]` runtime. Same semantics, smaller binary footprint, no interpreter loop.
