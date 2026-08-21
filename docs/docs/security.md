---
sidebar_position: 8
title: "Security"
description: "What the language enforces rather than recommends: private columns, server columns, signed cursors, the class whitelist, and where the boundaries are."
---

# Security

Most of this is enforced by the compiler rather than left to a review
checklist. That is the point: a rule a reviewer has to remember is one
that eventually gets forgotten.

## A `private` column cannot be selected

```jwc no-compile
password_hash varchar(255) private;
```

Not "should not". A projection cannot name it, so no query can return it
— including one written next year by someone who has not read this page.
There is no `select *` to forget to narrow.

## A `server` column cannot be written by a request

```jwc no-compile
added_by bigint server;
```

Whatever the client sends, this column is not one a request body can set.

## A class is a whitelist

Keys the class does not name are dropped. A request cannot smuggle a
field into a write by adding it to the JSON, and the write cannot set a
column that was never in the shape.

## Path parameters are parsed before middleware

A `{id: bigint}` that is not a bigint is a 400 at the router, before any
handler and before Postgres. Malformed input never reaches a query as a
string that "should" have been a number.

## Cursors are signed

A pagination cursor is a predicate the client hands back. Unsigned, it is
a second `WHERE` clause nobody checked. It is HMAC-signed with
`server { cursor_secret }`, verified in constant time, and a cursor that
does not verify is a 400 — not a 500 and not a silently empty page.

## Passwords

`hash.password` is Argon2id with a random salt. `hash.verify` compares
against the stored PHC string. There is no `hash.password_unsalted`, and
`hash.sha256` is not offered as a password function.

## Tokens

`crypto.token(n)` is a CSPRNG. `crypto.constant_time_eq` and
`hash.hmac_verify` do not stop at the first differing byte.

`jwt.sign` fixes `iat` and `exp`: a caller supplies the TTL, not the
expiry, so a token cannot be minted that outlives the policy that issued
it. `jwt.verify` checks the signature, `exp`, `nbf` and — when
configured — `iss` and `aud`.

## CORS is off unless declared

No `cors { }` block means no CORS headers at all. A browser refusing a
cross-origin call is the correct default, and a header emitted "just in
case" is a policy nobody wrote.

## Response splitting

A header value containing CR or LF is dropped rather than emitted. That
is the classic splitting vector, and both backends reject the same bytes.

## Bodies are bounded

`server { max_body_bytes }` (1 MiB by default) is enforced before
middleware. One client cannot OOM the process with `curl -d @huge.bin`.

## What the language does not do for you

- **Authorisation.** Membership, ownership and roles are your model.
  What the language gives you is `middleware … requires … provides`, so
  a handler that reads `context.account_id` cannot compile without the
  middleware that sets it.
- **Rate limiting.** `redis.rate_limit` is a primitive. What to key on
  and what the limit is are yours — and key on `request.route()`, not
  `request.path()`, or every distinct id gets its own bucket.
- **Secrets.** They come from the environment. Nothing in source should
  hold one, and `server { cursor_secret = env(…) }` is the shape.
- **TLS.** `server { tls { } }` terminates in-process; most deployments
  terminate at the edge instead.

## Reporting

Security issues:
[github.com/just-web-code/jwc-lang/security](https://github.com/just-web-code/jwc-lang/security).
