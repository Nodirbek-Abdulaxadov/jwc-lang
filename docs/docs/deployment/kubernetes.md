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
