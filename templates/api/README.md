# {{name}}

A JWC REST service: one table, a DTO pair, a service and five routes.

```bash
cp .env.example .env      # point DATABASE_URL at a database, set CURSOR_SECRET
jwc check                 # types, schema, routes — offline, no database
jwc migrate new init      # turn src/db/notes.jwc into DDL
jwc migrate up            # apply it
jwc serve                 # run
```

```
GET    /api/v1/notes           list, keyset-paginated  (?cursor=…)
POST   /api/v1/notes           create
GET    /api/v1/notes/{id}      read
PATCH  /api/v1/notes/{id}      partial update
DELETE /api/v1/notes/{id}      delete
```

## The shape

| Directory | What lives there |
|---|---|
| `src/app.jwc` | the database, its schemas, `server { }`, `main()` |
| `src/db/` | tables — one file per schema |
| `src/dto/` | `class` declarations: the request boundary |
| `src/services/` | queries, one `service` per area |
| `src/routes/` | HTTP only — parse the body, call a service, answer |

A file's `namespace` must match its path with `src/` stripped, and that is
the whole module system.

## Where to go next

- `jwc routes` — the resolved route table, middleware chains included.
- `jwc explain` — every query the program issues, with its SQL.
- `jwc openapi` — an OpenAPI 3.1 document, derived from the types.
- `jwc build --release` — one native binary, no runtime.
