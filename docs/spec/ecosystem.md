# JWC ekosistemasi: arxitektura va yo'l xaritasi

> Status: **draft** (2026-06-13). Bu doc — JWC paket ekosistemasining 1.0 va
> undan keyingi davrlar uchun **arxitektura qarori va yo'l xaritasi**. Hech qanday
> bayonot kod sifatida hali shipped emas; har bir faza alohida sprint sifatida
> implementatsiya qilinadi.

---

## 0. Maqsad va falsafa

JWC quyidagi savolga aniq javob bermoqchi:

> **Yangi backend xizmat (Redis, Stripe, Kafka, OpenAI, ...) qo'shish — bu core til
> kengayishimi yoki paket sifatida community-driven kengayishimi?**

Standart tilllar uchun bu savol noaniq:

- **Python:** har narsa `pip install` orqali paket. Lekin C-extension kerak bo'lganda
  `wheel` per-platform — distribyutsiya kabusi.
- **Node.js:** TCP socket + Buffer core'da. RESP, Postgres wire, MongoDB BSON —
  hammasi pure JS paketlar. Trade-off: hot path'da V8 perf ko'p qo'shimcha xarajat.
- **Rust:** har narsa `cargo add`. Kompilyatsiya har qadamda. Ekspressivlik past.
- **.NET:** NuGet ekosistemasi keng. SDK xarajati, GC paushasi.

JWC quyidagi **80/20 qarorni** qabul qiladi:

> **Performance-critical hot-path infra** (sub-ms latency talab qiluvchi) → **core
> til**ga Rust-backed builtin sifatida qo'shiladi.
>
> **HTTP REST asosida ishlaydigan har qanday narsa** → **pure JWC paketi** sifatida
> registry'ga publish qilinadi.

Bu qaror quyidagilarni unlocking qiladi:

1. **Core kichik qoladi** — faqat ~7-10 ta hot-path driver to'plamida.
2. **Package ecosystem o'zi-o'zicha kengayadi** — community contributor'lar Rust
   bilmasdan paket yoza oladi (faqat JWC + HTTP REST).
3. **Single-binary deployment falsafa saqlanadi** — paketlar dynamic-load qilinmaydi,
   pure JWC source sifatida proect bilan birga kompilyatsiya qilinadi.

---

## 1. Tier classifikatsiyasi

### 1.1 Core tier — JWC core'ga Rust-backed builtin

**Mezon:** **sub-millisecond latency budget**, binary wire protocol, connection
pool, retry/health check kerak.

| Kategoriya | Xizmat | Wire protokol | Holat |
|---|---|---|---|
| **Cache / KV** | Redis | RESP (TCP) | 🟡 Faza 1 |
|  | Memcached | text/binary (TCP) | 🔵 keyinroq |
|  | DragonflyDB / KeyDB | Redis-compatible | 🟢 Redis bilan birga |
| **Relational DB** | Postgres | PG wire (TCP, binary) | ✅ shipped |
|  | MySQL / MariaDB | MySQL protocol (TCP, binary) | 🔵 Faza 4 |
|  | ClickHouse | Native TCP / HTTP | 🔵 Faza 4 |
| **NoSQL DB** | MongoDB | Wire (TCP, BSON) | 🔵 Faza 5 |
|  | Cassandra / ScyllaDB | CQL binary (TCP) | 🔵 Faza 5 |
| **Embedded** | SQLite | file API | 🔵 Faza 5 |
|  | RocksDB | file API + FFI | 🔵 Faza 6+ |
| **Message brokers** | NATS / JetStream | NATS proto (TCP, text) | 🟡 Faza 3 |
|  | Kafka / Redpanda | Kafka wire (TCP, binary) | 🔵 Faza 4 |
|  | RabbitMQ | AMQP 0.9.1 (TCP, binary) | 🔵 Faza 4 |
| **Vector DB** | Qdrant native | gRPC (HTTP/2 + protobuf) | 🔵 Faza 6+ |
| **Crypto/Auth** | JWT, Argon2, HMAC, SHA-256, UUID | — | ✅ shipped |
| **Email transport** | SMTP | text TCP | ✅ shipped (`send_email`) |
| **Web realtime** | WebSocket | HTTP/1.1 upgrade + frames | ✅ shipped |
| **Tracing** | OTLP HTTP exporter | HTTP/1.1 + protobuf-JSON | ✅ shipped (feature `otlp`) |

### 1.2 Package tier — pure JWC, registry'dan

**Mezon:** HTTP REST yoki HTTPS asosida ishlaydi, latency budget ~10ms+.

| Kategoriya | Xizmat | Pure-JWC paket nomi |
|---|---|---|
| **Object storage** | S3, MinIO, R2, B2, GCS | `jwc-s3`, `jwc-r2` |
| **LLM / AI** | OpenAI, Anthropic, Gemini, Ollama, Groq | `jwc-openai`, `jwc-anthropic`, ... |
| **Payments** | Stripe, PayPal, Plaid, Adyen | `jwc-stripe`, `jwc-paypal` |
| **Email API** | Mailgun, SendGrid, Resend, Postmark | `jwc-mailgun`, `jwc-resend` |
| **SMS / Voice** | Twilio, Vonage, Plivo | `jwc-twilio` |
| **Push** | FCM, APNs, Expo | `jwc-fcm`, `jwc-apns` |
| **Identity / Auth** | Auth0, Clerk, Keycloak, WorkOS, Stytch | `jwc-auth0`, `jwc-clerk` |
| **Search / Index** | Algolia, Meilisearch, Typesense, Elastic REST | `jwc-algolia`, `jwc-meili` |
| **CMS / Headless** | Strapi, Sanity, Contentful, Hygraph | `jwc-strapi`, `jwc-sanity` |
| **Monitoring** | Sentry, Loki, BetterStack, Datadog | `jwc-sentry`, `jwc-loki` |
| **Infrastructure** | Kubernetes, GitHub, GitLab, Vercel | `jwc-k8s`, `jwc-github` |
| **Maps / Geo** | Mapbox, Google Maps | `jwc-mapbox` |
| **Analytics** | PostHog, Mixpanel, Plausible | `jwc-posthog`, `jwc-plausible` |
| **Captcha** | Turnstile, hCaptcha, reCAPTCHA | `jwc-turnstile` |
| **Chat / Bot** | Slack, Discord, Telegram | `jwc-slack`, `jwc-telegram` |
| **Pub/Sub (cloud)** | Google Pub/Sub, AWS SQS, EventBridge | `jwc-pubsub-gcp`, `jwc-sqs` |

### 1.3 Maxsus tier — defer to post-1.0

| Kategoriya | Sabab |
|---|---|
| **gRPC clients** | Protobuf core'da yo'q; codegen kerak. Paket sifatida ham qiyin. |
| **GraphQL clients** | Query AST lib kerak. Paket sifatida REST'ga qaraganda hint. |
| **LDAP / SAML** | ASN.1/BER, XML parser core'da yo'q. Niche surface. |
| **UDP-based** (StatsD, DNS, NTP) | UDP socket core'da yo'q. Hozirgi 1.0 surface'ga shartmas. |
| **Unix socket** (Docker daemon, PG unix domain) | Cross-platform Windows'da yo'q. |
| **HTTP/2 server** | reqwest client HTTP/2 qiladi, lekin server tomon hozirgi axum tier'da. |

---

## 2. Core tier integration template

Har bir core-tier driver quyidagi **majburiy elementlardan** iborat bo'lishi
kerak. Bu Postgres (`src/engine.rs`) va Postgres job queue
(`src/queue.rs::PostgresJobDriver`) bilan o'rnatilgan pattern.

### 2.1 Cargo feature gate

```toml
# Cargo.toml
[features]
default = []
otlp = [...]       # already shipped
redis = ["dep:redis", "dep:deadpool-redis"]
mysql = [...]
mongo = [...]
```

Default build hech qachon yangi dependency'larni o'zi bilan olib kelmaydi.
User `cargo build --features redis` yoki binary tarqatadigan release workflow
`--all-features` qilib build qiladi.

### 2.2 Singleton pool + boot init

```rust
// src/redis_engine.rs
pub struct JwcRedis {
    pool: deadpool_redis::Pool,
    retry_max: u32,
    retry_backoff_ms: u32,
}
static REDIS: OnceLock<JwcRedis> = OnceLock::new();

pub fn init_redis_from_env() -> Result<()> { /* JWC_REDIS_URL */ }
pub async fn redis_client() -> Result<deadpool_redis::Connection> { /* pool.get() */ }
```

Boot path — `server::serve` ichida `engine::init_engine_from_env()` qatorida
optional `redis_engine::init_redis_from_env()`.

### 2.3 Retry classifier

`engine::is_transient_error` bilan bir xil shaklda Redis-specific transient
hollarni klassifikatsiya qilish:

- `RedisError::is_connection_dropped()`
- `MOVED` / `ASK` cluster redirect
- Timeout

`transaction { }` ichida retry skip (xuddi Postgres'dagi kabi).

### 2.4 Builtinlar registry'ga ro'yxatga olish

```rust
// src/builtins.rs — BUILTIN_DEFS slice'ga qo'shish
BuiltinDef {
    name: "redis_get",
    aliases: &[],
    min_args: 1, max_args: Some(1),
    native: true,  // jwc build --native ham qabul qiladi
},
// ... redis_set / redis_del / redis_incr / redis_expire / redis_lpush / redis_brpop / redis_eval
```

### 2.5 Interpreter + AOT dispatch

- Interpreter: `src/runner/builtins.rs::eval_redis_*_call`
- AOT: `src/native_prelude.rs.in::jwc_b_redis_*`

Mavjud `request_id` / `client_ip` builtinlar bilan **bir xil pattern**.

### 2.6 Metrics gauges

`/metrics` Prometheus endpoint'ga avtomatik qo'shilsin:

```
jwc_redis_pool_size
jwc_redis_pool_available
jwc_redis_pool_max_size
jwc_redis_pool_waiting
```

### 2.7 Health check integratsiyasi

`/readyz` handler `redis::ping()` ham chaqirsin. Postgres `ping()` bilan
parallel — ikkalasidan biri unreachable bo'lsa 503.

### 2.8 Env var registry

`src/config.rs::REGISTRY`'ga yangi entrylar:

| Env var | Default | Type |
|---|---|---|
| `JWC_REDIS_URL` | (unset, redis disabled) | string |
| `JWC_REDIS_POOL_SIZE` | 64 | u32 |
| `JWC_REDIS_RETRY_MAX_ATTEMPTS` | 3 | u32 |
| `JWC_REDIS_RETRY_BACKOFF_MS` | 100 | u32 |
| `JWC_REDIS_TLS` | false | bool |

Redaction'ga `JWC_REDIS_URL` (parol bo'lishi mumkin) ham qo'shiladi —
`config.rs::scrub_database_url` template'ini qayta ishlatish.

### 2.9 Error kinds

`src/runner/mod.rs::JWC_ERROR_KINDS`'ga yangi subtypelar:

```
"RedisError",
"RedisError.ConnectionFailure",
"RedisError.TimedOut",
"RedisError.NoScript",  // EVAL'da kerakli script topilmasa
"RedisError.LoadingError",
```

`classify_jwc_error` `redis::RedisError` downcast'ini qo'shadi.

### 2.10 Conformance tests

- `tests/integration_redis.rs` — testcontainers Redis. `#[ignore]` Docker
  bo'lmagan platformalarda skip.
- Unit testlar `redis_engine::tests::is_transient_error_recognises_*`,
  `pool_status_shape_is_correct`, va h.k.

### 2.11 Docs

- `docs/docs/deployment/redis.md` (yangi) — setup, env vars, Sentinel/Cluster.
- `docs/docs/reference/builtins.md` — `redis_*` qatori.
- `docs/spec/aot-scope.md` — Redis builtins'ni "works on --native" qatoriga
  qo'shish.

---

## 3. Package tier konventsiyalari

### 3.1 Manifest

`jwc-stripe.jwcproj`:

```json
{
  "name": "jwc-stripe",
  "type": "pkg",
  "version": "0.1.0",
  "pkgVersion": "0.1.0",
  "license": "MIT",
  "homepage": "https://github.com/...",
  "minJwcVersion": "0.4.7",
  "description": "Stripe API client for JWC"
}
```

### 3.2 Naming

- **Prefix:** `jwc-*` (registry'da reserved namespace).
- **Vendor neutral nomi:** `jwc-redis` (xizmat nomi), `jwc-cache` (abstrakt
  category), `jwc-openai` (provider nomi).
- **Cloud vendor variantlari:** suffix bilan ajratiladi — `jwc-pubsub-gcp`,
  `jwc-pubsub-aws`.

### 3.3 Surface

Har paket quyidagi qatlamlarda eksport qilishi tavsiya etiladi:

1. **High-level class** — `Stripe.create_customer(email)` — eng oddiy API.
2. **Mid-level functions** — `stripe.post("/customers", body)` — quvurni
   ko'rsatish.
3. **Raw helpers** — `stripe_signature_verify(payload, sig, secret)` — webhook
   handler'lar uchun.

### 3.4 Auth pattern

```jwc
function Stripe(api_key) {
    return {
        api_key: api_key,
        base: "https://api.stripe.com/v1"
    };
}

function stripe_post(s, path, body) {
    let headers = json_stringify({
        "Authorization": "Bearer " + s.api_key,
        "Content-Type": "application/x-www-form-urlencoded"
    });
    let r = http_post(s.base + path, body, headers);
    return json_parse(r);
}
```

### 3.5 Webhook verifikatsiyasi

Stripe / Slack / GitHub kabi providerlar webhook signature talab qiladi —
JWC core'da `hmac_sha256` bor, demak pure JWC'da implementatsiya qilish
mumkin:

```jwc
function stripe_verify_webhook(payload, sig_header, secret) {
    let parts = split(sig_header, ",");
    // parse t=... v1=...
    let expected = hmac_sha256(secret, t + "." + payload);
    return expected == v1;
}
```

### 3.6 Retry helper

Paket sifatida `jwc-retry` core helper qilib publish qilamiz — har paket
o'zi `retry(fn, max_attempts, backoff_ms)` o'rashi shart bo'lmasin:

```jwc
import retry from "jwc-retry";

function get_user(id) {
    return retry(function() {
        return http_get(API_BASE + "/users/" + id);
    }, 3, 100);
}
```

### 3.7 Tests

Har paket o'zining `tests/` papkasida `case_*.jwc` + `.stdout.txt` conformance
testlari shaklida. `jwc test` paket root'ida ishlasin.

### 3.8 Publish workflow

```bash
jwc check                 # E018-E021 type checker yashil
jwc test                  # conformance testlari yashil
jwc lint --deny-warnings  # unused fn / middleware yo'q
jwc publish               # tar.gz pack + POST registry
```

### 3.9 Documentation

Har paketning `README.md`'ida quyidagi bo'limlar majburiy:

1. **Install** — `jwc add jwc-<name>`
2. **Quick start** — 5-qator misol
3. **API reference** — public funksiyalar ro'yxati
4. **Env vars** — paket talab qiladigan har bir `JWC_*` / vendor env vari
5. **Compatibility** — qaysi JWC versiyasidan ishlaydi

---

## 4. Implementatsiya yo'l xaritasi

### Faza 1 — Redis core (2 hafta)

**Maqsad:** Birinchi yangi core driver — Postgres patternini takrorlaymiz.

- [ ] Cargo `[features] redis`
- [ ] `src/redis_engine.rs` — singleton pool + retry classifier + ping
- [ ] `src/builtins.rs` — `redis_get/set/del/incr/expire/lpush/brpop/eval`
      BUILTIN_DEFS'ga
- [ ] Interpreter dispatch (`src/runner/builtins.rs`)
- [ ] Native AOT mirror (`src/native_prelude.rs.in::jwc_b_redis_*`)
- [ ] `/metrics` `jwc_redis_pool_*` gauges
- [ ] `/readyz` Redis ping integratsiyasi
- [ ] `config.rs::REGISTRY` `JWC_REDIS_*` qatorlari + redaction
- [ ] `JWC_ERROR_KINDS` `RedisError.*` subtypelar
- [ ] `tests/integration_redis.rs` (testcontainers, `#[ignore]`)
- [ ] `docs/docs/deployment/redis.md`
- [ ] Release v0.5.0 (minor bump — yangi major feature)

### Faza 2 — `jwc-cache` paket (1 hafta)

**Maqsad:** Ikkinchi qatlam — pure JWC paket sifatida Redis ustiga high-level
API.

- [ ] `jwc-cache` reposi yaratish (sibling jwc-lang yonida)
- [ ] `Cache.get/set/del/clear/cached` middleware
- [ ] Fallback: Redis bo'lmasa `cache_*` in-memory builtinlariga delegate
- [ ] `examples/` papkasi: cache_aside, write-through, request memoization
- [ ] Tests + README
- [ ] `jwc publish` registry'ga
- [ ] Doc o'qilishi: <https://registry-jwc.1kb.uz/pkg/jwc-cache>

### Faza 3 — `jwc-s3` pilot paket (1 hafta)

**Maqsad:** Birinchi pure JWC paket — Rust shim'siz, faqat HTTP + AWS SigV4
JWC'da. Bu **proof of concept** — registry ecosystem'ning core'ga tayanmasdan
o'sa olishini ko'rsatadi.

- [ ] `jwc-s3` reposi
- [ ] SigV4 signing pure JWC'da (sha256/hmac core'da bor)
- [ ] `S3.get/put/list/delete/presign`
- [ ] R2 / MinIO compatibility
- [ ] Tests with mocked HTTP (yoki testcontainers MinIO)
- [ ] `jwc publish`

### Faza 4 — Hot tier kengaytirish (4-6 hafta)

**Maqsad:** Postgres + Redis dan keyin keyingi hot path driverlari.

- [ ] **MySQL core driver** (`Cargo feature mysql`, `mysql_async` crate)
- [ ] **ClickHouse core driver** (`clickhouse` HTTP path; native TCP keyinroq)
- [ ] **NATS core driver** (`async-nats` crate — eng oddiy wire)
- [ ] Har biri o'z error kinds + metrics gauges + /readyz hook bilan

### Faza 5 — Murakkab DB'lar (8-12 hafta, dependencies)

- [ ] **Kafka core driver** (`rdkafka` crate — librdkafka bog'liqligi sezgir)
- [ ] **RabbitMQ core driver** (`lapin` crate)
- [ ] **MongoDB core driver** (`mongodb` official crate)
- [ ] **Cassandra core driver** (`scylla` crate)
- [ ] **SQLite embedded** (`rusqlite` crate)

### Faza 6 — Pure JWC paket cluster

Bu fazalarni har birini alohida sprint sifatida olish shart emas — community
contribute qila boshlaydi. Maslahat:

- [ ] `jwc-openai` (Anthropic API ham shu pattern bilan)
- [ ] `jwc-stripe`
- [ ] `jwc-twilio`
- [ ] `jwc-sentry`
- [ ] `jwc-k8s`
- [ ] `jwc-fcm`
- [ ] `jwc-slack`

Har birini 2-3 kun ish, single contributor qila oladi.

### Faza 7 — Ecosystem quality bar

- [ ] Registry'da "Official" badge — `Nodirbek-Abdulaxadov/jwc-*` org'i
- [ ] CI workflow templates: paket repo'siga drop qilinadigan
      `.github/workflows/jwc-package.yml` (lint + test + publish on tag)
- [ ] Paket security scanning: registry'da `cargo audit`-like
      `jwc audit` (paket'ning JWC kodi va deps'ini tekshirish)
- [ ] Paket'lar uchun rasmiy `jwc-cookbook` repo'si: "X qanday qilinadi"
      misollari to'plami

---

## 5. Versioning va deprecation strategiyasi

### 5.1 Core driverlar

JWC versioning yo'l xaritasiga bog'liq (SEMVER.md):

- Core driver builtin nomi (`redis_get`, ...) — **stable** v1.0 dan keyin.
  O'zgartirib bo'lmaydi.
- Env var nomi (`JWC_REDIS_URL`) — **stable** v1.0 dan keyin.
- Error kind nomi (`RedisError.ConnectionFailure`) — **stable**, lekin yangi
  subtypelar minor versionda qo'shilishi mumkin.
- Pool size / retry default qiymatlari — **observable behaviour**, minor
  versionda o'zgarishi mumkin lekin migration note bilan.

### 5.2 Paketlar

- Har paket o'z SemVer'ini saqlaydi (`jwcproj.json::pkgVersion`).
- `minJwcVersion` field paketning ishlaydigan eng past JWC versiyasini
  belgilaydi. Pasayish breaking change — paket major bump qiladi.
- Registry yanked versiyalarni resolver tashlab ketadi (allaqachon
  `RegistryVersion::yanked` orqali).

### 5.3 Migration

Core driver kelganda mavjud `jwc-cache` paketi (Faza 2) avtomatik foydalanadi —
real-paket kodida o'zgartirish kerak emas, faqat fallback logikasi
dispatching qiladi. Bu **principle**:

> Paket o'z imkoniyatini abstract API ortida yopiqlashi shart. Core driver
> mavjud bo'lsa undan foydalanadi; bo'lmasa yumshoq fallback.

---

## 6. Ochiq savollar

1. **Bytes type** — keyinroq qachon kerak? Hozirgi `Value::Str` UTF-8 invariant
   tutadi. Postgres BYTEA, Redis binary string, Kafka messages binary payload —
   ulardan birortasi muammoga aylangach `Value::Bytes` qo'shamiz. Shu paytgacha
   base64 string'da o'tkazamiz.

2. **Connection multiplexing** — fred-style auto-pipelining bizning Postgres pool
   yondashuvi bilan mos kelmaydi. v1.0 da deadpool patternida qolamiz, v1.x da
   `fred` switch evaluyatsiya qilamiz.

3. **gRPC** — protobuf parser core'da yo'q. Variantlar: (a) `prost` ni Cargo
   feature ortida core'ga qo'shish, (b) gRPC'ni pure JWC'da reflektsiya orqali
   amalga oshirish (juda sekin), (c) defer to post-1.0. **Hozirgi tavsiyam: (c)**.

4. **Paket sandboxing** — pure JWC paketlar to'liq access oladi
   (file IO, http_get arbitrary URL, env var read). `JWC_HTTP_ALLOWLIST` bor
   lekin fayl tizimi yo'q. WASM yondashuvi bilan kelajakda yopiq sandbox
   modeli mumkin.

5. **WASM plugin tier** — Faza 1-6 dan keyin, agar community qabul qilsa,
   WASM pluginlarni qo'shish mumkin: `wasmtime` embed, paket `.wasm` fayl
   ship qiladi. Bu post-1.0 ish.

6. **Native paket shim** — Variant 2 (manifest'da Rust crate dep deklarat
   qilish + `--native` paytda inject) hozirgi roadmap'da yo'q. Agar Faza 1-3
   uchayotgan paytda paketlar TCP socket talab qilsa qayta o'ylab ko'ramiz.

---

## 7. Cross-links

- [aot-scope.md](./aot-scope.md) — Native AOT'da nimalar ishlaydi
- [threat-model.md](./threat-model.md) — Paket o'rnatish + sandbox xavfsizligi
- [semantics.md](./semantics.md) — Til semantikasi, error kinds
- [`SEMVER.md`](../../SEMVER.md) — Core stable surface
- [`DEPRECATION.md`](../../DEPRECATION.md) — Cleanup roadmap
- Registry: <https://registry-jwc.1kb.uz/>
- Source of truth: [`PRODUCTION_READINESS_PLAN.md`](../../PRODUCTION_READINESS_PLAN.md)

---

## 8. Yo'l xaritasi qisqacha

| Faza | Sprint | Deliverable | Versiya |
|---|---|---|---|
| 1 | 2 hafta | Redis core driver (Postgres pattern) | v0.5.0 |
| 2 | 1 hafta | `jwc-cache` paket (high-level API) | jwc-cache@0.1.0 |
| 3 | 1 hafta | `jwc-s3` pilot paket (pure JWC + SigV4) | jwc-s3@0.1.0 |
| 4 | 4-6 hafta | MySQL + ClickHouse + NATS core drivers | v0.6.0 |
| 5 | 8-12 hafta | Kafka + RabbitMQ + Mongo + SQLite | v0.7.0 |
| 6 | rolling | Paketlar registry'da: OpenAI / Stripe / Twilio / ... | community |
| 7 | rolling | Ecosystem quality bar (audit, badges, cookbook) | v1.0 ga qadar |

**1.0 ship gate ushbu roadmap'dan:** Faza 1 + Faza 2 + Faza 3 yopiq bo'lishi
shart (core'da Redis, registry'da kamida 2 ta pilot paket).
