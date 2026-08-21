---
sidebar_position: 1
title: "The `jwc` command"
description: "Every subcommand: check, fmt, serve, build, migrate, test, lint, routes, explain, openapi, lsp, and the registry commands."
---

# The `jwc` command

One binary. It is the compiler, the server, the migration tool and the
language server.

## Every day

```bash
jwc check .        # parse and type-check
jwc serve .        # run it
jwc fmt .          # rewrite in canonical form
jwc test           # run every `test` block
```

`jwc check` is the one to put in a pre-commit hook. It needs no database
and no network: the schema is in the source, so the queries are checked
against it without connecting to anything.

## Running

```bash
jwc serve .                    # the interpreter — the whole language
jwc build .                    # a native binary at bin/debug/<name>
jwc build . --release          # optimised
jwc build . --emit-rust        # the generated Rust, without compiling it
```

`jwc build` produces one statically-linkable binary with no runtime
dependency on the compiler. It needs a Rust toolchain, because that is
what it hands the generated crate to.

The two backends are held to the same answers: the release check builds
each real application both ways, runs the same requests against both, and
compares the responses byte for byte — status, content-type, body and
headers.

Anything `jwc build` cannot lower it **refuses**, naming the construct.
A binary that quietly dropped a query would be a far worse outcome than
one that will not build.

## Schema

```bash
jwc migrate new <name> .       # diff against the last snapshot
jwc migrate up .               # apply what is pending
jwc migrate status .           # applied, pending, drifted
jwc migrate verify .           # constraints and indexes, by name
jwc migrate down .             # roll back, newest first
jwc gen-sql .                  # the whole schema as DDL, to stdout
```

## Seeing what the compiler sees

```bash
jwc routes .                   # method, path, middleware chain
jwc explain .                  # every query, with the SQL it lowers to
jwc openapi . > openapi.json   # OpenAPI 3.1 for the route table
jwc ast .                      # the parsed AST — a debugging aid
```

`jwc openapi` reads the types the checker already inferred rather than
re-deriving them. One type engine, one answer: a route returning
`json(OrgService.get(...))` documents a shape rather than shrugging.

`jwc routes` is the fastest way to answer "why is this endpoint 404" and
"which middleware actually runs here".

## Lints

```bash
jwc lint .                     # check, plus the advisory whole-program lints
jwc lint . --deny-warnings     # for CI
jwc lint . --constraints       # a where or orderby with no index behind it
```

## Packages

```bash
jwc add <name>                 # fetch and record as a dependency
jwc login --token jwc_...      # store a registry key
jwc publish                    # upload this package
```

## Editors

```bash
jwc lsp                        # the language server, LSP over stdio
```

Diagnostics, go-to-definition, hover and completion, from the same
front-end `jwc check` uses — so the editor and the build never disagree.
See [Editor setup](../getting-started/editor-setup.md).
