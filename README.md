# Converra

**Avila Labs' open-source coil cost optimization workbench, built in Rust and egui.** The engine and CLI crates are named `optcoil-*`; the desktop workbench is Converra.

**Expert-level magnet optimization in minutes.** Converra searches for lower-cost, manufacturable HTS magnet designs while holding engineering requirements fixed, and produces an auditable evidence trail (independent acceptance, external kernel cross-checks, verdict semantics that distinguish PASS/FAIL/INCONCLUSIVE/NOT_EVALUATED) alongside the design. Validated to within ~3% of a physically built CERN HTS dipole's conductor mass (OC-010), with magnetostatics independently confirmed against Bluemira (OC-011). The repository ships the full engine, the benchmark suite it was developed against, and the evidence docs recording what each milestone actually proved.

## Start here

Rust 1.95.0 is pinned in `rust-toolchain.toml`. Run these commands from this directory:

```bash
# Desktop workbench; the example is embedded and loads automatically.
cargo run -p optcoil-app

# Headless allocation and finite-cross-section field benchmarks.
cargo run -- demo
cargo run --release -- field-benchmark
cargo run --release -- material-benchmark
cargo run --release -- coupled-benchmark
cargo run --release -- material-validate benchmarks/measured/oc-005.json
cargo run --release -- material-validate benchmarks/measured/oc-006.json
cargo run --release -- coupled-search-benchmark --threads 5
cargo run --release -- coupled-refine-benchmark --threads 5

# A coupled search over an authored case; open records or cases in the workbench too.
cargo run --release -- coupled-search benchmarks/coupled/oc-007.json --threads 4 --json

# Package a customer material dataset, or run a search directly on it. The
# case's material.dataset_id and material.csv_sha256 pin the dataset's
# identity; a mismatch is rejected, never substituted.
cargo run --release -- dataset-bundle --metadata my-material.json --csv my-measurements.csv   --output customer-data/my-material.bundle.json
cargo run --release -- coupled-search case.json --dataset-bundle my-material.bundle.json

# Sensitivity: a declared sweep of ic_scale / temperature_k / price_usd_per_m
# perturbations, each point an ordinary coupled search of the mutated case.
cargo run --release -- sensitivity case.json sweep.json --output runs/sweep.json

# A self-contained HTML evidence digest of a coupled-search run record.
cargo run --release -- report runs/oc-012-record.json --output report.html

# Reprice a record at a different conductor price: exact arithmetic on the
# per-candidate cost ledgers, verdicts carried through unchanged (price
# never enters physics), emitted as a derived note bound to the source
# record's sha256. Provenance for the new price is required.
cargo run --release -- reprice runs/oc-012-record.json \
  --price-usd-per-m 62.5 --price-provenance "published industry band, midpoint" \
  --output runs/repriced.json

# Query the low-field extension explicitly; --dataset never silently switches.
cargo run --release -- material-query --dataset robinson-superpower-ap-v3-lowfield \
  --temperature-k 21 --applied-field-t 0.3 --angle-from-normal-deg 90

# Save a complete, reproducible record; use a new output filename for each run.
cargo run -- demo --output runs/oc-001.json

# Inspect or optimize an editable case.
cargo run -- inspect benchmarks/synthetic/oc-001.json
cargo run -- run benchmarks/synthetic/oc-001.json --json

# Bound work explicitly, or list the planned integrations.
cargo run -- demo --max-evaluations 1000
cargo run -- adapters
```

The desktop uses ACTINV's light Avila Labs theme and logo. Open projects with a native file picker or drag-and-drop. Resizable panels, interactive cost/material plots, selectable module tables and an inspector connect allocations to costs and constraints. Search, file dialogs and file operations run on workers. Menus and shortcuts provide opening (`Ctrl+O`), run export (`Ctrl+Shift+S`) and help (`F1`). It uses the same engine as the CLI.

Native desktop previews: [design comparison](docs/images/design-comparison.png) and [material envelope](docs/images/material-envelope.png). These show the actual synthetic benchmark run.

### Python

The engine is also a Python module — JSON in, JSON out, byte-identical
records to the CLI:

```bash
pip install maturin
maturin develop --manifest-path crates/optcoil-py/Cargo.toml
```

```python
import converra
record_json = converra.run_search(open("case.json").read())
converra.verify_record(record_json)   # independent ledger checks
converra.render_report(record_json)   # standalone HTML evidence digest
converra.list_datasets()              # embedded material datasets
```

See [crates/optcoil-py/README.md](crates/optcoil-py/README.md) for the
full surface. Wheels are not on PyPI yet — that is on the
[roadmap](ROADMAP.md).

**Desktop project input is OptCoil case JSON.** The CLI also imports measured-material CSV files with explicit SI columns and provenance metadata; see [OC-003 commands](docs/OC003.md). STEP CAD import, general Excel import, GUI column mapping and native solver connections are planned. JSON remains the internal case format, not a claimed industry-standard coil interchange format. The workbench's "New search case" builder authors and schema-validates coupled-search cases (`optcoil-coupled-search/v9`) before saving; path-geometry (`fixed_geometry.path`) cases are still authored as JSON externally, though the workbench renders their outlines; other case kinds are still edited externally.

On Linux the desktop needs a graphical session and graphics drivers. Headless users can run every calculation through the CLI. `cargo run --release -p optcoil-app` builds an optimized desktop binary. Cargo's default workspace member is the CLI, so ordinary headless commands do not compile graphics dependencies.

## What works now

- Versioned JSON cases with units in field names, stable module/material identities, provenance, bounded material domains and explicit module interfaces.
- Fixed-geometry allocation of tape grades and integer tape counts across separately wound modules. Exact enumeration is bounded by an evaluation limit and can be cancelled.
- Conductor, scrap, assembly and joint/change costs. The acceptance module independently recomputes the cost ledger from the selected candidate.
- Distinct `PASS`, `FAIL`, `INCONCLUSIVE` and `NOT_EVALUATED` outcomes. Unsupported material points are never extrapolated.
- Full run records containing inputs, options, model/checker/search versions, input hashes, termination reason and assessments. Existing records are never overwritten.
- A circular-loop axial-field analytical reference with numerical tests, separate from the allocation benchmark.
- OC-002: a finite-cross-section racetrack field evaluator in Rust, including pack-interior/boundary points, an independent Python-generated reference, refinement gates and reproducible CLI reports. The [benchmark documentation](docs/OC002.md) records its assumptions, measured errors and runtime. The same kernel (`planar-path-uniform-volume-duffy-gauss-graded-metric/v4`) now generates its source cells from an arbitrary piecewise line+arc planar `CoilPath` — D-shapes and picture-frame coils, not only racetracks — and is checked against an independent chord-filament walk of the same path; coupled-search schema v9 / coupled-conductor schema v5 declare a `path` centerline (OC-019), so general planar shapes run end to end through the search, not only through the physics API.
- OC-003: attributed measured SuperPower conductor data, CSV import, interpolation using actual temperature/field/angle coordinates, domain/criterion checks, whole-plane validation and a separately reserved challenge. [Results and limitations](docs/OC003.md) include the failed linear baseline and passing logarithmic model.
- OC-004: couples the OC-002 field evaluator to the OC-003 bridge law at explicit tape positions and orientations, producing a screening operating-current margin with reference-checked numerics. [Results and limitations](docs/OC004.md) include a determined FAIL candidate (utilization over its declared limit) and the still-`INCONCLUSIVE` conductor-qualification status; it is not a production operating-current limit.
- OC-005: validates the OC-003 interpolator against simultaneous multi-axis withheld planes (temperature, field and angle at once), the direct analogue of an OC-004 query. [Results and limitations](docs/OC005.md): the three-axis stratum's worst positive error (8.637%) stays under the 0.10 gate OC-004's overprediction budget depends on, so that budget is retained, but the fold's own overall status is `FAIL` on a separate general worst-error gate (25.034%, an underprediction at a known ab-plane peak).
- OC-006: adds a second embedded dataset, `robinson-superpower-ap-v3-lowfield` (`--dataset` on `material-query`/`material-validate` selects it explicitly; the original dataset and every OC-003/OC-004/OC-005 benchmark stay byte-identical), extending the characterization region down to 0.05–0.7 T. [Results and limitations](docs/OC006.md): the fold covering the new range (`low_field_planes`) passes every gate cleanly, but the benchmark's overall `interpolation_validation_status` is `FAIL` — driven by the same 35 K/7 T ab-plane peak OC-003 and OC-005 already flagged, not by the low-field data — while the low-field fold itself passes every gate. Under the pre-declared rule, which keys on the low-field folds, a future coupled case may pin the extension with that limitation disclosed; OC-004 itself is frozen and keeps its clamp below 1 T.
- OC-007: the first end-to-end coupled cost search — an exhaustive discrete search over racetrack pack geometry (turns x tapes, 15 candidates) under a fixed 0.9 T bore-field requirement, screened through the unmodified OC-004 coupled runner (reused, not reimplemented), with conservative coarse-plan pruning and a separate independent acceptance recomputation, including a finer refined-plan re-check of the selected optimum and the baseline. [Results](docs/OC007.md): the frozen primary case (OC-006's low-field dataset) finds a **120x4 optimum at $41,513.13 versus a $50,541.41 baseline — $9,028.28, 17.863% modeled savings** (synthetic prices, screening model), with `search_status: PASS` and acceptance agreement `PASS`. The pre-declared original-dataset control run reproduces the identical candidate table and optimum at the search level, but its acceptance-level refined-plan check comes back `INCONCLUSIVE` on 2 of 6,400 points — a genuine, diagnosed low-field angular-coverage gap in the original dataset, exactly the gap OC-006 was built to close. Both cases were re-run 2026-09-10 under a metric-consistent kernel fix (kernel v3, OC-008 contract Stage K): every status, the optimum and the saving figure are unchanged, and the v2-kernel records remain in `runs/` as superseded history — see [OC-007](docs/OC007.md)'s own re-validation section.
- OC-008: bracketing refinement of OC-007's discrete-grid optimum — per-pancake-count integer bisection (5 declared pancake counts, 2/3/4/5/6 tapes) finds the smallest passing turn count `n*(p)` at each, then selects the cheapest across `p`, reusing OC-007's own screening pipeline and acceptance module. [Results](docs/OC008.md): the global optimum is **72 turns x 6 tapes at $39,021.82** — $2,491.31 (6.001%) cheaper than OC-007's own 120x4 optimum and $11,519.60 (22.792%) cheaper than the original 200x3 baseline (both **modeled, synthetic prices, screening model**), `search_status: PASS`, acceptance agreement `PASS`. The optimum sits at the *top* of the declared pancake range and within 0.0235 utilization of the 0.8 limit — under this model a higher pancake count was not ruled out, and this design carries no margin beyond the declared interpolation and utilization limits, both stated plainly rather than implied as spare headroom.
- OC-010: measured parity study against CERN's Feather-M2 — a coil that was actually wound. [Results](docs/OC010.md): the verified optimum lands ~3% below the ~190 m published tape estimate, parallel-strand cables buy feasibility but never savings (capacity scales with conductor-metres exactly), and the small-radius corner correctly reports `INCONCLUSIVE` at the tape-edge self-field boundary rather than certifying what the model can't resolve.
- OC-011: independent kernel verification — Bluemira's own Biot–Savart implementation (installed from source, CAD/FEM stack stubbed, never exercised) agrees with our evaluator to **max |dB|/B ~5e-4, direction within 0.012°** across every OC-010 optimum. [Method and results](docs/OC011.md).
- OC-012: the first customer-style deliverable — a startup-scale 5 T / 80 mm demonstration-dipole spec through the full pipeline. [Results](docs/OC012.md): **~20% verified conductor reduction** vs a feasible declared baseline (coarse plan), and a complete per-candidate *blocker map* — every INCONCLUSIVE names its physical boundary (measured-data coverage vs oblique-field angle), which is itself the deliverable.
- OC-013/OC-015: the priced cases — sourced `$62.5/m` band midpoint for 12 mm REBCO. [OC-013](docs/OC013.md) (v4): $700 / 0.68% verified savings, acceptance PASS — a savings-floor result. OC-015 is the corrected-semantics (v5) counterpart.
- OC-014: the self-field work — Phase 1 `uniform_transport` correction (schema v5/v2) implemented and exercised end-to-end; [Phase 2 spec](docs/OC014.md) (critical-state strip) written against the measured residual population.
- OC-016: the coverage-boundary sensitivity study — a labeled model extension (`robinson-superpower-ap-v3-modelext`, schema v2, anchored power-law continuation to 20 T with a conservative margin) reruns the OC-012 grid. [Results](docs/OC016.md): coverage blockers go 54 → 0 and the model-informed optimum at the feasible-baseline cell is **75% lighter than baseline** — the size of the prize behind the data wall, explicitly *not* a measured-data-verified claim.
- OC-017: the `transverse_bound` along-current model (search schema v6, conductor v3, run record v3) — over-limit field tilts in `0.20 < f ≤ 0.50` are queried at full magnitude and transverse-plane angle and labeled `along_current_bounded` instead of excluded; above the ceiling they remain excluded. [Docs](docs/OC017.md). The v6mx OC-012 rerun quantifies the last INCONCLUSIVE class.
- OC-018: the hoop-stress mechanical bound (search schema v7, run record v6) — `σ = (I_op/s)·B_peak·R_outer / A_section` with the load path declared, turning the v4 force-per-length surrogate into the stress quantity conductor limits are published in. [Docs](docs/OC018.md).
- OC-019: planar-general paths end to end (search schema v9, conductor schema v5, search model v9) — `fixed_geometry.path` declares a closed piecewise line+arc centerline (the D-shape fixture), `Station::Path { s_m }` locates sampling stations by arc length, bend screening uses minimum local curvature radius, and the cost ledger meters tape along offset curves. Racetrack cases (v1–v8) are unchanged and stay bit-comparable; the generated conductor case carries the v5 schema so the OC-004 runner and acceptance evaluate the identical path.
- OC-020: graded packs end to end (search schema v10, conductor schema v6, search model v11) — named `tape_specs` each bind a full material dataset binding plus a price, `grading.regions` map fractional turn ranges to per-region spec choices, and the search enumerates the geometry × assignment product. Each turn screens under its own resolved dataset/interpolator and meters tape at its assigned spec's price (scrap included); generated conductor cases carry explicit `winding.regions`, and acceptance independently recomputes the graded ledger against the full multi-dataset map. The OC-020 fixture assigns a cheaper low-field-binned tape option to each half of the pack — the mechanism behind radial grading, on measured data, at placeholder prices.
- See also: [supported domains](docs/SUPPORTED_DOMAINS.md) — the honest spec sheet.

The included `oc-001` case has a $932 baseline and a $824 optimum under its synthetic model: $108, or approximately 11.59%, in modeled savings across 4,096 allocations. **These are invented benchmark inputs, not evidence of real magnet savings.** See the [hand calculation](docs/BENCHMARK.md).

## What comes next

The allocation model still uses prescribed fields and ideal current sharing. OC-004 now couples OC-002's finite-pack field to OC-003's measured conductor response at explicit tape positions, but that coupling is a screening margin, not yet connected to tape allocation or the desktop workflow: width transfer is unmodeled (`width_transfer.basis: none`), OC-005's multi-axis validation retains OC-004's 0.10 overprediction budget but its own fold FAILs a separate general worst-error gate, and `conductor_qualification_status` stays `INCONCLUSIVE`. HTS current sharing, mechanics, thermal behavior, quench and a real winding process remain `NOT_EVALUATED`. The cost checker is separate from search, but both use the same synthetic capacity model.

Allsolve, COMSOL and Ansys have explicit adapter contracts and capability placeholders. **No commercial solver connector is implemented.** FEM, CAD import, continuous optimization and production engineering acceptance remain future work. The first measured material entry is research characterization, not a qualified library of supplier lots.

Permitted manufacturing changes, width transfer and the remaining engineering checks are the next work. See [the roadmap](ROADMAP.md), [architecture](docs/ARCHITECTURE.md), [allocation benchmark](docs/BENCHMARK.md), [field benchmark](docs/OC002.md), [material benchmark](docs/OC003.md) and [coupled screening benchmark](docs/OC004.md).

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To capture all four desktop pages using the actual synthetic calculation, run `OPTCOIL_CAPTURE_DIR=/tmp/optcoil-capture cargo run -p optcoil-app` in a graphical session. This opt-in rendering check exports PNGs and closes the app. Ordinary startup does not run a calculation automatically.

For headless engine work: `cargo test --workspace --exclude optcoil-app`. Add `--offline` when using an already populated Cargo cache. Keep `Cargo.lock` in source control for reproducibility; no customer files belong in the synthetic benchmark directory.

Converra is open source under the [MIT license](LICENSE). Third-party dependencies retain their own licenses, and the measured conductor datasets under `data/materials/` retain theirs — each carries attribution in its own README. Nothing here uploads data or contacts any solver service; every calculation runs locally.
