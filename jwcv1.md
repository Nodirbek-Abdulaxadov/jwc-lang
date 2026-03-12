## 🧠 JWC (Just Web Code) — Core Vision & Architecture Docs v2

### 🎯 Maqsad

**JWC (Just Web Code)** — bu global miqyosdagi, backend-first, compiled, high-performance web dasturlash tili.
Asosiy maqsad:

> Web backend developmentni maksimal soddalashtirish, avtomatlashtirish va compile-time darajada xavfsiz qilish.

JWC oddiy framework emas.
JWC — web uchun noldan yozilgan maxsus til.

---

# 🧭 Falsafa (Philosophy)

### 1. Backend-first

JWC avvalo backend uchun yaratiladi.
Frontend va boshqa yo‘nalishlar keyin qo‘shiladi.

### 2. Compile-time safety

Runtime errorlar → compile-time’da ushlanishi kerak.

Misollar:

* SQL xato
* type mismatch
* null xatolar
* migration xatolari
* dto/entity mismatch
* endpoint contract buzilishi

> Agar compile bo‘lsa — productionga tayyor.

### 3. Kam kod — yuqori performance

Goal:

```
NodeJS simplicity
+
Django automation
+
.NET type safety
+
Rust performance
```

### 4. Zero unnecessary abstraction

Repository pattern, service layer boilerplate,
DTO mapping, ORM config kabi ortiqcha qatlamlar yo‘q.

Tilning o‘zi bularni hal qiladi.

---

# 🏗 Asosiy yo‘nalishlar

## 1. Native compiled

JWC interpreted emas.

```
jwc → native binary
jwc → wasm (future)
jwc → server binary
```

VM yo‘q.
Direct compile.

---

# 🧱 Core Architecture

## DbContext tizimi

JWC’da database til markazida turadi.

Har bir database alohida context:

```
dbcontext AppContext : Postgres;
dbcontext CacheContext : Redis;
dbcontext LogContext : Clickhouse;
```

Context:

* qaysi DB ishlashini biladi
* type mappingni belgilaydi
* entitylarni boshqaradi

---

# 🧩 Entity system (DB-first)

Entity = database table + model, va u aniq dbcontextga bog'lanadi.

```
dbcontext AppDbContext : Postgres;

entity User of AppDbContext {
    id uuid;
    name text(50);
    age int(0,200);
    balance decimal(18,2);
    created_at datetime;
}
```

Bu:

* model
* schema
* validation
* migration source

hammasi bir joyda.

---

# 🗄 Type System (DB bilan bir xil)

JWC typelari DBga yaqin.

### Misollar

```
int
bigint
text
varchar(50)
decimal(18,2)
bool
uuid
datetime
json
```

Har context o‘z typelarini beradi.

### Misol

```
AppContext → postgres typelar
CacheContext → redis typelar
```

---

# 🧠 Compile-time DB validation

JWC compile paytida:

* SQL syntax tekshiradi
* table mavjudligini tekshiradi
* column mavjudligini tekshiradi
* type mosligini tekshiradi
* index mavjudligini tekshiradi (future)

Runtime SQL error → bo‘lmaydi.

---

# 🧮 SQL til ichida (first-class)

SQL — native feature.

```
let users = select * from users where age > 18;
```

yoki typed:

```
let users: List<User> =
    select User from users where age > 18;
```

LINQ yo‘q.
ORM yo‘q.
SQL — primary.

---

# ⚡ Performance falsafa

### 1. Zero-cost abstractions

Har feature compile-time’da hal bo‘ladi.

### 2. Allocation control

Keraksiz heap allocation yo‘q.

### 3. Direct DB access

ORM layer yo‘q → tez.

### 4. Minimal runtime

Heavy runtime yo‘q.

---

# 🔐 Safety

## Compile-time:

* SQL
* types
* null
* dto
* endpoint return
* migration

## Runtime:

Minimal.

---

# 🧰 Backend built-in imkoniyatlar (future core)

JWC backend til sifatida:

### Built-in:

* routing
* auth
* caching
* queue
* job worker
* migration system
* validation
* logging
* config

Framework yozish shart emas.

---

# 🧪 Minimal syntax falsafa

### Lowercase keywords

```
function
entity
let
if
return
```

### ; required (meme + clarity 😄)

### {} optional (1 line)

```
if a > b
    return a;
```

---

# 🧠 Long-term vision

### Phase 1

Backend killer language.

### Phase 2

Frontend (WASM).

### Phase 3

Mobile.

### Phase 4

Self-host compiler.

### Phase 5

Global adoption.

---

# 💀 JWC nimani o‘ldirmoqchi?

Halol:

* Node backend complexity
* Django performance muammosi
* Spring boot og‘irligi
* .NET verbosity
* ORM hell
* 1000 qatlamli architecture

---

# 👑 Ultimate goal

> Web backend yozish → config yozish darajasida oson bo‘lishi.

va

> Performance → Rust/Go darajasida.

---

# 🧨 Real savol (endi sen founder sifatida javob ber)

Docsni qayta yozdim.

Endi 3 yo‘ldan biri:

### 1️⃣ Hardcore engineering mode

Real compiler + syntax freeze

### 2️⃣ Architecture deep dive

Memory, IR, ownership, concurrency

### 3️⃣ Killer feature design

Boshqa tillarda yo‘q narsalar

Qaysiga o‘tamiz? 😈
