# Your first study — fifteen minutes with Converra

A guided walk through a real coupled search: open a shipped case, run
it, read the verdicts, and export the evidence. Everything below uses
files already in the repository — no external data needed.

The example is **OC-007**, the benchmark the cost-search pipeline was
built against: a 0.9 T / 80 mm-bore racetrack dipole spec screened
against *measured* SuperPower REBCO data (Robinson Institute,
CC-BY). Its prices are declared synthetic — read the numbers as
"modeled", not quoted.

## 1. Run the search

From the repository root:

```bash
cargo run --release -- coupled-search benchmarks/coupled/oc-007.json \
  --output runs/oc-007-first.json --threads 4
```

This takes a few minutes in a release build (it is the 15-candidate
exhaustive grid). You'll see one line per candidate and a summary like:

```
  120 turns × 4 tapes   PASS    $41,513.13
```

The two verdicts that matter:

- **`search_status: PASS`** — the best candidate met every declared
  requirement under the screening model.
- **`acceptance` agreement `PASS`** — an *independent* recomputation
  module (separate cost ledger, separate screening walk, finer refined
  sampling plan) reproduced the result. This is the project's core
  contract: the search never grades its own homework.

## 2. Read what it found

Open the record in the workbench:

```bash
cargo run -p optcoil-app
```

Then File → Open → `runs/oc-007-first.json`. The Overview page shows the
baseline vs. optimum cost comparison; the candidate table lists all 15
pack geometries colored by verdict.

Click any candidate — the detail panel shows per-check verdicts
(requirement, refinement, screening, mechanical) and, under them, the
**reason the verdict closed**. FAIL and INCONCLUSIVE always name their
limiter, e.g.:

- `over the 0.80 utilization limit (1.57 at station p2)`
- `7 point(s) sit below the dataset floor — bounded, not determined`

## 3. Learn to read INCONCLUSIVE

This is the verdict most first-time users misread. It is not a failure
— it is a declared *unresolved* region:

- **below the dataset floor** — the tape's measured characterization
  stops around 1 T (the robinson dataset); a point at 0.9 T gets a
  conservative bound, and the candidate stays unresolved rather than
  certified on extrapolation.
- **outside the measured domain** — the point's field/angle/temperature
  exceed anything the lab measured.
- **along-current excluded** — the field points mostly along the tape's
  length, where the screening policy refuses to claim a capacity.

Try it: `benchmarks/coupled/oc-031-helix-layer.json` (the non-planar
helical-layer case — fast, ~4 candidates) was built to hit exactly this
boundary: thick packs settle at utilization 0.78 yet stay INCONCLUSIVE
because helix-end points dip under the dataset's field floor. The
detail panel names all three blockers it produced.

## 4. Export and verify

```bash
# Human-readable evidence digest — open runs/oc-007.html in a browser.
cargo run --release -- report runs/oc-007-first.json --output runs/oc-007.html

# Independent artifact check: hash bindings + ledger arithmetic.
cargo run --release -- verify runs/oc-007-first.json benchmarks/coupled/oc-007.json

# Modeled procurement document for the optimum.
cargo run --release -- bom runs/oc-007-first.json --output runs/oc-007.bom.json
```

`verify` prints `PASS` per check and — honestly — `NOT_CHECKED` for
physics: it confirms the record is internally consistent and bound to
the case file, not that the physics is right.

## 5. Change one knob

The fastest way to build intuition is to move one declared bound:

1. Copy `benchmarks/coupled/oc-007.json` to `runs/my-case.json`.
2. Edit `limits.utilization_limit`: `0.8` → `0.95`.
3. Re-run. More candidates pass — the optimum shifts, and the record
   shows you exactly which limiter released it.

Or reprice the existing record without rerunning physics:

```bash
cargo run --release -- reprice runs/oc-007-first.json \
  --price-usd-per-m 80 --output runs/oc-007-at-80.json
```

## Where to go next

- `docs/SUPPORTED_DOMAINS.md` — which field/temperature/angle ranges the
  embedded datasets actually cover (the honest envelope).
- `docs/OC010.md` — the physically-built-coil parity study (CERN
  Feather-M2, ~3% tape-mass agreement).
- `docs/OC031.md` — non-planar helical winding under a declared field map.
- `benchmarks/coupled/` — 20+ authored cases covering graded packs,
  piece procurement, vendor bakeoffs and the high-field regimes.
- `docs/BENCHMARK.md` — how the benchmark suite is organized.
