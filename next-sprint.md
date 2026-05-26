# Sprint: JWC v0.4.0 — Array + Builtin Parity

**Davomiyligi:** 10 ish kuni / 2 hafta
**Maqsad:** array literal/builder, builtin parity (interpreter ↔ native AOT), va kichik DX tuzatishlari.
**Branch:** `feature/v0.4.0-array-builtins`

---

## Hafta 1 — Asos

### 1-kun: Builtin single-source refactor
Eng birinchi qilinishi shart — qolgan barcha builtin ishlari shunga tayanadi.

**Ish:**
- Yangi `src/builtins.rs` fayli: har bir builtin uchun metadata.
  ```rust
  pub struct BuiltinDef {
      pub name: &'static str,
      pub args: &'static [BuiltinArgKind],
      pub returns: BuiltinReturnKind,
      pub eval_fn: BuiltinEvalFn,        // runner.rs uchun
      pub native_codegen: NativeCodegen, // native_build.rs uchun
  }
  pub static BUILTINS: &[BuiltinDef] = &[ /* ... */ ];
  ```
- `runner.rs::call_builtin` shu jadvaldan tarqaladi.
- `native_build.rs` BUILTINS roʻyxati shu yerdan generatsiya qilinadi (yoki shu jadvaldan oʻqiydi).
- Mavjud builtinlarni (text, html, json, redirect, status_code, headers, cache_*, db_*, env, hash_password, …) yangi formatga koʻchirish.

**Acceptance:**
- Yangi builtin qoʻshganda faqat bitta faylga tegiladi.
- `cargo test` to'liq yashil — regression yo'q.
- `examples/testapp` ham `jwc run`, ham `jwc build --native` da ishlaydi.

---

### 2-kun: Array literal `[1, 2, 3]`

**Ish:**
- `ast.rs`: `Expr::ArrayLit(Vec<Expr>)`.
- `lexer.rs`: `[` va `]` allaqachon bor — tekshirish kifoya.
- `parser.rs`: `[` keyin expression list (vergul ajratilgan), trailing comma ruxsat, `]`.
- `parser.rs::validate_program`: har elementni alohida validate (heterogenous ruxsat etiladi).
- `runner.rs`: `Value::Array(Vec<Value>)` ga eval.
- `native_build.rs`: `vec![...]` ga codegen.

**Acceptance:**
- `let xs = [1, 2, 3]; for x in xs { print(x) }` ikkala mode da ishlaydi.
- `let mixed = [1, "two", true];` parse va eval boʻladi.
- Boʻsh array `[]` qoʻllab-quvvatlanadi.

---

### 3-kun: `range`, `push`, `join`
String concat O(n²) muammoni hal qiladi — 1000 elementli JSON qurish < 50ms.

**Ish:**
- `range(n)` — `[0, 1, ..., n-1]` array.
- `range(start, end)` — `[start, ..., end-1]`.
- `range(start, end, step)` — step bilan.
- `push(arr, x)` / `append(arr, x)` — mutating, `Value::Array` ichidagi `Vec` ga.
- `join(arr, sep)` — array elementlarini stringga koʻchirib `sep` bilan birlashtiradi, O(n).
- Native codegen: `(start..end).step_by(step).collect::<Vec<_>>()`, `Vec::push`, `.iter().map(...).collect::<Vec<_>>().join(sep)`.

**Acceptance:**
- `let xs = range(0, 100000); let s = join(xs, ",");` < 50ms.
- `let xs = []; for i in range(0, 10) { push(xs, i*i) }` ishlaydi.
- `jwc build --native` da ham ishlaydi.

---

### 4-kun: Hash builtinlari + native parity

**Ish:**
- `sha256(s)` → hex string, `sha2` crate.
- `sha1(s)` → hex string, `sha1` crate.
- `md5(s)` → hex string, `md-5` crate.
- `hmac_sha256(key, msg)` → hex string, `hmac` + `sha2`.
- `hash_password(s)` va `verify_password(hash, s)` ni `native_build.rs` BUILTINS ga qoʻshish (argon2 crate aslida tayyor, faqat whitelistda yoʻq edi).
- `Cargo.toml` da yangi dependencylar.

**Acceptance:**
- `jwc build --native` `hash_password` ni reject qilmaydi.
- `sha256("hello") == "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"` ikkala mode da.
- HMAC RFC 4231 test vektorlari oʻtadi.

---

### 5-kun: Module-level `const`

**Ish:**
- `parser.rs`: top-level `const NAME = expr;` deklaratsiya.
- `ast.rs`: `ConstDecl { name, expr, ty }`.
- `Program` ga `consts: Vec<ConstDecl>` qoʻshish.
- `project::load`: const'lar bir marta `Vm::eval_expr` orqali eval qilinadi, natija `Program::const_values` ga saqlanadi.
- Route/function ichida `const` nomi `Vm::lookup_variable` da `consts` dan oʻqiladi.
- Static, frozen — mutable EMAS (mutable use-case `cache_*` da qoladi).
- `validate_program`: circular reference (`const X = X + 1;`) va non-const expression aniqlash.

**Acceptance:**
- `const PI = 3.14159;` route ichida koʻrinadi.
- `const X = X + 1;` validate xato beradi.
- `const Y = db_query(...);` validate xato beradi (non-const expression).

---

## Hafta 2 — DX, audit, release

### 6-kun: Custom MIME + json() fast-path docs

**Ish:**
- `response(body, mime)` yoki `raw(body, mime)` builtin — `text()`/`html()`/`json()` ichidagi umumiy logikani common helperga koʻchirib.
- README ga va `docs/builtins.md` (agar yoʻq boʻlsa yangi) ga:
  - `json(Value::Str)` fast-path xatti-harakati (string passthrough, parse qilmaydi).
  - Cache pattern uchun foydali ekanligi.
  - Invalid JSON string ham passthrough boʻlishi haqida ogohlantirish.

**Acceptance:**
- `route GET /export.csv { return response(csv_body, "text/csv") }` ishlaydi.
- `Content-Type: text/csv; charset=utf-8` header toʻgʻri keladi.
- README da `json()` semantikasi yozilgan.

---

### 7-kun: `serve()` graceful shutdown

**Ish:**
- `tokio::signal::ctrl_c` (Unix) + `tokio::signal::windows::ctrl_c` (Windows).
- Axum `with_graceful_shutdown(...)` ulash.
- Inflight requestlar tugashi uchun timeout: default 5s, `JWC_SHUTDOWN_TIMEOUT` env override.
- WebSocket connectionlar ham toza yopiladi (close frame yuborib).
- Logga `"Shutdown signal received, draining N inflight requests..."` chiqarish.

**Acceptance:**
- Ctrl+C bosilganda yangi conn qabul qilinmaydi.
- Ochilgan requestlar tugaydi (5s ichida).
- 5s dan keyin majburiy yopiladi.
- WebSocket clientlar `1001` close code oladi.

---

### 8-kun: Parity audit

**Ish:**
- `examples/testapp` va `examples/microblog` ni toʻliq:
  - `jwc run` da ishga tushirib har bir routeni curl bilan tekshirish.
  - `jwc build --native` ga build qilib, hosil boʻlgan binar bilan xuddi shu testlarni qaytarish.
  - Outputlarni diff bilan solishtirish.
- `tests/integration_native.rs` — har bir builtin uchun smoke test:
  - Native binarni `Command::new` bilan ishga tushirib HTTP response tekshirish.
- Topilgan parity gaplari uchun GitHub issue ochish.
- Agar 1 kun ichida hal boʻlmasa — v0.4.1 ga koʻchirish, blockerlar yoʻq.

**Acceptance:**
- Ikkala example da `jwc run` va `jwc build --native` outputi identik.
- Yangi `integration_native` testlari yashil.

---

### 9-kun: Testlar + CHANGELOG + version bump

**Ish:**
- Yangi integratsiya testlari:
  - `tests/integration_array.rs` — literal, range, push, join.
  - `tests/integration_hash.rs` — sha256/sha1/md5/hmac/argon2.
  - `tests/integration_const.rs` — module-level const, error casesi.
  - `tests/integration_shutdown.rs` — SIGINT graceful.
- Version bump v0.3.15 → v0.4.0:
  - `Cargo.toml` (workspace + jwc + jwc-lsp).
  - `src/main.rs` `--version` output.
  - `vscode-extension/package.json`.
  - `ROADMAP.md` — Phase 3 (array literals), Phase 4 (parity) galochkalar.
- `CHANGELOG.md`:
  - `### Added`: array literal, range/push/join, sha256/sha1/md5/hmac, custom MIME, module-level const, graceful shutdown.
  - `### Changed`: builtin definitions single-source refactor.
  - `### Fixed`: native AOT whitelist parity (hash_password va boshqalar).

**Acceptance:**
- `cargo test` toʻliq yashil.
- `cargo test --test integration_db` Docker borida yashil.
- `jwc --version` v0.4.0 koʻrsatadi.

---

### 10-kun: Bufer / examples / release

**Ish:**
- Yangi imkoniyatlarni koʻrsatadigan example:
  - `examples/csv-export` — array literal + range + join + custom MIME bilan CSV endpoint.
  - `examples/hash-demo` — sha256/hmac/argon2 demonstratsiyasi.
- README quickstart yangilash — array literal misoli, hash builtins jadvali.
- `docs/builtins.md` — barcha builtinlar roʻyxati (single-source jadvaldan generatsiya qilinadi).
- Git tag `v0.4.0`, release notes.
- Binar build artifactlari (Windows, Linux, macOS) — `build.ps1`/`build.sh` orqali.

**Acceptance:**
- Yangi exampleslar `jwc run` va `jwc build --native` da ikkalasi ishlaydi.
- GitHub release sahifasi tayyor.

---

## Risklar va sprintdan tashqarida qolganlar

| Element | Sabab |
|---------|-------|
| LCG loop perf cliff (100k=6ns/iter, 5M=204ns/iter) | Interpreterda fundamental — JIT yoki real codegen kerak. Alohida tadqiqot. |
| Native AOT build vaqti (51s birinchi, 17s keyin) | axum + tokio + hyper + rustls dep daraxti chuqur. v0.5.0 da `--minimal` feature flag. |
| Toʻliq mutable global | Semantika qayta loyihalashni talab qiladi. `cache_*` workaround hozircha yetadi. |
| LLVM IR backend | ROADMAP Phase 4.1/4.2 — atayin deferred. |

---

## Branch strategiyasi

- Har bir kun (1–8) alohida PR — kichik, review qilinadigan.
- `feature/v0.4.0-array-builtins` integratsiya branchi, har bir PR shunga merge.
- 9-kunda integratsiya branchi `main` ga PR.
- 10-kunda tag va release.

---

## Bugun (1-kun) boshlash uchun checklist

- [ ] `git checkout -b feature/v0.4.0-array-builtins`
- [ ] `src/builtins.rs` yaratish, `BuiltinDef` struct yozish
- [ ] Mavjud builtinlar roʻyxatini `runner.rs` dan koʻchirib chiqish (taxminan 30-40 ta)
- [ ] `runner.rs::call_builtin` ni jadvaldan oʻqiydigan qilib qayta yozish
- [ ] `native_build.rs::BUILTINS` ni shu jadvaldan kelib chiqib generatsiya qilish
- [ ] `cargo test` yashil
- [ ] Kommit + PR
