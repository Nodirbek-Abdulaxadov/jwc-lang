---
sidebar_position: 7
title: Static files
description: "static \"/assets\" from \"public\" — serving a directory, and putting it inside the native binary."
---

# Static files

```jwc no-compile
static "/assets" from "public";
```

Serves `public/` at `/assets`. That is the whole declaration: no body, no
`use` chain, no path parameters.

A single-page app is the same thing at the root, with a long cache because
the filenames carry hashes:

```jwc no-compile
static "/" from "dist" cache 31536000;
```

`from` is relative to the project directory, and it has to exist when the
program is checked — a typo is `E0741` at `jwc check`, not a 404 in
production.

## It ends up inside the binary

`jwc serve` reads the directory per request, so an edit shows on the next
refresh.

`jwc build` walks the tree at compile time and copies it into the generated
crate, where `include_bytes!` puts it in the executable. The binary is the
deployment:

```bash
jwc build --release
scp bin/release/app server:/srv/app     # nothing else goes with it
```

No `public/` beside the binary, no volume mount, no `COPY` line in a
Dockerfile that someone will forget.

## Precedence

1. a declared `route` or `socket`
2. `/healthz`, `/readyz`, `/metrics`
3. a `static` mount, in source order
4. 404

A mount never takes a declared path away, and a mount at `/` cannot capture
the health probes — a file named `healthz` in your `dist/` does not answer
`/readyz`.

## What it will not serve

| | |
|---|---|
| `..` in any encoding | 404 |
| a name beginning with `.` — `.env`, `.git/`, `.htpasswd` | 404 |
| a separator smuggled through an escape (`%2f`, `%5c`) | 404 |
| a malformed escape (`%zz`, a trailing `%`) | 404 |
| a symlink pointing outside the tree | 404 |
| a directory | its `index.html`, or 404 — there is no listing |

Nothing is normalised into shape. A segment that is not an ordinary file
name is refused, and the refusal happens before any syscall.

`jwc build` applies the same rules to the walk, so a `.env` that happened
to be sitting in your `dist/` is not merely unreachable in the binary — it
is not in it.

## Headers

| Header | Value |
|---|---|
| `Content-Type` | by extension; unknown is `application/octet-stream`, never a guess |
| `ETag` | the sha256 of the bytes |
| `Cache-Control` | `public, max-age=<cache>`, or `max-age=0, must-revalidate` without one |
| `X-Content-Type-Options` | `nosniff` |

`If-None-Match` gets a 304. `HEAD` gets the headers and the length. `POST`,
`PUT`, `DELETE` on a mounted path get **405** with `Allow: GET, HEAD` —
the path exists, the method is wrong, and a 404 there would send you
looking for a typo in a path that is right.

## Both backends answer the same bytes

The rules above — which URLs are refused, what the content type is, what
the ETag is — are one file that `jwc serve` includes and `jwc build` pastes
into the crate it generates. The two do not implement this page twice.

Spec: [`routing.md` §10](https://github.com/just-web-code/jwc-lang/blob/main/docs/spec/v1/routing.md).
