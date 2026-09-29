# Sensitivity sweeps

A sweep answers the robustness question a screening result alone cannot:
*does the optimum survive a pessimistic tape lot, a warmer operating point, or
a different price?* One spec file declares the axes; each grid point is an
ordinary coupled search of the mutated case — the same physics, statuses and
independent acceptance as the base run.

## Spec format (`optcoil-sensitivity/v1`, `/v2`, `/v3`)

```json
{
  "schema": "optcoil-sensitivity/v2",
  "id": "my-sweep",
  "provenance": "Why these perturbations, at what confidence.",
  "axes": [
    { "kind": "ic_scale", "values": [0.9, 1.0, 1.1] },
    { "kind": "temperature_k", "values": [19.0, 21.0] },
    { "kind": "price_usd_per_m", "values": [50.0, 62.5, 75.0] },
    { "kind": "requirement_b_target_t", "values": [4.0, 5.8] }
  ]
}
```

The grid is the full product of the axes (≤ 64 points). Per axis:

- **`ic_scale`** — multiplies every Ic in the base material dataset (values in
  `(0, 4]`). The scaled table is a distinct, declared-synthetic identity —
  it is never presented as measured data. Explicit graded tape specifications
  retain their own declared datasets.
- **`temperature_k`** — replaces `operating.temperature_k`. Each value must
  stay strictly inside the dataset's nominal temperature span; a value
  outside fails that point as an error, it is not clamped or extrapolated.
  On a schema-v20 case declaring `opex`, this axis becomes the
  capex-vs-opex frontier: colder points buy tape capacity (and often a
  cheaper magnet) but pay the Carnot-scaled refrigeration bill — read each
  point's `lifecycle_usd`, not just `total_usd`.
- **`price_usd_per_m`** — replaces `cost.price_usd_per_m`. Physics verdicts
  are price-independent; this axis moves dollars only.
- **`requirement_b_target_t`** *(v2+)* — replaces
  `requirement.b_target_t`: the certified-ceiling question, "is this
  winding verifiably holdable at this field?", run as a declared grid of
  full searches. The response need not be monotone — dataset low-field
  floors can leave lower targets `INCONCLUSIVE` while higher ones pass — so
  the certified ceiling is read as the **largest declared value whose
  `search_status` is `PASS`**, at declared-grid resolution, never a
  bisected or continuous bound. Each point still carries its own
  independent acceptance recomputation.
- **`utilization_limit`** *(v3 only)* — replaces
  `limits.utilization_limit`, strictly within `(0, 1)`: the
  capacity-margin frontier, "what does headroom cost?" asked by sweeping
  the declared utilization gate. A tighter bound can remove the cheapest
  passing geometry outright, so modeled cost can *step* rather than
  slope — read the frontier at declared-grid resolution, never
  interpolated. The axis perturbs the declared screening bound only; it
  does not change what the tape measured.

Axes compose. Declaring `ic_scale` **and** `requirement_b_target_t` in one
sweep produces the specimen-spread answer directly: group the record by
`ic_scale` value and read each row's certified ceiling — the ceiling at
`ic_scale = 0.9` is what the verdict is worth against a −10% tape lot,
computed without a single fabricated measurement (the scaled dataset is a
distinct, declared-synthetic identity bound by its own `dataset_csv_sha256`).

## Running

```bash
optcoil sensitivity case.json sweep.json \
  --output runs/my-sweep.json      # complete record, all point results
optcoil sensitivity case.json sweep.json --threads 4
optcoil sensitivity case.json sweep.json --dataset-bundle my-tape.bundle.json
```

`--threads` may only *lower* the case's declared `execution.max_threads`.
External datasets work exactly as in `coupled-search` — every point binds the
same declared identity.

The console prints one line per point: axis values, optimum geometry, search
and acceptance status, and savings where a PASS optimum exists. The `--output`
record stores each point's **mutated case JSON verbatim** plus its
`case_sha256`, `dataset_id` and `dataset_csv_sha256` — every point is
independently re-runnable through `coupled-search` to regenerate its complete
candidate-level evidence.

## Reading it

A sweep record is not itself a certification. A point whose search finds a
PASS optimum says the optimum *survives* that perturbation under the same
screening semantics; `FAIL`/`INCONCLUSIVE`/`NOT_EVALUATED` statuses carry
their ordinary meanings and are never collapsed into a single "robust" label.
Errors (e.g. a temperature outside the dataset span) are recorded per point,
not fatal to the sweep.
