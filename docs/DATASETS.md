# Customer material datasets

OptCoil runs coupled searches against a *material dataset*: a measured
critical-current table `Ic(B, T, θ)` plus provenance metadata. Five datasets
are embedded in the binary (the published Robinson characterizations —
three SuperPower AP variants, Shanghai Superconductor HFLT, and THEVA
Pro-Line Advanced Pinning — all over the identical declared 20–40 K ×
1–8 T × 0–180° window); customer data — another vendor's tape, QC
measurements, a different superconductor — is supplied at run time,
without rebuilding.

## The identity contract

A coupled-search case declares two fields:

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

**Metadata/CSV pair** — pass both paths directly:

```bash
optcoil coupled-search case.json --metadata material.json --csv measurements.csv
```

In the workbench, open the case first: the Materials page reports the declared
dataset and whether it resolved (embedded, a sibling `datasets/<id>.json` or
`<id>.json` beside the case, or missing). "Load dataset bundle…" picks a
bundle explicitly. **Load metadata + CSV…** accepts the attributed metadata and
canonical measurement CSV directly, natively and in the browser. The builder
binds their validated id and computed CSV SHA-256. Missing or null metadata hashes
are computed; a supplied mismatched hash is rejected. This does not supply missing
attribution, infer spreadsheet units, or attest a dataset. A case whose dataset
cannot be resolved will not run.

Review packages include exact referenced dataset bundles and an artifact manifest.
Browser tar archives preserve the dependency directory layout; extract before
verification. The CLI and workbench search entry points currently accept one
explicit external base dataset plus embedded spec datasets; additional external
spec dependencies are reported by preflight and require a supported resolution
path rather than silent substitution.

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

The reference registry lives at `data/registry/datasets.json` and lists
every embedded measured dataset's attested bundle (issuer `avila-labs`);
a countersigned mirror registry with identical entries is maintained in
the separate `converra-evidence` repository.

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

After authoring, `dataset-bundle` reports the CSV's SHA-256 — paste it into the
case's `material.csv_sha256` (the workbench case builder does this
automatically for embedded datasets and accepts a pasted hash for external
ones), then run a `material-validate`-equivalent check by loading it.

## Operating domain

The case's `operating.temperature_k` must lie strictly inside the dataset's
nominal temperature span; queries outside the declared field/angle grid fail
as `INCONCLUSIVE`/unsupported rather than extrapolate. Check a dataset's
envelope before authoring cases against it (`material-inspect`, or the
dataset summary on the workbench Materials page).
