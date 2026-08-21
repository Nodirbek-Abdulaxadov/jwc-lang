# The 1.0 documentation site — work in progress

This tree is **not built yet**. `docusaurus.config.ts` still points at
`archive-0.9`, so nothing here is served.

## Why it exists

jwc.1kb.uz serves `archive-0.9/`, which teaches a language this compiler
does not implement: `dbcontext`, `entity`, `pk autoincrement`, `validate
body`, top-level `route`. Every code sample on the live site fails to lex.

## What is here

| Page | State |
|---|---|
| `intro.md` | written |
| `getting-started/install.md` | written |
| `getting-started/hello-world.md` | written |
| `getting-started/project-structure.md` | written |
| `getting-started/editor-setup.md` | written |
| `language/syntax.md` | written |
| `language/types.md` | written |
| `language/control-flow.md` | written |
| `language/functions.md` | written |
| `data/*`, `backend/*`, `stdlib/*`, `packages/*`, `cli/*`, `deployment/*`, `tutorial/*`, `reference/*`, `security.md` | not written |

`intro.md` links to three pages that do not exist yet — `packages/`,
`security`, `reference/removed`. **Do not point the site at this tree
until they do**, or the build will fail on the broken links.

## A correction to the plan

An earlier plan for this rewrite was to drop the pages for the native AOT
build, the background queue, WebSocket/SSE and the in-process cache, on the
grounds that 1.0 does not have them. That was wrong twice over: those
capabilities were deleted from the compiler without the maintainer's
agreement, and deleting their documentation as well would have made the
loss invisible instead of visible.

They are being restored (`src/native/`). The pages stay, and they say what
the state actually is.
