---
sidebar_position: 1
slug: /security
description: "JWC's secure-by-default posture: what the runtime protects against out of the box, the hardening knobs it exposes, and how to report a vulnerability."
---

# Security

JWC is built for production — the goal is "secure by default" on the
HTTP / DB / secrets axes that bite hardest in real deployments. This
page is the index; the per-topic sources of truth live in the repo:

- [`SECURITY.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/SECURITY.md) — vulnerability disclosure policy + supported versions.
- [`docs/spec/threat-model.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/docs/spec/threat-model.md) — the formal threat model.

## SSRF allowlist (`JWC_HTTP_ALLOWLIST`)

The runtime's outbound HTTP builtins (`http_get`, `http_post`,
`fetch_json`) are a classic SSRF vector — a handler that does
`http_get(body().url)` can be tricked into hitting `169.254.169.254`
or any internal service.

The defence is `JWC_HTTP_ALLOWLIST` (v0.4.8): a comma-separated host
allowlist applied at the outbound-call boundary. Empty (the default)
means "no restriction" — fine for dev, but pin it down in production:

```
JWC_HTTP_ALLOWLIST=api.stripe.com,api.sendgrid.com
```

Any other host returns an `HttpError` before the socket opens. The
match is on host string, not IP — so an attacker can't bypass the list
by passing `http://127.0.0.1.nip.io`.

See [stdlib/http](../stdlib/http.md) and
[deployment/env-vars](../deployment/env-vars.md#http-server-hardening).

## JWT validation — `exp` is mandatory

`jwt_verify(token, secret)` rejects tokens without an `exp` claim, and
rejects expired tokens. There's no "ignore `exp`" flag — letting a
token live forever is the kind of thing that doesn't belong in a
default-secure stdlib.

```jwc
let claims = jwt_verify(token, env("JWT_SECRET"));
if (claims == null) { return unauthorized(); }
// claims.exp is in the past → claims is already null at this point
```

See [stdlib/jwt-passwords](../stdlib/jwt-passwords.md).

## Secrets redaction in logs

The boot config table — printed by default at server start — masks any
env var whose name contains `PASSWORD`, `SECRET`, `TOKEN`, `KEY`,
`JWT`, or `DATABASE_URL` (case-insensitive substring). The mask is
purely a display concern; the runtime still reads the real value when
it needs it.

`DATABASE_URL` itself is intentionally not in the registry and never
echoed to logs.

Set `JWC_PRINT_CONFIG=0` to suppress the table entirely once values
are pinned in a deployment.

See [deployment/env-vars](../deployment/env-vars.md#secrets-redaction).

## Trusted proxy header chain

`client_ip()` reads `JWC_REAL_IP_HEADER` (default `x-forwarded-for`)
and walks the chain right-to-left, peeling off entries that match
`JWC_TRUSTED_PROXIES`. The first untrusted entry wins.

The default (`JWC_TRUSTED_PROXIES` empty) is "trust no proxy" — the
peer address wins. That's the safe default for a server exposed
directly. Behind a proxy / load balancer, list the proxy IP / prefix
explicitly so the real client IP gets through but a hostile header
can't forge one.

See [reference/builtins](../reference/builtins.md#request-context).

## Request hardening

| Knob | Default | Effect |
|---|---|---|
| `JWC_MAX_BODY_BYTES` | 2 MiB | Caps inbound request bodies — protects against memory exhaustion. |
| `JWC_REQUEST_TIMEOUT` | 30 s | Per-request budget; the watchdog returns 504 if the handler hasn't finished. |
| `JWC_SHUTDOWN_TIMEOUT` | 5 s | Graceful-shutdown budget — drains in-flight requests + queued jobs before exit. |

See [deployment/env-vars](../deployment/env-vars.md#http-server-hardening).

## Reporting a vulnerability

Don't open a public issue. Follow the disclosure flow in
[`SECURITY.md`](https://github.com/Nodirbek-Abdulaxadov/jwc-lang/blob/main/SECURITY.md).
