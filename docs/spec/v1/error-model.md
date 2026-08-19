I've read DESIGN.md, all 1,100 lines of the sample app, and the current compiler's error/transaction internals (`src/runner/exec.rs`, `src/engine.rs::with_tx`, `src/runner/dispatch.rs::run_error_handler`). Here is the analysis and decision.

---

# The error model for JWC 1.0

**Verdict up front:** keep automatic propagation (`throw` + one `errorHandler`), but stop treating it as an untyped runtime mechanism. Make error types *declared*, make each function's raise set *inferred* by the compiler over the static call graph, and *check exhaustiveness once* at the app boundary. Add exactly one local recovery form, savepoint-scoped inside transactions. Net effect: the sample app's routes stay 3 lines, its `errorHandler` shrinks from 8 arms to 0, and the two real bugs in it (a webhook retry loop, a 403 where 401 belongs) become compile-time or automatic.

---

## 0. Gaps DESIGN.md's "Known-invented" section does not name

These are not restatements of `??` / `context.get` / `days()` / `transaction`+`return`. Each is load-bearing for the error decision.

**G1 — Two mechanisms for one outcome, no rule for which.** `middleware/auth.jwc:10` writes `return unauthorized("token kerak")`; `services/auth.jwc:24` writes `throw Forbidden("email yoki parol xato")`. Both stop the request with a 4xx. Nothing says middleware may not throw, or that services may not `return forbidden(...)`. Two spellings of one concept, chosen by which block you happen to be in, is how a language grows a dialect.

**G2 — Error types have no declaration site.** DESIGN.md lists five names; it never says how a name comes to exist. Consequence: `throw NotFund("...")` is not a typo, it is a valid program that falls through every typed arm into `catch (err)` and returns 500. The Go camp's best objection — *catch-all arms swallow real bugs* — already lands, today, on the throw design's own turf. Second consequence: the set is closed. There is no way to write `PaymentDeclined`.

**G3 — "Constraint violations become 400 automatically" contradicts `app.jwc:22`.** If it is automatic, `catch ConstraintViolation (err)` is dead code. If the arm is live, it isn't automatic. Worse, neither branch covers *unmessaged* constraints: every `foreign key` in the sample has no `: "message"`. A bad `plan_id` reaching `insert into Subscriptions` therefore hits `catch (err)` → 500, when the honest answer is 400 or 409.

**G4 — The sample calls builtins the design doesn't define, and picks a wrong status.** `app.jwc:23` calls `internalError()`; `app.jwc:19` reaches for `statusCode(409, …)` because there is no `conflict()`. Neither `internalError` nor `conflict` is in the bare-builtin list. And `services/auth.jwc:24,28` uses `Forbidden` (403) for bad credentials, where 401 is correct — 403 means *authenticated but not allowed*. There is no `Unauthorized` error type to reach for.

**G5 — Nothing says whether `after { }` runs on the throw path, or what status it sees.** `middleware/audit.jwc:10-14` reads `response.status()` and skips when `>= 400`. That code is only meaningful if the after-chain runs *after* the errorHandler produced its response — otherwise there is no status to read. This is unstated, and it is exactly the class of bug that bit the current compiler (task #11, "run the after-chain when a native middleware short-circuits").

**G6 — There is no local recovery construct at all.** The single global `errorHandler` is the only catch in the language. A service therefore cannot retry, cannot fall back, and cannot skip a bad row. `create_invoice` (`services/billing.jwc:106`) loops `for (line in req.lines) { insert … }` under `check (quantity > 0) : "miqdor musbat"`. One bad line aborts the whole request and the client is told which rule broke but not which row. There is no syntax that could do better.

**G7 — The errorHandler's position relative to the transaction is unspecified.** Distinct from the named `return`-semantics gap. If `create_invoice` throws at line 3 of the loop, does the handler run before or after `ROLLBACK`? If before, it runs on a connection Postgres has already put in `25P02` (aborted), and every statement it issues fails — so any handler that logs to `App.audit.Events` turns every 404 into a 500. Nothing in the design forbids that handler.

**G8 — `record_payment` has a TOCTOU race that the design's own rules turn into an infinite retry loop.** `services/billing.jwc:129-139` does select-then-insert against `provider_ref varchar(120) unique : "bu to'lov allaqachon qayd etilgan"`. Two concurrent deliveries of the same Stripe event both see `seen == null`, both insert, one violates the unique index. Per DESIGN.md that becomes a **400 with an Uzbek sentence**. Stripe reads 4xx as "malformed, retry with backoff" and redelivers. The one place in the app where "already happened" is explicitly a *normal* outcome is also the one place where the error model manufactures a client error out of it.

**G9 — No machine-readable code anywhere.** Every arm returns `{error: "<localized string>"}`. A client cannot distinguish "plan not found" from "org not found". A retry policy cannot distinguish retryable from terminal.

**G10 — Nested transactions across service calls are a runtime bail, not a compile error.** The sample has no service-to-service call, so this is latent — but `engine::with_tx` currently `bail!`s at runtime when one is already open. A future `OrgService.create` calling `BillingService.subscribe` (both wrapping `transaction { }`) produces a 500 discovered in production. The call graph is fully static; this is checkable.

---

## 1. Position: Go-style explicit error values

### The strongest case for it

**Signatures currently lie by omission.** `BillingService.subscribe(org_id, req)` is indistinguishable, at the call site, from `BillingService.plans()`. One can terminate the request with a 400 and a 409; the other cannot fail at all. Nothing in the source of `routes/billing.jwc:28` says so. The reader must open the service, and then open every function *it* calls, transitively. That is a real cost and no amount of "the handler is in one place" answers it.

**Invisible control flow is worst precisely where JWC puts it.** In `create_invoice` the `throw` happens inside `transaction { }`, six frames below the `BEGIN`. The reader of `create_invoice` sees a linear body; the actual exit is a non-local jump through a connection-scoped resource. Go's position is that a control transfer you cannot see in the text is a control transfer you will not reason about.

**The catch-all is a bug-eating machine.** `app.jwc:23` — `catch (err) { return internalError(); }` — catches a pool timeout, a malformed `JWT_SECRET`, an arithmetic overflow in `sum(req.lines, …)`, a `null` reaching `hash.verify`, and a genuine `throw` whose type name was misspelled (G2). All become the same opaque 500. Under errors-as-values there is no catch-all, because there is nothing to catch: each failure is produced by name at the point that knows what it means.

**Errors as values force naming at the site with the domain knowledge.** `plan == null` is not "an error"; it is "the plan code the client sent does not exist." Making the producer construct the value is making the producer say what happened.

### What the sample app actually degrades into

JWC has no tuples and no generics, so Go-style needs multi-value return. (The alternative — a `{value, err}` wrapper record — collides with the raw-forwarding promise: a raw `row_to_json` result would have to be boxed into a record, allocated, and re-serialised, which is exactly the parsing DESIGN.md eliminated. Multi-value return is the honest-best encoding here.)

```jwc no-compile
service BillingService {

    function subscribe(org_id, req: SubscribeRequest) -> (Subscription, Error) {
        let plan, err = select Plans from App.billing.Plans
            where Plans.code == req.plan_code and Plans.active == true
            as { id, interval }
            first;

        if (err != null)  { return null, err; }
        if (plan == null) { return null, BadRequest("tarif topilmadi"); }

        let trial_days = req.trial_days ?? 0;
        let is_trial   = trial_days > 0;
        let trial      = days(trial_days);
        let period     = (plan.interval == BillingInterval.yearly) ? days(365) : days(30);
        let starts_at  = now();
        let ends_at    = starts_at + trial + period;
        let status     = is_trial ? SubscriptionStatus.trialing : SubscriptionStatus.active;

        let sub, err2 = insert into App.billing.Subscriptions {
            org_id               = org_id,
            plan_id              = plan.id,
            status               = status,
            current_period_start = starts_at,
            current_period_end   = ends_at
        };

        if (err2 != null) { return null, err2; }

        return sub, null;
    }
}
```

```jwc no-compile
route POST "" use RequireOrgAdmin {
    let req, berr = request.body() as SubscribeRequest;

    if (berr != null) { return badRequest({ error: berr.message }); }

    let sub, err = BillingService.subscribe(@org_id, req);

    if (err != null) { return error_response(err); }

    return created(json(sub));
}
```

```jwc no-compile
function login(req: Login) -> (Session, Error) {
    let account, err = select Accounts from App.auth.Accounts
        where Accounts.email == req.email
        as { id, password_hash }
        first;

    if (err != null)      { return null, err; }
    if (account == null)  { return null, Unauthorized("email yoki parol xato"); }

    let ok = hash.verify(req.password, account.password_hash);

    if (!ok) { return null, Unauthorized("email yoki parol xato"); }

    let secret      = env("JWT_SECRET");
    let ttl_minutes = int(env("JWT_TTL_MINUTES") ?? "60");
    let token, terr = jwt.sign({ sub: account.id }, secret, ttl_minutes);

    if (terr != null) { return null, terr; }

    return { token: token, expires_in: ttl_minutes * 60 }, null;
}
```

```jwc no-compile
function record_payment(req: WebhookPayment) -> (Receipt, Error) {
    transaction {
        let seen, err = select Payments from App.billing.Payments
            where Payments.provider_ref == req.provider_ref
            as { id }
            first;

        if (err != null)  { return null, err; }
        if (seen != null) { return { status: "duplicate" }, null; }

        let _p, ierr = insert into App.billing.Payments { ...req, provider = "stripe" };

        if (ierr != null and ierr.type == "ConstraintViolation") {
            return { status: "duplicate" }, null;
        }
        if (ierr != null) { return null, ierr; }

        let succeeded = req.status == PaymentStatus.succeeded;

        if (succeeded) {
            let paid_at = now();
            let _u, uerr = update App.billing.Invoices
                set status = InvoiceStatus.paid, paid_at = paid_at
                where Invoices.id == req.invoice_id;
            if (uerr != null) { return null, uerr; }
        }

        return { status: "ok" }, null;
    }
}
```

Now the honest accounting.

**The webhook case is the only one that got better**, and it got better for a reason that has nothing to do with errors-as-values: it is the only place a caller genuinely branches. Notice what it cost — `ierr.type == "ConstraintViolation"` is a *string comparison*, because there are still no declared error types (G2). Go-style did not fix G2; it just moved the untyped dispatch from `catch` to `if`.

**The other three got measurably worse and gained nothing.** Not one of them branches. `subscribe` went from 20 lines to 27, and the 7 added lines are `if (err != null) { return null, err; }` three times. `login` went from 20 to 26. The route went from 3 lines to 6, which deletes the design's own stated rule — *"Route ichida mantiq yo'q. Route = middleware + service chaqiruvi + `json(...)`"* — as a casualty.

**Count across the whole sample app:** 24 route handlers, 10 service `throw`s, 7 middleware short-circuits, ~31 fallible DB operations inside services. Of those 41 failure points, **40 terminate the request with a status and zero callers branch.** Exactly one alternative outcome is a real branch — the webhook duplicate — and in the current design it is already expressed as a plain `return`, not as an error at all. Errors-as-values taxes 40 sites to serve 1, and the 1 was never using the error machinery.

Line cost: roughly +75 lines across routes (they grow ~34%) and +62 in services, ~140 lines on a ~660-line logic layer — **a 21% increase, none of it domain logic.** And note `err`, `err2`, `berr`, `ierr`, `uerr`: DESIGN.md's style rule is *"Name every intermediate value with `let`"*, and JWC has no shadowing story, so the error variables are forced to be numbered. That is not an aesthetic complaint; it is the mechanism by which the wrong `err` gets checked twice and the right one never.

**Structural cost the line count hides:** multi-value return is not a local addition. It splits every call into fallible and infallible, and a fallible call can no longer appear in expression position. `int(env("DB_POOL") ?? "20")`, `days(trial_days)`, the ternaries, `??` — all of these assume single-value calls. Go-style makes the grammar bifurcate.

**Where the Go camp is genuinely right, and I will not wave it away:** the 40-of-41 count holds *because the sample app is CRUD*. The first retry, the first fallback, the first bulk import that must continue past a bad row needs a branch — and the current design cannot express one at all (G6). That is a real hole. It is just not a hole that multi-value return is the right patch for.

---

## 2. Position: the current `throw` / `errorHandler` design

### The strongest case for it

**JWC is a request/response language, and in a request/response language a failure has exactly one meaningful action: stop, and produce a status.** The unit of work is bounded, short-lived, and owns no state the caller shares. Unwinding to the request boundary is not exotic control flow smuggled into the language — it is *the* control flow of the runtime, which every server framework in existence implements whether or not the language admits it (Rails `rescue_from`, ASP.NET exception middleware, Spring `@ControllerAdvice`). The design's job is to let the source say what the runtime already does, rather than making every developer hand-roll the propagation the framework will do anyway.

**One handler, one place.** Adding `Conflict` to the API is one line in `app.jwc`, not 24 edits across `routes/`. Under errors-as-values, `error_response(err)` reappears — it is the errorHandler, renamed — except now it is a call site you can forget, and forgetting it returns 200 with a null body. A mechanism that is mandatory beats a mechanism that is conventional.

**Services stay HTTP-ignorant, and that boundary is real.** `throw NotFound("hisob topilmadi")` states a domain fact. `app.jwc` decides that domain fact is a 404. `BillingService` is callable from a job or a test without dragging in a status code. The sample's own README names this as a design rule and it is a good one.

**The classic exception hazards require things JWC deliberately does not have.** The canonical arguments against unwinding are leaked resources, half-run destructors, and an object graph left in a half-updated state. JWC has no user-visible resource acquisition — no file handles, no locks, no `defer`. Load-modify-save *is not expressible* (DESIGN.md, "Writes"), so there is no in-memory aggregate to leave half-mutated. The one resource that matters is the DB connection, and it is owned by `engine::with_tx`, which rolls back and returns it to the pool on `Err` regardless of how deep the unwind started. **"Unwinding across a transaction boundary is dangerous" is true in C++ and Java; in JWC the transaction boundary is the one place unwinding is already correct.**

### Answering the invisible-control-flow objection honestly

**The objection is right, in one specific form, and "the handler is in one place" does not answer it.** Reading `routes/billing.jwc:28`, you cannot tell that `subscribe` can produce a 400 and a 409. The API surface of a route is not visible from the route. Anyone documenting this API has to read the transitive call graph by hand.

**It is worse here than in Java or C#, for a reason specific to this implementation.** Look at what `run_error_handler` actually does (`src/runner/dispatch.rs:338`): it takes an `anyhow::Error`, splits `err.chain()` into `{message, causes}`, and typed `catch` arms match **by name against a string**. There is no declaration site for `NotFound`. So the design's typed catch is nominal typing without nominals — and `throw NotFund("...")` compiles, misses every arm, and returns 500 (G2). The Go argument "catch-all arms swallow real bugs" is not a hypothetical here; the current design guarantees it for every typo.

**The catch-all genuinely erases information that matters.** `catch (err) { return internalError(); }` cannot distinguish "Postgres is unreachable" from "the developer passed null to `hash.verify`". Both are 500, both are silent. That is a defect of *this handler*, not of unwinding — but the design as written offers no way to write a better one, because it gives the handler nothing but `err.message`.

**Honest summary of position 2:** the propagation mechanism is right for this language. The *typing* of it is missing, and every serious objection to it lands on the missing typing rather than on the propagation.

---

## 3. Position: the third option

Both objections above point at the same missing thing: **the compiler does not know what a function can raise.** Fix that and most of the argument dissolves.

Take the checked-effect option, with the failure mode of Java's checked exceptions consciously designed out. Java's `throws` failed because the annotation was **manual**, appeared on **every intermediate signature**, and was enforced at **every call site** — so people wrote `throws Exception` and the guarantee evaporated. JWC differs on all three axes:

- The call graph is **fully static**. No first-class functions, no dynamic dispatch, no interfaces, no module system beyond a flat namespace. `project::load` already merges every `.jwc` file into one `Program`. A whole-program fixpoint over raise sets is not just possible, it is cheap.
- There is **exactly one handler**. Enforcement happens at one boundary, not at every call.
- Therefore the annotation can be **inferred**, and nobody writes anything.

The shape:

1. **Errors propagate automatically.** No `?`, no `, err`, no ceremony. Identical to today at the source level.
2. **The compiler infers each function's raise set** — its own `throw`s and `or throw`s, the callee raise sets, and the constraint violations of the tables it writes — minus anything a local `catch` absorbs. Fixpoint from empty, iterate to stability.
3. **The `errorHandler` must cover the union of every route's and middleware's inferred set.** An uncovered type is a compile error naming the route, the throw site, and the missing arm. Exhaustiveness checking, from pattern matching, applied to errors, done once.
4. **Error types are declared**, which kills G2 and makes the set open.
5. **Two kinds, not one.** `error` = declared, domain, part of the API contract. `fault` = undeclared runtime failure (pool timeout, JSON coercion, null deref, arithmetic) — always 500, always logged with type and origin, never silently reshaped. `catch (err)` catches faults only, and **does not** satisfy exhaustiveness for declared errors. The catch-all can no longer swallow a domain error you forgot, because the compiler makes you handle it; and it can no longer swallow a bug into an opaque 500, because faults log.
6. **One local recovery form** (closing G6), statement-scoped so it cannot metastasise into try/catch soup.

Rust-style `?` is the weaker sibling of this: it makes propagation visible at each hop, which is a real gain, but it re-imposes per-call ceremony to buy visibility the compiler could compute — and it needs a declared return type on every function, which JWC does not have and would have to invent.

The "values only where the caller branches" option turns out to be **already satisfied** by JWC as designed, which is the observation that settles this: `select … first` returns `null`, not an error. `hash.verify` returns bool. `jwt.verify` returns null. Every genuine branch in the sample app — `account == null`, `seen != null`, `!ok`, `!is_owner`, `claims == null` — **branches on a value, not on an error.** The Go benefit is already there, on 100% of the cases where it is wanted, and it costs nothing. Errors are reserved for the case where the caller does *not* branch. That split is already the right one; it was just never stated as a rule.

---

## 4. The decision

**JWC 1.0 uses automatic propagation with compiler-inferred, exhaustively-checked error types.** Concretely:

- Keep `throw` and one global `errorHandler`. Routes stay 3 lines.
- Add `error` declarations. Seven built-ins are pre-declared *with a default status*: `BadRequest` 400, `Unauthorized` 401, `Forbidden` 403, `NotFound` 404, `Conflict` 409, `TooManyRequests` 429, `ConstraintViolation` 400. **User-declared errors carry no default status** and therefore *must* get an `errorHandler` arm.
- Split `error` (declared, contract) from `fault` (undeclared, 500 + mandatory log).
- Add one local recovery form: postfix `catch`, whose block must diverge.
- `or throw` handles **absence** (null). Postfix `catch` handles **errors**. Never conflate them.

The default statuses matter more than they look: they mean the sample app's 8-arm `errorHandler` **can be deleted entirely** and the app behaves identically. Ceremony goes down relative to the current design, not up. An arm is written only to reshape an envelope or to give a user-declared error a status — and in the latter case the compiler demands it.

### The strongest objection to this decision

**Adding a new error type deep in a service breaks the build of an app whose `errorHandler` was fine.** That is Java's `throws` pain relocated — better, because it surfaces in one file instead of every intermediate signature, but not free. And it is genuinely bad in one context: **packages.** `jwc-registry` exists; JWC packages are a real thing. A package that adds `throw PaymentDeclined(...)` in a patch release breaks every consumer's compile.

Two mitigations, and I think they hold:

- The seven built-ins have default statuses, so a package raising `Conflict` or `NotFound` breaks nothing. Only a *new* error type breaks consumers — and that genuinely *is* a breaking API change, correctly reported as one.
- A package's exported service functions may write `raises (NotFound, Conflict)` explicitly. The compiler checks the declaration is a **superset** of the inferred set — widenable for forward compatibility, never silently narrowable. This is the only place an annotation is ever written, and it earns its place as a semver-visible contract. Application code never writes it.

The residual objection I cannot fully dissolve: on day one, someone will discover that declaring `error Whatever(message)` and giving it a `catch Whatever → 500` arm silences the compiler. Rule 4 (catch-all covers faults only) blocks the laziest version; nothing blocks the determined version. I accept that. A type system that can be defeated by someone deliberately defeating it is still worth having.

### What the compiler must enforce

| # | Rule |
|---|---|
| **E1** | Every name in `throw` / `catch` resolves to a declared or built-in `error`. Unknown name = compile error. *(kills G2)* |
| **E2** | Raise set inferred per function: own `throw`s ∪ `or throw`s ∪ callee raise sets ∪ constraint violations of tables written − types absorbed by an enclosing postfix `catch`. Fixpoint over the static call graph; handles mutual recursion by starting empty. |
| **E3** | The union of raise sets over all route handlers, middleware bodies, and `after` blocks must be covered by `errorHandler` arms **or** by a built-in default status. Uncovered user-declared type = compile error naming route, throw site, missing arm. |
| **E4** | Untyped `catch (err)` covers **faults** only. It does not satisfy E3 for any declared error. |
| **E5** | An `errorHandler` arm for a type nothing raises = warning, "unreachable arm". *(flags `app.jwc:22`, G3)* |
| **E6** | Every `errorHandler` arm must terminate in a response. Falling off the end = compile error. |
| **E7** | `after { }` blocks must have an **empty** raise set. A throw there is a compile error — the response already exists and there is nowhere for a second one to go. *(G5)* |
| **E8** | A postfix `catch` block must diverge (`return` / `continue` / `break` / `throw`). No expression-blocks introduced. |
| **E9** | Inside `transaction { }`, a postfix `catch` compiles to `SAVEPOINT` / `RELEASE` / `ROLLBACK TO`. Outside, it is a plain guard. **Non-optional:** catching a `ConstraintViolation` mid-transaction without a savepoint leaves Postgres in `25P02` and every later statement fails. |
| **E10** | Constraint promotion: a constraint **with** `: "message"` synthesises a declared error — `unique` → `Conflict` (409), `check` → `BadRequest` (400), `foreign key` → `BadRequest` (400). A constraint **without** a message raises a **fault** → 500 + log. *(resolves G3; makes the `: "message"` annotation the thing that promotes a DB constraint into a documented API error — which makes it serve the DBA test and the API contract at once)* |
| **E11** | Body validation (`request.body() as X`) synthesises `throw BadRequest(…)` with a `details` field, routed through the errorHandler like anything else. No out-of-band 400 path. |
| **E12** | Package-boundary services may declare `raises (…)`; checked as a superset of the inferred set. Application code may not. |
| **E13** | Transaction nesting is detected **at compile time** over the call graph, not bailed at runtime. *(G10)* |
| **E14** | Middleware may `throw`. `return <response>` stays legal, but only for a deliberate **non-error** response (redirect, 304, 202). Anything that is a failure with a status must throw, so the errorHandler owns the envelope shape and the `after` chain sees a consistent one. *(resolves G1)* |

### How transactions interact

- **A raised error inside `transaction { }` → `ROLLBACK`,** and the error keeps unwinding. (Matches `engine::with_tx` today: `Err(_)` → `ROLLBACK`.)
- **An early `return` inside `transaction { }` → `COMMIT`, then return.** `return` is a success exit. Both `create_invoice` and `record_payment` depend on this. (Also matches today: `Ok(_)` → `COMMIT`, including `Ok(Flow::Return(..))`.) This is the design decision DESIGN.md left open, and it should be written down as: **errors roll back, returns commit.**
- **The `errorHandler` runs after the rollback and outside any transaction.** Non-negotiable (G7): otherwise a handler that writes an audit row runs on an aborted connection and turns every 404 into a 500. If the handler needs its own DB work, it opens its own `transaction { }`.
- **A postfix `catch` inside a transaction does not roll back the whole transaction** — savepoint-scoped per E9. This is the only reason local recovery is expressible at all inside a transaction.
- **The `after` chain runs on the error path**, after the errorHandler has produced the response, so `response.status()` is the *mapped* status (404, not 500). This is what `middleware/audit.jwc` already assumes; state it (G5).
- `savepoint` remains available as an explicit user-facing block; postfix `catch` is its sugar for the single-statement case.

---

## 5. The decided form, written out

### `src/app.jwc` — declarations and handler

```jwc no-compile
namespace app;

database App : Postgres {
    init() {
        pool_size         = int(env("DB_POOL") ?? "20");
        statement_timeout = "10s";
        tls               = env("DB_TLS") == "1";
    }
}

schema auth    of App;
schema org     of App;
schema billing of App;
schema audit   of App;

-- Built-in errors are pre-declared with default statuses:
--   BadRequest 400, Unauthorized 401, Forbidden 403, NotFound 404,
--   Conflict 409, TooManyRequests 429, ConstraintViolation 400.
-- Nothing below is required. The whole errorHandler may be deleted.

-- A user-declared error has NO default status, so E3 forces an arm.
error PaymentDeclined (message, provider_code);

errorHandler (e) {
    catch PaymentDeclined (err) {
        return statusCode(402, { error: err.message, code: err.provider_code });
    }
    catch (err) {
        -- faults only; E4 forbids this from covering PaymentDeclined
        log.error(err.type, err.message, err.origin);
        return internalError();
    }
}

function main() {
    serve(int(env("PORT") ?? "8080"));
}
```

### 1. `BillingService.subscribe`

```jwc no-compile
service BillingService {

    function subscribe(org_id, req: SubscribeRequest) {
        let plan = select Plans from App.billing.Plans
            where Plans.code == req.plan_code and Plans.active == true
            as { id, interval }
            first
            or throw BadRequest("tarif topilmadi");

        let trial_days = req.trial_days ?? 0;
        let is_trial   = trial_days > 0;

        let trial  = days(trial_days);
        let period = (plan.interval == BillingInterval.yearly) ? days(365) : days(30);

        let starts_at = now();
        let ends_at   = starts_at + trial + period;

        let status = is_trial ? SubscriptionStatus.trialing
                              : SubscriptionStatus.active;

        return insert into App.billing.Subscriptions {
            org_id               = org_id,
            plan_id              = plan.id,
            status               = status,
            current_period_start = starts_at,
            current_period_end   = ends_at
        };
    }
}
```

Inferred: `raises (BadRequest, Conflict)`. `BadRequest` from the `or throw`; `Conflict` from `unique (org_id) where status != SubscriptionStatus.canceled : "bu tashkilotda faol obuna allaqachon bor"` via E10. Both built-in, so no handler arm is required. **The duplicate-subscription 409 needs no code at all** — it comes from the schema, which is the design's own stated promise (`README.md`: *"qo'lda 'band emasmi' deb tekshirish kerak emas"*), now actually delivered rather than asserted.

Net change from the original: `if (plan == null) { throw … }` collapses into `or throw`. Two lines shorter, and the failure is now visible on the line that can fail.

### 2. The route that calls it

```jwc no-compile
routes "/api/v1/orgs/{org_id}/subscription" use RequireAuth, RequireOrgMember, Audit {

    route POST "" use RequireOrgAdmin {
        let req = request.body() as SubscribeRequest;
        let sub = BillingService.subscribe(@org_id, req);

        return created(json(sub));
    }
}
```

Byte-for-byte identical to the original. That is the deliverable: the error model got a type system and the route did not move.

### 3. `AuthService.login`

```jwc no-compile
service AuthService {

    function login(req: Login) {
        let account = select Accounts from App.auth.Accounts
            where Accounts.email == req.email
            as { id, password_hash }
            first;

        if (account == null) { throw Unauthorized("email yoki parol xato"); }

        let ok = hash.verify(req.password, account.password_hash);

        if (!ok) { throw Unauthorized("email yoki parol xato"); }

        let secret      = env("JWT_SECRET");
        let ttl_minutes = int(env("JWT_TTL_MINUTES") ?? "60");
        let token       = jwt.sign({ sub: account.id }, secret, ttl_minutes);

        return {
            token:      token,
            expires_in: ttl_minutes * 60
        };
    }
}
```

Three things to note, and all three are the point.

`Forbidden` → `Unauthorized`: the original returned 403 for bad credentials (G4); 403 means authenticated-but-not-allowed. Fixed, and `Unauthorized` now exists as a type.

**Two branches, one outcome, deliberately** — identical message for "no such account" and "wrong password", so the endpoint is not an account-enumeration oracle. This is the case people cite as the argument for errors-as-values, and it is the case that refutes it: `login` is the *only* caller that could branch, and it branches on `account == null` — a **value**, not an error. Errors-as-values buys nothing here because the interesting branch was never carried by the error channel.

*Not* deliberate and worth fixing separately: `hash.verify` runs only when the account exists, so response latency distinguishes the two cases. That is a timing oracle and it is present in the original too. It is an auth-design bug, not an error-model bug, and it should not be smuggled into this decision — but the compiler cannot see it, so someone has to.

Inferred: `raises (Unauthorized)`. Built-in. No arm required.

### 4. Webhook idempotency

```jwc no-compile
service WebhookService {

    function record_payment(req: WebhookPayment) {
        transaction {
            let seen = select Payments from App.billing.Payments
                where Payments.provider_ref == req.provider_ref
                as { id }
                first;

            -- normal outcome, not an error: commits (an empty tx) and returns 200
            if (seen != null) { return { status: "duplicate" }; }

            insert into App.billing.Payments {
                ...req,
                provider = "stripe"
            }
            catch Conflict (err) {
                -- concurrent delivery won the race on
                --   provider_ref unique : "bu to'lov allaqachon qayd etilgan"
                -- E9: ROLLBACK TO SAVEPOINT, connection stays usable
                return { status: "duplicate" };
            };

            let succeeded = req.status == PaymentStatus.succeeded;

            if (succeeded) {
                let paid_at = now();

                update App.billing.Invoices
                    set status  = InvoiceStatus.paid,
                        paid_at = paid_at
                    where Invoices.id == req.invoice_id;
            }

            return { status: "ok" };
        }
    }
}
```

This one case exercises every rule and is why the decision is shaped this way.

*"Already seen" is a normal outcome, so it is a `return`, not a `throw`* — the error channel is never involved, and the response is 200 with `{status: "duplicate"}`. Exactly as the original had it, and correct.

*The race is real and the original loses it* (G8). Two concurrent deliveries both see `seen == null`; one violates the unique index. Under DESIGN.md as written that surfaces as a **400 with an Uzbek sentence**, Stripe reads 4xx as retryable, and the webhook redelivers forever. Under E10 it is a `Conflict`, and the postfix `catch` converts it to the same normal duplicate response. **One line, and the retry loop is gone.**

*The `catch` must be savepoint-scoped* (E9), because it is inside `transaction { }`. Without the savepoint, the failed `INSERT` puts the connection in `25P02` and the subsequent `UPDATE App.billing.Invoices` fails with "current transaction is aborted" — a 500 on a path that just successfully handled a duplicate. This is the concrete reason local recovery cannot be bolted onto the current design without touching transaction semantics, and it is why the two decisions had to be made together.

*The `catch` block diverges* (E8), and its type is subtracted from the inferred set (E2), so `record_payment` infers `raises ()` — nothing. The webhook route is provably total. The compiler can state that.

---

**Summary of what changes in the sample app:** `app.jwc`'s errorHandler drops from 8 arms to 2 (and could be 0). Four `if (x == null) { throw … }` pairs collapse into `or throw`. One wrong status code (403→401) is corrected. One infinite-retry bug is fixed by a single `catch Conflict` line. Zero routes change. The added machinery — `error` declarations, inferred raise sets, exhaustiveness at one boundary — costs the application author **nothing to write** and costs the package author one optional `raises (…)` clause at the export boundary.