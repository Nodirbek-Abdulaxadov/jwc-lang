# JWC

**JWC** — backend ilovalar uchun mo'ljallangan minimal dasturlash tili.  
Maqsad: REST API, ma'lumotlar bazasi va biznes logikasini oddiy, o'qilishi oson sintaks bilan yozish.

```jwc
route GET "api/brands/{id}" {
    let id = path_param("id");
    let brand = getBrandById(id);
    if (brand == null) {
        return notFound();
    }
    return json(brand);
}
```

> ⚠️ Hozirda **interpreter** rejimida ishlaydi. Native compiler — kelajakdagi bosqich.

---

## O'rnatish

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
```

Yoki reponi clone qilib:

```bash
git clone https://github.com/Nodirbek-Abdulaxadov/jwc-lang
cd jwc-lang
bash install.sh --release
```

---

## Tezkor boshlash

```bash
# 1. Yangi project yaratish
jwc new myapp
cd myapp

# 2. .env faylini to'ldirish
cat > .env <<EOF
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=myapp
EOF

# 3. Database yaratish (bir marta)
createdb myapp

# 4. Migration qo'llash
jwc migrate up

# 5. Syntax tekshirish
jwc test

# 6. Ishga tushirish (HTTP server + DB)
jwc run
```

---

## Til sintaksisi

### O'zgaruvchilar

```jwc
let name = "Alice";
let age  = 25;
let ok   = true;
name = "Bob";
```

### Funksiyalar

```jwc
// Untyped
function add(a, b) {
    return a + b;
}

// Typed params + return type annotation
function greet(name: string, times: int): string {
    return "Hello, " + name;
}
```

**Qo'llab-quvvatlanadigan turlar:** `string`, `int`, `bool`

### Shartlar va tsikllar

```jwc
if (age >= 18) {
    print("adult");
} else {
    print("minor");
}

let i = 0;
while (i < 5) {
    if (i == 3) { break; }
    print(i);
    i = i + 1;
}
```

### Entity (jadval schema)

```jwc
entity Brand {
    id   int pk;
    name varchar(255);
}

entity Car {
    id      int pk;
    model   varchar(255);
    year    int;
    brandId int;
    colorId int;
}
```

**Qo'llab-quvvatlanadigan column turlari:**

| JWC turi       | PostgreSQL     |
|----------------|----------------|
| `int`          | `INTEGER`      |
| `bigint`       | `BIGINT`       |
| `bool`         | `BOOLEAN`      |
| `uuid`         | `UUID`         |
| `text`         | `TEXT`         |
| `text(n)`      | `VARCHAR(n)`   |
| `varchar(n)`   | `VARCHAR(n)`   |
| `decimal(p,s)` | `NUMERIC(p,s)` |
| `datetime`     | `TIMESTAMP`    |
| `json`         | `JSONB`        |

### DbContext

```jwc
dbcontext AppDbContext : Postgres;
```

### DB operatsiyalar

```jwc
// SELECT — ro'yxat
let brands = select Brand from AppDbContext.Brands;

// SELECT — bitta
let brand = select Brand from AppDbContext.Brands where Brand.id == @id first;

// INSERT
let b = new Brand();
b.name = "Toyota";
insert b into AppDbContext.Brands;

// UPDATE
b.name = "Toyota Motors";
update b in AppDbContext.Brands;

// DELETE
delete b from AppDbContext.Brands;
```

> **Jadval nomi qoidasi:** `AppDbContext.Brands` → `brands`, `AppDbContext.CarItems` → `car_items` (CamelCase → snake_case)

### Route

```jwc
route GET "api/brands" {
    let brands = getAllBrands();
    return json(brands);
}

route POST "api/brands" {
    let data = body();
    let brand = createBrand(data.name);
    return created(brand);
}

route PUT "api/brands/{id}" {
    let id = path_param("id");
    let data = body();
    let updated = updateBrand(id, data.name);
    if (updated == null) { return notFound(); }
    return json(updated);
}

route DELETE "api/brands/{id}" {
    let id = path_param("id");
    let ok = deleteBrand(id);
    if (ok == false) { return notFound(); }
    return noContent();
}
```

**Route helper funksiyalari:**

| Funksiya             | HTTP status | Tavsif                     |
|----------------------|-------------|----------------------------|
| `json(val)`          | 200         | JSON response              |
| `created(val)`       | 201         | Yangi resurs, `id` bilan   |
| `noContent()`        | 204         | Bo'sh response             |
| `notFound()`         | 404         | Resurs topilmadi           |
| `internalError(msg)` | 500         | Server xatosi              |
| `body()`             | —           | Request body (JSON object) |
| `path_param("key")`  | —           | URL path parametri         |

### main() — Entrypoint

`main()` funksiyasi server sozlamalarini o'rnatib, `serve()` ni chaqiradi:

```jwc
function main() {
    setConnectionString(`postgresql://${env("PG_USER")}:${env("PG_PASSWORD")}@${env("PG_HOST")}:${env("PG_PORT")}/${env("PG_DATABASE")}`);
    serve(8080);
}
```

**Builtin funksiyalar (main ichida):**

| Funksiya                   | Tavsif                                        |
|----------------------------|-----------------------------------------------|
| `serve(port?)`             | HTTP serverni ishga tushiradi (default: 8080) |
| `setConnectionString(url)` | Postgres connection string o'rnatadi          |
| `env("VAR_NAME")`          | Environment variable qiymatini o'qiydi        |

### Template strings

Backtick (`` ` ``) ichida `${expr}` interpolatsiyasi:

```jwc
let url = `postgresql://${env("PG_USER")}:${env("PG_PASSWORD")}@${env("PG_HOST")}/${env("PG_DATABASE")}`;
let msg = `Hello, ${name}! Age: ${age}`;
```

### Namespace va import

```jwc
// utils.jwc
namespace myapp.utils;

function square(n: int): int {
    return n * n;
}
```

```jwc
// main.jwc
import myapp.utils;

function main() {
    print(square(5));  // 25
    serve(8080);
}
```

---

## .env fayl

JWC project root'idagi `.env` faylini avtomatik yuklab, env var sifatida o'rnatadi.  
`DATABASE_URL` mavjud bo'lmasa, `PG_*` varlaridan avtomatik quriladi.

```env
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=myapp
```

> `.env` da e'lon qilingan varlar process env'da mavjud bo'lsa **override qilinmaydi**.

---

## CLI buyruqlari

| Buyruq                                 | Tavsif                                             |
|----------------------------------------|----------------------------------------------------|
| `jwc new <name>`                       | Yangi project scaffold                             |
| `jwc run [path]`                       | `main()` ni ishga tushirish (server + DB)          |
| `jwc serve [path] [--port N]`          | HTTP server to'g'ridan (default: 8080)             |
| `jwc test`                             | Syntax va validation tekshirish                    |
| `jwc build [--release]`               | `bin/debug/` yoki `bin/release/` ga build          |
| `jwc check <file>`                     | Bitta faylni parse/validate qilish                 |
| `jwc gen-sql <file>`                   | Entity'lardan `CREATE TABLE` SQL chiqarish         |
| `jwc migrate new <name>`               | Yangi migration fayl yaratish                      |
| `jwc migrate up` / `jwc migrate apply` | Pending migrationlarni Postgres'ga qo'llash        |

---

## Project tuzilmasi

`jwc new myapp` yaratadigani:

```
myapp/
  myapp.jwcproj   ← project manifest
  main.jwc        ← entrypoint
```

`myapp.jwcproj` formati:

```json
{
  "name": "myapp",
  "version": "1.0.0",
  "dependencies": []
}
```

Kattaroq CRUD API uchun tavsiya etilgan tuzilma:

```
myapp/
  myapp.jwcproj
  main.jwc
  .env
  migrations/
    1709123456_init-db.up.sql
    1709123456_init-db.down.sql
  src/
    models/
      Brand.jwc
      Car.jwc
    data/
      AppDbContext.jwc
    services/
      BrandService.jwc
      CarService.jwc
    controllers/
      BrandController.jwc
      CarController.jwc
```

> `bin/` va `target/` papkalari avtomatik e'tibordan chetlatiladi.

---

## To'liq CRUD misoli — testapp

`testapp/` papkasida Brand, Car, Color uchun to'liq REST API mavjud.

**Ishga tushirish:**

```bash
cd testapp

# .env to'ldirish
cat > .env <<EOF
PG_HOST=localhost
PG_PORT=5432
PG_USER=postgres
PG_PASSWORD=secret
PG_DATABASE=testapp
EOF

# DB yaratish (bir marta)
createdb testapp

# Migrationlar
jwc migrate up

# Server
jwc run
# → http://0.0.0.0:8080
```

**API test:**

```bash
# Yaratish
curl -X POST http://localhost:8080/api/brands \
  -H "Content-Type: application/json" \
  -d '{"name":"Toyota"}'
# → {"id":1,"name":"Toyota"}

# Ro'yxat
curl http://localhost:8080/api/brands
# → [{"id":1,"name":"Toyota"}]

# ID bo'yicha
curl http://localhost:8080/api/brands/1
# → {"id":1,"name":"Toyota"}

# Yangilash
curl -X PUT http://localhost:8080/api/brands/1 \
  -H "Content-Type: application/json" \
  -d '{"name":"Toyota Motors"}'
# → {"id":1,"name":"Toyota Motors"}

# O'chirish
curl -X DELETE http://localhost:8080/api/brands/1
# → HTTP 204

# Topilmadi
curl http://localhost:8080/api/brands/999
# → {"error":"Not Found"}
```

---

## Migration

```bash
# Yangi migration yaratish
jwc migrate new init-db

# Qo'llash (.env dan DATABASE_URL avtomatik o'qiladi)
jwc migrate up
# yoki
jwc migrate apply

# Yoki URL ko'rsatib
jwc migrate up --database-url "postgres://user:pass@localhost/db"
```

Migration fayllar `migrations/` papkada saqlanadi:

```
migrations/
  1709123456_init-db.up.sql    ← CREATE TABLE ...
  1709123456_init-db.down.sql  ← DROP TABLE ...
```

Qo'llanilgan migrationlar `_jwc_migrations` jadvalida kuzatib boriladi.

---

## Lokal ishlab chiqish

```bash
git clone https://github.com/Nodirbek-Abdulaxadov/jwc-lang
cd jwc-lang

# Testlar
cargo test

# Debug build + lokal test
cargo build
cd examples/testapp
../../target/debug/jwc test
../../target/debug/jwc run

# Release + global o'rnatish
bash install.sh --release
```

---

## VS Code kengaytmasi

`vscode-extension/` papkasida `.jwc` fayllar uchun syntax highlighting mavjud.

O'rnatish:
1. VS Code → `Extensions` → `Install from VSIX...`
2. Yoki manba kodidan: `cd vscode-extension && vsce package`

---

## Roadmap

| Bosqich                          | Holat       |
|----------------------------------|-------------|
| 1.1 — Real HTTP Server           | ✅ Tayyor   |
| 1.2 — Generic DB Layer (Postgres)| ✅ Tayyor   |
| 1.3 — Type System (basic)        | ✅ Tayyor   |
| 1.4 — `validate body` syntax     | ⬜ Keyingi  |
| 2 — Language Completeness        | ⬜          |
| 3 — Developer Experience         | ⬜          |
| 4 — Native Compiler (LLVM)       | ⬜          |
| 5 — Ecosystem                    | ⬜          |

Batafsil: [ROADMAP.md](ROADMAP.md)

---

## Litsenziya

MIT
