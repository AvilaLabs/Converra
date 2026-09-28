# Dataset registry

`datasets.json` is the Avila Labs dataset registry: the canonical list of
issuer-attested material dataset bundles for Converra/OptCoil.

Each entry is independently countersigned by the registry key over

    optcoil-registry-entry/v1|<registry_issuer>|<dataset_id>|<bundle_sha256>|<attestation_sha256>|<status>

so entries verify individually — the file itself carries no signature.
`bundle_sha256` binds the exact attested bundle file bytes; `status` is
`current`, `superseded` or `revoked`.

Verify any bundle with the CLI:

    optcoil dataset verify <bundle.json> --registry datasets.json

An Avila Labs countersignature asserts that the bundle passed the dataset
contract validator and that its declared conventions are internally
consistent — not that we witnessed the measurement. Provenance claims belong
to the issuer identified in each bundle's `attestation`.

Keys: `keys[]` maps `(issuer, key_id)` to the ed25519 public keys trusted to
attest bundles. Issuer key pairs are generated with `optcoil dataset keygen`;
secret keys never enter this repository.

---

`products.json` is the companion product catalogue consumed by the case
builder's product picker — `optcoil-product-registry/v1`. Unlike the
dataset entries above it is **unsigned**: a product row is declared
convenience data (vendor, width, price-with-`price_source`, optional
piece catalogue and policy overrides), not attested evidence. The dataset
binding it carries is a pointer into the attested registry; the bound
dataset's own measured-domain applicability rides every run record.
Prices are `estimated` placeholders until a customer substitutes a
vendor quote.
