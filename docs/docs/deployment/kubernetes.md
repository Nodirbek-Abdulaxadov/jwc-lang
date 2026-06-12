---
sidebar_position: 3
---

# Kubernetes

The [`musanna-soft/k8s-gitops`](https://github.com/musanna-soft/k8s-gitops) repo is the live reference — ArgoCD-synced. Below is the minimum to deploy a single JWC service.

## Manifests

```yaml
# namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: my-api
---
# secret.yaml — fill in env via External Secrets / Sealed Secrets in prod
apiVersion: v1
kind: Secret
metadata: { name: my-api, namespace: my-api }
type: Opaque
stringData:
  JWC_DATABASE_URL: "postgres://app:secret@postgres.postgres.svc.cluster.local:5432/app"
  JWT_SECRET: "<openssl rand -hex 32>"
---
# deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: my-api, namespace: my-api }
spec:
  replicas: 2
  selector: { matchLabels: { app: my-api } }
  template:
    metadata: { labels: { app: my-api } }
    spec:
      containers:
        - name: my-api
          image: ghcr.io/me/my-api:main-<sha>
          ports: [ { containerPort: 8080, name: http } ]
          envFrom: [ { secretRef: { name: my-api } } ]
          env:
            - name: RUST_LOG
              value: "info,jwc=info"
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
            initialDelaySeconds: 10
            periodSeconds: 30
          readinessProbe:
            httpGet: { path: /healthz, port: 8080 }
            initialDelaySeconds: 5
            periodSeconds: 10
---
# service.yaml
apiVersion: v1
kind: Service
metadata: { name: my-api, namespace: my-api }
spec:
  selector: { app: my-api }
  ports: [ { port: 80, targetPort: 8080 } ]
---
# ingress.yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: my-api
  namespace: my-api
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
spec:
  ingressClassName: nginx
  tls: [ { hosts: [ api.example.com ], secretName: my-api-tls } ]
  rules:
    - host: api.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service: { name: my-api, port: { number: 80 } }
```

## Probes, observability, and trusted proxies

The bundled server registers `/healthz` (liveness — always 200) and
`/readyz` (readiness — round-trips a `SELECT 1` against the DB pool
when `JWC_DATABASE_URL` is set, returning 503 if unreachable) by
default. Wire them in like this:

```yaml
spec:
  containers:
    - name: app
      image: ghcr.io/me/my-api:main-<sha>
      livenessProbe:
        httpGet: { path: /healthz, port: 8080 }
        initialDelaySeconds: 3
        periodSeconds: 10
      readinessProbe:
        httpGet: { path: /readyz, port: 8080 }
        initialDelaySeconds: 5
        periodSeconds: 5
      env:
        # Behind an ingress that overwrites X-Forwarded-For,
        # tell client_ip() to peel off internal hops:
        - { name: JWC_REAL_IP_HEADER,  value: x-forwarded-for }
        - { name: JWC_TRUSTED_PROXIES, value: "10.,127.0.0.1,::1" }
        # Structured JSON logs for Loki / Datadog / CloudWatch:
        - { name: JWC_LOG_FORMAT, value: json }
        # k8s rolling deploys send SIGTERM; default 5s is usually
        # enough but bump if your handlers can run longer:
        - { name: JWC_SHUTDOWN_TIMEOUT, value: "30" }
```

Scrape Prometheus metrics from `/metrics`:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata: { name: my-api, namespace: my-api }
spec:
  selector: { matchLabels: { app: my-api } }
  endpoints: [ { port: http, path: /metrics, interval: 15s } ]
```

Useful metrics out of the box: `jwc_http_requests_total`,
`jwc_http_in_flight`, `jwc_http_request_latency_avg_seconds`,
`jwc_http_request_latency_max_seconds`, `jwc_queue_pending`,
`jwc_queue_dlq`.

## Replica safety

- **Stateless JWC apps** scale horizontally — `replicas: N` is fine.
- **In-memory cache / queue** is **per-pod**. If your app uses `cache_set` or `enqueue` and replicas > 1, you'll see split brain.
  - Cache: short TTLs are usually fine; switch to Redis (planned) for shared.
  - Queue: pin to a single replica (`replicas: 1` + `strategy.type: Recreate`) until persistent backing lands.

## Migrations

Run before the rollout (job, not init container, so rollback doesn't re-run):

```yaml
apiVersion: batch/v1
kind: Job
metadata: { name: my-api-migrate-2026-01-01, namespace: my-api }
spec:
  template:
    spec:
      restartPolicy: OnFailure
      containers:
        - name: migrate
          image: ghcr.io/me/my-api:main-<sha>
          command: [ "my-api", "migrate", "up" ]
          envFrom: [ { secretRef: { name: my-api } } ]
```

Use the same image as the app — same `entities.jwc` ⇒ same migration sql.

## GitOps reference

See [`musanna-soft/k8s-gitops/apps/jwc-registry/`](https://github.com/musanna-soft/k8s-gitops/tree/main/apps/jwc-registry) for a real, end-to-end example (PVC for blob storage, postgres shared from another namespace, cert-manager, ArgoCD app spec).
