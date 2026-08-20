#!/usr/bin/env python3
"""Analyse a soak run and decide PASS / FAIL against the exit-criteria
contract documented in soak/README.md.

Inputs:
    A directory of cycle CSVs produced by `run-soak.sh`. One CSV per
    cycle, with the schema:
        cycle, started_at_utc, total_count, 2xx_count, 4xx_count,
        5xx_count, errors_count, others_count, p50_ms, p95_ms, p99_ms,
        rss_mb_start, rss_mb_end,
        pool_size_end, pool_available_end, pool_waiting_end

Outputs:
    soak/results/summary-<run_id>.png   — RSS + p99 trend plot, when
                                          matplotlib is installed.
    stdout                              — per-cycle table + PASS/FAIL line.
    exit code 0 on PASS, 1 on FAIL, 2 on environmental failure (missing
    CSVs, malformed input).

Exit-criteria contract:
    PASS iff
        sum(2xx) == sum(total)                              # no lost responses
        AND abs(rss_end[N] - rss_end[1]) / rss_end[1] <= 0.10   # ±10% RSS
        AND max(pool_waiting_end) == 0                      # no pool leak
        AND pool_available_end[N] > 0                       # ditto
The p99 drift gate is informational only (printed alongside; not a strict
fail).

The pool half was in the contract from the start and was not checked,
because nothing exposed the gauges: `engine::pool_status()` existed and no
endpoint read it. `/metrics` (config.md §4) does now, `run-soak.sh` scrapes
it at each cycle boundary, and this is where it becomes a verdict. A leaked
connection costs a few KB, so RSS drift cannot see it — the shape is
`available` pinned at zero while `waiting` climbs.

This module reads CSVs with the standard library. It used to require pandas
and exit 2 without it, which made the analyzer another thing to install
before the soak could tell you anything.

Usage:
    python3 soak/analyze.py soak/results
"""

from __future__ import annotations

import csv
import sys
from pathlib import Path

# Columns that are numbers. A blank cell (an older CSV, or a `/metrics`
# scrape that timed out) reads as None and is skipped rather than crashing
# the analysis of everything around it.
NUMERIC = (
    "cycle total_count 2xx_count 4xx_count 5xx_count errors_count others_count "
    "p50_ms p95_ms p99_ms rss_mb_start rss_mb_end "
    "pool_size_end pool_available_end pool_waiting_end"
).split()


def load_run(results_dir: Path) -> list[dict]:
    csvs = sorted(results_dir.glob("*-cycle-*.csv"))
    if not csvs:
        print(f"[analyze] no cycle CSVs under {results_dir}", file=sys.stderr)
        sys.exit(2)
    rows: list[dict] = []
    for path in csvs:
        try:
            with path.open(newline="") as f:
                for raw in csv.DictReader(f):
                    row = dict(raw)
                    for k in NUMERIC:
                        v = (row.get(k) or "").strip()
                        row[k] = float(v) if v else None
                    if row.get("cycle") is None:
                        continue
                    rows.append(row)
        except Exception as e:  # noqa: BLE001 — one bad file must not stop the rest
            print(f"[analyze] skipping malformed {path.name}: {e}", file=sys.stderr)
    if not rows:
        print("[analyze] every CSV was malformed — aborting", file=sys.stderr)
        sys.exit(2)
    rows.sort(key=lambda r: r["cycle"])
    return rows


def column(rows: list[dict], key: str) -> list[float]:
    """Present values only, in cycle order."""
    return [r[key] for r in rows if r.get(key) is not None]


def render_plots(rows: list[dict], results_dir: Path):
    """RSS + p99 over cycles. Optional: a missing matplotlib is a missing
    picture, not a missing verdict."""
    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        return None

    cycles = [r["cycle"] for r in rows]
    fig, (ax_rss, ax_lat) = plt.subplots(2, 1, figsize=(10, 8), sharex=True)
    ax_rss.plot(cycles, [r["rss_mb_end"] for r in rows], label="RSS end-of-cycle (MB)", marker=".")
    ax_rss.set_ylabel("RSS (MB)")
    ax_rss.set_title("jwc-app memory across soak cycles")
    ax_rss.grid(True, alpha=0.3)
    ax_lat.plot(cycles, [r["p99_ms"] for r in rows], label="p99 (ms)", marker=".")
    ax_lat.set_ylabel("p99 (ms)")
    ax_lat.set_xlabel("cycle")
    ax_lat.grid(True, alpha=0.3)
    out = results_dir / "summary.png"
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    plt.close(fig)
    return out


def decide(rows: list[dict]) -> tuple[bool, dict]:
    """Apply the exit-criteria contract; return (pass_flag, metrics)."""
    total_total = int(sum(column(rows, "total_count")))
    total_2xx = int(sum(column(rows, "2xx_count")))
    lost = total_total - total_2xx

    rss = column(rows, "rss_mb_end")
    rss_first, rss_last = (rss[0], rss[-1]) if rss else (0.0, 0.0)
    rss_drift = abs(rss_last - rss_first) / rss_first if rss_first > 0 else 0.0

    p99 = column(rows, "p99_ms")
    p99_first, p99_last = (p99[0], p99[-1]) if p99 else (0.0, 0.0)
    p99_drift = abs(p99_last - p99_first) / p99_first if p99_first > 0 else 0.0

    # The pool half. `waiting` above zero at a cycle boundary means tasks
    # were blocked on a checkout with no load running — which is what a
    # connection that was never returned looks like from outside.
    waiting = column(rows, "pool_waiting_end")
    available = column(rows, "pool_available_end")
    max_waiting = max(waiting) if waiting else 0.0
    last_available = available[-1] if available else None
    # No gauges at all (an older run, or `/metrics` unreachable) is not a
    # pass: it is an unmeasured criterion, and saying so is the point.
    pool_measured = bool(waiting) and bool(available)
    pass_pool = pool_measured and max_waiting == 0 and (last_available or 0) > 0

    pass_zero_lost = lost == 0
    pass_mem_flat = rss_drift <= 0.10
    info_p99 = p99_drift <= 0.20

    return (
        pass_zero_lost and pass_mem_flat and pass_pool,
        {
            "cycles": int(max(column(rows, "cycle"))),
            "total_requests": total_total,
            "total_2xx": total_2xx,
            "lost_responses": lost,
            "rss_first_mb": rss_first,
            "rss_last_mb": rss_last,
            "rss_drift_pct": rss_drift * 100.0,
            "p99_first_ms": p99_first,
            "p99_last_ms": p99_last,
            "p99_drift_pct": p99_drift * 100.0,
            "pool_measured": pool_measured,
            "pool_max_waiting": max_waiting,
            "pool_last_available": last_available,
            "pass_zero_lost": pass_zero_lost,
            "pass_mem_flat": pass_mem_flat,
            "pass_pool": pass_pool,
            "info_p99_within_20pct": info_p99,
        },
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("Usage: analyze.py <results-dir>", file=sys.stderr)
        return 2
    results_dir = Path(argv[1])
    if not results_dir.is_dir():
        print(f"[analyze] {results_dir} is not a directory", file=sys.stderr)
        return 2

    rows = load_run(results_dir)
    plot_path = render_plots(rows, results_dir)
    passed, m = decide(rows)

    print()
    print(
        f"{'cycle':>5}  {'total':>10}  {'2xx':>10}  {'5xx':>7}  "
        f"{'p99 ms':>8}  {'rss MB':>8}  {'pool a/w':>10}"
    )
    for r in rows:
        def cell(key, fmt=",.0f"):
            v = r.get(key)
            return "-" if v is None else format(v, fmt)

        pool = "-"
        if r.get("pool_available_end") is not None:
            pool = f"{cell('pool_available_end')}/{cell('pool_waiting_end')}"
        print(
            f"{cell('cycle'):>5}  {cell('total_count'):>10}  {cell('2xx_count'):>10}  "
            f"{cell('5xx_count'):>7}  {cell('p99_ms', '.2f'):>8}  "
            f"{cell('rss_mb_end', '.1f'):>8}  {pool:>10}"
        )

    print()
    print(f"=== soak summary ({m['cycles']} cycles) ===")
    print(f"  total requests:        {m['total_requests']:>12,}")
    print(f"  2xx responses:         {m['total_2xx']:>12,}")
    print(f"  lost responses:        {m['lost_responses']:>12,}   (must be 0)")
    print(f"  RSS cycle 1:           {m['rss_first_mb']:>12.1f} MB")
    print(f"  RSS cycle N:           {m['rss_last_mb']:>12.1f} MB")
    print(f"  RSS drift:             {m['rss_drift_pct']:>12.1f} %   (must be <= 10.0)")
    if m["pool_measured"]:
        print(f"  pool max waiting:      {m['pool_max_waiting']:>12.0f}      (must be 0)")
        print(f"  pool available, end:   {m['pool_last_available']:>12.0f}      (must be > 0)")
    else:
        print("  pool gauges:                   not recorded   (criterion unmeasured)")
    print(f"  p99 cycle 1:           {m['p99_first_ms']:>12.2f} ms")
    print(f"  p99 cycle N:           {m['p99_last_ms']:>12.2f} ms")
    print(f"  p99 drift:             {m['p99_drift_pct']:>12.1f} %   (informational; target <= 20.0)")
    if plot_path:
        print(f"  plot:                  {plot_path}")
    print()
    if passed:
        print("PASS")
        return 0

    reason = []
    if not m["pass_zero_lost"]:
        reason.append(f"{m['lost_responses']} lost responses across {m['cycles']} cycles")
    if not m["pass_mem_flat"]:
        reason.append(f"RSS drifted {m['rss_drift_pct']:.1f}% (limit 10%)")
    if not m["pass_pool"]:
        if not m["pool_measured"]:
            reason.append("pool gauges were never recorded, so the leak criterion is unmeasured")
        else:
            reason.append(
                f"pool: max waiting {m['pool_max_waiting']:.0f} (limit 0), "
                f"available at end {m['pool_last_available']:.0f} (must be > 0)"
            )
    print(f"FAIL: {'; '.join(reason)}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
