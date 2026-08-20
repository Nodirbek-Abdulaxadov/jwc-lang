# JWC 72h Soak Test Harness

Sprint 5 / Phase 5 exit-criteria harness — the recipe future ops uses to
run the 72-hour soak that gates a v1.0 release candidate. This directory
ships the scripts and analyzer; the actual 72-hour run happens out-of-tree
on a dedicated Linux box (Hetzner / AWS) because it burns ~72 CPU-hours
per cycle and needs a real Postgres + a real network surface.

> The in-repo CI (`.github/workflows/ci.yml`) does NOT run this. The
> `.github/workflows/soak.yml` workflow is manual-dispatch only and pins
> itself to the self-hosted runner labelled `soak-runner` — see
> "Operator playbook" below.

## Exit-criteria contract

The 72h soak passes when **all** of the following hold:

| Metric                | Pass when                                              | Source of truth                          |
|------------------------|--------------------------------------------------------|-------------------------------------------|
| **Zero lost responses** | `sum(bombardier 2xx) == sum(total_requests)` across every cycle in `results/*.csv`, including the cycles that straddle a graceful restart. | `analyze.py` reads the `2xx_count` / `total_count` columns from each cycle CSV and asserts the difference is `0`. |
| **Memory flat**         | `abs(rss_at_cycle_N − rss_at_cycle_1) / rss_at_cycle_1 ≤ 0.10` (±10% drift over 72 cycles). | `analyze.py` plots the `rss_mb` column from each cycle CSV and asserts the drift stays within ±10%. |
| **No pool leak**        | `max(pool_waiting_end) == 0` and `pool_available_end` at the last cycle is above zero. | `run-soak.sh` scrapes `jwc_db_pool_*` from `/metrics` at each cycle boundary; `analyze.py` turns it into a verdict. |

The pool row was in this contract from the start and **was not checked**,
because nothing exposed the numbers: `engine::pool_status()` existed and no
endpoint read it. `/metrics` (config.md §4) does now. RSS drift cannot
stand in for it — a leaked connection costs a few KB, and the shape to look
for is `available` pinned at zero while `waiting` climbs.

A run with no pool columns recorded is a **FAIL**, not a pass: the
criterion is unmeasured, and an unmeasured criterion reported as green is
the thing this file exists to prevent.

The plan additionally tracks p99 latency drift; we surface it for
diagnosis but it is NOT a strict gate (a slow Postgres + cold-cache
restart cycle can spike p99 transiently). The threshold the analyser
prints is `p99_drift < 20%`; ops uses it as a "look-at-me-twice" signal.

## Files

| Path             | Role |
|-------------------|------|
| `run-soak.sh`     | Linux driver. Loops 72 hours: boot jwc-app under a fixture project, hit it with bombardier at sustained load, snapshot RSS + p99 to one CSV per cycle, gracefully restart at each cycle boundary. |
| `chaos-script.sh` | Optional sidecar. Sends `SIGTERM` to the jwc-app PID every 10 minutes to exercise the graceful-shutdown path more aggressively than the cycle boundary alone. |
| `analyze.py`      | Reads `results/*.csv`, plots RSS + p99 over time, prints `PASS` / `FAIL` against the exit-criteria contract above. Exits non-zero on FAIL so CI artifacts surface failure. |
| `results/`        | CSV per cycle: `results/$(date +%Y%m%d-%H%M%S)-cycle-NN.csv`. `.gitignored` (you commit the harness, not the output). |

## Prerequisites on the soak box

```bash
# Ubuntu 22.04 LTS reference image — adjust per your distro.
sudo apt-get update
sudo apt-get install -y \
    build-essential pkg-config libssl-dev \
    postgresql postgresql-contrib \
    python3-pip
# Optional: matplotlib draws the RSS/p99 plot. `analyze.py` reads the CSVs
# with the standard library and prints its verdict without it — it used to
# require pandas and exit 2 without it, which made the analyser one more
# thing to install before the soak could tell you anything.
pip3 install matplotlib

# bombardier — single static binary, drop into PATH:
wget -O /usr/local/bin/bombardier \
    https://github.com/codesenberg/bombardier/releases/download/v1.2.6/bombardier-linux-amd64
chmod +x /usr/local/bin/bombardier

# jwc release binary — produced by `cargo build --release` inside the
# checked-out jwc-lang repo. The workflow does this for you. For a
# manual smoke run:
cargo build --release --bin jwc
sudo cp target/release/jwc /usr/local/bin/jwc
```

Postgres should be running locally with a `jwc_soak` role + `jwc_soak`
database; `export DATABASE_URL=postgres://jwc_soak:jwc_soak@localhost/jwc_soak`
before invoking `run-soak.sh`. The app inherits this shell's environment, so
whatever else the fixture project needs — `CURSOR_SECRET`, `JWT_SECRET`,
`JWC_REDIS_URL` — is exported the same way. The sample rate-limits every
route through `redis.rate_limit`, so it needs a Redis and a binary built
`--features redis`; without one every request is a 500 and the run measures
the error path.

Point the load at a route that touches the database. `SOAK_PATH` defaults to
`/healthz`, which is deliberately dependency-free — good for the readiness
probe, useless as a leak test, because it never checks out a connection.
`/api/v1/plans` is what the reference run used.

## Operator playbook

The job-runner UX is GitHub Actions; the workflow is in
`.github/workflows/soak.yml` and is `workflow_dispatch` only — it never
runs on push, pull request, or schedule. To kick off the 72-hour cycle:

1. Provision a Linux box (8 vCPU / 16 GB RAM minimum; SSD; same region as
   your Postgres). Install Docker + Postgres + bombardier per the
   "Prerequisites" block above.
2. Register the box as a self-hosted GitHub Actions runner with the
   label `soak-runner`. (See GitHub docs: Settings → Actions → Runners.)
3. From the GitHub Actions UI, go to "Soak (manual dispatch)" → "Run
   workflow" → confirm the duration (default 72h, overridable for staging
   smoke tests). The workflow's job is to:
   - check out `jwc-lang` at the chosen ref,
   - `cargo build --release --bin jwc`,
   - launch `soak/run-soak.sh` with `SOAK_HOURS=$INPUT_HOURS`,
   - on completion, run `soak/analyze.py` and upload `soak/results/` as
     an artifact (so the cycle CSVs survive the runner).
4. Watch the cycle CSVs land in the artifacts tab. The analyser prints
   `PASS` / `FAIL` at the end; if `FAIL`, attach the CSVs + the analyser
   output to the v1.0 release-gate issue and reschedule after the
   relevant fix lands.

## What "graceful restart" means here

`run-soak.sh` exits the jwc-app process at the cycle boundary via
`SIGTERM` (and `chaos-script.sh` does the same every 10 minutes). The
contract is that in-flight requests at SIGTERM time complete normally
(2xx) before the process exits — they MUST NOT show up as connection
errors in the bombardier counters. The current shutdown path
(`server.rs` → tokio graceful shutdown + axum's connection drain) is
what's being measured. A regression here looks like the bombardier
`others` or `errors` counters going non-zero around the cycle boundary
CSV. The analyser dumps those columns side-by-side per cycle so the
failure mode is obvious.

## Why this is a manual workflow, not CI

A real 72-hour run consumes ~$15-30 of compute and ~3 days of wall time.
Running it on every commit would be wasteful and would block PRs. The
workflow exists so ops has a one-click recipe that can't drift from the
harness scripts (the workflow `actions/checkout`s the same repo); the
trigger choice (`workflow_dispatch`) keeps the CPU bill matched to the
v1.0 release cadence.
