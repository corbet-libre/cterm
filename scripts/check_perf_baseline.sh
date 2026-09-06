#!/usr/bin/env bash
# Hardware-normalized performance regression gate for the deterministic
# cterm-core Criterion suite (inventory and threshold scheme:
# crates/cterm-core/benches/README.md).
#
#   scripts/check_perf_baseline.sh mint  [--baseline NAME] [--quick] [--benches "parser resize_reflow"] [-- criterion-filter...]
#   scripts/check_perf_baseline.sh check [--baseline NAME] [--threshold-pct N] [--ceilings FILE] [--quick] [--benches ...] [-- criterion-filter...]
#
# mint:  runs the suite with `--save-baseline NAME`. Baselines live under
#        target/criterion/ (gitignored) and are never committed, so no
#        host-specific numbers bake into the repo. Mint from a clean tree
#        (usually main) on the machine that will run the check: same host,
#        same build flags, quiet load.
# check: runs the suite with `--baseline NAME`, then fails (exit 1) if any
#        benchmark's median time regressed more than --threshold-pct
#        (default 10) with its 95% confidence interval entirely above zero
#        (i.e. Criterion is confident it is a real slowdown, not noise).
#        Exit 2 means "no baseline minted yet" (with mint instructions) or
#        "nothing was compared" (bad filter?).
#
# The comparison is same-host relative change, which is what makes the gate
# hardware-normalized: absolute times differ per machine, but a >10%
# confident slowdown versus the same machine's baseline is portable.
# An optional --ceilings FILE adds absolute per-bench median caps
# (TSV: "<bench-id><TAB><max-median-ns>", "#" comments); ceilings are
# host-specific on purpose and are NOT committed -- mint them per runner
# from a known-good run (see benches/README.md).
#
# --quick sets CTERM_BENCH_* overrides for a seconds-long smoke run
# (mint+check round-trip). Full runs take a few minutes locally; full runs
# are CI's job. Plain `cargo bench -p cterm-core` behavior is unchanged.
#
# Deliberately NOT wired into .github/workflows here: flipping the CI
# switch needs Julian's go (proposed snippet in benches/README.md).

set -euo pipefail

cd "$(dirname "$0")/.."

MODE="${1:-}"
case "$MODE" in
    mint | check) shift ;;
    -h | --help | help | "")
        sed -n '1,32p' "$0"
        exit 0
        ;;
    *)
        echo "error: first argument must be 'mint' or 'check'" >&2
        exit 2
        ;;
esac

BASELINE="main"
THRESHOLD_PCT="10"
CEILINGS=""
QUICK=0
BENCHES="parser resize_reflow"

while [ $# -gt 0 ]; do
    case "$1" in
        --baseline)
            BASELINE="${2:?--baseline needs a name}"; shift 2 ;;
        --threshold-pct)
            THRESHOLD_PCT="${2:?--threshold-pct needs a number}"; shift 2 ;;
        --ceilings)
            CEILINGS="${2:?--ceilings needs a file}"; shift 2 ;;
        --benches)
            BENCHES="${2:?--benches needs a quoted list}"; shift 2 ;;
        --quick) QUICK=1; shift ;;
        --) shift; break ;;
        -*) echo "error: unknown flag $1" >&2; exit 2 ;;
        *) break ;;
    esac
done
# Remaining args (if any) are Criterion benchmark filters, e.g. screen_resize.

if ! command -v python3 >/dev/null 2>&1; then
    echo "error: check needs python3 to read Criterion's estimates.json" >&2
    exit 2
fi

if [ "$QUICK" -eq 1 ]; then
    export CTERM_BENCH_SAMPLE_SIZE=10
    export CTERM_BENCH_WARMUP_SECS=1
    export CTERM_BENCH_MEASUREMENT_SECS=1
fi

if [ "$MODE" = "mint" ]; then
    for bench in $BENCHES; do
        # shellcheck disable=SC2086
        cargo bench -p cterm-core --bench "$bench" -- "$@" --save-baseline "$BASELINE"
    done
    cat <<EOF
Minted baseline '$BASELINE' under target/criterion/ (gitignored, host-local).
Verify it: scripts/check_perf_baseline.sh check --baseline $BASELINE
EOF
    exit 0
fi

# --- check mode ---
if [ -n "$CEILINGS" ] && [ ! -f "$CEILINGS" ]; then
    echo "error: ceilings file not found: $CEILINGS" >&2
    exit 2
fi

if [ -z "$(find target/criterion -maxdepth 4 -type d -name "$BASELINE" 2>/dev/null | head -1)" ]; then
    cat <<EOF >&2
error: no '$BASELINE' baseline under target/criterion/ on this host.
Mint one from a clean tree first (same host, quiet load):

    scripts/check_perf_baseline.sh mint --baseline $BASELINE

(Quick smoke: add --quick to both mint and check.)
EOF
    exit 2
fi

MARKER="$(mktemp)"
touch "$MARKER"
trap 'rm -f "$MARKER"' EXIT

for bench in $BENCHES; do
    # shellcheck disable=SC2086
    cargo bench -p cterm-core --bench "$bench" -- "$@" --baseline "$BASELINE"
done

export GATE_BASELINE="$BASELINE" GATE_THRESHOLD_PCT="$THRESHOLD_PCT" GATE_CEILINGS="$CEILINGS" GATE_MARKER="$MARKER"
python3 - <<'EOF'
import json, os, subprocess

marker = os.environ["GATE_MARKER"]
threshold = float(os.environ["GATE_THRESHOLD_PCT"]) / 100.0
ceilings_path = os.environ["GATE_CEILINGS"]

out = subprocess.run(
    ["find", "target/criterion", "-path", "*/change/estimates.json", "-newer", marker],
    capture_output=True, text=True, check=True,
)
change_files = sorted(p for p in out.stdout.split() if p.strip())
if not change_files:
    print("error: no fresh baseline comparisons found; bad filter or bench run failed?", flush=True)
    raise SystemExit(2)

ceilings = {}
if ceilings_path:
    with open(ceilings_path) as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            bench_id, cap = line.split()
            ceilings[bench_id] = float(cap)

failures, rows = [], []
for path in change_files:
    bench_id = path[len("target/criterion/"):-len("/change/estimates.json")]
    with open(path) as f:
        change = json.load(f)["median"]
    rel, lo = change["point_estimate"], change["confidence_interval"]["lower_bound"]
    status, why = "ok", ""
    if rel > threshold and lo > 0.0:
        status = "FAIL"
        why = f"median +{100*rel:.2f}% > +{100*threshold:.0f}% and 95% CI above zero"
        failures.append(bench_id)
    if bench_id in ceilings:
        new_path = path[: -len("change/estimates.json")] + "new/estimates.json"
        with open(new_path) as f:
            median_ns = json.load(f)["median"]["point_estimate"]
        if median_ns > ceilings[bench_id]:
            status = "FAIL"
            why += f"; median {median_ns:,.0f}ns > ceiling {ceilings[bench_id]:,.0f}ns"
            if bench_id not in failures:
                failures.append(bench_id)
    rows.append((bench_id, 100 * rel, 100 * lo, status, why))

for bench_id, pct, lo, status, why in rows:
    print(f"[{status}] {bench_id}: change {pct:+.2f}% (95% CI lower {lo:+.2f}%) {why}")

if failures:
    print(f"\n{len(failures)} benchmark(s) regressed beyond +{os.environ['GATE_THRESHOLD_PCT']}% vs baseline '{os.environ['GATE_BASELINE']}':")
    for b in failures:
        print(f"  - {b}")
    print("If the slowdown is expected, re-mint the baseline (see benches/README.md).")
    print("On shared runners, re-run once on a quiet host before concluding.")
    raise SystemExit(1)
print(f"\nAll {len(rows)} benchmark(s) within +{os.environ['GATE_THRESHOLD_PCT']}% vs baseline '{os.environ['GATE_BASELINE']}'.")
EOF
