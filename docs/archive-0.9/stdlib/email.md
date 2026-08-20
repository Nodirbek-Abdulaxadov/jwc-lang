---
sidebar_position: 5
description: "Send mail from JWC over SMTP with TLS (lettre + rustls). Configuration, the async send builtin, and attachments."
---

# Email

> **Scope note.** Email is not part of JWC's CRUD north star. The current
> SMTP builtins ship in the core because they're useful for auth flows
> (password reset, magic links), but the surface is intentionally
> minimal — no templating engine, no provider-specific APIs (SendGrid /
> SES). Larger email needs belong in a separate `jwc-email` package
> post-1.0.

SMTP transport over TLS via `lettre` + `rustls`. Async.

```jwc
async function send_welcome(to: string, name: string) {
    await send_email(
        to,
        "Welcome to Acme",
        "<h1>Hi " + name + "</h1><p>Glad you're here.</p>"
    );
}
```

## Config — env

| Env | Default | Effect |
|---|---|---|
| `JWC_SMTP_HOST` | required | e.g. `smtp.sendgrid.net` |
| `JWC_SMTP_PORT` | 587 | typically 587 (STARTTLS) or 465 (TLS) |
| `JWC_SMTP_USER` | required | SMTP login |
| `JWC_SMTP_PASS` | required | SMTP password |
| `JWC_SMTP_FROM` | required | `From:` address, e.g. `Acme <no-reply@acme.com>` |
| `JWC_SMTP_TLS` | `starttls` | `starttls` / `tls` / `none` |

## Built-in

| Built-in | Returns | Notes |
|---|---|---|
| `await send_email(to, subject, html_body)` | `void` | HTML body. Plain-text alternative is auto-generated. |

The `to` field accepts a single address. For broadcasts, call from a [background job](../backend/queue) once per recipient — keeps a slow SMTP server off the request path.

## Failures

Throws `Error` (no `EmailError` kind yet) — wrap in `try / catch (e)`:

```jwc
try {
    await send_email(req.to, "Confirm", "...");
} catch (e) {
    // log and queue for retry
}
```
