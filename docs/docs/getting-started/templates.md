---
sidebar_position: 4
---

# Project templates

`jwc new` accepts a `--template <kind>` flag that scaffolds a complete
working starter project instead of the bare manifest + `main.jwc` you get
without it. Pick the one closest to what you're building and edit from
there — every template passes `jwc test` straight out of the box.

```bash
jwc new <name> --template <empty|api|auth|jobs>
```

| Template | Use when you want…                                             |
|----------|----------------------------------------------------------------|
| `empty`  | Minimal scaffold (default — same as omitting the flag)         |
| `api`    | A REST API with CRUD over one entity + Postgres migrations     |
| `auth`   | JWT login + a middleware-protected `/me` route                 |
| `jobs`   | An HTTP endpoint that enqueues a background-queue job          |

After generation, `cd <name>` and run `jwc test` to validate, then
`jwc run` to launch.

## `--template api`

```bash
jwc new myapi --template api
cd myapi
cp .env.example .env
jwc migrate up
jwc run
```

### Layout

```
myapi/
├── myapi.jwcproj
├── main.jwc                   # routes + main()
├── src/
│   ├── AppDbContext.jwc       # dbcontext AppDb : Postgres
│   └── Item.jwc               # entity Item + request classes
├── migrations/
│   └── 0000000001_init.up.sql / .down.sql
├── .env.example
└── README.md
```

### Endpoints

| Method | Path           |
|--------|----------------|
| GET    | `/items`       |
| GET    | `/items/{id}`  |
| POST   | `/items`       |
| PUT    | `/items/{id}`  |
| DELETE | `/items/{id}`  |

### Extend

- **Add an entity**: drop `src/<Name>.jwc` with
  `entity Foo of AppDb { ... }`, then run
  `jwc migrate new add-foo` to emit the diff migration.
- **Add a route**: append `route GET "/foo" { ... }` to `main.jwc`
  (or split into `src/FooController.jwc`).

## `--template auth`

```bash
jwc new myauth --template auth
cd myauth
cp .env.example .env            # then edit JWT_SECRET
jwc migrate up
jwc run
```

### Layout

```
myauth/
├── myauth.jwcproj
├── main.jwc
├── src/
│   ├── AppDbContext.jwc
│   ├── User.jwc               # entity User + RegisterRequest / LoginRequest
│   └── AuthMiddleware.jwc     # middleware Auth { ... }
├── migrations/
└── .env.example
```

### Endpoints

| Method | Path        | Auth | Description                       |
|--------|-------------|------|-----------------------------------|
| POST   | `/register` | -    | Create user, returns the new id   |
| POST   | `/login`    | -    | Returns a signed JWT              |
| GET    | `/me`       | JWT  | Returns the caller's user record  |

### Extend

- **Gate a route**: add `use Auth` to any `route` declaration.
  `route GET "/secret" use Auth { ... }`.
- **Add roles**: stash a `role` claim when signing, gate inside the
  `Auth` middleware with an `if (claims.role != "admin") return forbidden();`.
- **Refresh tokens**: a second route that re-issues a short-lived JWT
  from a longer-lived token persisted on `User`.

## `--template jobs`

```bash
jwc new myjobs --template jobs
cd myjobs
jwc run                              # in-memory queue, lost on restart
JWC_QUEUE_DRIVER=postgres jwc run    # durable queue (needs Postgres)
```

### Layout

```
myjobs/
├── myjobs.jwcproj
├── main.jwc                # POST /send-email + register_job_handler in main()
├── src/
│   └── EmailJob.jwc        # function process_email(payload) { ... }
└── .env.example
```

### Endpoints

| Method | Path           | Description                             |
|--------|----------------|-----------------------------------------|
| POST   | `/send-email`  | Enqueues `process_email` and returns 202|
| GET    | `/health`      | Liveness probe                          |

### Extend

- **Add another handler**: drop `src/ReportJob.jwc` with
  `function build_report(payload) { ... }`, register it in `main()` via
  `register_job_handler("build_report", build_report)`, then enqueue
  with `enqueue("build_report", json_stringify(payload))` from any route.
- **Time-sensitive work**: `enqueue_urgent(...)` jumps the queue.
- **Persist across restarts**: set `JWC_QUEUE_DRIVER=postgres` and supply
  a Postgres connection; the queue manages its own table.

## Why `process_email` instead of `send_email`?

`send_email` is a built-in (SMTP send) so we can't shadow it with a
user-defined function. The `jobs` template names the handler
`process_email` to stay out of the built-in's way; rename freely once
you've decided what the handler should actually do.

## Cookbook: switch a template

Templates are just scaffolds. Generated once, they're indistinguishable
from a hand-written JWC project — there's no "template runtime" tracking
state. Mix-and-match freely (e.g. start with `--template api`, drop the
`auth` template's `AuthMiddleware.jwc` into `src/`, then `use Auth` on
routes that need protection).
