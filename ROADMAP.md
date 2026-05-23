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
| Phase 10 — Observability + streaming + SSE | ⏳ Partial (10.5 typed-catch dispatch ✅ v1; tracing/OTel, stream `select`, `route SSE`, native cross-target qoldi) |

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

## Phase 1 — MVP Core ✅ done

**Maqsad:** Interpreter rejimida API + Postgres CRUD’ni yakuniy darajaga keltirish.

> v0.1.x tarixiy holat. 1.1 dagi `tiny_http` va 1.2 dagi `r2d2_postgres` —
> Phase 9 da axum + tokio + `deadpool-postgres`'ga ko'chirilgan.

### 1.1 HTTP Server ✅ → Phase 9 da yangilangan
- `server.rs` (v0.1.x) — `tiny_http` + worker pool (`JWC_SERVER_WORKERS`), bounded `sync_channel` queue, optional metrics.
- `serve()` til ichidan chaqiriladi: `main()` → `serve(8080)` → CLI orqali `server::serve` ishga tushadi.
- Per-request body 4xx/5xx error chain to‘liq log qilinadi.
- **Joriy holat:** axum + tokio, har request `tokio::spawn`. Phase 9.3'ga qarang.

### 1.2 DB Runtime Layer ✅ → Phase 9 da yangilangan
- `engine.rs` (v0.1.x) — `r2d2_postgres` pool, query-shape SQL cache, optional result cache (`JWC_QUERY_CACHE_TTL_SECS`).
- Native AST nodelari mavjud: `DbSelect / DbInsert / DbUpdate / DbDelete`, `new Entity()`, `var.field`, `var.field = value`.
- Phase 0.1 legacy hack’lar tozalandi.
- **Joriy holat:** `deadpool-postgres` + `tokio-postgres`, async checkout. Phase 9.2'ga qarang.

### 1.3 Type System (very basic) ✅ → Phase 2.1 da yakunlangan
- Hozir tan olinadigan primitive’lar runtime’da: `string`, `int`, `double`, `bool`.
- Avtomatik koersiya: `int → string`, `string → int` (parse bo‘lsa), `int ↔ double`.
- Typed param + return + model JSON validatsiyasi mavjud (`runner.rs::check_typed_value`).
- **Joriy holat:** `uuid`, `datetime`, `decimal`, `json`, `bigint` Phase 2.1 da birinchi-sinf typelar bo'ldi.

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
- ✅ Sprint 6: `group by Entity.col [, ...]` + `having <cond>` — AST
  (`group_by`, `having` Expr::DbSelect'da) + parser + runner SQL emission
  (`SELECT ... FROM ... [WHERE] GROUP BY ... HAVING ...`) + validator
  (column existence + having-requires-group-by check) + 5 ta smoke test.
- **Qoldi:** `join` (navigatsiya bilan qoplandi).

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
- `catch_type` Phase 10.5 da real ishladi — message-pattern classifier orqali type'ga qarab dispatch; noma'lum kinds kompayl vaqtida bail.

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

## Phase 3 — Developer Experience ⏳ partial

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
- ✅ Phase 10.5: typed catch noma'lum kind uchun `closest_known_kind` hint.
- ✅ Sprint 4: W003 lint — empty function body (handler returns null silently).
  Duplicate routes endi validator-level (E-level) bo'lib qoldi — W-level emas.
- ✅ Sprint 4: W004 missing-`first` heuristic — `select Entity ... where
  Entity.pk == @x` (top-level `==` atom, no `and`/`or`) `first` siz array
  qaytaradi → warn. PK metadata `program.models` orqali olinadi.
- ✅ Sprint 4: W006 unreachable code after top-level `return` — function /
  route inline body / middleware. Branchli body (if/while/try) exempt
  chunki return faqat shu branchda ishlaydi. (W005 builtin-shadow uchun
  band qilingan.)
- ⏳ Sprint 4: numbered diagnostic codes — catalog ✅ (`src/error_codes.rs`
  W001..W006, E001..E010 + lookup + monotonic-order tests). Wiring the
  E-codes into `parser.rs::validate_program` `bail!` sites is the next
  step.

### 3.3 CLI ⏳ qisman
- ✅ `jwc lint` — `lint.rs::lint_program`: unused function (W001) va unused middleware (W002).
- ✅ `jwc migrate down` (Phase 0.3 dan).
- ✅ `jwc serve --watch` — `notify` crate file watcher; parent process child `jwc serve` ni kuzatadi va `.jwc` o‘zgarsa qayta ishga tushiradi.
- ⏳ `jwc fmt` ✅ v1 (line-based) — `src/fmt.rs`: tabs → 4 spaces, strip
  trailing whitespace, collapse runs of 3+ blank lines, single trailing
  newline. `--check` rejim CI uchun (non-zero exit'da diff). Idempotent.
  Qoldi v2: AST → source qayta chiqaruvi + comment preservation
  (token-stream attach).
- ⬜ `jwc add <pkg>` — paket qo‘shish (3.4 ga bog‘liq).

### 3.4 Package sistemasi ✅ shipped (path + git source; registry client deferred)
- ✅ Strukturalashgan `jwcproj.json::dependencies` (`{ "pkg": "^1.2" }` / `{ "pkg": { "path": "../lib" } }` / `{ "pkg": { "git": "...", "rev": "..." } }`).
- ✅ `type: "app" | "pkg"` manifest field — `pkg`-type loyihalar `jwc run/serve/build` ga rad etiladi (`load.manifest.ensure_runnable()`).
- ✅ JSONC manifest format — `//` line va `/* */` block izohlar + trailing comma toleratsiyasi (`project::strip_jsonc_comments`).
- ✅ Reproducible `jwcproj.lock` (semver, sha256, source URI) — [src/lockfile.rs](src/lockfile.rs).
- ✅ Backtracking resolver, conflict zanjir reporting — [src/resolver/mod.rs](src/resolver/mod.rs).
- ✅ Source backends: `PathSource` (lokal dir), `GitSource` (shell out to `git clone --depth 1` + `git checkout <rev>`), `RegistrySource` skeleton — [src/resolver/source.rs](src/resolver/source.rs).
- ✅ User cache layout `~/.jwc/registry/<host>/<pkg>/<version>/` va `~/.jwc/registry/git/<host>-<rev>/` — [src/pkg_cache.rs](src/pkg_cache.rs).
- ✅ Namespace + import + visibility til xususiyatlari:
  - `namespace foo.bar;` fayl boshida
  - `import foo.bar;` — paketning publik a'zolarini ochadi
  - `public` / `private` — default private, opt-in eksport
  - `mount greet [at "/prefix"];` — library route'larini yoqadi
  - `group "/p" use Mw1, Mw2 { ... }` — prefix + middleware bilan o'rab oladi (recursive)
- ✅ CLI komandalar: `jwc add` (`--path`/`--git`+`--rev`/`--version`), `jwc install`, `jwc update [pkg]`, `jwc remove <pkg>`, `jwc tree`.
- ✅ Native build (`flatten_namespaces`) mount expansion + FQN resolution-ni codegen oldidan qiladi — interpreter bilan bir xil natija.
- ✅ HTTP Registry klienti (Cargo-shape JSON index + tar+gzip extract + sha256 verify, configurable URL: env > manifest > built-in default) — [src/registry/client.rs](src/registry/client.rs).
- ⚠️ **`hub.jwc.dev` registry server hali alohida repoda emas** — built-in default `https://jwc-registry.1kb.uz/` placeholder. Server up bo'lguncha `path =` va `git =` ishlatiladi.
- ⬜ `jwc publish` — keyingi fazada.
- ⬜ `jwc login` / `~/.jwc/credentials.json` — Bearer token placeholder bor, lekin yozish CLI yo'q.

### 3.5 Package registry serveri ⬜ deferred (sibling repo)
- Mustaqil HTTP service: Cargo-mos index API + tarball blob store.
- Domain: `jwc-registry.1kb.uz`.

---

## Phase 4 — Real native compiler ⏳ partial

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
- LLVM IR (4.1) tugamasidan oldin ma’nosi yo‘q.

---

## Phase 5 — Ecosystem ⏳ partial

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

## Phase 6 — DX Polish ✅ done (microblog feedback'idan)

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

---

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
- ✅ Retry policy (Sprint 14): `Job.attempts` tracking, env `JWC_QUEUE_MAX_ATTEMPTS`
  (default 3), `JWC_QUEUE_BACKOFF_MS` (default 1000) — exponential backoff
  60s'da capped, max attempts'dan keyin drop + log. Worker thread sleep qilib
  re-enqueue qiladi.
- **Qoldi v2:** persistent backing (Redis/PG `_jwc_jobs` jadval), priority
  queues, dead-letter queue.

### 8.2 LSP server (basic)
- `cargo build --bin jwc-lsp` — stdio orqali tower-lsp ishlaydi.
- Diagnostics + hover ishlaydi (yuqorida 3.1'ga qarang).
- **Qoldi:** go-to-definition, autocomplete, route/middleware hover, semantic tokens.

---

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

### 9.5 Maqsadli raqamlar (verify pending) — Phase 10 da yopiladi
- GET (oddiy SELECT) c=100: **40–60k RPS**, p99 < 10ms.
- POST: **30–50k RPS**.
- c=500+ Linux'da real ravishda yelka tortishi mumkin (Windows loopback alohida holat).
- Benchmark setup: `examples/bench.sh` (bombardier), `examples/bench.py`
  (Python harness), `examples/jmeter/` (JMeter plan), `examples/bench-cs/`
  (.NET baseline solishtirma uchun). Real natijalar — keyingi iteratsiyada
  qo'shiladi.

---

## Phase 10 — Observability + streaming + SSE ⬜ planned

**Maqsad:** Production-ready obzor: tracing, real-time stream, va Phase 2.3
da qoldirilgan typed-catch tafsilotini yopish. Phase 9 da qo'yilgan perf
shiftini ham shu Phase'da o'lchab tasdiqlaymiz.

### 10.1 Perf baseline (Phase 9.5 closure) ⏳ blocked-on-infra
- `examples/bench.sh` + `bench.py` + JMeter run natijalarini ROADMAP'ga
  yozish: yangi async stack vs eski sync (v0.1.2) RPS/p99 jadvali.
- `.NET` baseline (`examples/bench-cs/`) bilan kontroll solishtirma —
  bir xil endpoint, bir xil DB, bir xil yuk.
- Maqsad: 40–60k RPS GET / 30–50k RPS POST (Phase 9.5 target) tasdig'i
  yoki yangi shiftga moslab raqamlarni yangilash.
- **Blocked:** kerakli infratuzilma — Postgres (Docker), bombardier/JMeter
  agent, .NET runtime — bu sessiyada mavjud emas. Bench setup fayllari
  tayyor; raqamlar live-host muhitida olinishi kerak.

### 10.2 Tracing + OpenTelemetry
- `tracing` crate bilan structured logs: request_id, route, status, latency.
- OTLP exporter — Jaeger / Tempo / Honeycomb'ga to'g'ridan-to'g'ri.
- Built-in: `trace_span(name)` / `trace_event(name, attrs)` til ichida.
- Env: `JWC_OTEL_ENDPOINT`, `JWC_OTEL_SERVICE_NAME`, sample rate.
- HTTP middleware'iga otomatik `traceparent` header propagation.

### 10.3 Stream-based `select` (katta result setlar)
- `for row in stream select Post from db.Posts where ...` — `tokio-postgres`
  `query_raw` orqali async iterator, butun resultni xotirada to'plamasdan.
- `break` / `early return` — server-side cursor close.
- JSON streaming response: `return ndjson(stream)` — chunked transfer.

### 10.4 Server-Sent Events ⏳ v1 syntax-only
- ✅ `route SSE "/feed"` syntaxsi — `route WS` ga parallel. AST'da
  `RouteProtocol::Sse` variant; method normalisation "SSE" formaga.
- ✅ Validator: `SSE` known method ro'yxatida.
- ⬜ v2: `Content-Type: text/event-stream` + chunked transport,
  `sse_send(event_name, data)`, `sse_close()`, `sse_broadcast(topic,
  payload)` builtinlar + per-topic subscriber registry.

### 10.5 Typed-catch dispatch ✅ v1
- ✅ Built-in error kinds (`runner::JWC_ERROR_KINDS`): `Error`,
  `DbError`, `HttpError`, `ValidationError`, `TimeoutError`.
- ✅ `catch (e: DbError)` filter — runtime message-pattern classifier
  (`runner::classify_jwc_error`) ishlaydi; type mos kelmasa xato qayta
  ko'tariladi (outer handler / `errorHandler` ushlaydi).
- ✅ Catch'da bound bo'lgan err JSON endi `{ "type": kind, "message", "causes" }` —
  `e.type` orqali user code branch qila oladi.
- ✅ Validator: noma'lum catch type "Did you mean?" hint bilan kompayl
  vaqtida bail qiladi (`closest_known_kind`).
- ✅ Native AOT codegen mirror'i: `jwc_classify_error` + `jwc_catch_type_matches`
  prelude'da, mismatch'da `resume_unwind`.
- ⬜ **Qoldi v2:** bir `try` blokda bir nechta `catch` clause
  (`catch (e: DbError) {} catch (e) {}`) — hozir bitta catch. Bu AST refactor.
- ⬜ **Qoldi v2:** classifierni `JwcError` enum + `.downcast_ref` ga
  ko'chirish — message-pattern brittle.

### 10.6 Native cross-target (Phase 4.2 davomi)
- `jwc build --native --target x86_64-unknown-linux-musl` — static binary.
- `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` matrix.
- Generatsiya qilingan `Cargo.toml` `--target` ni hisobga olib o'zgaradi
  (`reqwest` features, TLS backend tanlovi).
- CI'ga release matrix qo'shish (release.yml allaqachon `v*` tag'da ishlaydi).

### 10.7 Schema diff `migrate new` ga to'liq ulash
- `schema_diff.rs` joriy entity'lar va oldingi `.up.sql` o'rtasidagi farqdan
  faqat `ALTER TABLE` / yangi `CREATE TABLE` chiqaradi.
- Diff bo'lmasa "no schema changes" — bo'sh migration yaratilmasin.
- `--force` flag — diff bo'lmasa ham bo'sh migration yaratish (manual SQL
  uchun).

---

## Priority Timeline

Phase 0–2, 3.4 (path/git), 6–9 tugallandi. Keyingi ish ustuvorligi:

```
hozir    →  Phase 10.1 — real benchmark natijalari (bench-cs/bench.py/jmeter)
1-2 oy   →  Phase 10.2 — tracing + OTel exporter
2-3 oy   →  Phase 10.5 — typed-catch dispatch (Phase 2.3 yopiladi)
3-4 oy   →  Phase 10.3/10.4 — stream `select` + SSE routes
4-6 oy   →  Phase 3.1+ — LSP go-to-definition, autocomplete, semantic tokens
6-8 oy   →  Phase 3.3 + 10.6 — `jwc fmt` + native cross-target build matrix
6-8 oy   →  Phase 3.5 — `jwc-registry.1kb.uz` registry server (alohida repo)
8-12 oy  →  Phase 3.4 v1.1 — `jwc publish` / `jwc login` (registry server ishga tushgandan keyin)
12+ oy   →  Phase 4.1 (LLVM IR) + Phase 5 (wasm, Redis-backed cache, S3)
```

---

## Sprint Tracker

Phase tashqaridagi tactical sprint-by-sprint progress (2026 sessiyalari).

| # | Sprint | Status | Eslatma |
|---|--------|--------|---------|
| 1 | Verify & Hygiene | ✅ qisman | rustfmt + clippy + CI gate ✅, CONTRIBUTING.md ✅, code map refresh ✅. 10.1 perf bench ⏳ blocked-on-infra. |
| 2 | Type system finishing | ⏳ qisman | uuid/datetime/decimal/json/bigint ✅ (Phase 2.1). `byte[]` + explicit koersiyalar + sema pass — deferred. |
| 3 | LSP power | ⬜ deferred | go-to-definition, autocomplete, semantic tokens, route/middleware hover. |
| 4 | Diagnostics polish | ⏳ qisman | W003 empty body, W004 missing-`first`, W005 builtin-shadow, W006 unreachable-after-return ✅. Typed-catch closest-match ✅ (Phase 10.5). `jwc lint --json` editor/CI output ✅. Numbered-code catalog `src/error_codes.rs` ✅ (W001..W006, E001..E010 stub). Bail-site wiring — qoldi. |
| 5 | `jwc fmt` | ✅ v1 | Line-based formatter (`src/fmt.rs`) + `--check` rejim. AST → source renderer + comment preservation — v2. |
| 6 | SQL completeness | ⏳ qisman | `group by` + `having` ✅ (AST + parser + runner SQL + validator + 5 tests). Insert/update/delete payload field-check + DB schema drift — qoldi. |
| 7 | Code health refactor | ⏳ qisman | `cmd/pkg.rs` extracted (Add/Install/Update/Remove/Tree) ✅. `builtins.rs` extracted (BUILTINS/SPECIAL_BUILTINS — shared by lint + native) ✅. runner.rs / parser.rs modul ajratish — review-friendly bir nechta PR'larga bo'linishi kerak. |
| 8 | Native vs interpreter parity | ⏳ qisman | `--emit-rust-source` flag ✅ + `tests/native_emit.rs` smoke tests ✅. `tests/examples_parse.rs` golden harness ✅ (every example loads+validates on each CI run). Run vs build behavioural diff — still deferred. |
| 9-10 | Registry server | ⬜ blocked-on-infra | Alohida repo `jwc-registry.1kb.uz` kerak; bu sessiyada bajarib bo'lmaydi. |
| 11 | Publish & login | ⬜ blocked | Registry server ishga tushgandan keyin. |
| 12-13 | Native cross-target | ⬜ deferred | `--target` matrix + LLVM IR skeleton. |
| 14 | Queue robustness | ✅ qisman | Retry policy + exponential backoff ✅ (this session). Persistent backing + priority + DLQ — deferred. |
| 15-18 | Phase 5 ecosystem | ⬜ deferred | WASM, Redis cache, S3, SSE — Phase 10 davomida. |
| 19+ | Long-term | ⬜ | IR + zero-cost abstractions + self-hosting. |

---

## Ultimate Goal

> Web backend yozish → config yozish darajasida oson.
> Performance → Rust/Go darajasida.

---

## Kod xaritasi (orientir uchun)

```
src/
  lexer.rs           441  tokenizer + template string + raw strings + comment skip
  ast.rs             404  Program / Model / Route / Function / Stmt / Expr
  parser.rs         4037  recursive-descent + validate_program (column + catch-type checks)
  runner.rs         5414  async tree-walking Vm + classify_jwc_error + HTTP dispatch
  engine.rs          528  deadpool-postgres + tokio-postgres + prep cache + TTL cache
  server.rs          380  axum + tokio::spawn + WebSocket + metrics
  sql.rs             315  Postgres DDL generator
  migrate.rs         443  migrate new / up / down (advisory lock)
  schema_diff.rs    1020  entity ↔ .up.sql diff for migrate new
  project.rs         634  jwcproj parser + dotenv loader + source walker + import resolver
  diag.rs             31  byte offset → (line, col)
  lint.rs            272  unused fn (W001) / unused middleware (W002)
  queue.rs           380  in-process job queue + worker pool
  native_build.rs   2494  AST → Rust source (async tokio AOT path)
  cache.rs           147  in-memory TTL cache (cache_set/get/del/clear)
  jwt.rs             115  HS256 sign/verify
  password.rs         55  Argon2id hash/verify
  email.rs           183  lettre + rustls SMTP transport
  pkg_cache.rs       121  path/git package fetch cache
  lockfile.rs        152  jwcproj lockfile read/write
  error_report.rs     40  CLI error chain pretty-printer
  main.rs            585  CLI subcommands + embedded launcher
  bin/jwc_lsp.rs     429  tower-lsp server (diagnostics + hover)
```

Jami: ~18.2k qator Rust.
