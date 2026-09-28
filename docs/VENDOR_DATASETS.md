# Issuing measured conductor datasets for Converra

*For conductor vendors. ~15 minutes of engineering time; no new measurement
work required.*

## What you get

Magnet designers choose tape inside the tools that compute purchase volume.
A signed Converra dataset bundle puts your tape's **real measured**
`Ic(B, T, θ)` — not a spec-sheet derating — inside every screening run a
designer performs, at their actual operating point. The bundle travels
between your lab, the designer, and their reviewers without losing
provenance: every recipient can verify offline that the data is yours and
unmodified.

The format is open and verifiers require no license. You can hand a signed
bundle to any customer directly; the registry adds discoverability and a
second signature.

## The artifact chain

```
your lab CSV + metadata   ── dataset build ──►   unsigned bundle (v2)
unsigned bundle + key     ── dataset attest ──►  signed bundle
signed bundle             ── registry     ──►    countersigned entry
```

- **Bundle** (`optcoil-material-dataset/v2`): one JSON file — your metadata
  and the measurement CSV embedded verbatim. `csv_sha256` inside the
  metadata pins the exact bytes your lab produced.
- **Attestation**: your ed25519 signature over
  `optcoil-dataset-attestation/v1|<issuer>|<key_id>|<dataset_id>|<csv_sha256>|<issued_at>`
  — a delimited string, so every implementation signs identical bytes.
- **Registry entry**: Avila Labs countersigns `bundle_sha256` (the exact
  file bytes) with a status: `current`, `superseded`, or `revoked`.

## How to issue one

```bash
# once: generate your issuer keypair (keep the file private)
optcoil dataset keygen --issuer your-vendor --key-id v2026-a \
  --output your-vendor.key.json

# per dataset: assemble and sign
optcoil dataset build material.json measurements.csv --output tape.bundle.json
optcoil dataset attest tape.bundle.json --key your-vendor.key.json \
  --output tape.signed.json

# check your own work
optcoil dataset verify tape.signed.json --pubkey <your public key>
```

Send us the signed bundle and your public key. We run the contract
validator, add your `(issuer, key_id) → public key` mapping, and publish a
countersigned entry in the public registry.

The metadata JSON your bundle embeds is described in `DATASETS.md`: it
carries sample id, measurement dates, provenance hashes (e.g. the source
spreadsheet's SHA-256), the electrical criterion, field basis, angle
convention, measured uncertainty, and a free-text `limitations` list —
your disclosures, verbatim, propagated into every run record that consumes
the data.

## What our countersignature means — and doesn't

An Avila Labs countersignature asserts that the bundle passed the dataset
contract validator and that its declared conventions are internally
consistent. It is **not** a claim that we witnessed or repeated your
measurement. Provenance belongs to you, under your name, in the
attestation. If a bundle's metadata claims measured data, the format keeps
it distinguishable from model extensions (`data_class`) — your customers
see the difference and so do we.

## Private datasets

Registry listing is optional. A bundle you sign for one customer can be
verified against your public key directly (`--pubkey`) or against a
customer-private registry — the raw data never needs to be public to be
verifiable.

## Licensing

The bundle's `license` / `license_url` metadata fields are yours to set —
e.g. "for use in Converra-compatible tools, attribution required." The
signature covers the data's identity, not its terms; terms travel with the
bundle as declared metadata.

## Corrections and new measurements

Data never mutates in place. A revised dataset gets a new `dataset_id`
(e.g. `metox-m345-ap-v2`); we mark the old registry entry `superseded` and
add the new one. If you discover a bad measurement, we mark the entry
`revoked` — verifiers then fail closed on that artifact.
