# Customer material datasets

OptCoil runs coupled searches against critical-current tables `Ic(B, T, θ)`
and their provenance metadata. Ten datasets are embedded: five attributed
measured characterizations, one explicitly labeled model extension, and four
published model-fit variants. Their operating domains differ; consult each
dataset's metadata. Customer measurements can be supplied at run time without
rebuilding. Measured, modeled and synthetic evidence remain distinguishable.

## The identity contract

A coupled-search case declares two fields for its base material and for each
explicit graded tape specification:

- `material.dataset_id` — must equal the dataset's metadata `id`
- `material.csv_sha256` — the SHA-256 of the dataset's CSV bytes

Every loader enforces both. A mismatched dataset is **rejected, never
silently substituted**: if the file you supply hashes differently than the
case declares, the run refuses to start. The case hash (`case_sha256` in the
run record) therefore binds the dataset identity end-to-end — but identity is
not endorsement; provenance and supported operating domain are the dataset's
own declarations and are carried through to the record.

## Supplying a dataset

Two equivalent paths; both run the same validation as `material-validate`
(exact SI column headers, metadata hash match, domain checks, 32 MiB cap).

**Single-file bundle** (`optcoil-material-dataset/v1` or signed `/v2`) — one
JSON containing `schema`, `metadata`, and the CSV as a `csv_data` string.
Package a pair:

```bash
optcoil dataset build material.json measurements.csv \
  --output my-tape.bundle.json    # v2 bundle, unsigned
optcoil coupled-search case.json --dataset-bundle my-tape.bundle.json
```

(`dataset-bundle` remains for writing unsigned v1 bundles.)

For a graded case, repeat the flag for every external dataset referenced by the
base or tape-spec bindings. The same flags apply to `preflight` and `sensitivity`:

```bash
optcoil coupled-search graded-case.json \
  --dataset-bundle base.bundle.json --dataset-bundle graded-tape.bundle.json
```

Each bundle must match a declared id and CSV hash. Duplicate or undeclared ids
are rejected. Omitted embedded dependencies resolve from the embedded store;
omitted external dependencies prevent the run.

`coupled-refine` currently accepts a single supplied dataset and rejects multiple
bundle flags explicitly; the multi-binding workflow uses `coupled-search`.

**Metadata/CSV pair** — pass both paths directly:

```bash
optcoil coupled-search case.json --metadata material.json --csv measurements.csv
```

In the workbench, open the case first: the Materials page reports every declared
dependency and whether it resolved (embedded, a sibling `datasets/<id>.json` or
`<id>.json` beside the case, or missing). "Load dataset bundle…" picks a
bundle explicitly. **Load metadata + CSV…** accepts the attributed metadata and
canonical measurement CSV directly, natively and in the browser. The builder
binds their validated id and computed CSV SHA-256. Missing or null metadata hashes
are computed; a supplied mismatched hash is rejected. This does not supply missing
attribution, infer spreadsheet units, or attest a dataset. A case whose dataset
cannot be resolved will not run.

Load a matching bundle or metadata/CSV pair for each external dependency. The
workbench retains those sources across revisions that preserve their bindings,
and uses them for search, margin sweeps, record verification and review export.

Review packages include every resolved material dependency, preserving the exact
supplied bundle JSON, CSV bytes and attestations, with an artifact manifest.
Issued embedded bundles retain their original bytes and signatures where
available. Their README contains a
rerun command listing each packaged bundle; it needs no original input directory.
Browser tar archives preserve the directory layout; extract before verification
or rerunning.

## Signed bundles — `optcoil-material-dataset/v2`

A v2 bundle adds an `attestation`: the issuer's ed25519 signature over

    optcoil-dataset-attestation/v1|<issuer>|<key_id>|<dataset_id>|<csv_sha256>|<issued_at_unix_ms>

Delimited text, not JSON — every implementation signs identical bytes. Since
`csv_sha256` already pins the measurement bytes, one signature binds the
whole dataset to its issuer. The attestation carries **no public key** —
trusted keys live in a dataset registry (`optcoil-dataset-registry/v1`), so a
forged bundle cannot supply its own trust anchor.

Issuer workflow (see `docs/VENDOR_DATASETS.md` for the full program):

```bash
optcoil dataset keygen --issuer my-vendor --key-id v2026-a --output issuer.key.json
optcoil dataset attest bundle.json --key issuer.key.json --output signed.json
```

Verifier workflow — offline, hash- and signature-bound:

```bash
optcoil dataset verify signed.json --registry datasets.json
```

Checks: bundle schema and csv binding, attestation signature against the
registry's key for `(issuer, key_id)`, the registry entry's `bundle_sha256`
(the exact file bytes), the Avila countersignature, and entry status
(`current` / `superseded` / `revoked`). Unsigned bundles are legal — they
report `NOT_CHECKED`, not failure. `optcoil verify --dataset` reports
attestation provenance alongside the record binding; the flag is
repeatable, and graded run records (schema v13+) declare one dataset
identity per tape spec — supplying fewer bundles than declared identities
reports partial coverage as `NOT_CHECKED`, not a pass.

An Avila Labs registry countersignature asserts that a bundle passed the
dataset contract validator — it is **not** a claim that we witnessed the
measurement. Provenance belongs to the issuer named in the attestation.

The reference registry lives at `data/registry/datasets.json` and contains issuer
keys and entries for selected issued bundles. Check the entry for each dataset;
an available signature does not establish registry coverage.

## Authoring the files

Copy an embedded dataset's layout — `data/materials/robinson-superpower-ap-v3/`
is the reference. The CSV's exact columns (SI units, in order):

```
source_row, nominal_temperature_k, nominal_field_t, nominal_angle_deg,
temperature_k, applied_field_t, angle_from_normal_deg, ic_a_per_m,
bridge_ic_a, n_value
```

The metadata JSON (`optcoil-measured-material/v1` or `/v2`) carries `id`,
`csv_sha256`, `data_class` (`"measured"` or a labeled model/synthetic class),
provenance (`source_doi`, `authors`, `license`, hashes of upstream artifacts),
the measurement conventions (`field_basis`, `angle_convention`,
`electric_field_criterion_v_per_m`), and `limitations` — free-text disclosures
that propagate into run records.

`data_class` and `limitations` are how measured customer data stays
distinguishable from modeled or synthetic data in every downstream artifact.

When editing case JSON directly, set each binding's `csv_sha256` to the validated
CSV hash. The workbench builder fills the base binding automatically for embedded
data and for an imported metadata/CSV pair; loading a pair into an existing case
requires its id and computed hash to match a declared binding. Validate the inputs
before calculating.

## Operating domain

The case's `operating.temperature_k` must lie strictly inside the dataset's
nominal temperature span; queries outside the declared field/angle grid fail
as `INCONCLUSIVE`/unsupported rather than extrapolate. Check a dataset's
envelope before authoring cases against it (`material-inspect`, or the
dataset summary on the workbench Materials page).
