# Roadmap

Where Converra is going, in priority order. This is a statement of intent,
not a commitment. Contributions that move any of these items forward are
welcome, and the architecture docs explain where each one lands.

## Direction from October 2026: the conductor record

Converra's design search, screening and review workflow are complete for
their supported scope (see [completed milestones](#completed-milestones--september-2026)).
The next direction extends Converra from comparing coil designs to keeping a
traceable record of the conductor itself:

- estimating what each reel of tape can carry from sparse measurements;
- allocating real reel inventory to positions in a fixed design;
- reporting the margin of the as-built coil from the tape actually placed in it;
- deriving procurement specifications at the coil's operating conditions;
- exchanging reel evidence in an open, signed format.

Each milestone below has its own frozen protocol and checker-derived verdict
before it is reported complete. The existing verdict vocabulary, evidence
classes, provenance rules and independent acceptance path apply throughout.
Further workbench polish is deferred unless a conductor-record milestone
needs it.

### CR-01 — Estimate tape performance from sparse measurements

- [ ] Fit a per-tape model of critical current versus temperature, field and
  angle from a restricted subset of measurements, such as those a
  reel-to-reel scanner or a short in-field sample provides. Report stated
  uncertainty.
- [ ] Retrospective hold-out benchmark on the public Robinson Research
  Institute critical-current database: predict withheld low-temperature,
  in-field data for each REBCO product from the restricted subset only.
  The protocol, model, tolerances and falsifiers are frozen and hashed before
  the first scored run. A FAIL is recorded and replanned, not tuned away.
  Protocol v1 (scanner-range fit with the three-parameter scaling law, with
  and without one 30 K anchor) was scored on 2026-10-04: **FAIL** in every
  scenario. Near-Tc data extrapolated to 20–40 K overpredicts unsafely. See
  [docs/CR01.md](docs/CR01.md). A revised method needs a new protocol
  version.
  Protocol v2 (development-grade, same targets) also FAILs at the headline,
  but a four-point 20/40 K short sample to 5 T passes 12 of 18 products. Its
  failures are all overprediction at 7–8 T, so short-sample evidence has to
  reach the operating field.
- [ ] Document which measurements are sufficient, and which are not, for
  each product and regime. The answer defines what a reel passport must
  contain.

### CR-02 — Reel passport schema and per-reel inventory

- [x] Versioned reel-evidence schema covering identity, length, length-resolved
  profiles, in-field points, ab-plane tilt, defects, vendor and batch,
  provenance and evidence class. It binds product maps by dataset identity;
  signing passports is left to CR-07. See [docs/CR02.md](docs/CR02.md).
- [ ] Inventory import and validation in the engine, CLI, Python and MCP.
  Engine and CLI done, including per-reel rating at an operating point;
  Python and MCP remain.
  Measured, model-informed and synthetic reels remain distinguishable.
- [ ] Synthetic inventories generated from published variation statistics,
  labelled synthetic in every output.

### CR-03 — Allocation and orientation

- [ ] Assign reels to positions and orientations in a fixed design so that
  every sampled position meets its declared margin. Derive the splice
  schedule.
- [ ] Route reels that miss a uniform specification to positions they can
  serve. Report conductor and scrap avoided against a declared uniform
  worst-case baseline, under documented prices.
- [ ] Re-allocate the remaining inventory after a defect is recorded during
  winding, preserving the history of earlier allocations.
- [ ] Independent acceptance recomputation of every allocation's screening and
  accounting.

### CR-04 — Recorded reel inventories

- [ ] Run a retrospective allocation on an authorized, recorded reel
  inventory for a coil that has already been built, and compare it with the
  allocation actually used. This requires real reel records and stays open
  until they are available.

### CR-05 — As-built margin and coil-test comparison

- [ ] Produce an as-built margin map from the allocated inventory.
- [ ] Import coil-test observations such as voltage taps and critical-current
  measurements. Compare them with predictions under a frozen protocol,
  starting from published coil-test data where it is available.

### CR-06 — Procurement specification

- [ ] Derive region-level conductor specifications at operating temperature,
  field and angle from a design and its allocation evidence.
- [ ] Compare multi-vendor order splits under documented quotes and lead
  times.

### CR-07 — Open reel-evidence format

- [ ] Publish the reel passport schema with issuer, laboratory and consumer
  workflows, examples and a conformance checker.

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
- **Vendor dataset pipeline.** The bundle + attestation format is done.
  A documented path for vendors and laboratories to contribute signed
  datasets and reel evidence is part of CR-02 and CR-07.

## What is explicitly not planned

- FEM field solving (Converra imports field maps; it does not compute
  them from mesh). External solvers stay external.
- Silent extrapolation beyond measured domains — that's a property,
  not a gap.


## Completed milestones — September 2026

These milestones are delivered and retained as a record of what each one
established. One item remains open: the PC-08 external engineer validation
gate, which needs an identified participant and authorized inputs.

### Product completion extension — September 2026

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

#### PC-01 — Consistent costs and recommendation authority

- [x] Display the record's actual per-spec cost ledger at its declared prices,
  including grading and purchased pieces, in every headline and cost chart.
- [x] Restrict scalar repricing to compatible records and distinguish a price
  scenario's screening choice from the originally recomputed recommendation.
- [x] Reproduce the graded-cost discrepancy as a regression and verify agreement
  between workbench metrics, CLI output, BOM and HTML report.

#### PC-02 — Open, revise, duplicate and compare

- [x] Edit a loaded case in the workbench, preserving every supported declaration
  and its schema when unchanged; provide an in-app advanced editor when a field
  cannot be represented faithfully by the guided form.
- [x] Save a revision or duplicate without overwriting the source or losing its
  existing result; invalidate stale derived state only after accepting a valid
  revision. Cancel leaves the current case and result intact.
- [x] Compare the changed inputs as well as costs, checks and material identities.
  Regression checks cover ordinary, graded, field-map and non-planar cases.

#### PC-03 — A useful first study

- [x] Start with a small measured-conductor study, clearly labeling its illustrative
  prices and model limits; retain the synthetic allocation example as an explicit
  selectable example.
- [x] Offer a clear sequence: select a supported study, review inputs and applicability,
  run, inspect the decision, revise, and export. Show winding geometry and units.
- [x] Remove stale browser/import instructions and avoid presets that imply external
  fields or geometry were modeled when only context was recorded.

#### PC-04 — Applicability and work estimate before a run

- [x] Share a headless preflight between CLI and workbench: geometry support, dataset
  identity/availability, temperature and criterion compatibility, evidence class,
  tape-width transfer assumptions, price basis, declared and missing screens.
- [x] Show candidate and sampling workload plus execution settings before dispatch;
  an estimate does not establish field/material coverage or engineering acceptance.
- [x] Give a concrete correction for missing or incompatible inputs and preserve
  unsupported queries as unresolved rather than silently widening their domain.

#### PC-05 — Intake of existing conductor and cost evidence

- [x] Load a validated metadata/CSV pair directly in the workbench and builder,
  alongside bundles; bind the computed identity without pasted hashes.
- [x] Carry external datasets with the study's exported review package so it can
  be rerun away from the author's original directory.
- [x] Collect and display a documented price basis, keeping placeholders,
  published estimates and actual quotes distinguishable. Do not invent supplier
  data or customer validation.

#### PC-06 — An actionable decision and review package

- [x] Share a decision summary between workbench and report: the selected option,
  comparison basis, binding constraint, cost components, evidence limitations,
  and specific next actions for unresolved or unperformed work.
- [x] Preserve recommendation authority when repricing or comparing records;
  agreement with shared physics is not an independent physical validation.
- [x] Export the case, record, BOM/RFQ, readable report, dataset dependencies and
  an artifact manifest with verification instructions. Confirm cross-file totals.

#### PC-07 — Productive calculation and honest performance evidence

- [x] Preserve the previous completed result while a replacement runs, communicate
  its relationship to the active input, and make cancellation responsive through
  search and acceptance without publishing partial work as a completed result.
- [x] Provide predictable work estimates and meaningful progress for search and
  acceptance; retain required numerical/sampling gates.
- [x] Profile an unchanged representative case, implement a justified reduction
  in redundant work if found, and record before/after release measurements and
  equivalent verdicts/costs. Report the result without an unsupported fastest claim.

#### PC-08 — Verify the complete supported workflow

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

### Engineering decision workspace — September 2026

**Implemented software scope:** deliver a polished GUI and local MCP interface that
let engineers and AI agents create, revise, evaluate, explain, compare, save,
and export supported HTS winding studies through one shared, reproducible
workflow. The first audience is magnet engineers and HTS researchers evaluating
existing winding designs using declared conductor evidence, field models and
prices. External engineer validation remains the separate open PC-08 gate above;
it is not an acceptance condition for this software extension.

#### EW-01 — Structured GUI revision

- [x] Revise and duplicate supported loaded studies using controls for routine
  requirements, operating points, winding/pack choices, material bindings,
  prices, declared limits and execution settings without JSON editing.
- [x] Preserve the original schema, grading, field maps, non-planar paths and
  every declaration outside the edited controls. Clearly identify unavailable
  controls; validate edits before accepting them and preserve prior work on
  cancellation or errors. Retain an advanced editor for exceptional declarations.

#### EW-02 — Persistent alternatives and shared study operations

- [x] Define a versioned, dependency-carrying study workspace with named
  alternatives, exact source case/dataset bytes, completed results and input
  comparisons. GUI and MCP use the same headless operations.
- [x] Save, reopen, revise, duplicate and compare multiple alternatives without
  losing data dependencies or attributing an old result to revised inputs.
- [x] Verify imported results and exported review artifacts; reject corrupted,
  incompatible and missing dependencies with actionable errors.

#### EW-03 — Explain constraints and test the next change

- [x] Share structured diagnoses: the specific failed or unresolved check,
  relevant input, limiting location, observed quantity, declared bound and
  missing evidence where available. Distinguish numerical, physical-screening,
  coverage and unperformed-work issues.
- [x] Offer explicit follow-up experiments with visible changed inputs and
  workload estimates. Preserve requirements, screening limits and numerical
  gates unless the engineer explicitly edits them; recommendations do not
  claim improvement before a new calculation establishes it.
- [x] Run, retain and compare follow-up results against their source study,
  preserving PASS, FAIL, INCONCLUSIVE and NOT_EVALUATED and the selection's
  original authority.

#### EW-04 — Responsive work and measured reuse

- [x] Reuse only completed results for exactly matching source inputs,
  datasets, execution options and engine identity. Imported artifacts are
  viewable evidence, not automatically trusted executable cache entries.
- [x] Keep the GUI responsive during calculations and communicate progress,
  cancellation and the relationship between previous results and active inputs.
- [x] Measure cold calculation and repeated-work latency on fixed attributed
  software fixtures; confirm equivalent costs/verdicts and invalidation after
  material, physics, price or execution changes. Record measured scope honestly.

#### EW-05 — Local MCP for AI agents

- [x] Provide a local stdio MCP server using the official Rust SDK with typed,
  discoverable study, dataset, preflight, search/sensitivity, progress/cancel,
  explanation, comparison, verification and review-export operations.
- [x] Return compact structured summaries and expose detailed records/artifacts
  separately. Enforce bounded inputs, computation and jobs; preserve source
  identities and verdict meanings and protect workspace file boundaries.
- [x] Exercise the real protocol with a client, including discovery, errors,
  completed and cancelled jobs, multiple material dependencies, export and
  numerical parity with equivalent headless/CLI studies.

#### EW-06 — GUI quality and complete workflow verification

- [x] Use clear units, contextual errors, consistent existing visual components,
  keyboard navigation and layouts that remain usable at smaller window sizes.
- [x] Inspect the actual interface and retain screenshots/workflow evidence for
  completed, failed and inconclusive studies, invalid input, cancellation, named
  comparisons, follow-up calculations and saved-workspace reopening.
- [x] Run the complete formatting, workspace Clippy/tests and browser compilation
  gates. Every pushed SHA must pass GitHub CI; retain delivery links and accurately
  document completed capabilities, remaining limitations and performance evidence.

Implementation and verification scope are recorded in the
[workspace guide](docs/ENGINEERING_WORKSPACE.md) and
[verification evidence](docs/ENGINEERING_WORKSPACE_EVIDENCE.md).
The separate PC-08 external validation gate remains open.

### Decision clarity and scenario evidence — September 2026

**Implemented software scope:** shorten the path from existing HTS winding inputs to a
reviewable design and cost decision. Keep the current egui application, exact
source evidence, and declared numerical fidelity. External engineer validation
remains the open PC-08 gate.

Implementation and observed verification are recorded in
[decision clarity evidence](docs/DECISION_CLARITY_EVIDENCE.md),
[scenario studies](docs/ROBUSTNESS.md) and
[workflow measurements](docs/WORKFLOW_COMPARISON.md).
Every delivered SHA must pass GitHub CI before completion is reported.

#### DC-01 — Prioritized decisions

- [x] Rank and deduplicate actionable diagnoses while retaining every raw check.
- [x] Distinguish the selected recommendation, rejected alternatives, required
  input/numerical gates and optional unperformed engineering checks.
- [x] Show concise next actions in the existing GUI and shared MCP diagnosis.

#### DC-02 — Explicit uncertainty scenarios

- [x] Rerun comparable named alternatives across declared supplier-price,
  conductor-Ic and operating-temperature scenarios with a mandatory nominal row.
- [x] Preserve graded bindings, purchased-piece costs, unchanged constraints and
  source provenance; label scaled material data synthetic.
- [x] Report winner changes, nominal geometry survival, unresolved outcomes and
  cost ranges without assigning probabilities or claiming qualification.
- [x] Preview bounded aggregate work, calculate away from the GUI event thread,
  cancel without attaching partial evidence, and expose local MCP jobs.
- [x] Retain exact replay inputs and records in a portable workspace; distinguish
  current results from history after revisions and verify imported evidence.

#### DC-03 — Representative workflow comparison

- [x] Freeze three attributed representative input sets and compare explicit
  CLI/files with the shared workspace at identical numerical settings.
- [x] Record fresh solves, exact repeat reuse, revision, save/reopen and review
  export separately; assert semantic parity and preserve failed outcomes.
- [x] Publish measured software timings and operation units accurately. Human
  effort and commercial-tool superiority need separate observed comparisons.
- [x] Verify the complete extension through local formatting, workspace
  Clippy/tests, wasm and release builds, and observed browser workflows.

### Self-directed engineering workflow — September 30, 2026

Implemented software milestone: an engineer can bring existing material data
into the existing egui application, understand a supported or unresolved
decision, and recover or share the complete study without author assistance.

Implementation and exercised scope are recorded in the
[self-directed workflow evidence](docs/SELF_DIRECTED_WORKFLOW_EVIDENCE.md).

#### SD-01 — Existing measurement files

- [x] Preview CSV/TSV and Excel workbooks with explicit sheet, column and unit
  mapping, required source attribution, and physical measurement declarations.
- [x] Validate canonical derived data before binding it to a case; retain the
  source identity and transformation recipe in portable evidence.
- [x] Carry imported material bundles through case authoring, running, saving,
  and reopening without a second manual dataset-loading step.

#### SD-02 — Understand the outcome

- [x] Present the decision, why it holds or remains unresolved, and the first
  prioritized next action together; retain the complete diagnostic ledger.
- [x] Label recomputation agreement precisely and distinguish saved evidence
  from currently runnable input dependencies.
- [x] Provide a guided first journey with useful supported and unresolved
  examples while preserving every engineering gate.

#### SD-03 — Protect work

- [x] Autosave bounded project/study state and unfinished editable drafts on
  native and web, using atomic writes or browser transactions.
- [x] Offer restore/discard after restart, preserve exact source identities and
  completed evidence, and expose saved/unsaved/recovery-error state.
- [x] Preserve the last valid recovery snapshot when a write or validation
  fails, and avoid attaching partial calculation results.

#### SD-04 — Verify the user journey

- [x] Exercise import, run, cancellation, revision, comparison, save, reopen and
  recovery through meaningful desktop action tests and real browser checks.
- [x] Check navigation responsiveness during long worker jobs and retain
  completed evidence after cancellation.
- [x] Run native formatting/Clippy/workspace tests, wasm/release builds and
  browser regression checks; verify GitHub CI for every delivered commit.
