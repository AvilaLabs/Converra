# Changelog

## Unreleased

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
