---
sidebar_position: 1
description: "The .jwcproj manifest: JSONC format, every supported field, and how the compiler resolves it from the working directory."
---

# Manifest

Every JWC project has a `<name>.jwcproj` file at its root. JSONC (JSON with `//` and `/* */` comments + trailing commas).

```jsonc
{
  "name": "my-api",
  "version": "1.0.0",      // free-form / language version (legacy)
  "type": "app",           // "app" (default, runnable) or "pkg" (library)
  "pkgVersion": "0.1.0",   // semver — what other projects depend on (only `type=pkg`)

  "dependencies": {
    // From the registry (semver range)
    "logger":   "^0.1.0",
    "json-pp":  "=1.2.3",

    // From a local path (great for monorepos)
    "shared":   { "path": "../shared" },

    // From git
    "auth-mw":  { "git": "https://github.com/me/auth-mw", "rev": "main" }
  },

  // Optional — override the default registry per-project
  "registry": {
    "url": "https://registry-jwc.1kb.uz/"
  }
}
```

## Field reference

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Lowercase, `[a-z][a-z0-9_-]{0,63}` |
| `type` | no | `"app"` (default) — runnable. `"pkg"` — library, depend-on-only. |
| `version` | no | Free-form. Used for human reference and as the publish fallback. |
| `pkgVersion` | only for `type=pkg` | Semver. What `jwc publish` ships under. |
| `dependencies` | no | Object of name → version-or-source. |
| `registry.url` | no | Per-project override for the registry base URL. |

## Source forms

```jsonc
"name": "^0.1.0"                                    // registry
"name": { "version": "^0.1.0" }                     // explicit registry
"name": { "path": "../lib" }                        // local
"name": { "git": "https://...", "rev": "main" }     // git
```

`rev` accepts any git refspec: branch, tag, commit hash.

## type=app vs type=pkg

| | `"app"` | `"pkg"` |
|---|---|---|
| `jwc run` / `jwc serve` | ✅ | ❌ (refused) |
| `jwc publish` | ❌ | ✅ |
| Needs `main()` | ✅ | ❌ |
| Can declare routes | ✅ (mounted on top-level) | ✅ (mounted by consumer with `mount`) |

Setting `type` explicitly is recommended — the default `app` is for legacy compatibility.
