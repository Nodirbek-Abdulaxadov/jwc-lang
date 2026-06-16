# JWC Query Layer plani v2 (0.4.9'dan keyin)

**Nimaga yangi:** oldingi planda "join/relation"ni bitta qatorга tiqqandim — xato edi. Aslida bu **to'rtta alohida mexanizm**, har xil qiyinlikda. 0.4.9 ulardan birini (has-many `with`) qildi. Bu plan qolganini to'g'ri ajratadi.

**Muvaffaqiyat mezoni (konkret):** task-tracker app'ida **0 ta `raw_sql`, 0 ta N+1** qolsin. Har bir feature aniq bitta workaround'ni o'chiradi — pastда qaysi birini deb belgilangan. Query Layer "tugadi" = app toza.

**Tamoyil:** har feature uchun regression test = task-tracker'ning real use-case'i. Feature qo'shdingmi → app'dan bitta workaround o'chir → test yashil.

---

## Track A — Relation loading (qolgan ishning aksariyati; 4 mexanizm)

Tartib: arzon polish → eng tez-uchraydigan → umumiy/qiyin oxirida.

### A1. `with` polish *(kichik — mavjudни yaxshilaydi)*
- Nav-to'plamga **ordering** (`with columns order by position`) — hozir json_agg tartibi aniq emas.
- Ko'p-bosqichli nested `with` (project → boards → columns bitta query'да).
- **O'chiradi:** board-kolonka tartib noaniqligi. *(pain2 #3)*

### A2. belongs-to eager load *(eng oson yangi mexanizm, eng tez-uchraydigan)*
- Bola → ota FK bo'yicha JOIN: `... JOIN parent p ON child.fk = p.id`, natijada nested obyekt.
- **O'chiradi:** `Comment → author`, `Activity → actor`, `WorkspaceMember → workspace` (myWorkspaces) N+1'lari. *(pain2 #2)*

### A3. m2m eager load *(o'rtacha)*
- Join-jadval orqali ikki JOIN + massivga yig'ish: `... JOIN TaskLabel l ON l.taskId=t.id JOIN Label lb ON lb.id=l.labelId`, `json_agg(lb)`.
- **O'chiradi:** `task ↔ label`, `task ↔ assignee` (`labelsFor`/`assigneesFor`) N+1'lari. *(pain2 #2)*

### A4. explicit multi-entity JOIN *(eng umumiy, eng qiyin — oxirida)*
- Nav-asosli (has-many/belongs-to/m2m) yetmaganда ixtiyoriy JOIN uchun escape hatch.
- **Eng past ustuvorlik:** has-many + A2 + A3 keng holatlarni qoplaydi; buni faqat kerak bo'lganда. *(pain2 #2)*

---

## Track B — Agregatsiya (stats use-case; oxirgi `raw_sql`ni o'ldiradi)

### B1. Aggregate + GROUP BY + arbitrary/aliased projection
- `select { status, total: count(*) } from Task ... group by status` → to'g'ri `SELECT status, count(*) AS total ... GROUP BY status`.
- `count`/`sum`/`avg`/`min`/`max`; aliased projection (yalang ustun emas). Parser + codegen.
### B2. `having`
- `having count(*) > N` — hozir parse bo'lmaydi.
- **O'chiradi:** `StatsService` (byStatus/byColumn/byAssignee) `raw_sql`'i. *(pain2 #1 — eng katta P0)*
- *(Skalyar `count(*)` allaqachon bor — buni yana qilish shart emas.)*

---

## Track C — Dinamik so'rovlar (filter/qidiruv)

### C1. Dinamik/optional filter kompozitsiyasi
- Shartли where qurish (status/priority bo'lsa qo'shiladi, bo'lmasa yo'q).
### C2. Dinamik-uzunlikdagi `in (@list)`
- Hozir faqat fixed-arity `in (a,b,c)`.
- **O'chiradi:** task ro'yxatidagi in-code filter+pagination; m2m `where id in (list)`. *(pain2 #4)*

---

## Track D — Kichik, mustaqil, arzon (parallel ketadi)

- **D1. `schema_diff` → `unique`ni ALTER sifatida chiqarsin** — mavjud ustunга qo'shilgan constraint'ni sezsin (hozir faqat fresh CREATE TABLE). *(pain2 #6)*
- **D2. Position/reorder primitivi** — fractional indexing yoki reorder helper (collision'ni yopadi). *(pain2 #7)*
- **D3.** *(past)* atomik `update set col = @param` paramref — yoki shunchaki "`update x in` kanonik yo'l" deb hujjatla. *(pain2 #5)*

---

## Ketma-ketlik

1. **Parallel arzon:** A1 (`with` polish) + D1 + D2 — kichik, tez, mustaqil.
2. **Eng katta ta'sir:** A2 (belongs-to) → A3 (m2m) — qolgan N+1'larning aksariyatini o'ldiradi.
3. **Oxirgi raw_sql:** Track B (agregatsiya) — mustaqil subsystem, stats'ni tozalaydi.
4. **Filtr use-case:** Track C — task filter/qidiruv tishlaganда.
5. **Eng oxiri:** A4 (explicit JOIN) — umumiy bolg'a, faqat nav-asosli yetmaganда.

> Har qadamда: task-tracker'ni qayta yurgiz, bitta `raw_sql`/N+1 yo'qolganini ko'r. App **toza** bo'lganда — Query Layer tayyor, va "ORM'дan qochish" va'dasi rost bo'ladi.
