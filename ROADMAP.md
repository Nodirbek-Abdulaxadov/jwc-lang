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
| Phase 2 — Language Completeness | ✅ Done |
| Phase 3 — Developer Experience | ⏳ Partial (lint + did-you-mean + serve --watch done; LSP/fmt/pkg deferred) |
| Phase 4 — Real Compiler (Native) | ⏳ Partial (native AOT via Rust codegen + cargo, async runtime, compile-time column validation; LLVM IR + cross-target build deferred) |
| Phase 5 — Ecosystem | ⏳ Partial (Http + JWT + cache + email + WebSocket + queue stdlib done; wasm/hub/Redis-cache deferred) |
| Phase 6 — DX Polish (real-app feedback) | ✅ Done (literals/now/@var.field/!/raw strings/typed-field/error-handler) |
| Phase 7 — Standard helpers (strings/arrays/iteration/json) | ✅ Done |
| Phase 8 — Background jobs + LSP | ✅ Done (queue + jwc-lsp + WebSocket) |
| Phase 9 — Async runtime + perf ceiling | ✅ Done (async Vm + tokio-postgres + reqwest; native AOT also async) |

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
- Operatorlar: `like @p`, `ilike @p`, `in (@a, @b, ...)`, `between @a and @b`, `is null`, `is not null`.
- Aggregatsiyalar: `select count(*)`, `select sum|avg|min|max(Entity.col) from ...`.
- Projection: `select User { name, email } from ...` — entity field subset, kompayl-vaqt column existence check, `with rel` bilan birga ishlaydi.
- **Qoldi:** `group by` + `having` (projection bilan multi-col aggregate uchun), `join` (navigatsiya bilan qoplandi).

### 2.2b DB business-logic primitivlari ✅
- PK: `update var in ...` va `delete var from ...` entity'da belgilangan `pk` field(lar)ni hisobga oladi (composite PK qo'llab-quvvatlanadi). Ad-hoc table uchun `id` fallback.
- Dirty-field tracking: `var.field = X; update var in ...` faqat o'zgargan maydonlarni SET qiladi. O'zgarishsiz `update` aniq xato.
- Bulk delete: `delete from CTX.Table where ...;` — variable kerakmas, `where` majburiy (xavfsizlik).
- `transaction { ... }` bloki — `TxGuard` RAII (Drop'da `ROLLBACK`), thread-local connection. Worker thread reuse'da leak yo'q.
- `raw_sql(sql, params_json)` — parameterized escape hatch; SELECT'da text natija, mutationda affected rows count qaytaradi.

### 2.2c DB operatsion sifat ✅
- Pool to'liq env-sozlanadigan: `JWC_DB_POOL_SIZE`, `JWC_DB_MIN_IDLE`, `JWC_DB_MAX_LIFETIME_SECS` (default 30 min), `JWC_DB_IDLE_TIMEOUT_SECS` (default 10 min), `JWC_DB_CONNECTION_TIMEOUT_SECS` (default 5s). Stale connection muammosi yumshatildi.
- Migration session advisory lock (`pg_try_advisory_lock`) — bir vaqtning o'zida ikkita `migrate up/down` ishlasa "already in progress" xato, race yo'q.
- TLS Postgres: `JWC_DB_TLS=1` yoqadi (`postgres-native-tls`), `JWC_DB_TLS_INSECURE_SKIP_VERIFY=1` self-signed sert uchun. Pool va migration ulanishlari bir xil TLS bilan ishlaydi.
- Schema diff: `jwc migrate new` endi oldingi `.up.sql` faylni parse qilib joriy entity'lar bilan diff hisoblaydi va faqat ALTER (yoki CREATE TABLE yangi entity uchun) chiqaradi. Diff bo'lmasa "-- no schema changes".
- Integration tests: `tests/integration_db.rs` — testcontainers-rs orqali Docker Postgres'da 6 ta scenario (basic query, tx commit, tx rollback on drop, migrate up/down, advisory lock concurrency, full CRUD). Docker yo'q hostda graceful skip.

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

### 2.5 Entity relationships ✅
- `field uuid references EntityName.column [on delete cascade|restrict|set null]` — FK CONSTRAINT.
- Navigation property: `posts: List<Post> via Post.user_id;` (one-to-many) / `profile: Profile via Profile.user_id;` (one-to-one) — entity body ichida.
- `select User with posts, profile from AppDb.User ...` — correlated `json_agg` subquery, kompayl-vaqt nav nomini tekshirish.
- Validator: target entity + FK column + nav nomi mavjudligi tekshiriladi.

### 2.6 async/await ✅
- ✅ Lexer: `async`, `await` keywordlar.
- ✅ AST: `FunctionDecl.is_async` flag va `Expr::Await(Box<Expr>)`.
- ✅ HTTP server axum + tokio; har request `tokio::spawn` (Phase 9 da
  `spawn_blocking` olib tashlandi).
- ✅ WebSocket: `route WS "..."` + `ws_send`/`ws_recv`/`ws_close`; frame
  I/O endi `tokio::io::{AsyncReadExt, AsyncWriteExt}` orqali (Phase 9).
- ✅ Vm o'zini async (recursive `#[async_recursion]`), DB layer
  `tokio-postgres` + `deadpool-postgres`, `await` real yield qiladi.
  Tafsilot — Phase 9 (Async runtime).

---

## Phase 3 — Developer Experience `~6-12 oy`

**Maqsad:** JWC bilan yozish — Node yoki Go bilan yozishdek tezroq bo‘lsin.

### 3.1 Real LSP ✅ basic
- `src/bin/jwc_lsp.rs` — stdio orqali ishlaydigan tower-lsp asoslangan server.
- Diagnostics: parse xato (regex bilan `at line N, col M` ushlanadi), validate xato (file boshi), lint warning (W001/W002).
- Hover: cursor pozitsiyadagi identifier'ga qarab `entity / class / function` haqida ma'lumot — fields soni, context, params, return type, `async` prefiksi.
- Document sync: `TextDocumentSyncKind::FULL` (har edit'da to'liq matn).
- **Qoldi:** go-to-definition, autocomplete, route hover, semantic tokens.

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

> `jwc build` (alias `bundle`) hali ham embedded-launcher rejimi. Real
> native AOT — `jwc build --native`: `src/native_build.rs` (2062 qator)
> AST'dan Rust source generatsiya qiladi, `cargo --release` orqali tokio
> + `tokio-postgres` + `reqwest` bilan stripped binary chiqaradi (Phase
> 9 da to'liq async). LLVM IR yo'li hozircha tushirilmagan — kelajakda
> faster cold start uchun.

### 4.1 IR ⬜ deferred
- AST → JWC IR (linear three-address code).
- Dead code elimination, constant folding.

### 4.2 Native codegen ⏳ partial (Rust path)
- ✅ Rust codegen yo'li: `native_build.rs` AST'ni Rust source'ga
  tushiradi, `cargo --release` build qiladi. Route / user-fn /
  middleware / errorHandler `Pin<Box<dyn Future<Output=V> + Send>>`
  qaytaradi; `#[tokio::main(flavor = multi_thread)]` runtime.
- ✅ `hellocompile` misoli — 1.1 MB stripped release binary
  (`async_demo` reqwest + rustls bilan ~2.9 MB).
- ⬜ LLVM IR path — JWC IR → LLVM IR → native binary.
- ⬜ Cross-target: `jwc build --target linux-x64 --release`,
  `aarch64-darwin`, `x86_64-windows`.

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
  - ✅ Http client: `http_get(url)`, `http_post(url[, body[, headers]])`,
    `fetch_json(url)` — `reqwest` + `rustls` orqali, async, JSON envelope
    qaytaradi (`fetch_json` to'g'ridan-to'g'ri decoded value).
  - ✅ Async helpers: `sleep_ms(ms)` — tokio scheduler'ga yield qiladi.
  - ✅ Auth: `jwt_sign(payload_json, secret)` / `jwt_verify(token, secret)` — HS256 (hmac + sha2 + base64).
  - ✅ Password hashing: `hash_password(pwd)` / `verify_password(pwd, hash)` — Argon2id (argon2 crate).
  - ✅ Email (SMTP): `send_email(to, subject, body_html)` — `lettre` + `rustls`.
  - ✅ Cache (in-memory, TTL): `cache_set/get/del/clear`.
  - ✅ WebSocket: `route WS "..."`, `ws_send`/`ws_recv`/`ws_close` —
    Phase 9 da to'liq async frame I/O.
  - ✅ Background queue: `register_job_handler` / `enqueue` / `job_count`
    (Phase 8).
  - ⬜ Qoldi: Redis-backed cache, Storage (S3), SSE.
- **WebAssembly target:** `jwc build --target wasm` — edge runtime’da ishlatish.
- **JWC Hub:** `hub.jwc.dev` — paket registry.
- **Self-hosting:** JWC kompilatori JWC tilida qayta yozilishi.

---

## Phase 6 — DX Polish `now` (microblog feedback'idan)

**Maqsad:** Real backend yozish jarayonida til "DSL'dan" "kundalik tilga" aylanishi uchun aniqlangan to'siqlarni tozalash.

### 6.1 JSON object literal ✅
- Hozir: `return "{\"items\":" + items + ",\"total\":" + total + "}";` — manual concat + escape.
- Maqsad: `return { items: items, total: total };` — birinchi-sinf object literali.
- AST: `Expr::ObjectLit(Vec<(String, Expr)>)`. Runtime JSON string'ga serializatsiya.

### 6.2 `now()` built-in ✅
- Hozir: hardcoded `"2026-05-19T..."` string. Schema-side default'lar yo'q.
- Maqsad: `now()` → ISO 8601 UTC string (chrono dep). Keyingi iteratsiyada `entity` ichida `created_at datetime default now;`.

### 6.3 `@var.field` shortcut ✅
- Hozir: `where ... == @req.username` → parse xato. Workaround: `let v = req.username; ... == @v;`.
- Maqsad: `@ident.field` doim FieldGet sifatida parse qilinsin.

### 6.4 `!expr` unary negation ✅
- Hozir: `if (ok == false)` yozish kerak.
- Maqsad: `if (!ok)` ham ishlasin. Lexer'da `!` simvol allaqachon bor (`!=` uchun); `parse_unary_expr`da boshqaruv qo'shiladi.

### 6.5 Raw string literal `r"..."` ✅
- Regex pattern'larida `\\.` ikki marta escape kerak. `r"^[^@]+@[^@]+\.[^@]+$"` toza.

### 6.6 Typed param field check (compile-time) ✅
- `function takes(req: ReqClass)` ichida `req.field` kompayl-vaqt `ReqClass` schema'ga tekshiriladi (`Type error: field 'X' is not declared on ReqClass`).
- `T?` va `Optional<T>` ham tekshiriladi; `List<T>` — skip (var'ning o'zi list, field access mantiqsiz).
- Local variables (Let/Assign bilan binding) hozircha untyped — kelajakda `let x = body()` typed handler argumenti orqali inferansiya qilinishi mumkin.

### 6.7 Global error handler ✅
- Top-level `errorHandler (e) { ... }` deklaratsiyasi. Bitta handler programma per. Faqat uncaught route errorlari ushlanadi (response oqim yoki middleware short-circuit emas).
- `e` JSON sifatida bound: `{ "message": "...", "causes": [...] }`.
- Handler `return` qiymati response body bo'ladi; status JSON ichidagi `"status"`'dan olinadi (default 200, lekin `internalError(...)` 500 beradi).

## Phase 8 — Background jobs + LSP ✅

### 8.1 In-process job queue
- Yangi modul `src/queue.rs` + 3 built-in: `register_job_handler(name, fn_name)`,
  `enqueue(name, payload_json)`, `job_count()`.
- `Mutex<VecDeque<Job>>` + `Condvar` orqali worker thread'lar polls qiladi.
  Default 2 worker, `JWC_QUEUE_WORKERS` env bilan sozlanadi.
- Server `serve(port)` chaqiruvi paytida `queue::init_queue(Arc::clone(&program))`
  yoqiladi; worker'lar process lifetime davomida tirik.
- Validator: `register_job_handler` ikkinchi argi haqiqiy function nomi ekanligini
  kompayl-vaqt tekshiradi.
- **Qoldi:** retry policy, persistent backing (Redis/PG), priority queues.

### 8.2 LSP server (basic)
- `cargo build --bin jwc-lsp` — stdio orqali tower-lsp ishlaydi.
- Diagnostics + hover ishlaydi (yuqorida 3.1'ga qarang).
- **Qoldi:** go-to-definition, autocomplete, route/middleware hover, semantic tokens.

## Phase 9 — Async runtime + perf ceiling ✅ done

**Maqsad:** Hozirgi ~20k RPS shiftini Rust async stack darajasida (~50–100k RPS) ko'tarish.

### Joriy baseline (v0.1.2, localhost + Postgres, bombardier)

| Test | Conn | RPS | p50 | p95 | p99 |
|---|---|---|---|---|---|
| GET /notes | 100 | **21,512** | 3.0ms | 7.4ms | 14.1ms |
| GET /notes | 200 | 20,146 | 7.0ms | 17.6ms | 39.2ms |
| POST /notes | 100 | 16,930 | 4.7ms | 8.6ms | 25.2ms |

Bottleneck — sync tree-walking `Vm` + `r2d2_postgres` (blocking driver).
Har request `spawn_blocking` orqali tokio'dan blocking pool'ga ko'chiriladi
→ async I/O bekor bo'ladi, har handler bitta thread band qiladi.

### 9.1 Async Vm ✅
- `runner.rs` Vm to'liq async — `eval_expr` / `exec_block` / `call_function`
  `#[async_recursion]` orqali, har joyda `.await`.
- `Flow::{Continue, Return, Break, ContinueLoop}` o'zgarmadi.
- Tree-walking saqlandi; har step await-able.

### 9.2 `tokio-postgres` ✅
- `engine.rs` `r2d2_postgres` → `deadpool-postgres` + `tokio-postgres`'ga
  ko'chdi. `engine::checkout()` async; `TxGuard` async-aware.
- TLS pathlar `tokio-postgres-rustls` bilan ishlaydi.
- Bind layer `Box<dyn ToSql + Sync + Send>` orqali — uuid / datetime real
  Postgres typelariga bind qilinadi.

### 9.3 `spawn_blocking` ni olib tashlash ✅
- `server.rs` har request uchun `tokio::spawn` ishlatadi — `spawn_blocking`
  butunlay yo'q.
- WebSocket bridge'ga ehtiyoj qolmadi — frame I/O `tokio::io::{AsyncReadExt,
  AsyncWriteExt}` orqali, `WS_STREAM` `tokio::task_local!` Arc<Mutex<TcpStream>>.

### 9.4 Native AOT ham async ✅
- `native_build.rs` route/user-fn/middleware/errorHandler uchun
  `Pin<Box<dyn Future<Output=V> + Send>>` chiqaradi; `#[tokio::main(flavor =
  multi_thread)]` runtime; `jwc_serve_impl` async; try/catch +
  transaction async block ustida `futures::FutureExt::catch_unwind` orqali.
- `native_prelude*.rs.in` — `tokio::net::TcpListener`, `tokio::spawn`,
  task-local request context, async WS prelude, deadpool-postgres prelude,
  `sleep_ms` / `http_get` / `fetch_json` async helperlar.
- Workspace Cargo.toml: sync `r2d2*`/`postgres`/`ureq` o'chirildi; `tokio
  (full)`, `tokio-postgres`, `deadpool-postgres`, `async-recursion`,
  `reqwest (rustls)` qo'shildi.

### 9.5 Maqsadli raqamlar (verify pending)
- GET (oddiy SELECT) c=100: **40–60k RPS**, p99 < 10ms.
- POST: **30–50k RPS**.
- c=500+ Linux'da real ravishda yelka tortishi mumkin (Windows loopback alohida holat).
- Benchmark setup: `examples/bench.sh` (bombardier), `examples/bench.py`
  (Python harness), `examples/jmeter/` (JMeter plan), `examples/bench-cs/`
  (.NET baseline solishtirma uchun). Real natijalar — keyingi iteratsiyada
  qo'shiladi.

---

## Phase 7 — Standard helpers ✅

- `length(x)` — char count for strings, element count for JSON arrays,
  key count for JSON objects, 0 for null.
- String ops: `lower`, `upper`, `trim`, `replace`, `contains`, `starts_with`,
  `ends_with`, `split` (returns JSON array string).
- Array ops: `first(xs)`, `last(xs)`, plus `contains` and `length`.
- `for VAR in EXPR { ... }` — iterate a JSON array. `break` / `continue` /
  `return` all work. `EXPR` is evaluated once, items round-trip through
  `json_to_value`.
- `json_parse(s)` / `json_stringify(v)` — explicit conversion between
  JSON-string carriers and structured Value shapes.
- `in` is now a real keyword (reserved by `where ... in (...)` and `for ... in ...`).

## Priority Timeline

Phase 0–2, 6–9 tugallandi. Keyingi ish ustuvorligi:

```
hozir    →  Phase 9.5 — real benchmark natijalari (bench-cs/bench.py/jmeter)
1-2 oy   →  Phase 3.1+ — LSP go-to-definition, autocomplete, semantic tokens
2-4 oy   →  Phase 3.3 — `jwc fmt` (AST → source, comment preservation)
4-8 oy   →  Phase 4.2 — native codegen kengaytmasi (cross-target, LLVM IR)
8-12 oy  →  Phase 3.4 — package sistemasi (`jwcproj.json::dependencies`)
12+ oy   →  Phase 5 — wasm target, hub.jwc.dev, Redis-backed cache, S3, SSE
```

---

## Ultimate Goal

> Web backend yozish → config yozish darajasida oson.
> Performance → Rust/Go darajasida.

---

## Kod xaritasi (orientir uchun)

```
src/
  lexer.rs           422  tokenizer + template string + raw strings + comment skip
  ast.rs             343  Program / Model / Route / Function / Stmt / Expr
  parser.rs         3691  recursive-descent + validate_program (column checks)
  runner.rs         5077  async tree-walking Vm + HTTP dispatch (async_recursion)
  engine.rs          528  deadpool-postgres + tokio-postgres + prep cache + TTL cache
  server.rs          380  axum + tokio::spawn + WebSocket + metrics
  sql.rs             315  Postgres DDL generator
  migrate.rs         443  migrate new / up / down (advisory lock)
  project.rs         356  jwcproj parser + dotenv loader + source walker
  diag.rs             31  byte offset → (line, col)
  lint.rs            257  unused fn (W001) / unused middleware (W002)
  queue.rs           380  in-process job queue + worker pool
  native_build.rs   2062  AST → Rust source (async tokio AOT path)
  main.rs            513  CLI subcommands + embedded launcher
```

Jami: ~14.8k qator Rust.
