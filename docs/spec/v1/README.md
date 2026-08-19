# JWC v1 — redesign

Bu katalog **kelajakdagi** tilni tavsiflaydi. Bu yerdagi hech narsa hozirgi
kompilyator bilan kompilyatsiya qilinmaydi va qilinishi ham kutilmaydi.

Hozirgi til (`entity` / `dbcontext` / `with` / `via` / `validate body`) v0.9.x
da ishlaydi va `docs/` ning qolgan qismida hujjatlangan. v1 uni almashtiradi.

| Fayl | Nima |
|---|---|
| [`design.md`](./design.md) | Normativ dizayn qarorlari — lug'at, sintaksis, semantika |
| [`gaps.md`](./gaps.md) | 44 tasdiqlangan bo'shliq (138 topilmadan, adversarial tekshiruvdan keyin) |
| [`error-model.md`](./error-model.md) | Xato modeli tahlili va qarori |
| [`sample/`](./sample/) | ~1100 qatorlik namuna loyiha — 4 sxema, 25 endpoint |

Reja: [`ROADMAP.md`](../../../ROADMAP.md).

## Namuna loyihasi haqida ogohlantirish

`sample/` — dizaynning yagona to'liq ishlatilishi, lekin u hali
spetsifikatsiyaga moslanmagan. ROADMAP'ning `v0.20.0 Spec` relizida u qayta
yoziladi. Ma'lum nuqsonlari:

- `AuthService.login` yomon parolga 403 qaytaradi, 401 bo'lishi kerak
- `RequireOrgAdmin` o'zi e'lon qilmagan `context` kalitiga bog'liq
- `WebhookService.record_payment` da select-keyin-insert poygasi bor —
  bir vaqtda kelgan ikki yetkazib berish 400 hosil qiladi va Stripe uni
  "qayta yubor" deb o'qiydi
- oltita `where` sayti ustun/lokal nom to'qnashuvi bo'yicha ikki ma'noli

Ular ataylab tuzatilmagan: `gaps.md` shu nuqsonlar orqali topilgan.
