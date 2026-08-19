# JWC Language — Roadmap

> Bu hujjat hozirgi kod holatining halol tahlili asosida tuzilgan.
> "Done" deb belgilangan band — manba kodda to'liq amalga oshirilgan demakdir.
> "Partial" — qisman ishlaydi, lekin yashirin hack yoki cheklov bor.
>
> Joriy holat: **v0.9.2** — Phase 0–11 yopildi, native query-layer parity bor.
> v0.7.0 real ilovalardan (MyWallet, jwc-shortener) kelgan feedback bo'yicha
> DSL/editor/HTTP-kontrakt tuzatishlari; v0.8.0 query layer (where'dan
> `and`/`or` jimgina yo'qolishi, having'da agregatlar, `select distinct`);
> v0.8.5 SQL parametrlarini ustun turiga qarab bog'lash + E022;
> v0.8.7 console/fayl builtinlari (`console.*`, `file.*`, `directory.*`).
> Batafsil — `CHANGELOG.md`.
>
> **1.0 gacha nima qolgani pastdagi "1.0 gacha yo'l xaritasi" bo'limida** —
> olti reliz: entity DSL, mapper, ergonomika, query yakuni, `jwc test`, rc.
> Sprint Tracker (pastda) yangi 1.0 yo'l xaritasini ("1.0 Readiness Plan")
> aks ettiradi.

---

## North Star (v1.0 fokusi)

> "Web backend yoz — CRUD'ni qo'lda yozmasdan, ORM bilan kurashmasdan, native-tez."

Har bir roadmap bandi shu jumlaga xizmat qilishi shart. Qilmasa — **Non-goals** ga ketadi.

## Non-goals (1.0 ga qadar va undan keyin ham — qat'iy "yo'q")

Bu band loyihaning soddaligini himoyalash uchun. Foydalanuvchilar so'rasa
yoki PR yuborsa ham — siyosat darajasidagi rad etish.

| Item | Sabab | Status |
|---|---|---|
| **LLVM IR backend** | Native AOT Rust-codegen orqali yetadi. LLVM yakka muhandis sig'imidan tashqarida | Non-goal |
| **Cross-target native build matrisi** (Windows-ARM, macOS-ARM, FreeBSD, …) | Linux x86_64 + aarch64 (glibc + musl), Windows x86_64, Docker amd64/arm64 yetadi. Boshqa target'lar shovqin | Non-goal — **lekin aarch64 Linux 0.9.6'da qo'shildi**: bu qator ARM uchun javob sifatida Docker arm64'ga ishora qilardi, holbuki u QEMU osilib qolgani uchun o'chirilgan edi — ya'ni hech qanday arm64 yo'li yo'q edi. Endi ikkalasi ham bor |
| **Self-hosting compiler** | JWC kompilyatori JWC'da yozilishi maqsad emas. Rust qoladi | Non-goal |
| **WASM target** | Backend tili — brauzer/edge runtimega chiqish niche'ga to'g'ri kelmaydi | Non-goal |
| **Multi-database driver** (MySQL/SQLite/MSSQL/Oracle) | Postgres-first va'dasi. SQL'ning Postgres dialect'iga sodiq qolamiz | Non-goal |
| **HTTP route SSE v2 / `stream select`** | CRUD og'rig'ini kamaytirmaydi | Non-goal (basic WebSocket bor) |
| **Background-job priority queue / DLQ ML retry policy** | Hozirgi durable queue + dead-letter yetarli | Non-goal (over-engineering) |
| **OTLP'ni "yadro" featuresi qilish** | `otlp` Cargo feature ortida qolishi kifoya, default-off | Non-goal (ops vositasi, ergonomika emas) |
| **Rich-domain object graph, change-tracking, lazy-loading, EF-style navigation propertylar** | Maqsad — ORM'siz qolish. JWC bu hududga kirmaydi | Non-goal (by design) |
| **Module / import sistemasi** | Bir-proyekt-bir-flat-namespace yetarli; modullar 1.0-blocker emas | Defer post-1.0 |

Ushbu band'lar uchun PR'lar yopiladi yoki forka tavsiya etiladi.

---

## Progress Snapshot

| Phase | Status |
|-------|--------|
| Phase 0 — Texnik qarz (legacy hack’larni tozalash) | ✅ Done |
| Phase 1 — MVP Core | ✅ Done (Sprint 1 closeout) |
| Phase 2 — Language Completeness | ✅ Done (Sprint 2A/2B/2C code-health audit yopildi) |
| Phase 3 — Developer Experience | ✅ Done (Sprint 3: typed catch + dotted subtypes + gradual type checker + AOT visibility) |
| Phase 4 — Real Compiler (Native) | ✅ Done for 1.0 scope (native AOT via Rust codegen + cargo). Query-layer native parity ✅ v0.6.x (nav eager-load/grouped agg/JOIN/op? + camelCase call-resolution fix). JWT builtinlari (`jwt_sign`/`jwt_verify`) ✅ native (v0.9.6 auditi: registry'da `native: true`) — Bearer-auth app endi to'liq native build bo'ladi. Bu qator uzoq vaqt teskarisini yozib turgan edi. LLVM IR + cross-target → **Non-goals** |
| Phase 5 — Ecosystem | ✅ Done for 1.0 surface (config registry + OTLP + persistent queue + DLQ + soak harness ✅; 72h soak real run = ops) |
| Phase 6 — DX Polish (real-app feedback) | ✅ Done (literals/now/@var.field/!/raw strings/typed-field/error-handler) |
| Phase 6 (security) — SECURITY.md + cargo audit + threat model + SSRF allowlist + JWT exp + secrets redaction | ✅ Done (external review = ops) |
| Phase 7 — Standard helpers (strings/arrays/iteration/json) | ✅ Done |
| Phase 7 (perf) — bench DB endpoints + AOT scope + README link | ✅ Done (Linux real-run + CI regression gate + 72h soak burn = ops) |
| Phase 8 — Background jobs + LSP + dev-experience close-out | ✅ Done (queue + jwc-lsp go-to-def/rename/completion + WebSocket + Docker/musl/templates/fmt/upgrade/Marketplace/autogen) |
| Phase 9 — Async runtime + perf ceiling | ✅ Done (async Vm + tokio-postgres + reqwest; native AOT also async) |
| Phase 10 — Observability (kichiroq scope) | ✅ Done for 1.0 surface (typed-catch dispatch ✅; OTLP exporter ✅ behind `otlp` feature). Stream `select` / `route SSE v2` / cross-target → **Non-goals** |
| **Phase 11 — Query Layer (1.0-blocker)** | ✅ Done (v0.5.0→v0.6.1): explicit JOIN + grouped agg over JOIN (0 raw_sql) + nav eager-load (belongs-to/has-many/one/m2m/2-level nested) + projection + `op?` optional filter + dynamic in-list (`= ANY`) + atomic `update set`/reorder + jonli `/openapi.json`. Native query-layer parity ✅. Kamchiliklar: pastdagi Phase 11 bo'limiga qarang |

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

### 2.7 Code-health audit (Sprint 2A/2B/2C) ✅
- ✅ Sprint 2A: `parser.rs` modul ajratish (parser/{mod,validate,tests}),
  `runner.rs` builtins ajratish, `clippy -- -D warnings` toza.
- ✅ Sprint 2B: unwrap budget — prod kodda **1 → 0** unwrap qoldi
  (qolgan barcha `.unwrap()` chaqiruvlar test fayllarda; CI `prod_unwraps.rs`
  audit testi yashil).
- ✅ Sprint 2C: anyhow chain depth audit + dead code clean-up + docstring
  pass kritik `src/*.rs` ustida.

---

## Phase 3 — Developer Experience ✅ done (1.0 surface)

**Maqsad:** JWC bilan yozish — Node yoki Go bilan yozishdek tezroq bo‘lsin.

### 3.0 Sprint 3 closeout (v0.4.7) ✅
- ✅ Typed catch + dotted subtypes — `catch (e: DbError.UniqueViolation)` PG
  SQLSTATE `23505` ga aniq mos keladi. Kinds: `Error`, `DbError`,
  `DbError.UniqueViolation`, `DbError.ForeignKeyViolation`,
  `DbError.NotNullViolation`, `HttpError`, `ValidationError`, `TimeoutError`.
- ✅ Gradual static type checker — `validate_program` ichida E018 (return
  type mismatch), E019 (wrong arg count), E020 (arg type mismatch); CLI
  `jwc check/run/build --no-typecheck` opt-out.
- ✅ AOT visibility re-check — E021 (private function cross-namespace call)
  kompayl-vaqt tekshiruvi.
- ✅ Integer / float / encoding / `==` semantics → `docs/spec/semantics.md`
  qarang (lexer/parser tahrirlari Sprint 3 closeout'da).

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

### 4.5 Sprint 4 closeout (data + runtime safety, v0.4.7) ✅
- ✅ Migration safety: SHA-256 checksum har applied migration bilan yozib
  qo'yiladi; `jwc migrate status` applied/pending/sha-mismatch/orphan
  ko'rsatadi; `jwc migrate up --dry-run` / `down --dry-run` SQLni chiqaradi,
  DB ga tegmaydi.
- ✅ `savepoint <name> { ... }` — `transaction { ... }` ichida nested
  rollback chegarasi. Literal nested transaction E016 bilan rad etiladi;
  `savepoint` transaction tashqarisida E017.
- ✅ `json()` string validatsiyasi + `json_unchecked()` escape hatch — eski
  v0.4.4 passthrough semantikasini saqlaydi (cached JSON fragmentlar
  uchun).
- ✅ Pool resilience: transient xatolar `JWC_DB_RETRY_MAX_ATTEMPTS` (default
  3) + `JWC_DB_RETRY_BACKOFF_MS` (default 100) eksponensial backoff bilan
  qayta urinadi. `engine::ping()` (`SELECT 1`) — `/readyz` uchun real
  end-to-end probe.
- ✅ Prometheus pool gauges: `jwc_db_pool_size`, `jwc_db_pool_available`,
  `jwc_db_pool_max_size`, `jwc_db_pool_waiting` `/metrics`'da chiqadi.
- ✅ Chaos test stub — `JWC_CHAOS_DB_FAIL_RATIO` integration testlarda
  retry yo'lini majburlaydi.

---

## Phase 5 — Ecosystem ✅ 1.0 surface done

**Maqsad:** JWC ni global backend tiliga aylantirish.

### 5.0 Sprint 5 closeout (ops + observability + queue durability, v0.4.7) ✅
- ✅ **Boot-time config registry** (`src/config.rs`) — har `JWC_*` env var
  schema bilan ro'yxatdan o'tadi; `JWC_PRINT_CONFIG=1` startup'da resolved
  jadval chiqaradi; valid bo'lmagan qiymatlar (range/type) bail qiladi.
- ✅ **OTLP tracing** Cargo feature `otlp` orqasida — `JWC_OTLP_ENDPOINT` +
  `JWC_SERVICE_NAME`. Default build slim, dependencies optsional.
- ✅ **Persistent job queue** — `JWC_QUEUE_DRIVER=postgres` durable `_jwc_jobs`
  jadvalga yozadi; terminal failure DLQ'ga ko'chadi. In-memory `memory`
  driver — backward-compatible default.
- ✅ **Soak harness** — `examples/soak/` orqali long-running load profil; 72h
  real soak run = ops javobgarligi (CI gate emas).

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
  - ✅ Redis (core tier, `ecosystem.md` Faza 1): `redis_get/set/del/
    exists/incr/expire/eval/ping/enabled` — `--features redis` ortida,
    interpreter + `--native` ikkalasida. `rediss://` TLS, deadpool,
    `/readyz` probe va `jwc_redis_pool_*` metrikalari bilan.
    ⬜ Qoldi: `redis_lpush` / `redis_brpop` — `brpop` bloklovchi
    operatsiya bo'lgani uchun (pool slotini timeout davomida ushlab
    turadi) durable queue'ning Redis backend'i bilan birga ko'riladi,
    KV cache bilan emas.
  - ⬜ Qoldi: Storage (S3), SSE.
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

### Phase 6 (security) closeout ✅
- ✅ `SECURITY.md` — coordinated-disclosure policy, supported versions.
- ✅ `cargo audit` blocking — release CI failonk known RustSec advisory.
- ✅ Threat model — `docs/spec/threat-model.md`.
- ✅ SSRF allowlist — `JWC_HTTP_ALLOWLIST` `http_get` / `http_post` /
  `fetch_json` ga deny-by-default policy yoqadi.
- ✅ JWT `exp` claim — `jwt_verify` muddati o'tgan tokenni rad etadi.
- ✅ Secrets redaction — log/error chain'larda `password`, `secret`, `token`
  pattern'lar maskalanadi.
- ⏳ External security review = ops javobgarligi.

### Phase 7 (perf) closeout ✅
- ✅ Bench DB endpoints — `examples/bench/` har stack uchun bir xil
  endpoint shape: ping, json-small, json-large, cpu, async-delay, db-read,
  db-write.
- ✅ Native AOT 1.0 scope hujjati — `docs/spec/aot-scope.md`.
- ✅ README ↔ bench repo cross-link.
- ⏳ Linux real-run + CI regression gate + 72h soak burn = ops.

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

## Phase 10 — Observability ✅ done for 1.0 surface

**Maqsad:** Production-ready obzor. North star fokusiga moslab qisqartirildi —
streaming/SSE va cross-target **Non-goals** ga ko'chdi.

### 10.5 Typed-catch dispatch ✅ v1
- ✅ Built-in error kinds (`runner::JWC_ERROR_KINDS`): `Error`,
  `DbError`, `HttpError`, `ValidationError`, `TimeoutError`.
- ✅ `catch (e: DbError)` filter — runtime message-pattern classifier
  (`runner::classify_jwc_error`); type mos kelmasa xato qayta ko'tariladi.
- ✅ Catch'da bound bo'lgan err JSON: `{ "type": kind, "message", "causes" }`.
- ✅ Validator: noma'lum catch type "Did you mean?" hint bilan kompayl
  vaqtida bail (`closest_known_kind`).
- ✅ Native AOT codegen mirror'i: `jwc_classify_error` + `jwc_catch_type_matches`.
- 📦 Defer post-1.0: multi-catch (`catch (e: DbError) {} catch (e) {}`) +
  `JwcError` enum + `.downcast_ref` classifier.

### 10.2 OTLP tracing ✅ optional, gated
- ✅ `otlp` Cargo feature ortida; runtime'da `JWC_OTLP_ENDPOINT` env bilan yoqiladi.
- ✅ Postgres pool, HTTP request, queue worker span'lari.
- Non-goal: OTLP'ni "yadro" qilish. Ergonomika emas, ops vositasi. Default-off.

### 10.7 Schema diff `migrate new` ga to'liq ulash ✅
- ✅ `schema_diff.rs` joriy entity'lar va oldingi `.up.sql` o'rtasidagi farqdan
  faqat `ALTER TABLE` / yangi `CREATE TABLE` chiqaradi.
- ✅ Diff bo'lmasa "no schema changes" — bo'sh migration yaratilmaydi.

### Cut: Streaming + SSE v2 + cross-target
- ❌ Stream `select` / `route SSE v2` / native cross-target matrix —
  hammasi **Non-goals** ga ko'chdi. CRUD og'rig'ini kamaytirmaydi.

---

## Phase 11 — Query Layer ✅ Done (v0.5.0 → v0.6.1)

**Maqsad (bajarildi):** Joinsiz "ORM og'rig'ini o'ldirdik" da'vosi yarim edi.
Bu Phase qolgan ~80% holatda raw_sql fallback'iga muhtojlikni o'ldirdi.
Dogfood: `task-tracker` — read-path N+1 = 0, stats/reorder uchun raw_sql = 0
(PAIN_LOG2/3/4). Reja: `jwc-query-layer-plan-v2.md` + `jwc-plan-v3.md`.

### 11.1 Join (FK orqali) ✅
- `select Task { columnId, columnName: Column.name, total: count(*) } from
  AppDb.Task join Column on Column.id == Task.columnId group by …` — explicit
  multi-entity equi-join, `j{i}` alias bilan kvalifikatsiya. SQL gen +
  prepared statement + native AOT codegen mirror ✅.
- (Cheklov) faqat Inner equi-join (`a == b`); LEFT/outer join — post-1.0.

### 11.2 Projection / shape ✅
- `select Entity { col, alias: Other.col, total: count(*) }` — aliased plain
  ustun (joined entity'dan) + aliased aggregate. Nav projection
  (`author: User { id, name }`) parolni yashiradi. HTTP response avto-JSON.

### 11.3 Composable filter ✅
- `and`/`or` + qavslar; joined entity field'lariga `where`'da murojaat.
- **`op?` optional predicate** (`status ==? @s`): qiymat bo'sh/null bo'lsa shart
  tushadi — bitta statik query barcha filter kombinatsiyasiga xizmat qiladi
  (in-code shoxlanish o'ldi).
- **Dynamic in-list** (`where id in (@arr)` → `= ANY($1)`): runtime massiv param.

### 11.4 Aggregation ✅
- `count`/`sum`/`avg`/`min`/`max` scalar **va** `group by` + `having` bilan
  grouped aggregation (1.0 scope'dan oshib bajarildi). JOIN ustidan grouped
  agg ham.

### 11.5 Eager-load + nav + boshqa (v3) ✅
- **`with`** eager-load: belongs-to, has-many/one, m2m (join-jadval orqali),
  **ikki-bosqichli nested** (`with boards.columns`) — barchasi korrelyatsiyali
  `json_agg` subquery, bitta query. Nav ordering (`orderby` nav-decl'da).
- **Atomik `update CTX.T set col = expr where …`** (D3) — read'siz partial
  update + reorder (`position = position ± 1`); lost-update oynasi yo'q.
- **Jonli `/openapi.json` + `/docs`** — runtime'da route'lardan generatsiya,
  drift mumkin emas.
- **`schema_diff`** mavjud ustunga qo'shilgan `unique`ni `ALTER` qiladi (D1).
- **Native AOT query-layer parity**: yuqoridagi nav/agg/JOIN/op? formalari
  native codegen'da (interpreter SQL'ini qayta ishlatadi) + camelCase
  funksiya-chaqiruv rezolyutsiya bug fix.

### 11.6 Conformance + docs ✅ qisman
- `tests/group_by.rs`, `tests/nested_with.rs`, `tests/native_emit.rs` +
  runner `nav_sql_tests` unit testlari ✅. `docs/.../queries.md` to'liq surface
  — qoldi.

### Phase 11 — qolgan kamchiliklar (halol)
1. **✅ YOPILGAN — Native: JWT builtinlari.** `jwt_sign`/`jwt_verify`
   registry'da `native: true`, ya'ni Bearer auth ishlatadigan app to'liq
   native build bo'ladi. Bu band yopilganidan keyin ham ochiq turgan edi;
   v0.9.6 auditida registry bilan solishtirilib aniqlandi.
2. **🟡 Native: dinamik in-list (`= ANY`) interpreter-only.** Massiv-param
   binding native'da yo'q (runtime coverage Linux/CI). Statik `in (a,b,c)` ✅.
3. **🟡 Native: JOIN where faqat asosiy entity ustuni.** WHERE/HAVING bind
   tipi asosiy entity'dan resolve bo'ladi; joined-entity ustuni bo'yicha WHERE
   native'da interpreter-only (struktura/SELECT/ON to'liq qo'llanadi).
4. **🟢 Faqat Inner equi-join.** LEFT/outer + non-equi ON — post-1.0.
5. **🟢 `group by`/`having` interpreter'da to'liq; arbitrary projection-agg
   aralashmasi** ba'zi holatda raw_sql talab qiladi (kam uchraydi).
6. **🟢 Native binar Windows'da runtime-test bo'lmaydi** — AOT Linux
   x86_64(+musl) only; bu env'da emit-source + SQL-probe darajasi (CI compile).

---

## 1.0 gacha yo'l xaritasi

Hozirgi holat: Phase 0–11 yopildi (v0.9.2). Query Layer tugadi, native
parity bor. Qolgani — quyidagi olti reliz.

Tartib bitta prinsipga bo'ysunadi: **buzuvchi o'zgarishlar erta, ergonomika
keyin, ishonch doimiy.** 1.0 sintaksisni muzlatadi, demak har qanday
breaking o'zgarish 0.10–0.13 ichida tugashi shart.

```
✅ v0.5.0 →  Query Layer yadrosi (eager-load + grouped agg)
✅ v0.6.0 →  explicit JOIN (0 raw_sql) + op? + dynamic in-list + nested with
             + atomic update-set + live OpenAPI + native query-layer parity
✅ v0.7.0 →  field feedback: index + unique(a,b) + &&/||/+=/?:/?? + CORS/405/
             dual-stack + bitta error envelope + project-wide diagnostics +
             fmt round-trip + native jwt_sign/jwt_verify va decimal
✅ v0.8.0 →  where'dan `and`/`or` yo'qolishi tuzatildi + having'da agregatlar
             (+ alias) + select distinct
✅ v0.8.5 →  SQL parametrlari ustun turiga qarab bog'lanadi + E022 (builtin
             aritetini `jwc check` da rad etish) + brending
✅ v0.8.7 →  console/fayl builtinlari: `console.*`, `file.*`, `directory.*`
             + `IoError` xato turi (qo'shimcha, breaking emas)
✅ v0.8.8 →  `console.writeln` + `int()` endi trim qiladi va parse bo'lmasa
             xato beradi (0 qaytarmaydi) — BREAKING
✅ v0.9.0 →  Redis core-tier driver: `redis_*` builtinlari (interpreter +
             `--native`), `rediss://` TLS, `/readyz` probe va
             `jwc_redis_pool_*` metrikalari. `--features redis` ortida.
✅ v0.9.2 →  `log_insert` — buferlangan, batchli telemetriya yozuvi
             (so'rov yo'lidan tashqarida) + native'da `/metrics`.
             Uchta native-parity tuzatmasi: `pattern(...)` endi haqiqatan
             bajariladi (xavfsizlik), `after { }` bloklari ishlaydi,
             vendored paket ikki marta yuklanmaydi.
   v0.10.0→  Entity DSL: default / private / server / enum / composite pk
   v0.11.0→  Mapper: new X from Y / patch / class validatsiyasi / check
   v0.12.0→  Til ergonomikasi: body().x / xs[0] / throw / default param
   v0.13.0→  Query layer yakuni: ko'p ustunli orderby / LEFT JOIN / subquery
   v0.14.0→  `jwc test` — haqiqiy test framework
   v1.0.0-rc.1 → ishonch: differensial qamrov, audit, soak, pilot ko'chirish
   v1.0.0 →  sintaksis muzlaydi. Breaking faqat 2.0 da
```

---

### v0.10.0 — Entity DSL: default'lar va chegaralar

Birinchi, chunki mapper'ning butun ma'nosi "qaysi ustun avtomatik to'ladi,
qaysi biri taqiqlangan" degan savolga bog'liq. Buni oldin belgilamasak,
mapper semantikasini ikki marta ta'riflashga to'g'ri keladi.

- `default <expr>` — `default uuid()`, `default now()`, `default "posted"`
- `on update now()` — `updatedAt` uchun
- **`private`** — na javobda, na body'dan (`passwordHash`, `importHash`)
- **`server`** — javobda bor, body'dan yo'q (`createdBy`, `status`)
- `enum Direction { in, out }` — DB'da `CHECK`, validatsiyada avtomatik,
  qoida ikki joyda takrorlanmaydi
- composite `pk on (a, b)`
- `on update cascade`

> **BREAKING:** `private` bugungi `select E from ...` javobini o'zgartiradi.
> Bu ataylab: default xavfsiz tomonga buriladi, proyeksiya esa xavfsizlik
> uchun emas, trafik uchun yoziladigan bo'ladi.

### v0.11.0 — Mapper

80 ustunli entity uchun router ichida 80 qator o'zlashtirish yozilmasin.
Chiqish tomonini `select` proyeksiyasi allaqachon hal qiladi — bu reliz
faqat **kirish** tomoni haqida.

- **`new Entity from <record>`** — manba `body()` ham, DTO instance ham
  bo'lishi mumkin. Nom bo'yicha moslash; `pk` / `default` / `server` /
  `private` avtomatik chetlab o'tiladi
- **`patch e from body()`** — faqat body'da **mavjud** kalitlar. `insert`
  dan boshqa semantika, shuning uchun alohida so'z
- **class maydonlarida validatsiya qoidalari** —
  `amount decimal(14,2) required, min(1);`
- **`check <expr> : "xabar"`** — maydonlararo qoida. Deklarativ va
  avtomatik; `validate()` metodi emas, chunki metodni chaqirishni unutish
  mumkin va bu aynan biz `private` bilan yopayotgan xato turi
- **`error[E011]`** — `NOT NULL`, default'siz, body'dan kelolmaydigan va
  hech qayerda o'zlashtirilmagan ustun → kompilyatsiya xatosi

`validate body { ... }` qoladi va DTO majburiy emas: u shakl takrorlanganda,
maydonlararo qoida kerak bo'lganda yoki OpenAPI'da nomlangan sxema kerak
bo'lganda o'zini oqlaydi.

Mass-assignment himoyasi til darajasida: `{"id":1,"role":"admin"}` yuborilsa
o'sha maydonlar jimgina tashlanadi. Bugun bunga to'siq — dasturchining har
maydonni qo'lda yozgani, ya'ni himoya diqqatga bog'liq.

### v0.12.0 — Til ergonomikasi

Har bir handler'da seziladigan, additive (breaking emas):

- `body().x` — chaqiruv natijasidan maydon olish
- `xs[0]` — indeksatsiya
- `throw`
- default parametr qiymatlari
- `for i, x in xs`

### v0.13.0 — Query layer yakuni

- **Ko'p ustunli `orderby`** — bugun parser bitta ustun oladi; jadval UI'si
  uchun majburiy
- **`LEFT JOIN`** — "har bir kategoriya va undagi tranzaksiyalar soni, nol
  bo'lsa ham". Avval post-1.0 ga qo'yilgandi; CRUD hisobotlarining yarmi shu
  shaklda, shuning uchun 1.0 ichiga ko'chirildi
- `count(distinct col)`
- `where` ichida subquery (`exists` / `in (select ...)`)

Window funksiya, CTE, `union` — `raw_sql` da qoladi (Non-goal).

### v0.14.0 — `jwc test`

Eng katta strukturaviy bo'shliq: bugun `jwc test` faqat validatsiya qiladi,
ya'ni **JWC'da yozilgan kodni JWC'da test qilib bo'lmaydi**. 1.0 tili uchun
bu qabul qilib bo'lmaydigan kamchilik.

Minimum: test bloki, assert'lar, DB fixture, har test uchun transaction
rollback, runner.

### v1.0.0-rc.1 — Ishonch

Yangi funksiya yo'q, faqat dalil.

- Differensial query suite'ni har yangi shaklga kengaytirish
  (`tests/query_differential.rs`, CI'da Postgres service bilan)
- `integration_db` ni service container'ga ko'chirish — hozir
  testcontainers'da va CI'da skip bo'ladi; **skip pass sifatida
  o'qilmasligi** kerak
- **"Hujjatda bor, kodda yo'q" auditi** — `where col is null` shunday chiqdi:
  hujjatda qo'llab-quvvatlanadigan operator sifatida sanalgan, amalda hech
  qachon parse bo'lmagan. Yana borligini tekshirish kerak
- Parser fuzz, 72 soatlik soak
- MyWallet va task-tracker'ni to'liq yangi sintaksisga ko'chirish

### v1.0.0

Sintaksis muzlaydi. Buzuvchi o'zgarish faqat 2.0 da. Xavfsizlik tuzatmalari
qo'llab-quvvatlanadi.

---

### Doimiy shart — ishonch

Bu reliz emas, **har bir relizning qabul shartlari**.

v0.6.3–v0.8.0 oralig'ida topilgan bug'larning aksariyati bitta turda edi:
**jimgina noto'g'ri javob**. `where` dan `and` yo'qolishi, `RETURNING`
ustun o'rniga affected-count qaytarishi, native'da decimal `null` bo'lib
kelishi. Va ularning hech biri foydalanuvchi shikoyatidan kelmadi —
qaralganda topildi.

Shuning uchun v0.10.0 dan boshlab: **har bir yangi query shakli differensial
testsiz tugallangan hisoblanmaydi.** Qo'lda yozilgan SQL bilan natija
solishtirilmasa, u "ishlaydi" deb aytilmaydi. Unit test SQL matnini
tekshiradi, matn esa har doim to'g'ri ko'rinadi.

---

Post-1.0 (xohlasak): jwc-registry server, jwc publish/login, modul
sistemasi, qo'shimcha package ekotizimi. Hammasi opsional, north star
fokusini buzmasligi shart.

---

## Sprint Tracker

Phase tashqaridagi tactical sprint-by-sprint progress (2026 sessiyalari).

| # | Sprint | Status | Eslatma |
|---|--------|--------|---------|
| 1 | Verify & Hygiene | ✅ qisman | rustfmt + clippy + CI gate ✅, CONTRIBUTING.md ✅, code map refresh ✅. 10.1 perf bench ⏳ blocked-on-infra. |
| 2 | Type system finishing | ⏳ qisman | uuid/datetime/decimal/json/bigint ✅ (Phase 2.1). `bytea` entity column + `bytes`/`byte[]` typed-param (base64 runtime check via `looks_like_base64`) ✅. `src/sema.rs` skeleton (forwards to validate_program; future state-extraction hook) ✅. Real `Value::Bytes` variant + explicit koersiyalar — qoldi. |
| 3 | LSP power | ⏳ qisman | `textDocument/documentSymbol` outline ✅. `textDocument/definition` go-to-def (same-doc, function / model / middleware) ✅. `textDocument/completion` — 30 keywords + 33 builtins ✅. Semantic tokens + cross-file definition lookup — qoldi. |
| 4 | Diagnostics polish | ⏳ qisman | W003 empty body, W004 missing-`first`, W005 builtin-shadow, W006 unreachable-after-return ✅. Typed-catch closest-match ✅ (Phase 10.5). `jwc lint --json` editor/CI output ✅. Numbered-code catalog `src/error_codes.rs` ✅ (W001..W006, E001..E010). Bail-site wiring: E001/E002/E004/E005/E007/E008/E009/E010 ✅; E003/E006 — qoldi. |
| 5 | `jwc fmt` | ✅ v1 | Line-based formatter (`src/fmt.rs`) + `--check` rejim. AST → source renderer + comment preservation — v2. |
| 6 | SQL completeness | ⏳ qisman | `group by` + `having` ✅. `jwc migrate list` offline enumerator ✅. Insert/FieldAssign payload field-name compile-time check ✅ (tracks `let v = new Entity()` bindings + if/else/loop branch-aware intersect). Live DB schema drift — qoldi. |
| 7 | Code health refactor | ⏳ qisman | 8 cmd modullari: `pkg`, `migrate`, `lint`, `check`, `fmt`, `build`, `run`, `serve` ✅ + `builtins.rs` ✅. main.rs 349 qator — pure Clap dispatcher, har handler `cmd::<sub>::run` ortida. runner.rs `src/runner/{mod,builtins}.rs` ga ajratildi ✅ (v0.4.0). parser.rs modul ajratish — qoldi. |
| 8 | Native vs interpreter parity | ⏳ qisman | `--emit-rust-source` flag ✅, `tests/native_emit.rs` ✅, `tests/examples_parse.rs` ✅, `tests/native_parity.rs` ✅ (golden harness). v0.4.0 parity auditi: array literal, hash builtinlari, `const`, custom MIME bayt-aynan. **v0.6.x: Query Layer native parity** ✅ — nav eager-load (belongs-to/has-many/one/m2m/nested), grouped aggregation, explicit JOIN + aliased cols, `op?` optional predicate — barchasi interpreter SQL builder'larini qayta ishlaydi (`build_navigation_subqueries`/`where_col_sql`/`agg_select_sql` `pub(crate)`), natija `row_to_json(r)::text` → `jwc_db_query_json`. **camelCase funksiya-chaqiruv rezolyutsiya bug fix** (`rewrite_expr`) — `byStatus()` kabi root call FQN'ga o'tmasdan "unknown function" berardi; real app'lar uchun native'ni ochdi. **Qolgan kamchiliklar:** (a) ~~JWT builtinlari native'da yo'q~~ — **yopilgan**, `jwt_sign`/`jwt_verify` registry'da `native: true`; (b) dinamik in-list `= ANY` native'da interpreter-only; (c) JOIN WHERE joined-entity ustuni native'da interpreter-only; (d) native runtime faqat Linux/CI (Windows — emit + SQL-probe). **v0.9.6: Cargo-build-and-diff v2** ✅ — `tests/differential.rs` generatsiya qilingan crate'ni haqiqatan `cargo` bilan quradi, binarni ishga tushiradi va ikkala backend'ga real HTTP so'rov yuboradi; kutilgan natija fixture'da e'lon qilinadi, backend'lar ovoz bermaydi. Shu harness darhol yangi divergensiya topdi: string argumentli `notFound`/`badRequest`/`internalError`/`unauthorized`/`forbidden` native'da `{"error":...}` konvertiga o'ralmasdan xom matn qaytarardi. |
| 9-10 | Registry server | ⬜ blocked-on-infra | Alohida repo `jwc-registry.1kb.uz` kerak; bu sessiyada bajarib bo'lmaydi. |
| 11 | Publish & login | ⬜ blocked | Registry server ishga tushgandan keyin. |
| 12-13 | Native cross-target | ⏳ qisman | Sprint 12 `--target` flag ✅ + 5-triple allowlist + `tests/native_target.rs` (5 tests). Sprint 13 `src/native_ir.rs` skeleton ✅. End-to-end AST → IR → LLVM via inkwell — deferred. |
| 14 | Queue robustness | ✅ qisman | Retry policy + exponential backoff ✅. `enqueue_urgent` 2-tier priority (front-of-queue + FIFO within urgent block) ✅. Dead-letter queue (`JWC_QUEUE_DLQ_MAX`, `dlq_count()`/`dlq_drain()` built-ins) ✅. Persistent backing (Postgres `_jwc_jobs`) + n-level priority — deferred. |
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
