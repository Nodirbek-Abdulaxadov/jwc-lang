---
sidebar_position: 4
description: "Sign and verify JWTs (HS256) and hash passwords with argon2id — jwt_sign, jwt_verify, hash_password and verify_password."
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

## OIDC access tokens (RS256 + JWKS)

An OIDC provider signs access tokens with **RS256** and publishes the public
half of the key at its `jwks_uri`. `jwt_verify` cannot check those — its
second argument is a shared secret. Use `jwt_verify_jwks` instead:

```jwc
middleware Auth {
    let raw = header("authorization");
    if (raw == null) { return unauthorized({ error: "missing authorization" }); }

    let claims = json_parse(
        await jwt_verify_jwks(raw, env("JWKS_URL"))
    );
    setContext("userId", claims.sub);
    return null;
}

route GET "/me" use Auth {
    return { id: context("userId") };
}
```

| Built-in | Returns | Notes |
|---|---|---|
| `jwt_verify_jwks(token, jwks_url)` | `any` | RS256. Picks the key by the token's `kid`, fetches and caches the JWKS. Async — `await` it. Same `Bearer ` tolerance as `jwt_verify`. |

Find `jwks_url` in the provider's discovery document:

```bash
curl -s https://<issuer>/.well-known/openid-configuration | jq .jwks_uri
```

It is a **separate built-in on purpose**. Overloading `jwt_verify`'s secret
parameter to sometimes mean "a URL to fetch a public key from" is how
algorithm-confusion bugs get written; the two never share a signature path.

### Claim checks

`exp`, `nbf`, `iss` and `aud` are validated identically for both built-ins,
through the environment. Everything is off unless set:

| Env var | Effect |
|---|---|
| `JWC_JWT_LEEWAY_SECS` | Clock-skew tolerance for `exp` / `nbf` |
| `JWC_JWT_EXPECTED_ISS` | Required `iss` (exact match — a trailing `/` is significant) |
| `JWC_JWT_EXPECTED_AUD` | Required value in `aud`; matches a bare string or membership in an array |
| `JWC_JWT_JWKS_TTL_SECS` | JWKS cache lifetime (default 300) |
| `JWC_JWT_JWKS_MIN_REFETCH_SECS` | Floor between forced JWKS refetches (default 60) |

`iat` is parsed but never enforced — a token issued slightly in the future is
clock skew, not an attack, and `nbf` is the claim that expresses an activation
time.

### Key rotation, and why refetches are rate-limited

An unknown `kid` triggers one forced refetch, which is how a provider's key
rotation heals without a restart. That refetch is rate-limited by
`JWC_JWT_JWKS_MIN_REFETCH_SECS`, and the reason matters: `kid` is read from an
**unverified** token header, so without a floor an attacker could send tokens
carrying random `kid`s and turn every request into an outbound fetch against
your identity provider. Concurrent misses also collapse into a single request.

### Gotchas

- **Private identity providers.** `JWC_HTTP_BLOCK_PRIVATE` blocks the JWKS
  fetch when the IdP is on a private network or `localhost`. Leave it off, or
  put the host in `JWC_HTTP_ALLOWLIST`.
- **Repeated claims are string-or-array.** `aud`, and `role` on providers that
  emit it as a repeated claim, arrive as a bare string when there is one value
  and an array when there are several. `if (claims.role == "admin")` silently
  stops matching the moment a user gets a second role — branch on both shapes.
- **`scope` is a space-separated string**, not an array: `split(claims.scope, " ")`.

ES256 is not supported yet; RS256 is what OIDC providers default to.

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
