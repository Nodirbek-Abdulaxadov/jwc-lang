# testing.md — `test`, `assert`, and isolation

Normative. Closes gap **#28** and **N9**.

---

## 1. `test`

1.1 A `test "<name>" { … }` block is a declaration. It may appear in any
file; by convention they live under `tests/`.

1.2 A test body is a **service** body: it may write, it may call service
functions, and it may open a `transaction` — the same rules, because a test
that cannot do what a service does cannot test one.

1.3 `jwc test [path]` runs every test in declaration order and reports each
by name. The exit code is non-zero if any fails.

---

## 2. Isolation (N9)

2.1 Each test runs inside its own transaction, which is **rolled back**
when the test ends — whether it passed, failed, or faulted. Nothing a test
writes outlives it.

2.2 This is the whole of the isolation model, and it is what makes the order
irrelevant. The sample's tests before v0.20.0 shared one org: the first test
inserted an active subscription, the second tried to create another for the
same org, and the partial unique index the first had satisfied made the
*second* test fail on its own first `insert` — a failure with nothing to do
with what it was testing.

2.3 Tests run **serially**, one connection at a time. Concurrency would
need each test on its own connection *and* a way to keep two uncommitted
transactions from deadlocking on the same rows, and neither buys anything
at this size.

2.4 There is no `seed` construct. `DEFERRED-11`: a shared fixture model is
exactly what N9 showed going wrong, and a test that builds its own data is
readable on its own. A helper `function` in the test file is how a fixture
is shared, and it is an ordinary function.

2.5 `jwc test --no-rollback` commits instead, for the case where a test's
effect is the thing being inspected by hand. It prints a warning naming the
database it is about to leave data in.

---

## 3. `assert`

3.1 `assert <expr>;` fails the test when `<expr>` is not `true`. A `null`
is not `true` (types §6.1), so an assertion on a `T?` fails rather than
passing vacuously.

3.2 The failure message names the expression as written, so a failing
assertion says which one failed without a line number lookup.

---

## 4. `assert fails` (#28)

```jwc no-compile
assert fails Conflict {
    insert into App.billing.Subscriptions { … };
} with "bu tashkilotda faol obuna allaqachon bor";
```

4.1 `assert fails <Error> { … }` requires the block to raise **that error
type**. Any other error, or none, fails the test. An error type is
mandatory: `assert fails { … }` on its own passes when a typo makes the
block raise something unrelated, which is the assertion testing itself
rather than the program.

4.2 `with "<message>"` additionally requires the raised error's message to
equal that string.

This is the half that matters (#28). A constraint's message is a
user-visible sentence and the *only* checkable artefact tying a schema
declaration to what a caller sees (errors §6.1). Without `with`, a test
passes when the message changes, when a different constraint on the same
table is violated instead, or when a refactor moves the failure to another
statement — all of which are the thing the test existed to catch.

4.3 The message is compared exactly. A near-miss is a failure with both
strings printed, because "close enough" is how a message drifts.

4.4 The block runs in a **savepoint**. Its writes are rolled back to the
point before it, so a test can assert a failure and keep going with a
database that is not in an aborted transaction — Postgres refuses every
subsequent statement in a transaction that has seen an error (`25P02`), and
without the savepoint the rest of the test would fail for that reason
instead of its own.

---

## 5. Diagnostics introduced here

| Code | Meaning |
|---|---|
| `E1401` | `assert fails` without an error type |
| `E1402` | `with` on an `assert fails` whose message cannot be a literal |
| `E1403` | `test` declared inside a service, routes block or middleware |
