# Robinson / SuperPower AP, version 3 -- labeled model extension above 8 T

Mixed provenance dataset, **CC BY 4.0** for the measured rows; the modeled
rows are an Avila Labs artifact of `tools/prepare_oc016.py` and carry no
third-party measurement claim.

**This is not a measured dataset.** Every node at nominal field above 8 T
is a model continuation. Any engineering verdict that leans on those nodes
is model-informed and must not be represented as measured-data-verified.

## What this is

`robinson-superpower-ap-v3-lowfield` ends at 8 T applied field. OC-012's
coverage-blocked candidates see 9-17 T peak conductor fields, so they
return INCONCLUSIVE rather than extrapolate. This dataset exists to answer
one question: *which of those candidates would pass if high-field
measurements confirmed a smooth continuation?* It is a sensitivity map
pending real data (see `docs/RESEARCH_ANCHORS.md` for the measured-data
candidates being pursued).

## Construction

- All 3,225 measured rows of `robinson-superpower-ap-v3-lowfield` are
  copied byte-identically (asserted by content hash in
  `preparation-audit.json`).
- 860 modeled rows extend each of the 215 (temperature, angle) curves to
  nominal fields 10, 12.5, 16 and 20 T via an anchored power law:
  `Ic(B) = Ic_measured(8 T) * (B/8)^(-alpha)`, with
  `alpha = max(3-8 T tail fit, local 7->8 T slope, 0.1)` and a uniform
  conservative margin of ~26% applied to every modeled node.
- The margin is set by held-out edge validation: the same continuation
  fitted only on data below 5-7 T over-predicts the measured 7/8 T nodes
  by at most ~21% across all curves; the margin exceeds that bound plus a
  5% pad. Modeled nodes are therefore at-or-below what this model family
  would have predicted at the measured edge -- conservative by
  construction, in the direction that under-claims capacity.
- Modeled rows are marked by `source_row >= 900000`. Their measured
  coordinates (`temperature_k`, `angle_from_normal_deg`) inherit the
  curve's 8 T anchor row so the measured-coordinate sheets stay aligned;
  `applied_field_t` equals the nominal extended level; `n_value` is
  carried forward from the 8 T row.

## Schema

`optcoil-measured-material/v2` with
`data_class = "measured_with_model_extension"`. Schema v1 remains
restricted to purely measured datasets, so this file cannot be relabeled
into the original schema identity. The `data/materials/` directories for
the purely measured datasets are untouched and byte-identical.

## Honest limits

Beyond every limitation of the source dataset: the extension is a
power-law family fitted to 3-8 T tails, anchored at the last measured
node. Real REBCO curves steepen toward the irreversibility field; the
margin is a guard, not a guarantee. Temperatures remain 20-40 K measured /
21 K used; no new physics (strain, irreversibility cutoff, per-width
rescale to a commercial tape) is introduced or implied.
