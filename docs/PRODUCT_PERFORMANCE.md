# Product performance check

This note records the release measurements used to assess cooperative
cancellation and the acceptance rerun reuse in the 2026-09-29 product pass.
The change does not alter the field kernel or search criteria. Run records
still count search kernel work; they do not include the acceptance module's
independent rerun work in that counter.

## Acceptance rerun reuse

When the selected best index is exactly the baseline index, acceptance now
reuses the baseline's independently recomputed candidate result and relabels
the copy as `best`. The baseline still receives a fresh cost, field, screening,
and refined-plan recomputation. A different candidate index still receives its
own independent recomputation.

The before/after software fixture was generated from
`benchmarks/coupled/first-study.json` (SHA-256
`2ffd17fe14ebfd902ecccb252c616a3478da927da9117864a03034c70531f4bf`). It
keeps the same measured material, geometry, quadrature orders, and sampling,
reduces the choices to the 60-turn baseline, and sets the self-field ratio
screen to 1.0 so this software-only case reaches the baseline-equals-best
acceptance path:

```bash
jq '.choices.turns_along_normal = [60] |
    .baseline.turns_along_normal = 60 |
    .limits.max_self_field_ratio = 1.0' \
    benchmarks/coupled/first-study.json > /tmp/product-acceptance-reuse.json
```

The generated input SHA-256 is
`87a7a97b059eb0bfe7d0c53ad6ce33908c48536ac08e91e107d991f5ac5778c0`. This
fixture uses the case's invented $30/m conductor price and a deliberately
broader self-field screen. Its result is software-path evidence only; it is
not a customer design or a claim that the broadened screen is physically
adequate. It enumerates one 60-turn × 2-tape candidate, uses quadrature
orders `[6, 8]`, the original 0.0025 field-refinement gate, and the unchanged
coarse and 10-station refined sampling plans.

| Release run | Acceptance implementation | Run elapsed | CLI wall time | Search kernel evaluations | Search / acceptance |
| --- | --- | ---: | ---: | ---: | --- |
| Before | baseline binary `f7da4c980842328f88b8fcd7871ca2834085befa488eb9525dea6181202c5143` | 89.928 s | 90.03 s | 155,675,520 | PASS / PASS |
| After | binary `a572b32a89bd99bc551f8461e68ca107c4c812185413cff44b529474f8e13529` | 47.484 s | 48.01 s | 155,675,520 | PASS / PASS |

Both runs selected candidate 0 as both baseline and best. Both reported the
same candidate status (`PASS`), cost ($10,928.28), screening and refined-plan
results. The top-level kernel counter is unchanged because it covers the
search candidate calculation, not acceptance work. The release run elapsed
time fell by 47.2% in this single software-fixture comparison after removing
the duplicate independent acceptance rerun. This measures that path on this
machine; it is not a general speed guarantee.

These binaries recorded acceptance checker identity v18. The v19 bump records
the baseline-equals-best result reuse and progress-leg behavior; the binary
hashes above remain the evidence for the v18 measurements. The identity bump
did not change the measured arithmetic or candidate verdicts, and no v19
performance measurement is claimed here.

## Two-candidate measured case

The unmodified two-candidate study was also run to retain the representative
measured-data behavior. It declares a 0.1 self-field ratio limit; the two
candidate diagnostics exceed it (about 0.34 and 0.39), so both candidate
screenings are `INCONCLUSIVE`, `search_status` is `FAIL`, and independent
acceptance agreement is `PASS`. No optimum is selected, so this case does not
exercise baseline-equals-best reuse. It enumerates 60 × 2 and 80 × 2 turns and
tapes at `[6, 8]` quadrature orders with the same 0.0025 refinement gate.

| Release run | Run elapsed | Search kernel evaluations | Candidate costs | Search / acceptance |
| --- | ---: | ---: | --- | --- |
| Copied pre-change baseline | 32.149 s | 295,353,968 | $10,928.28; $14,171.04 | FAIL / PASS |
| Cancellation-enabled, before reuse | 22.520 s | 295,353,968 | $10,928.28; $14,171.04 | FAIL / PASS |
| After reuse | 28.193 s | 295,353,968 | $10,928.28; $14,171.04 | FAIL / PASS |

The statuses, candidate costs, and search kernel counts match exactly. The
single-run times vary substantially even though the host is the same, so these
numbers do not support a performance claim for the two-candidate study.

## Measurement environment

- Intel Core i3-N305, 8 cores; release profile; two search threads.
- Rust `1.95.0` (`59807616e`, 2026-04-14); `Cargo.lock` SHA-256
  `0ecedc6af013873c55f3145d863e26490788c370c2c52daccf7745eed95d4062`.
- Workspace revision `25c085686b9bce0815f137fd80c5d84fcb7e25ab`; local source
  edits were present, so the binary SHA-256 values above identify the actual
  measured executables more precisely than the revision alone.

The output `elapsed_ms` is the runner's own timer. Shell wall time also
includes CLI startup and serialization. These were single runs on a shared
machine, without fixed CPU frequency or an idle-host guarantee. Candidate
kernel counts and all recorded costs/statuses provide the equivalence check;
timings describe these executions only.
