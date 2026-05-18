# JWC Language — Roadmap

> Bu hujjat hozirgi kod holatining halol tahlili asosida tuzilgan.
> "Done" deb belgilangan band — manba kodda to'liq amalga oshirilgan demakdir.
> "Partial" — qisman ishlaydi, lekin yashirin hack yoki cheklov bor.

---

## Progress Snapshot

| Phase | Status |
|-------|--------|
| Phase 0 — Texnik qarz (legacy hack’larni tozalash) | ✅ Done |
| Phase 1 — MVP Core | ✅ Done |
| Phase 2 — Language Completeness | ✅ Done (auto-JOIN + real async deferred) |
| Phase 3 — Developer Experience | ⏳ Partial (lint + did-you-mean + serve --watch done; LSP/fmt/pkg deferred) |
| Phase 4 — Real Compiler (Native) | ⏳ Partial (compile-time column validation done; IR/LLVM deferred) |
| Phase 5 — Ecosystem | ⏳ Partial (Http + JWT stdlib done; cache/queue/wasm/hub deferred) |

---

## Phase 0 — Texnik qarz ✅ done

**Maqsad:** Real Phase 2 ga o‘tishdan oldin "Done" deb belgilangan, lekin asli pala-partish qolgan joylarni tozalash.

### 0.1 Legacy WebAPI normalizer’ni olib tashlash ✅
- `parser.rs::normalize_webapi_compat()` butunlay olib tashlandi.
- `runner.rs` — hardcoded `db_*_todo` built-in funksiyalari va dispatch shoxlari o‘chirildi (≈ 224 qator − ).
- Native AST nodelari yagona DB API bo‘ldi.

### 0.2 Drivers majmuasini halollashtirish ✅
- Validator endi aniq xabar beradi: *"Postgres is currently the only supported dbcontext driver. Multi-driver support is planned for Phase 2."*
- README’da `## Supported Drivers` bo‘limi qo‘shildi.

### 0.3 Migrations’ni to‘ldirish ✅ (rollback qismi)
- `jwc migrate down --steps N` qo‘shildi (`main.rs`, `migrate.rs::rollback_migrations`).
- Har migration alohida transaksiyada `<base>.down.sql` ni ishga tushiradi va `_jwc_migrations` jadvalidan o‘chiradi.
- Boshqa qoldi (Phase 2 da hal qilinadi): schema diff generator.

### 0.4 Build komanda’ning haqiqiy ma’nosi ✅
- `jwc build` (alias: `jwc bundle`) — output xabari aniq: *"Bundled runtime + launcher"*.
- README va `--help` matnida AOT kompilator hali yo‘qligi va Phase 4 ga rejalashtirilgani belgilab qo‘yildi.

---

## Phase 1 — MVP Core `current`

**Maqsad:** Interpreter rejimida API + Postgres CRUD’ni yakuniy darajaga keltirish.

### 1.1 HTTP Server ✅
- `server.rs` — `tiny_http` + worker pool (`JWC_SERVER_WORKERS`), bounded `sync_channel` queue, optional metrics.
- `serve()` til ichidan chaqiriladi: `main()` → `serve(8080)` → CLI orqali `server::serve` ishga tushadi.
- Per-request body 4xx/5xx error chain to‘liq log qilinadi.

### 1.2 DB Runtime Layer ✅
- `engine.rs` — `r2d2_postgres` pool, query-shape SQL cache, optional result cache (`JWC_QUERY_CACHE_TTL_SECS`).
- Native AST nodelari mavjud: `DbSelect / DbInsert / DbUpdate / DbDelete`, `new Entity()`, `var.field`, `var.field = value`.
- Phase 0.1 legacy hack’lar tozalandi.

### 1.3 Type System (very basic) ⏳ partial
- Hozir tan olinadigan primitive’lar runtime’da: `string`, `int`, `double`, `bool`.
- Avtomatik koersiya: `int → string`, `string → int` (parse bo‘lsa), `int ↔ double`. Bu noaniqlik manbai — `decimal/uuid/datetime/json` typelar runtime’da string sifatida o‘tadi.
- Typed param + return + model JSON validatsiyasi mavjud (`runner.rs::check_typed_value`).
- **Qoldi:** `uuid`, `datetime`, `decimal`, `json`, `bigint` — birinchi sinf runtime typelar bo‘lishi kerak.

### 1.4 Query string + path params ✅
- `query_param(name)` va `query_param(name, default)` built-inlari `runner.rs` ga qo‘shildi.
- `server.rs` to‘liq URL’ni runner’ga uzatadi; route matching `?...` ni avtomatik ajratadi.
- 3 ta yangi test: query value, default fallback, query bilan route matching buzilmaydi.

### 1.5 `validate body` bloki ✅
- Yangi keyword `validate` (lexer), `Stmt::ValidateBody { fields }` (AST), parser bloki.
- Qoidalar: `required`, `minLength(n)`, `maxLength(n)`, `min(n)`, `max(n)`, `pattern("regex")`.
- Xatolik bo‘lsa, route 400 status va `{"errors": {"field": "rule"}}` javob qaytaradi.
- Regex `regex` crate orqali compile-time va runtime'da ishlatiladi.

### 1.6 Route handler signaturalari ✅
- `route GET "users/{id}" -> getUser;` endi `getUser` ning typed params’ini path/query orqali avtomatik to‘ldiradi (`Vm::build_handler_args`).
- Argument string sifatida o‘tadi, mavjud `check_param_type` orqali declared typeg’a coerce qilinadi (`id: int` → `42` int).

---

## Phase 2 — Language Completeness ✅ done

**Maqsad:** Tilning ifoda kuchini real productionga yetkazish.

### 2.1 Full type system ✅
- Birinchi-sinf primitive’lar: `string`, `int`, `bigint`, `double`, `decimal`, `bool`, `uuid`, `datetime`, `json`.
- `T?` va `Optional<T>` — null qabul qilish; `List<T>` — JSON array + element check.
- Runtime check `runner.rs::check_typed_value` + JSON-level `json_value_matches_type`.
- **Qoldi (kichik):** Kompayl-vaqt type checking (assignment/call args/return) — hozir runtime. `byte[]` turi, explicit koersiyalar — keyingi iterazda.

### 2.2 SQL syntax kengaytmasi ✅
- `orderby <field> [asc|desc]`, `limit N`, `offset N` AST + parser + runner.
- `@param` referensi `limit`/`offset` ichida.
- Compound `where` — `and`/`or` + qavslar, `and` `or`dan yuqori precedence.
- Operatorlar: `like @p`, `in (@a, @b, ...)`.
- `select count(*) from CTX.Table [where ...]` → `int`.
- **Qoldi:** `join`, `between`.

### 2.3 `try / catch` ✅
- Sintaksis: `try { ... } catch (e[: ErrorType]) { ... }`.
- Catch var bound: `{"message": "...", "causes": [...]}` JSON sifatida.
- `catch_type` AST'da saqlanadi, hozircha barcha xatolarni ushlaydi (typed dispatch — Phase 3 da).

### 2.4 Middleware ✅
- Top-level `middleware Name { ... }` deklaratsiyasi.
- `route GET "..." use M1, M2 { body }` yoki `... use M -> handler;`.
- Built-in: `header(name)`, `context(key)`, `setContext(key, value)`, `unauthorized()`, `forbidden()`.
- Middleware qaytaradigan qiymat butun routeni qisqartiradi (short-circuit).
- Server'dan inbound headers `runner::run_request_with_headers` orqali keladi.

### 2.5 Entity relationships ⏳ partial
- `field uuid references EntityName.column [on delete cascade|restrict|set null]` qabul qilinadi.
- SQL generator FK CONSTRAINT chiqaradi va validator target entity+column mavjudligini tekshiradi.
- **Qoldi:** Navigation property (`posts: List<Post> via Post.user_id`) va `select User with posts ...` auto-JOIN — keyingi iterazga.

### 2.6 async/await ⏳ syntax-only
- Lexer: `async`, `await` keywordlar.
- AST: `FunctionDecl.is_async` flag va `Expr::Await(Box<Expr>)`.
- Hozircha interpreter sync ishlaydi — `await expr` shunchaki ichki ifodani qaytaradi.
- **Qoldi:** Tokio runtime + `tokio-postgres` + `hyper`/`axum` server. Bu eng katta keyingi ish.

---

## Phase 3 — Developer Experience `~6-12 oy`

**Maqsad:** JWC bilan yozish — Node yoki Go bilan yozishdek tezroq bo‘lsin.

### 3.1 Real LSP ⬜ deferred
- `vscode-extension/` papka mavjud, faqat `language-configuration.json` + snippet.
- Maqsad: alohida `jwc-lsp` binary (LSP protocol) — entity field autocomplete, route hover, go-to-definition, diagnostics push.
- Hozircha bo‘shliq sezilarli ish bo‘lgani uchun keyingi katta ish.

### 3.2 Compiler diagnostics ✅ qisman
- ✅ "Did you mean?" suggestion — `runner.rs::closest_match` Levenshtein based, unknown function / undefined variable xabariga qo‘shiladi.
- ✅ Aniq `at line X, col Y` xabari `diag::SourceMap` orqali yozib qo‘yilgan.
- ⬜ Qoldi: `error[E001]` numbered diagnostic codelar tizimi.
- ⬜ Qoldi: unreachable route, missing `first` on single-row select kabi semantik warninglar.

### 3.3 CLI ⏳ qisman
- ✅ `jwc lint` — `lint.rs::lint_program`: unused function (W001) va unused middleware (W002).
- ✅ `jwc migrate down` (Phase 0.3 dan).
- ✅ `jwc serve --watch` — `notify` crate file watcher; parent process child `jwc serve` ni kuzatadi va `.jwc` o‘zgarsa qayta ishga tushiradi.
- ⬜ `jwc fmt` — formatlash. AST → source qayta chiqaruvi. Comment preservation muammosi.
- ⬜ `jwc add <pkg>` — paket qo‘shish (3.4 ga bog‘liq).

### 3.4 Package sistemasi ⬜ deferred
- `jwcproj.json::dependencies` real ishlatiladi.
- Local cache (`~/.jwc/registry/`) → kelajakda `hub.jwc.dev`.
- Versioning: semver, `^1.2.3`.

---

## Phase 4 — Real native compiler `~12-24 oy`

**Maqsad:** Interpreter’ni siqib chiqarish, real native binary chiqarish.

> Hozirgi `jwc build` — runtime CLI’ni `bin/{profile}/{name}.exe` ga **copy** qiladi. Bu Phase 4 emas, embedded launcher.

### 4.1 IR ⬜ deferred
- AST → JWC IR (linear three-address code).
- Dead code elimination, constant folding.

### 4.2 LLVM backend ⬜ deferred
- JWC IR → LLVM IR → native binary.
- Targets: `x86_64-linux`, `aarch64-darwin`, `x86_64-windows`.
- `jwc build --target linux-x64 --release`.

### 4.3 Kompayl-vaqt SQL validation ⏳ partial
- ✅ Static check: `select Entity from CTX.Table` da `where Entity.col`/`orderby Entity.col` — `col` entitining haqiqiy maydoni ekanligi `parser::validate_program` ichida tekshiriladi.
- ✅ Misspelled columns serverni ishga tushirmasdan bail qiladi.
- ⬜ Qoldi: live DB schema snapshot (`information_schema` o‘qish) + migration drift detector.
- ⬜ Qoldi: `insert`/`update`/`delete` payload field-name moslik tekshiruvi.

### 4.4 Zero-cost abstractions ⬜ deferred
- Entity field access → struct field offset.
- Route handler → inlined function, virtual dispatch yo‘q.
- LLVM backend (4.2) tugamasidan oldin ma’nosi yo‘q.

---

## Phase 5 — Ecosystem `~24+ oy`

**Maqsad:** JWC ni global backend tiliga aylantirish.

- **Standard library ⏳ partial:**
  - ✅ Http client: `http_get(url[, headers])`, `http_post(url[, body[, headers]])` — `ureq` orqali, JSON envelope qaytaradi.
  - ✅ Auth: `jwt_sign(payload_json, secret)` / `jwt_verify(token, secret)` — HS256 (hmac + sha2 + base64).
  - ✅ Password hashing: `hash_password(pwd)` / `verify_password(pwd, hash)` — Argon2id (argon2 crate).
  - ⬜ Qoldi: Cache (Redis), Queue (BullMQ-like), Email, Storage, Websocket.
- **WebAssembly target:** `jwc build --target wasm` — edge runtime’da ishlatish.
- **JWC Hub:** `hub.jwc.dev` — paket registry.
- **Self-hosting:** JWC kompilatori JWC tilida qayta yozilishi.

---

## Priority Timeline

```
hozir   →  Phase 0 (legacy hack’lar tozalanadi)
1-2 oy  →  Phase 1.4 / 1.5 / 1.6 (query, validate, typed handlers)
3-6 oy  →  Phase 2.1 / 2.2 / 2.3 (types, SQL, try/catch)
6-12 oy →  Phase 2.4-2.6 + Phase 3 (middleware, relations, LSP, package)
12-24 oy→  Phase 4 (IR + LLVM + native + compile-time SQL)
24+ oy  →  Phase 5 (stdlib, wasm, hub, self-hosting)
```

---

## Ultimate Goal

> Web backend yozish → config yozish darajasida oson.
> Performance → Rust/Go darajasida.

---

## Kod xaritasi (orientir uchun)

```
src/
  lexer.rs    317  tokenizer + template string + comment skip
  ast.rs      135  Program / Model / Route / Function / Stmt / Expr
  parser.rs  1541  full parser + program validator
  runner.rs  1621  tree-walking interpreter + HTTP request dispatch
  engine.rs   178  Postgres pool + SQL cache + result TTL cache
  server.rs   210  tiny_http worker pool + metrics
  sql.rs      199  Postgres DDL generator
  migrate.rs  192  migrate add / up (down hali yo‘q)
  project.rs  236  jwcproj parser + dotenv loader + source walker
  diag.rs      27  byte offset → (line, col)
  main.rs     345  CLI subcommands + embedded launcher
```
