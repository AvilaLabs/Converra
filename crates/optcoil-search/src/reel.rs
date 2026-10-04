//! Per-reel rating of an inventory at one operating point (CR-02). Each reel
//! is rated from its passport and the product's critical-current map; the
//! map's measured domain bounds every rating and nothing is extrapolated.

use std::{collections::BTreeMap, path::Path};

use optcoil_model::{
    material::{MaterialDataClass, MaterialDataset, MaterialQuery},
    reel::{
        AbOffset, DefectAction, DefectKind, EvidenceClass, EvidenceClassCounts, InventorySummary,
        LengthProfile, ProductMapRef, ReelInventory, ReelPassport, passport_sha256,
    },
};
use optcoil_physics::critical_current::{IcInterpolationMethod, IcInterpolator, LOG_IC_MODEL_ID};
use serde::{Deserialize, Serialize};

use crate::{RunError, hash, write_json_new};

pub const REEL_RATING_SCHEMA: &str = "optcoil-reel-rating/v1";

/// Half-width of the window around the tape plane (90 degrees from the tape
/// normal) in which ab-plane offsets matter.
pub(crate) const ORIENTATION_WINDOW_DEG: f64 = 15.0;
const CONSISTENCY_TEMPERATURE_K: f64 = 0.5;
const CONSISTENCY_FIELD_FRACTION: f64 = 0.02;
const CONSISTENCY_ANGLE_DEG: f64 = 2.5;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OperatingPoint {
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub electric_field_criterion_v_per_m: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReelRatingStatus {
    NoProductMap,
    MapUnavailable,
    OutsideMapDomain,
    OrientationUnresolved,
    RatedProductLevel,
    ProfileUnscalable,
    Rated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatingDatasetIdentity {
    pub dataset_id: String,
    pub csv_sha256: String,
    /// `embedded` or `supplied`.
    pub source: String,
    pub data_class: MaterialDataClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrientationEvaluation {
    /// |angle - 90| < 15 degrees: the operating angle is near the tape plane.
    pub in_tape_plane_window: bool,
    /// Largest |offset| + uncertainty on the reel; present only when offsets
    /// were applied.
    pub offset_bound_deg: Option<f64>,
    /// Angles at which the map was evaluated; the smaller value is used.
    pub angles_evaluated_deg: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileCondition {
    pub profile_id: String,
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub electric_field_criterion_v_per_m: f64,
    pub map_ic_a_per_m: f64,
    pub map_ic_a: f64,
    /// The `map_reference` row that supplied the map value, if any.
    pub map_reference_row: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedDefectRating {
    pub start_m: f64,
    pub end_m: f64,
    pub kind: DefectKind,
    pub min_ic_fraction: Option<f64>,
    pub note: String,
    pub profile_points_in_span: usize,
    /// None when no profile point falls inside the span.
    pub min_ic_a: Option<f64>,
    pub min_position_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaledRating {
    pub profile: ProfileCondition,
    pub profile_point_count: usize,
    pub usable_point_count: usize,
    pub min_ic_a: f64,
    pub p5_ic_a: f64,
    pub median_ic_a: f64,
    pub min_position_m: f64,
    pub accepted_defects: Vec<AcceptedDefectRating>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyCheck {
    pub sample_id: String,
    pub position_m: f64,
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub measured_ic_a: f64,
    /// The scaled rating interpolated linearly along the profile; None when
    /// the point lies outside the profile's positions (no extrapolation).
    pub scaled_rating_ic_a: Option<f64>,
    /// (measured - scaled) / scaled; None when no comparison is valid.
    pub relative_difference: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReelRating {
    pub reel_id: String,
    pub status: ReelRatingStatus,
    /// None when no rating is given.
    pub evidence_class: Option<EvidenceClass>,
    pub passport_evidence_class: EvidenceClass,
    pub basis: String,
    pub explanation: String,
    /// What evidence would allow a rating; present for every status other
    /// than `rated`.
    pub next_evidence: Option<String>,
    pub width_m: f64,
    /// Map Ic per width at the operating point (the smaller of the two
    /// offset-adjusted evaluations in the tape-plane window).
    pub map_ic_a_per_m: Option<f64>,
    pub orientation: Option<OrientationEvaluation>,
    /// Map value times reel width; set for `rated_product_level`.
    pub product_level_ic_a: Option<f64>,
    pub scaled: Option<ScaledRating>,
    pub consistency_checks: Vec<ConsistencyCheck>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReelRatingRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub inventory_id: String,
    pub inventory_sha256: String,
    pub inventory_evidence_class: EvidenceClass,
    pub operating_point: OperatingPoint,
    pub datasets: Vec<RatingDatasetIdentity>,
    pub summary: InventorySummary,
    pub status_counts: BTreeMap<String, usize>,
    pub rated_evidence_class_counts: EvidenceClassCounts,
    pub reels: Vec<ReelRating>,
    pub limitations: Vec<String>,
}

impl ReelRatingRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

/// What a `map_reference` is checked against: a map row's nominal
/// coordinates and its Ic.
struct MapRow {
    nominal: [f64; 3],
    ic_a_per_m: f64,
}

pub(crate) struct MapModel {
    identity: RatingDatasetIdentity,
    rows: BTreeMap<u32, MapRow>,
    criterion: f64,
    interpolator: IcInterpolator,
}

impl MapModel {
    /// Map Ic per width, or None outside the measured domain or when the
    /// criterion differs from the dataset's.
    fn value(
        &self,
        temperature_k: f64,
        field_t: f64,
        angle_deg: f64,
        criterion: f64,
    ) -> Result<Option<f64>, RunError> {
        if (criterion / self.criterion - 1.0).abs() > 1e-12 {
            return Ok(None);
        }
        let query = MaterialQuery {
            temperature_k,
            applied_field_t: field_t,
            angle_from_normal_deg: angle_deg,
            electric_field_criterion_v_per_m: criterion,
        };
        if query.validate().is_err() {
            return Ok(None);
        }
        Ok(self
            .interpolator
            .evaluate(query.position())?
            .map(|e| e.ic_a_per_m))
    }

    /// Checks a `map_reference` row against a profile condition under the
    /// contract tolerances and returns the row's Ic per width, or why the
    /// reference is rejected.
    pub(crate) fn check_reference(
        &self,
        row: u32,
        temperature_k: f64,
        field_t: f64,
        angle_deg: f64,
        criterion: f64,
    ) -> Result<f64, String> {
        let Some(entry) = self.rows.get(&row) else {
            return Err(format!("map_reference row {row} does not exist in the map"));
        };
        let [t, b, a] = entry.nominal;
        if (temperature_k - t).abs() > 1.0 {
            return Err(format!(
                "map_reference row {row} (nominal {t} K) is more than 1.0 K from the profile temperature {temperature_k} K"
            ));
        }
        let both_zero = field_t == 0.0 && b == 0.0;
        if !both_zero && (b == 0.0 || (field_t / b - 1.0).abs() > 0.02) {
            return Err(format!(
                "map_reference row {row} (nominal {b} T) does not match the profile field {field_t} T (both 0 T, or within 2%)"
            ));
        }
        if !both_zero && (angle_deg - a).abs() > 5.0 {
            return Err(format!(
                "map_reference row {row} (nominal {a} degrees) is more than 5 degrees from the profile angle {angle_deg} degrees"
            ));
        }
        if (criterion / self.criterion - 1.0).abs() > 1e-12 {
            return Err(format!(
                "map_reference criterion check: profile criterion {criterion:e} V/m differs from the map's {:e} V/m",
                self.criterion
            ));
        }
        Ok(entry.ic_a_per_m)
    }

    /// Map Ic per width representing a profile's condition, and the row used
    /// when the profile carries a `map_reference`. Err is why the profile is
    /// not evaluable. A valid reference is preferred over interpolation.
    fn profile_value(&self, profile: &LengthProfile) -> Result<(f64, Option<u32>), String> {
        let fail = |why: String| Err(format!("profile '{}': {why}", profile.id));
        if let Some(reference) = &profile.map_reference {
            return self
                .check_reference(
                    reference.source_row,
                    profile.temperature_k,
                    profile.field_t,
                    profile.angle_from_normal_deg,
                    profile.electric_field_criterion_v_per_m,
                )
                .map(|value| (value, Some(reference.source_row)))
                .or_else(fail);
        }
        match self.value(
            profile.temperature_k,
            profile.field_t,
            profile.angle_from_normal_deg,
            profile.electric_field_criterion_v_per_m,
        ) {
            Ok(Some(value)) if value > 0.0 => Ok((value, None)),
            Ok(_) => fail(
                "the map cannot evaluate the profile condition (outside its measured cells or a different criterion) and no map_reference is given".into(),
            ),
            Err(error) => fail(error.to_string()),
        }
    }

    fn class(&self) -> EvidenceClass {
        match self.identity.data_class {
            MaterialDataClass::Measured => EvidenceClass::Measured,
            MaterialDataClass::SyntheticSensitivity => EvidenceClass::Synthetic,
            _ => EvidenceClass::ModelInformed,
        }
    }
}

pub(crate) enum Resolved {
    Model(Box<MapModel>),
    Unavailable(String),
}

/// Rates every reel of a validated inventory at `operating`. `supplied` holds
/// datasets from bundles; an unsupplied id falls back to the embedded store.
/// A map is used only when its id and `csv_sha256` equal the passport's.
pub fn rate_inventory(
    inventory: &ReelInventory,
    inventory_sha256: &str,
    operating: OperatingPoint,
    supplied: Vec<MaterialDataset>,
) -> Result<ReelRatingRecord, RunError> {
    inventory.validate()?;
    MaterialQuery {
        temperature_k: operating.temperature_k,
        applied_field_t: operating.field_t,
        angle_from_normal_deg: operating.angle_from_normal_deg,
        electric_field_criterion_v_per_m: operating.electric_field_criterion_v_per_m,
    }
    .validate()?;
    let mut supplied_by_id: BTreeMap<String, MaterialDataset> = BTreeMap::new();
    for dataset in supplied {
        let id = dataset.metadata.id.clone();
        if supplied_by_id.insert(id.clone(), dataset).is_some() {
            return Err(RunError::Invalid(format!(
                "duplicate supplied dataset id '{id}'"
            )));
        }
    }

    let mut resolved: BTreeMap<(String, String), Resolved> = BTreeMap::new();
    let mut reels = Vec::with_capacity(inventory.passports.len());
    for passport in &inventory.passports {
        if let Some(map) = &passport.product.product_map {
            let key = (map.dataset_id.clone(), map.csv_sha256.clone());
            resolved
                .entry(key.clone())
                .or_insert_with(|| resolve_map(&key.0, &key.1, &supplied_by_id));
        }
        reels.push(rate_reel(passport, operating, &resolved)?);
    }

    let mut datasets: Vec<RatingDatasetIdentity> = resolved
        .values()
        .filter_map(|r| match r {
            Resolved::Model(m) => Some(m.identity.clone()),
            Resolved::Unavailable(_) => None,
        })
        .collect();
    datasets.sort_by(|a, b| a.dataset_id.cmp(&b.dataset_id));

    let mut status_counts = BTreeMap::new();
    let mut rated_counts = EvidenceClassCounts::default();
    for reel in &reels {
        let name = serde_json::to_value(reel.status)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        *status_counts.entry(name).or_insert(0) += 1;
        match reel.evidence_class {
            Some(EvidenceClass::Measured) => rated_counts.measured += 1,
            Some(EvidenceClass::ModelInformed) => rated_counts.model_informed += 1,
            Some(EvidenceClass::Synthetic) => rated_counts.synthetic += 1,
            None => {}
        }
    }

    let implementation_sha256 = implementation_hash()?;
    let input_sha256 = hash(&serde_json::to_vec(&(
        inventory_sha256,
        &operating,
        &datasets,
        &implementation_sha256,
        LOG_IC_MODEL_ID,
    ))?);
    let limitations = record_limitations(inventory, reels.iter().any(|r| r.scaled.is_some()));
    Ok(ReelRatingRecord {
        schema: REEL_RATING_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: LOG_IC_MODEL_ID.into(),
        implementation_sha256,
        input_sha256,
        inventory_id: inventory.inventory_id.clone(),
        inventory_sha256: inventory_sha256.into(),
        inventory_evidence_class: inventory.evidence_class,
        operating_point: operating,
        datasets,
        summary: inventory.summary(),
        status_counts,
        rated_evidence_class_counts: rated_counts,
        reels,
        limitations,
    })
}

pub(crate) fn resolve_map(
    dataset_id: &str,
    csv_sha256: &str,
    supplied: &BTreeMap<String, MaterialDataset>,
) -> Resolved {
    let embedded;
    let (dataset, source) = match supplied.get(dataset_id) {
        Some(dataset) => (dataset, "supplied"),
        None => match MaterialDataset::embedded_by_id(dataset_id) {
            Ok(dataset) => {
                embedded = dataset;
                (&embedded, "embedded")
            }
            Err(_) => {
                return Resolved::Unavailable(format!(
                    "dataset '{dataset_id}' was not supplied and is not an embedded dataset"
                ));
            }
        },
    };
    if dataset.metadata.csv_sha256 != csv_sha256 {
        return Resolved::Unavailable(format!(
            "dataset '{dataset_id}' ({source}) has csv_sha256 {} but the passport pins {csv_sha256}",
            dataset.metadata.csv_sha256
        ));
    }
    match IcInterpolator::with_method(
        &dataset.points,
        dataset.metadata.max_cell_spans,
        IcInterpolationMethod::LogFieldLogCurrent,
    ) {
        Ok(interpolator) => Resolved::Model(Box::new(MapModel {
            identity: RatingDatasetIdentity {
                dataset_id: dataset_id.into(),
                csv_sha256: csv_sha256.into(),
                source: source.into(),
                data_class: dataset.metadata.data_class,
            },
            criterion: dataset.metadata.electric_field_criterion_v_per_m,
            rows: dataset
                .points
                .iter()
                .map(|p| {
                    (
                        p.source_row,
                        MapRow {
                            nominal: p.nominal_position(),
                            ic_a_per_m: p.ic_a_per_m,
                        },
                    )
                })
                .collect(),
            interpolator,
        })),
        Err(error) => Resolved::Unavailable(format!(
            "dataset '{dataset_id}' could not build an interpolator: {error}"
        )),
    }
}

fn unrated(
    passport: &ReelPassport,
    status: ReelRatingStatus,
    explanation: String,
    next_evidence: &str,
) -> ReelRating {
    ReelRating {
        reel_id: passport.reel_id.clone(),
        status,
        evidence_class: None,
        passport_evidence_class: passport.evidence_class,
        basis: "No rating was produced.".into(),
        explanation,
        next_evidence: Some(next_evidence.into()),
        width_m: passport.geometry.width_m,
        map_ic_a_per_m: None,
        orientation: None,
        product_level_ic_a: None,
        scaled: None,
        consistency_checks: Vec::new(),
        limitations: passport.limitations.clone(),
    }
}

fn offset_bound(offsets: &[AbOffset]) -> f64 {
    offsets
        .iter()
        .map(|o| o.offset_deg.abs() + o.uncertainty_deg)
        .fold(0.0, f64::max)
}

fn rate_reel(
    passport: &ReelPassport,
    op: OperatingPoint,
    resolved: &BTreeMap<(String, String), Resolved>,
) -> Result<ReelRating, RunError> {
    // Rule 1.
    let Some(map_ref) = &passport.product.product_map else {
        return Ok(unrated(
            passport,
            ReelRatingStatus::NoProductMap,
            "The passport names no product_map, so there is no critical-current map to rate the reel against.".into(),
            "Add a product_map (dataset_id and csv_sha256) for a material dataset that covers this product and the operating point.",
        ));
    };
    // Rule 2.
    let key = (map_ref.dataset_id.clone(), map_ref.csv_sha256.clone());
    let model = match resolved.get(&key) {
        Some(Resolved::Model(model)) => model,
        Some(Resolved::Unavailable(reason)) => {
            return Ok(unrated(
                passport,
                ReelRatingStatus::MapUnavailable,
                format!("The product map is unavailable: {reason}."),
                "Supply the dataset bundle whose id and csv_sha256 match the passport's product_map (--dataset-bundle), or correct the passport's pin.",
            ));
        }
        None => {
            return Ok(unrated(
                passport,
                ReelRatingStatus::MapUnavailable,
                "The product map was not resolved before rating.".into(),
                "Supply the dataset bundle whose id and csv_sha256 match the passport's product_map.",
            ));
        }
    };
    let width = passport.geometry.width_m;
    let map_label = format!(
        "{} (csv_sha256 {})",
        map_ref.dataset_id,
        &map_ref.csv_sha256[..16]
    );

    // Rule 3.
    let central = model.value(
        op.temperature_k,
        op.field_t,
        op.angle_from_normal_deg,
        op.electric_field_criterion_v_per_m,
    )?;
    if central.is_none() {
        return Ok(unrated(
            passport,
            ReelRatingStatus::OutsideMapDomain,
            format!(
                "Product map {map_label} cannot evaluate {:.3} K, {:.4} T, {:.2} degrees from the tape normal at {:e} V/m: the point is outside its measured cells or the criterion differs from the map's. No extrapolation is applied.",
                op.temperature_k,
                op.field_t,
                op.angle_from_normal_deg,
                op.electric_field_criterion_v_per_m
            ),
            "A product map measured at this temperature, field and angle, at this electric-field criterion.",
        ));
    }
    let central = central.unwrap_or_default();

    // Rules 4 and 5.
    let in_window = (op.angle_from_normal_deg - 90.0).abs() < ORIENTATION_WINDOW_DEG;
    let mut orientation = OrientationEvaluation {
        in_tape_plane_window: in_window,
        offset_bound_deg: None,
        angles_evaluated_deg: vec![op.angle_from_normal_deg],
    };
    let mut map_value = central;
    let mut orientation_text = String::new();
    if in_window {
        if passport.ab_offsets.is_empty() {
            let mut reel = unrated(
                passport,
                ReelRatingStatus::OrientationUnresolved,
                format!(
                    "The operating angle {:.2} degrees is within {ORIENTATION_WINDOW_DEG} degrees of the tape plane and the passport has no ab_offsets. A few degrees of ab-plane offset can change Ic there by up to about 20% (Cheng et al. 2025).",
                    op.angle_from_normal_deg
                ),
                "Measured ab-plane offsets for this reel (XRD rocking curve, transport or torque), or an operating angle further than 15 degrees from the tape plane.",
            );
            reel.map_ic_a_per_m = Some(central);
            reel.orientation = Some(orientation);
            return Ok(reel);
        }
        let bound = offset_bound(&passport.ab_offsets);
        let low = op.angle_from_normal_deg - bound;
        let high = op.angle_from_normal_deg + bound;
        let mut values = Vec::new();
        for angle in [low, high] {
            values.push(model.value(
                op.temperature_k,
                op.field_t,
                angle,
                op.electric_field_criterion_v_per_m,
            )?);
        }
        orientation.offset_bound_deg = Some(bound);
        orientation.angles_evaluated_deg = vec![low, high];
        let (Some(a), Some(b)) = (values[0], values[1]) else {
            let mut reel = unrated(
                passport,
                ReelRatingStatus::OutsideMapDomain,
                format!(
                    "Product map {map_label} cannot evaluate the offset-adjusted angles {low:.2} and {high:.2} degrees (offset bound {bound:.2} degrees). No extrapolation is applied."
                ),
                "A product map measured over the offset-adjusted angles, or tighter ab-offset measurements.",
            );
            reel.orientation = Some(orientation);
            return Ok(reel);
        };
        map_value = a.min(b);
        orientation_text = format!(
            " Near the tape plane, the map is evaluated at the angle minus and plus the largest |ab offset| + uncertainty on the reel ({bound:.2} degrees) and the smaller value is used; the reel's orientation in the coil is not chosen until CR-03."
        );
    }

    let mut rating = ReelRating {
        reel_id: passport.reel_id.clone(),
        status: ReelRatingStatus::RatedProductLevel,
        evidence_class: None,
        passport_evidence_class: passport.evidence_class,
        basis: String::new(),
        explanation: String::new(),
        next_evidence: None,
        width_m: width,
        map_ic_a_per_m: Some(map_value),
        orientation: Some(orientation),
        product_level_ic_a: None,
        scaled: None,
        consistency_checks: Vec::new(),
        limitations: passport.limitations.clone(),
    };

    // Rule 6.
    if passport.length_profiles.is_empty() {
        rating.status = ReelRatingStatus::RatedProductLevel;
        rating.evidence_class = Some(model.class().min(passport.evidence_class));
        rating.product_level_ic_a = Some(map_value * width);
        rating.basis = format!(
            "Product map {map_label} at the operating point times the reel width {width} m. The passport has no length profile, so nothing about this reel enters the value.{orientation_text}"
        );
        rating.explanation = format!(
            "Product-level rating {:.1} A: the map's value, not a statement about this reel.",
            map_value * width
        );
        rating.next_evidence = Some(
            "A length profile measured at a condition the product map covers, to scale the product map to this reel.".into(),
        );
        return Ok(rating);
    }

    // Rule 7.
    let mut chosen: Option<(&LengthProfile, f64, Option<u32>)> = None;
    let mut reasons = Vec::new();
    for profile in &passport.length_profiles {
        match model.profile_value(profile) {
            Ok((value, row)) => {
                chosen = Some((profile, value, row));
                break;
            }
            Err(reason) => reasons.push(reason),
        }
    }
    let Some((profile, profile_map, reference_row)) = chosen else {
        let mut reel = unrated(
            passport,
            ReelRatingStatus::ProfileUnscalable,
            format!(
                "None of the {} length profile(s) can be scaled against product map {map_label}: {}.",
                passport.length_profiles.len(),
                reasons.join("; ")
            ),
            "A length profile measured at a temperature, field, angle and criterion inside the product map's measured domain, or a map_reference to a map row that matches the profile condition.",
        );
        reel.map_ic_a_per_m = rating.map_ic_a_per_m;
        reel.orientation = rating.orientation;
        return Ok(reel);
    };

    let profile_map_a = profile_map * width;
    let rated_series: Vec<(f64, f64)> = profile
        .points
        .iter()
        .map(|[x, ic]| (*x, ic / profile_map_a * map_value * width))
        .collect();
    let usable: Vec<(f64, f64)> = rated_series
        .iter()
        .copied()
        .filter(|(x, _)| passport.is_usable_position(*x))
        .collect();
    if usable.is_empty() {
        let mut reel = unrated(
            passport,
            ReelRatingStatus::ProfileUnscalable,
            format!(
                "Every point of length profile '{}' lies inside an excluded or cut defect span, so no usable length is rated.",
                profile.id
            ),
            "A length profile with points in the usable length of the reel.",
        );
        reel.map_ic_a_per_m = rating.map_ic_a_per_m;
        reel.orientation = rating.orientation;
        return Ok(reel);
    }
    let mut sorted: Vec<f64> = usable.iter().map(|(_, v)| *v).collect();
    sorted.sort_by(f64::total_cmp);
    let (min_position_m, min_ic_a) =
        usable
            .iter()
            .copied()
            .fold((f64::NAN, f64::INFINITY), |best, (x, v)| {
                if v < best.1 { (x, v) } else { best }
            });

    let accepted_defects = passport
        .defects
        .iter()
        .filter(|d| d.action == DefectAction::Accepted)
        .map(|d| {
            let inside: Vec<&(f64, f64)> = rated_series
                .iter()
                .filter(|(x, _)| *x >= d.start_m && *x <= d.end_m)
                .collect();
            let best = inside
                .iter()
                .fold(None::<(f64, f64)>, |best, (x, v)| match best {
                    Some(b) if b.1 <= *v => Some(b),
                    _ => Some((*x, *v)),
                });
            AcceptedDefectRating {
                start_m: d.start_m,
                end_m: d.end_m,
                kind: d.kind,
                min_ic_fraction: d.min_ic_fraction,
                note: d.note.clone(),
                profile_points_in_span: inside.len(),
                min_ic_a: best.map(|b| b.1),
                min_position_m: best.map(|b| b.0),
            }
        })
        .collect();

    let consistency_checks = consistency(passport, op, &rated_series);
    rating.status = ReelRatingStatus::Rated;
    rating.evidence_class = Some(
        if passport.evidence_class == EvidenceClass::Synthetic
            || model.class() == EvidenceClass::Synthetic
        {
            EvidenceClass::Synthetic
        } else {
            EvidenceClass::ModelInformed
        },
    );
    let reference_text = reference_row
        .map(|row| format!(" The map value at the profile condition is map row {row}, declared by the passport's map_reference."))
        .unwrap_or_default();
    rating.basis = format!(
        "Length profile '{}' ({:.3} K, {:.4} T, {:.2} degrees) scales product map {map_label}: Ic(x) = ic_a(x) / (map at the profile condition x width) x (map at the operating point x width). This assumes that a reel's deviation at the profile condition carries over to the operating point; that transfer is untested (CR-01 had one sample per product), so the rating is model_informed at best.{reference_text}{orientation_text}",
        profile.id, profile.temperature_k, profile.field_t, profile.angle_from_normal_deg
    );
    rating.explanation = format!(
        "Scaled rating over {} of {} profile points in usable length: minimum {:.1} A at {:.3} m, 5th percentile {:.1} A, median {:.1} A.",
        usable.len(),
        profile.points.len(),
        min_ic_a,
        min_position_m,
        percentile(&sorted, 0.05),
        percentile(&sorted, 0.5)
    );
    rating.scaled = Some(ScaledRating {
        profile: ProfileCondition {
            profile_id: profile.id.clone(),
            temperature_k: profile.temperature_k,
            field_t: profile.field_t,
            angle_from_normal_deg: profile.angle_from_normal_deg,
            electric_field_criterion_v_per_m: profile.electric_field_criterion_v_per_m,
            map_ic_a_per_m: profile_map,
            map_ic_a: profile_map_a,
            map_reference_row: reference_row,
        },
        profile_point_count: profile.points.len(),
        usable_point_count: usable.len(),
        min_ic_a,
        p5_ic_a: percentile(&sorted, 0.05),
        median_ic_a: percentile(&sorted, 0.5),
        min_position_m,
        accepted_defects,
    });
    rating.consistency_checks = consistency_checks;
    Ok(rating)
}

/// Linear-interpolated percentile of a sorted, non-empty slice (fraction in
/// 0..=1; the same definition as numpy's default).
fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let h = (sorted.len() - 1) as f64 * fraction;
    let lo = h.floor() as usize;
    let hi = h.ceil() as usize;
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

fn interpolate_along(series: &[(f64, f64)], position: f64) -> Option<f64> {
    let first = series.first()?;
    let last = series.last()?;
    if position < first.0 || position > last.0 {
        return None;
    }
    let after = series.partition_point(|(x, _)| *x < position);
    let (x1, y1) = series[after];
    if x1 == position || after == 0 {
        return Some(y1);
    }
    let (x0, y0) = series[after - 1];
    Some(y0 + (y1 - y0) * (position - x0) / (x1 - x0))
}

fn consistency(
    passport: &ReelPassport,
    op: OperatingPoint,
    rated_series: &[(f64, f64)],
) -> Vec<ConsistencyCheck> {
    passport
        .in_field_points
        .iter()
        .filter(|p| {
            (p.temperature_k - op.temperature_k).abs() <= CONSISTENCY_TEMPERATURE_K
                && (p.field_t - op.field_t).abs() <= CONSISTENCY_FIELD_FRACTION * op.field_t
                && (p.angle_from_normal_deg - op.angle_from_normal_deg).abs()
                    <= CONSISTENCY_ANGLE_DEG
        })
        .map(|p| {
            let criterion_matches = (p.electric_field_criterion_v_per_m
                / op.electric_field_criterion_v_per_m
                - 1.0)
                .abs()
                <= 1e-12;
            let scaled = interpolate_along(rated_series, p.position_m);
            let (difference, note) = match (scaled, criterion_matches) {
                (_, false) => (
                    None,
                    "The point's electric-field criterion differs from the operating criterion; no comparison is made.",
                ),
                (None, true) => (
                    None,
                    "The point lies outside the length profile's positions; the scaled rating is not extrapolated.",
                ),
                (Some(s), true) if s > 0.0 => (
                    Some((p.ic_a - s) / s),
                    "A check against the scaled rating; it changes no status and does not adjust the rating.",
                ),
                (Some(_), true) => (
                    None,
                    "The scaled rating is zero at this position; a relative difference is undefined.",
                ),
            };
            ConsistencyCheck {
                sample_id: p.sample_id.clone(),
                position_m: p.position_m,
                temperature_k: p.temperature_k,
                field_t: p.field_t,
                angle_from_normal_deg: p.angle_from_normal_deg,
                measured_ic_a: p.ic_a,
                scaled_rating_ic_a: scaled,
                relative_difference: difference,
                note: note.into(),
            }
        })
        .collect()
}

fn record_limitations(inventory: &ReelInventory, scaled_rating_given: bool) -> Vec<String> {
    let mut limitations = vec![
        "A rating is not an engineering current allowance and not an acceptance; no pass or fail verdict is given.".into(),
        "Scale transfer is untested: a scaled rating assumes that a reel's deviation from the product map at the length profile's condition carries over to the operating point. CR-01 could not test this because the Robinson data has one sample per product. Every scaled rating is therefore at most model_informed.".into(),
        "Every rating comes from a product map that covers the operating point. Outside the map's measured cells the status is outside_map_domain; fields, temperatures and angles are not extrapolated.".into(),
        "Map values are per unit width and are multiplied by the passport's width_m; the map's own measurement geometry and self-field behaviour are those of its dataset.".into(),
        "Near the tape plane, ab-plane offsets are applied conservatively as plus and minus the largest |offset| + uncertainty on the reel; the reel's orientation in the coil is not chosen until CR-03.".into(),
        "In-field points on the reel are reported as a consistency check only; they do not adjust the rating or change a status.".into(),
    ];
    if scaled_rating_given {
        limitations.push("Published within-product transfer scatter between 77 K self-field and 20 K, 20 T Ic is about 15% (1 sigma) for one production YBCO wire (Molodyk et al., Sci. Rep. 11, 2084, 2021); scaled ratings at low temperature carry at least comparable uncertainty.".into());
    }
    limitations.extend(inventory.limitations.iter().cloned());
    limitations
}

fn implementation_hash() -> Result<String, RunError> {
    Ok(hash(&serde_json::to_vec(&(
        include_str!("../../optcoil-model/src/reel.rs"),
        include_str!("../../optcoil-model/src/material.rs"),
        include_str!("../../optcoil-physics/src/critical_current.rs"),
        include_str!("reel.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?))
}

/// Result of validating one passport document.
#[derive(Debug, Clone, Serialize)]
pub struct PassportValidation {
    pub schema: String,
    pub reel_id: String,
    /// SHA-256 of the document's exact bytes.
    pub passport_sha256: String,
    pub evidence_class: EvidenceClass,
    pub length_m: f64,
    pub usable_length_m: f64,
    pub length_profiles: usize,
    pub in_field_points: usize,
    pub ab_offsets: usize,
    pub defects: usize,
    pub product_map: Option<ProductMapRef>,
    pub limitations: Vec<String>,
}

/// Result of validating one inventory document.
#[derive(Debug, Clone, Serialize)]
pub struct InventoryValidation {
    pub schema: String,
    pub inventory_id: String,
    /// SHA-256 of the document's exact bytes.
    pub inventory_sha256: String,
    pub evidence_class: EvidenceClass,
    pub summary: InventorySummary,
    pub limitations: Vec<String>,
}

/// Parses and validates a passport document given as text.
pub fn validate_passport_json(json: &str) -> Result<PassportValidation, RunError> {
    let passport = ReelPassport::from_json(json)?;
    Ok(PassportValidation {
        usable_length_m: passport.usable_length_m(),
        passport_sha256: passport_sha256(json.as_bytes()),
        schema: passport.schema,
        reel_id: passport.reel_id,
        evidence_class: passport.evidence_class,
        length_m: passport.geometry.length_m,
        length_profiles: passport.length_profiles.len(),
        in_field_points: passport.in_field_points.len(),
        ab_offsets: passport.ab_offsets.len(),
        defects: passport.defects.len(),
        product_map: passport.product.product_map,
        limitations: passport.limitations,
    })
}

/// Parses and validates an inventory document given as text.
pub fn validate_inventory_json(json: &str) -> Result<InventoryValidation, RunError> {
    let inventory = ReelInventory::from_json(json)?;
    Ok(InventoryValidation {
        summary: inventory.summary(),
        inventory_sha256: passport_sha256(json.as_bytes()),
        schema: inventory.schema,
        inventory_id: inventory.inventory_id,
        evidence_class: inventory.evidence_class,
        limitations: inventory.limitations,
    })
}

/// Rates an inventory given as text. `bundle_jsons` are dataset bundle
/// documents; their identities are enforced exactly as for the CLI.
pub fn rate_inventory_json(
    inventory_json: &str,
    operating: OperatingPoint,
    bundle_jsons: &[&str],
) -> Result<ReelRatingRecord, RunError> {
    let inventory = ReelInventory::from_json(inventory_json)?;
    let datasets = bundle_jsons
        .iter()
        .map(|bundle| MaterialDataset::from_bundle_json(bundle))
        .collect::<Result<Vec<_>, _>>()?;
    rate_inventory(
        &inventory,
        &passport_sha256(inventory_json.as_bytes()),
        operating,
        datasets,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const MAP_ID: &str = "robinson-superpower-ap-v3";

    fn map_hash() -> String {
        MaterialDataset::embedded_by_id(MAP_ID)
            .unwrap()
            .metadata
            .csv_sha256
    }

    /// A measured node of the embedded dataset (rows 1817 and 1818) so the
    /// expected values can be computed by hand from the CSV.
    const PROFILE_CONDITION: (f64, f64, f64) = (19.98, 1.0, -0.64);
    const PROFILE_MAP_A_PER_M: f64 = 293_111.0;
    const OPERATING_MAP_A_PER_M: f64 = 292_719.0;

    fn op() -> OperatingPoint {
        OperatingPoint {
            temperature_k: 19.99,
            field_t: 1.0,
            angle_from_normal_deg: 4.41,
            electric_field_criterion_v_per_m: 1e-4,
        }
    }

    fn passport_json(id: &str) -> Value {
        json!({
            "schema": "optcoil-reel-passport/v1",
            "reel_id": id,
            "evidence_class": "measured",
            "product": {
                "vendor": "Illustrative Vendor",
                "product": "Illustrative Tape",
                "batch": null,
                "product_map": {"dataset_id": MAP_ID, "csv_sha256": map_hash()}
            },
            "geometry": {"length_m": 100.0, "width_m": 0.012},
            "length_profiles": [{
                "id": "p1", "method": "reel-to-reel transport",
                "evidence_class": "measured",
                "temperature_k": PROFILE_CONDITION.0,
                "field_t": PROFILE_CONDITION.1,
                "angle_from_normal_deg": PROFILE_CONDITION.2,
                "electric_field_criterion_v_per_m": 1e-4,
                "resolution_m": 10.0,
                "source_sha256": null,
                "points": [[0.0, 3000.0], [10.0, 3300.0], [20.0, 2400.0]]
            }],
            "in_field_points": [],
            "ab_offsets": [],
            "defects": [],
            "provenance": {"issuer": "test", "issued_at": "2026-10-01", "sources": []},
            "limitations": ["test passport"]
        })
    }

    fn passport(mutate: impl FnOnce(&mut Value)) -> ReelPassport {
        let mut value = passport_json("r1");
        mutate(&mut value);
        ReelPassport::from_json(&value.to_string()).unwrap()
    }

    fn inventory_of(passports: Vec<ReelPassport>) -> ReelInventory {
        let json = json!({
            "schema": "optcoil-reel-inventory/v1",
            "inventory_id": "inv",
            "evidence_class": "synthetic",
            "passports": passports,
            "limitations": []
        });
        ReelInventory::from_json(&json.to_string()).unwrap()
    }

    fn rate_one(p: ReelPassport, op: OperatingPoint, supplied: Vec<MaterialDataset>) -> ReelRating {
        let record = rate_inventory(&inventory_of(vec![p]), "00", op, supplied).unwrap();
        record.reels.into_iter().next().unwrap()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "{a} vs {b}");
    }

    #[test]
    fn scale_formula_matches_hand_computation() {
        let rating = rate_one(passport(|_| {}), op(), vec![]);
        assert_eq!(rating.status, ReelRatingStatus::Rated);
        assert_eq!(rating.evidence_class, Some(EvidenceClass::ModelInformed));
        assert!(rating.next_evidence.is_none());
        assert!(rating.basis.contains("untested") || rating.basis.contains("assumes"));
        let r = OPERATING_MAP_A_PER_M / PROFILE_MAP_A_PER_M;
        let scaled = rating.scaled.unwrap();
        close(scaled.profile.map_ic_a_per_m, PROFILE_MAP_A_PER_M);
        close(scaled.profile.map_ic_a, PROFILE_MAP_A_PER_M * 0.012);
        close(rating.map_ic_a_per_m.unwrap(), OPERATING_MAP_A_PER_M);
        // Rated Ic(x) = ic(x) * (operating map / profile map).
        close(scaled.min_ic_a, 2400.0 * r);
        close(scaled.min_position_m, 20.0);
        close(scaled.median_ic_a, 3000.0 * r);
        // Sorted 2400, 3000, 3300: p5 sits at index 0.1.
        close(scaled.p5_ic_a, 2460.0 * r);
        assert_eq!(scaled.usable_point_count, 3);
    }

    #[test]
    fn excluded_spans_leave_usable_points_and_accepted_spans_report_minimum() {
        let rating = rate_one(
            passport(|v| {
                v["defects"] = json!([
                    {"start_m": 5.0, "end_m": 12.0, "kind": "dropout", "action": "excluded",
                     "min_ic_fraction": null, "note": "x"},
                    {"start_m": 15.0, "end_m": 25.0, "kind": "mechanical", "action": "accepted",
                     "min_ic_fraction": 0.8, "note": "y"},
                    {"start_m": 50.0, "end_m": 60.0, "kind": "splice", "action": "accepted",
                     "min_ic_fraction": null, "note": "empty"}
                ]);
            }),
            op(),
            vec![],
        );
        let r = OPERATING_MAP_A_PER_M / PROFILE_MAP_A_PER_M;
        let scaled = rating.scaled.unwrap();
        assert_eq!(scaled.usable_point_count, 2);
        close(scaled.min_ic_a, 2400.0 * r);
        close(scaled.median_ic_a, 2700.0 * r);
        close(scaled.p5_ic_a, 2430.0 * r);
        assert_eq!(scaled.accepted_defects.len(), 2);
        close(scaled.accepted_defects[0].min_ic_a.unwrap(), 2400.0 * r);
        close(scaled.accepted_defects[0].min_position_m.unwrap(), 20.0);
        assert_eq!(scaled.accepted_defects[1].profile_points_in_span, 0);
        assert!(scaled.accepted_defects[1].min_ic_a.is_none());
    }

    #[test]
    fn every_status_is_reached_with_explanations() {
        let no_map = rate_one(
            passport(|v| v["product"]["product_map"] = Value::Null),
            op(),
            vec![],
        );
        assert_eq!(no_map.status, ReelRatingStatus::NoProductMap);

        let bad_hash = rate_one(
            passport(|v| v["product"]["product_map"]["csv_sha256"] = json!("0".repeat(64))),
            op(),
            vec![],
        );
        assert_eq!(bad_hash.status, ReelRatingStatus::MapUnavailable);
        let unknown = rate_one(
            passport(|v| v["product"]["product_map"]["dataset_id"] = json!("not-a-dataset")),
            op(),
            vec![],
        );
        assert_eq!(unknown.status, ReelRatingStatus::MapUnavailable);

        let mut high_field = op();
        high_field.field_t = 12.0;
        let outside = rate_one(passport(|_| {}), high_field, vec![]);
        assert_eq!(outside.status, ReelRatingStatus::OutsideMapDomain);
        let mut other_criterion = op();
        other_criterion.electric_field_criterion_v_per_m = 1e-3;
        assert_eq!(
            rate_one(passport(|_| {}), other_criterion, vec![]).status,
            ReelRatingStatus::OutsideMapDomain
        );

        let plane = OperatingPoint {
            temperature_k: 24.99,
            field_t: 2.0,
            angle_from_normal_deg: 88.69,
            electric_field_criterion_v_per_m: 1e-4,
        };
        let unresolved = rate_one(passport(|_| {}), plane, vec![]);
        assert_eq!(unresolved.status, ReelRatingStatus::OrientationUnresolved);

        let product_level = rate_one(passport(|v| v["length_profiles"] = json!([])), op(), vec![]);
        assert_eq!(product_level.status, ReelRatingStatus::RatedProductLevel);
        close(
            product_level.product_level_ic_a.unwrap(),
            OPERATING_MAP_A_PER_M * 0.012,
        );
        assert_eq!(product_level.evidence_class, Some(EvidenceClass::Measured));

        let unscalable = rate_one(
            passport(|v| v["length_profiles"][0]["temperature_k"] = json!(77.0)),
            op(),
            vec![],
        );
        assert_eq!(unscalable.status, ReelRatingStatus::ProfileUnscalable);

        let all_cut = rate_one(
            passport(|v| {
                v["defects"] = json!([{"start_m": 0.0, "end_m": 30.0, "kind": "other",
                    "action": "cut", "min_ic_fraction": null, "note": ""}]);
            }),
            op(),
            vec![],
        );
        assert_eq!(all_cut.status, ReelRatingStatus::ProfileUnscalable);

        let rated = rate_one(passport(|_| {}), op(), vec![]);
        assert_eq!(rated.status, ReelRatingStatus::Rated);

        for reel in [
            no_map,
            bad_hash,
            unknown,
            outside,
            unresolved,
            product_level,
            unscalable,
            all_cut,
        ] {
            assert!(!reel.explanation.is_empty(), "{:?}", reel.status);
            assert!(
                reel.next_evidence.as_deref().is_some_and(|s| !s.is_empty()),
                "{:?}",
                reel.status
            );
        }
        assert!(rated.next_evidence.is_none());
    }

    #[test]
    fn first_evaluable_profile_is_used() {
        let rating = rate_one(
            passport(|v| {
                let mut unevaluable = v["length_profiles"][0].clone();
                unevaluable["id"] = json!("p0");
                unevaluable["temperature_k"] = json!(77.0);
                v["length_profiles"]
                    .as_array_mut()
                    .unwrap()
                    .insert(0, unevaluable);
            }),
            op(),
            vec![],
        );
        assert_eq!(rating.scaled.unwrap().profile.profile_id, "p1");
    }

    #[test]
    fn orientation_window_uses_the_smaller_offset_adjusted_value() {
        let plane = OperatingPoint {
            temperature_k: 24.99,
            field_t: 2.0,
            angle_from_normal_deg: 88.69,
            electric_field_criterion_v_per_m: 1e-4,
        };
        let offsets = |v: &mut Value| {
            v["length_profiles"] = json!([]);
            v["ab_offsets"] = json!([
                {"position_m": 1.0, "offset_deg": -1.5, "uncertainty_deg": 0.5,
                 "method": "xrd_rocking_curve", "evidence_class": "measured"},
                {"position_m": 2.0, "offset_deg": 0.5, "uncertainty_deg": 0.1,
                 "method": "torque", "evidence_class": "measured"}
            ]);
        };
        let rating = rate_one(passport(offsets), plane, vec![]);
        assert_eq!(rating.status, ReelRatingStatus::RatedProductLevel);
        let orientation = rating.orientation.unwrap();
        assert!(orientation.in_tape_plane_window);
        close(orientation.offset_bound_deg.unwrap(), 2.0);
        let dataset = MaterialDataset::embedded_by_id(MAP_ID).unwrap();
        let model = match resolve_map(MAP_ID, &map_hash(), &BTreeMap::new()) {
            Resolved::Model(model) => model,
            Resolved::Unavailable(reason) => panic!("{reason}"),
        };
        drop(dataset);
        let low = model.value(24.99, 2.0, 86.69, 1e-4).unwrap().unwrap();
        let high = model.value(24.99, 2.0, 90.69, 1e-4).unwrap().unwrap();
        close(rating.map_ic_a_per_m.unwrap(), low.min(high));
        // Just outside the window (|theta - 90| = 15) nothing is applied.
        let outside_window = OperatingPoint {
            angle_from_normal_deg: 75.0,
            ..plane
        };
        let r = rate_one(passport(offsets), outside_window, vec![]);
        assert!(!r.orientation.unwrap().in_tape_plane_window);
        // An offset pushing an evaluated angle outside the map stops the rating.
        let huge = rate_one(
            passport(|v| {
                offsets(v);
                v["ab_offsets"][0]["offset_deg"] = json!(400.0);
            }),
            plane,
            vec![],
        );
        assert_eq!(huge.status, ReelRatingStatus::OutsideMapDomain);
    }

    #[test]
    fn identity_mismatch_is_rejected_for_supplied_datasets() {
        let mut altered = MaterialDataset::embedded_by_id(MAP_ID).unwrap();
        altered.metadata.csv_sha256 = "1".repeat(64);
        let rating = rate_one(passport(|_| {}), op(), vec![altered]);
        assert_eq!(rating.status, ReelRatingStatus::MapUnavailable);
        assert!(rating.explanation.contains("csv_sha256"));

        let matching = MaterialDataset::embedded_by_id(MAP_ID).unwrap();
        let record = rate_inventory(
            &inventory_of(vec![passport(|_| {})]),
            "00",
            op(),
            vec![matching.clone()],
        )
        .unwrap();
        assert_eq!(record.reels[0].status, ReelRatingStatus::Rated);
        assert_eq!(record.datasets.len(), 1);
        assert_eq!(record.datasets[0].source, "supplied");

        let duplicate = rate_inventory(
            &inventory_of(vec![passport(|_| {})]),
            "00",
            op(),
            vec![matching.clone(), matching],
        );
        assert!(duplicate.is_err());
    }

    #[test]
    fn consistency_check_interpolates_and_changes_no_status() {
        let rating = rate_one(
            passport(|v| {
                v["in_field_points"] = json!([
                    {"sample_id": "near", "position_m": 5.0, "temperature_k": 20.3,
                     "field_t": 1.01, "angle_from_normal_deg": 5.0, "ic_a": 3000.0,
                     "n_value": 25.0, "electric_field_criterion_v_per_m": 1e-4,
                     "lab": "lab", "evidence_class": "measured", "source_sha256": null},
                    {"sample_id": "far-temperature", "position_m": 5.0, "temperature_k": 21.0,
                     "field_t": 1.0, "angle_from_normal_deg": 4.41, "ic_a": 1.0,
                     "n_value": null, "electric_field_criterion_v_per_m": 1e-4,
                     "lab": "lab", "evidence_class": "measured", "source_sha256": null},
                    {"sample_id": "off-profile", "position_m": 50.0, "temperature_k": 20.0,
                     "field_t": 1.0, "angle_from_normal_deg": 4.41, "ic_a": 1.0,
                     "n_value": null, "electric_field_criterion_v_per_m": 1e-4,
                     "lab": "lab", "evidence_class": "measured", "source_sha256": null}
                ]);
            }),
            op(),
            vec![],
        );
        assert_eq!(rating.status, ReelRatingStatus::Rated);
        assert_eq!(rating.consistency_checks.len(), 2);
        let r = OPERATING_MAP_A_PER_M / PROFILE_MAP_A_PER_M;
        let near = &rating.consistency_checks[0];
        let scaled = 3150.0 * r;
        close(near.scaled_rating_ic_a.unwrap(), scaled);
        close(
            near.relative_difference.unwrap(),
            (3000.0 - scaled) / scaled,
        );
        let off = &rating.consistency_checks[1];
        assert!(off.scaled_rating_ic_a.is_none() && off.relative_difference.is_none());
    }

    #[test]
    fn synthetic_passport_caps_the_evidence_class() {
        let rating = rate_one(
            passport(|v| {
                v["evidence_class"] = json!("synthetic");
            }),
            op(),
            vec![],
        );
        assert_eq!(rating.evidence_class, Some(EvidenceClass::Synthetic));
        let product = rate_one(
            passport(|v| {
                v["evidence_class"] = json!("model_informed");
                v["length_profiles"] = json!([]);
            }),
            op(),
            vec![],
        );
        assert_eq!(product.evidence_class, Some(EvidenceClass::ModelInformed));
    }

    #[test]
    fn model_extension_maps_cap_product_level_at_model_informed() {
        let id = "robinson-superpower-ap-v3-modelext";
        let dataset = MaterialDataset::embedded_by_id(id).unwrap();
        let hash = dataset.metadata.csv_sha256.clone();
        let rating = rate_one(
            passport(|v| {
                v["product"]["product_map"] = json!({"dataset_id": id, "csv_sha256": hash});
                v["length_profiles"] = json!([]);
            }),
            op(),
            vec![],
        );
        assert_eq!(rating.status, ReelRatingStatus::RatedProductLevel);
        assert_eq!(rating.evidence_class, Some(EvidenceClass::ModelInformed));
    }

    #[test]
    fn record_carries_identity_summary_and_limitations() {
        let record =
            rate_inventory(&inventory_of(vec![passport(|_| {})]), "abc", op(), vec![]).unwrap();
        assert_eq!(record.schema, REEL_RATING_SCHEMA);
        assert_eq!(record.inventory_sha256, "abc");
        assert_eq!(record.datasets[0].source, "embedded");
        assert_eq!(record.status_counts["rated"], 1);
        assert_eq!(record.rated_evidence_class_counts.model_informed, 1);
        assert_eq!(record.summary.reel_count, 1);
        assert!(
            record
                .limitations
                .iter()
                .any(|l| l.contains("Scale transfer is untested"))
        );
        assert_eq!(record.implementation_sha256.len(), 64);
        let bad = OperatingPoint {
            temperature_k: -1.0,
            ..op()
        };
        assert!(rate_inventory(&inventory_of(vec![passport(|_| {})]), "abc", bad, vec![]).is_err());
    }

    /// Row 1817: nominal 20 K, 1 T, 0 degrees; Ic 293111 A/m.
    fn with_reference(v: &mut Value, field: &str, value: Value) {
        v["length_profiles"][0]["temperature_k"] = json!(20.0);
        v["length_profiles"][0]["field_t"] = json!(1.0);
        v["length_profiles"][0]["angle_from_normal_deg"] = json!(0.0);
        v["length_profiles"][0]["map_reference"] = json!({"source_row": 1817});
        if !field.is_empty() {
            v["length_profiles"][0][field] = value;
        }
    }

    #[test]
    fn valid_map_reference_supplies_the_profile_map_value() {
        let rating = rate_one(
            passport(|v| with_reference(v, "", Value::Null)),
            op(),
            vec![],
        );
        assert_eq!(rating.status, ReelRatingStatus::Rated);
        let scaled = rating.scaled.unwrap();
        assert_eq!(scaled.profile.map_reference_row, Some(1817));
        assert_eq!(scaled.profile.map_ic_a_per_m, PROFILE_MAP_A_PER_M);
        assert!(rating.basis.contains("map row 1817"));
        let r = OPERATING_MAP_A_PER_M / PROFILE_MAP_A_PER_M;
        close(scaled.min_ic_a, 2400.0 * r);
        // The reference wins over the interpolator at the same condition.
        let model = match resolve_map(MAP_ID, &map_hash(), &BTreeMap::new()) {
            Resolved::Model(model) => model,
            Resolved::Unavailable(reason) => panic!("{reason}"),
        };
        let interpolated = model.value(20.0, 1.0, 0.0, 1e-4).unwrap().unwrap();
        assert_ne!(interpolated, PROFILE_MAP_A_PER_M);
    }

    #[test]
    fn map_reference_tolerances_and_missing_rows_make_a_profile_unevaluable() {
        // Edges that still pass: 0.9 K, 1.5%, 5 degrees.
        for (field, value) in [
            ("temperature_k", json!(20.9)),
            ("field_t", json!(1.015)),
            ("angle_from_normal_deg", json!(5.0)),
        ] {
            let rating = rate_one(
                passport(|v| with_reference(v, field, value.clone())),
                op(),
                vec![],
            );
            assert_eq!(rating.status, ReelRatingStatus::Rated, "{field}");
        }
        for (field, value) in [
            ("temperature_k", json!(21.5)),
            ("field_t", json!(1.05)),
            ("field_t", json!(0.0)),
            ("angle_from_normal_deg", json!(5.5)),
            ("electric_field_criterion_v_per_m", json!(1e-3)),
        ] {
            let rating = rate_one(
                passport(|v| with_reference(v, field, value.clone())),
                op(),
                vec![],
            );
            assert_eq!(
                rating.status,
                ReelRatingStatus::ProfileUnscalable,
                "{field} {value}"
            );
            assert!(
                rating.explanation.contains("map_reference"),
                "{}",
                rating.explanation
            );
            assert!(rating.next_evidence.is_some());
        }
        let missing = rate_one(
            passport(|v| {
                with_reference(v, "", Value::Null);
                v["length_profiles"][0]["map_reference"] = json!({"source_row": 999_999});
            }),
            op(),
            vec![],
        );
        assert_eq!(missing.status, ReelRatingStatus::ProfileUnscalable);
        assert!(missing.explanation.contains("999999"));
        // A failed reference falls through to the next profile.
        let fallthrough = rate_one(
            passport(|v| {
                let mut bad = v["length_profiles"][0].clone();
                with_reference(v, "", Value::Null);
                bad["id"] = json!("p0");
                bad["map_reference"] = json!({"source_row": 999_999});
                v["length_profiles"].as_array_mut().unwrap().insert(0, bad);
            }),
            op(),
            vec![],
        );
        assert_eq!(fallthrough.status, ReelRatingStatus::Rated);
        assert_eq!(fallthrough.scaled.unwrap().profile.profile_id, "p1");
    }

    #[test]
    fn transfer_scatter_limitation_appears_with_a_scaled_rating_only() {
        let sentence = "about 15% (1 sigma)";
        let rated =
            rate_inventory(&inventory_of(vec![passport(|_| {})]), "00", op(), vec![]).unwrap();
        assert!(
            rated
                .limitations
                .iter()
                .any(|l| l.contains(sentence) && l.contains("Molodyk"))
        );
        let none = rate_inventory(
            &inventory_of(vec![passport(|v| v["length_profiles"] = json!([]))]),
            "00",
            op(),
            vec![],
        )
        .unwrap();
        assert!(!none.limitations.iter().any(|l| l.contains(sentence)));
    }

    const EXAMPLE_INVENTORY: &str =
        include_str!("../../../examples/reels/inventory-illustrative.json");
    const EXAMPLE_PASSPORT: &str =
        include_str!("../../../examples/reels/passport-illustrative.json");

    #[test]
    fn json_entry_points_hash_exact_text_and_match_the_struct_api() {
        let passport = validate_passport_json(EXAMPLE_PASSPORT).unwrap();
        assert_eq!(passport.reel_id, "ILLUSTRATIVE-REEL-A");
        assert_eq!(passport.usable_length_m, 45.0);
        assert_eq!(
            passport.passport_sha256,
            optcoil_model::attestation::sha256_hex(EXAMPLE_PASSPORT.as_bytes())
        );
        let inventory = validate_inventory_json(EXAMPLE_INVENTORY).unwrap();
        assert_eq!(inventory.summary.reel_count, 3);
        assert!(validate_passport_json("{}").is_err());
        assert!(
            validate_inventory_json(&EXAMPLE_INVENTORY.replace("reel_id", "reel_ident")).is_err()
        );

        let op = OperatingPoint {
            temperature_k: 25.0,
            field_t: 2.0,
            angle_from_normal_deg: 0.0,
            electric_field_criterion_v_per_m: 1e-4,
        };
        let record = rate_inventory_json(EXAMPLE_INVENTORY, op, &[]).unwrap();
        assert_eq!(record.status_counts["rated"], 2);
        assert_eq!(record.datasets[0].source, "embedded");

        let genuine = MaterialDataset::embedded_bundle_json(MAP_ID).unwrap();
        let supplied = rate_inventory_json(EXAMPLE_INVENTORY, op, &[genuine.as_str()]).unwrap();
        assert_eq!(supplied.datasets[0].source, "supplied");
        assert!(rate_inventory_json(EXAMPLE_INVENTORY, op, &["{}"]).is_err());
    }

    #[test]
    fn percentile_uses_linear_interpolation() {
        assert_eq!(percentile(&[1.0], 0.05), 1.0);
        assert_eq!(percentile(&[0.0, 10.0], 0.5), 5.0);
        close(percentile(&[0.0, 10.0, 20.0, 30.0, 40.0], 0.05), 2.0);
    }
}
