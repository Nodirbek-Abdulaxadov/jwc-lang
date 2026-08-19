# saas

Obuna/billing platformasi — JWC tili uchun ideologiya namunasi.
**Kompilyatsiya qilinmaydi**: bu til dizayni uchun yozilgan namuna loyiha.

## Tuzilma

```
jwcproj.json           manifest
.env.example           muhit o'zgaruvchilari
src/
  app.jwc              database, schema'lar, init(), main()
  db/                  jadvallar — sxema bo'yicha bir fayl
    auth.jwc           accounts, sessions, api_keys
    org.jwc            orgs, members, invites
    billing.jwc        plans, subscriptions, invoices, lines, payments
    audit.jwc          events
  views/               chiquvchi shakllar
    org.jwc
    billing.jwc
  dto/                 kiruvchi shakllar
    auth.jwc
    org.jwc
    billing.jwc
  middleware/
    auth.jwc           RequireAuth, RequireOrgMember, RequireOrgAdmin
    ratelimit.jwc      RateLimit, StrictRateLimit, VerifySignature
    audit.jwc          Audit (after bloki)
  services/            domen amallari — route'lar shularni chaqiradi
    auth.jwc           AuthService
    org.jwc            OrgService
    billing.jwc        BillingService, WebhookService
  routes/
    auth.jwc
    orgs.jwc
    billing.jwc
    webhooks.jwc
migrations/            `jwc migrate new` chiqaradi
tests/
```

## To'rt sxema

| sxema | nima uchun |
|---|---|
| `auth` | shaxs — akkaunt, sessiya, API kalit |
| `org` | tashkilot — a'zolik, taklifnoma |
| `billing` | pul — tarif, obuna, hisob, to'lov |
| `audit` | o'zgarishlar jurnali |

Sxemalar chegara: `billing` `auth` ga faqat FK orqali tegadi, aksincha emas.

## Dizayn qoidalari

- **`of <database>`** = bazadagi obyekt. `table`, `view`, `enum` — hammasi.
  `of` siz enum — CHECK bilan matn; `of` bilan — `CREATE TYPE`.
- **Yo'l hech qachon yig'ilmaydi.** `routes "..."` prefiksi + `route` qo'shimchasi,
  ikki bo'lak, uchinchisi yo'q.
- **Proyeksiya so'rovda.** `as { ... }` — SELECT ro'yxati. Mapper yo'q.
- **`class` faqat kirish, `view` faqat chiqish.**
- **`private` / `server`** — mass-assignment til darajasida yopiladi.
- Raw default; `as` yozilsa record. Maydon o'qish raw'da kompilyatsiya xatosi.
- **Route ichida mantiq yo'q.** Route = middleware + service chaqiruvi + `json(...)`.
  Domen mantiqi `service` da, tekshiruvlar cheklovlarda.
- **Cheklovda xabar bo'lsa** (`unique (...) : "..."`) buzilish 400 ga aylanadi —
  qo'lda "band emasmi" deb tekshirish kerak emas.
- **Servislar HTTP bilmaydi.** Ular `throw NotFound(...)` qiladi,
  `errorHandler` esa statusga o'giradi.
