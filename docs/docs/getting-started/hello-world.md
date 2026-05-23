---
sidebar_position: 2
---

# Hello world

A 2-minute tour from empty directory to a running HTTP server.

## 1 · New project

```bash
jwc new hello
cd hello
```

This scaffolds:

```
hello/
├── hello.jwcproj    # manifest (name, version, deps)
└── main.jwc         # source
```

## 2 · Hello, terminal

Edit `main.jwc`:

```jwc
function main() {
    print("hello, jwc!");
}
```

```bash
jwc run             # → hello, jwc!
```

## 3 · Hello, HTTP

```jwc
route GET "/" {
    return text("hello over HTTP");
}

function main() { serve(8080); }
```

```bash
jwc run
# in another terminal
curl http://localhost:8080/   # → hello over HTTP
```

## 4 · Hello, JSON

```jwc
route GET "/users" {
    return json({
        users: [
            { id: 1, name: "ali" },
            { id: 2, name: "vali" }
        ],
        total: 2
    });
}

function main() { serve(8080); }
```

`{...}` is a first-class object literal; arrays nest naturally; `json(...)` serialises with the right `Content-Type`.

## 5 · Hot reload

```bash
jwc serve --watch
```

Edits to any `.jwc` under the project trigger a clean restart (~250 ms debounce).

## Next

- [Project structure](./project-structure)
- [Types](../language/types)
- [Routes & middleware](../backend/routes)
