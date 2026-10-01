# Benchmarks and comparisons

Converra's comparisons answer several different questions: whether its ledger is correct, whether a field kernel agrees with an independent implementation, and what a bounded search changes relative to a stated baseline. Read the inputs and acceptance scope before comparing headline figures.

## Synthetic allocation: a hand-checkable optimum

[OC-001](https://github.com/AvilaLabs/Converra/blob/main/docs/BENCHMARK.md) searches 4,096 grade/count combinations for three fixed modules. All prices and conductor capacities are invented. Fields are prescribed rather than recalculated from allocation.

| Cost | Uniform premium baseline | Mixed allocation |
| --- | ---: | ---: |
| Installed conductor | $720 | $600 |
| Scrap | $72 | $60 |
| Assembly | $90 | $90 |
| Joints and changes | $50 | $74 |
| Total | $932 | $824 |

The mixed allocation saves $108, or 11.59% of $932, under those assumptions. Full enumeration and the independent hand derivation establish the optimum within this finite synthetic problem. The result checks allocation, constraints and arithmetic; it does not predict customer savings.

## A built magnet: Feather-M2

[OC-010](https://github.com/AvilaLabs/Converra/blob/main/docs/OC010.md) compares equivalent tape consumption with CERN's Feather-M2. The reference is approximately 190 m of 12 mm-equivalent tape, derived from published design quantities. The comparison targets the achieved 3.1 T operating specification.

The closest coarse-screened candidate uses 184 m: about 0.97 times the approximate reference. Its refined check remained `INCONCLUSIVE` at a tape-edge self-field point, and campaign-level gates did not produce a passing recommendation. This is a scoped conductor-quantity comparison with an approximate reference, rather than a qualified replacement design or a demonstrated cost reduction.

## Independent field calculation: Bluemira

[OC-011](https://github.com/AvilaLabs/Converra/blob/main/docs/OC011.md) sends the same declared geometry, ampere-turns and probe deck through Converra and an independent Bluemira kernel.

The historical finite-cross-section comparison reports maximum relative field-magnitude differences of 2.2 × 10⁻⁶, 2.8 × 10⁻⁶ and 4.1 × 10⁻⁶ across three optimum-cell probe sets. Its predeclared magnitude gate was 10⁻³, with a near-zero absolute floor and a separate direction gate. A relative difference of 2.8 × 10⁻⁶ is about 2.8 parts per million.

Both implementations use the same uniform-current physical model. The agreement supports that kernel on those inputs; it does not compare conductor qualification, optimization quality or complete engineering workflows. The historical records are referenced under ignored `runs/`; the newer workflow harness did not rerun Bluemira and found no installed independent runtime in its inspected environments.

## Coupled cost refinement and a blocked helix

[OC-008](https://github.com/AvilaLabs/Converra/blob/main/docs/OC008.md) records 22.792% lower modeled cost than the original OC-007 baseline after bounded refinement. The prices are synthetic and the percentage is conditional on that baseline and screening model.

[OC-031](https://github.com/AvilaLabs/Converra/blob/main/docs/OC031.md) records no passing helix design in its declared candidate set. It identifies current-capacity and material-coverage blockers. Retaining this negative result helps show where the supported inputs stop resolving the problem.

## Workflow comparison: what was actually measured

The [workflow comparison](https://github.com/AvilaLabs/Converra/blob/main/docs/WORKFLOW_COMPARISON.md) compares Converra's explicit-file CLI path with its shared-study API on three frozen fixtures. Both use the same inputs and one solver thread. It measures software operations and timings.

All base/revised records were semantically equal after removing only allowlisted time/runtime fields. All three fixtures ended with failed searches; no selected geometry or cost was produced. Internal agreement passes in those records are separate from search success.

The shared-session exact repeats took 0.022–0.055 s because they reused verified completed results. The CLI repeats took 46.034–118.069 s because they launched new processes and solved again. These figures compare cache reuse with fresh calculation; they cannot establish a fresh-solver speedup. The single host observation did not measure human setup or review effort.

## How to read a competitive claim

There is currently no matched end-to-end benchmark against COMSOL, Ansys, Allsolve or another commercial platform in this evidence. A fair comparison needs the same requirement, materials, geometry, numerical fidelity, prices and acceptance criteria, with fresh calculations distinguished from cached work. The existing evidence supports the narrower numerical and workflow questions above.
