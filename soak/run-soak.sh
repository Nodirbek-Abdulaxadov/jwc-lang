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
#   SOAK_PATH          route bombardier hammers (default: /healthz)
#   SOAK_SERVE_ARGS    extra args for `jwc serve` (default: --skip-schema-check)
#
# The app inherits this shell's environment, so DATABASE_URL, CURSOR_SECRET
# and JWC_REDIS_URL are set by exporting them before calling this.
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
SOAK_PATH="${SOAK_PATH:-/healthz}"
SOAK_SERVE_ARGS="${SOAK_SERVE_ARGS:---skip-schema-check}"

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
# A port already in use makes the readiness probe below pass against
# somebody else's process: the cycle then measures a server this script
# did not start, samples RSS from a pid that failed to bind, and records
# it all as a healthy hour.
if curl --silent --output /dev/null --max-time 2 \
        "http://127.0.0.1:${SOAK_PORT}/healthz" 2>/dev/null; then
    echo "[soak] something is already answering on port ${SOAK_PORT} — stop it, or set SOAK_PORT" >&2
    exit 2
fi

# Per-cycle CSV columns. Keep in sync with analyze.py's READ.
write_header() {
    local f="$1"
    cat > "$f" <<'EOF'
cycle,started_at_utc,total_count,2xx_count,4xx_count,5xx_count,errors_count,others_count,p50_ms,p95_ms,p99_ms,rss_mb_start,rss_mb_end,pool_size_end,pool_available_end,pool_waiting_end
EOF
}

# Scrape one gauge out of `/metrics`. The pool half of the exit criterion
# (`soak/README.md`) is not visible in RSS: a leaked connection costs a few
# KB and shows up as `available` pinned at zero while `waiting` climbs.
gauge() {
    curl --silent --max-time 5 "http://127.0.0.1:${SOAK_PORT}/metrics" \
        | awk -v k="$1" '$1 == k { print $2; found=1 } END { if (!found) print "" }'
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
    # shellcheck disable=SC2086 # SOAK_SERVE_ARGS is a deliberate word list
    "$SOAK_BIN" serve "$PROJECT" --port "$SOAK_PORT" $SOAK_SERVE_ARGS \
        > "$SOAK_RESULTS_DIR/${run_id}-cycle-${cycle_num}.log" 2>&1 &
    app_pid=$!

    # Wait for the listening socket. 30 s budget — anything longer is a
    # boot failure that the operator needs to see.
    #
    # Any HTTP answer means the socket is up. `--fail` on `/` used to be
    # the probe, which reads a 404 as a boot failure — and the v1 sample
    # declares no `/`, so this loop timed out against a perfectly healthy
    # process. `/healthz` is served by the runtime at a fixed path
    # (config.md §4) precisely so a probe has something to ask.
    boot_deadline=$(( $(date +%s) + 30 ))
    until curl --silent --output /dev/null \
              "http://127.0.0.1:${SOAK_PORT}/healthz" 2>/dev/null; do
        # A dead child is a boot failure now, not in 30 s. `serve` exits
        # immediately on a bind conflict or a bad config, and waiting out
        # the full budget only delays the message.
        if ! kill -0 "$app_pid" 2>/dev/null; then
            echo "[soak] cycle $cycle: jwc-app exited during boot — see the cycle log" >&2
            tail -n 5 "$SOAK_RESULTS_DIR/${run_id}-cycle-${cycle_num}.log" >&2 || true
            exit 3
        fi
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
    #
    # `-p r` (print: result only) is not optional. `--format=json` alone
    # still writes the intro line and the live progress bar to the same
    # stdout, so the file is JSON with a paragraph of prose in front of it
    # and the parse below dies on `char 0`. The harness had that bug for
    # as long as it had never been run.
    bombardier \
        --rate "$SOAK_RPS" \
        --duration "$SOAK_DURATION" \
        --format=json \
        -p r \
        "http://127.0.0.1:${SOAK_PORT}${SOAK_PATH}" \
        > "$bombardier_out" || true

    rss_end="$(rss_mb "$app_pid")"
    pool_size_end="$(gauge jwc_db_pool_size)"
    pool_available_end="$(gauge jwc_db_pool_available)"
    pool_waiting_end="$(gauge jwc_db_pool_waiting)"

    # Lift counters. bombardier 1.2's JSON has the shape:
    #   { "result": { "bytesRead": ..., "req1xx": 0, "req2xx": N, ..., "errors": 0, "others": 0, "latency": { "mean": ns, "max": ..., "stddev": ..., "percentiles": { "50": ns, "95": ns, "99": ns } } } }
    py_parse() {
        python3 - "$bombardier_out" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        r = json.load(f).get("result", {})
except Exception as e:
    # A cycle whose counters could not be read is recorded as zeros and
    # the run continues. Aborting here under `set -e` would throw away
    # every hour already spent because one scrape came back malformed —
    # and `analyze.py` reads a zero-total cycle as the anomaly it is.
    print(f"[soak] cycle counters unreadable: {e}", file=sys.stderr)
    print("0,0,0,0,0,0,0,0,0")
    sys.exit(0)
latency = r.get("latency", {}) or {}
# Percentiles are absent from some bombardier builds — the object carries
# only mean/stddev/max. Falling back to the mean keeps the column
# meaningful instead of writing 0.00 and reading as a flat p99 forever.
lat = latency.get("percentiles") or {
    "50": latency.get("mean", 0),
    "95": latency.get("max", 0),
    "99": latency.get("max", 0),
}
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
    echo "${cycle},${started_at},${counters},${rss_start},${rss_end},${pool_size_end},${pool_available_end},${pool_waiting_end}" >> "$csv"

    echo "[soak] cycle $cycle: counters=${counters} rss_end=${rss_end} MB \
pool=${pool_size_end}/${pool_available_end} waiting=${pool_waiting_end} — sending SIGTERM"

    # Graceful shutdown. This is the production-readiness gate: in-flight
    # requests must complete (2xx == total). Bombardier already counted
    # them above.
    # Guarded: `kill` on a process that has already exited returns
    # non-zero, and under `set -e` that ends the run — throwing away every
    # cycle recorded so far because the last one crashed.
    if ! kill -TERM "$app_pid" 2>/dev/null; then
        echo "[soak] cycle $cycle: jwc-app was already gone before SIGTERM" >&2
    fi
    if ! wait "$app_pid" 2>/dev/null; then
        echo "[soak] cycle $cycle: jwc-app exited non-zero on graceful shutdown" >&2
    fi

    sleep "$SOAK_RESTART_GAP"
done

echo "[soak] run $run_id complete — $SOAK_HOURS cycles written to $SOAK_RESULTS_DIR"
echo "[soak] run analyze.py on the results to render PASS / FAIL:"
echo "       python3 soak/analyze.py $SOAK_RESULTS_DIR"
