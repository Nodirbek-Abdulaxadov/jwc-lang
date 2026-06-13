#!/usr/bin/env bash
# Optional sidecar — every $CHAOS_INTERVAL seconds, send SIGTERM to the
# named jwc process so the soak run exercises graceful shutdown more
# aggressively than the cycle boundaries in run-soak.sh.
#
# Use only when investigating a flake in the graceful-shutdown path;
# leaving this on during a normal 72-hour run will conflate the cycle CSV
# with mid-cycle restarts and make the per-cycle delta noisy.
#
# Usage:
#   CHAOS_INTERVAL=600 ./soak/chaos-script.sh
#
# Defaults to one SIGTERM every 600 s (10 min). Stop with Ctrl-C.

set -euo pipefail

CHAOS_INTERVAL="${CHAOS_INTERVAL:-600}"
CHAOS_PROCNAME="${CHAOS_PROCNAME:-jwc}"

echo "[chaos] interval=${CHAOS_INTERVAL}s  target=${CHAOS_PROCNAME}"

while true; do
    sleep "$CHAOS_INTERVAL"
    pid="$(pgrep -f "$CHAOS_PROCNAME serve" | head -1)"
    if [[ -z "$pid" ]]; then
        echo "[chaos] $(date -u +%FT%TZ) — no live $CHAOS_PROCNAME process; skipping"
        continue
    fi
    echo "[chaos] $(date -u +%FT%TZ) — SIGTERM pid=$pid"
    kill -TERM "$pid" || true
done
