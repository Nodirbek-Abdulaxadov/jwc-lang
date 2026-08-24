---
sidebar_position: 3
title: "Errors"
description: "Declared errors carry their status. `or throw` turns an absent value into one, postfix `catch` handles a specific kind, and the handler is one place."
---

# Errors

An error is declared, and the declaration carries the status:

```jwc
namespace app;

error PaymentDeclined(message: text) = 402 : "to'lov rad etildi";
error RateLimited(message: text) = 429 : "so'rov ko'p";

function main() { serve(8080); }
```

Eight exist without being declared:

| | Status |
|---|---|
| `BadRequest` | 400 |
| `Unauthorized` | 401 |
| `Forbidden` | 403 |
| `NotFound` | 404 |
| `Conflict` | 409 |
| `Gone` | 410 |
| `TooManyRequests` | 429 |
| `ConstraintViolation` | 400 |

## `or throw`

The construct that most JWC code is built out of:

```jwc no-compile
let account = select A from App.auth.Accounts
    where id == $account_id
    as { id, email }
    first or throw NotFound("akkaunt topilmadi");
```

`first` answers `Record?`. `or throw` turns the null into the error, and
what comes out is a `Record` — so `$account.email` type-checks, with no
narrowing step and no null propagating three call frames before it
surfaces as a 500.

The same shape works on anything nullable:

```jwc no-compile
let secret = env("JWT_SECRET") or throw Unauthorized("server sozlanmagan");
let header = request.header("Authorization") or throw Unauthorized("token kerak");
```

## `throw`

```jwc no-compile
if ($board.project_id != $task.project_id) {
    throw BadRequest("vazifani boshqa loyihaga ko'chirib bo'lmaydi");
}
```

## Postfix `catch`

Handles one kind, and the block must **diverge** — return or throw. There
is no falling through to a value that was never produced.

```jwc no-compile
WorkspaceService.require_member($p.workspace_id, $req.user_id)
    catch NotAMember (e) {
        throw BadRequest("bu foydalanuvchi ish maydoni a'zosi emas");
    };
```

That is the shape for translating one layer's error into another's: a
service raising "not a member" is a 403 when it is about the caller, and a
400 when it is about somebody the caller named.

## The handler

```jwc no-compile
errorHandler {
    on PaymentDeclined (e) {
        return statusCode(402, { error: $e.message, retry: false });
    }
}
```

An arm is optional, because a declared error already has a status and a
message. The arm is for the ones that need a different body.

Without an arm, a thrown error becomes:

```json
{"error":"akkaunt topilmadi"}
```

with the declared status.

## Faults

Anything not a declared error — a database that is down, a bug — is a
*fault*. It is logged and answered as:

```json
{"error":"internal_error"}
```

with a 500, and nothing else. A fault is by definition something the
author did not anticipate, so there is nothing true to tell the client
about it.

## Validation has its own body

```json
{"error":"validation_failed","fields":[{"path":"email","rule":"pattern","message":"email shakli mos emas"}]}
```

Fixed, and not reachable from user code — see
[Validation](./validation.md).
