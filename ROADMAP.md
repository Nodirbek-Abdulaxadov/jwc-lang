> ## 0.9.903 tuzatishi — native backend qaytdi
>
> Quyida `--native` haqida yozilgan har bir band **eskirgan**. `DEFERRED-2`
> qaytarib olindi: `jwc build` native binary chiqaradi va tilni qoplaydi
> (faqat `view` nomi bilan rad etiladi). `--native` degan bayroq yo'q va
> `E0910` hech qachon mavjud bo'lmagan.
>
> Muzlatishning sababi — "ikkinchi backend har bir query-compiler
> o'zgarishini ikki marta qildiradi" — 1.0 front-end'iga nisbatan
> to'g'ri emas: `query_sql` so'rovni kompilyatsiya vaqtida SQL matniga
> tushiradi, shuning uchun codegen query kompilyatorini *chaqiradi*,
> qaytadan yozmaydi.
>
> Ikkala backend jwc-shortener, MyWallet va task-tracker ustida bayt-bayt
> bir xil javob berishi bilan tekshiriladi.
>
> Shuningdek `DEFERRED-16` "0.9.x runtime kodi saqlanadi" degan edi —
> queue uchun bu **noto'g'ri**: v0.25.0 kesuvida o'chirilgan.
> `docs/spec/v1/DEFERRED.md` dagi tuzatishga qarang.

# JWC — v1.0.0 Roadmap (qayta loyihalangan til)

Bu hujjat eski `ROADMAP.md` ni **butunlay almashtiradi**. Eski reliz raqamlari
(v0.10.0–v0.14.0 rejasi), eski faza ro'yxati (Phase 0–11) va eski lug'at
(`entity` / `dbcontext` / `with` / `via` / nav-property / `validate body` /
`new X from Y` / `patch` / `group` / `mount` / `dome`) bekor qilindi.
Normativ manba — `DESIGN.md` va `saas/` namuna loyihasi.

---

## 0. Boshlang'ich holat — halol tavsif

`/home/user/jwc-lang` da **eski tilning** ishlaydigan kompilyatori bor:
~48 500 satr Rust, v0.9.6, Phase 0–11 yopilgan. Bu kod redizayn uchun
*asos emas* — u boshqa tilni kompilyatsiya qiladi.

Muhim yengillik: **ko'chiriladigan foydalanuvchi yo'q.** Deprecation davri
kerak emas, `--compat` bayrog'i kerak emas, ikki grammatikani bir vaqtda
qo'llab-quvvatlash kerak emas. Eski sintaksis bir relizda o'ladi.

### Qayta ishlatiladi (infratuzilma, til emas — ~40%)

| Modul | Nima uchun qoladi |
|---|---|
| `src/engine.rs` | deadpool-postgres pool, prepared-statement cache, TLS, transient-error klassifikatsiyasi, `with_tx` |
| `src/migrate.rs` (applier) | advisory lock (`MIGRATION_LOCK_KEY`), forward-only qo'llash, fayl runner |
| `src/server.rs` | axum + tokio skeleti, so'rov hayot sikli, `tokio::spawn` per-request |
| `src/lexer.rs` | tokenizatorning ~70%i (token to'plami o'zgaradi, mexanizm emas) |
| `config.rs`, `error_report.rs`, `error_codes.rs`, `jwt.rs`, `jwks.rs`, `redis_engine.rs`, `log_writer.rs`, `cors.rs` | runtime xizmatlari, tilga bog'liq emas |
| `project.rs` (loader), `resolver/`, `registry/` | manifest + paket yechish |
| CI, Docker, `install*.sh/ps1`, `templates/`, docs sayti | ops |

### Qayta yoziladi (tilga bog'liq — ~60%)

`ast.rs`, butun `parser/`, `typecheck.rs`, `lint.rs`, `sql.rs`,
`schema_diff.rs` (model qismi), butun `runner/`, `builtins.rs` registri,
`openapi.rs` + `swagger.rs`.

### O'chiriladi

Eski deklaratsiyalarning barcha kod yo'llari va ularning testlari
(`nested_with.rs`, `mutation_fields.rs`, `group_by.rs`, `insert_codegen.rs`,
`typed_catch.rs`, `query_differential.rs` va h.k.).

### Muzlatiladi

`src/native_build.rs` (5 149 satr) — pastdagi **Kechiktirilganlar** ga qarang.

---

## 1. North star va ikki qabul testi

> "Web backend yoz — CRUD'ni qo'lda yozmasdan, ORM bilan kurashmasdan,
> native-tez."

`DESIGN.md` ikki qabul testini beradi. 1.0 uchun ular **vibe emas, harness**:

**DBA test** — `jwc gen-sql --explain` har bir DDL statementni manba
fayl:satr ga bog'laydi; `tests/ddl_golden/` da har bir schema fayli uchun
qo'lda ko'rib chiqilgan `.sql` etaloni bor; chiqish bayt-ma-bayt mos kelishi
shart. Har bir reliz oxirida bitta tashqi Postgres muhandisi etalon
fayllarni ko'rmasdan schema faylini o'qiydi va DDL ni yozadi; farq — bug.

**Developer test** — `tests/sql_golden/` da namunadagi har bir query uchun
generatsiya qilingan SQL saqlanadi va qo'lda tasdiqlanadi; `EXPLAIN
(ANALYZE, BUFFERS)` seed'langan bazada indeks bor predikatlar uchun
`Seq Scan` bermasligi assert qilinadi.

---

## 2. Ketma-ketlik prinsiplari

Reliz tartibi beshta qoidaga bo'ysunadi.

1. **Qaror koddan oldin.** 44 tasdiqlangan bo'shliqning ko'pi — *yozilmagan
   semantika*, kamchilik emas. Spetsifikatsiya arzon va hamma narsani ochadi.
2. **Lug'at feature'dan oldin.** Yangi sintaksis birinchi relizda tugaydi;
   undan keyin hech narsa eski grammatikada yozilmaydi.
3. **Bitta implementatsiya.** `--native` muzlatiladi. Semantika 1.0 gacha
   harakatda ekan, ikkinchi backend har bir query-compiler o'zgarishini
   ikki marta yozishga majbur qiladi.
4. **`saas/` — ground truth.** Spetsifikatsiya namunani ifodalay olmasa,
   spetsifikatsiya noto'g'ri. Har reliz namunaning o'sib boruvchi qismini
   ishga tushiradi.
5. **Jim noto'g'ri javob yo'q.** Ikki ma'noli konstruksiya 1.0 da kompilyatsiya
   xatosi bo'ladi, "biz birini tanladik" emas. Feature keyin qo'shiladi;
   jim noto'g'ri javobni keyin tuzatib bo'lmaydi.

### Implementatsiya joylashuvi — rejadan chekinish

§0 "eski sintaksis bir relizda o'ladi" deydi. Amalda uni **o'sha yerda**
o'ldirish v0.21.0–v0.24.0 ni jo'natib bo'lmaydigan qiladi: yangi front-end
v0.25.0 gacha namunani ishga tushira olmaydi, ya'ni eski kodni v0.21.0 da
o'chirish to'rt reliz davomida hech narsa qilmaydigan kompilyator qoldiradi
va mavjud test to'plamini butunlay qizil qiladi.

Shuning uchun yangi til **`src/v1/`** daraxtida qurildi va `jwc v1 …`
buyruqlari orqali ochildi. Eski front-end o'z joyida qoldi, `cargo test`
yashil qoldi, va **kesish nuqtasi v0.25.0** bo'ldi.

**Bajarildi.** v0.25.0 da eski `parser/`, `runner/`, `sql.rs`,
`typecheck.rs`, `native_build.rs`, `server.rs`, `project.rs`,
`schema_diff.rs`, `migrate.rs`, paket menejeri va LSP o'chirildi;
`src/v1/` yuqoriga ko'chdi va `jwc v1 …` prefiksi yo'qoldi. Eski hujjatlar
— `docs/docs/`, `docs/spec/` ning v1 dan tashqari qismi va eski README —
`docs/archive-0.9/` ga arxivlandi: ular joylashtirilgan 0.9.x binarlari
nimani bajarishini tasvirlaydi, bu kompilyatorni emas.

O'chgan narsalardan qaytadiganlari: migratsiyalar — v0.26.0, LSP va
`jwc openapi` — v0.27.0, test freymvorki va paketlar — v0.28.0. Native AOT
`DEFERRED-2` bo'yicha 1.1 ga qoldirilgan.

Bu — joylashuv haqidagi qaror, semantika haqida emas: `src/v1/` eski
grammatikaning bironta konstruksiyasini qabul qilmaydi.

---

## 3. Relizlar

### v0.20.0 — **Spec** (kod deyarli yo'q)

**Maqsad:** har bir ochiq semantik savolga normativ javob yozish, toki
keyingi relizlar ixtiro qilmasin.

**Ichida:**
- `docs/spec/` ni noldan yozish: `grammar.ebnf`, `names.md` (nom yechish),
  `types.md` (`Raw | Record` panjarasi, `T?` propagatsiyasi),
  `queries.md`, `writes.md`, `routing.md`, `middleware.md`, `errors.md`,
  `schema.md`, `migrations.md`, `builtins.md`, `config.md`.
- **Error model qarori** ni normativ band sifatida yozish (§4).
- 44 tasdiqlangan + 12 yangi bo'shliqning (§5) har biri uchun: normativ band
  **yoki** aniq `DEFERRED` hukmi + sabab.
- `saas/` namunasini spetsifikatsiyaga moslab qayta yozish — jumladan
  hozirgi 6 ta ikki ma'noli `where` sayti, `AuthService.login` dagi 403→401,
  `RequireOrgAdmin` ning e'lon qilinmagan bog'liqligi, webhook TOCTOU.
- `spec-coverage.json`: namunadagi har bir konstruksiya → spec bandi.

**Tugadi =** `spec-coverage.json` da 0 ta `unspecified`; 56 bo'shliqning
har biri `spec/` da band raqamiga yoki `DEFERRED` jadvaliga havola qiladi;
qayta yozilgan namuna 3 kishi tomonidan o'qib chiqilgan va spec bandiga
tayanmagan bironta konstruksiya qolmagan.

**Holat: yopildi.** `docs/spec/v1/` da 13 ta normativ hujjat
(`grammar.ebnf` + 11 `.md` + `DEFERRED.md`). `check_sample.py` namunaning
125 konstruksiyasini tasniflaydi, har birini band raqamiga bog'laydi,
bandning mavjudligini tekshiradi va olib tashlangan lug'atni rad etadi —
`unspecified: 0`, `dangling_clauses: []`. Namuna 4 ta nuqsonidan
qutuldi (403→401, e'lon qilinmagan `context` bog'liqligi, webhook TOCTOU,
6 ta ikki ma'noli `where`).

Uchinchi qabul mezoni — "3 kishi o'qib chiqqan" — **bajarilmadi**: bu
tashqi ko'rib chiqish, kod emas. Namunani o'qiydigan uchta muhandis
topilgunga qadar ochiq qoladi.

---

### v0.21.0 — **Vocabulary** (buzuvchi o'zgarish shu yerda tugaydi)

**Maqsad:** yangi sintaksisning to'liq front-end'i; eski til o'ladi.

**Ichida:**
- Lexer token to'plami; `ast.rs` noldan; recursive-descent parser.
- Deklaratsiyalar: `database`, `schema`, `table`, `view`, `enum`, `class`,
  `service`, `middleware` + `after`, `routes`/`route`, `errorHandler`,
  `error`, `namespace`/`import`, top-level `function`, `test`.
- Grammatik hal qilinadigan bo'shliqlar (semantika keyin):
  - join alias: `left join App.auth.Accounts inviter on inviter.id == ... as one inviter` (#1)
  - typed path segment: `routes "/api/v1/orgs/{org_id: bigint}"` (#9, #20)
  - `as many` suffikslari: `orderby ... limit ...` (#5)
  - `delete ... as { } first` (#6)
  - response header: `return created(json(x)) with { "Location": ... }` (#10)
  - keyset pagination: `... page after @cursor size N max M` (#11, #40)
  - `was "old_name"` rename markeri (#26, #27)
  - `middleware RequireOrgMember(@org_id: bigint) requires RequireOrgMember` (#13, #37)
  - service signaturalari: `function invoices(org_id: bigint, status: InvoiceStatus?) -> InvoiceDetail` (#31)
  - `raises (...)` faqat paket eksport chegarasida (E12)
  - `server { }` config bloki (#39)
  - lambda **yo'q** — `line => ...` grammatikadan olib tashlanadi (#22)
- `jwc fmt` yangi grammatika uchun; round-trip idempotent.
- Eski kalit so'zlar uchun maxsus diagnostika: `E0900: 'entity' 1.0 da olib
  tashlangan — 'table Accounts of App.auth' ni ko'ring`, migratsiya yo'lisiz.

**Tugadi =** `jwc check --parse-only` namunaning 25 faylini 0 xato bilan
o'qiydi; `tests/parse_corpus/` grammatikaning har bir produksiyasini qamrab
oladi (qamrov skripti 100% talab qiladi); `jwc fmt` corpus'da idempotent;
eski grammatikaning 10 ta kalit so'zi uchun 10 ta `E0900` testi bor.

**Holat: yopildi.** `src/v1/` — token, lexer, ast, parser, fmt, diag
(~4 600 satr). `jwc v1 check` namunaning 21 faylini 0 xato bilan o'qiydi;
`tests/v1_parse_corpus.rs` 110 ta snippet bilan grammatikaning **har bir**
produksiyasini qamrab oladi va qamrov testi grammatikani o'qib tekshiradi —
yangi produksiya qo'shilsa va corpus'da bandi bo'lmasa, test yiqiladi.
`jwc v1 fmt` — AST'dan qayta chop etadi, ya'ni qat'iy nuqta konstruksiya
bo'yicha; namuna **formatlangan holda** commit qilingan, shuning uchun
layout regressiyasi namunada diff sifatida ko'rinadi. 10 ta `E0900` testi
bor.

Ikki chekinish, ikkalasi ham namunaning o'zi topdi:

- **Zaxiralangan so'z yo'q** (names §2.6). `route`, `server`, `size`,
  `max`, `check`, `key`, `text`, `date`, `int` — hammasi namunada oddiy
  ustun nomi, qoida nomi yoki builtin namespace sifatida uchraydi.
  Zaxiralangan so'zlar ro'yxati tilning **o'z misolini** taqiqlagan bo'lardi.
- **`except (a, b)`** qavs ichida (types §9.1). Spread vergul bilan
  ajratilgan obyekt literali ichida turadi, u yerda `except a, b` ni
  `except a` + keyingi band'dan ajratib bo'lmaydi.

Namunaning uchta so'rovi `orderby` ni `as { }` dan oldin yozgan edi;
parser E0501 bilan ushladi va ular tuzatildi.

---

### v0.22.0 — **Schema** — DDL va DBA testi

**Maqsad:** schema fayllardan to'liq, tartibli, deterministik DDL.

**Ichida:**
- Skalyar tip lug'ati (**N2**): `bigint`, `int`, `smallint`, `numeric(p,s)`,
  `varchar(n)`, `text`, `boolean`, `timestamptz`, `date`, `time`,
  `interval`, `uuid`, `jsonb`, `inet`, `bytea`, `T[]`. Har biri uchun
  Postgres tipi + JSON wire ko'rinishi jadvalda. **`numeric` majburiy** —
  billing tilida pul tipi bo'lmasligi qabul qilinmaydi.
- `identity` → `GENERATED BY DEFAULT AS IDENTITY` (`bigserial` emas), yozib
  qo'yiladi (**N10**).
- Constraint nomlash funksiyasi — versiyalangan, xabardan mustaqil,
  har doim DDL da ochiq yoziladi (#28, #30):
  `pk_<table>`, `uq_<table>__<cols>`, `uq_<table>__<cols>__<8hex(predicate)>`,
  `ck_<table>__<cols>__<n>`, `fk_<table>__<cols>`, `ix_<table>__<cols>[__<8hex>]`.
- Partial `unique` → `CREATE UNIQUE INDEX ... WHERE`, table CONSTRAINT emas;
  predikat kompilyatsiya vaqtida kanonlashtiriladi (`== null` → `IS NULL`,
  enum literal → fizik shakl) (#25).
- `on update now()` → generatsiya qilingan trigger + trigger funksiyasi
  (**N1**). Bu — to'rtinchi DDL obyekt sinfi; snapshot modeliga kiritiladi.
- `--- doc comment` → `COMMENT ON TABLE/COLUMN` (**N10**).
- Emissiya tartibi (#33): `CREATE SCHEMA` → enum type → table → **barcha FK
  alohida o'tishda** → index → trigger → comment. Cross-schema FK sikli
  (`auth → org → auth` namunada bor) shu bilan yechiladi.
- `private` ustunlar RAW yo'lidan chiqariladi: default query `row_to_json`
  emas, **ochiq ustun ro'yxati** emit qiladi (#35).
- `NOT NULL ADD COLUMN` siyosati: `default` yo'q va jadval bo'sh emas —
  generatsiya rad etiladi, expand/contract shakli taklif qilinadi (#23).
- `view` DDL **bu relizda emas** — u query compiler'ga bog'liq (0.25.0).

**Tugadi =** `tests/ddl_golden/` da 4 schema fayli uchun etalon `.sql`;
`jwc gen-sql` bayt-ma-bayt mos; chiqish bo'sh Postgres 16/17 ga xatosiz
qo'llanadi; `jwc gen-sql --explain` har bir statementga `file:line` beradi;
DBA protokoli bir marta o'tkazilgan va farqlar 0.

**Holat: yopildi (DBA auditidan tashqari).** `src/v1/{naming,model,ddl,
workspace}.rs`. `jwc v1 gen-sql` — offline, deterministik, 7 fazali
tartibda. `tests/ddl_golden/` da 5 ta etalon (4 ta fokusli holat + to'liq
namuna); `tests/v1_ddl_golden.rs` bayt-ma-bayt solishtiradi, ikki yurishning
bir xilligini, har bir statementning `file:line` iga egaligini va faza
tartibini tekshiradi. `tests/v1_schema_diagnostics.rs` — 24 test, har bir
qoida uchun bittadan.

**Haqiqiy Postgres 16.13 ga qo'llandi** (`JWC_V1_PG` bilan, skip emas):
beshta etalonning hammasi toza tushdi, keyin tekshirildi — `identity`
(`bigserial` emas, `nextval` yo'q), qisman unique indekslar ikkinchi faol
obunani bloklaydi va bekor qilingandan keyin ruxsat beradi, `on update
now()` triggeri ishlaydi, `check` buzilishi generatsiya qilingan nom bilan
qaytadi (errors §6.5 shunga tayanadi), pul ustunlari `numeric(14,2)`.

Postgres ikki xatoni topdi, ikkalasi ham tuzatildi:
- bir xil ustunlar ustidagi ikki indeks (btree + gin) bir xil nom olardi;
  endi metod nomga kiradi;
- jadval `check` ining tartib raqami ustun qoidalarini ham sanardi, ya'ni
  `minLength(...)` qo'shilishi mavjud constraint'ni qayta nomlardi.

Yangi qoida — `E0431`: `using gin` skalyar `varchar`/`text` ustida rad
etiladi (`gin_trgm_ops` kerak, JWC extension o'rnatmaydi).

**Ochiq qolgani:**
- ~~**`E0440`**~~ (`NOT NULL ADD COLUMN`) — v0.26.0 da differ bilan keldi.
- **DBA protokoli** — tashqi muhandis. v0.20.0 dagi "3 kishi o'qidi" bilan
  bir xil sababdan ochiq.
- **`view` DDL** — ataylab bu relizda emas (query compiler'ga bog'liq).

---

### v0.23.0 — **Types** — qiymat panjarasi, null va kirish qatlami

**Maqsad:** har bir qiymatga tip berish, toki `x.field` va `json(x)` ni
kompilyator hukm qila olsin.

**Ichida:**
- **`Raw | Record{fields}` panjarasi to'liq** (#17, #41): har bir qiymat
  ishlab chiqaruvchi uchun qoida — table projection, view select
  (view = nomlangan `as { }`, demak `Record`), builtin qaytimlari
  (`jwt.verify -> Record{sub, exp, iat}?`), `jsonb` ustun va `context.get`
  → `Raw`. Raw'ning maydonini o'qish — kompilyatsiya xatosi, va'da qilinganidek.
- **Raw kompozitsiyasi** (#11, #41): raw qiymat object literal maydoni
  bo'lishi mumkin; splice = **matn konkatenatsiyasi**, parse emas. Har bir
  query uchun diagnostika: `raw preserved` / `raw lost here: <konstruksiya>`.
- **`T?` birinchi darajali** (#19): `first : T -> T?`;
  `left join ... as one a` → `a : R?`; `inner join` va `as many` → null emas
  (bo'sh massiv); `count -> int`, `sum|min|max|avg -> T?`. Flow narrowing:
  `if (x == null) { return|throw }` davomida `x` toraytiriladi.
  `T?` maydonini narrowing'siz o'qish — xato; `json(x)` da `x : T?` — ham xato.
- **Nom yechish** (#2, #18, #34): query ichida qualifikatsiyasiz identifikator
  **faqat ustun**; local/parametrga murojaat sigil talab qiladi (`$org_id`).
  View query'lar alias binder oladi. O'tish davri uchun: ikkala tomon bir
  ustunga yechilsa — `E: 'org_id' is ambiguous` ikkala saytni nomlab.
- **Spread qoidalari** (#21, #36): `as <Class>` — validatsiya **va**
  proyeksiya (noma'lum kalitlar tushadi); `...` operandi statik `class`
  tipli bo'lishi shart (`...request.body()` — xato); yo'q maydon INSERT
  ustun ro'yxatidan va UPDATE SET dan tushadi (`=?` kabi); mos ustuni yo'q
  class maydoni — xato, `transient` markeri yoki `...req except password`
  bilan yechiladi.
- **Bo'sh spread** (#7): `set ...req` da hamma maydon yo'q bo'lsa —
  statement o'tkazib yuboriladi, `as { }` proyeksiyasi joriy qatorni
  qaytaradi (200, SQL syntax error emas).
- **Class validatsiyasi + 400 shartnomasi** (#32): `minItems` massivlar
  uchun ajratiladi; barcha xatolar yig'iladi (fail-fast emas); javob shakli
  qat'iy: `{"error":"validation_failed","fields":[{"path":"lines[2].quantity","rule":"min","limit":1,"message":"..."}]}`;
  default xabarlar lokalizatsiya qilinadi.
- **Service chegarasi tiplangan** (#31): parametrlar annotatsiya **majburiy**;
  return annotatsiyasi ixtiyoriy, lekin shakllar farq qilsa **majburiy**
  (`WebhookService.record_payment` shu holat).
- **Expression yadrosi** (**N3**): `+` ning uch xil yuklamasi
  (string konkat, son, `timestamptz + interval`) tiplar jadvali bilan
  ta'riflanadi; truthiness; taqqoslash qoidalari; `int` toshib ketishi
  (`quantity * unit_cents` yig'indisi) — kompilyatsiyada `numeric` ga
  ko'tarish yoki ochiq xato.
- **`bigint` wire ko'rinishi** (#42): bitta tanlov, raw va record yo'lida
  bir xil. Tanlov — **string**, JS iste'molchilari uchun.
- Lambda yo'q: `sum(req.lines, line => ...)` o'rniga `array.sum(req.lines, "quantity", "unit_cents")` yoki ochiq `for` + akkumulyator (#22).

**Tugadi =** `tests/type_corpus/` — har bir fayl `-- expect: E0xxx@line`
annotatsiyasi bilan; `jwc check` 100% mos keladi; namunadagi 6 ta ikki
ma'noli `where` ning hammasi rad etiladi; `9007199254740993` id raw va
record yo'lida bayt-bir xil chiqadi (test).

**Holat: yopildi (bitta mezon qisman).** `src/v1/{types,symbols,check}.rs`.
`jwc v1 check docs/spec/v1/sample` — 21 fayl, 0 xato, 0 ogohlantirish.
`tests/type_corpus/` — 15 ta holat fayli, umumiy `prelude.jwc` ustida.
Annotatsiya qatorning o'zida turadi (`-- expect: E0310`), `@line` emas:
raqam qator pozitsiyasidan kelib chiqadi, ya'ni fayl tahrirlanganda
annotatsiya siljimaydi. Moslik **ikki tomonlama qat'iy** — yetishmagan
diagnostika ham, kutilmagani ham testni yiqitadi; shuning uchun corpus
diagnostikaning *yo'qligini* ham xuddi borligi kabi mahkamlaydi.

**Ikki ma'noli `where` masalasi**: 6 ta sayt endi *rad etilmaydi* — ular
**yozib bo'lmaydigan** bo'ldi. `$` majburiy bo'lgani uchun query bandidagi
yalang'och nom faqat ustun bo'ladi (v0.21.0). `sigils.jwc` shu qarorni
mahkamlaydi: sigilsiz lokal `E0376`/`E0211` beradi, `where org_id == org_id`
esa `W0104` ("har doim rost").

Implementatsiya to'rtta bandni aniqlashtirishga majbur qildi, hammasi
spec'ga yozildi:
- **queries §6.1** — proyeksiyadagi yalang'och nom **haydovchi** binding
  ustuni. Aks holda joinli har bir query'da `id` ikki ma'noli bo'lardi.
- **queries §4.6** — join natijasining o'z `orderby`/`limit` i o'sha
  qo'shilgan jadvalga scoped.
- **types §6.4** — query bandi ichida nullable maydonga murojaat xato emas:
  bu SQL, NULL o'zi tarqaladi. `E0320` — koddagi qiymatlar haqida.
- **schema §3.1** — `private` qoidasi aniqlashtirildi. `#35` topgan narsa
  to'g'ri edi: login query'si private ustunni nomlaydi va nomlashi **kerak**
  (`hash.verify` ga hash kerak). Qoida qiymatni **o'qish** haqida emas,
  **chiqib ketishi** haqida: lokalga proyeksiya qilish mumkin, javobga
  berish — `E0410`. `view` da esa umuman mumkin emas.
- **errors §1.1** — har bir xato `message: text` tashiydi, e'lon qilinganmi
  yo'qmi.

**Qisman:** `9007199254740993` uchun **uchdan ikki** qism bor — wire
qoidasi (`bigint`/`numeric` → string) lattice'da va emitter'ning cast'ida
tekshiriladi va ikkovi mos kelishi test qilinadi. **Uchinchi qismi —
haqiqiy so'rovni ikkala yo'ldan o'tkazib baytlarni solishtirish — runtime
talab qiladi va v0.24.0 ga qoladi.**

---

### v0.24.0 — **Runtime** — routing, middleware, error model, bir jadvalli CRUD

**Maqsad:** namunaning join'siz qismi haqiqatan ishlaydi.

**Ichida:**
- **Routing** (#12): `(method, resolved_path)` dublikati — qattiq xato,
  ikkala saytni nomlab (last-wins hech qachon); literal segment parametr
  segmentidan ustun, to'liq soyalangan route — xato; bir URL slotidagi
  parametr hamma blokda bir xil nom va tipda bo'lishi shart.
- **Path parametr tiplari** (#9, #20): router parse qiladi va **middleware'dan
  oldin** 400 qaytaradi; `@org_id : bigint` haqiqiy tip; prefix ∪ suffix
  binder to'plami tekshiriladi.
- **Middleware** (#13, #14, #37): blok-darajadagi ro'yxat yozilish
  tartibida, keyin route-darajadagi ro'yxat (qo'shiladi, almashtirmaydi;
  takroriy nom — xato); `after` bloklar teskari tartibda; `after` **har
  qanday** javob uchun ishlaydi, jumladan short-circuit, va
  `response.status()` yuboriladigan statusni ko'radi; `middleware X(@org_id: bigint)`
  — har bir biriktirish saytida tekshiriladi; `requires` bilan e'lon
  qilingan bog'liqlik statik tekshiriladi.
- **Javob konstruktsiyasi** (#10): `with { }` header suffiksi barcha
  builder'larda; `response.set_header(...)` `after` ichida; takrorlanuvchi
  header'lar (`Set-Cookie`) alohida API.
- **`server { }` config bloki** (#39): `trusted_proxies`, `max_body_bytes`,
  `request_timeout`, `header_timeout`, `cors`, `tls`.
- **`request.client_ip()`** (#15, #39): `peer_ip()` har doim socket manzili;
  `client_ip()` — `trusted_proxies` e'lon qilinmagan bo'lsa peer manzili,
  XFF **e'tiborsiz**. `request.route()` — e'lon qilingan pattern
  (rate-limit kaliti kardinalligi uchun).
- **Body bufferi** (#16): bir marta chegaralangan buferga o'qiladi;
  `raw_body()` va `body() as T` — **bir xil buferning ikki ko'rinishi**;
  chegaradan oshsa middleware'dan oldin 413.
- **Error model — to'liq E1–E14** (§4). Bu relizning yarmi.
- Bir jadvalli `select`/`insert`/`update`/`delete`: `where`, `as { }`,
  `first`, `orderby`, `limit`, `delete ... as { } first` (#6);
  `update ... first` → `FOR UPDATE` sub-select (#43); `first` bilan
  `orderby` majburiy, agar WHERE unique/PK ga tushmasa (#43).
- `transaction { }` semantikasi yozib qo'yiladi: **xato → ROLLBACK,
  `return` → COMMIT**; `errorHandler` rollback'dan **keyin**, tranzaksiyadan
  tashqarida ishlaydi (G7).

**Tugadi =** `jwc serve` namunadagi `/api/v1/auth/*`, `/api/v1/me`,
`/api/v1/plans`, `/api/v1/webhooks/*` ni ishga tushiradi va
`tests/http_golden/` dagi 40+ so'rov-javob juftligi (status, header, body)
mos keladi; route-conflict corpus'i (12 holat) mos diagnostika beradi;
error model conformance corpus'i (E1–E14 uchun kamida 2 test) o'tadi.

**Holat: yopildi (bitta endpoint guruhidan tashqari).**
`src/v1/{wiring,sql,value,db,validate,exec,exec_call,serve}.rs`.

**Statik yarmi** — `src/v1/wiring.rs`: route jadvali, dublikat
`(method, path)` (E0710), slot kelishuvi (E0701), middleware zanjiri
(E0802/E0803/E0804/E0805), tiplangan `context` (E0820/E0821), raise-set
fixpoint'i, `after` bloki bo'sh raise-set (E0811), nested transaction
(E0620), yetib bo'lmaydigan arm (W1001), javob bermaydigan arm (E1011).
`tests/wiring_corpus/` — 5 fayl, `-- expect:` annotatsiyasi bilan.

**Runtime yarmi** — `jwc v1 serve` haqiqiy Postgres 16.13 ga qarshi ishlaydi.
`tests/v1_http_golden.rs` — **47 ta so'rov-javob juftligi**, `serve::handle`
ni to'g'ridan-to'g'ri chaqiradi (server chaqiradigan aynan o'sha funksiya),
ya'ni tartib, middleware, error model va SQL — hech biri stub emas. Soket
ham alohida tekshirildi: `curl` bilan 200 (`with { }` header'i bilan), 404,
400 (yaroqsiz path parametri) va 401 (middleware) olindi.

Uchta xato faqat haqiqiy ishga tushirishda chiqdi:
- **Parametr bog'lash** — `$1::bigint` Postgres'ga `$1` ni `bigint` deb
  ko'rsatadi va string'ni rad etadi. `($1::text)::bigint` — bitta bog'lash
  yo'li, hamma tip uchun.
- **`created(json(x))` kompozitsiya qilmasdi.** Javob endi **qiymat**
  (`Value::Response`), shuning uchun `json` 200 da quradi va `created` uni
  201 ga o'zgartiradi — o'rab olmaydi.
- **`jsonb` kalitlarni saralaydi.** Proyeksiya tartibi = JSON kalit
  tartibi, shuning uchun `json_build_object`; va `serde_json` ning default
  map'i ham saralangani uchun record yo'li proyeksiya tartibida qayta
  yig'iladi.

Bittasi validatsiyada: `min`/`max` o'nlik son ustida jimgina o'tib ketardi
(`"-1.00".parse::<i64>()` xato beradi). Endi o'nlik sifatida solishtiriladi
va chegara ham o'nlik bo'lishi mumkin.

**Ochiq qolgani:**
- ~~**`/api/v1/me/orgs`**~~ va ~~**`page`**~~ — ikkalasi ham v0.25.0 da
  yopildi. Namunaning **25 ta endpoint'ining hammasi** javob beradi.
- `E1`–`E14` ning `E3` (exhaustiveness) qismi amalda har doim qanoatlanadi,
  chunki 1.0 grammatikasida har bir e'lon qilingan xatoning default status'i
  bor (errors §4.3). Bu — dizayn, kamchilik emas.

---

### v0.25.0 — **Query compiler** — ENG KATTA BO'LAK (butun ishning ~28%i)

**Maqsad:** join'lar, view'lar, agregatlar, raw kuzatuvi, pagination.

Bu reliz yolg'iz o'zi qolgan har bir relizdan katta. Uni kichraytirib
ko'rsatish foydasiz — ichki bosqichlarga bo'linadi va har biri alohida
merge qilinadi.

**25.a — Join va nom qatlami**
- Alias binder (#1): alias — **yagona** binding; `Accounts.` hech qachon
  binding emas; self-join ishlaydi (`Invites.invited_by` + acceptor,
  `Events.actor_id` namunada bor).
- `from` oldidagi slot yakuniy hal qilinadi: root binding aliasi,
  view'lar uchun ham majburiy (#1, #18).
- **Join biriktirish daraxti** (**N12**): join qaysi binding'ga osilishi
  ochiq yoziladi. Hozir `OrgWithMembers` da `as one account`
  `as many members` ichiga faqat `Members.account_id` ga murojaat qilgani
  uchun tushadi — pozitsion va e'lon qilinmagan. 1.0 da: `on` ikki
  binding'ga murojaat qilsa yoki tartib noaniq bo'lsa — xato,
  `under <alias>` bilan yechiladi.
- `inner join` mavjudmi — javob: ha, va u `as one` uchun null emaslikni
  beradi (#3).

**25.b — Proyeksiya va kardinallik**
- `as one` + `left join`: `CASE WHEN <right pk> IS NULL THEN NULL ELSE
  json_build_object(...) END` — ya'ni **null obyekt**, null'lardan iborat
  obyekt emas; JWC tipi `R?` (#3).
- `as many`: lateral + `json_agg`; **`orderby` majburiy**, `limit`
  ixtiyoriy — ikkalasi ham lateral ichida, `json_agg(... ORDER BY ...)`
  emas (#5).
- Bola kolleksiyasini filtrlash join ustida: `left join Members m on ...
  where m.role == admin as many admins` (#8).

**25.c — Agregatlar**
- Bare join — uchinchi rejim sifatida **o'z kalit so'zi bilan** rasmiylashadi;
  `as many` bilan bir query'da aralashtirish — kompilyatsiya xatosi (#4).
- `count(distinct x)`, `avg`, `having` (**N6**) qo'shiladi.
- Agregat null'ligi tip qatlamiga ulanadi (`sum → T?`) (#19).

**25.d — View'lar**
- `CREATE VIEW` emissiyasi (0.22.0 dan qoldirilgan qism).
- **View — haqiqiy DB obyekti** deb yozib qo'yiladi (makro emas) (#44).
- **Ikki bosqichli pushdown** (#44): `many` bola `orderby`/`limit` bilan
  uchrashganda — CTE 1 haydovchi jadval kalitlarini `WHERE + ORDER BY +
  LIMIT` bilan tanlaydi, CTE 2 bolalarni faqat shu kalitlar ustidan
  LATERAL qiladi. Pushdown isbotlanmasa — kompilyatsiya xatosi, qayta
  yozish varianti ko'rsatilib.
- `orderby` nested `one` maydoni bo'yicha (`orderby org.name`) — ta'riflanadi:
  asosidagi join qilingan ustunga tushadi, JSON path emas (**N6**).

**25.e — Pagination va raw**
- Keyset pagination birinchi darajali (#11, #40):
  `orderby issued_at desc, id desc page after @cursor size 50 max 200`
  → `{data, next_cursor, has_more}` bitta raw payload sifatida; `max`
  kompilyatsiya vaqtida majburlanadi.
- Raw kuzatuvi query bo'yicha diagnostika beradi (0.23.0 dagi qoidalarning
  query tomoni) (#41).
- `where exists (...)` / `not exists (...)` (#8).
- **`raw` escape hatch**: parametrlangan xom SQL, faqat `Raw` qaytaradi,
  interpolyatsiya yo'q, **`view` ichida taqiqlanadi**, `jwc lint` da
  ko'rinadi. Bu — window function / CTE / recursive / full-text uchun
  yagona klapan.
- `jwc explain` ilgaklari (0.27.0 da UI oladi) (#29).

**Tugadi =** namunaning **barcha 25 endpoint'i** ishlaydi;
`tests/sql_golden/` da har bir query uchun qo'lda tasdiqlangan SQL;
`InvoiceDetail` ustidan `orderby+limit` bo'yicha `EXPLAIN` seed'langan
100k qatorli bazada `json_agg` ni faqat sahifa kalitlari uchun bajaradi
(plan assertioni); self-join corpus'i (6 holat) kompilyatsiya bo'ladi;
raw-tracking diagnostikasi namunadagi har bir query uchun to'g'ri
`preserved`/`lost` beradi.

**Holat: yopildi.** Besh bosqich, beshta alohida commit.
`src/v1/{query,query_sql,views,cursor}.rs` + `ddl.rs`, `check.rs`,
`exec.rs`, `sql.rs`.

**25.a** — join daraxti. Biriktirish endi ochiq: join natijasi `on` bandi
nomlagan (qo'shilayotgandan boshqa) binding'ga osiladi; ikki nomzod —
`E0510`, yechimi `under <nom>`. `under` alias'ni ham, join chiqaradigan
maydon nomini ham qabul qiladi. `as group` kalit so'zi qo'shildi: ilgari
`as` siz join "agregat uchun" degani edi, ya'ni "men agregat qilmoqchi
edim" bilan "proyeksiyani unutdim" bir xil matn edi (`E0535`). Join ustida
`where` — bola kolleksiyasini filtrlaydi, haydovchi qatorni emas.

**25.b** — emissiya. `as one` + `left join` → bola PK'si bo'yicha
`CASE WHEN … IS NULL THEN NULL` (null obyekt, null'lardan iborat obyekt
emas); `inner join` da guard yo'q. `as many` → `LEFT JOIN LATERAL`,
`where`/`orderby`/`limit` lateral ichida — `json_agg(… ORDER BY …)` emas:
u tartiblaydi, lekin chegaralay olmaydi, va yonma-yon ikki kolleksiya
bir-birining qatorlarini ko'paytiradi. `sql.rs::select` o'chirildi: bitta
yo'l qoldi. `tests/sql_golden/` — namunaning har bir query'si + fokusli
holatlar, SQL va bind parametrlari bilan muzlatilgan; jonli Postgres'da
esa golden ko'rmaydigan narsalar tekshiriladi (3 nota + 2 tag → 3 va 2,
6 va 6 emas).

**25.c** — agregatlar. `count`/`sum`/`min`/`max`/`avg`, `count.distinct`,
`FILTER (WHERE …)`, `having`. **`sum` kengayadi** (`int → bigint`,
`bigint`/`numeric` → `numeric`) va `avg` — `numeric?`: operand kengligini
saqlagan `sum` aynan sumni ma'noli qiladigan ma'lumotda toshib ketadi.
`W0502` — ikki bare join fan-out beradi, `count` ikkinchisining qatorlarini
ham sanaydi; yechimi `count.distinct`.

**25.d** — view'lar. `CREATE VIEW` chiqadi, view — haqiqiy DB obyekti.
Ustunlari — proyeksiyasi; skalyarlar Postgres tipini saqlaydi (aks holda
`where org_id == @org_id` sonni matn bilan solishtirardi), nested `one`
qo'shimcha `x__<maydon>` ustunlarini beradi — `orderby org.name` shularga
tushadi, JSON path'ga emas (N6). **Ikki bosqichli pushdown** (#44):
kolleksiyali relation ustidan chegaralangan sahifa avval kalitlarni oladi
(`WITH page AS MATERIALIZED`, **base jadval** ustidan — view ustidan
kalit olish uning lateral'larini baribir bajaradi). Isbotlanmasa —
`E0542`, jimgina O(table) plan emas. `EXPLAIN ANALYZE` bilan tasdiqlandi
va test o'z kontrolini olib yuradi: indekssiz tartibda rewrite'siz shakl
kolleksiyani 200 marta quradi, rewrite bilan 5 marta.

**25.e** — pagination va valve. Keyset kursor imzolanadi (`E1205` —
`cursor_secret` siz `page` yo'q): kursor — mijoz beradigan predikat, imzosiz
u hech kim tekshirmagan ikkinchi filtr bo'lardi. Konvert uch ustun bilan
qaytadi, chunki `items` Postgres bergan matn bo'lib yetib borishi kerak.
`exists` / `not exists`, `in (…)` va `in ($massiv)`. `raw()` — SQL literal
bo'lishi shart (`E0610`), `view` ichida taqiqlangan (`E0611`),
`jwc v1 explain` hammasini sanab beradi. `W0501` — chegarasiz kolleksiya.

Yo'l-yo'lakay ikkita runtime kamchiligi topildi: `set value = value + 1`
jarayonda hisoblanardi (endi SQL'da — aks holda avval o'qish kerak, ikki
chaqiruvchi esa bir xil sonni o'qiydi), va `==?` dagi yalang'och
`$2 IS NULL` Postgres'ga tip bermasdi.

---

### v0.26.0 — **Migrations** — snapshot, fazalar, e'lon qilingan rename

**Maqsad:** schema o'zgarishi ma'lumot yo'qotmasdan va rad etilmasdan qo'llanadi.

**Ichida:**
- Snapshot modeli barcha obyekt sinflari uchun: table, column, enum
  (`EnumSnapshot { name, schema, ordered_values }`),
  `UniqueSnapshot { columns, predicate }`, `IndexSnapshot { columns,
  predicate, unique }`, view (kompilyatsiya qilingan SQL + murojaat
  qilingan jadvallar to'plami), trigger, comment (#24, #25, #26).
- **Fazali emissiya** (#24): `DROP VIEW` teskari topologik tartibda →
  table DDL → `CREATE VIEW` topologik tartibda. Har doim drop-and-recreate;
  `CREATE OR REPLACE VIEW` xavfsizligini isbotlashga urinilmaydi.
- **E'lon qilingan rename** (#27): `created_at timestamptz was "createdAt"`
  yoki `jwc migrate rename <table>.<col> <new>`. Diff mos tipli
  `DropColumn+AddColumn` juftligini `--allow-destructive` siz **rad etadi**
  va gumon qilingan rename'ni chop etadi.
- **NOT NULL backfill** (#23): expand/contract ikki migratsiya shakli;
  unique/check/FK ko'targan ustun uchun hech qachon nol qiymat taxmin
  qilinmaydi; noma'lum tip uchun `DEFAULT NULL` o'rniga qattiq xato.
- **Enum evolyutsiyasi** (#26): append → `-- jwc:no-transaction` sarlavhali
  alohida faylda `ADD VALUE`; rename → `ALTER TYPE RENAME VALUE`, faqat
  manba darajasidagi `was` markeri bilan; `run_migration_file` ga
  `-- jwc:no-transaction` direktivasi o'rgatiladi (hozirgi
  `file_opens_transaction` teskari ishlaydi).
- `jwc migrate status` (qo'llangan / kutilayotgan / drift) va
  `jwc migrate verify` — `pg_constraint` / `pg_indexes` ni binary kutgan
  nomlar bilan solishtiradi (#28).
- Dev-rejim: `jwc serve` boot'da `information_schema` ni dasturga
  solishtiradi va yetishmayotgan ustunni nomlab startup xatosi beradi
  (PG 42703 ni o'rab 500 qaytarish o'rniga) (#33).

**Tugadi =** **round-trip property testi**: bo'sh bazadan boshlab N ta
tasodifiy schema tahriri (ustun qo'shish/o'chirish/rename, unique
predikatini o'zgartirish, enum label qo'shish, view proyeksiyasini
o'zgartirish) → `migrate up` ketma-ketligi → natijadagi baza tuzilishi
yangi `gen-sql` ni bo'sh bazaga qo'llash bilan **bir xil** (`pg_dump
--schema-only` normallashtirilgan solishtiruv). 200 ta tasodifiy ketma-ketlik
o'tadi; `varchar(20)→varchar(40)` on `Invoices.number` (view ostidagi ustun)
xatosiz qo'llanadi.

**Bajarildi.** Besh bosqich, har biri alohida commit: snapshot modeli
(`src/snapshot.rs`), differ (`src/diff.rs`, 29 ta operatsiya, 28 ta korpus
keysi), fazali emissiya va `down` (`src/migrate.rs`), applier
(`src/apply.rs` — `up`/`down`/`status`/`verify` + advisory lock), va
qabul testi. 200 ta ketma-ketlik ×8 tahrir Postgres 16.13 da 43 soniyada
o'tdi; 18 ta operatsiya sinfining hammasi haqiqatan ham chiqqani
tekshiriladi.

Yo'l-yo'lakay topilgan uchta narsa:

- **View ustida view umuman emissiya bo'lmasdi.** `views::attach` ustunlarni
  e'lon tartibida, `model.views` hali bo'sh holatda hisoblardi — tashqi view
  hech qanday diagnostika bermasdan yo'qolib ketardi, holbuki
  `ddl.rs::ordered_views` shu holat uchun butun boshli funksiya tutib
  turardi. Endi u ham qo'zg'almas nuqta.
- **`ddl.rs` endi snapshot tiplari ustidan yozadi.** `gen-sql` va
  `jwc migrate` bitta renderer'dan chiqadi, ya'ni bir xil obyekt uchun
  boshqacha DDL chiqarishi *konstruksiya bo'yicha* mumkin emas.
- Spetsifikatsiyada uchta qarama-qarshilik: §2 "olti sinf" deb yozib yetti
  tasini sanagan; §5.3 enum reorder'ni ikki abzats oralig'ida ham rad
  etgan ham "operatsiya bermaydi" degan; §4 view kommentini 7-fazaga
  qo'ygan, holbuki view 8-fazada yaratiladi. Uchalasi ham tuzatildi.

**Ochiq qolgani:**
- **`E0910`** — `--native` rad etish kodi. `jwc build` bu daraxtda yo'q
  (DEFERRED-2), shuning uchun rad etadigan narsa ham yo'q.

---

### v0.27.0 — **Tooling** — SQL ko'rinadigan bo'ladi

**Maqsad:** ikki qabul testini editor'da tekshirish mumkin bo'lsin.

**Ichida:**
- LSP: `select`/`insert`/`update` ustida **hover → generatsiya qilingan
  SQL** (`$n` placeholder'lar + join strategiyasi bilan) (#29);
  signature help va `.` completion (0.23.0 dagi tiplangan service
  chegarasidan kelib chiqadi) (#31); go-to-definition; diagnostikalar.
- `jwc explain <Service.function>` va
  `jwc explain --route "GET /api/v1/orgs/{org_id}/invoices"` — SQL + dev
  bazaga qarshi `EXPLAIN` (#29).
- `JWC_LOG_SQL=1` — `jwc serve` da so'rov bo'yicha SQL, bog'langan
  parametrlar, davomiylik, qator soni.
- `debug.dump(x)` — raw qiymatlarda ham ruxsat, faqat `jwc serve --dev` da (#29).
- `jwc lint --constraints` — route'dan yetib boriladigan har bir constraint
  va uning natijaviy status kodi; xabarsiz `unique`/FK uchun ogohlantirish (#30).
- `jwc openapi` — tiplangan service signaturalari + view proyeksiyalaridan (#31).

**Tugadi =** namunadagi 25 endpoint uchun `jwc explain` chiqishi
`tests/sql_golden/` bilan mos; LSP smoke testi hover-SQL ni tekshiradi;
`jwc openapi` chiqishi OpenAPI 3.1 validatoridan o'tadi;
`jwc lint --constraints` namunadagi xabarsiz unique'larni ko'rsatadi.
(Reliz yozilganda "5 ta" deb taxmin qilingan edi; v0.20.0 da namuna spec'ga
moslashtirilgandan keyin ular **3 ta** — `Sessions.token_hash`,
`ApiKeys.key_hash`, `Invites.token_hash` — va shulardan bittasigina bugun
route'dan yetib boriladi. Aynan shu uchtasi v0.29.0 ning hash mavzusi.)

**Bajarildi.** To'rt bosqich, har biri alohida commit: `jwc explain`
nishonlari + `JWC_LOG_SQL` + `debug.dump` (`docs/spec/v1/tooling.md` —
yangi normativ hujjat), `jwc lint --constraints`, `jwc openapi`, va til
serveri (`src/lsp.rs`). Barcha to'rt qabul mezoni testda:
`explain_and_the_sql_golden_are_the_same_compiler`,
`openapi_passes_a_real_validator` (haqiqiy `openapi-spec-validator`),
`tests/lsp.rs` dagi hover-SQL, va `lint_constraints_...`.

Yo'l-yo'lakay:

- **Route pattern'i ikki qatorning yopishtirilishi emas edi.** v0.24.0 dan
  beri `jwc explain` `/api/v1/authregister` va
  `/api/v1/orgs/{org_id: bigint}/invoices` deb yozib kelgan. Endi uchala
  chaqiruvchi ham bitta `wiring::route_pattern` dan o'tadi — bu
  `request.route()` javob beradigan qator (routing §5.4).
- **Annotatsiyasiz funksiyaning qaytish tipi endi e'lon qilinadi.**
  types.md §10.2 annotatsiyani faqat ikki `return` kelishmaganda talab
  qiladi, shuning uchun ko'pchilik funksiyada u yo'q va har bir chaqiruv
  joyi `Unknown` ko'rardi. `Checked` endi ularni chiqaradi; `jwc openapi`
  pass'ni ikki marta yuritadi. `jwc check` hamon bir marta.
- **`delete` hech qanday unique/check buzmaydi.** `wiring::promote` uni
  raise set'ga qo'shadi (u yerda ortiqcha baho xavfsiz), lekin
  `--constraints` aniq javobni beradi: `DELETE /orgs/{org_id}` →
  `fk_invoices__org_id`, 400.

**Ochiq qolgani:**
- **`E0910`** — hamon `jwc build` yo'q (DEFERRED-2).

---

### v0.28.0 — **Test framework va paketlar**

**Maqsad:** constraint xabarlari va paket chegarasi tekshiriladigan bo'lsin.

**Ichida:**
- `test` bloklari, `assert`, `assert fails { } with "<message>"` — **xabarni
  ham** tekshiradi, faqat "insert failed" emas (#28).
- **Test izolyatsiyasi modeli** (**N9**): har bir `test` o'z tranzaksiyasida
  ishlaydi va oxirida rollback qilinadi. Hozir namunadagi 3 ta test bir-birini
  buzadi: 1-test `Subscriptions` ga qator qo'shadi, 2-test o'sha org uchun
  faol obuna yaratishga urinadi va partial unique tufayli *birinchi* insert'da
  yiqiladi.
- `seed.*` — e'lon qilinadigan seed fixture'lari, sehrli global emas.
- **Paket kontent modeli** (**N8**): paket nima e'lon qila oladi (`service`,
  `middleware`, `class`, builtin namespace) va nima **yo'q** (`table`,
  `schema`, `routes` — ya'ni paketlar migratsiya keltirmaydi). `import redis;`
  paket importi sifatida rasmiylashadi.
- `raises (...)` paket eksport chegarasida — inferred to'plamning superset'i
  ekanligi tekshiriladi (E12).
- `jwc-registry` bilan integratsiya: `jwc publish` / `jwc add`.

**Tugadi =** `jwc test` namunaning `tests/billing_test.jwc` ni 3/3 yashil
ishga tushiradi va tartibdan qat'i nazar takrorlanadi (shuffle testi);
`assert fails ... with` noto'g'ri xabarda yiqiladi (negativ test);
`raises` narrowing urinishi kompilyatsiya xatosi beradi.

**Bajarildi.** Uch bosqich: `jwc test` + izolyatsiya + `assert fails … with`
(`docs/spec/v1/testing.md`), paket kontent modeli (`packages.md`, N8), va
registry klienti. Namunada 3 emas **4 ta** test bor va to'rttasi ham yashil;
teskari tartibda ham. Barcha uch mezon testda.

`seed.*` bu relizga kirmadi va kirmaydi: `DEFERRED-11` uni hali v0.20.0 da
hal qilgan — umumiy fixture modeli aynan N9 ko'rsatgan narsa, va namunaning
testlari o'z ma'lumotini o'zi quradigan qilib qayta yozilgan. Ro'yxatdagi
band o'sha qarordan oldin yozilgan.

Yo'l-yo'lakay topilganlar:

- **`transaction { }` tranzaksiya emas edi.** `exec::transaction` pool'dan
  ulanish olib unda `BEGIN` qilardi, ichidagi har bir statement esa
  `db.rs` orqali *boshqa* ulanishga tushardi. writes §7.1 v0.24.0 dan beri
  atomarlikni va'da qilib, uni bermay kelgan. 0.9.x engine'da shu uchun
  `TX_CONN` bor edi; v1 `db.rs` uni hech qachon o'qimagan. Endi o'qiydi, va
  test izolyatsiyasi ham xuddi shu mexanizm.
- **`assert fails` deyarli hech narsani tekshirmasdi** — berilgan xato
  tipini e'tiborsiz qoldirib, *nimadir* yiqilsa o'tib ketardi.
- **Namunaning o'zida ikki nuqson**: `Invoices` da qarama-qarshi yo'nalishli
  check yozilgan edi (test boshqa qoidani tekshirardi), va test
  `ConstraintViolation` kutardi — uni hech narsa ko'tarmaydi (errors §6.1).

---

### v0.29.0 — **Hardening**

**Maqsad:** xavfsizlik va ops bo'shliqlarini yopish.

**Ichida:**
- **Hash builtinlari ajratiladi** (#38): `hash.sha256`, `hash.hmac_sha256`
  (deterministik, indeks seek uchun), `hash.password`/`verify` (KDF, faqat
  tekshirish uchun), `crypto.constant_time_eq`. Namunaga accept-invite
  endpoint'i yoziladi — hozir `Sessions.token_hash` / `ApiKeys.key_hash` /
  `Invites.token_hash` ustidagi `unique` cheklovlari **ma'nosiz**, chunki
  yagona hash builtin har chaqiruvda boshqa digest beradi.
- `verify_signature` shu primitivlar ustida qayta yoziladi.
- Rate-limit kalitlari `request.route()` ga o'tadi (cheksiz Redis kaliti
  muammosi) va auth endpoint'lari IP + identity bo'yicha kalitlanadi (#39).
- Body limitlari, request/header timeout, CORS, TLS — `server { }` orqali
  soak ostida tekshiriladi.
- Threat model hujjati redizayn uchun yangilanadi; `cargo audit` / `deny`
  toza.
- Perf baseline: bombardier + Postgres, `/metrics`, 24h soak.

**Tugadi =** XFF spoof testi (trusted_proxies bo'sh) limiterni chetlab
o'tolmaydi; 413 body-limit testi middleware'gacha yetmaydi; timing testi
`login` ning ikki tarmog'i orasida statistik farq bermaydi (yoki farq
hujjatlashtiriladi); 24h soak'da RSS o'sishi < 5%, 0 ta pool leak.

**Bajarildi (soak'dan tashqari).** To'rt bosqich. Uchta mezon testda:
`serve::client_ip_tests` (besh xil XFF shakli hech narsani o'zgartirmaydi),
`an_oversized_body_never_reaches_the_chain` (413, va nazorat sifatida
limitdan past tanada 403), va
`the_two_failure_branches_of_login_cost_the_same`.

**24 soatlik soak** — quyidagi tuzatish o'tishida yurgizildi, qarang.

Yo'l-yo'lakay topilgan eng muhim narsa: **`login` da timing orakuli bor
edi.** Noma'lum email Argon2id'gacha yetmasdan qaytardi — **2.4ms**, ma'lum
email esa **415.8ms**. "Ikkala xato uchun bir xil xabar" ni soat butunlay
bekor qilardi. Endi ikkala tarmoq ham verify qiladi (nomaʼlumi decoy'ga
qarshi): 410.9ms va 414.8ms.

Boshqalar:

- **`hash.*` bo'linishi allaqachon bor edi** (v0.24.0 dan), lekin
  namunada hashlangan tokenni **qidiradigan** joy yo'q edi — endi
  `POST /api/v1/invites/accept` bor, va `Invites.token_hash` ustidagi
  `unique` shu tufayli ma'noga ega.
- **`server { }` ning ko'p kaliti o'qilib tashlab yuborilardi.**
  `request_timeout`, `shutdown_grace`, `cors { }` endi ishlaydi; `tls { }`
  va `header_timeout` esa **boot'da rad etiladi** — e'lon qilingan TLS
  ostida ochiq HTTP xizmat qilish operator ko'ra olmaydigan yagona
  noto'g'ri sozlama.
- **`cargo audit` dagi 7 ta advisory faqat dev-dependency'da**, va bu
  grafik haqidagi da'vo — `cargo audit` uni tekshira olmaydi, shuning uchun
  test tekshiradi.

#### Tuzatish o'tishi (2026-08-20)

v0.29.0 dan keyin ochiq qolgan bandlar bo'yicha. Ikkitasi yopildi, biri
ochiq qoladi (24 soatlik soak), va yo'l-yo'lakay ulardan kattarog'i
topildi.

**`tls { }` va `header_timeout` endi rad etmaydi — ishlaydi.** Ikkalasi
ham `axum::serve` ostida yashiringan edi, shuning uchun listener
`hyper-util` ning accept sikliga ko'chirildi: TLS `tokio-rustls` bilan
(ALPN `h2` + `http/1.1`), header muddati esa hyper'ning `http1` builder'ida.
Sertifikat boot'da o'qiladi — yechilmagan `tls { }` serverni to'xtatadi,
ochiq HTTP'ga qaytmaydi.

> Bu yerda **unit testlar ko'rmaydigan** nuqson chiqdi: `header_read_timeout`
> yonida `Timer` bo'lmasa, hyper **har bir HTTP/1 ulanishida** o'z poll'i
> ichida panic qiladi. Barcha unit testlar yashil edi. `tests/serve_listener.rs`
> haqiqiy soketga gapiradi va `TokioTimer` olib tashlansa yiqiladi.

**`server { bind }`** qo'shildi (config §3.2.1). Manzil umuman
sozlanmas edi — `0.0.0.0` yozib qo'yilgandi.

**`server { }` da xato yozilgan kalit jim edi** — endi `E1206`.
`init()` da bu `E1202` bilan yopilgan, `server { }` da esa hech narsa
yo'q edi: `trusted_proxie` proksi ro'yxatini bo'sh qoldirardi, ya'ni
`client_ip()` har so'rovda proksining o'z manzilini qaytarar va unga
kalitlangan rate limiter bitta umumiy paqirga aylanardi — aynan o'sha
kalit oldini olishi kerak bo'lgan nosozlik. `jwc check` toza o'tardi.

**`db::run_on` noto'g'ri proyeksiyani "qator yo'q" ga aylantirardi**
(`.unwrap_or(None)`): `Shape::First` → 404, `Shape::Rows` → `[]`, ikkalasi
ham bo'sh jadvaldan farq qilmaydi. Endi fault.

**Eng kattasi: testlarning bir qismi hech qachon yurmagan.** `tests/` dagi
7 ta suite (`hardening` ham!) CI'ning birorta ish o'rnida nomlanmagan, yana
4 tasi esa faqat **ma'lumotlar bazasisiz** yurgan — ular esa bazasiz
`SKIPPED` chop etib, `ok` qaytaradi. Bu muhitga Postgres 16 o'rnatilib
birinchi marta qaratilganda **7 suite'da 21 ta test yiqildi**:

| Suite | Yiqilgan | Sabab |
|---|---|---|
| `sql_golden` | 9 | psql'ga URI'dan keyin `-d` berilgan — psql uni yangi ulanish deb oladi va host/port/user'ni tashlab, standart unix soketga qaytadi |
| `migrate_apply` | 5 | bitta bazani bo'lishadi, mutex yo'q — biri `reset` qilar ekan ikkinchisi yarmida edi |
| `jwc_test` | 3 | shu sabab |
| `ddl_golden` | 1 | psql `-d` |
| `migrate_golden` | 1 | psql `-d`, va baza yaratishda `postgres` maintenance bazasi ko'rsatilmagan — CI'da foydalanuvchi `postgres` bo'lgani uchun tasodifan ishlagan |
| `migrate_roundtrip` | 1 | ikki test bitta `_rt_a`/`_rt_b` juftini bo'lishardi |
| `http_golden` | 1 | `answered == 25` deb yozib qo'yilgan, namunada esa 26 ta route — v0.29.a da qo'shilgan, va test o'sha kuni yurmagan |

Hammasi tuzatildi; psql yordamchisi `tests/common/mod.rs` ga chiqarildi.
CI endi har bir suite'ni nomlaydi va `hardening` ni **bazali** ham yuradi
(timing testi faqat shu yerda ma'noga ega). Takrorlanmasligi uchun
`every_test_suite_is_named_in_ci` — `tests/` ni `ci.yml` ga solishtiradi,
bu ham `no_triaged_advisory_crate_reaches_the_shipped_binary` bilan bir xil
shakl: repo haqidagi da'vo, repoga qarshi tekshiriladi.

**`redis.*` butunlay stub edi, va ulardan biri xavfsizlik nuqsoni.**
`builtins.md` §8 yetti nomni hujjatlashtiradi. Amalda: `redis.enabled()`
doim `false`, **`redis.rate_limit()` doim `true`**, qolgan beshtasi esa
umuman yo'q — tipdan o'tardi va har so'rovda `unknown function` fault'i
bilan yiqilardi. Ya'ni hujjatlashtirilgan API'ga qarshi yozilgan rate
limiter **hamma so'rovni o'tkazib yuborardi**, va javobda buni aytadigan
hech narsa yo'q edi.

Sababi bitta qatorda: `src/redis_engine.rs` da to'liq drayver bor —
`get/set/del/incr/expire/eval`, pool, retry, transient tasnifi — lekin
**uni hech kim boshlatmasdi.** v1 da `redis_engine::init_from_env()` ga
birorta chaqiruv yo'q edi, shuning uchun `--features redis` bilan
yig'ilgan va `JWC_REDIS_URL` qo'yilgan binarda ham `is_enabled()` `false`
qaytarardi. Butun drayver o'lik kod edi.

Endi `redis.*` drayverga ulangan (`exec_call.rs::redis_call`), boot'da
`serve` va `test` ikkalasida ham init qilinadi, va **server bo'lmasa
`enabled()` dan boshqa hammasi raise qiladi** — rate limiter uchun
"Redis yo'q" hech qachon "ruxsat" degani bo'lmasligi kerak.
`the_redis_package_surface_reaches_the_server` haqiqiy serverga qarshi
200/200/429/429 ni tekshiradi; stub'ni qaytarsam `limit = 2 did not bind`
deb yiqiladi.

Buning natijasi darhol ko'rindi: **namunaning `RateLimit` middleware'i ham
hech qachon cheklamagan.** U to'g'ri yozilgan — `redis.enabled()` bilan
o'ralmagan, chunki do'koni yo'q bo'lganda so'rovni o'tkazadigan limiter
limiter emas — lekin stub ostida har doim `true` olardi. v0.29.b ning
kalit qurilishi haqidagi testlari o'z o'rnida turadi (`client_ip` ni
to'g'ridan-to'g'ri tekshiradi), ammo middleware'ning o'zi ishlamasdi.
Endi `http_golden` va timing testi Redis talab qiladi — namuna Postgres'ni
qanday talab qilsa, shunday — va CI'ning Postgres ish o'rniga Redis
xizmati qo'shildi.

**`redis` paketi v0.25.0 cutover'idan beri kompilyatsiya bo'lmasdi.**
`//` izohlar (v1 da `--`), `public function`, `const`, `cache_*` — hech
biri 1.0 lug'atida yo'q, va repoda `jwc check` yurgizadigan CI yo'q edi.
Manifest nomi ham `jwc-redis` edi — `packages.md` §1 bo'yicha chiziqchali
nom `jwc publish` tomonidan rad etiladi. Paket v1 ga ko'chirildi: nomi
`redis`, va kodi yo'q — sirt kompilyatorniki, paket esa `import redis;`
ni yechadigan manifest (names §6.2.3).

Shu qatorda: **`spec-coverage.json` ni ham hech kim yangilamasdi.**
§10 uni "namuna spec'dan ortda qolsa build yiqiladi" mitigatsiyasi deb
sanaydi, lekin `check_sample.py` ni na CI, na test chaqirardi — fayl
oxirgi qo'lda yurgizilgan payt suratiga aylanib qolgan va namunadan
uzilgan edi. `the_spec_coverage_map_is_current` endi generatorni yurgizib
solishtiradi.

#### Soak: "yurmaydi" emas edi, harness ishlamasdi

Avval bu mezon "bu muhitda yurgizib bo'lmaydi" deb yopilgandi. Sabab
boshqa bo'lib chiqdi: **harness'ning o'zi hech qachon ishlamagan.** Beshta
nuqson, hammasi "bir marta ham yurgizilmagan" turkumidan:

| Nuqson | Oqibati |
|---|---|
| `--format=json` yolg'iz — bombardier intro va progress bar'ni ham shu stdout'ga yozadi | JSON emas; parser `char 0` da o'ladi |
| tayyorlik probe'i `curl --fail http://.../` | namunada `/` yo'q, ya'ni sog'lom jarayon 30s kutib "boot failure" deb yiqiladi |
| port band bo'lsa ham probe o'tardi | sikl **boshqa birovning** serverini o'lchaydi, RSS'ni bog'lanolmagan pid'dan oladi |
| `kill -TERM` himoyasiz, `set -e` ostida | oxirgi sikl yiqilsa, undan oldingi hamma soat chiqindiga ketadi |
| latency `percentiles` yo'q bilan | p50/p95/p99 doim 0.00 — abadiy tekis p99 |

Bundan tashqari `analyze.py` `pandas` talab qilib, u bo'lmasa `exit 2`
qilardi, va **pool mezonini umuman tekshirmasdi** — chunki tekshiradigan
raqam yo'q edi (yuqoriga qarang).

Tuzatilgandan keyin **yurgizildi**, namunaga qarshi, haqiqiy Postgres 16
va Redis 7 bilan, `/api/v1/plans` (bazaga tegadigan route) ustida:

```
8 sikl, har birida 75s yuk, orasida graceful restart
  480,051 so'rov      480,051 2xx      0 yo'qolgan
  RSS  18.9 → 19.5 MB      drift 3.2%   (chegara 10%)
  pool max waiting 0       oxirida available 42   (chegara: >0)
PASS
```

**Bu 24 soat emas — 8 sikl, ~12 daqiqa,** va shuni ochiq aytish kerak:
sekin o'sadigan sizish uchun bu qisqa. Lekin mezon endi **o'lchanmagan**
emas: harness ishlaydi, gauge'lar bor, `analyze.py` hukm chiqaradi, va
`soak.yml` o'sha buyruq bilan 72 siklni yurgizadi. `p99 drift 40%` —
0.05ms dan 0.07ms ga, ya'ni 20 mikrosekund; informatsion, va bu
bombardier build'i persentil bermagani uchun mean/max'ga tushadi.

#### `jwc-shortener`: o'lchandi, va bu port emas — qayta yozish

Avval "`jwc build` yo'qligi uchun bloklangan" deb aytilgandi. O'lchov
aniqroq javob beradi. v1 checker 873 qatorli manbada **801 xato** topadi;
izohlarni `--` ga o'tkazgandan keyin ham **616**, va ulardan 576 tasi —
`views.jwc` dagi ko'p qatorli `r"..."` ichidagi `'`. Qolganlari esa:
`E0900: 'dbcontext' was removed in 1.0`, `E0900: 'entity' ...`, `try/catch`,
`routes` bloki tashqarisidagi `route`, deklaratsiya sifatidagi `after`.

Ya'ni shortener boshidan oxirigacha **0.9.x dasturi**, va uchta narsa
bir vaqtda kerak:

1. **Ko'p qatorli literal 1.0 lug'atida yo'q** — names §2.3 satr ichida
   yangi qator taqiqlaydi, §2.4 esa `r"..."` ni *regulyar ifodalar uchun*
   deb belgilaydi. 484 qatorli landing sahifasining v1 da ifodasi yo'q.
   Bu til qarori, nuqson emas. (Diagnostika esa noto'g'ri edi: raw satrga
   ham "`\n` yozing" deb maslahat berardi — `r"..."` da `\n` teskari
   chiziq va `n`. Tuzatildi.)
2. `qr-lite` — chiziqchali nom, v1 da `import` qilib bo'lmaydi.
3. `--native` → `E0910` (`DEFERRED-2`).

Bu aynan ROADMAP'ning **v1.0.0-rc.1** dagi "pilot loyiha ko'chirish"
bandi, va u ishlab turgan xizmatni (1kb.uz) o'zi pin qilgan kompilyatordan
uzadi. Yo'l-yo'lakay qilinadigan ish emas — lekin endi taxmin emas,
o'lchov: 616 xato, uchta aniq to'siq.

**`TODO.md`** dagi to'rt dala nuqsoniga v1 hukmi berildi: ikkitasi
qayta yozuvda yopilgan (tekis e'lon fazosi; `E0732`), biri
konstruksiya bo'yicha ma'nosiz (har bir statement `::text` proyeksiya
qiladi), biri esa `bind` bilan yopildi.

---

### v1.0.0-rc.1 — **Freeze candidate**

**Maqsad:** sintaksisni muzlatishdan oldin ishonch.

**Ichida:**
- To'liq conformance corpus (parse + type + sql-golden + http-golden +
  ddl-golden + migration round-trip) CI'da bloklovchi.
- Tashqi audit: bir DBA (DBA testi), bir backend muhandisi (Developer testi),
  bir xavfsizlik ko'rigi.
- Pilot: `saas/` dan tashqari bitta haqiqiy loyihani ko'chirish va
  o'lchash — nechta konstruksiya spec'dan tashqarida ekan.
- `docs/` sayti yangi til uchun to'liq; eski hujjatlar arxivga.
- `CHANGELOG.md` 1.0 uchun tozalanadi; SEMVER siyosati yangilanadi.

**Tugadi =** pilot loyiha `raw` escape hatch'siz kompilyatsiya bo'ladi
(yoki har bir `raw` ishlatilishi kechiktirilgan feature'ga havola qiladi);
0 ta P0/P1 audit topilmasi ochiq; barcha corpus'lar yashil.

---

### v1.0.0 — **Syntax freeze**

Sintaksis muzlaydi. Buzuvchi o'zgarish faqat 2.0 da. Kechiktirilgan
feature'lar 1.1+ da, qo'shimcha sifatida.

---

## 4. Error model qarori (rejalashtirilgan band)

**Qaror:** avtomatik propagatsiya (`throw` + bitta `errorHandler`) qoladi,
lekin u endi tiplanmagan runtime mexanizmi emas — **tiplar e'lon qilinadi,
har bir funksiyaning raise-set'i statik call-graph ustidan inferred bo'ladi,
exhaustiveness bitta chegarada bir marta tekshiriladi.**

Sabab qisqacha: namunadagi 41 ta xato nuqtasidan **40 tasi so'rovni status
bilan tugatadi va hech bir chaquruvchi shoxlanmaydi**. Yagona haqiqiy
shoxlanish — webhook duplicate — va u allaqachon `return` bilan, xato
kanalisiz yozilgan. Errors-as-values 40 saytga soliq solib 1 taga xizmat
qiladi (~+140 satr, route'lar 34% o'sadi, `Route ichida mantiq yo'q`
qoidasi buziladi). Go lagerining eng kuchli e'tirozlari — `throw NotFund(...)`
jim 500 ga tushishi va catch-all bug yeyishi — **tiplashning yo'qligiga**
tegadi, propagatsiyaga emas. Shuning uchun tiplash qo'shiladi, mexanizm
almashtirilmaydi.

**Rejalashtirish:**
- **Qaror hujjati:** v0.20.0 (`docs/spec/errors.md`).
- **Grammatika** (`error` deklaratsiyasi, postfix `catch`, `or throw`,
  `raises`): v0.21.0.
- **E10 constraint promotion** ning nomlash asosi: v0.22.0.
- **E1–E9, E11, E13, E14 implementatsiyasi:** v0.24.0.
- **E2/E3 ning yozuv tomoni** (yozilgan jadvallarning constraint'lari raise
  to'plamiga qo'shilishi) query compiler yozuv statement'lari to'liq
  bo'lgach: v0.25.0.
- **E12 paket chegarasi:** v0.28.0.

**Qoidalar (v0.24.0 uchun qabul mezoni — har biriga kamida 2 test):**

| # | Qoida |
|---|---|
| E1 | `throw`/`catch` dagi har bir nom e'lon qilingan yoki built-in `error` ga yechiladi. Noma'lum nom — kompilyatsiya xatosi |
| E2 | Raise-set inferred: o'z `throw`lari ∪ `or throw`lari ∪ callee raise-set'lari ∪ yozilgan jadval constraint'lari − postfix `catch` yutgan tiplar. Bo'sh to'plamdan fixpoint |
| E3 | Barcha route/middleware/`after` raise-set'larining birlashmasi `errorHandler` arm'lari **yoki** built-in default status bilan qoplanishi shart |
| E4 | Tipsiz `catch (err)` faqat **fault**larni tutadi; e'lon qilingan xato uchun E3 ni qanoatlantirmaydi |
| E5 | Hech kim ko'tarmaydigan tip uchun arm — ogohlantirish "unreachable arm" |
| E6 | Har bir `errorHandler` arm'i javob bilan tugashi shart |
| E7 | `after { }` bloklarining raise-set'i **bo'sh** bo'lishi shart |
| E8 | Postfix `catch` bloki divergent bo'lishi shart (`return`/`throw`/`continue`/`break`) |
| E9 | `transaction { }` ichida postfix `catch` → `SAVEPOINT`/`RELEASE`/`ROLLBACK TO`. Majburiy: aks holda `25P02` |
| E10 | Xabarli constraint → e'lon qilingan xato (`unique`→`Conflict` 409, `check`→`BadRequest` 400, FK→`BadRequest` 400). Xabarsiz → **fault** → 500 + log |
| E11 | Body validatsiyasi `BadRequest` ko'taradi, `details` maydoni bilan; band-tashqi 400 yo'li yo'q |
| E12 | Paket chegarasidagi service `raises (...)` yozishi mumkin; inferred to'plamning superset'i ekanligi tekshiriladi. Ilova kodi yozolmaydi |
| E13 | Nested tranzaksiya **kompilyatsiya vaqtida** call-graph ustidan aniqlanadi (hozirgi runtime `bail!` emas) |
| E14 | Middleware `throw` qila oladi. `return <response>` faqat ataylab **xato bo'lmagan** javob uchun (redirect, 304, 202) |

Oldindan e'lon qilinganlar (default status bilan): `BadRequest` 400,
`Unauthorized` 401, `Forbidden` 403, `NotFound` 404, `Conflict` 409,
`TooManyRequests` 429, `ConstraintViolation` 400. Foydalanuvchi e'lon
qilgan xatoning default statusi **yo'q** — demak E3 arm talab qiladi.
Natija: namunaning 8 arm'li `errorHandler` i **butunlay o'chirilishi**
mumkin va ilova bir xil ishlaydi.

**Ochiq tuzatish (yangi topilgan, §5/N4):** `int(request.query("limit") ?? "50")`
— mijoz `?limit=abc` yuborsa, `int()` fault ko'taradi va E10/fault qoidasi
bo'yicha bu 500. Bu noto'g'ri: mijoz xatosi 400 bo'lishi kerak. v0.20.0 da
hal qilinadi: koersiya builtinlarining **kirish manbasiga** qarab
klassifikatsiyasi (`request.*` dan kelgan qiymat ustida `int()` →
`BadRequest`, ichki qiymat ustida → fault), yoki `int?()` ko'rinishidagi
ochiq nullable variant. Ikkisidan biri tanlanadi, uchinchisi yo'q.

---

## 5. Yangi topilgan bo'shliqlar

Bular 44 talik ro'yxatda ham, `DESIGN.md` ning "Known-invented" bandida ham
yo'q. Har biri namunadan dalil bilan.

| # | Bo'shliq | Dalil | Reliz |
|---|---|---|---|
| **N1** | `on update now()` — `DESIGN.md` schema bandida umuman yo'q, lekin namunada bor. Postgres'da `ON UPDATE` yo'q: bu trigger + trigger funksiyasi, ya'ni **to'rtinchi DDL obyekt sinfi** (view/enum/index dan tashqari), snapshot va diff talab qiladi | `src/db/billing.jwc:38` | 0.22.0, 0.26.0 |
| **N2** | Skalyar tip lug'ati **hech qayerda e'lon qilinmagan**. `inet`, `text[]`, `jsonb` namunada ishlatiladi va `DESIGN.md` da yo'q. `numeric`/`decimal` umuman yo'q — billing tilida pul `int` sentlarda, `uuid`/`date`/`interval` ham yo'q | `auth.jwc:23,37`, `audit.jwc:14`, `billing.jwc:19` | 0.22.0 |
| **N3** | Ifoda yadrosi ta'riflanmagan: `+` uch xil yuklangan (string konkat, son, `timestamptz + interval`); truthiness yo'q; `int` toshib ketishi yo'q | `ratelimit.jwc:9`, `billing.jwc:42`, `auth.jwc:36` | 0.23.0 |
| **N4** | Koersiya builtinlari mijoz kirishida — `int()` fault ko'taradi → 500, holbuki 400 kerak. Bu **qabul qilingan error model ichidagi** teshik | `routes/billing.jwc:44` | 0.20.0 qaror, 0.24.0 kod |
| **N5** | `import` semantikasi yo'q va u ikki xil narsani bir spelling bilan qiladi: namespace importi (`import db.org`) va **paket** importi (`import redis`). Amalda majburlanmaydi ham: `views/billing.jwc` `InvoiceStatus` ni ishlatadi, lekin `db.billing` ni import qilmaydi; `views/org.jwc` `Accounts` ga join qiladi, `db.auth` ni import qilmaydi. Bundan tashqari **erkin funksiyalar deklaratsiya saytiga ega emas**: `invite_body(token)` chaqiriladi, hech qayerda e'lon qilinmaydi | `views/billing.jwc:3,49`, `views/org.jwc:3,9`, `services/org.jwc:102`, `ratelimit.jwc:3` | 0.20.0, 0.21.0 |
| **N6** | Query lug'atida `having` yo'q (agregatni filtrlash imkonsiz), `in`/`like`/`between` yo'q (`?status=open,paid` yozib bo'lmaydi), va nested `one` maydoni bo'yicha `orderby` ta'riflanmagan | `services/auth.jwc:62` (`orderby org.name asc`) | 0.25.0 |
| **N7** | `after { }` ichidagi yalang'och `return;` — `return` ning ikkinchi ma'nosi ("bu bloknı to'xtat"), route'dagi "javob qaytar" dan farqli | `middleware/audit.jwc:14` | 0.21.0, 0.24.0 |
| **N8** | Paket kontent modeli yo'q: paket `table` e'lon qila oladimi? Agar ha — uning migratsiyalari qanday qo'shiladi? `jwcproj.json` `redis` ni deklaratsiya qiladi va u builtin namespace beradi | `jwcproj.json:6`, `ratelimit.jwc:3,11` | 0.28.0 |
| **N9** | Test izolyatsiya modeli yo'q. Namunaning 3 testi bir-birini buzadi: 1-test `Subscriptions` ga qator qo'shadi; 2-test o'sha `seed.org.id` uchun faol obuna yaratadi va partial unique tufayli **birinchi** insert'da yiqiladi | `tests/billing_test.jwc:8,19` | 0.28.0 |
| **N10** | `--- doc comment` → `COMMENT ON` — beshinchi drift qiladigan obyekt sinfi; `identity` ning fizik shakli (`GENERATED ... AS IDENTITY` vs `bigserial`) yozilmagan, DBA testi buni talab qiladi | `DESIGN.md:41,59` | 0.22.0 |
| **N11** | **`now()` vs `date.now()`.** `DESIGN.md` `date.now()` ni app soati, `default now()` ni Postgres soati deb ajratadi va "farq muhim (billing periods)" deb ogohlantiradi — namuna esa ilova kodida **6 marta yalang'och `now()`** chaqiradi va aynan billing davrlarini shundan hisoblaydi. `date.now()` namunada bir marta ham yo'q. Ya'ni ogohlantirilgan xato ground-truth'da sodir bo'lgan | `services/billing.jwc:41,57,93,144`, `services/org.jwc:91` | 0.20.0, 0.23.0 |
| **N12** | **Join biriktirish daraxti pozitsion va e'lon qilinmagan.** `OrgWithMembers` da `as one account` `as many members` ichiga tushadi — faqat `on` bandi `Members.account_id` ga murojaat qilgani uchun. `on` ikki binding'ga tegsa, biriktirish noaniq va proyeksiya daraxti aniqlanmaydi | `views/org.jwc:8-9` | 0.25.0 (25.a) |

---

## 6. 44 tasdiqlangan bo'shliq → reliz xaritasi

Har bir bo'shliq **v0.20.0 da yozma javob** oladi; jadval **implementatsiya**
relizini ko'rsatadi.

| # | Bo'shliq (qisqa) | Reliz |
|---|---|---|
| 1 | No join alias / self-join | 0.25.0 (25.a) |
| 2 | `where col == param` ambiguity | 0.23.0 |
| 3 | `left join ... as one` null shape | 0.25.0 (25.b) |
| 4 | Bare-join aggregates, `count(distinct)` | 0.25.0 (25.c) |
| 5 | No order/limit inside `as many` | 0.25.0 (25.b) |
| 6 | `delete` returns nothing | 0.21.0 + 0.24.0 |
| 7 | Empty `set ...req` | 0.23.0 |
| 8 | Filter parent by children / `exists` | 0.25.0 (25.b, 25.e) |
| 9 | Path params untyped | 0.21.0 + 0.24.0 |
| 10 | No response headers | 0.21.0 + 0.24.0 |
| 11 | Envelope pagination / raw composition | 0.23.0 + 0.25.0 (25.e) |
| 12 | Route conflicts / precedence | 0.24.0 |
| 13 | Middleware undeclared path-param dep | 0.24.0 |
| 14 | Middleware order / `use` composition / after | 0.24.0 |
| 15 | `client_ip()` proxy trust | 0.24.0 + 0.29.0 |
| 16 | Double body read / buffering | 0.24.0 |
| 17 | Raw-vs-record not total | 0.23.0 |
| 18 | View queries have no alias binder | 0.23.0 + 0.25.0 |
| 19 | `?` never propagates | 0.23.0 |
| 20 | Path param coercion → 500 | 0.24.0 |
| 21 | Spread absent-vs-null | 0.23.0 |
| 22 | `sum(xs, lambda)` / closures | **Non-goal** (§8) — `array.*` ga almashtiriladi, 0.23.0 |
| 23 | NOT NULL backfill | 0.22.0 + 0.26.0 |
| 24 | Views veto ALTERs / phases | 0.25.0 (25.d) + 0.26.0 |
| 25 | Partial index predicates in diff | 0.22.0 + 0.26.0 |
| 26 | Enum evolution | 0.26.0 (**qisman deferred** — §7) |
| 27 | Renames → data loss | 0.21.0 + 0.26.0 |
| 28 | Constraint name ↔ message coupling | 0.22.0 + 0.26.0 + 0.28.0 |
| 29 | Generated SQL invisible | 0.25.0 (ilgaklar) + 0.27.0 (UI) |
| 30 | Message-less constraints & FK → 500 | 0.22.0 + 0.24.0 + 0.27.0 |
| 31 | Untyped service params / returns | 0.21.0 + 0.23.0 + 0.27.0 |
| 32 | Validation 400 body / `minLength` overload | 0.23.0 |
| 33 | Cross-schema FK cycles / gen-sql order | 0.22.0 + 0.26.0 |
| 34 | Bare identifiers in `where` | 0.23.0 |
| 35 | `private` contradicted by projection/view | 0.22.0 + 0.23.0 |
| 36 | Spread whitelist preconditions | 0.23.0 |
| 37 | Block vs route `use` order | 0.24.0 |
| 38 | Hashed-token lookup impossible | 0.29.0 |
| 39 | No server config surface | 0.24.0 |
| 40 | No pagination primitive | 0.21.0 + 0.25.0 (25.e) |
| 41 | Raw boundary at composition | 0.23.0 + 0.25.0 |
| 42 | `bigint` fidelity raw vs record | 0.23.0 |
| 43 | `update ... first` locking / `first` order | 0.24.0 + 0.25.0 |
| 44 | `as many` aggregation before limit | 0.25.0 (25.d) |

---

## 7. 1.0 dan keyinga kechiktirilganlar

1.0 hamma bo'shliqni yopishga urinsa — chiqmaydi. Quyidagilar ataylab
qoldiriladi. Har birida "1.0 da nima bo'ladi" ustuni bor — chunki
kechiktirish **jim noto'g'ri javob** demak emas.

| Kechiktirilgan | 1.0 da nima bo'ladi | Sabab |
|---|---|---|
| **`--native` AOT backend** (hozirgi `native_build.rs`, 5 149 satr) | `jwc build` faqat launcher + runtime bundling; `--native` mavjud emas, `E0910` beradi | Semantika 1.0 gacha harakatda. Har bir query-compiler o'zgarishi ikkinchi implementatsiya + differential case talab qilardi. Interpretator — yagona reference. **1.1 da qaytadi**, eski kod ko'chirilmaydi, qayta yoziladi |
| **Background jobs, durable queue, DLQ, WebSocket, SSE** | Yo'q. Eski runtime kodi saqlanadi, ammo yangi til ularni e'lon qila olmaydi | `DESIGN.md` bu hududlarga umuman tegmaydi. Ularni yangi lug'atda qanday e'lon qilish — hech kim loyihalamagan dizayn ishi. 1.0 lug'atiga taxmin bilan qo'shish — ikki marta yozish |
| **Enum `reorder` / `DROP VALUE` rebuild** (#26 ning uchdan biri) | Qattiq xato + qo'lda retsept chop etiladi: yangi tip yarat → har bir ustunni `USING` bilan o'zgartir → eski tipni tashla, plus qolgan qatorlarni tekshiruvchi `SELECT count(*)` guard | To'rt statementli rebuild cross-schema ustun xaritasini talab qiladi. Xato + retsept ma'lumot yo'qotmaydi; noto'g'ri avtomatika yo'qotadi |
| **Har bir FK uchun maxsus xabar** (#30 ning yarmi) | FK buzilishi default status oladi (`BadRequest` 400) va `jwc lint --constraints` uni ko'rsatadi | Grammatikaga yana bir xabar sloti qo'shish arzon, lekin FK xabarining to'g'ri statusi (400 vs 409 vs 404) holatga bog'liq. Ma'lumot yig'ilsin |
| **Umumiy subquery / CTE / window function / recursive / full-text** | `where exists`/`not exists` bor; qolgani uchun **`raw` escape hatch** (parametrlangan, `view` ichida taqiqlangan, lint'da ko'rinadi) | Query compiler'ning eng katta bo'lagi allaqachon 28%. Escape hatch klapan bo'ladi va qaysi feature haqiqatan kerakligini o'lchaydi |
| **Bare-join aggregation + `as many` bir query'da** (#4 ning yarmi) | Kompilyatsiya xatosi, aniq diagnostika bilan | Ikkisining birgalikdagi semantikasi (lateral agregatga kiradimi, guruhlashdan omon qoladimi) haqiqiy dizayn savoli. Xato — to'g'ri javob; jim ko'paytirilgan `count` — emas |
| **Dev-only `/__jwc/queries` endpoint'i** (#29 ning bir qismi) | `JWC_LOG_SQL=1`, `jwc explain`, LSP hover-SQL bor | Uchtasi DBA/Developer testini qoplaydi. To'rtinchisi — qulaylik |
| **To'liq modul/visibility sistemasi** | `import` semantikasi yozib qo'yiladi (**N5**), namespace nomlash majburlanadi, lekin nom maydoni **flat** qoladi va `import` ko'rinishni cheklamaydi | Flat namespace + majburlanadigan `import` deklaratsiyasi 1.0 uchun yetadi. Haqiqiy visibility — 2.0 masalasi |
| **Tiplangan klient generatsiyasi (TS/Go/Python SDK)** | `jwc openapi` bor | OpenAPI — chegara. Har bir til uchun SDK — alohida loyiha |
| **Migration `down` ning to'liq avtomatik teskarisi** | `migrate down` bor, lekin destruktiv operatsiyalar uchun teskari skript **generatsiya qilinmaydi** — `-- irreversible` deb belgilanadi | Ustun tushirilgandan keyin ma'lumot yo'q. Teskarilikni va'da qilish — yolg'on |

---

## 8. Non-goals — redizayn uchun yangilangan

Bular kechiktirilgan emas: **siyosat darajasidagi "yo'q".** PR yopiladi.

| Band | Sabab |
|---|---|
| **LLVM IR backend** | Native AOT Rust-codegen orqali yetadi (1.1 da). LLVM yakka muhandis sig'imidan tashqarida |
| **Cross-target native matritsa** | Linux x86_64/aarch64 (glibc + musl) + Windows x86_64 yetadi |
| **Self-hosting compiler** | JWC kompilyatori Rust'da qoladi |
| **WASM target** | Backend tili |
| **Multi-database driver** (MySQL/SQLite/MSSQL/Oracle) | Postgres-first va'dasi. Butun query compiler Postgres dialektiga bog'langan (LATERAL, `json_agg`, partial index, `GENERATED AS IDENTITY`) — abstraksiya qatlami DBA testini o'ldiradi |
| **Rich-domain object graph, change-tracking, lazy loading, nav-property** | Maqsad — ORM'siz qolish |
| **Load-modify-save** (`select` → maydonni o'zgartir → `update`) | `DESIGN.md` da ataylab ifodalanmaydigan. Bu — raw fast-path va "yozuv to'plami ko'rinadi" qoidasining asosi |
| **Birinchi darajali funksiyalar, lambda, closure** | Namunadagi yagona ishlatilishi (`sum(xs, line => ...)`) `array.*` bilan almashtiriladi. Funksiya tipi → tip sistemasi + capture qoidalari + element-tip propagatsiyasi; "bitta amal bir satrda" uslubiga ham zid |
| **Generic / parametrik foydalanuvchi tiplari** | `T[]` tip konstruktori bor; qolgani yo'q |
| **Nested `routes` bloklari / prefix rewriting** | `DESIGN.md` ataylab rad etadi: to'liq yo'l literal yoziladi |
| **`as <Table>` natija bog'lash** | Loyihalangan va tashlangan — to'liq namuna ilovada 0 ta ishlatilish |
| **OTLP'ni yadro featuresi qilish** | `otlp` Cargo feature ortida, default-off |
| **Job priority queue / DLQ ML retry policy** | Over-engineering |

---

## 9. Hajm — halol baholash

Kompilyator ishi teng taqsimlanmagan. Query compiler qolgan har bir
relizdan katta va uni yashirish rejani yolg'onga aylantiradi.

| Reliz | Ulush | Izoh |
|---|---|---|
| 0.20.0 Spec | 5% | Kod deyarli yo'q, lekin kalendarda qisqa emas — 56 ta qaror |
| 0.21.0 Vocabulary | 8% | Lexer + parser + AST + fmt; mexanik, hajmli |
| 0.22.0 Schema | 10% | 5 ta DDL obyekt sinfi + nomlash + tartib |
| 0.23.0 Types | 12% | `Raw\|Record` panjarasi + `T?` propagatsiyasi + flow narrowing |
| **0.25.0 Query compiler** | **28%** | Alias/join daraxti, `one`/`many` lateral, agregat rejimlar, view kompilyatsiyasi + ikki bosqichli pushdown, raw kuzatuvi, keyset pagination |
| 0.24.0 Runtime | 12% | Yarmi — error model |
| 0.26.0 Migrations | 12% | Snapshot 5 obyekt sinfi + fazalar + rename + enum |
| 0.27.0 Tooling | 6% | |
| 0.28.0 Tests + paketlar | 4% | |
| 0.29.0 Hardening | 3% | |

Query compiler amalda 28% dan katta: 0.23.0 dagi tip qoidalarining
ko'pi (`first`/left-join/agregat null'ligi, raw klassifikatsiyasi)
query semantikasi haqida. Ikkovini birga hisoblasak — **~35%**.

Kod hajmi bo'yicha kutilma: ~30–35k satr yangi Rust (eski 48.5k dan
~19k infratuzilma qayta ishlatiladi, ~29k tashlanadi).

---

## 10. Risklar

| Risk | Yumshatish |
|---|---|
| **Query compiler o'z relizida tiqilib qoladi** | Ichki 5 bosqich (25.a–25.e) alohida merge qilinadi; har biri o'z golden SQL to'plami bilan. 25.a tugamasa 25.b boshlanmaydi |
| **Spec relizi cheksiz cho'ziladi** | Qat'iy chegara: 56 bo'shliqning har biri **javob yoki `DEFERRED`** oladi. "Keyinroq o'ylaymiz" — javob emas, `DEFERRED` — javob |
| **`--native` ni muzlatish 1.0 ni sekin qiladi degan e'tiroz** | To'g'ri, va qabul qilinadi. Interpretator 0.9.x da allaqachon tokio + async va o'lchangan; native'ning foydasi 1.0 ning bloklovchisi emas. 1.1 uni muzlatilgan, o'zgarmas semantika ustiga quradi — bu arzonroq |
| **Namuna ilova spec'ni ushlab turolmaydi** | `spec-coverage.json` CI'da; namunadagi spec bandiga bog'lanmagan konstruksiya build'ni yiqitadi |
| **Migratsiya round-trip testi juda sekin** | Property testi nightly'da 200 ketma-ketlik, PR'da 20 ta |
| **Tashqi DBA auditi topilma bermaydi (soxta ishonch)** | Har relizda **boshqa** muhandis; etalon fayllar ko'rsatilmaydi; farqlar CHANGELOG'ga yoziladi |

