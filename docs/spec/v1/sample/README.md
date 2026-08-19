# saas — the specification's ground truth

An obuna/billing platform written in JWC v1. It does not compile with the
0.9.x toolchain; it is the reference the specification is validated against
(ROADMAP §2, rule 4).

Checked by `python3 docs/spec/v1/check_sample.py`, which classifies every
construct here, maps it to the clause that defines it, and fails on anything
from the removed vocabulary.

## Layout

```
jwcproj.json           manifest — deps: redis, mail
src/
  app.jwc              database, schemas, server { }, errorHandler, main()
  db/                  tables — one file per schema
    auth.jwc           accounts, sessions, api_keys
    org.jwc            orgs, members, invites
    billing.jwc        plans, subscriptions, counters, invoices, lines, payments
    audit.jwc          events
  views/               output shapes
    org.jwc            OrgWithMembers, MemberAccess
    billing.jwc        SubscriptionDetail, InvoiceDetail, OrgBillingSummary
  dto/                 input shapes
    auth.jwc  org.jwc  billing.jwc
  middleware/
    auth.jwc           RequireAuth, RequireOrgMember, RequireOrgAdmin
    ratelimit.jwc      RateLimit, StrictRateLimit, VerifySignature
    audit.jwc          Audit (an `after` block)
  services/            domain operations — routes call these
    auth.jwc           AuthService
    org.jwc            OrgService + invite_body()
    billing.jwc        BillingService, WebhookService + next_invoice_number()
  routes/
    auth.jwc  orgs.jwc  billing.jwc  webhooks.jwc
tests/
  billing_test.jwc     each test builds its own fixtures
```

## Four schemas

| schema | what |
|---|---|
| `auth` | identity — account, session, API key |
| `org` | tenant — membership, invite |
| `billing` | money — plan, subscription, invoice, payment |
| `audit` | change log |

The boundary is one-directional: `billing` reaches `auth` only through a
foreign key, never the reverse.

## The rules this app is written to

- **`of <database>` means a real database object** — `table`, `view`, `enum`
  alike. An enum without `of` is varchar + CHECK; with `of` it is
  `CREATE TYPE` (schema §5).
- **A path is never assembled.** `routes "..."` prefix plus a `route`
  suffix. Two pieces, never three (routing §1.1).
- **Projection lives in the query.** `as { … }` is the SELECT list. There is
  no mapper (queries §6.1).
- **`class` is input only, `view` is output only** (types §4.1).
- **`private` / `server` close mass assignment in the language**, not by
  discipline (schema §3, types §9.4).
- **Raw by default; `as { }` makes a record.** Reading a field of a raw
  result is a compile error (types §5.2).
- **`$` on every local.** A bare name inside a query is a column, always
  (names §5.3).
- **No logic in a route.** Route = middleware + one service call +
  `json(...)`. Domain logic lives in a `service`, invariants live in
  constraints.
- **A constraint with a message becomes an error type.** `unique … : "…"`
  raises `Conflict` 409; `check … : "…"` raises `BadRequest` 400. Nobody
  writes a "is it taken?" pre-check (errors §6.1).
- **Services do not know HTTP.** They `throw NotFound(...)`; the status comes
  from the error type's declaration (errors §2.1, §4.3).
- **Money is `numeric`.** Not an integer count of cents (types §2.1).

## Endpoints

| Method | Path |
|---|---|
| POST | `/api/v1/auth/register` |
| POST | `/api/v1/auth/login` |
| GET PATCH | `/api/v1/me` |
| GET | `/api/v1/me/orgs` |
| POST | `/api/v1/orgs` |
| GET PATCH DELETE | `/api/v1/orgs/{org_id}` |
| GET | `/api/v1/orgs/{org_id}/members` |
| PATCH DELETE | `/api/v1/orgs/{org_id}/members/{account_id}` |
| GET POST | `/api/v1/orgs/{org_id}/invites` |
| DELETE | `/api/v1/orgs/{org_id}/invites/{invite_id}` |
| GET | `/api/v1/plans` |
| GET POST | `/api/v1/orgs/{org_id}/subscription` |
| POST | `/api/v1/orgs/{org_id}/subscription/cancel` |
| GET POST | `/api/v1/orgs/{org_id}/invoices` |
| GET | `/api/v1/orgs/{org_id}/invoices/{invoice_id}` |
| GET | `/api/v1/orgs/{org_id}/billing/summary` |
| POST | `/api/v1/webhooks/payments` |
