# pkg-demo

End-to-end namuna: bitta **library** paket (`greet-lib`) va uni
ishlatadigan **app** loyihasi (`app`). `import`, `public`/`private`, `mount`,
`group`, library middleware eksporti va path-source dependency-larni
ko'rsatadi.

## Struktura

```
pkg-demo/
├── greet-lib/                  ← library paket (type: "pkg")
│   ├── greet-lib.jwcproj
│   └── main.jwc
└── app/                        ← runnable loyiha (type: "app")
    ├── app.jwcproj
    └── main.jwc
```

## Manifest farqlari

`greet-lib/greet-lib.jwcproj`:
```json
{
    "name": "greet-lib",
    "version": "1.0.0",
    "type": "pkg",
    "pkgVersion": "0.1.0",
    "dependencies": {}
}
```

`type: "pkg"` — bu paket alohida ishlamaydi. `jwc run greet-lib` aniq xato
beradi: *"Project 'greet-lib' is declared as a package (type: \"pkg\") and
cannot be run directly."*

`app/app.jwcproj`:
```json
{
    "name": "app",
    "version": "1.0.0",
    "type": "app",
    "dependencies": {
        "greet-lib": { "path": "../greet-lib" }
    }
}
```

`type: "app"` — runnable. Path source orqali `../greet-lib` ga ulanadi.

## Library — `greet-lib/main.jwc`

```jwc
namespace greet;

private function build_message(name: string): string {
    return `Hello, ${name}! Welcome to JWC.`;
}

public function hello(name: string): string { return build_message(name); }
public function shout(name: string): string { return upper(build_message(name)); }

public middleware RequestLog {
    print(`[greet] inbound request`);
    return null;
}

route GET "/ping"          { return json({ source: "greet-lib", status: "ok" }); }
route GET "/greet/{name}"  {
    let name = path_param("name");
    return json({ message: hello(name) });
}
```

**Diqqat:**
- `private function build_message` — paket ichida, tashqaridan ko'rinmaydi.
- `public function hello/shout` — boshqa namespacedan chaqirsa bo'ladi.
- `public middleware RequestLog` — consumer `use greet.RequestLog` orqali ishlatadi.
- `route ...` — INACTIVE; faqat consumer `mount` qilsa yoqiladi.

## Consumer — `app/main.jwc`

```jwc
import greet;

middleware Cors {
    print("[cors] ok");
    return null;
}

middleware ApiKey {
    let key = header("X-API-Key");
    if (key == null) {
        return unauthorized({ error: "missing X-API-Key" });
    }
    return null;
}

// Group — barcha ichidagi route va mount-ga prefix + middleware qo'shadi
group "/health" use Cors {
    route GET "/" {
        return json({ status: "ok" });
    }
}

group "/api" use Cors, ApiKey {
    mount greet at "/greet";          // → /api/greet/ping, /api/greet/greet/{name}
    route GET "/me" {                 // → /api/me (Cors + ApiKey ham qo'llanadi)
        return json({ user: "alice" });
    }
}

// Bir paketni boshqa joyda, boshqa middleware bilan ham mount qilsa bo'ladi
mount greet at "/public";             // → /public/ping va /public/greet/{name}

// Top-level route, library-dan eksport qilingan middleware bilan
route GET "/" use greet.RequestLog {
    return json({ message: greet.hello("world") });
}

function main() {
    print("starting pkg-demo on :8080");
    serve(8080);
}
```

## Yangi tushunchalar (qisqacha)

| Element | Vazifa |
|---|---|
| `import greet;` | Namespace ichidagi public elementlarga FQN orqali kirish (`greet.hello()`) |
| `mount greet;` | Library route'larini yoqish (root-da) |
| `mount greet at "/p";` | Library route'larini `/p/...` prefix bilan yoqish |
| `group "/p" use Mw { ... }` | Ichidagi `route` va `mount`-ga prefix + middleware qo'llash |
| `group use Mw { ... }` | Prefix-siz, faqat middleware |
| `group "/p" { ... }` | Middleware-siz, faqat prefix |
| `use greet.RequestLog` | Cross-namespace middleware, FQN bilan |

## Ishga tushirish

```bash
cd examples/pkg-demo/app
jwc run                           # path source — tarmoqsiz resolve
# yoki
jwc install                       # jwcproj.lock yaratadi
jwc tree                          # dep grafini chiqaradi
jwc serve --port 8080
```

Test:

```bash
# Public health (faqat Cors)
curl http://localhost:8080/health/                          # {"status":"ok"}

# /api ostidagilar — Cors + ApiKey shart
curl http://localhost:8080/api/greet/ping                   # 401 (api-key yo'q)
curl -H "X-API-Key: test" http://localhost:8080/api/greet/ping
# {"source":"greet-lib","status":"ok"}

curl -H "X-API-Key: test" http://localhost:8080/api/me      # {"user":"alice"}

# Public mount — middleware-siz
curl http://localhost:8080/public/ping                       # {"source":"greet-lib","status":"ok"}
curl http://localhost:8080/public/greet/anvar               # {"message":"Hello, anvar! ..."}

# Top-level, library RequestLog middleware bilan
curl http://localhost:8080/                                  # log: "[greet] inbound request"
                                                             # {"message":"Hello, world! ..."}
```

## Library-ning ichki funksiyasini chaqirib ko'rish (visibility xato)

`app/main.jwc` ga qo'shsangiz:
```jwc
function debug() {
    let x = greet.build_message("test");   // ❌
}
```

`jwc check` yoki `jwc run` aniq xato beradi:
```
Function 'build_message' is private to namespace 'greet' and
cannot be called from '<root>'
```

`greet-lib/main.jwc`-da `private function build_message` ni `public function`
qilib o'zgartirsangiz, xato yo'qoladi.

## Native build

```bash
cd examples/pkg-demo/app
jwc build --native --release
./bin/release/app.exe
```

`flatten_namespaces` pass mount expansion va FQN resolve-ni codegendan oldin
qiladi, shuning uchun native ham interpreter bilan bir xil natija beradi.
