# Changelog

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
