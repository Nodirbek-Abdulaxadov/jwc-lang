---
sidebar_position: 4
description: "An end-to-end deploy pipeline for a real JWC app, from build and migration to a running service — using jwc-shortener as the worked example."
---

# CI/CD walkthrough

End-to-end deploy pipeline using a real JWC app — [`jwc-shortener`](https://github.com/Nodirbek-Abdulaxadov/jwc-shortener), live at [1kb.uz](https://1kb.uz/).

```
git push App-repo:main
    └─→ GitHub Actions: jwc build --native --release
        └─→ docker build (multi-stage)
            └─→ ghcr.io/OWNER/APP:main-<sha>
                └─→ sed-bump image tag in GitOps-repo/apps/APP/deployment.yaml
                    └─→ commit + push (separate PAT)
                        └─→ ArgoCD detects, rolls the pod
                            └─→ traffic up

Time: ~5–7 min from push → live
```

Three repos in play:

- **App repo** — JWC source + Dockerfile + workflow.
- **GitOps repo** — k8s manifests + ArgoCD app spec.
- **Container registry** — ghcr.io (free for public; no separate auth wiring).

## 1 · App repo layout

```
jwc-shortener/
├── jwc-shortener.jwcproj
├── main.jwc                 # the app — entities, routes, main()
├── migrations/
│   └── 1779576077_init.up.sql
├── Dockerfile               # multi-stage: jwc build --native → debian-slim
├── .gitignore
├── README.md
└── .github/workflows/deploy.yml
```

## 2 · Dockerfile

```dockerfile
# --- builder -----------------------------------------------------------
FROM rust:1.83-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

ARG JWC_VERSION=0.9.2
RUN curl -fsSL https://github.com/Nodirbek-Abdulaxadov/jwc-lang/releases/download/v${JWC_VERSION}/jwc-x86_64-unknown-linux-gnu.tar.gz \
        | tar -xz -C /usr/local/bin

COPY . .
RUN jwc build --native --release

# --- runtime -----------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates wget && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/bin/release/jwc-shortener /usr/local/bin/jwc-shortener
COPY --from=builder /app/migrations /app/migrations
COPY --from=builder /app/jwc-shortener.jwcproj /app/jwc-shortener.jwcproj
WORKDIR /app
EXPOSE 8080
ENV RUST_LOG=info
HEALTHCHECK --interval=30s --timeout=3s \
    CMD wget -q -O- http://127.0.0.1:8080/healthz || exit 1
CMD ["jwc-shortener"]
```

Final image: **~80 MB**. The migration files are baked in so the deployment can run `jwc-shortener migrate up` from an init container.

## 3 · GitHub Actions workflow

```yaml
# .github/workflows/deploy.yml
name: Build and Deploy
on:
  push: { branches: [main] }
env:
  REGISTRY: ghcr.io
  IMAGE_NAME: OWNER/APP

jobs:
  build-and-push:
    runs-on: ubuntu-latest
    permissions: { contents: read, packages: write }
    steps:
      - uses: actions/checkout@v5
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ${{ env.REGISTRY }}
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - id: meta
        uses: docker/metadata-action@v5
        with:
          images: ${{ env.REGISTRY }}/${{ env.IMAGE_NAME }}
          tags: |
            type=sha,prefix={{branch}}-,format=long
            type=raw,value=latest,enable={{is_default_branch}}
      - uses: docker/build-push-action@v6
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          cache-from: type=gha
          cache-to: type=gha,mode=max

  update-gitops:
    needs: build-and-push
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
        with:
          repository: GITOPS_ORG/k8s-gitops
          token: ${{ secrets.GITOPS_PAT }}
      - run: |
          IMAGE_TAG="ghcr.io/OWNER/APP:main-${{ github.sha }}"
          sed -i "s|image:.*APP:.*|image: ${IMAGE_TAG}|g" \
            apps/APP/deployment.yaml
      - run: |
          git config user.name  "GitHub Actions"
          git config user.email "actions@github.com"
          git add apps/APP/deployment.yaml
          git commit -m "Deploy APP main-${{ github.sha }}" || exit 0
          git push
```

### One-time secret

In the **app repo** Settings → Secrets → `GITOPS_PAT`: a personal access token with `repo` scope on the GitOps repo. `GITHUB_TOKEN` is provided automatically for ghcr.io push.

## 4 · GitOps repo manifests

```
k8s-gitops/
├── apps/jwc-shortener/
│   ├── deployment.yaml      # initContainers: jwc-shortener migrate up
│   ├── service.yaml
│   ├── ingress.yaml         # cert-manager + ingress-nginx
│   └── secret.yaml          # JWC_DATABASE_URL + co.
└── argocd/jwc-shortener-app.yaml
```

Highlights from `deployment.yaml`:

```yaml
spec:
  template:
    spec:
      initContainers:
        - name: migrate
          image: ghcr.io/OWNER/APP:latest
          command: ["APP", "migrate", "up"]
          envFrom: [{ secretRef: { name: APP-secret } }]
      containers:
        - name: APP
          image: ghcr.io/OWNER/APP:latest
          ports: [{ containerPort: 8080 }]
          envFrom: [{ secretRef: { name: APP-secret } }]
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
          readinessProbe:
            httpGet: { path: /healthz, port: 8080 }
      imagePullSecrets: [{ name: ghcr-secret }]
```

`initContainers` runs `jwc APP migrate up` before the app container starts. The migration uses a Postgres advisory lock so concurrent rollouts serialise safely.

## 5 · One-time cluster setup

Per cluster:

```bash
# 1. Image pull secret for ghcr.io
kubectl create secret docker-registry ghcr-secret \
    --docker-server=ghcr.io \
    --docker-username=GITHUB_USER \
    --docker-password=GHP_TOKEN \
    --namespace=jwc

# 2. Database
kubectl exec -n postgresql postgresql-0 -- \
    psql -U admin -c 'CREATE DATABASE jwc_shortener'

# 3. ArgoCD app
kubectl apply -f argocd/jwc-shortener-app.yaml
```

After that — every `git push main` in the app repo deploys automatically.

## 6 · Verify

```bash
# Pod up?
kubectl get pods -n jwc -l app=jwc-shortener
kubectl logs   -n jwc deploy/jwc-shortener --tail=50

# ArgoCD sync state?
kubectl get application jwc-shortener -n argocd

# End-to-end
curl https://1kb.uz/healthz
curl -X POST https://1kb.uz/api/links \
    -H 'content-type: application/json' \
    -d '{"url":"https://example.com"}'
```

## Variations

- **No GitOps repo** — drop the `update-gitops` job, deploy with `kubectl apply` from the workflow directly. Simpler, but no PR-able change history of what's running.
- **Bundled launcher instead of native** — change `jwc build --native --release` to `jwc build --release` in the Dockerfile. Image grows by ~15 MB (full `jwc` runtime); first build is 50× faster.
- **No Postgres** — strip the `initContainers` + secret. Useful for stateless JWC services (gateway, proxy, computation API).
