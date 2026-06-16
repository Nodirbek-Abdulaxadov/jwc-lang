# JWC plan v3 (0.5.1'dan keyin)

**Tugagan (0.4.8 → 0.5.1):** canonical patterns (status-safe json, `update x in`,
schema-aware bind, partial PATCH, Bearer, unique+ALTER, DB-side paging + scalar
count), `with` eager-load (belongs-to + has-many + m2m → read-path N+1 = 0),
single-entity grouped aggregation, nav projection/ordering.

Bu plan — qolgani. Joy/craft trek — bosim yo'q. Qaysi epic qiziqarli bo'lsa,
o'shani ol; pastdagi tartib tavsiya, qonun emas.

**Mezon:** har epic task-tracker'dan bitta workaround o'chiradi yoki bitta halol
imkoniyat qo'shadi.

## Epic 1 — Query Layer'ni tugat (→ 0 raw_sql)

Qolgan yagona raw_sql — cross-table agregatsiya. Endi izolyatsiyalangan va aniq.

- **1a. Explicit multi-entity JOIN (A4)** — cross-table query poydevori.
- **1b. JOIN ustida grouped aggregation (B + A4 birga)** — `byColumn`
  (task→column.name), `byAssignee` (task_assignee→user).

O'chiradi: StatsService'ning oxirgi raw_sql'i. Marra: app'da **0 raw_sql**.
Eng qiyin, lekin eng katta narrativ qiymat — "ORM'siz, N+1'siz, 0 raw_sql"
to'liq rost bo'ladi.

## Epic 2 — Dinamik so'rovlar (in-code filter'ni o'chir)

- **2a. Dinamik/optional filter kompozitsiyasi (C1)** — shartli where.
- **2b. Dinamik-uzunlik `in (@list)` (C2)**.

O'chiradi: task ro'yxatidagi "filter-bor → in-code" shoxlanish. To'liq DB-side,
katta hajmda ham ishlaydi.

## Epic 3 — Data literal + halol OpenAPI (fasadni yop)

- **3a. Data-literal kengaytmasi (fundamental):** obyekt literalda string kalit +
  array literal. Ixtiyoriy JSON (map/massiv)ni JWC qiymati sifatida qurishni
  ochadi — OpenAPI'dan kattaroq ta'sir.
- **3b. OpenAPI generatsiya:** route + validate introspeksiyasidan
  `/openapi.json`. Qo'lda boqiladigan, driftga ketgan statik spec o'rniga.
  (3a uni ancha tozaroq qiladi.)

Natija: spec koddan tug'iladi — drift mumkin emas; "Swagger bor" da'vosi halol
bo'ladi.

## Epic 4 — Native AOT parity (ergonomik + tez pitching'ni qayta birlashtir)

Query Layer (nav eager-load, grouped agg, yangi formalar)ni native compilerga
keltir — `jwc build --native` real app'da ishlasin (hozir `compile_error`).

Natija: task-tracker native compile bo'ladi va Query Layer ishlatadi. "Ergonomik
va tez" qayta birlashadi. Eng katta, eng past shoshilinch — interpreter
ishlaydi, performance ikkilamchi pitch.

## Epic 5 — Kichik (parallel, arzon)

- Position/reorder primitivi (D2).
- Ikki-bosqichli nested `with` (test/support).
- (past) atomik `update CTX.Table set col = @param` paramref.

## Tavsiya etilgan tartib

1. **Epic 3a + Epic 5** — fundamental + arzon; data literal ko'p narsani ochadi,
   tez g'alaba.
2. **Epic 1** — sarlavha marra ("0 raw_sql"); eng qiyin, eng qoniqarli.
3. **Epic 2** — oxirgi DX dog'i (filter).
4. **Epic 3b** — data literal kelgach OpenAPI generatsiya; fasadni halol yop.
5. **Epic 4** — katta strategik birlashma, tayyor bo'lganda.

> Lekin bu joy trek: qaysi epic seni qiziqtirsa, o'shandan boshla. Agar
> "0 raw_sql" (Epic 1) eng qoniqarli marra bo'lsa — undan boshlasang ham to'g'ri.
