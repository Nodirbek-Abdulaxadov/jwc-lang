---
sidebar_position: 2
title: Kubernetes
description: "A Deployment, the migration Job that has to run before it, the three probes, and where the configuration goes."
---

# Kubernetes

Nothing here is JWC-specific machinery — it is the ordinary shape, with
the four places a JWC program has an opinion marked.

## Migrations first

`jwc migrate up` takes a Postgres advisory lock, so several replicas
starting at once do not both apply the same migration. That makes an init
container safe, and it is the simplest correct place for it:

```yaml
initContainers:
  - name: migrate
    image: ghcr.io/you/app:1.4.2
    args: ["jwc", "migrate", "up", "/app"]
    envFrom: [{ secretRef: { name: app-secrets } }]
```

Use a pre-deploy `Job` instead when the migration is long enough that you
do not want every replica's start blocked on it, or when it must run once
under review. The lock makes both correct; the difference is whose
timeout you are spending.

The image is the same one the pods run — a JWC binary is a compiler, a
migrator and a server, so there is no second image to keep in step.

## The Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: app }
spec:
  replicas: 3
  selector: { matchLabels: { app: app } }
  template:
    metadata: { labels: { app: app } }
    spec:
      terminationGracePeriodSeconds: 30
      containers:
        - name: app
          image: ghcr.io/you/app:1.4.2
          ports: [{ containerPort: 8080 }]
          envFrom:
            - configMapRef: { name: app-config }
            - secretRef:    { name: app-secrets }
          livenessProbe:
            httpGet:  { path: /healthz, port: 8080 }
            periodSeconds: 10
          readinessProbe:
            httpGet:  { path: /readyz,  port: 8080 }
            periodSeconds: 5
          startupProbe:
            httpGet:  { path: /healthz, port: 8080 }
            failureThreshold: 30
            periodSeconds: 2
```

### The probes

`/healthz` touches nothing. That is deliberate and it matters most here:
a liveness probe with a dependency behind it turns a database blip into a
restart storm, and the restarts make the blip worse.

`/readyz` round-trips every configured dependency and names the one that
failed. Redis is checked only when `JWC_REDIS_URL` is set, so adding the
variable adds the check and removing it removes it — a deployment that
never used Redis does not start failing readiness because the page
mentioned it.

`/metrics` is Prometheus text. Point a `ServiceMonitor` or a
`prometheus.io/scrape` annotation at it.

### Graceful shutdown

SIGTERM drains: the listener stops accepting, in-flight requests finish,
and after `JWC_SHUTDOWN_TIMEOUT` seconds (default 5) the process exits
whatever is left.

`terminationGracePeriodSeconds` must be **larger** than
`JWC_SHUTDOWN_TIMEOUT`, or the kubelet kills the process mid-drain and the
drain was for nothing. 30 against a default of 5 leaves room for a slow
request.

## Configuration

Split it the way the platform already wants to:

```yaml
apiVersion: v1
kind: ConfigMap
metadata: { name: app-config }
data:
  PORT: "8080"
  JWC_DB_POOL_SIZE: "40"
  JWC_REQUEST_LOG: "1"
  JWC_LOG_FORMAT: "json"
---
apiVersion: v1
kind: Secret
metadata: { name: app-secrets }
stringData:
  DATABASE_URL: postgres://app:…@postgres/app
  JWT_SECRET: …
  CURSOR_SECRET: …
```

**The environment wins over a `.env` file**, always — so a `.env` that
ends up in an image by accident cannot override what the cluster sets.

`JWC_LOG_FORMAT=json` turns the access line into one JSON object per line,
which is what a cluster log collector wants; it is read only when
`JWC_REQUEST_LOG` is on.

## Pool size against replica count

`JWC_DB_POOL_SIZE` is **per process**. Three replicas at 40 is 120
connections, and Postgres's default `max_connections` is 100 — so the
third replica fails to start and the readiness probe correctly says so.

Pick it as `max_connections` minus what everything else needs, divided by
the replica count you scale to, not the one you start with.

## Jobs

`jwc serve` runs the HTTP server **and** the job workers in one process,
so scaling the Deployment scales both. `JWC_JOB_WORKERS` (default 2) is
per process too.

If you want them apart — workers that scale on queue depth rather than
request rate — run a second Deployment from the same image with
`JWC_JOB_WORKERS=0` on the web one. There is no separate worker command:
the queue is Postgres, and any process with the same sources and the same
`DATABASE_URL` is a worker.
