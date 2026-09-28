# Bill of materials

`optcoil bom` turns a completed coupled search into the purchasable
document its optimum implies: conductor metres per spec and per contiguous
turn range, installed vs purchased length under the declared scrap
fraction, joint and pancake counts, and the ledger cost split per spec.

## Running

```bash
optcoil coupled-search case.json --output runs/design.json
optcoil bom runs/design.json --output runs/design.bom.json   # optcoil-bom/v1
```

The BOM is derived from the record's optimum through `turn_ledger_rows` —
the same per-turn walk the production cost ledger sums — so totals can
never drift from the priced design. `totals_agree` reports the 1e-9-relative
reconciliation against the record's own ledger figures, which are stored
beside the derived ones; a disagreement is a defect, not a view difference.
A record with no `PASS` optimum is rejected — there is no design to buy.

## Reading the BOM

- **`spec_rows`** — one row per spec id the optimum actually uses
  (ungraded designs produce a single `base` row). Each carries the
  resolved `dataset_id`, declared `price_usd_per_m`, the contiguous
  `turn_ranges` it covers in winding order (1-based, inclusive; a spec
  can recur in separated regions), `turn_count`,
  `installed_length_m`, `purchased_length_m` (installed × (1 +
  scrap_fraction)), `conductor_usd` and `scrap_usd` — scrap priced at the
  spec's own price, so scrapped premium tape is scrapped at the premium
  price.
- **`joint_count` / `pancake_count`** — the ledger's own derivations:
  `tapes − 1` joints, `tapes` pancakes, with their declared costs.
  Under a v24 `piece_policy`, `joint_count` splits into
  `module_joints` + `piece_splices` + `spec_splices`.
- **`geometry`** — the optimum's full geometry (turns × tapes × strands +
  per-region assignment).
- **`total_usd`** — the BOM's recomputation of the record ledger; compare
  `record_total_usd` — `totals_agree` is the check.

## Piece procurement (schema v24)

When the case declares `cost.piece_policy`, purchased length is the
pieces actually bought, not a flat scrap multiplier:

- **`piece_length_m` / `pieces` / `piece_splices` / `remnant_length_m`**
  on each `spec_rows` entry — the chosen offering's length, the pieces
  bought at it, the in-winding splices its boundaries force, and the
  bought-but-unneeded metres beyond installed-plus-attrition.
- **`cost.piece_offerings`** is a catalogue: `{length_m, price_usd_per_m}`
  entries the ledger argmin's per spec on *spend* (pieces × length ×
  price + splice cost), so a longer premium piece can win on fewer
  splices. A `tape_specs[*].piece_offerings` catalogue overrides the
  base catalogue for that spec.
- **`boundary`** — `per_module` resets pieces at pancake boundaries and
  keeps the `tapes − 1` interface joints; `continuous` winds through
  them (0 module joints, longer runs, fewer remnant fragments).
- **`piece_unit`** — `conductor_unit` prices pieces for the whole
  `strands_parallel` stack; `per_strand` buys and splices each strand
  independently (same tape-metres, strands× the splice count).
- **`price_source`** (`synthetic`/`estimated`/`published`/`quoted`)
  labels where a price came from — synthetic stays synthetic in the
  record and BOM.

## What it is not

A modeled procurement document, not a purchase order: prices are the
case's declared figures (often synthetic — check `price_source`), and
under a piece policy the piece/splice counts follow the declared
boundary and unit semantics. Lead lengths, insulation, tooling, splice
resistance/QA yield, freight and vendor lead times are all unmodeled —
the BOM says what the *design* requires, not what a supplier will quote.
