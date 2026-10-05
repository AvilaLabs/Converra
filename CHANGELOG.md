# Changelog

## Unreleased

- Add truth evaluation of an allocation against a synthetic truth file
  (`optcoil-allocation-truth-evaluation/v1`) and measurement planning
  (`optcoil-measurement-plan/v1`), with the CLI commands
  `allocation evaluate-truth` and `allocation plan-measurements`. The plan
  re-runs the allocator with one reel's derate changed, ranks the reels by
  what a measurement would save, and builds a cumulative plan; every
  allocation in it passes the independent check. With a truth file each step
  also reports whether the measurement assumption held. Synthetic evidence
  only.
- Fix the allocation demand table at offset bounds above zero: angle samples
  are now folded into [0, 180) like the engine's, so positions near the tape
  normal are no longer reported as outside the map domain.
- Add reel passports (`optcoil-reel-passport/v1`) and per-reel inventories
  (`optcoil-reel-inventory/v1`) with strict validation. Evidence classes are
  `measured`, `model_informed` and `synthetic`. A passport may not claim a
  stronger class than its weakest section, and an inventory may not claim a
  stronger class than its weakest passport. Totals count overlapping excluded
  or cut spans once. New `optcoil reel validate` and `optcoil inventory
  validate` commands.
- Add `optcoil inventory rate`, which rates each reel at an operating point
  (`optcoil-reel-rating/v1`) from its passport and a product map bound by
  dataset id and CSV SHA-256. A reel's length profile only scales the map.
  Every reel gets a status; each status other than `rated` carries an
  explanation and the evidence that would allow a rating. Points outside the
  map's measured cells are never extrapolated, near-tape-plane angles need
  measured ab-plane offsets, and scaled ratings are labelled `model_informed`
  because the transfer from the profile condition to the operating point is
  untested. Records with scaled ratings quote the published within-product
  transfer scatter of about 15%. A length profile can declare the product-map
  row that represents its condition (`map_reference`), which is checked
  against the row's nominal coordinates within tight tolerances. Synthetic,
  illustrative examples are in `examples/reels/`.
- Expose the same three reel operations in Python (`validate_reel_passport`,
  `validate_reel_inventory`, `rate_reel_inventory`) and as local MCP tools of
  the same names. Both take the passport, inventory and dataset bundles as
  exact JSON text and share the CLI's code and dataset-identity checks.
- Add a deterministic synthetic reel inventory generator
  (`optcoil-synthetic-inventory-spec/v1`): `optcoil inventory synthesize`,
  `synthesize_reel_inventory` in Python and an MCP tool of the same name. It
  draws lot, along-length, hidden transfer and ab-plane offset variation from
  published statistics (each recorded with its source and a published, derived
  or user-supplied label) around a product-map row, labels every output
  `synthetic`, and writes the drawn factors to a separate truth file that
  binds the inventory's SHA-256. Example spec in `examples/reels/`.
- Add `optcoil allocation demand` (CR-03 slice A), which builds the allocation
  demand table `optcoil-allocation-demand/v1` from a coupled-search record's
  selected optimum. For every turn and module it gives the scale factor
  `s_req` a reel needs relative to the product map, at each tabulated ab-plane
  offset bound (default 0 to 6 degrees in 0.5 degree steps), with the turn
  lengths and start coordinates along a conductor stream. The per-point
  capacity is recomputed with the engine's own query, and the offset-0
  recomputation must equal the engine's `allowed_screening_a` within 1e-12
  relative or the build fails. Positions outside the map's measured cells
  carry no value and an explanation. Radial tape normal with the legacy
  racetrack or planar path only; a product map that differs from the one the
  record screened with is refused unless `--allow-map-substitution` is given.
  Every v1 scope limit is listed in the record.
- Add `optcoil allocation run` and `optcoil allocation check` (CR-03 slice B,
  `optcoil-allocation/v1`). Given a demand table, an inventory and declared
  parameters (`optcoil-allocation-params/v1`: margin, a required transfer
  derate, minimum piece length, price with its source), the run places reel
  stretches along each (module, strand) conductor stream where the reel's
  derated scale factor meets the demand with the margin, checked exactly on the
  merged breakpoints of the reel and turn step functions. It reports the splice
  schedule, reel usage, metres, and the shortfall and money difference against a
  uniform worst-case baseline in which a reel is accepted only if its weakest
  point meets the coil's highest requirement. The allocator is a deterministic
  greedy with no optimality claim. Reels with no ab offsets, or an offset bound
  beyond the tabulated range, are kept off offset-sensitive positions. A zero
  derate is accepted but labelled unsafe in the record. The check is an
  independent module that does not call the allocator: it recomputes every
  piece's feasibility, coverage with no overlaps, reel-length conservation,
  splice counts and all totals, and prints PASS or FAIL with each mismatch.
- Fix the Python package metadata: `pyproject.toml` now takes its version from
  Cargo (`dynamic = ["version"]`), so `maturin develop` and `maturin build` run.

## v0.3.0 — 2026-10-02

- Add an optional Avila Labs sign-in. The browser workbench has a Sign in
  button and a first-visit prompt; the desktop app signs in with a device code
  approved in the browser. A tool launcher links the other Avila Labs tools.
  Nothing from your work is sent, and every feature works without an account.
  Includes a new application icon.
- Import bounded CSV, TSV and XLSX measurement tables with a source preview,
  explicit column/unit/nominal-coordinate mapping and source declarations.
  Portable dataset bundles retain the upload hash, import recipe and subsequent
  Ic scaling receipts; imported data binds directly into new or revised cases.
- Present the decision, reason and first next action together. Label independent
  recomputation agreement separately from the engineering outcome, and include
  an optional measured two-candidate comparison example.
- Recover device-local studies, exact inputs, completed evidence, unfinished
  case edits and import forms after restart. Atomic desktop writes and browser
  transactions protect previous drafts and detect competing windows/tabs.
  Failed saves remain visible; desktop close waits for a saved draft or an
  explicit user choice.
- Add real browser workflow checks and native action/recovery regressions for
  importing, running, cancellation, revision, comparison, export and reopening.

- Fix browser margin sweeps freezing the page by running them in the existing
  calculation worker. Add cancellation on web and desktop, retaining the prior
  completed sweep when cancelled.
- Rank study diagnoses into actionable groups, retaining the raw check ledger
  and separating required gates, selected candidates, rejected alternatives and
  optional unperformed checks.
- Add bounded scenario reruns for named alternatives with dataset-specific
  conductor prices and Ic multipliers plus operating-temperature offsets. Retain
  full replay evidence, label transformed material data synthetic, and show
  recommendation changes and unresolved scenarios in the existing egui GUI.
- Preserve scenario evidence in workspace v2 with v1 import migration and
  explicit current/history binding. Expose preview, background jobs and evidence
  resources through the local MCP server and a `study-robustness` CLI command.
- Add a frozen-input workflow comparison harness for the explicit CLI/files and
  shared workspace paths. Software timings and operation counts do not measure
  human usability or superiority over commercial tools.

## v0.2.0 — engineering decision workspace

### Study workspaces and AI agents

- Shared versioned study workspaces carry named variants, exact input and dataset
  bytes, historical result dependencies, structured diagnoses and decision diffs.
- Loaded-case GUI revisions use unit-labelled controls while preserving other
  declarations; explicit follow-up experiments keep requirements and limits fixed.
- Local `optcoil-mcp` stdio server uses the official Rust SDK, with typed tools,
  bounded background jobs, cancellation, resources and portable review exports.
- Exact-input reuse is limited to completed, verified calculations in the running
  engine session. Reopened and imported evidence remains outside that cache.
- Failed candidates with undefined pre-screen current or field quantities now
  round-trip as JSON `null`. The verifier rejects undefined quantities on evaluated
  candidates. Run schema advances to v26 and search checker identity to v23;
  numerical kernels and engineering gates are unchanged.

### Complete supported study workflow

- Measured-conductor startup study with explicit illustrative prices, shared
  CLI/workbench applicability checks, workload estimates and unresolved actions.
- Loaded-case revision and duplication preserve complete declarations; comparison
  includes input changes and retains previous results during replacement runs.
- Graded and purchased-piece costs use their actual ledgers. Scalar repricing is
  restricted to compatible cases; scenario selection acceptance is
  `NOT_EVALUATED` in the version 2 reprice artifact.
- Direct bounded metadata/CSV intake, documented price basis, and portable review
  packages with original inputs, dataset dependencies and offline verification.
- Multiple external material bindings pass through preflight, search, margin
  sweeps, the browser worker and review export. CLI search, preflight and
  sensitivity accept repeated bundle flags; packaged rerun commands include
  every dependency. Supplied bundle files retain their original bytes so
  registry checks remain bound to the same signed artifact.
- Cooperative cancellation through acceptance and reuse of an independently
  recomputed baseline when it is also the selected candidate. Acceptance checker
  identity advances to v19; field kernels and numerical gates are unchanged.
- Complete headless workflow regression and recorded release performance evidence.
  External engineer validation remains an open product milestone.

### Distribution

- Desktop, CLI and MCP archives for Windows x64, Linux x64, and both macOS
  architectures include setup guides, examples, licenses and dataset attribution.
- Releases require successful CI for the exact tagged commit, provide SHA-256
  checksums and remain drafts until the built archives are inspected.
- The hosted browser build carries a release manifest with its source commit and
  asset hashes. Browser staging stops on build errors.

## v0.1.0 — first public release

Public release of Converra: an MIT-licensed screening and
cost-optimization workbench for planar HTS racetrack coils.

- Coupled cost search over discrete candidate spaces: exhaustive grid,
  conservative coarse-plan pruning, bracketing refinement, and an
  independent acceptance recomputation — `PASS`, `FAIL`,
  `INCONCLUSIVE`, `NOT_EVALUATED` are never smoothed over.
- Finite-cross-section racetrack/D-shape field kernel
  (`planar-path-uniform-volume-duffy-gauss-graded-metric/v4`),
  cross-checked against an independent Python reference, Bluemira, and
  the CERN Feather-M2 wound coil (~3% tape-length agreement).
- Ten embedded material datasets: five measured (Robinson lab, CC BY
  4.0), one measured-with-model-extension to 20 T, and two published
  MEM-fit parametrizations (Babouche et al. 2026) including `-v2`
  variants reaching the published quad's 19 T corner.
- Signed dataset bundles with ed25519 attestation and registry
  verification; run records bind all inputs by SHA-256 and are
  independently verifiable offline.
- CLI (`optcoil`), egui desktop workbench, and Python bindings
  (`import converra`, pyo3/maturin — JSON in, JSON out,
  byte-identical records with the CLI).
- Sensitivity sweeps, dataset bakeoffs, HTML evidence reports,
  price recompute that carries verdicts unchanged.

See `docs/SUPPORTED_DOMAINS.md` for the honest capability envelope —
most embedded measured data covers 20–40 K and up to 8 T, and
unsupported regions produce `INCONCLUSIVE`, never extrapolated answers.
