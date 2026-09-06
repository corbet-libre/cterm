# cterm-core benchmarks and the performance regression gate

Deterministic Criterion suite for the terminal core: `cargo bench -p cterm-core`
runs two bench binaries (default behavior is pinned and unchanged by the gate;
see "Fast smoke runs" for the only override mechanism).

## Inventory

All workloads are fixed loop-constructed byte streams (no RNG, no clock, no I/O,
no threads in the workload). Each iteration parses into a fresh `Screen(80x24)`
+ `Parser` via `iter_batched(..., BatchSize::LargeInput)`, with `black_box` on
the input slice and on the resulting cursor/line counts, so the optimizer cannot
elide the work and no state leaks across iterations. Parser benches report
`Throughput::Bytes`; resize/reflow benches report time only. Pinned config for
every group: 40 samples, 2 s warm-up, 5 s measurement.

Note: there is no dedicated GPU/renderer bench; "render" coverage is the
full-screen TUI repaint workload (`alternate_screen_redraw`) plus the
resize/reflow benches below.

### `parser` (benches/parser.rs) — byte-stream parse throughput, no scrollback

| Benchmark | What it measures | Corpus shape (~size) |
|---|---|---|
| `parser/mixed_session/cterm-core` | Representative shell session: colored `ls`-style lines + periodic full clears | 3,000 SGR lines + `ESC[2J` refresh every 64 lines (~225 KB) |
| `parser/ascii_plain/cterm-core` | Plain printable ASCII, no escape sequences (parser fast path) | 4,000 x ~71 B lines (~275 KB) |
| `parser/sgr_churn/cterm-core` | Worst-case attribute churn: 256-color SGR before every byte | 2,500 x 34 styled cells (~950 KB) |
| `parser/scroll_storm/cterm-core` | Viewport scrolling throughput: short lines, constant scroll | 24,000 short lines (~260 KB) |
| `parser/alternate_screen_redraw/cterm-core` | Full-screen TUI repaint: 400 addressed frames on the alt screen | 400 x 24 cursor-addressed rows (~630 KB) |
| `parser/unicode_wide_and_graphemes/cterm-core` | Wide/Unicode path: CJK, combining marks, ZWJ emoji, flags | 3,000 mixed-grapheme lines (~300 KB) |

### `resize_reflow` (benches/resize_reflow.rs) — viewport resize + scrollback reflow

| Benchmark | What it measures |
|---|---|
| `screen_resize/filled_viewport_80x24_to_120x40` | Resize round-trip 80x24 -> 120x40 -> 80x24 on one addressed screen, no scrollback |
| `screen_reflow/wrapped_ascii_scrollback` | Three resizes (120x40, 60x24, 80x24) over 10,000 scrollback lines of long wrapped ASCII |
| `screen_reflow/wrapped_unicode_grapheme_scrollback` | Same, over 10,000 scrollback lines of wide/multi-codepoint graphemes (grapheme-safe reflow path) |

Remaining nondeterminism is all host-side (CPU frequency/turbo, thermals,
co-tenants on shared runners, allocator layout) -- hence the gate compares
same-host relative change against a locally minted baseline instead of
committing absolute numbers.

## Commands

```bash
cargo bench -p cterm-core                                    # full suite (minutes locally; CI's job)

scripts/check_perf_baseline.sh mint --baseline main          # mint host-local baseline (target/, gitignored)
scripts/check_perf_baseline.sh check --baseline main         # fail (exit 1) on regression > 10% (see scheme)
scripts/check_perf_baseline.sh check --baseline main --quick # seconds-long smoke round-trip (also add --quick to mint)

# Subsets / knobs:
scripts/check_perf_baseline.sh check --benches "resize_reflow" -- screen_resize   # one binary + Criterion filter
scripts/check_perf_baseline.sh check --threshold-pct 15 --ceilings /path/ceilings.tsv
```

`check` exit codes: 0 = within threshold, 1 = regression beyond threshold,
2 = no baseline minted / nothing compared / usage error. Bench IDs in `check`
output and in `--ceilings FILE` are Criterion's directory form (e.g.
`parser_mixed_session/cterm-core`, `screen_reflow/wrapped_ascii_scrollback`):
copy them from `check` output verbatim.

## Threshold scheme (hardware-normalized)

1. **Relative change vs a same-host baseline (the portable gate).** FAIL when a
   benchmark's median time regresses more than `--threshold-pct` (default 10)
   AND its 95% confidence interval sits entirely above zero. The magnitude
   threshold absorbs shared-runner jitter; the CI-above-zero requirement
   absorbs measurement noise that Criterion itself flags as insignificant.
   Default 10% was chosen because single-host reruns of this suite vary ~1-3%
   (see calibration), leaving ample headroom above noise without hiding real
   regressions.
2. **Absolute ceilings per bench (optional, host-specific, never committed).**
   `--ceilings FILE` caps per-bench median nanoseconds (TSV
   `<bench-id><TAB><max-median-ns>`). Mint per runner from a known-good run's
   `target/criterion/<id>/new/estimates.json` medians plus headroom; keep the
   file next to the runner config, not in the repo.
3. **Warm-up discipline.** Bench sources pin 2 s warm-up / 5 s measurement /
   40 samples, matching what CI runs. The Criterion CLI flags for these are
   *ignored* (programmatic group settings take precedence -- verified); the
   only override is `CTERM_BENCH_SAMPLE_SIZE` / `CTERM_BENCH_WARMUP_SECS` /
   `CTERM_BENCH_MEASUREMENT_SECS`, which default to the pinned values so plain
   `cargo bench` is byte-for-byte the old behavior. `--quick` sets 10 / 1 / 1.
4. **Noise handling on shared runners.** Mint and check on the same host at
   quiet load; on a `FAIL`, re-run `check` once before concluding (the script
   prints this reminder). The CI proposal below confirms a breach with a
   second run.
5. **No committed numbers.** Baselines live under `target/criterion/`
   (gitignored). Minting procedure: clean tree (usually main) -> `mint`
   -> `check` passes trivially -> keep that `target/` for branch comparisons,
   or re-mint on the CI runner each gate run (see proposal).

## Calibration (this host only -- do NOT treat as thresholds)

Single full-default run, 2026-09-06, shared LXC host (`corbet-archlxc`,
32 vCPU, 125 GB RAM, rustc 1.98.0, co-tenants active): medians --
`mixed_session` 17.2 ms (13.4 MiB/s), `ascii_plain` 31.2 ms (9.0 MiB/s),
`sgr_churn` 31.6 ms (31.0 MiB/s), `scroll_storm` 48.4 ms (5.5 MiB/s),
`alternate_screen_redraw` 50.8 ms (12.7 MiB/s),
`unicode_wide_and_graphemes` 26.2 ms (11.7 MiB/s),
`filled_viewport_80x24_to_120x40` 470 us,
`wrapped_ascii_scrollback` 183 ms, `wrapped_unicode_grapheme_scrollback`
59.3 ms. Outliers per bench 0-17.5% (high-only, typical of a loaded shared
host); reruns on the same host varied ~1-3%. Absolute values are meaningless
off this machine -- the gate only compares same-host relative change.

`--quick` (10 samples, 1 s warm-up/measurement) verifies gate mechanics only,
not regressions: on the same loaded host, back-to-back quick round-trips swung
up to +63% minute-to-minute (all parser benches within +/-3% on the rerun,
`wrapped_ascii_scrollback` still +18%), while the next back-to-back
mint-then-check passed all 9. Regression verdicts require full pinned runs
plus the re-run discipline above.

## Proposed CI gate (NOT applied -- needs Julian's go; do not touch `.github/workflows` without it)

```yaml
# benchmarks.yml, new job after the existing report upload (sketch):
perf-gate:
  runs-on: ubuntu-24.04
  steps:
    - uses: actions/checkout@v7
      with: { ref: ${{ github.event.pull_request.base.ref || 'main' }} }  # or: mint on main, check on PR head
    - run: scripts/check_perf_baseline.sh mint --baseline pr-base
    - uses: actions/checkout@v7  # PR head
    - run: scripts/check_perf_baseline.sh check --baseline pr-base
      # on failure: re-run once to confirm (shared-runner noise), then fail the job
```

Decisions for Julian: (a) gate on PRs touching `crates/cterm-core/**`
(path-gate, like `benchmarks.yml` today) vs weekly-main only; (b) threshold
(10% default ok?); (c) breach policy -- fail the job vs non-blocking comment;
(d) whether to upload `target/criterion/` as the existing 30-day HTML artifact
already covers, or extend retention for minted baselines. Until decided, the
gate stays local-only: `mint` + `check` as documented above.
