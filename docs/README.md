# JWC Docs

Docusaurus 3.6 site for **https://jwc.1kb.uz**. Source for the public
documentation that ships with the JWC language.

## Local development

```bash
cd docs
npm install
npm run start         # http://localhost:3000
npm run build         # production static site → docs/build/
```

## Container image

The `Dockerfile` builds a multi-stage image (node-20 build → nginx-1.27
runtime). Build context is the **repo root** (so the `COPY docs/...`
paths resolve correctly):

```bash
docker build -f docs/Dockerfile -t ghcr.io/just-web-code/jwc-docs:latest .
```

## CI / CD

`.github/workflows/docs.yml` runs on every push to `main` touching `docs/`:

1. Builds the image and pushes to `ghcr.io/just-web-code/jwc-docs`
   with two tags: `main-<short-sha>` and `latest`.
2. Checks out **musanna-soft/k8s-gitops** and rewrites
   `apps/jwc-docs/deployment.yaml` to pin the new SHA tag.
3. Commits the bump back to `k8s-gitops` so ArgoCD picks it up.

### Required secrets

| Secret | Where to set | Why |
|---|---|---|
| `GITHUB_TOKEN` | provided automatically | push image to GHCR |
| `GITOPS_PAT` | repo → Settings → Secrets → Actions | cross-repo write access to `musanna-soft/k8s-gitops`. PAT needs `contents: write` on that repo. Reuse the same PAT as the mongodbcore-docs workflow if you already have one. |

### Required cluster secret

The Deployment expects `imagePullSecrets: [ghcr-secret]` inside the `jwc`
namespace. Create it once after the ArgoCD app first syncs (it creates
the namespace):

```bash
kubectl create secret docker-registry ghcr-secret \
  --namespace=jwc \
  --docker-server=ghcr.io \
  --docker-username=<your-github-username> \
  --docker-password=<PAT-with-read:packages> \
  --docker-email=you@example.com
```

(Reuse the same PAT as the other `ghcr-secret`s on the cluster — same
auth, separate namespace.)

## Indexing invariants

Four things here are load-bearing for search indexing. They were all
broken at once, which is why the site wasn't getting indexed.

1. **`trailingSlash: false` in `docusaurus.config.ts` and `nginx.conf` are
   one decision, not two.** That setting makes Docusaurus emit flat
   `backend/routes.html` files and canonical tags without a trailing
   slash, so nginx resolves a request with `try_files $uri $uri.html`.
   The `$uri/` form that used to be there answered every canonical URL
   with a 301 to the slashed variant — i.e. every entry in the sitemap
   redirected, to a URL whose own canonical tag pointed back.

2. **Redirects must stay relative** (`absolute_redirect off`). TLS
   terminates at Cloudflare, so nginx sees plain HTTP and an absolute
   redirect names `http://` — a protocol downgrade on every hop.

3. **Unknown paths must 404.** An SPA-style `/index.html` fallback
   answers every mistyped or stale URL with the homepage under a 200,
   which is an unbounded set of soft 404s to a crawler.

4. **Every page needs a `description` in its frontmatter.** Without one
   Docusaurus falls back to the first line of body text, which produced
   meta descriptions like `Add`, `insert` and `| Built-in | Returns |`.

`robots.txt` lives in `static/`. Cloudflare prepends its own managed
block to whatever the origin returns, so the file here is what shows up
*after* `# END Cloudflare Managed Content` — including the `Sitemap:`
line.

To check the four after a change:

```bash
npm run build
grep -c '<lastmod>' build/sitemap.xml     # 50, one per page
ls build/backend/routes.html              # flat file, not a directory
```

## Site structure

- `docs/intro.md` — landing (north star + niche statement)
- `docs/getting-started/` — install, first project, templates, editor
- `docs/tutorial/zero-to-crud.md` — 15-minute end-to-end walkthrough
- `docs/language/` — syntax, types, control flow, functions, async
- `docs/data/` — entities, dbcontext, select / insert / update / delete,
  migrations, transactions
- `docs/backend/` — routes, middleware, validation, error handler,
  websockets, queue
- `docs/stdlib/` — strings, arrays / JSON, HTTP, JWT, hashing
- `docs/deployment/` — native build, Docker, musl, k8s, env vars
- `docs/reference/` — autogen builtins, fmt, upgrade, error codes
- `docs/security/` — SSRF, JWT validation, secrets redaction
