# JWC Language — Implementation-Level Feedback

Feedback1 strategik va positioning haqida edi.
Bu feedback esa boshqa burchakdan — **hozirgi kod holati, implementation sifati va engineering decisionlar** bo‘yicha.

JWC ROADMAP halol yozilgan: "Done" haqiqatan done, "Partial" halol partial deb belgilangan. Bu juda yaxshi signal — ko‘pchilik open-source projectlarda bunday halollik yo‘q.

---

# Kuchli implementation tomonlari

## 1. Roadmap’ning halolligi — engineering integrity belgisi

Phase 0 sarlavhasi **"Texnik qarz — legacy hack’larni tozalash"** deb yozilgan.

Yangi language yaratuvchi insonlarning aksariyati legacy hack’larini yashiradi yoki "v2 da hal qilamiz" deydi.

JWC esa:

* `parser.rs::normalize_webapi_compat()` butunlay olib tashlandi,
* `runner.rs` dan ≈224 qator hardcoded built-in shoxlari o‘chirildi,
* Multi-driver claim halol kamaytirildi ("Postgres only").

Bu **maturity belgisi**.

---

## 2. Async + Native integration — eng kutilmagan kuchli joy

ROADMAP Phase 4 da "Native AOT — deferred" deyilgandi.
Hozir esa kodda:

* `src/native_build.rs` — **74 KB**,
* `src/native_prelude.rs.in` — **36 KB**,
* `native_prelude_db.rs.in`, `native_prelude_ws.rs.in` — alohida modullar,
* Phase 9 da **"native AOT also async"** belgilangan.

Ya’ni interpreter va native compiler **bir vaqtning o‘zida** async runtime’ga o‘tdi. Bu uncha tez-tez ko‘rinmaydigan yutuq — ko‘p tillarda native va interpreted execution model bir-biridan ajralib ketadi va tushuncha mismatch tug‘iladi.

Eng katta keyingi savol:

> Native generated code idiomatic Rust ga qanchalik yaqin?

Agar `cargo expand` qilsa o‘qib bo‘ladigan Rust bo‘lsa — JWC nafaqat language, balki **"Rust DSL generator"** sifatida ham qiziq position oladi.

---

## 3. DB layer — boshqa "new language"larga nisbatan kuchli boshlanish

ROADMAP 2.2c da yozilganlar:

* `JWC_DB_POOL_SIZE`, `MIN_IDLE`, `MAX_LIFETIME`, `IDLE_TIMEOUT`, `CONNECTION_TIMEOUT` — to‘liq env-tunable pool,
* `pg_advisory_lock` bilan migration race protection,
* `JWC_DB_TLS` + `INSECURE_SKIP_VERIFY` — real prod uchun TLS sozlash,
* `testcontainers-rs` orqali Docker Postgres’da 6 ta integration scenario.

Bu o‘rta kompaniyaning ichki backendidagi DB infra’dan kuchliroq.

Ko‘p yangi language’larda DB layer hech qachon "actually operational" darajaga yetmaydi — JWC esa allaqachon yetib bordi.

---

## 4. Schema diff generator’ning real bo‘lishi

`src/schema_diff.rs` — **36 KB**. ROADMAP da "auto-migration generator" deyilgan va `jwc migrate new` allaqachon eski `.up.sql` ni parse qilib ALTER chiqaryapti.

Bu Prisma/Drizzle/EF Core ekosistemasidagi eng katta DX qulayliklaridan biri. Yangi language uchun bu featureni shu darajada erta bosqichda ko‘rish — kutilmaganda kuchli.

---

# Yashirin xavflar — kod holatidan kelib chiqib

## 1. `runner.rs` — 187 KB. `parser.rs` — 125 KB.

Bu **single-file complexity bomb**.

Hozircha bitta inson bu fayllarning hammasini bosh ichida saqlay oladi.
Lekin:

* yangi contributor kelganda,
* yoki muallifning o‘zi 6 oydan keyin qaytib kelganda,

187 KB file ichida `match` arms’da yashirin bug topish — soatlik ish bo‘ladi.

**Tavsiya:**

* `runner.rs` ni built-in family bo‘yicha modullashtirish: `runner/control_flow.rs`, `runner/db_ops.rs`, `runner/http_ctx.rs`, `runner/json.rs`, `runner/strings.rs`.
* `parser.rs` ni grammar bo‘limlari bo‘yicha bo‘lish: `parser/expr.rs`, `parser/stmt.rs`, `parser/sql.rs`, `parser/entity.rs`, `parser/validate.rs`.

Bu test surface’ni o‘zgartirmaydi — `pub use` bilan API ushlanadi.

---

## 2. `validate_program` `parser.rs` ichida — sezilarli architecture smell

CLAUDE.md da aniq aytilgan:

> `validate_program` re-walks the AST to enforce dbcontext/entity/column compile-time checks. Many invariants are enforced here, not in the runtime.

Bu pragmatik qaror, lekin uzoq muddatda muammo:

* LSP (`jwc-lsp`) bu invariantlarni qayta bilishi kerak — code duplication,
* `lint.rs` bu invariantlarni ko‘rmaydi,
* Native compiler bu invariantlarga tayanib kod generatsiya qiladi.

**Tavsiya:** O‘rta muddatda alohida `sema.rs` (semantic analysis pass) yaratish. AST → typed-AST yo‘nalishi. Bu Phase 5 oxiriga qadar boshlanmasa, technical debt sifatida o‘sib boradi.

---

## 3. `ENGINE: OnceLock<JwcEngine>` — process-global singleton

CLAUDE.md o‘zi tan oladi:

> Tests reset the `public` schema between runs but **do not** reset `ENGINE`.

Bu hozircha ishlaydi, lekin:

* multi-tenant ishlash (bir process, bir nechta DB) imkonsiz,
* native binarylarda `ENGINE` initialization vaqti — startup ovorachilik,
* test isolation `Mutex` bilan ushlangan — concurrent test running yo‘q.

**Tavsiya:** `ENGINE` ni `Vm` ga inject qilinadigan parameter ga aylantirish. Singleton remove qilish — uzoqroq ish, lekin bir-marotaba qilinadi.

---

## 4. Native AOT yo‘li bilan interpreter parallel rivojlanmoqda — feature drift xavfi

Hozir har bir yangi built-in:

1. `runner.rs` da implement bo‘lishi kerak,
2. `native_prelude*.rs.in` da implement bo‘lishi kerak,
3. `native_build.rs` da codegen bo‘lishi kerak.

Bu **3x effort multiplier**.

Hozircha solo author hammasiga ulguryapti. Lekin yangi feature qo‘shilganda biri kechikib ketadi va native binary bilan interpreter natijasi farq qila boshlaydi.

**Tavsiya:**

* `tests/integration_db.rs` ga qo‘shimcha **golden test suite** kerak: har bir misol fayl uchun `cargo run -- run` natijasi va `cargo run -- build && ./binary` natijasi `diff` bo‘lmasligi shart.
* CI da bu majburiy gate bo‘lsin.

Aks holda 3-6 oydan keyin "interpreterda ishlaydi, native’da ishlamaydi" bug’lari boshlanadi.

---

## 5. `cargo fmt config yo‘q, clippy lint config yo‘q`

CLAUDE.md o‘zi:

> There is no `cargo fmt` config and no clippy lint config beyond defaults; match the surrounding style.

Bu 1 ta odam yozayotganda muammo emas. 2-chi contributor kelishi bilan style drift boshlanadi.

**Tavsiya:** `rustfmt.toml` + `clippy.toml` + CI da `cargo fmt --check && cargo clippy -- -D warnings` qo‘shish. **1 soatlik ish, uzoq muddatli foyda.**

---

## 6. `vscode-extension` LSP binary path’i bilan bog‘liq risk

CLAUDE.md ogohlantirgan:

> if you rename or move the `jwc-lsp` binary in Rust, update the extension config in the same change.

Bu manual coordination — buzilishi muqarrar.

**Tavsiya:** Extension `package.json` da `jwc-lsp` ni `PATH` dan topish strategiyasini default qilish, fallback sifatida well-known locationlar ro‘yxati. Hard-coded path olib tashlanishi kerak.

---

# Implementation-level checkpointlar (3-6 oy)

Strategik tavsiyalar emas — **aniq, kuzatilishi mumkin texnik checkpointlar**:

| # | Checkpoint | Sabab |
|---|---|---|
| 1 | `runner.rs` < 50 KB, modul bo‘yicha bo‘lingan | Solo maintainability ceiling |
| 2 | `sema.rs` (yoki `validate.rs`) — parser’dan ajratilgan | LSP/lint/codegen duplikatsiyasini to‘xtatish |
| 3 | Native vs interpreter golden test suite, CI gate | Feature drift muammosi |
| 4 | `cargo fmt` + `clippy` CI gate | Style/quality regression himoyasi |
| 5 | `ENGINE` singleton’dan injected `Vm` field’ga | Multi-tenant, test isolation, native startup |
| 6 | Schema diff generator — destructive operation’lar (DROP COLUMN, ALTER TYPE) safety check | Real prod migration’lar uchun majburiy |
| 7 | `examples/testapp` va `examples/testapp_copy` — bir testapp dan ikki release path uchun ishlatilyapti; bu intentional bo‘lsa documentation kerak | Build artifact discipline |
| 8 | `target/` ichida `testapp.exe`, `testapp.jwcroot` git tracked — bu accident bo‘lishi mumkin | Repo hygiene |

---

# Generated Rust code o‘qib ko‘rilishi kerak

feedback1 da yozilgandek, native compilerning generated Rust code sifati JWC ning kelajagini hal qiladi.

Hozir `native_prelude.rs.in` (36 KB) — bu **template-style prelude**, ya’ni har bir generated binary ichiga embed bo‘ladi. Bu yondashuv:

* **Plus:** Generated code kichkina bo‘ladi, runtime helper’lar bir joyda.
* **Minus:** Har bir misol fayl 36 KB+ prelude o‘zining ichida olib yuradi — agar 100 ta endpoint bo‘lsa, har biri uchun emas, balki bitta loyihada 1x bu cost. Hozircha OK.

**Asosiy savol:** generated `main.rs` o‘qib bo‘ladimi?

> `cargo run -- build examples/testapp --release --emit-rust-source` kabi flag bo‘lsa, generated code ni real ko‘rib, debug qilib bo‘ladi. Hozircha bu flag yo‘q — qo‘shilishi muhim DX feature.

---

# Eng katta xavf — solo maintainer bottleneck

Hozir codebase:

* `parser.rs` 125 KB,
* `runner.rs` 187 KB,
* `native_build.rs` 74 KB,
* 3 ta `native_prelude*.rs.in`,
* `schema_diff.rs` 36 KB,
* LSP, VS Code extension, docs, examples, benchmarks, Docker setup.

Bu **bitta inson uchun juda katta surface**.

**Tavsiya:**

1. `CONTRIBUTING.md` yozish — yangi odam qayerdan boshlashi mumkinligi haqida.
2. `good first issue` style yo‘nalishlar — masalan: yangi built-in funksiya qo‘shish bo‘yicha aniq qadamlar.
3. Modullashtirish (yuqorida aytilgan) — onboarding uchun majburiy.

Aks holda JWC technically impressive solo project bo‘lib qoladi — feedback1 da aytilgan "abandoned language" toifasiga tushib qolish xavfining real ko‘rinishi shu.

---

# Yakuniy implementation-level baho

JWC kod holati:

* **Halollik:** ROADMAP haqiqatda kodga mos keladi. Bu juda kam uchraydi.
* **Operatsion sifat:** DB layer + migration + pool tuning real prod darajasida.
* **Architecture:** Sync interpreter + async HTTP + native AOT — uchchaladan birgalikda ishlayotgani professional.
* **Riskli zonalar:** Single-file size, validate-in-parser, native vs interpreter drift, solo maintainer.

Bir necha gap bilan:

> **Bu o‘zining yoshiga (early-stage) nisbatan kutilmaganda mature codebase. Asosiy xavf — texnik emas, balki bandwidth: muallif bitta odam, kod allaqachon "ikki yarim odam yoza oladigan" hajmda. Modullashtirish va onboarding infrastructure — keyingi 3 oyning eng muhim ishi.**
