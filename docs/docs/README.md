# The documentation site

This tree is what jwc.1kb.uz serves. `docusaurus.config.ts` points at it.

## Layout

| Path | |
|---|---|
| `intro.md` | the homepage, slug `/` |
| `getting-started/` | install, hello world, project layout, editors |
| `tutorial/` | a link bin — four endpoints, a real database |
| `language/` | syntax, types, control flow, functions |
| `data/` | schema, queries, writes, migrations |
| `backend/` | routing, middleware, errors, validation, config |
| `stdlib/` | every built-in |
| `packages/`, `cli/`, `deployment/` | one page each |
| `security.md` | what the language enforces, and what it does not |
| `reference/removed.md` | what 0.9.x had that 1.0 does not |

## The rules this tree is held to

**Every ```` ```jwc ```` block is compiled** by `tests/docs_parse.rs`. A
block that is prose rather than a program — an operator table, a `{ … }`
elision — is marked ```` ```jwc no-compile ````, and the marker sits in
the fence's info string, which the site ignores.

**The spec is normative, this is not.** `docs/spec/v1/` is where the rules
are decided. Where a page here disagrees with it, the page is wrong.

**Transcripts are copied from runs.** The tutorial's responses were
produced by running the program in it, not typed out.

## `archive-0.9/`

The 0.9.x documentation, kept and no longer served. It describes the
language deployed 0.9.x binaries implement — `dbcontext`, `entity`,
`pk autoincrement`, `validate body` — every sample of which fails to lex
against this compiler.

It stays in the repository because it is the only description of what
those binaries do. `reference/removed.md` is the bridge for a reader
arriving from it.

## Building

```bash
npm install
npm run build      # fails on a broken link, deliberately
npm run serve      # check the build locally
npm start          # dev server with hot reload
```
