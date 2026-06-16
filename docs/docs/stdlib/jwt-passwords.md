---
sidebar_position: 4
---

# JWT & passwords

## JWT (HS256)

```jwc
let token = jwt_sign(
    json_stringify({ sub: user.id, role: "admin", exp: unix_timestamp() + 3600 }),
    env("JWT_SECRET")
);

let claims = jwt_verify(token, env("JWT_SECRET"));
if (claims == null) { return unauthorized(); }
// claims is a JSON object — claims.sub / claims.role / claims.exp
```

| Built-in | Returns | Notes |
|---|---|---|
| `jwt_sign(payload_json, secret)` | `string` | HS256 (HMAC + SHA-256). Header is fixed `{"alg":"HS256","typ":"JWT"}`. |
| `jwt_verify(token, secret)` | `any?` | Decoded claims, or `null` on bad signature / `exp` expired. An optional `Bearer ` prefix (case-insensitive) is stripped, so you can pass `header("authorization")` straight through. |

RS256 / ES256 + JWKS verification land with the OIDC sprint.

## Passwords (Argon2id)

```jwc
let hash = hash_password(req.password);
// store `hash` in the DB

let ok = verify_password(req.password, stored_hash);
if (!ok) { return unauthorized(); }
```

| Built-in | Returns | Notes |
|---|---|---|
| `hash_password(plaintext)` | `string` | Argon2id with default parameters (memory-hard) |
| `verify_password(plaintext, hash)` | `bool` | constant-time |

The hash includes the salt + parameters; never store the salt separately.

## Hashing & HMAC

New in v0.4.0. These return raw digests — for password storage prefer `hash_password` above.

```jwc
let digest = sha256("hello");                       // lowercase hex
let sig    = hmac_sha256(env("API_SECRET"), body);  // lowercase hex
```

| Built-in | Returns | Notes |
|---|---|---|
| `sha256(s)` | `string` | lowercase hex digest |
| `sha1(s)` | `string` | lowercase hex digest |
| `md5(s)` | `string` | lowercase hex digest |
| `hmac_sha256(key, msg)` | `string` | lowercase hex HMAC-SHA256 |
