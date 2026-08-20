# Security Policy

## Supported versions

Security fixes land on `main` and ship in the next patch release of the
latest minor line. Until v1.0, only the **latest released version** is
supported — older 0.x releases do not receive backported fixes.

| Version        | Supported |
| -------------- | --------- |
| latest release | ✅        |
| older releases | ❌        |

Note that 0.9.x binaries predating the v0.25.0 cutover implement a
different language. They are unsupported like any older release; a fix
for this compiler will not be backported to them.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

**Preferred channel — GitHub private security advisory.** Open a
private report at
[Security → Report a vulnerability](https://github.com/just-web-code/jwc-lang/security/advisories/new)
on this repository. GitHub routes the advisory directly to the
maintainers; no third-party form or email address is involved, and the
advisory stays private until coordinated disclosure.

Include where possible:

- **Affected component** — CLI, runtime/server, language server, install
  scripts, VS Code extension.
- **Minimal reproduction** — a `.jwc` snippet or a `curl` trace that
  triggers the issue. Smaller is better. If it involves a query,
  `jwc explain` output is worth attaching: it shows the emitted SQL.
- **Version** — output of `jwc --version`, or the commit SHA if you
  reproduced from source.
- **Impact assessment** — what an attacker gains, what privileges they
  need, and whether the issue is remotely or locally exploitable.

## What to expect

- **Acknowledgement** within **72 hours** of the advisory being filed.
- **Triage + severity assessment** within **7 days** of acknowledgement,
  including a CVSS estimate and the targeted fix window.
- **Fix target**: high/critical within **14 days** of triage; lower
  severities in the next regular release.
- **Coordinated disclosure** — we publish the advisory and the fix
  together. If you have a preferred disclosure date, say so in the
  report and we will align where the fix timeline allows.
- **Credit.** Reporters are named in the GitHub Release notes and in the
  published advisory unless you ask to remain anonymous. There is no
  paid bounty.

## Scope

In scope: the `jwc` CLI and runtime — the HTTP server, the database
layer, query and DDL emission, and the security-relevant builtins
(`jwt.*`, `hash.password`, `hash.verify`, `hash.hmac_verify`,
`crypto.token`, `crypto.constant_time_eq`, and the `raw()` escape
hatch) — plus `jwc lsp`, `install.sh` / `install.ps1`, the release
artifacts, and the VS Code extension.

Out of scope: vulnerabilities in applications *written in* JWC (report
those to the application's authors), and issues that require a malicious
local user with write access to the project directory.

[`docs/spec/v1/security.md`](docs/spec/v1/security.md) is the normative
threat model. It states what is trusted, how the request body is bounded,
how the caller is identified, and what a response may carry. Read it
before opening a PR that touches the HTTP server, the SQL layer, the JWT
helpers, or a log path that handles a connection string.

## What the language does for you

Two properties are load-bearing, and both are language guarantees rather
than conventions a program has to remember:

- **Every value reaching SQL is a bind parameter.** Nothing is
  interpolated. Parameters are bound as text and cast in SQL —
  `($1::text)::bigint`, never `$1::bigint` — so one binding path covers
  every type and there is no position in an emitted statement that a
  caller's string can reach. `jwc explain` prints the SQL, so this is
  checkable rather than assumed.
- **A query result is `Raw` until you project it.** `select` with no
  `as { }` produces one JSON value Postgres builds and the runtime never
  parses. Adding `as { … }` opts into a record. Which of the two a query
  is appears in `jwc explain`.

Breaking either is a breaking change under [`SEMVER.md`](SEMVER.md).

There is also a footgun that no longer exists: v1 has no `file.*` or
`directory.*` builtins. In 0.9.x those passed a path to the OS unchanged
with no jail or allowlist, so `file.read(query_param("path"))` would
serve `/etc/passwd`. The v1 builtin surface has no filesystem access at
all, and the only path the process opens by configuration is the TLS
key pair.

## Hardening notes for users

- **TLS.** `server { tls { cert = …; key = … } }` is enforced or the
  process refuses to boot. There is deliberately no fall back to plain
  HTTP when the paths do not resolve: that fallback is the one
  misconfiguration nothing outside the process can see, because the
  listener answers either way and every byte is in the clear.
- **Misspelled configuration is `E1206`, not a default.** `server { }`
  rejects unknown keys at boot. `trusted_proxie` would otherwise leave
  the proxy list empty, and a rate limiter keyed on
  `request.client_ip()` would collapse into one shared bucket.
- **Behind a reverse proxy**, set `server { trusted_proxies = [ … ] }`
  to the proxy's prefixes so `request.client_ip()` doesn't trust an
  inbound `X-Forwarded-For` from an untrusted hop. Without the list it
  falls back to the closest hop — safe, but it loses the real client IP,
  which is what a rate limit needs. `request.peer_ip()` is always the
  socket peer and is never derived from a header.
- **`/healthz`, `/readyz` and `/metrics` are served without being
  declared.** If the service is internet-facing without an ingress
  filter, declare your own routes at those paths — a declared route wins
  — or deny them at the edge. `/healthz` touches nothing; `/readyz`
  touches every configured dependency; `/metrics` carries pool gauges.
- **Never set `JWC_DB_TLS_INSECURE_SKIP_VERIFY=1` in production.**
- **`raw()` is the escape hatch, and it is tracked.** It still binds its
  parameters, but it is the one place the query layer stops reasoning
  about the SQL. `jwc explain` lists every `raw()` in the program, and
  `jwc lint --constraints` shows what each route can reach.
- **Rate limiting across replicas needs shared state.** `redis.rate_limit`
  is atomic — INCR and EXPIRE in one Lua script, one round trip. A
  process-local counter is not a rate limit when you run more than one
  pod.
- Release archives ship `.sha256` sidecars, and `install.sh` /
  `install.ps1` verify them.

## Dependency hygiene

`cargo audit` runs on every push to main and on every PR via
[`.github/workflows/security.yml`](.github/workflows/security.yml). The
job is BLOCKING — a new advisory against a transitive dependency fails
CI until the dependency is upgraded or the advisory is justified in
`deny.toml`.

The triaged ignore list lives in `deny.toml` under `[advisories].ignore`,
each entry citing the advisory ID, the upstream dependency blocking the
fix, and the impact rationale (dev-only vs. runtime). The workflow's
`--ignore` flags MUST stay in sync with that list. When an upstream patch
lands, drop the ID from BOTH places in the same change.

## Supply chain

Each release tag triggers
[`release.yml`](.github/workflows/release.yml), which:

1. Builds the binaries across the supported target matrix — glibc and
   musl, x86_64 and aarch64, plus Windows.
2. Computes SHA-256 over each archive and writes a `.sha256` sidecar.
3. Uploads both the archive and the sidecar to the GitHub Release.

`install.sh` / `install.ps1` fetch the sidecar alongside the archive and
verify it before extracting. Tags predating `.sha256` support emit a
warning but install; pinning to a recent tag is the recommended path.

The glibc builds are pinned to the oldest supported runner on purpose. A
glibc binary runs on its build glibc or newer, never older, so building
on `ubuntu-latest` silently drops every distro below it. Anything older
than the pinned glibc is served by the musl builds, which carry no libc
dependency.

Container images are published per tag as multi-arch manifests, built
per architecture on native runners and stitched into one manifest list,
with provenance and SBOM attestations attached.

Sigstore / cosign signing of the release artifacts is still outstanding —
see [`PRODUCTION_READINESS_PLAN.md`](PRODUCTION_READINESS_PLAN.md).
