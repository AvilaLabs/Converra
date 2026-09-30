# Contributing / development conventions

Converra is an MIT-licensed Avila Labs project. Contributions are welcome; the conventions below exist because this codebase's value is its auditability — a contribution that weakens the evidence chain is worse than no contribution. Dependencies retain their own licenses.

Use the pinned Rust toolchain and workspace dependencies. Prefer small, typed operations with explicit errors. Unsafe code is forbidden by the workspace lint. Do not add network services or a plugin framework just to connect the first solver.

Case schema v1 uses SI units named in each dimensional field and USD for costs. It represents ordered, separately wound modules in a single series circuit. A grade/count change is permitted only at an explicitly allowed module interface. This is deliberately narrower than a general coil CAD model.

Magnetostatics has a separate `optcoil-magnetostatics/v1` schema and [OC-002 contract](docs/OC002.md). Keep its frozen geometry, probes and accuracy gates intact. Regenerate reference data using the independent Python tool when developing a separately identified case; do not populate expected values from the production Rust evaluator. Field reports fingerprint relevant implementation sources as well as case/reference inputs, and reject stale reference case/source hashes.

Measured material data has its own schema and [OC-003 contract](docs/OC003.md). Preserve third-party data attribution and source hashes. Use measured coordinates; commanded coordinates establish connectivity only. Keep failed baselines, development folds and the reserved challenge identifiable. Do not tune against the reserved challenge and continue presenting it as unseen validation. Material query/domain success cannot establish an intrinsic local current law or an engineering-current allowance.

Coupled conductor-geometry screening has its own schema and [OC-004 contract](docs/OC004.md). Keep the frozen case, its declared numerics gates and its `reference_subset` intact. Regenerate `benchmarks/coupled/oc-004.reference.json` with `tools/reference_oc004.py` (never from the Rust evaluator) and `benchmarks/coupled/oc-004.assumption-audit.json` with `tools/audit_oc004_material_assumptions.py` whenever the material dataset, the frozen case, or either tool's own logic changes; both refuse to overwrite an existing output. A PASS screening status is a current allowance under declared assumptions only, never a production operating-current limit; `conductor_qualification_status` and `engineering_status` stay non-PASS until the width-transfer, strain, current-sharing, mechanical, thermal, quench and manufacturing checks are actually performed.

Coupled cost search over pack geometry has its own schema and [OC-007 contract](docs/OC007.md); keep it calling the OC-004 coupled runner's own screening functions rather than reimplementing them, with its acceptance recomputation staying a separate module from the search's cost ledger, exactly as `optcoil-search::acceptance` stays separate from `lib.rs::objective` for OC-001. Both frozen search cases (`benchmarks/coupled/oc-007.json` and `oc-007-control.json`) are run and their results recorded in [OC-007](docs/OC007.md); do not edit either to recover a result, and treat the acceptance module's finer refined-plan re-check (not the coarse search plan) as the actual authority behind `search_status`, since it is what caught a genuine coverage gap the coarse plan missed on the control case.

Bracketing refinement of that cost optimum has its own schema v2 (extending v1; a v1 case still parses) and [OC-008 contract](docs/OC008.md): keep `optcoil-search::coupled_refine` calling `coupled_search::evaluate_one_candidate` and OC-007's own `search_acceptance` module rather than reimplementing either, report an unmet bracket as `INVALID` (evaluating nothing further for that pancake count) and a non-monotone evaluated sequence as `INCONCLUSIVE` rather than silently patching either, and do not edit the frozen `benchmarks/coupled/oc-008.json` to recover a result — its run is recorded in [OC-008](docs/OC008.md).

Validate inputs before numerical work. Reject malformed identities, unsupported schema versions and nonfinite inputs. A valid input outside a material envelope is inconclusive, not physically infeasible. Keep required engineering checks visible when they have not been performed.

New physics needs reference cases appropriate to the claimed operating regime. Use independent expected results, meaningful conservation/symmetry checks and numerical tolerances. Do not validate a formula solely by repeating it in its test. Benchmark a competitive baseline under unchanged requirements, and include all modeled manufacturing costs.

Change the relevant implementation identity when numerical or interpretation behavior changes. Run identity covers the exact supplied case bytes, search options, package version, model/checker identities and the recorded implementation fingerprint. Preserve the source revision, lockfile and runtime context alongside serious benchmark results. These identities do not prove correctness. `StudyEngineSession` reuses only results calculated and verified by that running process, keyed by exact case and bundle bytes, execution options and engine identities. Imported records and reopened workspaces never populate its cache.

Before finishing changes:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

During core work the app can be excluded from tests, but compile it before finalizing changes to shared APIs. Generated run files are excluded from source control; checked-in expected results must include provenance and a documented derivation.
