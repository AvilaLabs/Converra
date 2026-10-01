# Command-line workflow

The executable is `optcoil`. With a source checkout, replace it with `cargo run --release --`; Cargo defaults to the CLI package. Examples below use repository-relative fixtures.

## A small reference run

```bash
optcoil demo
optcoil preflight benchmarks/coupled/first-study.json
optcoil coupled-search benchmarks/coupled/first-study.json \
  --output runs/first-study.json
```

The first measured study intentionally yields an unresolved selection. The CLI writes its diagnostic record and exits nonzero because the search is not fully passing. Read the record before treating that exit as a software error.

## A larger coupled search

```bash
optcoil coupled-search benchmarks/coupled/oc-007.json \
  --output runs/design.json
optcoil verify runs/design.json benchmarks/coupled/oc-007.json
optcoil report runs/design.json --output runs/design.html
optcoil bom runs/design.json --output runs/design.bom.json
```

OC-007 has measured conductor data and synthetic prices. Its search and refined checks can take several minutes. BOM export requires an eligible passing optimum.

Pass repeated `--dataset-bundle` flags to `preflight`, `coupled-search` and `sensitivity` for every external dependency. Output files are protected; use a new filename for each run.

Use `optcoil --help` and `optcoil COMMAND --help` for the installed version's flags. The command families cover fields, material queries, sensitivity, dataset comparisons, reporting, review packages and source-build scenario studies.

Next: [export and review](exports.md). Detailed examples: [first-study reference](https://github.com/AvilaLabs/Converra/blob/main/docs/TUTORIAL.md).
