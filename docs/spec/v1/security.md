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

---

## 7. Transport

7.1 The listener is HTTP. `tls { }` is specified and **not implemented**,
and declaring it makes `jwc serve` refuse to boot rather than serve plain
text under a name that says otherwise (config §3.5). Terminate at a proxy.

7.2 `header_timeout` is likewise not enforced and refuses to boot, for the
same reason: an operator who wrote it down believes it (config §3.6).

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
| TLS termination | not implemented (§7.1) |
| slow-header defence | not implemented (§7.2) |
| CSRF tokens | out of scope: the API is token-authenticated, not cookie-authenticated |
| a 24-hour soak | not run in this environment; the criterion stands open (ROADMAP v0.29.0) |
| a third-party security review | ROADMAP v1.0.0-rc.1 |
