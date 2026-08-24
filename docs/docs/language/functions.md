---
sidebar_position: 4
title: Functions and services
description: "Free functions, services as the unit of logic, and the raise set that crosses a package boundary."
---

# Functions and services

## Free functions

```jwc no-compile
function invite_body(token: text) -> text {
    let base = env("APP_URL") ?? "https://app.example.com";
    return "Taklifnomani qabul qilish: " + $base + "/invites/" + $token;
}
```

Parameters are typed. The return type may be omitted and is inferred; write
it when it is part of a contract you want checked.

## Services

A `service` is the unit of logic, and the unit a package exports:

```jwc no-compile
service OrgService {
    function create(req: OrgCreate, owner_id: bigint) {
        transaction {
            let org = insert into App.org.Orgs { ...$req }
                as { id, slug, name, created_at };

            insert into App.org.Members {
                org_id     = $org.id,
                account_id = $owner_id,
                role       = MemberRole.owner
            };

            return $org;
        }
    }

    function detail(org_id: bigint) {
        return select O from App.org.Orgs
            where id == $org_id
            as { id, slug, name }
            first or throw NotFound("tashkilot topilmadi");
    }
}
```

Call it as `OrgService.create($req, $owner_id)`. Everything in a service is
exported; there is no `public` marker, because the service *is* the
boundary.

## Why routes stay thin

A route parses the request, calls a service and returns a response. That is
a style rule the language enforces only indirectly — but the pressure is
real: a postfix `catch` must diverge, so a route cannot use one to build
result-plumbing, and a package may not declare routes at all.

## Raise sets

The compiler infers what a function can raise. Application code may not
write a `raises` clause (`E1003`) — the inference is authoritative.

A **package** must write one, because a consumer compiles against the
declaration and not the body:

```jwc no-compile
service Billing {
    function charge(invoice_id: bigint) raises (NotFound, PaymentDeclined) {
        -- …
    }
}
```

The declared set must be a superset of the inferred one (`E1002`).
Narrowing is refused: a caller who handles exactly what the declaration
names would otherwise meet an error nothing told them about. Widening is
allowed, so a raise set can stay stable across a minor version.

An exported function that can raise and declares nothing is `W1501`.
