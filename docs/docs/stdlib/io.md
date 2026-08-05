---
sidebar_position: 8
description: "Console and filesystem built-ins for JWC: console.write, console.error, console.read, and the file.* / directory.* families, with their error kinds and security caveats."
---

# Console + files

Three namespaces: `console.*` for the terminal, `file.*` and `directory.*`
for the filesystem. All of them work under both `jwc run` and
`jwc build --native`.

## console

| Built-in | Returns | Notes |
|---|---|---|
| `console.write(v)` | `null` | writes to stdout **immediately**, no trailing newline |
| `console.error(v)` | `null` | same, to stderr |
| `console.read()` | `string?` | one line from stdin, trailing newline stripped; `null` at EOF |

Any value works, not just strings — `console.write(42)` is fine, the same
way `print` takes anything.

## file

| Built-in | Returns | Notes |
|---|---|---|
| `file.read(path)` | `string` | whole file as UTF-8; **raises** if missing |
| `file.write(path, content)` | `null` | creates, or truncates an existing file |
| `file.append(path, content)` | `null` | creates if absent |
| `file.exists(path)` | `bool` | `true` only for a regular file; never raises |
| `file.delete(path)` | `null` | raises if the file is missing |
| `file.copy(src, dst)` | `null` | overwrites `dst` |
| `file.move(src, dst)` | `null` | overwrites `dst`; works across filesystems |
| `file.size(path)` | `int` | size in bytes |
| `file.lines(path)` | `array` | split on newlines; a trailing newline does **not** add an empty last element |

## directory

| Built-in | Returns | Notes |
|---|---|---|
| `directory.list(path)` | `array` | entry names (not full paths), **sorted** |
| `directory.create(path)` | `null` | recursive, and succeeds if it already exists |
| `directory.exists(path)` | `bool` | `true` only for a directory; never raises |
| `directory.delete(path)` | `null` | **not recursive** — raises if the directory is not empty |

`file.exists` and `directory.exists` are complements: exactly one of them is
true for any given path that exists. That is how you tell a file from a
directory in one call.

`directory.delete` is deliberately not recursive, while `directory.create`
is. Creating too much is recoverable; deleting too much is not. To remove a
tree, walk it yourself with `directory.list`.

## Errors

Failures raise, and carry a typed kind you can catch:

| Kind | When |
|---|---|
| `IoError.NotFound` | path does not exist |
| `IoError.PermissionDenied` | the process may not touch it |
| `IoError.AlreadyExists` | destination is in the way |
| `IoError` | anything else — disk full, not a directory, bad UTF-8 |

`catch (e: IoError)` catches all four; the dotted forms catch only
themselves.

```jwc no-compile
try {
    let cfg = file.read("config.json");
    return json(json_parse(cfg));
} catch (e: IoError.NotFound) {
    return notFound(json("config yo'q"));
} catch (e: IoError) {
    return internalError(json(e.message));
}
```

`file.read` raises rather than returning `null` on a missing file. That is
what `file.exists` is for — a `null` return would flatten "missing",
"permission denied", "is a directory" and "not valid UTF-8" into one
indistinguishable value.

## Do not mix `print` and `console.write`

They are not two spellings of the same thing, and interleaving them
produces different output on the two backends:

```jwc no-compile
console.write("a\n");
print("b");
console.write("c\n");
```

| | result |
|---|---|
| `jwc run` | `a`, `c`, `b` |
| `jwc build --native` binary | `a`, `b`, `c` |

`print` appends to an internal buffer that the interpreter flushes only
after `main()` returns, while `console.write` goes straight to the process
stdout. In a native binary `print` is an immediate `println!`, so there is
nothing to reorder. **Pick one per program.**

### Inside a route, this difference matters

When a route body falls through without an explicit `return`, whatever it
`print`-ed becomes the HTTP response body. `console.write` never does —
it is not part of the response at all. So inside a handler:

```jwc no-compile
route GET "api/items" {
    console.write("so'rov keldi\n");   // goes to the server log
    print("bu javob bo'lib qoladi");   // becomes the response body
    // ...
}
```

Use `console.write` / `console.error` to log from a handler. Use `print`
only when you actually mean "this is the response".

## `console.read()` is for CLI programs, not routes

A JWC program that never calls `serve()` is an ordinary command-line
program, and that is where `console.read()` belongs:

```jwc no-compile
function main() {
    console.write("Ismingiz: ");
    let ism = console.read();
    if (ism == null) { return; }        // EOF
    console.write("Salom, " + ism + "\n");
}
```

`null` at EOF is distinct from `""`, which is a real empty line (someone
pressed Enter). That is what makes a read loop terminable.

Calling `console.read()` inside an HTTP route is a mistake. stdin is not
request input — use `body()`, `query_param()` or `header()`. Under a
container or systemd, stdin is usually closed and the call returns `null`
immediately; but if the server was started from a terminal, the route
blocks until somebody types, and because stdin is a process-wide lock it
blocks every other request that calls it too.

## Security: paths are not restricted

**These built-ins pass paths to the operating system unchanged.** There is
no jail, no allowlist and no root directory setting. A route like this is a
local-file-include vulnerability:

```jwc no-compile
route GET "api/files/{name}" {
    return json(file.read("uploads/" + path_param("name")));   // XAVFLI
}
```

Route `{param}` capture already rejects segments that are `.`, `..`, or
that contain a slash, so that specific example is harder to exploit than it
looks — but `query_param()`, `header()` and `body()` are **not** screened at
all, and nothing stops an absolute path:

```jwc no-compile
route GET "api/download" {
    return json(file.read(query_param("path")));   // /etc/passwd ni beradi
}
```

Validate the input yourself before it reaches a path, or keep request data
out of paths entirely. Operationally, run the process as a dedicated user
with only the directories it needs mounted. This is recorded as an accepted
risk in `docs/spec/threat-model.md`.
