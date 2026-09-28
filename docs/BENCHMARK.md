# OC-001: synthetic fixed-module allocation

Fixture: [`benchmarks/synthetic/oc-001.json`](../benchmarks/synthetic/oc-001.json).

**Purpose:** verify an end-to-end manufacturing-allocation search, quantity accounting, constraint handling and repeatable results. Every price and material/operating value is invented. This is not a validated HTS magnet or an estimate of customer savings.

## Fixed requirements

Three separately wound modules carry 1,000 A in series. Each has 10 turns of 2 m mean length and 1–8 parallel tapes. The utilization ceiling is 80% of the stipulated tape-stack current capacity. Fields are prescribed at 2, 6 and 10 T; temperature is 20 K and field angle to the tape surface is zero. Fields do not respond to allocation changes in this benchmark.

Two synthetic grades are allowed in every module:

| Grade | Price / m | Stipulated tape capacity |
| --- | --- | --- |
| Standard | $2 | `400 * (1 - 0.05 * B)` A |
| Premium | $4 | `650 * (1 - 0.025 * B)` A |

These expressions are supported only over 0–12 T, exactly 20 K and exactly zero degrees for this fixture. Ideal equal sharing is assumed. The minimum tape counts satisfying the fixed current requirement are:

| Module | Standard | Premium |
| --- | --- | --- |
| 2 T | 4 | 3 |
| 6 T | 5 | 3 |
| 10 T | 7 | 3 |

Purchased tape adds 10% of installed length as scrap allowance. Assembly is $30 per module. Each of the two interfaces costs $25, plus $12 if grade or tape count changes across it. A change fee is charged once per interface even if both change.

## Baseline and optimum

The baseline is three premium tapes in every module. It is the cheapest feasible uniform grade/count allocation in this model: two premium tapes fail at 10 T, while seven standard tapes everywhere cost more.

| Cost component | Uniform premium baseline | Mixed optimum |
| --- | ---: | ---: |
| Installed conductor | $720 | $600 |
| Scrap | $72 | $60 |
| Assembly | $90 | $90 |
| Joints and changes | $50 | $74 |
| Total | **$932** | **$824** |

The optimum is standard × 4, standard × 5, premium × 3, in module order. Installed tape lengths are 80, 100 and 60 m; purchased lengths are 88, 110 and 66 m. Allowed stack currents at 80% utilization are 1,152, 1,120 and 1,170 A, giving margins of 152, 120 and 170 A.

At the minimum feasible counts, the eight grade patterns have totals $868, $824, $912, $856, $956, $912, $988 and $932 for SSS, SSP, SPS, SPP, PSS, PSP, PPS and PPP respectively. Adding a tape costs at least $44 including scrap. Removing both possible change charges saves at most $24, so extra tapes cannot improve any pattern here. This gives a hand-checkable optimum independent of the enumerator.

There are `(2 grades * 8 counts)^3 = 4096` raw combinations. Modeled savings are `$108 / $932 ≈ 11.59%`. Full exhaustion establishes an optimum only within this finite synthetic screening model.

## Regression requirements

- Exact baseline, allocation, ledger, total cost and search-space count match the derivation above.
- Forbidding allocation changes recovers the uniform baseline. High transition prices eliminate the incentive to change allocation.
- Outside-domain material capacity remains inconclusive; it cannot produce an optimality claim or fabricated capacity value.
- A truncated or cancelled search retains its incumbent and cannot claim optimality.
- The accepted candidate's capacity margins are nonnegative, but missing engineering checks remain `NOT_EVALUATED`.
- Input identities are repeatable and change when case/settings change. CLI records match stdout JSON and cannot overwrite earlier output.

The analytical circular-loop reference is a separate numerical test: at radius 1 m and 1,000 A-turns, Bz is approximately 0.000628318530718 T at the center and 0.000222144146908 T one meter along the axis. It uses the conventional permeability value `4*pi*1e-7 H/m` and a filament approximation. It does not compute OC-001's input fields.
