---
sidebar_position: 2
title: "Middleware"
description: "Chains that declare what they provide and what they require, so a route that forgets one fails to compile rather than reading null at run time."
---

# Middleware

```jwc
namespace middleware.auth;

middleware RequireAuth provides account_id: bigint {
    let header = request.header("Authorization") or throw Unauthorized("token kerak");
    let secret = env("JWT_SECRET") or throw Unauthorized("server sozlanmagan");

    let claims = jwt.verify($header, $secret)
        or throw Unauthorized("token yaroqsiz");

    context.account_id = bigint($claims.sub);
}
```

## `provides` and `requires` are checked

`provides account_id: bigint` says this middleware writes
`context.account_id`. A handler that reads it must have this middleware
in its chain, and the compiler checks that — a route that forgets it does
not compile, rather than reading null on the first request in production.

`requires` names another middleware that must run first:

```jwc no-compile
middleware RequireOrgMember(@org_id: bigint)
    requires RequireAuth
    provides org_id: bigint, role: MemberRole
{
    let account_id = context.account_id;
    …
}
```

The `(@org_id: bigint)` is a declared dependency on a path parameter: a
route whose pattern has no `{org_id}` cannot use this middleware.

## Attaching a chain

```jwc no-compile
routes "/api/v1/orgs/{org_id: bigint}" use RequireAuth, RequireOrgMember, Audit {
    route GET "" { … }

    route POST "" use RequireOrgAdmin {
        …
    }
}
```

The block's list runs first, then the route's. A middleware that appears
twice in one chain is an error.

## Short-circuiting

A middleware that **returns a response** stops the chain — the handler
does not run:

```jwc no-compile
middleware RateLimit {
    let ip = request.client_ip();
    let allowed = redis.rate_limit("rl:" + string.of($ip), 60, 60);

    if (!$allowed) {
        return tooManyRequests("juda ko'p so'rov");
    }
}
```

Falling off the end continues to the next one. A bare `return;`
short-circuits with a 204.

Throwing is different from returning: a throw goes to the error model
(see [Errors](./errors.md)), where the declared error's status and the
`errorHandler` decide the response.

## `after`

```jwc no-compile
middleware Audit {
    after {
        let status = response.status();
        let method = request.method();

        if ($method == "GET" or $status >= 400) {
            return;
        }

        insert into App.audit.Events {
            route      = request.route(),
            status     = $status,
            duration   = response.duration_ms(),
            created_at = date.now()
        };
    }
}
```

An `after` block runs on **every** outcome — the handler's response, a
middleware's short-circuit, an `errorHandler`'s answer — and it sees the
status actually being sent.

Every middleware that *started* runs its `after` block, in reverse chain
order, including the one that short-circuited. A middleware that opened
something has to be able to close it even when the request stopped at it.

An `after` block may add headers and nothing else. It cannot change the
status or the body: the response has already been decided, and a block
that could rewrite it would make the decision unreadable.

It also cannot recover from its own failure. An `after` that writes an
audit row and fails has nowhere to report it — which is why the audit
table's columns are nullable and its foreign keys are `on delete set
null`.
