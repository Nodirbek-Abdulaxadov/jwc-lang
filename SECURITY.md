# Security Policy

## Supported versions

Security fixes land on `main` and ship in the next patch release of the
latest minor line. Until v1.0, only the **latest released version** is
supported — older 0.x releases do not receive backported fixes.

| Version        | Supported |
| -------------- | --------- |
| latest release | ✅        |
| older releases | ❌        |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

**Preferred channel — GitHub private security advisory.** Open a
private report at
[Security → Report a vulnerability](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/security/advisories/new)
on this repository. GitHub routes the advisory directly to the
maintainers; no third-party form or email address is involved, and the
advisory stays private until coordinated disclosure.

Include where possible:

- **Affected component** — CLI, runtime/server, LSP, install scripts,
  VS Code extension.
- **Minimal reproduction** — a `.jwc` snippet or `curl` trace that
  triggers the issue. Smaller is better.
- **Version** — output of `jwc --version` (or the commit SHA if you
  reproduced from source).
- **Impact assessment** — what an attacker gains, what privileges they
  need, and whether the issue is remotely or locally exploitable.

## What to expect

- **Acknowledgement** within **72 hours** of the advisory being filed
  (we monitor the security tab daily).
- **Triage + severity assessment** within **7 days** of acknowledgement
  — including a CVSS estimate and the targeted fix window.
- **Fix target**: high/critical issues within **14 days** of triage;
  lower severities in the next regular release.
- **Coordinated disclosure** — we publish the advisory + fix together.
  If you have a preferred disclosure date, tell us in the report and
  we will align where the fix timeline allows.
- **Hall of fame.** Reporters are credited in the GitHub Release notes
  and in the published advisory, unless you ask to remain anonymous.
  We do not currently offer a paid bounty.

## Scope

In scope: the `jwc` CLI and runtime (HTTP server, DB layer, job queue,
builtins such as `jwt_*`, `hash_password`, `raw_sql`), `jwc-lsp`, the
install/uninstall scripts, release artifacts, and the VS Code extension.

Out of scope: vulnerabilities in applications *written in* JWC (report those
to the application's authors), and issues requiring a malicious local user
with write access to the project directory.

## Hardening notes for users

- Release archives ship with `.sha256` checksum files; `install.sh` /
  `install.ps1` verify them automatically when present.
- Never set `JWC_DB_TLS_INSECURE_SKIP_VERIFY=1` in production.
- Dependency advisories are tracked in CI via `cargo audit` and
  `cargo deny` (`.github/workflows/security.yml`).
- Behind a reverse proxy (nginx, k8s ingress, Cloudflare), set
  `JWC_TRUSTED_PROXIES` to the proxy's IP / prefix list so the
  `client_ip()` builtin doesn't blindly trust an inbound
  `X-Forwarded-For` from an untrusted hop. Without the list the
  builtin returns the rightmost chain entry (the closest hop) — safe
  but loses the real client IP. See `docs/docs/deployment/env-vars.md`
  for the full list of hardening knobs.
- The `raw_sql(...)` builtin takes parameterised positional binds —
  string concatenation into the SQL is a footgun; prefer the typed
  `select` / `insert` / `update set` forms whenever possible.
- The HTTP `/metrics`, `/healthz`, `/readyz` endpoints are exposed by
  default. If your service is internet-facing without an ingress
  filter, register your own handlers for those paths (the built-ins
  yield) or place a route-level deny.

## Dependency hygiene

`cargo audit` runs on every push to main and on every PR via
`.github/workflows/security.yml`. The job is BLOCKING — a new
advisory against a transitive dep fails CI until the dep is
upgraded or the advisory is justified in `deny.toml`.

The triaged ignore list lives in `deny.toml` under `[advisories].ignore`,
each entry citing the advisory ID, the upstream dep blocking the fix,
and the impact rationale (dev-only vs. runtime). The workflow's
`--ignore` flags MUST stay in sync with that list. When an upstream
patch lands, drop the ID from BOTH places in the same commit.

The threat model that drives the rest of this section is in
`docs/spec/threat-model.md` — read it before opening a PR that
touches the HTTP server, the SQL layer, the JWT helpers, or the
secrets-bearing log paths.

## Supply chain

Each release tag triggers a CI workflow that:

1. Builds the binaries on the supported target matrix.
2. Computes SHA-256 over each archive and writes a `.sha256` sidecar.
3. Uploads both the archive and the sidecar to the GitHub Release.

`install.sh` / `install.ps1` fetch the sidecar alongside the archive
and verify it before extracting. Older tags shipped before `.sha256`
support emit a warning but install — pinning to a recent tag is the
recommended path. Sigstore / cosign signing is on the
`PRODUCTION_READINESS_PLAN.md` Phase 6 list for after 1.0.
