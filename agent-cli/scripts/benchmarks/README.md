# CLI startup measurements

Requires Python 3.11+ and `pyte==0.8.2` for terminal measurements. The comparator
and its regression tests use only the standard library. No runtime package or
customer installation depends on this benchmark tooling.

```bash
uv run --with pyte==0.8.2 python agent-cli/scripts/benchmarks/startup.py \
  /path/to/baseline/cloudthinker-agent /path/to/candidate/cloudthinker-agent \
  --count 10 --output /tmp/startup-comparison.json
python3 agent-cli/scripts/benchmarks/comparison.py /tmp/startup-comparison.json
python3 -m unittest discover -s agent-cli/scripts/benchmarks -p test_comparison.py
uv run --with pyte==0.8.2 python -m unittest discover \
  -s agent-cli/scripts/benchmarks -p test_terminal.py
```

Every trial uses an isolated home and local fixture backend. It does not call a
model or measure production networking. The paired order alternates to limit
ordering bias. Only compiled ELF/Mach-O binaries qualify, and their SHA-256
digests identify the tested artifacts.

Ready means raw/no-echo input, a random ten-character probe rendered on the
emulated screen, and successful erasure with the cursor back at the exact
insertion point. Synchronized partial frames are ignored. A shortened probe,
placeholder, model label, process spawn, or raw mode alone is not readiness.
The readiness timestamp is the first rendered probe; successful cleanup is
required before the trial can be recorded. Timeout and early exit fail the run.

`calibration.json` records ten same-binary pairs measured on Linux x86_64 on
2026-09-12. Their median difference was 2.2%. The 20% floor is a conservative
noise filter, not a statistical significance test and not a universal threshold.
Recalibrate on the target environment before making a cross-machine claim.
The committed controls must continue to report `no clear change` after any
threshold change. Missing, duplicate, non-finite, or incorrect trials fail.

`compile-comparison.json` records ten pairs from the same source tree with
`--keep-names`, then with `--bytecode --minify --keep-names --format=esm`.
Median readiness was 420.4 ms versus 141.2 ms. The optimization was retained.
These artifacts were built from the in-progress adoption tree based on
`e28eb7bfb0`; the artifact hashes, not that base commit alone, identify the builds.

Do not apply this control fixture or its threshold to Code Review or SREGym.
Those workloads need their own same-arm controls and correctness checks before
timing comparisons. Live model/judge calibration is separate from this local
fixture benchmark and requires the benchmark owner's authorization.

The terminal-query and readiness algorithm is adapted from prime-agent at
`66658d2`; attribution and its MIT license are in `LICENSE`.
