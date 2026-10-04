//! Reel passports and per-reel inventories (CR-02). A passport records what
//! is known about one physical reel of tape and where that knowledge came
//! from; an inventory is a set of passports. Rating a reel against a product
//! map lives in `optcoil-search::reel`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{ModelError, attestation::sha256_hex};

pub const REEL_PASSPORT_SCHEMA: &str = "optcoil-reel-passport/v1";
pub const REEL_INVENTORY_SCHEMA: &str = "optcoil-reel-inventory/v1";

const MAX_PASSPORT_BYTES: usize = 64 * 1024 * 1024;
const MAX_PASSPORTS: usize = 10_000;
const MAX_ITEMS: usize = 100_000;
const MAX_PROFILE_POINTS: usize = 1_000_000;

/// Ordered by strength: `Synthetic < ModelInformed < Measured`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Synthetic,
    ModelInformed,
    Measured,
}

impl EvidenceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::ModelInformed => "model_informed",
            Self::Measured => "measured",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProductMapRef {
    pub dataset_id: String,
    pub csv_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelProduct {
    pub vendor: String,
    pub product: String,
    pub batch: Option<String>,
    pub product_map: Option<ProductMapRef>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelGeometry {
    pub length_m: f64,
    pub width_m: f64,
}

/// The issuer's declaration of which product-map row represents a profile's
/// condition. Needed where the interpolator cannot evaluate the condition
/// (for example 77 K self-field scans). Checked against the map at rating
/// time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MapReference {
    pub source_row: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthProfile {
    pub id: String,
    /// Free text, e.g. "reel-to-reel transport".
    pub method: String,
    pub evidence_class: EvidenceClass,
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub electric_field_criterion_v_per_m: f64,
    pub resolution_m: f64,
    pub source_sha256: Option<String>,
    #[serde(default)]
    pub map_reference: Option<MapReference>,
    /// `[position_m, ic_a]`, whole-tape current as measured.
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InFieldPoint {
    pub sample_id: String,
    pub position_m: f64,
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub ic_a: f64,
    pub n_value: Option<f64>,
    pub electric_field_criterion_v_per_m: f64,
    pub lab: String,
    pub evidence_class: EvidenceClass,
    pub source_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AbOffsetMethod {
    XrdRockingCurve,
    Transport,
    Torque,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbOffset {
    pub position_m: f64,
    pub offset_deg: f64,
    pub uncertainty_deg: f64,
    pub method: AbOffsetMethod,
    pub evidence_class: EvidenceClass,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefectKind {
    Dropout,
    Delamination,
    Mechanical,
    Splice,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefectAction {
    Excluded,
    Cut,
    Accepted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defect {
    pub start_m: f64,
    pub end_m: f64,
    pub kind: DefectKind,
    pub action: DefectAction,
    pub min_ic_fraction: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSource {
    pub description: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelProvenance {
    pub issuer: String,
    /// RFC 3339 date or date-time.
    pub issued_at: String,
    pub sources: Vec<ProvenanceSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelPassport {
    pub schema: String,
    pub reel_id: String,
    pub evidence_class: EvidenceClass,
    pub product: ReelProduct,
    pub geometry: ReelGeometry,
    pub length_profiles: Vec<LengthProfile>,
    pub in_field_points: Vec<InFieldPoint>,
    pub ab_offsets: Vec<AbOffset>,
    pub defects: Vec<Defect>,
    pub provenance: ReelProvenance,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelInventory {
    pub schema: String,
    pub inventory_id: String,
    pub evidence_class: EvidenceClass,
    pub passports: Vec<ReelPassport>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProductLength {
    pub vendor: String,
    pub product: String,
    pub reel_count: usize,
    pub length_m: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceClassCounts {
    pub measured: usize,
    pub model_informed: usize,
    pub synthetic: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InventorySummary {
    pub reel_count: usize,
    pub total_length_m: f64,
    /// Total length minus the union of `excluded` and `cut` defect spans.
    pub usable_length_m: f64,
    /// Grouped by vendor and product; batches are not distinguished.
    pub length_by_product: Vec<ProductLength>,
    pub evidence_class_counts: EvidenceClassCounts,
}

/// SHA-256 of the passport file's exact bytes.
pub fn passport_sha256(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

impl ReelPassport {
    /// Parses and validates a passport document.
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        if json.len() > MAX_PASSPORT_BYTES {
            return Err(invalid("reel passport exceeds 64 MiB"));
        }
        let passport: Self = serde_json::from_str(json)?;
        passport.validate()?;
        Ok(passport)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == REEL_PASSPORT_SCHEMA,
            &format!("unsupported reel passport schema (expected {REEL_PASSPORT_SCHEMA})"),
        )?;
        let id = &self.reel_id;
        let ctx = |msg: &str| format!("reel '{id}': {msg}");
        require(!self.reel_id.trim().is_empty(), "reel_id must be non-empty")?;
        require(
            !self.product.vendor.trim().is_empty() && !self.product.product.trim().is_empty(),
            &ctx("product vendor and product must be non-empty"),
        )?;
        if let Some(map) = &self.product.product_map {
            require(
                !map.dataset_id.trim().is_empty() && valid_sha256(&map.csv_sha256),
                &ctx("product_map needs a dataset_id and a lowercase hex csv_sha256"),
            )?;
        }
        let length = self.geometry.length_m;
        let width = self.geometry.width_m;
        positive(length, &ctx("geometry.length_m"))?;
        positive(width, &ctx("geometry.width_m"))?;
        require(
            self.length_profiles.len() <= MAX_ITEMS
                && self.in_field_points.len() <= MAX_ITEMS
                && self.ab_offsets.len() <= MAX_ITEMS
                && self.defects.len() <= MAX_ITEMS
                && self.limitations.len() <= MAX_ITEMS,
            &ctx("too many list entries"),
        )?;

        let mut profile_ids = HashSet::new();
        for profile in &self.length_profiles {
            require(
                !profile.id.trim().is_empty() && profile_ids.insert(profile.id.as_str()),
                &ctx("empty or duplicate length profile id"),
            )?;
            let pctx = |msg: &str| format!("reel '{id}' profile '{}': {msg}", profile.id);
            require(!profile.method.trim().is_empty(), &pctx("empty method"))?;
            positive(profile.temperature_k, &pctx("temperature_k"))?;
            nonnegative(profile.field_t, &pctx("field_t"))?;
            finite(
                profile.angle_from_normal_deg,
                &pctx("angle_from_normal_deg"),
            )?;
            positive(
                profile.electric_field_criterion_v_per_m,
                &pctx("electric_field_criterion_v_per_m"),
            )?;
            positive(profile.resolution_m, &pctx("resolution_m"))?;
            optional_sha256(&profile.source_sha256, &pctx("source_sha256"))?;
            if let Some(reference) = &profile.map_reference {
                require(
                    reference.source_row >= 1,
                    &pctx("map_reference source_row must be at least 1"),
                )?;
            }
            require(
                !profile.points.is_empty() && profile.points.len() <= MAX_PROFILE_POINTS,
                &pctx("needs 1..=1000000 points"),
            )?;
            let mut previous = f64::NEG_INFINITY;
            for [position, ic] in &profile.points {
                require(
                    position.is_finite()
                        && *position >= 0.0
                        && *position <= length
                        && *position > previous,
                    &pctx("positions must strictly increase within [0, length_m]"),
                )?;
                require(
                    ic.is_finite() && *ic >= 0.0,
                    &pctx("ic_a must be finite and nonnegative"),
                )?;
                previous = *position;
            }
        }

        let mut sample_ids = HashSet::new();
        for point in &self.in_field_points {
            require(
                !point.sample_id.trim().is_empty() && sample_ids.insert(point.sample_id.as_str()),
                &ctx("empty or duplicate in-field sample_id"),
            )?;
            let pctx = |msg: &str| format!("reel '{id}' sample '{}': {msg}", point.sample_id);
            require(
                point.position_m.is_finite()
                    && point.position_m >= 0.0
                    && point.position_m <= length,
                &pctx("position_m outside [0, length_m]"),
            )?;
            positive(point.temperature_k, &pctx("temperature_k"))?;
            nonnegative(point.field_t, &pctx("field_t"))?;
            finite(point.angle_from_normal_deg, &pctx("angle_from_normal_deg"))?;
            nonnegative(point.ic_a, &pctx("ic_a"))?;
            if let Some(n) = point.n_value {
                positive(n, &pctx("n_value"))?;
            }
            positive(
                point.electric_field_criterion_v_per_m,
                &pctx("electric_field_criterion_v_per_m"),
            )?;
            require(!point.lab.trim().is_empty(), &pctx("empty lab"))?;
            optional_sha256(&point.source_sha256, &pctx("source_sha256"))?;
        }

        for offset in &self.ab_offsets {
            let octx = |msg: &str| format!("reel '{id}' ab offset: {msg}");
            require(
                offset.position_m.is_finite()
                    && offset.position_m >= 0.0
                    && offset.position_m <= length,
                &octx("position_m outside [0, length_m]"),
            )?;
            finite(offset.offset_deg, &octx("offset_deg"))?;
            nonnegative(offset.uncertainty_deg, &octx("uncertainty_deg"))?;
        }

        for defect in &self.defects {
            let dctx = |msg: &str| format!("reel '{id}' defect: {msg}");
            require(
                defect.start_m.is_finite()
                    && defect.end_m.is_finite()
                    && defect.start_m >= 0.0
                    && defect.start_m <= defect.end_m
                    && defect.end_m <= length,
                &dctx("span must satisfy 0 <= start_m <= end_m <= length_m"),
            )?;
            if let Some(fraction) = defect.min_ic_fraction {
                require(
                    fraction.is_finite() && (0.0..=1.0).contains(&fraction),
                    &dctx("min_ic_fraction must be within [0, 1]"),
                )?;
            }
        }

        require(
            !self.provenance.issuer.trim().is_empty(),
            &ctx("provenance issuer must be non-empty"),
        )?;
        require(
            valid_rfc3339(&self.provenance.issued_at),
            &ctx("provenance issued_at must be an RFC 3339 date or date-time"),
        )?;
        for source in &self.provenance.sources {
            require(
                valid_sha256(&source.sha256),
                &ctx("provenance source sha256 must be lowercase hex"),
            )?;
        }

        let weakest = self
            .length_profiles
            .iter()
            .map(|p| p.evidence_class)
            .chain(self.in_field_points.iter().map(|p| p.evidence_class))
            .chain(self.ab_offsets.iter().map(|o| o.evidence_class))
            .min();
        if let Some(weakest) = weakest {
            require(
                self.evidence_class <= weakest,
                &ctx(&format!(
                    "evidence_class {} is stronger than its weakest section ({})",
                    self.evidence_class.as_str(),
                    weakest.as_str()
                )),
            )?;
        }
        Ok(())
    }

    /// The union of `excluded` and `cut` spans, sorted and merged. Spans that
    /// overlap or touch become one.
    pub fn unusable_spans(&self) -> Vec<(f64, f64)> {
        let mut spans: Vec<(f64, f64)> = self
            .defects
            .iter()
            .filter(|d| matches!(d.action, DefectAction::Excluded | DefectAction::Cut))
            .map(|d| (d.start_m, d.end_m))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    pub fn usable_length_m(&self) -> f64 {
        let removed: f64 = self.unusable_spans().iter().map(|(s, e)| e - s).sum();
        (self.geometry.length_m - removed).max(0.0)
    }

    /// Whether `position_m` lies outside every excluded or cut span (spans
    /// are closed intervals).
    pub fn is_usable_position(&self, position_m: f64) -> bool {
        !self
            .unusable_spans()
            .iter()
            .any(|(s, e)| position_m >= *s && position_m <= *e)
    }
}

impl ReelInventory {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        if json.len() > MAX_PASSPORT_BYTES {
            return Err(invalid("reel inventory exceeds 64 MiB"));
        }
        let inventory: Self = serde_json::from_str(json)?;
        inventory.validate()?;
        Ok(inventory)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == REEL_INVENTORY_SCHEMA,
            &format!("unsupported reel inventory schema (expected {REEL_INVENTORY_SCHEMA})"),
        )?;
        require(
            !self.inventory_id.trim().is_empty(),
            "inventory_id must be non-empty",
        )?;
        require(
            !self.passports.is_empty() && self.passports.len() <= MAX_PASSPORTS,
            "an inventory needs 1..=10000 passports",
        )?;
        let mut ids = HashSet::new();
        for passport in &self.passports {
            passport.validate()?;
            require(
                ids.insert(passport.reel_id.as_str()),
                &format!("duplicate reel_id '{}' in inventory", passport.reel_id),
            )?;
        }
        let weakest = self.passports.iter().map(|p| p.evidence_class).min();
        if let Some(weakest) = weakest {
            require(
                self.evidence_class <= weakest,
                &format!(
                    "inventory evidence_class {} is stronger than its weakest passport ({})",
                    self.evidence_class.as_str(),
                    weakest.as_str()
                ),
            )?;
        }
        Ok(())
    }

    pub fn summary(&self) -> InventorySummary {
        let mut counts = EvidenceClassCounts::default();
        let mut by_product: BTreeMap<(String, String), (usize, f64)> = BTreeMap::new();
        for passport in &self.passports {
            match passport.evidence_class {
                EvidenceClass::Measured => counts.measured += 1,
                EvidenceClass::ModelInformed => counts.model_informed += 1,
                EvidenceClass::Synthetic => counts.synthetic += 1,
            }
            let entry = by_product
                .entry((
                    passport.product.vendor.clone(),
                    passport.product.product.clone(),
                ))
                .or_default();
            entry.0 += 1;
            entry.1 += passport.geometry.length_m;
        }
        InventorySummary {
            reel_count: self.passports.len(),
            total_length_m: self.passports.iter().map(|p| p.geometry.length_m).sum(),
            usable_length_m: self.passports.iter().map(|p| p.usable_length_m()).sum(),
            length_by_product: by_product
                .into_iter()
                .map(
                    |((vendor, product), (reel_count, length_m))| ProductLength {
                        vendor,
                        product,
                        reel_count,
                        length_m,
                    },
                )
                .collect(),
            evidence_class_counts: counts,
        }
    }
}

/// RFC 3339 `full-date` or `date-time` (`T` or `t` separator, `Z` or numeric
/// offset, optional fraction).
fn valid_rfc3339(text: &str) -> bool {
    let b = text.as_bytes();
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let num = |s: &[u8]| -> u32 { s.iter().fold(0, |a, d| a * 10 + u32::from(d - b'0')) };
    if b.len() < 10 || !digits(&b[0..4]) || b[4] != b'-' || !digits(&b[5..7]) || b[7] != b'-' {
        return false;
    }
    if !digits(&b[8..10]) {
        return false;
    }
    let (year, month, day) = (num(&b[0..4]), num(&b[5..7]), num(&b[8..10]));
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if day < 1 || day > days {
        return false;
    }
    if b.len() == 10 {
        return true;
    }
    if !matches!(b[10], b'T' | b't') || b.len() < 20 {
        return false;
    }
    let t = &b[11..];
    if !(digits(&t[0..2]) && t[2] == b':' && digits(&t[3..5]) && t[5] == b':' && digits(&t[6..8])) {
        return false;
    }
    if num(&t[0..2]) > 23 || num(&t[3..5]) > 59 || num(&t[6..8]) > 60 {
        return false;
    }
    let mut rest = &t[8..];
    if rest.first() == Some(&b'.') {
        let n = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        rest = &rest[1 + n..];
    }
    match rest {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => {
            let hh = [*h1, *h2];
            let mm = [*m1, *m2];
            digits(&hh) && digits(&mm) && num(&hh) <= 23 && num(&mm) <= 59
        }
        _ => false,
    }
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn optional_sha256(hash: &Option<String>, name: &str) -> Result<(), ModelError> {
    match hash {
        Some(h) => require(
            valid_sha256(h),
            &format!("{name} must be lowercase hex SHA-256"),
        ),
        None => Ok(()),
    }
}

fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

fn require(condition: bool, message: &str) -> Result<(), ModelError> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn finite(value: f64, name: &str) -> Result<(), ModelError> {
    require(value.is_finite(), &format!("{name} must be finite"))
}

fn positive(value: f64, name: &str) -> Result<(), ModelError> {
    require(
        value.is_finite() && value > 0.0,
        &format!("{name} must be finite and positive"),
    )
}

fn nonnegative(value: f64, name: &str) -> Result<(), ModelError> {
    require(
        value.is_finite() && value >= 0.0,
        &format!("{name} must be finite and nonnegative"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354";

    pub(crate) fn passport(id: &str) -> ReelPassport {
        ReelPassport {
            schema: REEL_PASSPORT_SCHEMA.into(),
            reel_id: id.into(),
            evidence_class: EvidenceClass::Measured,
            product: ReelProduct {
                vendor: "Illustrative Vendor".into(),
                product: "Illustrative Tape".into(),
                batch: None,
                product_map: Some(ProductMapRef {
                    dataset_id: "robinson-superpower-ap-v3".into(),
                    csv_sha256: HASH.into(),
                }),
            },
            geometry: ReelGeometry {
                length_m: 100.0,
                width_m: 0.012,
            },
            length_profiles: vec![LengthProfile {
                id: "p1".into(),
                method: "reel-to-reel transport".into(),
                evidence_class: EvidenceClass::Measured,
                temperature_k: 77.0,
                field_t: 0.0,
                angle_from_normal_deg: 0.0,
                electric_field_criterion_v_per_m: 1e-4,
                resolution_m: 1.0,
                source_sha256: None,
                map_reference: None,
                points: vec![[0.0, 500.0], [10.0, 480.0], [20.0, 520.0]],
            }],
            in_field_points: vec![],
            ab_offsets: vec![],
            defects: vec![],
            provenance: ReelProvenance {
                issuer: "test".into(),
                issued_at: "2026-10-01".into(),
                sources: vec![],
            },
            limitations: vec![],
        }
    }

    fn defect(start: f64, end: f64, action: DefectAction) -> Defect {
        Defect {
            start_m: start,
            end_m: end,
            kind: DefectKind::Dropout,
            action,
            min_ic_fraction: None,
            note: String::new(),
        }
    }

    fn inventory(passports: Vec<ReelPassport>) -> ReelInventory {
        ReelInventory {
            schema: REEL_INVENTORY_SCHEMA.into(),
            inventory_id: "inv".into(),
            evidence_class: EvidenceClass::Measured,
            passports,
            limitations: vec![],
        }
    }

    #[test]
    fn valid_passport_round_trips_and_rejects_unknown_fields() {
        let p = passport("r1");
        p.validate().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        ReelPassport::from_json(&json).unwrap();
        let bad = json.replacen("\"reel_id\"", "\"reel_idd\"", 1);
        assert!(ReelPassport::from_json(&bad).is_err());
        let extra = json.replacen('{', "{\"surprise\":1,", 1);
        assert!(ReelPassport::from_json(&extra).is_err());
    }

    #[test]
    fn every_validation_rule_rejects() {
        type Mutation = Box<dyn Fn(&mut ReelPassport)>;
        let cases: Vec<(&str, Mutation)> = vec![
            ("schema", Box::new(|p| p.schema = "x".into())),
            ("empty id", Box::new(|p| p.reel_id = " ".into())),
            ("length", Box::new(|p| p.geometry.length_m = 0.0)),
            ("width", Box::new(|p| p.geometry.width_m = f64::NAN)),
            (
                "map hash",
                Box::new(|p| p.product.product_map.as_mut().unwrap().csv_sha256 = "abc".into()),
            ),
            (
                "profile position decreasing",
                Box::new(|p| p.length_profiles[0].points[1][0] = 0.0),
            ),
            (
                "profile position out of range",
                Box::new(|p| p.length_profiles[0].points[2][0] = 100.5),
            ),
            (
                "profile negative ic",
                Box::new(|p| p.length_profiles[0].points[1][1] = -1.0),
            ),
            (
                "profile nonfinite ic",
                Box::new(|p| p.length_profiles[0].points[1][1] = f64::INFINITY),
            ),
            (
                "map_reference row zero",
                Box::new(|p| {
                    p.length_profiles[0].map_reference = Some(MapReference { source_row: 0 })
                }),
            ),
            (
                "profile empty",
                Box::new(|p| p.length_profiles[0].points.clear()),
            ),
            (
                "profile criterion",
                Box::new(|p| p.length_profiles[0].electric_field_criterion_v_per_m = 0.0),
            ),
            (
                "duplicate profile id",
                Box::new(|p| {
                    let q = p.length_profiles[0].clone();
                    p.length_profiles.push(q);
                }),
            ),
            (
                "defect reversed",
                Box::new(|p| p.defects.push(defect(5.0, 4.0, DefectAction::Cut))),
            ),
            (
                "defect out of range",
                Box::new(|p| p.defects.push(defect(90.0, 101.0, DefectAction::Cut))),
            ),
            (
                "defect fraction",
                Box::new(|p| {
                    let mut d = defect(1.0, 2.0, DefectAction::Accepted);
                    d.min_ic_fraction = Some(1.5);
                    p.defects.push(d);
                }),
            ),
            (
                "offset uncertainty",
                Box::new(|p| {
                    p.ab_offsets.push(AbOffset {
                        position_m: 1.0,
                        offset_deg: 1.0,
                        uncertainty_deg: -0.1,
                        method: AbOffsetMethod::Other,
                        evidence_class: EvidenceClass::Measured,
                    })
                }),
            ),
            (
                "offset position",
                Box::new(|p| {
                    p.ab_offsets.push(AbOffset {
                        position_m: 101.0,
                        offset_deg: 1.0,
                        uncertainty_deg: 0.1,
                        method: AbOffsetMethod::Torque,
                        evidence_class: EvidenceClass::Measured,
                    })
                }),
            ),
            (
                "in-field position",
                Box::new(|p| {
                    p.in_field_points.push(InFieldPoint {
                        sample_id: "s".into(),
                        position_m: -1.0,
                        temperature_k: 20.0,
                        field_t: 1.0,
                        angle_from_normal_deg: 0.0,
                        ic_a: 100.0,
                        n_value: None,
                        electric_field_criterion_v_per_m: 1e-4,
                        lab: "lab".into(),
                        evidence_class: EvidenceClass::Measured,
                        source_sha256: None,
                    })
                }),
            ),
            (
                "issued_at",
                Box::new(|p| p.provenance.issued_at = "2026-02-30".into()),
            ),
            (
                "source hash",
                Box::new(|p| {
                    p.provenance.sources.push(ProvenanceSource {
                        description: "d".into(),
                        sha256: "ZZ".into(),
                    })
                }),
            ),
            (
                "synthetic section under measured passport",
                Box::new(|p| p.length_profiles[0].evidence_class = EvidenceClass::Synthetic),
            ),
            (
                "model-informed section under measured passport",
                Box::new(|p| p.length_profiles[0].evidence_class = EvidenceClass::ModelInformed),
            ),
        ];
        for (name, mutate) in cases {
            let mut p = passport("r1");
            mutate(&mut p);
            assert!(p.validate().is_err(), "rule not enforced: {name}");
        }
        let mut weaker = passport("r1");
        weaker.length_profiles[0].evidence_class = EvidenceClass::ModelInformed;
        weaker.evidence_class = EvidenceClass::ModelInformed;
        weaker.validate().unwrap();
        weaker.evidence_class = EvidenceClass::Synthetic;
        weaker.validate().unwrap();
    }

    #[test]
    fn rfc3339_forms() {
        for ok in [
            "2026-10-01",
            "2024-02-29",
            "2026-10-01T12:30:00Z",
            "2026-10-01T12:30:00.25+02:00",
            "2026-10-01t23:59:60-07:00",
        ] {
            assert!(valid_rfc3339(ok), "{ok}");
        }
        for bad in [
            "",
            "2026-10",
            "2025-02-29",
            "2026-13-01",
            "2026-10-01T12:30:00",
            "2026-10-01T24:00:00Z",
            "2026-10-01 12:30:00Z",
            "2026-10-01T12:30:00.Z",
            "2026-10-01T12:30:00+0200",
            "20261001",
        ] {
            assert!(!valid_rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn inventory_rejects_duplicate_ids_and_overstated_class() {
        assert!(
            inventory(vec![passport("a"), passport("a")])
                .validate()
                .is_err()
        );
        inventory(vec![passport("a"), passport("b")])
            .validate()
            .unwrap();
        let mut weak = passport("b");
        weak.evidence_class = EvidenceClass::Synthetic;
        let inv = inventory(vec![passport("a"), weak]);
        assert!(inv.validate().is_err());
        let mut ok = inv;
        ok.evidence_class = EvidenceClass::Synthetic;
        ok.validate().unwrap();
        let mut bad_schema = inventory(vec![passport("a")]);
        bad_schema.schema = "other".into();
        assert!(bad_schema.validate().is_err());
        assert!(inventory(vec![]).validate().is_err());
    }

    #[test]
    fn summary_counts_overlapping_spans_once() {
        let mut a = passport("a");
        a.defects = vec![
            defect(10.0, 20.0, DefectAction::Excluded),
            defect(15.0, 30.0, DefectAction::Cut),
            defect(50.0, 60.0, DefectAction::Accepted),
            defect(90.0, 95.0, DefectAction::Cut),
        ];
        assert_eq!(a.unusable_spans(), vec![(10.0, 30.0), (90.0, 95.0)]);
        assert_eq!(a.usable_length_m(), 75.0);
        assert!(a.is_usable_position(50.0));
        assert!(!a.is_usable_position(30.0));
        let mut b = passport("b");
        b.geometry.length_m = 50.0;
        b.product.product = "Other".into();
        b.evidence_class = EvidenceClass::ModelInformed;
        b.length_profiles[0].points = vec![[0.0, 1.0]];
        b.length_profiles[0].evidence_class = EvidenceClass::ModelInformed;
        let mut inv = inventory(vec![a, b]);
        inv.evidence_class = EvidenceClass::ModelInformed;
        inv.validate().unwrap();
        let s = inv.summary();
        assert_eq!(s.reel_count, 2);
        assert_eq!(s.total_length_m, 150.0);
        assert_eq!(s.usable_length_m, 125.0);
        assert_eq!(s.length_by_product.len(), 2);
        assert_eq!(s.length_by_product[0].product, "Illustrative Tape");
        assert_eq!(s.length_by_product[0].length_m, 100.0);
        assert_eq!(s.length_by_product[1].product, "Other");
        assert_eq!(
            s.evidence_class_counts,
            EvidenceClassCounts {
                measured: 1,
                model_informed: 1,
                synthetic: 0
            }
        );
    }

    #[test]
    fn passport_hash_is_over_exact_bytes() {
        assert_eq!(
            passport_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(passport_sha256(b"abc"), passport_sha256(b"abc\n"));
    }
}
