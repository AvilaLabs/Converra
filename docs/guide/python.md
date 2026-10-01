# Python

The `converra` Python module wraps the Rust search engine. Cases, specifications and returned records use the same JSON contracts as the CLI. Wheels are not yet published on PyPI.

From the source checkout, create and activate a virtual environment, then build:

```bash
python -m pip install maturin
maturin develop --release --manifest-path crates/optcoil-py/Cargo.toml
```

## Run and inspect a study

```python
import json
from pathlib import Path
import converra

case_json = Path("benchmarks/coupled/oc-007.json").read_text()
record_json = converra.run_search(case_json, threads=2)
checks = json.loads(converra.verify_record(record_json, case_json))
print(checks)
Path("runs/python-design.json").write_text(record_json)
Path("runs/python-design.html").write_text(converra.render_report(record_json))
```

Create the `runs` directory first and use fresh output names. Inspect the returned verdicts as well as the verification checks. `verify_record` binds the case and checks the ledger; it does not independently validate physics.

## Other operations

`list_datasets` lists embedded data. `run_sensitivity` evaluates a case and sensitivity specification. `run_dataset_bakeoff` compares datasets from a supplied bundle directory. `verify_dataset` checks a material bundle, with optional registry information.

The expensive run calls release the Python GIL. Bound calculations through the case's search limits and thread option. Python cancellation is not exposed, and this binding does not expose every GUI/MCP workflow.

API details: [Python binding reference](https://github.com/AvilaLabs/Converra/blob/main/crates/optcoil-py/README.md) and [binding implementation](https://github.com/AvilaLabs/Converra/blob/main/crates/optcoil-py/src/lib.rs).
