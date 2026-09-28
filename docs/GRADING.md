# Grading comparison report

A graded case (`optcoil-coupled-search/v10+`: `tape_specs` +
`grading.regions`) searches mixed assignments — different conductor specs
on different winding regions — and its run record already contains every
uniform alternative the grid explored. `optcoil gradereport` reads a
completed run record and quantifies what grading bought: *premium tape
only where the field demands it*, as a modeled dollar figure.

## Running

```bash
optcoil coupled-search case.json --output runs/graded-run.json
optcoil gradereport runs/graded-run.json \
  --output runs/graded-report.json   # optcoil-grading-report/v1
```

The command never recomputes: the report binds `source_record_sha256`
(SHA-256 of the record file bytes), so every figure traces to candidates
the record's own acceptance recomputation checked. An ungraded record —
no per-region assignments anywhere — is rejected: there is nothing to
compare.

## Reading the report

- **`uniform_rows`** — per spec id, the cheapest PASS candidate using
  that spec on *every* region ("one vendor everywhere"). `base` is the
  case-level material binding; named ids resolve through the record's
  `spec_datasets` to the dataset that actually ran. A spec with no
  passing uniform row is absent — that is data, not a report error.
- **`optimum_assignment`** — the winner's resolved per-region spec ids.
- **`optimum_is_uniform`** — true when the optimum needed no mix; the
  delta is then 0 by construction (grading expanded the grid but bought
  nothing this case required).
- **`grading_delta_usd` / `grading_delta_percent`** — best-uniform cost
  minus optimum cost: the modeled value of mixing specs across regions.
  Nonnegative by construction — the graded grid strictly contains the
  uniform assignments, so the optimum can never be worse.

All figures are modeled under the case's declared prices — invented
placeholders carry no supplier basis. A spec id is a declared
material+price binding, not a supplier quote or a stock-keeping unit.
