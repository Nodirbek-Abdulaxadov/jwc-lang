# JWC Language — Roadmap

## Progress

| Phase | Status |
|-------|--------|
| Phase 1.1 — Real HTTP Server | ✅ Done |
| Phase 1.2 — Generic DB Layer | ✅ Done |
| Phase 1.3 — Type System (basic) | ✅ Done |
| Phase 1.4 — `validate body` syntax | ⬜ Next |
| Phase 2 — Language Completeness | ⬜ |
| Phase 3 — Developer Experience | ⬜ |
| Phase 4 — Native Compiler | ⬜ |
| Phase 5 — Ecosystem | ⬜ |

---

## Phase 1 — Solid Core (MVP completion) `now`

**Goal:** Make the current interpreter production-ready

### 1.1 Real HTTP Server ✅
- `server.rs` — TCP listener via `tiny_http`
- `jwc serve` command — starts `http://0.0.0.0:8080`
- Request/Response cycle: method, path, headers, body
- `JWC_REQUEST_BODY` env var hack removed

### 1.2 Generic DB Layer ✅
- Removed hardcoded `db_insert_todo`, `db_select_todo` etc.
- Removed `normalize_webapi_compat()` hack from parser
- Native AST nodes: `DbSelect`, `DbInsert`, `DbUpdate`, `DbDelete`
- `new Entity()`, `var.field`, `var.field = value` parsing & execution
- `select Entity from db.Table where Entity.field == @id first`
- `insert var into db.Table` → `INSERT INTO ... RETURNING *`
- `update var in db.Table` → `UPDATE ... SET ... WHERE id = ... RETURNING *`
- `delete var from db.Table` → `DELETE FROM ... WHERE id = ...`
- Auto SQL generation with `build_insert_sql()`, `build_update_sql()`

### 1.3 Type System (basic) ✅
- `function createUser(name: string, age: int)` — typed params parsed
- `TypedParam { name, ty }` struct in AST
- Runtime type mismatch error with clear message: `Type error: parameter 'x' expects int, got bool`
- Return type annotation: `function getUser(id: int): User`
- Untyped params backward-compatible: `function foo(x)` still works
- Type coercion: `string → int` (if parseable), `int → string`

### 1.4 `validate body` syntax ⬜
- Parser: `validate body { field: rule, ...; }` block
- Rules: `required`, `minLength(n)`, `maxLength(n)`, `min(n)`, `max(n)`, `pattern("regex")`
- Validation runs before route handler body executes
- Returns `400 Bad Request` with field errors as JSON on failure
- Compile-time: field names checked against entity schema

---

## Phase 2 — Language Completeness `3–6 months`

**Goal:** Make the language fully expressive

### 2.1 Type System (full) ⬜
- Built-in types: `string`, `int`, `float`, `bool`, `uuid`, `datetime`, `json`
- `List<T>`, `Optional<T>`
- Return type annotation: `function getUser(id: int): User`
- Compile-time type checking across assignments and calls

### 2.2 SQL native syntax (full parser) ⬜
```jwc
let users = select User from db.Users
    where User.age > 18
    orderby age desc
    limit 10
    offset 0;
```
- `orderby`, `limit`, `offset`, `left join` — AST nodes
- Compile-time: table and column existence checked
- SQL injection prevention — parameterized queries

### 2.3 `async/await` ⬜
- `async function` declaration
- `await expr` expression
- Non-blocking DB calls
- `tokio` runtime integration

### 2.4 Error handling ⬜
```jwc
try {
    insert car into db.Cars;
} catch (e: DbError) {
    return internalError(e.message);
}
```
- `try { } catch (e: ErrorType) { }` blocks
- `throws ErrorType` on function signature
- Built-in error types: `DbError`, `NotFoundError`, `ValidationError`

### 2.5 Middleware ⬜
```jwc
middleware AuthMiddleware {
    let token = headers.authorization;
    if (token == null) { return unauthorized(); }
}

route GET "api/users" use AuthMiddleware {
    ...
}
```

### 2.6 Entity relations ⬜
- `one_to_many`, `many_to_one`, `many_to_many`
- Eager/lazy loading syntax
- Cascade delete/update
- Auto JOIN generation from relation declarations

---

## Phase 3 — Developer Experience `6–12 months`

**Goal:** Make developers productive and happy

### 3.1 VS Code Extension ⬜
- Syntax highlighting for `.jwc` files
- Intellisense — entity field autocomplete in routes/services
- Route hover — shows HTTP method + full path
- Inline error diagnostics from `diag.rs`
- `Go to Definition` — jump to entity or function
- Snippet support: `route`, `entity`, `function`

### 3.2 Better compiler diagnostics ⬜
- Precise error location: `error[E001] at main.jwc:14:5`
- Suggestions: `did you mean 'getAllColors'?`
- Warning system: unused variable, unreachable route, missing `first` on single-item select
- `jwc lint` command

### 3.3 CLI improvements ⬜
```bash
jwc new myapp          # ✅ done
jwc run                # ✅ done (interpreter mode)
jwc serve              # ✅ done (HTTP server)
jwc build --release    # ✅ done (launcher script)
jwc migrate new init   # ✅ done
jwc migrate up         # ✅ done
jwc lint               # ⬜ not yet
jwc fmt                # ⬜ code formatter
jwc add <package>      # ⬜ package manager
jwc serve --watch      # ⬜ hot reload
```

### 3.4 Package system ⬜
- `jwcproj.json` `dependencies` field activated
- `jwc add postgres-utils` installs packages
- `jwc hub` — browse registry

### 3.5 Hot reload ⬜
- `jwc serve --watch` — restarts on `.jwc` file change
- Fast incremental re-parse

---

## Phase 4 — Compiler (Native binary) `12–24 months`

**Goal:** Replace interpreter with compiled native binary

### 4.1 IR (Intermediate Representation) ⬜
- AST → JWC IR (linear instruction set)
- Dead code elimination
- Constant folding

### 4.2 LLVM backend ⬜
- JWC IR → LLVM IR → native binary
- Targets: Linux (`x86_64`), macOS (`arm64`), Windows (`x86_64`)
- `jwc build --target linux-x64`

### 4.3 Compile-time SQL validation (full) ⬜
- Read DB schema at compile time
- Verify all `select`, `insert`, `update`, `delete` statements
- Detect table/column mismatches before deploy
- Migration drift detection

### 4.4 Zero-cost abstractions ⬜
- Route handler → inlined native code
- Entity field access → direct memory offset
- No heap allocation for simple operations

---

## Phase 5 — Ecosystem `24+ months`

**Goal:** Make JWC a global backend language

### 5.1 Standard library ⬜
- `Http` — client requests
- `Auth` — JWT, OAuth2 built-in
- `Cache` — Redis/Memcached abstraction
- `Queue` — job queue (BullMQ-like)
- `Email`, `Storage`, `Websocket`

### 5.2 WebAssembly target ⬜
- `jwc build --target wasm`
- Run JWC backend logic in browser or edge runtime

### 5.3 JWC Hub (package registry) ⬜
- `hub.jwc.dev` — central package repository
- `jwcproj.json` versioned dependencies
- Community packages

### 5.4 Self-hosting ⬜
- JWC compiler rewritten in JWC itself
- Bootstrapping milestone

---

## Priority Timeline

```
Now         →  Phase 1.3 (Type system, basic)
             + Phase 1.4 (validate body)
1–2 months  →  Phase 2.1–2.2 (Full types + SQL)
3–6 months  →  Phase 2.3–2.6 (async, errors, middleware, relations)
6–12 months →  Phase 3 (VS Code ext, CLI, hot reload)
12–24 months→  Phase 4 (Native compiler, LLVM)
24+ months  →  Phase 5 (Ecosystem, self-hosting)
```

---

## Ultimate Goal

> Web backend yozish → config yozish darajasida oson.
> Performance → Rust/Go darajasida.
