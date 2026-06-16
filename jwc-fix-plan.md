# JWC tuzatish plani (pain log asosida)

> **Bajarilish holati (2026-06-16, branch `jwc-fixes`, uncommitted):**
> ✅ **Phase 0** — 0.1 `status` reserved key (→ `__jwc_status__` sentinel), 0.2 `jwt_verify` Bearer-strip, 0.3 DB-level `unique` (parser+AST+gen-sql+schema_diff round-trip), 0.4 kanonik test to'plami (unit + live), 0.5 docs (`for x in xs`, entities.md syntaksisi).
> ✅ **Phase 1** — 1A Value modeli birlashtirildi (`select…first`=Record endi `update x in`/entity-return/typed-param tomonidan qabul qilinadi), 1B schema-aware binding (datetime→varchar, jsonb-obyekt), 1C partial/PATCH (typed class param yo'q maydonni majburlamaydi).
> ✅ **Phase 2d** — `limit`/`offset` dinamik qiymat bug'i (SQL-compile cache to'qnashuvi) → bound parametr.
> ✅ **Phase 3** — `query_param` yo'q bo'lsa `""` (null emas).
> ⏳ **Qoldi (katta Query Layer epic — ROADMAP Phase 11, 1.0-blocker):** 2a join/relation loading (`with`/m2m), 2b agregatsiya + arbitrary-shape projection (`count`/`group by`), 2c dinamik/optional filter + `in (<list>)`. Phase 3 position/reorder primitivi ham qoldi. Bular alohida dizayn talab qiladi; yarim-implement qilinmadi.
>
> Tasdiq: 341 unit test yashil; `jwc-canon-test` app'da barcha fixlar live tasdiqlandi; `task-tracker` app patched jwc'da regressiyasiz ishladi.


**Asos:** simptomlarni emas, **ildiz-sabablarni** tuzatamiz. Sening o'z analizing to'g'ri edi — bir nechta xato bitta ildizdan: shularni jamlaymiz, shunda bitta fix ko'p simptomni o'ldiradi.

**Bosh tamoyil — har fix uchun regression test.** Eng chuqur topilma: JWC'ning *kanonik, hujjatlangan* pattern'lari (testapp `updateCar`) hech sinalmagani uchun 0.4.8 buzuq happy-path bilan chiqdi. Demak eng muhim tizimli tuzatish — **kanonik pattern'lar test to'plamini qurish** (har documented misol bir test). Shusiz bu xatolar yana qaytadi.

---

## Phase 0 — Tez g'alabalar (arzon, izolyatsiya, ishonch-tiklovchi)

Eng noqulay/jimgina xatolarni darhol to'xtatadi. Har biri kichik, lokal fix.

| # | Tuzatish | Pain log |
|---|---|---|
| 0.1 | **`status` reserved key** — `json({...})` body kalitini HTTP status deb olmasin. Status'ni alohida API orqali ber (`json(body, status: 201)` yoki `.status()`), body kalitlari **muqaddas** bo'lsin. `status` maydonli test qo'sh. | #1, #3 (P0) |
| 0.2 | **`jwt_verify` Bearer strip** — ixtiyoriy `Bearer ` prefiksini avtomatik olib tashlasin. Auth template/middleware'ni ham tuzat. | #3 (P0), #7 |
| 0.3 | **DB-darajali `unique`** — entity column'da `unique` deklaratsiyasi → migration'da unique constraint (TOCTOU'ni yo'q qiladi). | #9 |
| 0.4 | **Kanonik-pattern test to'plami** — testapp pattern'larini (CRUD, `update x in`, entity-return, m2m, agregatsiya) test sifatida yoz. Keyingi fazalar shu testlarni yashilga aylantiradi. | (ildiz) |
| 0.5 | **Docs'ni 0.4.8'ga moslash** — `for x in xs` (paren'siz), `nullable` keyword, va agregatsiya misollarini: **yo implement, yo docs'dan olib tashla**. "Docs-ahead-of-impl"ни yop. | 🟢 docs |

---

## Phase 1 — Ikki ildiz-fix (chuqur, lekin katta emas; ko'p simptomni yopadi)

### A. Value modelini birlashtir (Str↔Object) — *senning #2 topilmang*
**Ildiz:** `select...first` → `Object`, lekin entity-return / `update·insert·delete <var>` / typed-class-param `Value::Str` (JSON-string) kutadi.
**Fix:** bu sink'larning hammasi **bitta** value tasviri (Object) bilan gaplashsin; chegaralarда normalize qil, JSON-string talab qilma.
**Natija:** kanonik `let x = select...; x.f=..; update x in` **ishlaydi**; entity-return va typed-param `select` natijasini qabul qiladi.
**Yopadi:** P0 #2; Phase 2 #1 (`update x in`); Phase 3 #1 (`update x in`).

### B. Schema-aware parameter binding — *senning #4/#4b topilmang*
**Ildiz:** binder qiymat-shakliga qaraydi, ustun tipiga emas (datetime varcharga, jsonb obyektга bind muammosi).
**Fix:** param bind qilishда maqsad **ustunning deklaratsiya qilingan tipini** (entity/migration metadata) oqib, **o'shanga** bind/coerce qil (varchar→text, jsonb→json, datetime→timestamp).
**Natija:** datetime ham, jsonb-obyekt ham ishlaydi.
**Yopadi:** P0 #4, #4b.
**Test:** varchar'da ISO-sana; jsonb'да obyekt qiymat.

### C. Typed param — partial/PATCH semantikasi
**Ildiz:** typed-class-param DTO'ning BARCHA maydonini majburlaydi → PATCH buzuq.
**Fix:** optional/partial maydon semantikasi (`Partial<T>` yoki optional field) — yetishmayotgan maydon xato bermasin.
**Yopadi:** P1 #6.

---

## Phase 2 — Query Layer (katta strategik investitsiya — "ORM'дan qochish"ни rost qiladi)

Eng katta bo'lak. Monolit emas, sub-qadamlarga bo'l:

- **2a. Join / relation loading (`with`, m2m bilan).** Entity'ni bog'liqlari bilan bitta query'да yukla (1-N va N-N join-jadval orqali). **Yopadi:** m2m o'qish (task+label+assignee), nested load (project→…→task), denormalizatsiya workaround'i, N+1. *(pain #1, #2)*
- **2b. Agregatsiya + arbitrary-shape projection.** `count`/`sum`/`group by` + `select { status, total: count(*) } ... group by` (hozir parser rad etadi). **Yopadi:** barcha stats (raw_sql'siz), pagination uchun `count(*)`. *(pain #4 P0)*
- **2c. Dinamik / optional filter + `in (<list>)`.** Shartли where kompozitsiyasi + list bind. **Yopadi:** code'dagi filter+pagination, m2m `where id in (list)`. *(pain #5 P1)*
- **2d. Ishonchli `limit`/`offset` (dinamik qiymat bilan).** offset-ignored bug'ni tuzat; 2b'dagi `count(*)` bilan to'liq DB-side pagination. **Yopadi:** fetch+slice workaround'i. *(pain #10 P1)*

---

## Phase 3 — Kichik ergonomika
- **Position/reorder helper** — fractional indexing yoki reorder primitivi (qo'shnilarни qayta raqamlash). *(pain #8 / #7)*
- Qolgan mayda DX nuqtalari (`query_param` null coerce, h.k.)

---

## Ketma-ketlik (xulosa)
1. **Phase 0** — arzon, ishonch-tiklovchi (kunlar). Til *ishonchli* bo'ladi, kanonik test to'plami quriladi.
2. **Phase 1** — ikki ildiz-fix + partial. Buzuq kanonik pattern va binding to'g'rilanadi (katta emas, ta'siri katta).
3. **Phase 2** — Query Layer. Aynan u N+1/raw_sql'ni yo'q qiladi va "ORM'дan qochish" va'dasini rost qiladi.
4. **Phase 3** — qolgan ergonomika.

> Tartib mantig'i: avval **qonni to'xtat** (jimgina/uyat xatolar) → keyin **ildizni davola** → keyin **katta yangi imkoniyat**. Har qadam — test bilan.
