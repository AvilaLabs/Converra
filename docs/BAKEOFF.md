# Vendor bake-off

A bake-off answers the procurement question a single-dataset case cannot:
*which vendor's measured tape gives the cheapest screening-passing design
for this requirement*. One spec file declares the datasets; each entry runs
an ordinary coupled search of the case mutated only in
`material.dataset_id` / `material.csv_sha256` — the same physics, statuses
and independent acceptance as the base run, like-for-like on every declared
policy.

## Spec format (`optcoil-bakeoff/v1`)

```json
{
  "schema": "optcoil-bakeoff/v1",
  "provenance": "Why this comparison, which vendors and why.",
  "datasets": [
    { "dataset_id": "robinson-superpower-ap-v3" },
    { "dataset_id": "robinson-shanghai-hflt-v3" },
    { "dataset_id": "my-customer-tape", "bundle": "my-tape.bundle.json" }
  ]
}
```

1–16 datasets, ids distinct. Each entry resolves from the embedded registry
by `dataset_id`, or from `bundle` — a path to an `optcoil-material-dataset`
v1/v2 bundle, resolved relative to the spec file — for customer/QC data.
Only the dataset identity mutates per row; `low_field_clamp_t`, field
basis, angle mapping, width transfer and every screen stay at the case's
declared values so rows compare honestly.

## Spec format (`optcoil-bakeoff/v2`) — product comparison

v2 answers the *purchasing* question: which product gives the cheapest
accepted design. Entries are products carrying the attributes a real
product brings — its manufactured width, its price, its provenance:

```json
{
  "schema": "optcoil-bakeoff/v2",
  "provenance": "Why this comparison, which products and why.",
  "products": [
    {
      "product_id": "acme-4mm-st",
      "dataset_id": "acme-4mm-measured",
      "tape_width_m": 0.004,
      "tape_thickness_m": 0.000095,
      "price_usd_per_m": 42.0,
      "price_source": "quoted",
      "piece_offerings": [
        { "length_m": 300.0, "price_usd_per_m": 43.5 },
        { "length_m": 800.0, "price_usd_per_m": 42.0 }
      ]
    }
  ]
}
```

Per row the runner mutates: the dataset binding; `tape_width_m` when
declared (a different winding topology — axial extent is `tapes × width`,
so a 4 mm product is a physically different coil, not a relabeled row);
`tape_thickness_m` into `manufacturing` (requires the case's declared
manufacturing block); `cost.price_usd_per_m` and `cost.price_source`;
`cost.piece_offerings` (requires the case's `cost.piece_policy`); and any
`material_policy` overrides — every policy field except the dataset-bound
`method`. The mutated case is re-validated before it runs; an invalid
combination records an `error` row with the reason.

Requirements and refusals, deliberately:

- The case must already be schema v24 — product prices write the
  v24-gated `price_source` provenance field, and silently upgrading a
  case's schema is a runner edit, not a declared input.
- Graded cases (`grading`/`tape_specs`) are refused: one product price
  cannot reprice per-spec selections honestly. v2 compares uniform
  products; graded multi-product comparison is future work.
- `price_usd_per_m` and `price_source` are required — a procurement
  comparison without a price is a materials table, and an unlabeled
  price is how placeholders get mistaken for quotes.

v2 ranks `PASS` rows by the accepted optimum's `optimum_total_usd`
ascending — the purchasable figure — then `INCONCLUSIVE`, then `FAIL`;
the ranking names `product_id`s. Rows are *different physical cases*
answering the same requirement: they compare through the declared
baseline topology, not identical windings. Whether a product's dataset
supports its declared width (bridge-to-full-width transfer) remains the
dataset's own provenance question — the record says so.

## Running

```bash
optcoil bakeoff case.json bakeoff.json \
  --output runs/my-bakeoff.json    # complete record, all entries
optcoil bakeoff case.json bakeoff.json --threads 4
```

`--threads` may only *lower* the case's declared `execution.max_threads`.
The console prints one line per dataset — status pair, optimum geometry,
modeled savings, the optimum's `max_utilization` (the operating margin the
price buys) — then the ranking.

The `--output` record stores each entry's **mutated case JSON verbatim**
plus `case_sha256` and the resolved `dataset_csv_sha256`, so every row is
independently re-runnable through `coupled-search` to regenerate its full
candidate-level evidence.

## Reading the record

`ranking` is mechanical, not a verdict on tape quality: `PASS` entries by
modeled `savings_usd` descending, then `INCONCLUSIVE`, then `FAIL`; entries
whose dataset could not resolve or whose mutated case failed validation
carry `error` and are unranked. A `FAIL` row means no candidate passed the
declared screening under that dataset — it is a boundary of what this case
asks, not a defect claim. All cost figures are modeled under the case's
declared prices; invented placeholders carry no supplier basis.

A bake-off *cannot* certify outside each dataset's own measured domain —
the same coverage wall applies row by row, and `INCONCLUSIVE` remains the
honest answer where measured data runs out.
