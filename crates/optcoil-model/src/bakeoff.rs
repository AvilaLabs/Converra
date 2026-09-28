//! `optcoil-bakeoff/v1`: the vendor bake-off spec — one coupled-search
//! case run once per declared dataset, ranked into a comparison table.
//!
//! A bake-off answers the procurement question the single-dataset case
//! cannot ask: *which vendor's measured tape gives the cheapest
//! screening-passing design for this requirement*. Every entry mutates only
//! `material.dataset_id` / `material.csv_sha256` — the declared policies
//! (low-field clamp, field basis, angle mapping, width transfer) stay
//! identical so the comparison is like-for-like — and runs an ordinary
//! `run_coupled_search_case` whose acceptance recomputation stands on its
//! own. Entries that cannot resolve or validate record an error and keep
//! the bake-off's other rows honest; they are data, not failures of the
//! comparison.

use serde::{Deserialize, Serialize};

use crate::{
    ModelError,
    coupled::{
        AngleMapping, FieldBasisMapping, FieldMagnitudePolicy, LowFieldPolicy, MaterialSettings,
        MirrorPolicy,
    },
    coupled_search::{PieceOffering, PriceSource},
    require,
};

pub const BAKEOFF_SPEC_SCHEMA: &str = "optcoil-bakeoff/v1";
pub const BAKEOFF_SPEC_SCHEMA_V2: &str = "optcoil-bakeoff/v2";

/// One dataset in the comparison — resolved by `dataset_id` from the
/// embedded registry, or from `bundle` (a path to an
/// `optcoil-material-dataset` v1/v2 bundle) for customer data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffDataset {
    /// Must equal the resolved dataset's metadata `id`.
    pub dataset_id: String,
    /// Optional bundle path for a runtime (non-embedded) dataset. The
    /// entry's `csv_sha256` binds whichever bytes actually resolved.
    #[serde(default)]
    pub bundle: Option<String>,
}

/// v2: the per-product material-policy override — declared fields replace
/// the case's `material` policies for that row. `dataset_id`,
/// `csv_sha256` and `method` are not overridable: identity is bound by
/// the resolved dataset, and the method is a dataset property. Every
/// override is recorded verbatim in the row's `case_json`, so a row that
/// wins under a different policy says so.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialPolicyOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_mapping: Option<AngleMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_policy: Option<MirrorPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_basis_mapping: Option<FieldBasisMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_magnitude_policy: Option<FieldMagnitudePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_field_policy: Option<LowFieldPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_field_clamp_t: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monotonicity_tolerance: Option<f64>,
}

impl MaterialPolicyOverride {
    pub fn apply(&self, m: &mut MaterialSettings) {
        if let Some(v) = self.angle_mapping {
            m.angle_mapping = v;
        }
        if let Some(v) = self.mirror_policy {
            m.mirror_policy = v;
        }
        if let Some(v) = self.field_basis_mapping {
            m.field_basis_mapping = v;
        }
        if let Some(v) = self.field_magnitude_policy {
            m.field_magnitude_policy = v;
        }
        if let Some(v) = self.low_field_policy {
            m.low_field_policy = v;
        }
        if let Some(v) = self.low_field_clamp_t {
            m.low_field_clamp_t = v;
        }
        if let Some(v) = self.monotonicity_tolerance {
            m.monotonicity_tolerance = v;
        }
    }
}

/// v2: one vendor *product* in the comparison — the dataset plus the
/// attributes a purchasing decision needs: the product's real tape width
/// (mutating `fixed_geometry.tape_width_m`, which changes winding
/// topology — a 4 mm product is a different coil, not a label), optional
/// stack thickness for the bend bound, a declared price with its
/// provenance class, an optional piece-length catalogue, and optional
/// justified policy overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffProduct {
    /// Display identity — `acme-4mm-st`, not necessarily a part number.
    pub product_id: String,
    /// Must equal the resolved dataset's metadata `id`.
    pub dataset_id: String,
    #[serde(default)]
    pub bundle: Option<String>,
    /// The product's manufactured tape width. Absent keeps the case's
    /// declared `tape_width_m` (a dataset-only procurement comparison).
    #[serde(default)]
    pub tape_width_m: Option<f64>,
    /// The product's full tape stack thickness — applied to
    /// `manufacturing.tape_thickness_m`; requires the case to declare a
    /// manufacturing block.
    #[serde(default)]
    pub tape_thickness_m: Option<f64>,
    /// The product's declared conductor price — required in v2: a
    /// procurement comparison without a price is a materials table.
    pub price_usd_per_m: f64,
    /// Provenance class of `price_usd_per_m` — required so a `synthetic`
    /// placeholder is never mistaken for a quote.
    pub price_source: PriceSource,
    /// The product's piece-length catalogue — applied to
    /// `cost.piece_offerings`; requires the case to declare
    /// `cost.piece_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_offerings: Option<Vec<PieceOffering>>,
    /// Justified per-product policy overrides — applied to the case's
    /// `material` policies for this row only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_policy: Option<MaterialPolicyOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BakeoffSpec {
    pub schema: String,
    /// Who declared this comparison and why — the list is an
    /// engineering choice.
    pub provenance: String,
    /// v1: 1..=16 datasets, `dataset_id`s distinct. Absent on v2.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<BakeoffDataset>,
    /// v2: 1..=16 products, `product_id`s distinct. Absent on v1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub products: Vec<BakeoffProduct>,
}

impl BakeoffSpec {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let spec: Self = crate::encoding::parse_case(json)?;
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == BAKEOFF_SPEC_SCHEMA || self.schema == BAKEOFF_SPEC_SCHEMA_V2,
            "bakeoff spec schema must be optcoil-bakeoff/v1 or optcoil-bakeoff/v2",
        )?;
        require(
            !self.provenance.trim().is_empty(),
            "bakeoff spec provenance is empty",
        )?;
        if self.schema == BAKEOFF_SPEC_SCHEMA {
            require(
                self.products.is_empty(),
                "optcoil-bakeoff/v1 declares `datasets`, not `products`",
            )?;
            require(
                (1..=16).contains(&self.datasets.len()),
                "bakeoff spec must declare 1..=16 datasets",
            )?;
            for entry in &self.datasets {
                require(
                    !entry.dataset_id.trim().is_empty(),
                    "bakeoff dataset_id is empty",
                )?;
            }
            let mut ids: Vec<&str> = self
                .datasets
                .iter()
                .map(|d| d.dataset_id.as_str())
                .collect();
            ids.sort_unstable();
            ids.dedup();
            require(
                ids.len() == self.datasets.len(),
                "bakeoff dataset ids must be distinct",
            )?;
        } else {
            require(
                self.datasets.is_empty(),
                "optcoil-bakeoff/v2 declares `products`, not `datasets`",
            )?;
            require(
                (1..=16).contains(&self.products.len()),
                "bakeoff spec must declare 1..=16 products",
            )?;
            for p in &self.products {
                require(
                    !p.product_id.trim().is_empty(),
                    "bakeoff product_id is empty",
                )?;
                require(
                    !p.dataset_id.trim().is_empty(),
                    "bakeoff product dataset_id is empty",
                )?;
                require(
                    p.price_usd_per_m.is_finite() && p.price_usd_per_m >= 0.0,
                    "bakeoff product price_usd_per_m must be a non-negative finite number",
                )?;
                if let Some(w) = p.tape_width_m {
                    require(
                        w.is_finite() && w > 0.0,
                        "bakeoff product tape_width_m must be positive",
                    )?;
                }
                if let Some(t) = p.tape_thickness_m {
                    require(
                        t.is_finite() && t > 0.0,
                        "bakeoff product tape_thickness_m must be positive",
                    )?;
                }
            }
            let mut ids: Vec<&str> = self
                .products
                .iter()
                .map(|p| p.product_id.as_str())
                .collect();
            ids.sort_unstable();
            ids.dedup();
            require(
                ids.len() == self.products.len(),
                "bakeoff product ids must be distinct",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_json(datasets: &str) -> String {
        format!(
            r#"{{"schema": "optcoil-bakeoff/v1",
            "provenance": "unit test fixture", "datasets": [{datasets}]}}"#
        )
    }

    #[test]
    fn spec_validates() {
        let spec =
            BakeoffSpec::from_json(&spec_json(r#"{"dataset_id": "a"}, {"dataset_id": "b"}"#))
                .unwrap();
        assert_eq!(spec.datasets.len(), 2);
    }

    #[test]
    fn spec_rejects_duplicate_ids() {
        assert!(
            BakeoffSpec::from_json(&spec_json(r#"{"dataset_id": "a"}, {"dataset_id": "a"}"#,))
                .is_err()
        );
    }

    #[test]
    fn spec_rejects_empty_datasets() {
        assert!(BakeoffSpec::from_json(&spec_json("")).is_err());
    }

    #[test]
    fn spec_rejects_wrong_schema() {
        let bad = spec_json(r#"{"dataset_id": "a"}"#).replace("v1", "v9");
        assert!(BakeoffSpec::from_json(&bad).is_err());
    }

    #[test]
    fn spec_rejects_empty_id() {
        assert!(BakeoffSpec::from_json(&spec_json(r#"{"dataset_id": "  "}"#)).is_err());
    }

    fn product_spec_json(products: &str) -> String {
        format!(
            r#"{{"schema": "optcoil-bakeoff/v2",
            "provenance": "unit test fixture", "products": [{products}]}}"#
        )
    }

    #[test]
    fn v2_spec_validates_products() {
        let spec = BakeoffSpec::from_json(&product_spec_json(
            r#"{"product_id": "a", "dataset_id": "d1",
                "tape_width_m": 0.004, "price_usd_per_m": 42.0,
                "price_source": "quoted"},
               {"product_id": "b", "dataset_id": "d2",
                "price_usd_per_m": 30.0, "price_source": "estimated"}"#,
        ))
        .unwrap();
        assert_eq!(spec.products.len(), 2);
    }

    #[test]
    fn v2_rejects_mixed_lists_and_duplicates() {
        assert!(
            BakeoffSpec::from_json(
                &product_spec_json(
                    r#"{"product_id": "a", "dataset_id": "d", "price_usd_per_m": 1.0,
                        "price_source": "quoted"}"#
                )
                .replace(
                    "\"products\"",
                    "\"datasets\": [{\"dataset_id\": \"x\"}], \"products\""
                )
            )
            .is_err()
        );
        assert!(
            BakeoffSpec::from_json(&product_spec_json(
                r#"{"product_id": "a", "dataset_id": "d1", "price_usd_per_m": 1.0,
                    "price_source": "quoted"},
                   {"product_id": "a", "dataset_id": "d2", "price_usd_per_m": 2.0,
                    "price_source": "quoted"}"#
            ))
            .is_err()
        );
    }

    #[test]
    fn v2_requires_price_and_rejects_bad_dimensions() {
        // price_usd_per_m is required (missing field → parse error).
        assert!(
            BakeoffSpec::from_json(&product_spec_json(
                r#"{"product_id": "a", "dataset_id": "d", "price_source": "quoted"}"#
            ))
            .is_err()
        );
        // Non-positive widths/thicknesses are rejected.
        assert!(
            BakeoffSpec::from_json(&product_spec_json(
                r#"{"product_id": "a", "dataset_id": "d", "tape_width_m": -0.004,
                    "price_usd_per_m": 1.0, "price_source": "quoted"}"#
            ))
            .is_err()
        );
        assert!(
            BakeoffSpec::from_json(&product_spec_json(
                r#"{"product_id": "a", "dataset_id": "d", "tape_thickness_m": 0.0,
                    "price_usd_per_m": 1.0, "price_source": "quoted"}"#
            ))
            .is_err()
        );
        // v1 spec cannot declare products.
        assert!(
            BakeoffSpec::from_json(
                &product_spec_json(
                    r#"{"product_id": "a", "dataset_id": "d", "price_usd_per_m": 1.0,
                        "price_source": "quoted"}"#
                )
                .replace("v2", "v1")
            )
            .is_err()
        );
    }
}
