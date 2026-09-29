# Changelog

## Unreleased — complete supported study workflow

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
