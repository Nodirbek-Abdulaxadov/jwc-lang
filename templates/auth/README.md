# {{name}}

A JWC service with email + password accounts and JWT sessions.

```bash
cp .env.example .env      # point DATABASE_URL at a database, set JWT_SECRET
jwc check                 # types, schema, routes — offline, no database
jwc migrate new init      # turn src/db/auth.jwc into DDL
jwc migrate up            # apply it
jwc serve                 # run
```

```
POST  /api/v1/auth/register    { email, password, display_name } -> 201
POST  /api/v1/auth/login       { email, password }               -> { token, expires_in }
GET   /api/v1/me               Authorization: Bearer <token>
PATCH /api/v1/me               { display_name }
```

## What the auth code is doing, and why

- **Passwords are Argon2id** — `hash.password` / `hash.verify`. The column
  is `private`, so it cannot reach a response through a projection that
  does not name it.
- **Login is constant-work.** An unknown address is verified against a
  decoy hash, so a miss costs the same as a hit. Returning early would
  make the response time say what the error message refuses to.
- **`middleware RequireAuth provides account_id`** is a contract, not a
  convention: a route that reads `context.account_id` without the
  middleware in its chain does not compile.
- **The token subject is re-typed**, `bigint($claims.sub)`. It is not
  client-derived — the signature proves this process minted it.

## Next

Rate-limit `login` before it goes anywhere real. `redis.rate_limit` is the
primitive; key it on `request.client_ip()`, not `request.path()`.
