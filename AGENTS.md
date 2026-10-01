# Working on Converra (optcoil)

The maintained user handbook is `docs/guide/`, built with mdBook 0.5.4 using `book.toml`. Update its task pages when behavior changes. Keep dated benchmark records outside the handbook, preserve their verdicts, and distinguish current source from packaged releases. Publishing and checks are in `docs/maintainers/DOCUMENTATION.md`.

Read `README.md`, `CONTRIBUTING.md` and the relevant benchmark before changing calculations.

- Keep authoritative case and manufacturing semantics in `optcoil-model`, numerical models in `optcoil-physics`, and headless operations in `optcoil-search`. CLI and egui are clients.
- Preserve `PASS`, `FAIL`, `INCONCLUSIVE` and `NOT_EVALUATED`. Search completion or a screening pass cannot establish engineering acceptance.
- Keep synthetic and measured data distinguishable. Add explicit provenance and supported operating domains to new material models. Never silently extrapolate.
- Maintain the independent cost recomputation in `optcoil-search::acceptance`; document shared physics assumptions accurately.
- Version changed schemas, numerical models, checker logic and search semantics. Input hashes are identities, not proof of correctness or an implemented cache.
- Keep long calculations and external solvers off the egui event thread.
- Use the ACTINV light theme: Avila blue `#1800AD`, background `#F7F8FC`, and the supplied Avila Labs logo beside the software name. Prefer egui's existing panels, tables, plots and native file dialogs where they help engineering work.
- User-supplied cases and datasets stay out of source control — keep them under ignored `customer-data/`; generated runs go to ignored `runs/`.
- Run formatting, appropriate numerical/regression tests and Clippy for changed targets. Report checks actually run and missing physics honestly.
- Every push must pass CI. Before pushing, run the gates in `.github/workflows/ci.yml`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, the app's wasm check, and `node tools/run_gui_workflow_check.mjs` with Chrome available. Use the complete suites. After pushing, watch `gh run list` until the pushed commit's CI run is green; a red run is a broken push, fix it immediately and re-verify. Never report a push as done while its CI is still queued or failing.

## Product assessment preference

The user understands that independent external validation is required. Treat it
as implicit in product assessments and next-step discussions; do not repeatedly
list it as a generic caveat or outstanding task. Discuss it when a concrete
action, result, or decision requires that information. Preserve existing
engineering gates and evidence labels.
