# `Value::Record` — shape-table design for the interpreter

Status: **design** (Phase 1 of `PRODUCTION_READINESS_PLAN.md`, sprint item 2).
Nothing here is implemented yet; this document fixes the shape of the change
so the implementation PRs can land incrementally without semantic drift.

## Problem

The interpreter `Value` enum (`src/runner/mod.rs`) is
`Int/Float/Str/Bool/Null/Void/Array` — there is **no structured object**.
Objects and DB rows travel as JSON **strings**:

- `Expr::FieldGet` (`runner/mod.rs`, `Value::Str` arm) calls
  `serde_json::from_str` on **every** field access — a full re-parse of the
  whole row to read one column.
- `Expr::FieldSet` does parse → mutate → re-serialize.
- `value_to_json_smart` has to *guess* whether a `Value::Str` is a plain
  string or a JSON object/array by attempting a parse, so a user string that
  happens to look like JSON (`"[1,2]"`) silently changes serialization
  behavior.
- Object-literal evaluation (`Expr::ObjectLit`) builds a `serde_json::Map`
  and immediately throws the structure away by calling `.to_string()`.

The AOT runtime already solved its half in v0.4.1: dynamic
`V::Object(Arc<FxHashMap<String, V>>)` with CoW. The interpreter is the
remaining "objects are strings" runtime.

## Design

### Two representations, chosen by what the compiler knows

```rust
enum Value {
    // ... existing variants ...
    /// Statically-shaped instance: entity/class row. Field name → slot
    /// resolved through the shape table; access is an array index.
    Record {
        shape: ShapeId,
        fields: Arc<Vec<Value>>,
    },
    /// Dynamic object: json_parse of arbitrary payloads, object literals
    /// with unknown/computed shape, middleware ctx. Same representation
    /// family as the AOT `V::Object`.
    Object(Arc<FxHashMap<String, Value>>),
}
```

`Arc` + copy-on-write (`Arc::make_mut` on mutation) mirrors the AOT v0.4.1
payload design, so clones are refcount bumps and the eventual shared
`jwc-runtime` crate can unify both enums.

### Shape table

Built once at program load (after `validate_program`, before `run_main`)
from every `ModelDecl` (entities **and** classes — both have compile-time
shapes):

```rust
struct ShapeId(u32);                 // index into ShapeTable.shapes

struct Shape {
    name: String,                    // "User"
    fields: Vec<String>,             // declaration order, slot = index
    index: FxHashMap<String, u32>,   // lowercase field name → slot
}

struct ShapeTable { shapes: Vec<Shape>, by_name: FxHashMap<String, ShapeId> }
```

- Slot order = declaration order. JSON serialization of a `Record` walks
  slots and **sorts keys at serialisation time**, matching the AOT
  `jwc_write_json` contract, so interpreter and native output stay
  byte-for-byte identical (conformance suite enforces this).
- The table is immutable after load → safe to share via `Arc<ShapeTable>`
  across tokio tasks; no locking on the hot path.

### Field access resolution

Three tiers, best available wins:

1. **Compile-time slot.** When the variable's static type is known (typed
   params `function f(u: User)`, `select User ...` results, `new User()`),
   lowering rewrites `FieldGet { var, field }` to
   `FieldGetSlot { var, shape, slot }` — O(1) vector index, no hashing.
2. **Runtime shape lookup.** `Record` accessed through an untyped path:
   one `FxHashMap` probe in `shape.index`, then the vector index.
3. **Dynamic.** `Object`: one `FxHashMap` probe. `Value::Str` carrying JSON
   keeps the current parse-based path **during migration only** (see below).

Unknown field on a `Record` is a runtime error naming the shape
(`"User has no field 'emial'"`) — today's JSON path silently yields `Null`;
the conformance suite must pin the new, stricter behavior and the CHANGELOG
must call it out as a breaking change gated on v0.5.

### Producers and consumers to convert

| Site | Today | After |
|---|---|---|
| `engine.rs` row → value | row → JSON string | row → `Record` (shape = entity) |
| `new Entity()` | `"{}"` | `Record` with all-Null slots |
| `Expr::ObjectLit` | `Map::to_string()` | `Object` (or `Record` when it matches a declared class shape exactly — later optimization) |
| `json_parse(obj)` | JSON string passthrough | `Object` |
| `body()` / `cache_get` / middleware ctx | JSON strings | `Object` |
| `value_to_json_smart` string-sniffing | parse attempt per `Str` | only needed for `Str` until migration completes, then deleted |

## Migration plan (each step lands green on the conformance suite)

1. **Introduce `Object` + `Record` variants and the shape table**; keep all
   producers emitting JSON strings. Consumers (`FieldGet`/`FieldSet`,
   serializers, `==`, `as_string`) learn to handle the new variants. Pure
   additive, zero behavior change.
2. **Flip producers one at a time** (engine rows first — biggest win; then
   `ObjectLit`, `json_parse`, `body()`), running conformance + native
   parity after each. The `Value::Str` JSON fallback in consumers keeps
   mixed states working mid-migration.
3. **Wire compile-time slots** for typed params and select results
   (`FieldGetSlot` lowering).
4. **Delete the string paths**: `value_to_json_smart` sniffing, `FieldGet`
   re-parse arm. Exit criterion of Phase 1: "no internal JSON-string object
   representation left".
5. **Extract `jwc-runtime`**: one shape table + one dynamic value + one
   serializer consumed by interpreter and emitted AOT code.

## Invariants that must not move (conformance-gated)

- JSON output key order: sorted at serialisation time (AOT contract).
- `print(obj)` renders compact JSON, identical bytes to today.
- `raw_sql` first-column alphabetic semantics (depends on sorted keys).
- Equality: `Record == Record` compares shape + slots; `Record == Object`
  compares field-by-field (an entity fetched twice must equal itself
  regardless of representation tier).
- `Arc` (not `Rc`): axum tasks are `Send`.

## Performance expectation

`FieldGet` on a 20-field row goes from parse-whole-row-per-access
(O(row size), allocs) to a vector index (slot path) or one hash probe
(dynamic path). Phase 1 gate: interpreter hot select path regression ≤ 5%,
expected to be a large win instead; AOT `/json-large` is covered separately
by struct monomorphization (same shape table feeds the codegen).
