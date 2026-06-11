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

Preferred channel: GitHub **private vulnerability reporting** —
[Security → Report a vulnerability](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/security/advisories/new)
on this repository.

Include where possible: affected component (CLI, runtime/server, LSP,
install scripts, VS Code extension), a minimal `.jwc` reproduction or
request trace, the version (`jwc --version`), and impact assessment.

## What to expect

- **Acknowledgement** within 72 hours.
- **Triage + severity assessment** within 7 days.
- **Fix target**: high/critical issues within 14 days of triage; lower
  severities in the next regular release.
- Credit in the release notes and advisory, unless you ask otherwise.

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
