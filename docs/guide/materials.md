# Conductor data and imports

Converra evaluates critical-current data against local conductor field, temperature and tape orientation. The usable domain belongs to each dataset. The bore field alone does not describe the peak field on the tape.

## Embedded data

Embedded sources include attributed measured characterizations, a labeled model extension and published model-fit variants. Most measured tables cover approximately 20–40 K and fields up to 8 T, with distinct lower field limits and angular coverage. Higher-field fits and extensions are model-informed inputs.

Review the dataset's source, measured bridge width, original tape width, measurement criterion and supported domain. Transferring bridge current to full-width tape requires declared assumptions. A signature establishes a provenance binding; it does not establish measurement accuracy or conductor qualification.

## Import a table

Use **File → Import spreadsheet / CSV** for UTF-8 CSV/TSV or Excel XLSX. Select the worksheet, preview rows and map measured temperature, field, angle and critical current. Supply n-value from a column or an explicit constant.

Choose units for each quantity. Map nominal coordinates separately, or explicitly choose to reuse measured coordinates as the nominal grid. Record attribution, usage terms, material identity and measurement declarations. Current-per-width conversion uses the declared measured bridge width.

Imports are limited to 32 MiB. Invalid, duplicate or unsupported data requires correction; the importer does not invent metadata or extrapolate values. A validated import can be applied to a case or exported as a material bundle.

## Attach all dependencies

A case binds each material by dataset ID and CSV SHA-256. The supplied source must match both. In the Materials view, resolve every base and graded tape dependency. CLI commands accept repeated bundle flags:

```bash
optcoil coupled-search graded-case.json \
  --dataset-bundle base.bundle.json --dataset-bundle grade.bundle.json \
  --output runs/graded.json
```

Metadata plus canonical CSV is also supported. An omitted/null metadata hash is computed; a declared mismatched hash is rejected. Review packages include all resolved dependencies.

Canonical columns, signed bundles and registry checks: [dataset reference](https://github.com/AvilaLabs/Converra/blob/main/docs/DATASETS.md) and [vendor guide](https://github.com/AvilaLabs/Converra/blob/main/docs/VENDOR_DATASETS.md).
