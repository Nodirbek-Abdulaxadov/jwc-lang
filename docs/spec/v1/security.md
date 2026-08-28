# security.md — the threat model

Normative where it names behaviour; descriptive where it names posture.
Closes gap **#39** and the threat-model half of ROADMAP v0.29.0.

`docs/archive-0.9/spec/threat-model.md` describes the language the v0.25.0
cutover removed. This replaces it.

---

## 1. What is trusted

| | Trusted |
|---|---|
| the request body, headers, query, path | **no** — every one is attacker-controlled |
| `X-Forwarded-For` | **only** when `trusted_proxies` is non-empty (§3) |
| a keyset cursor | **no** — it is a client-supplied predicate, so it is signed (§4) |
| a package's sources | as far as its checksum (packages §4a.6) |
| `env(...)` | yes — the process environment is the deployment's |
| the database | yes — it is the program's own storage |

---

## 2. The request body

2.1 Read **once**, into a buffer bounded by `max_body_bytes`, before any
middleware runs (routing §5.1). Over the limit is 413 and the chain never
starts: a rate-limit bucket, a signature check or an audit row spent on a
body the server was always going to refuse is work an attacker chose.

2.2 `request.body() as C` validates against a `class`, which is a
**whitelist** — unlisted keys are dropped rather than rejected, so a client
cannot reach a column by naming it. `private` and `server` columns are
unreachable from a spread at all (types §9.4).

2.3 Every value in a query is a **bind parameter**, bound as text and cast
in SQL (queries §7.3). There is no string interpolation anywhere in the
compiler's output. `raw()` is the one hand-written path, its placeholders
are counted (`E0610`), its SQL must be a literal, and `jwc explain` prints
every occurrence with a count.

2.4 `JWC_LOG_SQL=1` prints each bound parameter's **length**, not its
value. `=values` prints the values and warns at the first statement that it
is doing so. Until 0.9.941 `=1` printed them, so switching the SQL log on
in production wrote passwords, session tokens and personal data into a file
that is collected and kept. A bind is positional and has no name, so there
is no way to filter by name the way a framework filters a params hash — the
only honest default is not to print them.

---

## 3. Who the caller is

3.1 `request.peer_ip()` is the socket. `request.client_ip()` is the socket
too, **unless** `trusted_proxies` is non-empty; then `X-Forwarded-For` is
walked from the right past addresses in the set and the first untrusted hop
is the answer.

3.2 With the default empty list the header is ignored **entirely**. This is
what makes a rate limiter keyed on `client_ip()` unspoofable by default:
otherwise anyone who can set a header mints a fresh bucket per request and
the limit never binds.

3.3 No other header is read. Not `X-Real-IP`, not `Forwarded`. One header,
one rule — a second source of truth is a second thing to get wrong.

---

## 4. Rate limiting

4.1 A limiter keys on `request.route()`, the **declared** pattern — one
bucket per endpoint, not one per id. Keying on `request.path()` gives an
unbounded key space that an attacker fills for free, which is `W0602`.

4.2 A credential endpoint keys on **both** the address and the account. One
address spraying many accounts is stopped by the first; many addresses
targeting one account by the second. Keying on either alone leaves the other
attack completely untouched.

4.3 The identity is hashed into the key. A key space that reads
`rl:auth:id:…:someone@example.com` tells anyone who can list keys which
accounts exist, which is the enumeration the endpoint refuses to do.

---

## 5. Credentials and tokens

5.1 `hash.password` is Argon2id, salted, and every call answers
differently. `hash.sha256` is deterministic and is for **high-entropy
tokens the server generated** — a session token, an API key, an invite —
so they can be found by equality on a unique index. A human-chosen password
never goes through `hash.sha256`, and a token never goes through
`hash.password`, because nothing could then look it up (`W1201` catches the
comparison that can never match).

5.2 A credential endpoint answers the **same sentence** for an unknown
address and a wrong password — and takes the **same time**. The message
alone is not enough: an unknown address that returned before hashing would
answer in microseconds where a known one takes an Argon2id verification, and
the clock would say what the sentence refuses to. Both branches verify; the
miss goes against a decoy hash.

5.3 `crypto.constant_time_eq` compares without an early exit. `hash.verify`
and `hash.hmac_verify` are constant time already.

---

## 6. Responses

6.0 **Security headers.** Every response carries `X-Content-Type-Options:
nosniff`, `X-Frame-Options: DENY` and
`Referrer-Policy: strict-origin-when-cross-origin`; HSTS, a CSP and a
Permissions-Policy are available and off until asked for (config §3.9).
Until 0.9.941 a route response carried **none** of them and a `static`
mount carried the first.

6.0.1 `html(body)` and `content(mime, body)` send their argument verbatim,
which is what they are for. `string.escape_html` and `string.escape_url`
(builtins §4a) are the safe primitives for building one; they are not
applied automatically, because a builder that escaped on its own could not
emit markup on purpose. There is no template engine, so there is nothing
that could escape by default.

6.1 A `private` column is never in a response, and a local holding one is
tracked so that returning it later is still refused (schema §3.1).

6.2 A violated constraint carrying a message becomes a declared error with
the author's sentence. A message-less one is a **fault** — 500 and a log —
because a generic "constraint violated" leaks schema names to the client
(errors §6.2). `jwc lint --constraints` enumerates that set rather than
leaving it to be discovered in production.

6.3 A fault's message never reaches the client. The response is
`{"error": "internal_error"}` and the detail goes to the log with the
request id.

6.4 **Cookies, and what this does and does not do about CSRF.**

This section used to say CSRF was out of scope because "the API is
token-authenticated, not cookie-authenticated". That was not true of the
language: `cookie(name, value, opts)` is a response builder (routing §6.2),
so a program can and does set a session cookie, and a claim in a threat
model that the language contradicts is worse than no claim.

What the language does:

* A cookie is `HttpOnly` and `SameSite=Lax` unless the author says
  otherwise. `Lax` is the cross-site defence: a cookie set this way is not
  sent on a cross-site `POST`, which is the shape a forged request takes.
  Until 0.9.941 the attributes were documented and **discarded**, so every
  cookie was `Path=/` and nothing else.
* `same_site: "None"` — the setting that turns that defence off — requires
  `secure: true` (`E0739`) and cannot be written by accident.
* `X-Frame-Options: DENY` is on by default (config §3.9), so the
  clickjacking route to the same end needs an explicit opt-out.

What it does **not** do: there is no token facility, no double-submit
helper, and no automatic `Origin` check on state-changing methods. A
program that authenticates with a cookie **and** needs to defend a
non-idempotent endpoint against a cross-site form post has to write that
itself — the pieces are `request.header("Origin")`, `crypto.token` and
`crypto.constant_time_eq`.

That is a gap, stated as one. `SameSite=Lax` covers the common case and is
the reason this is not urgent; it is not the same as a framework's
antiforgery middleware, and calling it one would be the same mistake the
old sentence made.

---

## 7. Transport

7.1 `tls { }` makes the listener HTTPS (config §3.5). The certificate and
key are read at boot, and a block that cannot be resolved into a working
pair stops the server rather than falling back to plain HTTP — an operator
cannot see that fallback from outside, because the listener answers either
way. Without the block the listener is plain HTTP, which is what a
terminating proxy in front expects.

7.2 `header_timeout` bounds the request line and headers, which
`request_timeout` structurally cannot: its clock starts in the handler, and
a slow-header dribble never arrives at one (config §3.6). Past the deadline
the connection is closed.

7.3 `request_timeout` **is** enforced, and a timed-out handler's task is
dropped, which releases the connection and the pool slot.

---

## 8. Supply chain

8.1 `cargo audit` and `cargo deny` run in CI. Every ignored advisory in
`deny.toml` cites its ID, the upstream blocking the fix, and whether it is
reachable at runtime.

8.2 "Dev-dependency only" is a claim about the graph, and
`tests/hardening.rs::no_triaged_advisory_crate_reaches_the_shipped_binary`
checks it: `cargo tree --edges normal` must contain none of the triaged
crates, and the `rustls-webpki` it does contain must be past every fixed
version. `cargo audit` reads `Cargo.lock`, which cannot tell a
dev-dependency from a shipped one, so without this the distinction lives
only in a comment.

8.3 `jwc add` verifies a package against a checksum from a **separate**
request, and refuses an archive entry whose path escapes its directory
(packages §4a.6–7).

---

## 9. What is not covered

| | Status |
|---|---|
| a CSRF token facility | none is provided — see §6.4 for what is, and why |
| a 24-hour soak | the harness runs and passes at 8 cycles / 480k requests (ROADMAP v0.29.0); the full 72-cycle run is `soak.yml`, on a runner that has the hours |
| a third-party security review | ROADMAP v1.0.0-rc.1 |
