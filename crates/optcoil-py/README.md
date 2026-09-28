# converra — Python bindings

The Converra engine (`optcoil-search`) as a Python extension module. The
boundary is JSON throughout: case documents, specs and records are the
same schemas the CLI accepts, so results are byte-identical with the
CLI's own runs.

## Build

```bash
pip install maturin
maturin develop --manifest-path crates/optcoil-py/Cargo.toml   # into the active venv
# or build a wheel:
maturin build --release --manifest-path crates/optcoil-py/Cargo.toml
```

## Usage

```python
import json, converra

record_json = converra.run_search(open("case.json").read())          # run record
print(converra.verify_record(record_json))                            # check ledger
converra.render_report(record_json)                                   # HTML report
converra.list_datasets()                                              # embedded datasets
converra.run_sensitivity(case_json, spec_json)                        # sweep record
converra.run_dataset_bakeoff(case_json, spec_json, "bundles/")        # bakeoff record
converra.verify_dataset(bundle_json, registry_json=registry_json)     # bundle checks
```

Every function raises `RuntimeError` on invalid input or a failed run.
`run_search`/`run_sensitivity`/`run_dataset_bakeoff` release the GIL —
the module is safe to call from Python threads. Cancellation is not
exposed; bound runs via the case's `search_limits`.
