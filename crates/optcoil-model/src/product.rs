//! `optcoil-product-registry/v1`: the product catalogue the authoring
//! wizard offers — one entry per purchasable conductor product, carrying
//! exactly the fields `optcoil-bakeoff/v2` consumes plus the display and
//! provenance metadata a picker needs (vendor, product name, notes).
//!
//! A registry product is *declared* convenience data, not attested
//! evidence: unlike `data/registry/datasets.json` entries it carries no
//! countersignature, and `price_source` says honestly where the number
//! came from (`estimated` until a customer substitutes a real quote).
//! The dataset binding is a pointer — the dataset's own measured-domain
//! applicability (bridge width, original tape width, data class) stays
//! with the dataset and surfaces on every run record.
//!
//! `dataset_variants` lists alternate datasets covering the *same*
//! specimen family (e.g. a low-field or model-extended variant) so the
//! picker can offer the coverage/evidence-class trade-off explicitly
//! rather than hiding it inside one id.

use serde::{Deserialize, Serialize};

use crate::{
    ModelError,
    bakeoff::{BakeoffProduct, MaterialPolicyOverride},
    coupled_search::{PieceOffering, PriceSource},
    material::MaterialDataset,
    require,
};

pub const PRODUCT_REGISTRY_SCHEMA: &str = "optcoil-product-registry/v1";

/// One purchasable product — the wizard's pickable row. Field names
/// mirror `BakeoffProduct` so a registry pick converts losslessly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryProduct {
    /// Stable registry identifier (e.g. `superpower-scs-class-12mm`).
    pub product_id: String,
    pub vendor: String,
    /// Display name, e.g. "SCS-class 12 mm".
    pub product_name: String,
    /// Default dataset binding — the evidence behind this product.
    pub dataset_id: String,
    /// Alternate datasets covering the same specimen family (low-field,
    /// model-extended, published-fit), offered as an explicit
    /// coverage/evidence-class choice — never silently substituted.
    #[serde(default)]
    pub dataset_variants: Vec<String>,
    /// Commercial tape width the product is sold at.
    pub tape_width_m: f64,
    #[serde(default)]
    pub tape_thickness_m: Option<f64>,
    pub price_usd_per_m: f64,
    pub price_source: PriceSource,
    #[serde(default)]
    pub piece_offerings: Option<Vec<PieceOffering>>,
    #[serde(default)]
    pub material_policy: Option<MaterialPolicyOverride>,
    /// Provenance note — who priced it, when, and what the dataset
    /// actually measured. Required: an unannotated product is an
    /// unattributed claim.
    pub notes: String,
}

impl RegistryProduct {
    /// Convert into the bakeoff-v2 product payload — the catalogue row
    /// and the comparison row share one meaning.
    pub fn to_bakeoff_product(&self) -> BakeoffProduct {
        BakeoffProduct {
            product_id: self.product_id.clone(),
            dataset_id: self.dataset_id.clone(),
            bundle: None,
            tape_width_m: Some(self.tape_width_m),
            tape_thickness_m: self.tape_thickness_m,
            price_usd_per_m: self.price_usd_per_m,
            price_source: self.price_source,
            piece_offerings: self.piece_offerings.clone(),
            material_policy: self.material_policy.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductRegistry {
    pub schema: String,
    pub registry_issuer: String,
    pub issued_at_unix_ms: i64,
    pub products: Vec<RegistryProduct>,
}

impl ProductRegistry {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let registry: Self = serde_json::from_str(json)?;
        require(
            registry.schema == PRODUCT_REGISTRY_SCHEMA,
            &format!(
                "expected schema {}, got {}",
                PRODUCT_REGISTRY_SCHEMA, registry.schema
            ),
        )?;
        require(
            !registry.products.is_empty(),
            "product registry declares no products",
        )?;
        let mut seen = std::collections::HashSet::new();
        for p in &registry.products {
            require(
                seen.insert(p.product_id.clone()),
                &format!("duplicate product_id '{}'", p.product_id),
            )?;
            require(
                !p.dataset_id.trim().is_empty(),
                &format!("product '{}' binds an empty dataset_id", p.product_id),
            )?;
            require(
                p.tape_width_m.is_finite() && p.tape_width_m > 0.0,
                &format!("product '{}' has nonpositive tape_width_m", p.product_id),
            )?;
            if let Some(t) = p.tape_thickness_m {
                require(
                    t.is_finite() && t > 0.0,
                    &format!(
                        "product '{}' has nonpositive tape_thickness_m",
                        p.product_id
                    ),
                )?;
            }
            require(
                p.price_usd_per_m.is_finite() && p.price_usd_per_m >= 0.0,
                &format!("product '{}' has negative price_usd_per_m", p.product_id),
            )?;
            require(
                !p.notes.trim().is_empty(),
                &format!(
                    "product '{}' carries no notes — an unannotated price is an unattributed claim",
                    p.product_id
                ),
            )?;
            for v in &p.dataset_variants {
                require(
                    !v.trim().is_empty(),
                    &format!("product '{}' lists an empty dataset variant", p.product_id),
                )?;
            }
        }
        Ok(registry)
    }

    /// The bundled catalogue shipped with the binary.
    pub fn embedded() -> Self {
        Self::from_json(PRODUCT_REGISTRY_JSON)
            .expect("the embedded product registry must parse and validate")
    }

    /// Every dataset id a product may bind — defaults plus declared
    /// variants — that resolves to an embedded dataset. External ids
    /// (customer bundles) are skipped here; they resolve at pick time.
    pub fn embedded_dataset_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .products
            .iter()
            .flat_map(|p| std::iter::once(&p.dataset_id).chain(p.dataset_variants.iter()))
            .filter(|id| MaterialDataset::embedded_by_id(id).is_ok())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn product(&self, product_id: &str) -> Option<&RegistryProduct> {
        self.products.iter().find(|p| p.product_id == product_id)
    }
}

const PRODUCT_REGISTRY_JSON: &str = include_str!("../../../data/registry/products.json");

#[cfg(test)]
mod tests {
    use super::*;

    /// Every product in the shipped catalogue must bind a resolvable
    /// embedded dataset — defaults and declared variants alike. A
    /// registry row pointing at nothing is a broken promise at pick
    /// time, so it is pinned here rather than discovered in the wizard.
    #[test]
    fn embedded_registry_validates_and_every_dataset_resolves() {
        let registry = ProductRegistry::embedded();
        assert!(registry.products.len() >= 4);
        for p in &registry.products {
            for id in std::iter::once(&p.dataset_id).chain(p.dataset_variants.iter()) {
                assert!(
                    MaterialDataset::embedded_by_id(id).is_ok(),
                    "product '{}' binds unresolvable dataset '{id}'",
                    p.product_id
                );
            }
        }
        // Embedded ids are the deduplicated union of defaults + variants.
        assert!(registry.embedded_dataset_ids().len() >= 6);
    }

    /// A registry pick converts to the bakeoff-v2 payload losslessly.
    #[test]
    fn registry_product_converts_to_bakeoff_product() {
        let registry = ProductRegistry::embedded();
        let p = registry.product("superpower-scs-class-12mm").unwrap();
        let b = p.to_bakeoff_product();
        assert_eq!(b.product_id, p.product_id);
        assert_eq!(b.dataset_id, p.dataset_id);
        assert_eq!(b.tape_width_m, Some(p.tape_width_m));
        assert_eq!(b.price_usd_per_m, p.price_usd_per_m);
        assert!(matches!(b.price_source, PriceSource::Estimated));
        // Variants carry the evidence-class choice explicitly.
        assert_eq!(p.dataset_variants.len(), 2);
    }

    /// Validation rejects the dishonest shapes: unknown schema,
    /// duplicate ids, unattributed prices, unresolvable-by-construction
    /// widths.
    #[test]
    fn validation_rejects_dishonest_registry_rows() {
        let mut bad: serde_json::Value = serde_json::from_str(PRODUCT_REGISTRY_JSON).unwrap();
        bad["schema"] = serde_json::json!("optcoil-product-registry/v0");
        assert!(serde_json::from_value::<ProductRegistry>(bad.clone()).is_ok());
        assert!(ProductRegistry::from_json(&serde_json::to_string(&bad).unwrap()).is_err());

        let mut bad: serde_json::Value = serde_json::from_str(PRODUCT_REGISTRY_JSON).unwrap();
        bad["products"][1]["product_id"] = bad["products"][0]["product_id"].clone();
        assert!(ProductRegistry::from_json(&serde_json::to_string(&bad).unwrap()).is_err());

        let mut bad: serde_json::Value = serde_json::from_str(PRODUCT_REGISTRY_JSON).unwrap();
        bad["products"][0]["notes"] = serde_json::json!("  ");
        assert!(ProductRegistry::from_json(&serde_json::to_string(&bad).unwrap()).is_err());

        let mut bad: serde_json::Value = serde_json::from_str(PRODUCT_REGISTRY_JSON).unwrap();
        bad["products"][0]["tape_width_m"] = serde_json::json!(0.0);
        assert!(ProductRegistry::from_json(&serde_json::to_string(&bad).unwrap()).is_err());
    }
}
