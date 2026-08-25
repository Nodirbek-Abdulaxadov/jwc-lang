---
sidebar_position: 1
title: "Packages"
description: "A package is JWC source with a namespace. How to depend on one, what a version pin means, and how to publish."
---

# Packages

A package is JWC source under a namespace. There is no build step and no
compiled artifact — a dependency is code the compiler reads alongside
yours, and it is checked the same way.

## Depending on one

```json title="app.jwcproj"
{
  "name": "my-app",
  "version": "0.1.0",
  "entry": "src/app.jwc",
  "dependencies": {
    "redis": "^0.1.0"
  }
}
```

```jwc no-compile
import redis;

middleware RateLimit {
    let allowed = redis.rate_limit("rl:" + string.of(request.client_ip()), 60, 60);
    if (!$allowed) {
        return tooManyRequests("too many requests");
    }
}
```

`jwc install` writes the requirement into `jwcproj.json` and vendors the
package under `jwc_packages/`. **There is no lockfile and no resolver** —
what pins a build is the vendored directory, so commit `jwc_packages/` if
you want every deploy to compile the same source.

A requirement is a version string. Path and git dependencies do not
exist; `{"path": "../redis"}` is refused with a message saying so rather
than being read as a wildcard. To work on a package beside its consumer,
either point `JWC_REGISTRY` at a local registry, or copy the package into
`jwc_packages/<name>/` — `jwc check` reads what is vendored and does not
re-fetch it.

## Writing one

```
redis/
├── redis.jwcproj      { "name": "redis", "type": "pkg", "pkgVersion": "0.1.0" }
├── main.jwc           namespace redis;
├── README.md
├── LICENSE
└── tests/
```

The namespace is the package name, and a name has to be a valid
identifier — `import jwc-redis;` does not parse, so a package meant to be
imported cannot have a hyphen in it.

`public` marks what callers may use; everything else is private to the
package. That is the whole of the interface: there is no separate header
and nothing to keep in sync.

## Publishing

```bash
jwc login --token jwc_...
jwc publish
```

Names are **first-publisher-wins** and permanent. Check the name is free
before publishing, because the registry will not give it back.

A published version is immutable. Fixing a bug means publishing the next
version, not replacing the one people are already running.

## Testing a package

```bash
jwc test
```

Every `test` block in the workspace runs, wherever its file sits — there
is no filename convention, and `jwc test` does not look for one. Each runs
in a transaction that is rolled back, so cases cannot see each other's
rows and the order they run in does not matter. `--filter` matches the
test block's **name**, not a path.
