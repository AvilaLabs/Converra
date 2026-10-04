//! Synthetic reel inventory generator (CR-02). Draws reels from published
//! variation statistics around a product-map row, labels everything
//! `synthetic`, and writes the hidden factors to a separate truth file.
//!
//! The generator is deterministic: `splitmix64` seeded from the spec, with
//! standard normals from the Box-Muller transform. The draw order is fixed
//! (see `generate`), so a different implementation can reproduce the output.
//! Exp, ln, cos and sqrt come from the platform's math library, so
//! bit-for-bit agreement holds between builds that share one.

use std::collections::BTreeMap;

use optcoil_model::{
    material::MaterialDataset,
    reel::{
        AbOffset, AbOffsetMethod, EvidenceClass, LengthProfile, MapReference, ProductMapRef,
        ProvenanceSource, REEL_INVENTORY_SCHEMA, REEL_PASSPORT_SCHEMA, ReelGeometry, ReelInventory,
        ReelPassport, ReelProduct, ReelProvenance, passport_sha256,
    },
    synthetic::{
        DEFAULT_LOT_RELATIVE_SD, DEFAULT_TRANSFER_LOG_SD, SYNTHETIC_TRUTH_SCHEMA, SyntheticSpec,
        SyntheticTruth, TruthReel,
    },
};

use crate::{
    RunError,
    reel::{Resolved, resolve_map},
};

const TWO_POW_MINUS_53: f64 = 1.0 / 9_007_199_254_740_992.0;
/// Published along-length relative SD range for some manufacturers.
const PUBLISHED_LENGTH_SD: (f64, f64) = (0.02, 0.03);

/// The standard splitmix64 generator.
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// One draw: (u >> 11) x 2^-53, in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * TWO_POW_MINUS_53
    }

    /// A standard normal by Box-Muller from exactly two draws. The first
    /// uniform must lie in (0, 1], so an exact zero becomes 2^-53, the next
    /// representable value above it on this grid. The cosine branch is used.
    pub fn normal(&mut self) -> f64 {
        let mut u1 = self.uniform();
        let u2 = self.uniform();
        if u1 == 0.0 {
            u1 = TWO_POW_MINUS_53;
        }
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Generates the inventory and the truth reels. `nominal_a` is the nominal
/// reel level: the referenced row's Ic per width times the width.
fn generate(spec: &SyntheticSpec, nominal_a: f64) -> (ReelInventory, Vec<TruthReel>) {
    let mut rng = SplitMix64::new(spec.seed);
    let range = spec.reel_length_m;
    let p = spec.profile;
    let r = p.resolution_m;
    let sigma = spec.length_relative_sd;
    let mut passports = Vec::with_capacity(spec.reel_count as usize);
    let mut truth = Vec::with_capacity(spec.reel_count as usize);
    for index in 0..spec.reel_count {
        // 1. Length.
        let length = range.min + (range.max - range.min) * rng.uniform();
        // 2. Lot factor.
        let lot = (1.0 + spec.lot_sd() * rng.normal()).max(0.05);
        // 3. Profile positions.
        let mut positions = Vec::new();
        let mut k = 0u64;
        while (k as f64) * r <= length {
            positions.push((k as f64) * r);
            k += 1;
        }
        if positions.last().is_none_or(|last| *last != length) {
            positions.push(length);
        }
        // 4. Along-length factor: stationary AR(1).
        let mut points = Vec::with_capacity(positions.len());
        let mut epsilon = 0.0;
        for (i, x) in positions.iter().enumerate() {
            let z = rng.normal();
            epsilon = if i == 0 {
                sigma * z
            } else {
                let rho = (-(x - positions[i - 1]) / spec.length_correlation_m).exp();
                rho * epsilon + sigma * (1.0 - rho * rho).sqrt() * z
            };
            points.push([*x, nominal_a * lot * (1.0 + epsilon).max(0.0)]);
        }
        // 5. Hidden transfer factor.
        let tsd = spec.transfer_sd();
        let transfer = (tsd * rng.normal() - tsd * tsd / 2.0).exp();
        // 6. Ab offset.
        let offset = spec.ab_offset_group.map(|group| {
            let stats = group.stats();
            let drawn = stats.mean_deg + stats.sd_deg * rng.normal();
            (
                drawn,
                AbOffset {
                    position_m: length / 2.0,
                    offset_deg: drawn,
                    uncertainty_deg: stats.ci95_deg,
                    method: AbOffsetMethod::XrdRockingCurve,
                    evidence_class: EvidenceClass::Synthetic,
                },
            )
        });
        let reel_id = format!("{}-R{:05}", spec.inventory_id, index);
        truth.push(TruthReel {
            reel_id: reel_id.clone(),
            lot_factor: lot,
            transfer_factor: transfer,
            ab_offset_deg: offset.as_ref().map(|(d, _)| *d),
        });
        passports.push(ReelPassport {
            schema: REEL_PASSPORT_SCHEMA.into(),
            reel_id,
            evidence_class: EvidenceClass::Synthetic,
            product: ReelProduct {
                vendor: spec.product.vendor.clone(),
                product: spec.product.product.clone(),
                batch: None,
                product_map: Some(spec.product.product_map.clone()),
            },
            geometry: ReelGeometry {
                length_m: length,
                width_m: spec.width_m,
            },
            length_profiles: vec![LengthProfile {
                id: "synthetic-profile".into(),
                method: "synthetic generator (optcoil-synthetic-inventory-spec/v1)".into(),
                evidence_class: EvidenceClass::Synthetic,
                temperature_k: p.temperature_k,
                field_t: p.field_t,
                angle_from_normal_deg: p.angle_from_normal_deg,
                electric_field_criterion_v_per_m: p.electric_field_criterion_v_per_m,
                resolution_m: r,
                source_sha256: None,
                map_reference: Some(MapReference {
                    source_row: p.map_reference_row,
                }),
                points,
            }],
            in_field_points: vec![],
            ab_offsets: offset.into_iter().map(|(_, o)| o).collect(),
            defects: vec![],
            provenance: ReelProvenance {
                issuer: "optcoil synthetic inventory generator".into(),
                issued_at: "1970-01-01T00:00:00Z".into(),
                sources: vec![],
            },
            limitations: vec![
                "Synthetic reel: every value was drawn by the generator and none is a measurement of a real reel.".into(),
            ],
        });
    }
    let inventory = ReelInventory {
        schema: REEL_INVENTORY_SCHEMA.into(),
        inventory_id: spec.inventory_id.clone(),
        evidence_class: EvidenceClass::Synthetic,
        passports,
        limitations: Vec::new(),
    };
    (inventory, truth)
}

fn inventory_limitations(spec: &SyntheticSpec, spec_sha256: &str, nominal_a: f64) -> Vec<String> {
    let p = spec.profile;
    let mut l = vec![
        format!(
            "Synthetic inventory generated by optcoil-synthetic-inventory-spec/v1 (seed {}, spec sha256 {spec_sha256}). Every passport, section and this inventory are synthetic; no reel is a measurement. issued_at is the fixed placeholder 1970-01-01T00:00:00Z so the output is deterministic.",
            spec.seed
        ),
        format!(
            "Nominal reel level: row {} of product map {} (csv_sha256 {}), times width {} m = {nominal_a} A (derived from the map).",
            p.map_reference_row,
            spec.product.product_map.dataset_id,
            spec.product.product_map.csv_sha256,
            spec.width_m
        ),
        match spec.lot_relative_sd {
            None => format!(
                "Lot-to-lot relative SD {DEFAULT_LOT_RELATIVE_SD} (derived from Molodyk et al. 2021, Methods: mean 175 A per 4 mm over about 300 km)."
            ),
            Some(v) => format!(
                "Lot-to-lot relative SD {v} (user-supplied; the derived default is {DEFAULT_LOT_RELATIVE_SD}, Molodyk et al. 2021, Methods)."
            ),
        },
        format!(
            "Along-length relative SD {} (user-supplied; published range for some manufacturers {}-{}, Jaroszynski et al. 2025, section 4.3).",
            spec.length_relative_sd, PUBLISHED_LENGTH_SD.0, PUBLISHED_LENGTH_SD.1
        ),
        format!(
            "Along-length correlation length {} m (user-supplied; there is no published value).",
            spec.length_correlation_m
        ),
        match spec.transfer_log_sd {
            None => format!(
                "Hidden transfer factor log-SD {DEFAULT_TRANSFER_LOG_SD} (published: about 15% standard deviation of the 20 K, 20 T to 77 K self-field Ic ratio over 200 production samples of one product, Molodyk et al. 2021, Fig. 4a)."
            ),
            Some(v) => format!(
                "Hidden transfer factor log-SD {v} (user-supplied; the published value is {DEFAULT_TRANSFER_LOG_SD}, Molodyk et al. 2021, Fig. 4a)."
            ),
        },
    ];
    if !(PUBLISHED_LENGTH_SD.0..=PUBLISHED_LENGTH_SD.1).contains(&spec.length_relative_sd) {
        l.push(format!(
            "length_relative_sd {} is outside the published {}-{} range (Jaroszynski et al. 2025, section 4.3).",
            spec.length_relative_sd, PUBLISHED_LENGTH_SD.0, PUBLISHED_LENGTH_SD.1
        ));
    }
    l.push(match spec.ab_offset_group {
        Some(group) => {
            let s = group.stats();
            format!(
                "ab-plane offsets: one per reel at mid-length, drawn from group {} with mean {} deg and SD {} deg, uncertainty {} deg (95% CI) (published, Cheng et al. 2025, Table 2).",
                group.name(),
                s.mean_deg,
                s.sd_deg,
                s.ci95_deg
            )
        }
        None => "No ab_offsets are generated (no ab_offset_group set).".into(),
    });
    l.push(format!(
        "These statistics were reported for 77 K measurements and are applied here at the declared profile condition ({} K, {} T, {} degrees from the tape normal), which is an assumption.",
        p.temperature_k, p.field_t, p.angle_from_normal_deg
    ));
    l.push("No defects are generated, because no defect-rate statistics are sourced.".into());
    l
}

/// Generates an inventory and its truth file from a spec document.
/// `bundle_jsons` supply product maps that are not embedded; the passport
/// identity contract applies (dataset id and csv_sha256 must match).
/// Returns `(inventory_json, truth_json)`. The inventory is serialized
/// deterministically (pretty JSON, trailing newline) and the truth file
/// binds the SHA-256 of exactly those bytes.
pub fn synthesize_inventory_json(
    spec_json: &str,
    bundle_jsons: &[&str],
) -> Result<(String, String), RunError> {
    let spec = SyntheticSpec::from_json(spec_json)?;
    let mut supplied: BTreeMap<String, MaterialDataset> = BTreeMap::new();
    for bundle in bundle_jsons {
        let dataset = MaterialDataset::from_bundle_json(bundle)?;
        let id = dataset.metadata.id.clone();
        if supplied.insert(id.clone(), dataset).is_some() {
            return Err(RunError::Invalid(format!(
                "duplicate supplied dataset id '{id}'"
            )));
        }
    }
    let map_ref: &ProductMapRef = &spec.product.product_map;
    let model = match resolve_map(&map_ref.dataset_id, &map_ref.csv_sha256, &supplied) {
        Resolved::Model(model) => model,
        Resolved::Unavailable(reason) => {
            return Err(RunError::Invalid(format!(
                "the product map cannot be resolved: {reason}"
            )));
        }
    };
    let p = spec.profile;
    let row_ic = model
        .check_reference(
            p.map_reference_row,
            p.temperature_k,
            p.field_t,
            p.angle_from_normal_deg,
            p.electric_field_criterion_v_per_m,
        )
        .map_err(|why| RunError::Invalid(format!("profile.map_reference_row rejected: {why}")))?;
    let nominal_a = row_ic * spec.width_m;
    let spec_sha256 = passport_sha256(spec_json.as_bytes());

    let (mut inventory, truth_reels) = generate(&spec, nominal_a);
    inventory.limitations = inventory_limitations(&spec, &spec_sha256, nominal_a);
    for passport in &mut inventory.passports {
        passport.provenance.sources.push(ProvenanceSource {
            description: "synthetic inventory generator specification".into(),
            sha256: spec_sha256.clone(),
        });
    }
    inventory.validate()?;
    let mut inventory_json = serde_json::to_string_pretty(&inventory)?;
    inventory_json.push('\n');

    let truth = SyntheticTruth {
        schema: SYNTHETIC_TRUTH_SCHEMA.into(),
        inventory_id: spec.inventory_id.clone(),
        inventory_sha256: passport_sha256(inventory_json.as_bytes()),
        spec_sha256,
        seed: spec.seed,
        reels: truth_reels,
        limitations: vec![
            "Synthetic truth: the drawn lot factors, hidden transfer factors and ab offsets of a generated inventory.".into(),
            "A synthetic reel's true operating-point Ic at x is its scaled rating at x times its transfer factor.".into(),
            "This file is never read by the rating, so a rating cannot see it.".into(),
        ],
    };
    let mut truth_json = serde_json::to_string_pretty(&truth)?;
    truth_json.push('\n');
    Ok((inventory_json, truth_json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reel::{OperatingPoint, rate_inventory_json, validate_inventory_json};
    use serde_json::{Value, json};

    const MAP_ID: &str = "robinson-superpower-ap-v3";
    const ROW_1817_A_PER_M: f64 = 293_111.0;

    fn spec_value() -> Value {
        let hash = MaterialDataset::embedded_by_id(MAP_ID)
            .unwrap()
            .metadata
            .csv_sha256;
        json!({
            "schema": "optcoil-synthetic-inventory-spec/v1",
            "inventory_id": "SYN",
            "seed": 42,
            "product": {"vendor": "Illustrative Vendor", "product": "Illustrative Tape",
                        "product_map": {"dataset_id": MAP_ID, "csv_sha256": hash}},
            "reel_count": 5,
            "reel_length_m": {"min": 100.0, "max": 200.0},
            "width_m": 0.012,
            "profile": {"temperature_k": 20.0, "field_t": 1.0, "angle_from_normal_deg": 0.0,
                        "electric_field_criterion_v_per_m": 0.0001, "resolution_m": 5.0,
                        "map_reference_row": 1817},
            "length_relative_sd": 0.025,
            "length_correlation_m": 5.0,
            "ab_offset_group": "cheng2025_HX"
        })
    }

    fn run(spec: &Value) -> (String, String) {
        synthesize_inventory_json(&spec.to_string(), &[]).unwrap()
    }

    #[test]
    fn splitmix64_matches_the_published_reference_for_seed_zero() {
        let mut rng = SplitMix64::new(0);
        assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(rng.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn normals_consume_exactly_two_draws_and_zero_uniform_is_guarded() {
        let mut a = SplitMix64::new(7);
        let mut b = SplitMix64::new(7);
        a.normal();
        b.next_u64();
        b.next_u64();
        assert_eq!(a.next_u64(), b.next_u64());
        // A first draw of exactly 0 (state + constant wraps to 0) must not
        // produce an infinite normal.
        let start = 0u64.wrapping_sub(0x9E37_79B9_7F4A_7C15);
        assert_eq!(SplitMix64::new(start).next_u64(), 0);
        assert!(SplitMix64::new(start).normal().is_finite());
    }

    #[test]
    fn same_seed_is_byte_identical_and_a_different_seed_differs() {
        let spec = spec_value();
        let (inv1, truth1) = run(&spec);
        let (inv2, truth2) = run(&spec);
        assert_eq!(inv1, inv2);
        assert_eq!(truth1, truth2);
        assert!(inv1.ends_with("}\n"));
        let mut other = spec.clone();
        other["seed"] = json!(43);
        let (inv3, truth3) = run(&other);
        assert_ne!(inv1, inv3);
        assert_ne!(truth1, truth3);
    }

    #[test]
    fn output_is_synthetic_valid_ratable_and_the_truth_binds_the_bytes() {
        let spec = spec_value();
        let (inventory_json, truth_json) = run(&spec);
        let validation = validate_inventory_json(&inventory_json).unwrap();
        assert_eq!(validation.summary.reel_count, 5);
        assert_eq!(validation.summary.evidence_class_counts.synthetic, 5);
        let inventory: Value = serde_json::from_str(&inventory_json).unwrap();
        assert_eq!(inventory["evidence_class"], "synthetic");
        for passport in inventory["passports"].as_array().unwrap() {
            assert_eq!(passport["evidence_class"], "synthetic");
            let profile = &passport["length_profiles"][0];
            assert_eq!(profile["evidence_class"], "synthetic");
            assert_eq!(profile["map_reference"]["source_row"], 1817);
            assert_eq!(passport["ab_offsets"][0]["evidence_class"], "synthetic");
            assert_eq!(passport["ab_offsets"][0]["uncertainty_deg"], 0.94);
            assert_eq!(passport["ab_offsets"][0]["method"], "xrd_rocking_curve");
            assert_eq!(passport["defects"].as_array().unwrap().len(), 0);
            let length = passport["geometry"]["length_m"].as_f64().unwrap();
            assert!((100.0..=200.0).contains(&length));
            let points = profile["points"].as_array().unwrap();
            assert_eq!(points[0][0], 0.0);
            assert_eq!(points.last().unwrap()[0].as_f64().unwrap(), length);
            assert_eq!(
                passport["ab_offsets"][0]["position_m"].as_f64().unwrap(),
                length / 2.0
            );
        }
        let ids: Vec<&str> = inventory["passports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["reel_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids[0], "SYN-R00000");
        assert_eq!(ids[4], "SYN-R00004");

        let truth: Value = serde_json::from_str(&truth_json).unwrap();
        assert_eq!(
            truth["inventory_sha256"].as_str().unwrap(),
            passport_sha256(inventory_json.as_bytes())
        );
        assert_eq!(truth["reels"].as_array().unwrap().len(), 5);
        assert!(truth["reels"][0]["transfer_factor"].as_f64().unwrap() > 0.0);
        assert!(truth["reels"][0]["ab_offset_deg"].is_number());
        // Not leaked into the inventory.
        assert!(!inventory_json.contains("transfer_factor"));

        let limits = inventory["limitations"].to_string();
        for needle in [
            "derived",
            "published",
            "user-supplied",
            "Molodyk",
            "Cheng et al. 2025, Table 2",
            "77 K measurements",
            "No defects are generated",
        ] {
            assert!(limits.contains(needle), "missing {needle}");
        }
        assert!(!limits.contains("outside the published"));

        let record = rate_inventory_json(
            &inventory_json,
            OperatingPoint {
                temperature_k: 25.0,
                field_t: 2.0,
                angle_from_normal_deg: 0.0,
                electric_field_criterion_v_per_m: 1e-4,
            },
            &[],
        )
        .unwrap();
        assert_eq!(record.status_counts["rated"], 5);
        assert_eq!(record.rated_evidence_class_counts.synthetic, 5);
    }

    #[test]
    fn overrides_and_out_of_range_length_sd_are_recorded() {
        let mut spec = spec_value();
        spec["lot_relative_sd"] = json!(0.2);
        spec["transfer_log_sd"] = json!(0.1);
        spec["length_relative_sd"] = json!(0.05);
        spec.as_object_mut().unwrap().remove("ab_offset_group");
        let (inventory_json, _) = run(&spec);
        let limits: Value = serde_json::from_str(&inventory_json).unwrap();
        let text = limits["limitations"].to_string();
        assert!(text.contains("Lot-to-lot relative SD 0.2 (user-supplied"));
        assert!(text.contains("Hidden transfer factor log-SD 0.1 (user-supplied"));
        assert!(text.contains("outside the published 0.02-0.03 range"));
        assert!(text.contains("No ab_offsets are generated"));
        assert!(
            limits["passports"][0]["ab_offsets"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_specs_are_rejected() {
        let base = spec_value();
        type Mutation = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("schema", Box::new(|v| v["schema"] = json!("x"))),
            ("empty id", Box::new(|v| v["inventory_id"] = json!(""))),
            ("zero reels", Box::new(|v| v["reel_count"] = json!(0))),
            (
                "too many reels",
                Box::new(|v| v["reel_count"] = json!(10_001)),
            ),
            (
                "min > max",
                Box::new(|v| v["reel_length_m"]["min"] = json!(300.0)),
            ),
            (
                "min zero",
                Box::new(|v| v["reel_length_m"]["min"] = json!(0.0)),
            ),
            ("width", Box::new(|v| v["width_m"] = json!(0.0))),
            (
                "resolution",
                Box::new(|v| v["profile"]["resolution_m"] = json!(0.0)),
            ),
            (
                "correlation",
                Box::new(|v| v["length_correlation_m"] = json!(0.0)),
            ),
            (
                "negative sd",
                Box::new(|v| v["length_relative_sd"] = json!(-0.1)),
            ),
            (
                "row zero",
                Box::new(|v| v["profile"]["map_reference_row"] = json!(0)),
            ),
            (
                "unknown group",
                Box::new(|v| v["ab_offset_group"] = json!("cheng2025_X")),
            ),
            (
                "too many points",
                Box::new(|v| {
                    v["reel_count"] = json!(10_000);
                    v["profile"]["resolution_m"] = json!(1.0);
                }),
            ),
            (
                "missing length_relative_sd",
                Box::new(|v| {
                    v.as_object_mut().unwrap().remove("length_relative_sd");
                }),
            ),
            (
                "missing length_correlation_m",
                Box::new(|v| {
                    v.as_object_mut().unwrap().remove("length_correlation_m");
                }),
            ),
            (
                "missing product_map",
                Box::new(|v| {
                    v["product"].as_object_mut().unwrap().remove("product_map");
                }),
            ),
            ("unknown field", Box::new(|v| v["surprise"] = json!(1))),
            ("seed negative", Box::new(|v| v["seed"] = json!(-1))),
        ];
        for (name, mutate) in cases {
            let mut spec = base.clone();
            mutate(&mut spec);
            assert!(
                synthesize_inventory_json(&spec.to_string(), &[]).is_err(),
                "not rejected: {name}"
            );
        }
        let mut max = base.clone();
        max["seed"] = json!(u64::MAX);
        assert!(synthesize_inventory_json(&max.to_string(), &[]).is_ok());
    }

    #[test]
    fn product_map_must_resolve_and_the_row_must_match() {
        let mut spec = spec_value();
        spec["product"]["product_map"]["csv_sha256"] = json!("0".repeat(64));
        assert!(synthesize_inventory_json(&spec.to_string(), &[]).is_err());
        let mut spec = spec_value();
        spec["product"]["product_map"]["dataset_id"] = json!("not-a-dataset");
        assert!(synthesize_inventory_json(&spec.to_string(), &[]).is_err());
        for (path, value) in [
            ("temperature_k", json!(25.0)),
            ("field_t", json!(2.0)),
            ("angle_from_normal_deg", json!(30.0)),
            ("electric_field_criterion_v_per_m", json!(0.001)),
            ("map_reference_row", json!(999_999)),
        ] {
            let mut spec = spec_value();
            spec["profile"][path] = value;
            let error = synthesize_inventory_json(&spec.to_string(), &[]).unwrap_err();
            assert!(
                error.to_string().contains("map_reference"),
                "{path}: {error}"
            );
        }
        // A supplied bundle under the same identity works; a duplicate does not.
        let bundle = MaterialDataset::embedded_bundle_json(MAP_ID).unwrap();
        assert!(synthesize_inventory_json(&spec_value().to_string(), &[bundle.as_str()]).is_ok());
        assert!(
            synthesize_inventory_json(
                &spec_value().to_string(),
                &[bundle.as_str(), bundle.as_str()]
            )
            .is_err()
        );
    }

    fn mean(v: &[f64]) -> f64 {
        v.iter().sum::<f64>() / v.len() as f64
    }

    fn sd(v: &[f64]) -> f64 {
        let m = mean(v);
        (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
    }

    #[test]
    fn statistics_match_the_specification_on_a_large_set() {
        let mut value = spec_value();
        value["seed"] = json!(20_261_004u64);
        value["reel_count"] = json!(10_000);
        value["reel_length_m"] = json!({"min": 20.0, "max": 40.0});
        value["profile"]["resolution_m"] = json!(1.0);
        let spec = SyntheticSpec::from_json(&value.to_string()).unwrap();
        let nominal = ROW_1817_A_PER_M * spec.width_m;
        let (inventory, truth) = generate(&spec, nominal);

        let lots: Vec<f64> = truth.iter().map(|t| t.lot_factor).collect();
        let transfers: Vec<f64> = truth.iter().map(|t| t.transfer_factor).collect();
        let logs: Vec<f64> = transfers.iter().map(|t| t.ln()).collect();
        let offsets: Vec<f64> = truth.iter().filter_map(|t| t.ab_offset_deg).collect();
        let (lot_mean, lot_sd) = (mean(&lots), sd(&lots));
        let (tr_mean, tr_log_sd) = (mean(&transfers), sd(&logs));
        let (ab_mean, ab_sd) = (mean(&offsets), sd(&offsets));
        assert!((lot_mean - 1.0).abs() < 0.01, "lot mean {lot_mean}");
        assert!((lot_sd - 0.11).abs() < 0.01, "lot sd {lot_sd}");
        assert!((tr_mean - 1.0).abs() < 0.01, "transfer mean {tr_mean}");
        assert!(
            (tr_log_sd - 0.15).abs() < 0.01,
            "transfer log sd {tr_log_sd}"
        );
        assert!((ab_mean + 1.09).abs() < 0.02, "ab mean {ab_mean}");
        assert!((ab_sd - 0.58).abs() < 0.02, "ab sd {ab_sd}");

        let mut eps = Vec::new();
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for (passport, t) in inventory.passports.iter().zip(&truth) {
            let pts = &passport.length_profiles[0].points;
            let e: Vec<f64> = pts
                .iter()
                .map(|[_, ic]| ic / (nominal * t.lot_factor) - 1.0)
                .collect();
            for i in 1..pts.len() {
                // Only unit-spacing steps; the appended end point differs.
                if pts[i][0] - pts[i - 1][0] == 1.0 {
                    xs.push(e[i - 1]);
                    ys.push(e[i]);
                }
            }
            eps.extend(e);
        }
        let eps_sd = sd(&eps);
        assert!((eps_sd - 0.025).abs() < 0.001, "AR(1) marginal sd {eps_sd}");
        let (mx, my) = (mean(&xs), mean(&ys));
        let cov: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        let vx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        let vy: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
        let lag1 = cov / (vx * vy).sqrt();
        let rho = (-1.0f64 / 5.0).exp();
        assert!((lag1 - rho).abs() < 0.01, "lag-1 {lag1} vs {rho}");
        println!(
            "stats: lot mean {lot_mean:.4} sd {lot_sd:.4}; transfer mean {tr_mean:.4} log-sd {tr_log_sd:.4}; ab mean {ab_mean:.3} sd {ab_sd:.3}; eps sd {eps_sd:.5}; lag1 {lag1:.4} (rho {rho:.4})"
        );
    }
}
