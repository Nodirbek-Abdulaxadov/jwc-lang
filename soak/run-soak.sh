#!/usr/bin/env bash
# 72-hour soak driver. Loops one cycle per hour: boot jwc-app, hammer it
# with bombardier at sustained load for ~58 minutes, sample RSS + p99 to
# a CSV at the cycle boundary, then SIGTERM (graceful shutdown) and let
# the next iteration boot a fresh process. The exit-criteria contract is
# documented in soak/README.md.
#
# Usage:
#   SOAK_HOURS=72 ./soak/run-soak.sh path/to/jwcproj
#
# Env overrides:
#   SOAK_HOURS         total cycles to run (default: 72)
#   SOAK_RPS           bombardier rate per cycle (default: 5000)
#   SOAK_DURATION      bombardier per-cycle duration (default: 58m)
#   SOAK_RESTART_GAP   seconds to wait after SIGTERM before next boot (default: 60)
#   SOAK_PORT          port the jwc-app binds to (default: 8080)
#   SOAK_BIN           path to the jwc binary (default: target/release/jwc)
#   SOAK_RESULTS_DIR   CSV output directory (default: soak/results)
#
# Exits non-zero on infrastructure failure (jwc-app refused to boot,
# bombardier missing). The pass/fail decision against the exit-criteria
# contract is `analyze.py`'s job — this driver just records the cycles.

set -euo pipefail

PROJECT="${1:-examples/testapp}"
SOAK_HOURS="${SOAK_HOURS:-72}"
SOAK_RPS="${SOAK_RPS:-5000}"
SOAK_DURATION="${SOAK_DURATION:-58m}"
SOAK_RESTART_GAP="${SOAK_RESTART_GAP:-60}"
SOAK_PORT="${SOAK_PORT:-8080}"
SOAK_BIN="${SOAK_BIN:-target/release/jwc}"
SOAK_RESULTS_DIR="${SOAK_RESULTS_DIR:-soak/results}"

mkdir -p "$SOAK_RESULTS_DIR"

run_id="$(date -u +%Y%m%dT%H%M%SZ)"
echo "[soak] run_id=$run_id  hours=$SOAK_HOURS  rps=$SOAK_RPS  project=$PROJECT"
echo "[soak] results dir: $SOAK_RESULTS_DIR"

if ! command -v bombardier >/dev/null 2>&1; then
    echo "[soak] bombardier not found on PATH — see soak/README.md prerequisites" >&2
    exit 2
fi
if [[ ! -x "$SOAK_BIN" ]]; then
    echo "[soak] $SOAK_BIN is not executable — did you cargo build --release --bin jwc?" >&2
    exit 2
fi
if [[ ! -d "$PROJECT" ]]; then
    echo "[soak] project directory $PROJECT not found" >&2
    exit 2
fi

# Per-cycle CSV columns. Keep in sync with analyze.py's READ.
write_header() {
    local f="$1"
    cat > "$f" <<'EOF'
cycle,started_at_utc,total_count,2xx_count,4xx_count,5xx_count,errors_count,others_count,p50_ms,p95_ms,p99_ms,rss_mb_start,rss_mb_end
EOF
}

# Sample RSS (resident set size) of $1 in megabytes. Linux-only.
rss_mb() {
    local pid="$1"
    if [[ -r "/proc/$pid/status" ]]; then
        awk '/^VmRSS:/ { printf "%.1f", $2 / 1024.0 }' "/proc/$pid/status"
    else
        echo "0"
    fi
}

for ((cycle=1; cycle<=SOAK_HOURS; cycle++)); do
    cycle_num=$(printf "%03d" "$cycle")
    csv="$SOAK_RESULTS_DIR/${run_id}-cycle-${cycle_num}.csv"
    write_header "$csv"
    started_at="$(date -u +%FT%TZ)"
    echo "[soak] [$started_at] cycle $cycle / $SOAK_HOURS — booting jwc-app"

    # Boot jwc-app in the background. Redirect its stdout/stderr to a
    # per-cycle log so a crash post-mortem is grep-friendly.
    "$SOAK_BIN" serve "$PROJECT" --port "$SOAK_PORT" \
        > "$SOAK_RESULTS_DIR/${run_id}-cycle-${cycle_num}.log" 2>&1 &
    app_pid=$!

    # Wait for the listening socket. 30 s budget — anything longer is a
    # boot failure that the operator needs to see.
    boot_deadline=$(( $(date +%s) + 30 ))
    until curl --silent --fail "http://127.0.0.1:${SOAK_PORT}/" >/dev/null 2>&1; do
        if (( $(date +%s) > boot_deadline )); then
            echo "[soak] cycle $cycle: jwc-app failed to come up within 30 s" >&2
            kill -TERM "$app_pid" 2>/dev/null || true
            wait "$app_pid" 2>/dev/null || true
            exit 3
        fi
        sleep 1
    done

    rss_start="$(rss_mb "$app_pid")"
    echo "[soak] cycle $cycle: app up (pid=$app_pid, rss_start=${rss_start} MB)"

    # Hit it. JSON output so we can lift counters into the CSV without
    # regexing English prose.
    bombardier_out="$SOAK_RESULTS_DIR/${run_id}-cycle-${cycle_num}.bombardier.json"
    bombardier \
        --rate "$SOAK_RPS" \
        --duration "$SOAK_DURATION" \
        --format=json \
        "http://127.0.0.1:${SOAK_PORT}/" \
        > "$bombardier_out" || true

    rss_end="$(rss_mb "$app_pid")"

    # Lift counters. bombardier 1.2's JSON has the shape:
    #   { "result": { "bytesRead": ..., "req1xx": 0, "req2xx": N, ..., "errors": 0, "others": 0, "latency": { "mean": ns, "max": ..., "stddev": ..., "percentiles": { "50": ns, "95": ns, "99": ns } } } }
    py_parse() {
        python3 - "$bombardier_out" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    r = json.load(f).get("result", {})
lat = r.get("latency", {}).get("percentiles", {}) or {}
total = sum(r.get(k, 0) for k in ("req1xx","req2xx","req3xx","req4xx","req5xx"))
def ms(ns):
    try: return f"{float(ns)/1e6:.2f}"
    except (TypeError, ValueError): return "0"
print(",".join([
    str(total),
    str(r.get("req2xx", 0)),
    str(r.get("req4xx", 0)),
    str(r.get("req5xx", 0)),
    str(r.get("errors", 0)),
    str(r.get("others", 0)),
    ms(lat.get("50", 0)),
    ms(lat.get("95", 0)),
    ms(lat.get("99", 0)),
]))
PY
    }

    counters="$(py_parse)"
    echo "${cycle},${started_at},${counters},${rss_start},${rss_end}" >> "$csv"

    echo "[soak] cycle $cycle: counters=${counters} rss_end=${rss_end} MB — sending SIGTERM"

    # Graceful shutdown. This is the production-readiness gate: in-flight
    # requests must complete (2xx == total). Bombardier already counted
    # them above.
    kill -TERM "$app_pid"
    if ! wait "$app_pid" 2>/dev/null; then
        echo "[soak] cycle $cycle: jwc-app exited non-zero on graceful shutdown" >&2
    fi

    sleep "$SOAK_RESTART_GAP"
done

echo "[soak] run $run_id complete — $SOAK_HOURS cycles written to $SOAK_RESULTS_DIR"
echo "[soak] run analyze.py on the results to render PASS / FAIL:"
echo "       python3 soak/analyze.py $SOAK_RESULTS_DIR"
