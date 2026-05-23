---
sidebar_position: 2
---

# Dependencies

## Add

```bash
jwc add logger --version "^0.1.0"           # registry
jwc add shared --path ../shared             # local path
jwc add auth-mw --git https://github.com/me/auth-mw --rev main
```

This mutates the manifest and runs the resolver.

## Install

```bash
jwc install
```

Reads `jwcproj.lock`, materialises every dep under `~/.jwc/registry/...`, and verifies sha256.

## Update

```bash
jwc update              # re-resolve everything within current ranges
jwc update logger       # just this one
```

## Remove

```bash
jwc remove logger
```

Drops from the manifest + lockfile + cache.

## Tree

```bash
jwc tree                # full dep tree, depth-first
```

## Lockfile

`jwcproj.lock` records exact versions + sha256 + source URI for every resolved package. **Commit it.** Reproducible builds depend on it.

```toml
# example
[[package]]
name = "logger"
version = "0.1.0"
source = "registry+https://registry-jwc.1kb.uz/"
checksum = "8bc2771170b9bc59662428d59cf4b88da59d239f87c76466175900cc18a724fd"
dependencies = []
```

## Importing in code

```jwc
import logger;          // entire pkg
import logger.fmt;      // a sub-namespace
```

What's importable is governed by `public` / `private` on the package side. Default is `private`; only `public` items leak out.

## Mounting library routes

If a package ships routes, the consumer chooses where to mount them:

```jwc
import auth_mw;
mount auth_mw at "/auth";    // → /auth/login, /auth/logout, …
```

Mounting is opt-in — importing a package doesn't auto-attach its routes.
