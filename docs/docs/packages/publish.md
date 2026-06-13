---
sidebar_position: 3
---

# Publishing

> **Scope note.** JWC's package registry is small and curated by design.
> Mass-ecosystem ambition is not part of the north star — JWC competes on
> CRUD ergonomics, not on the breadth of npm-style availability. If you
> need 1000s of packages, JWC isn't the right tool. See
> [`docs/spec/ecosystem.md`](../../../spec/ecosystem.md) for what belongs
> in core vs. what belongs in a package.

## One-time setup

1. Sign in to [registry-jwc.1kb.uz](https://registry-jwc.1kb.uz/) with Google.
2. Go to **API Keys** → create a key (e.g. `cli`).
3. Copy the `jwc_...` plaintext **once** — it isn't shown again.
4. On your machine:

   ```bash
   jwc login --token jwc_<paste-here>
   ```

   This writes `~/.jwc/credentials.json` (chmod 600 on Unix; user-only on Windows).

## Prepare the manifest

In the package's `<name>.jwcproj`:

```jsonc
{
  "name": "my-logger",
  "type": "pkg",
  "pkgVersion": "0.1.0",
  "dependencies": {}
}
```

`type=pkg` and `pkgVersion` are both required — `jwc publish` refuses `type=app` (you'd never want to publish an executable as a library).

## Publish

```bash
cd my-logger
jwc publish
```

That's it. The CLI:

1. Reads `~/.jwc/credentials.json` for token + registry URL.
2. Packs the project directory (skipping `target/`, `.git/`, `bin/`, `node_modules/`, `.env`, `jwcproj.lock`) into an in-memory `tar.gz`.
3. POSTs to `<registry>/api/v1/pkg/<name>/<pkgVersion>` with `Authorization: Bearer <key>`.

```
Packing  my-logger@0.1.0 (1.2 KB)
Uploading → https://registry-jwc.1kb.uz/api/v1/pkg/my-logger/0.1.0
OK     {"name":"my-logger","version":"0.1.0","sha256":"...","size_bytes":1234}
```

## Republishing the same version

Returns **409 Conflict** — publishes are immutable. Bump `pkgVersion` and try again.

## CI publishing

```yaml
# .github/workflows/publish.yml
on:
  release:
    types: [published]
env:
  JWC_REGISTRY_TOKEN: ${{ secrets.JWC_REGISTRY_TOKEN }}
jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: curl -fsSL https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.sh | bash
      - run: jwc publish
```

`JWC_REGISTRY_TOKEN` env beats the credential file — no `jwc login` needed in CI.

## Yank / unpublish

Use **Revoke** in the UI's package detail page to delete a single version (owner only). The package name stays reserved so a third party can't squat on a freed slot.
