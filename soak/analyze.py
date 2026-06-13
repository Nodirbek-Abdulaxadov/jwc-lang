#!/usr/bin/env python3
"""Analyse a 72h soak run and decide PASS / FAIL against the exit-criteria
contract documented in soak/README.md.

Inputs:
    A directory of cycle CSVs produced by `run-soak.sh`. One CSV per
    hour-cycle, with the schema:
        cycle, started_at_utc, total_count, 2xx_count, 4xx_count,
        5xx_count, errors_count, others_count, p50_ms, p95_ms, p99_ms,
        rss_mb_start, rss_mb_end

Outputs:
    soak/results/summary-<run_id>.png   — RSS + p99 trend plot.
    stdout                              — per-cycle table + PASS/FAIL line.
    exit code 0 on PASS, 1 on FAIL, 2 on environmental failure (missing
    CSVs, malformed input).

Exit-criteria contract:
    PASS iff
        sum(2xx) == sum(total)                          # no lost responses
        AND
        abs(rss_end[N] - rss_end[1]) / rss_end[1] <= 0.10   # ±10% RSS drift
The p99 drift gate is informational only (printed alongside; not a strict
fail).

Usage:
    python3 soak/analyze.py soak/results
"""

from __future__ import annotations

import sys
from pathlib import Path

try:
    import pandas as pd
except ImportError:
    print("[analyze] pandas missing — `pip3 install pandas matplotlib`", file=sys.stderr)
    sys.exit(2)


def load_run(results_dir: Path) -> pd.DataFrame:
    csvs = sorted(results_dir.glob("*-cycle-*.csv"))
    if not csvs:
        print(f"[analyze] no cycle CSVs under {results_dir}", file=sys.stderr)
        sys.exit(2)
    frames = []
    for path in csvs:
        try:
            frames.append(pd.read_csv(path))
        except Exception as e:
            print(f"[analyze] skipping malformed {path.name}: {e}", file=sys.stderr)
    if not frames:
        print("[analyze] every CSV was malformed — aborting", file=sys.stderr)
        sys.exit(2)
    df = pd.concat(frames, ignore_index=True)
    df = df.sort_values("cycle").reset_index(drop=True)
    return df


def render_plots(df: pd.DataFrame, results_dir: Path) -> Path | None:
    try:
        import matplotlib

        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("[analyze] matplotlib missing — skipping plot", file=sys.stderr)
        return None
    fig, (ax_rss, ax_lat) = plt.subplots(2, 1, figsize=(10, 8), sharex=True)
    ax_rss.plot(df["cycle"], df["rss_mb_end"], label="RSS end-of-cycle (MB)", marker=".")
    ax_rss.set_ylabel("RSS (MB)")
    ax_rss.set_title("jwc-app memory across soak cycles")
    ax_rss.grid(True, alpha=0.3)
    ax_lat.plot(df["cycle"], df["p99_ms"], label="p99 latency (ms)", marker=".", color="C1")
    ax_lat.set_xlabel("cycle")
    ax_lat.set_ylabel("p99 (ms)")
    ax_lat.set_title("jwc-app p99 latency across soak cycles")
    ax_lat.grid(True, alpha=0.3)
    out = results_dir / "summary.png"
    fig.tight_layout()
    fig.savefig(out)
    plt.close(fig)
    return out


def decide(df: pd.DataFrame) -> tuple[bool, dict]:
    """Apply the exit-criteria contract; return (pass_flag, metrics)."""
    total_total = int(df["total_count"].sum())
    total_2xx = int(df["2xx_count"].sum())
    lost = total_total - total_2xx
    rss_first = float(df["rss_mb_end"].iloc[0])
    rss_last = float(df["rss_mb_end"].iloc[-1])
    rss_drift = abs(rss_last - rss_first) / rss_first if rss_first > 0 else 0.0
    p99_first = float(df["p99_ms"].iloc[0])
    p99_last = float(df["p99_ms"].iloc[-1])
    p99_drift = abs(p99_last - p99_first) / p99_first if p99_first > 0 else 0.0

    pass_zero_lost = (lost == 0)
    pass_mem_flat = (rss_drift <= 0.10)
    info_p99 = (p99_drift <= 0.20)

    return (
        pass_zero_lost and pass_mem_flat,
        {
            "cycles": int(df["cycle"].max()),
            "total_requests": total_total,
            "total_2xx": total_2xx,
            "lost_responses": lost,
            "rss_first_mb": rss_first,
            "rss_last_mb": rss_last,
            "rss_drift_pct": rss_drift * 100.0,
            "p99_first_ms": p99_first,
            "p99_last_ms": p99_last,
            "p99_drift_pct": p99_drift * 100.0,
            "pass_zero_lost": pass_zero_lost,
            "pass_mem_flat": pass_mem_flat,
            "info_p99_within_20pct": info_p99,
        },
    )


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("Usage: analyze.py <results-dir>", file=sys.stderr)
        return 2
    results_dir = Path(argv[1])
    if not results_dir.is_dir():
        print(f"[analyze] not a directory: {results_dir}", file=sys.stderr)
        return 2

    df = load_run(results_dir)
    plot_path = render_plots(df, results_dir)
    passed, m = decide(df)

    print()
    print(f"=== soak summary ({m['cycles']} cycles) ===")
    print(f"  total requests:        {m['total_requests']:>12,}")
    print(f"  2xx responses:         {m['total_2xx']:>12,}")
    print(f"  lost responses:        {m['lost_responses']:>12,}   (must be 0)")
    print(f"  RSS cycle 1:           {m['rss_first_mb']:>12.1f} MB")
    print(f"  RSS cycle N:           {m['rss_last_mb']:>12.1f} MB")
    print(f"  RSS drift:             {m['rss_drift_pct']:>12.1f} %   (must be <= 10.0)")
    print(f"  p99 cycle 1:           {m['p99_first_ms']:>12.2f} ms")
    print(f"  p99 cycle N:           {m['p99_last_ms']:>12.2f} ms")
    print(f"  p99 drift:             {m['p99_drift_pct']:>12.1f} %   (informational; target <= 20.0)")
    if plot_path:
        print(f"  plot:                  {plot_path}")
    print()
    if passed:
        print("PASS")
        return 0
    else:
        reason = []
        if not m["pass_zero_lost"]:
            reason.append(f"{m['lost_responses']} lost responses across {m['cycles']} cycles")
        if not m["pass_mem_flat"]:
            reason.append(f"RSS drifted {m['rss_drift_pct']:.1f}% (limit 10%)")
        print(f"FAIL: {'; '.join(reason)}")
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
