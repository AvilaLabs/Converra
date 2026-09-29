# Roadmap

Where Converra is going, roughly in priority order. This is a statement
of intent, not a commitment — contributions that move any of these
forward are welcome, and the architecture docs explain where each lands.

## Product completion extension — September 2026

The next product milestone is a complete supported study: an engineer brings
a planar winding or declared field map, conductor evidence and a documented
cost basis; revises and compares alternatives; and exports a package another
engineer can review. Numerical screening, artifact agreement and production
engineering acceptance remain separate. The existing physics roadmap remains
valuable, but this workflow is the immediate implementation priority.

Implementation and verification evidence: [product completion](docs/PRODUCT_COMPLETION.md) and
[release performance measurements](docs/PRODUCT_PERFORMANCE.md).

Each item below is a tracked deliverable with an explicit acceptance gate.
Implementation completion and external validation are recorded separately;
reference fixtures cannot stand in for customer observations.

### PC-01 — Consistent costs and recommendation authority

- [x] Display the record's actual per-spec cost ledger at its declared prices,
  including grading and purchased pieces, in every headline and cost chart.
- [x] Restrict scalar repricing to compatible records and distinguish a price
  scenario's screening choice from the originally recomputed recommendation.
- [x] Reproduce the graded-cost discrepancy as a regression and verify agreement
  between workbench metrics, CLI output, BOM and HTML report.

### PC-02 — Open, revise, duplicate and compare

- [x] Edit a loaded case in the workbench, preserving every supported declaration
  and its schema when unchanged; provide an in-app advanced editor when a field
  cannot be represented faithfully by the guided form.
- [x] Save a revision or duplicate without overwriting the source or losing its
  existing result; invalidate stale derived state only after accepting a valid
  revision. Cancel leaves the current case and result intact.
- [x] Compare the changed inputs as well as costs, checks and material identities.
  Regression checks cover ordinary, graded, field-map and non-planar cases.

### PC-03 — A useful first study

- [x] Start with a small measured-conductor study, clearly labeling its illustrative
  prices and model limits; retain the synthetic allocation example as an explicit
  selectable example.
- [x] Offer a clear sequence: select a supported study, review inputs and applicability,
  run, inspect the decision, revise, and export. Show winding geometry and units.
- [x] Remove stale browser/import instructions and avoid presets that imply external
  fields or geometry were modeled when only context was recorded.

### PC-04 — Applicability and work estimate before a run

- [x] Share a headless preflight between CLI and workbench: geometry support, dataset
  identity/availability, temperature and criterion compatibility, evidence class,
  tape-width transfer assumptions, price basis, declared and missing screens.
- [x] Show candidate and sampling workload plus execution settings before dispatch;
  an estimate does not establish field/material coverage or engineering acceptance.
- [x] Give a concrete correction for missing or incompatible inputs and preserve
  unsupported queries as unresolved rather than silently widening their domain.

### PC-05 — Intake of existing conductor and cost evidence

- [x] Load a validated metadata/CSV pair directly in the workbench and builder,
  alongside bundles; bind the computed identity without pasted hashes.
- [x] Carry external datasets with the study's exported review package so it can
  be rerun away from the author's original directory.
- [x] Collect and display a documented price basis, keeping placeholders,
  published estimates and actual quotes distinguishable. Do not invent supplier
  data or customer validation.

### PC-06 — An actionable decision and review package

- [x] Share a decision summary between workbench and report: the selected option,
  comparison basis, binding constraint, cost components, evidence limitations,
  and specific next actions for unresolved or unperformed work.
- [x] Preserve recommendation authority when repricing or comparing records;
  agreement with shared physics is not an independent physical validation.
- [x] Export the case, record, BOM/RFQ, readable report, dataset dependencies and
  an artifact manifest with verification instructions. Confirm cross-file totals.

### PC-07 — Productive calculation and honest performance evidence

- [x] Preserve the previous completed result while a replacement runs, communicate
  its relationship to the active input, and make cancellation responsive through
  search and acceptance without publishing partial work as a completed result.
- [x] Provide predictable work estimates and meaningful progress for search and
  acceptance; retain required numerical/sampling gates.
- [x] Profile an unchanged representative case, implement a justified reduction
  in redundant work if found, and record before/after release measurements and
  equivalent verdicts/costs. Report the result without an unsupported fastest claim.

### PC-08 — Verify the complete supported workflow

- [x] Automate the supported workflow through load/preflight, revision, search,
  comparison, export and offline verification using attributed conductor data and
  explicitly illustrative prices; include failure and cancellation paths.
- [x] Document supported inputs and completed capability accurately; run formatting,
  workspace Clippy/tests, wasm compilation and applicable release checks.
- [x] Every pushed commit must pass GitHub CI; retain its commit and run links.
- [ ] External validation gate: a real engineer completes a real supported decision
  using their case, appropriate conductor evidence and documented prices; record
  observed effort, unresolved issues and comparison with their existing process.
  This gate needs an identified participant and authorized inputs and stays open
  until that evidence exists.

The standards are task-specific: **best in class** means a supported comparison
an external engineer can complete and review; **fastest** means total time from
existing inputs to a defensible decision at matched fidelity; **most accurate**
means correct constraint and economic decisions within a validated input domain.

## Domain coverage (the real frontier)

- **Wider measured domains.** Embedded datasets currently cover
  20–40 K and up to 8 T. Extending certification reach means binding
  datasets measured at higher fields (~10–20 T) and at 65–77 K — the
  industrial HTS regime. The plumbing is done: any conforming dataset
  bundle can be signed, verified and shipped. The blocker is sourcing
  measured data, not code.
- **More geometry families.** TF/D-shaped coils, solenoids and
  non-planar windings. The `CoilPath`/`CoilPath3D` machinery exists —
  OC-031 exercises a helical layer end-to-end under a declared
  Cartesian field map; certifying a new family means a kernel (or a
  declared map), a sampling plan and frozen cross-checks, not a
  rewrite.
- **Critical-state applicability.** The declared critical-state strip bound is
  implemented; extending its qualified regimes requires appropriate physical
  reference evidence. It does not replace conductor or magnet qualification.
- **Deeper mechanical/thermal screens.** Today's Lorentz/hoop checks
  are declared first-order bounds, not structural analysis. Quench
  protection and thermal margins are unmodelled by design until they
  can be modelled honestly.

## Optimization

- **Continuous/structured optimization.** Profiling (2025-12):
  the largest shipped benchmark enumerates 36 candidates; wall time is
  dominated by per-candidate field quadrature, not candidate count, so
  a structured optimizer would not pay for itself on the current case
  shape — and the one continuous axis (bend/straight geometry) is
  enumerable at finer grids cheaper than a local search converges.
  Deferred until a case ships with a candidate space (~10³+) or a
  continuous axis where grid resolution demonstrably binds the answer.
  What would genuinely help sooner: cheaper field evaluation — pruning
  and adaptive quadrature are already partially implemented.

## Platform

- **Python package on PyPI.** `crates/optcoil-py` already exposes the
  engine (`import converra`); publishing wheels is the remaining step.
- **Vendor dataset pipeline.** The bundle + attestation format is done;
  a documented path for vendors/labs to contribute signed datasets is
  the multiplier for domain coverage.

## What is explicitly not planned

- FEM field solving (Converra imports field maps; it does not compute
  them from mesh). External solvers stay external.
- Silent extrapolation beyond measured domains — that's a property,
  not a gap.
