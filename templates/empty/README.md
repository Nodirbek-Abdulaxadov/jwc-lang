# {{name}}

A JWC service.

```bash
cp .env.example .env      # then point DATABASE_URL at a database
jwc check                 # types, schema, routes — offline
jwc migrate new init      # write the first migration from the schema
jwc migrate up            # apply it
jwc serve                 # run
```

`jwc routes` prints the resolved route table, `jwc openapi` emits an
OpenAPI 3.1 document, and `jwc build --release` produces a single native
binary.
