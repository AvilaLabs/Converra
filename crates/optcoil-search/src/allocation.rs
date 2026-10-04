//! Allocation demand table (`optcoil-allocation-demand/v1`, CR-03 slice A).
//!
//! A reel's capacity relative to its product map is a scale factor s(x)
//! (CR-02 rule 7). The coil's screening allows
//! `(1 - budget) * width * K_map(T, B, theta) * strands` at a position and
//! passes when `I_op <= u * allowed`, so a reel of scale `s` placed there
//! passes iff `s >= s_req = I_op / (u * allowed_map)`. This module computes
//! `s_req` for every turn and module of a coupled-search optimum, with the
//! product map substituted at every turn, and for each tabulated ab-plane
//! offset bound. It does not allocate anything.
//!
//! The per-point capacity is recomputed with the engine's own query function
//! (`query_mirror_pair_with_clamp`), the engine's monotonicity gate and
//! mirror-pair combination, and the engine's tape-level formula
//! `(1 - budget) * (width * min_k) * strands`. At offset 0 the recomputation
//! must equal the engine's `allowed_screening_a` for every tape within 1e-12
//! relative, or the build fails.

use std::{collections::BTreeMap, path::Path};

use optcoil_model::{
    Status,
    coupled::{CoupledCase, TapeNormal},
    coupled_search::BASE_TAPE_SPEC_ID,
    material::{MaterialDataClass, MaterialDataset},
    reel::ProductMapRef,
};
use optcoil_physics::{
    critical_current::LOG_IC_MODEL_ID,
    racetrack::MU0_H_PER_M,
    tape_frame::{self, LOBATTO_POINT_COUNT, QueryBasis},
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    coupled::{
        CandidateWidthPoint, CoupledOptions, PointBasis, SpecRuntime, TapeCandidateResult,
        combine_mirror_pair, run_coupled_case_with_datasets, spec_runtime,
    },
    coupled_search::{CoupledSearchRunRecord, turn_strand_lengths_m},
    hash,
    reel::ORIENTATION_WINDOW_DEG,
    write_json_new,
};

pub const ALLOCATION_DEMAND_SCHEMA: &str = "optcoil-allocation-demand/v1";
pub const ALLOCATION_DEMAND_MODEL_ID: &str = "allocation-demand/map-scale-requirement/v1";

/// The engine's sampling cap per axis (`MAX_SAMPLED_INDICES` in the coupled
/// case schema); turns and modules are chunked to stay within it.
const SAMPLING_CHUNK: u32 = 64;
/// Required agreement of the offset-0 recomputation with the engine.
const GATE_RELATIVE_TOLERANCE: f64 = 1e-12;
/// Agreement of the turn-length sum with the record's cost ledger.
const LEDGER_RELATIVE_TOLERANCE: f64 = 1e-9;
const MAX_OFFSET_DEG: f64 = 45.0;
const MAX_OFFSET_COUNT: usize = 64;

/// The default tabulated offset bounds: 0 to 6 degrees in 0.5 degree steps.
pub fn default_angle_offsets_deg() -> Vec<f64> {
    (0..=12).map(|i| f64::from(i) * 0.5).collect()
}

#[derive(Debug, Clone)]
pub struct DemandOptions {
    /// The product map the demand is computed with. `None` takes the record's
    /// own base material (`case.material`).
    pub product_map: Option<ProductMapRef>,
    /// Allow the product map to differ from the material the record screened
    /// with; the substitution is recorded.
    pub allow_map_substitution: bool,
    /// Offset bounds to tabulate, in degrees. Must be strictly ascending.
    /// Offset 0 is always tabulated first (it is the engine gate).
    pub angle_offsets_deg: Vec<f64>,
}

impl Default for DemandOptions {
    fn default() -> Self {
        Self {
            product_map: None,
            allow_map_substitution: false,
            angle_offsets_deg: default_angle_offsets_deg(),
        }
    }
}

/// Whether a requirement value exists at a position.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DemandStatus {
    Ok,
    /// Some sampled width point has an Unsupported basis: the product map
    /// has no estimate there. Nothing is extrapolated.
    OutsideMapDomain,
    /// Some sampled width point was excluded by the along-current gate, so
    /// the measured law does not apply there.
    AlongCurrentExcluded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandIdentities {
    /// SHA-256 of the search record's exact bytes.
    pub record_sha256: String,
    /// The record's own `case_sha256`.
    pub case_sha256: String,
    pub search_case_id: String,
    pub record_search_status: Status,
    pub optimum_index: usize,
    pub product_map: ProductMapIdentity,
    /// Distinct (dataset id, csv sha) the record screened the optimum's
    /// turns with.
    pub screened_materials: Vec<MaterialRef>,
    /// True when the product map differs from any material the record
    /// screened with.
    pub map_substituted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MaterialRef {
    pub dataset_id: String,
    pub csv_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductMapIdentity {
    pub dataset_id: String,
    pub csv_sha256: String,
    /// `embedded` or `supplied`.
    pub source: String,
    pub data_class: MaterialDataClass,
    pub interpolation_method: String,
    pub low_field_clamp_t: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandGeometry {
    pub turns_along_normal: u32,
    pub modules: u32,
    pub strands: u32,
    /// Conductor streams: one per (module, strand).
    pub streams: u32,
    pub tape_width_m: f64,
    pub tape_normal: String,
    /// Conductor length of one stream (sum of the turn lengths).
    pub stream_length_m: f64,
    pub operating_current_a: f64,
    pub operating_temperature_k: f64,
    pub electric_field_criterion_v_per_m: f64,
    pub stations: Vec<String>,
}

/// The requirement at one offset bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SReqAtOffset {
    pub offset_deg: f64,
    /// `I_op / (u * allowed)`, maximised over stations; absent when the
    /// position is blocked at this offset.
    pub s_req: Option<f64>,
    pub status: DemandStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDemand {
    pub module: u32,
    /// |theta - 90| < 15 degrees at any sampled width point.
    pub offset_sensitive: bool,
    /// Status at offset 0; equals `s_req[0].status`.
    pub status: DemandStatus,
    /// One entry per tabulated offset, in `angle_offsets_deg` order.
    pub s_req: Vec<SReqAtOffset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnDemand {
    pub turn: u32,
    pub length_m: f64,
    /// Stream coordinate of the start of this turn; 0 at the start of turn 1.
    pub start_m: f64,
    /// Any module of this turn is offset sensitive.
    pub offset_sensitive: bool,
    /// Worst module status at offset 0.
    pub status: DemandStatus,
    pub modules: Vec<ModuleDemand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandGate {
    /// PASS iff every tape's offset-0 recomputation equals the engine.
    pub status: Status,
    pub relative_tolerance: f64,
    pub tapes_compared: u64,
    pub max_relative_difference: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerCheck {
    pub installed_length_ledger_m: f64,
    /// Sum of turn lengths times modules times strands.
    pub installed_length_from_turns_m: f64,
    pub relative_difference: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandSummary {
    /// Largest s_req over unblocked positions, per tabulated offset.
    pub peak_s_req: Vec<Option<f64>>,
    pub blocked_positions: u64,
    pub offset_sensitive_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationDemand {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub ic_model_id: String,
    pub implementation_sha256: String,
    pub identities: DemandIdentities,
    pub geometry: DemandGeometry,
    pub utilization_limit: f64,
    pub budget: f64,
    pub angle_offsets_deg: Vec<f64>,
    pub offset_window_deg: f64,
    pub gate: DemandGate,
    pub ledger_check: LedgerCheck,
    pub turns: Vec<TurnDemand>,
    pub summary: DemandSummary,
    pub limitations: Vec<String>,
}

impl AllocationDemand {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

/// Builds the demand table from the exact text of a coupled-search run
/// record. `bundle_jsons` are dataset bundle documents; the product map
/// resolves from them or from the embedded store, and its identity is
/// enforced as for reel ratings.
pub fn build_allocation_demand_json(
    record_json: &str,
    options: &DemandOptions,
    bundle_jsons: &[&str],
) -> Result<AllocationDemand, RunError> {
    let datasets = bundle_jsons
        .iter()
        .map(|bundle| MaterialDataset::from_bundle_json(bundle))
        .collect::<Result<Vec<_>, _>>()?;
    build_allocation_demand(record_json, options, &datasets)
}

pub fn build_allocation_demand(
    record_json: &str,
    options: &DemandOptions,
    supplied: &[MaterialDataset],
) -> Result<AllocationDemand, RunError> {
    build_with_chunk(record_json, options, supplied, SAMPLING_CHUNK)
}

fn invalid(message: impl Into<String>) -> RunError {
    RunError::Invalid(message.into())
}

/// Whether a tape angle (degrees from the normal) lies within the window
/// around the tape plane where ab-plane offsets matter.
pub(crate) fn is_offset_sensitive(angle_from_normal_deg: f64) -> bool {
    (angle_from_normal_deg - 90.0).abs() < ORIENTATION_WINDOW_DEG
}

fn validated_offsets(requested: &[f64]) -> Result<Vec<f64>, RunError> {
    for value in requested {
        if !value.is_finite() || !(0.0..=MAX_OFFSET_DEG).contains(value) {
            return Err(invalid(format!(
                "angle offset {value} must be finite and within 0..={MAX_OFFSET_DEG} degrees"
            )));
        }
    }
    if requested.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("angle offsets must be strictly ascending"));
    }
    let mut offsets = requested.to_vec();
    if offsets.first().is_none_or(|first| *first != 0.0) {
        offsets.insert(0, 0.0);
    }
    if offsets.len() > MAX_OFFSET_COUNT {
        return Err(invalid(format!(
            "at most {MAX_OFFSET_COUNT} angle offsets may be tabulated"
        )));
    }
    Ok(offsets)
}

/// Why a position has no requirement value.
#[derive(Debug, Clone)]
struct Blocked {
    status: DemandStatus,
    explanation: String,
    next_step: String,
}

enum TapeOutcome {
    Allowed(f64),
    Blocked(Blocked),
}

/// Everything the per-tape recomputation reads from the generated case.
struct PointContext<'a> {
    runtime: &'a SpecRuntime,
    temperature_k: f64,
    clamp_t: f64,
    correction: Option<&'a str>,
    k_transport_a_per_m: f64,
    width_m: f64,
    strands: f64,
    budget: f64,
    map_id: &'a str,
}

/// Fold-angle sample set for an offset bound: theta - delta, theta,
/// theta + delta, and every integer degree strictly between the ends.
fn angle_samples(theta_deg: f64, delta_deg: f64) -> Vec<f64> {
    let mut samples = vec![theta_deg];
    if delta_deg > 0.0 {
        let (low, high) = (theta_deg - delta_deg, theta_deg + delta_deg);
        samples.push(low);
        samples.push(high);
        let mut k = low.floor() + 1.0;
        while k < high {
            if k > low {
                samples.push(k);
            }
            k += 1.0;
        }
    }
    samples
}

impl PointContext<'_> {
    /// The magnitude the engine queries the map with: the applied field plus
    /// the declared self-field correction.
    fn query_magnitude_t(&self, point: &CandidateWidthPoint) -> f64 {
        match self.correction {
            Some("critical_state_strip") => {
                point.magnitude_t + point.critical_state_edge_field_t.unwrap_or(0.0)
            }
            Some(_) => point.magnitude_t + MU0_H_PER_M * self.k_transport_a_per_m / 2.0,
            None => point.magnitude_t,
        }
    }

    /// The engine's per-point capacity at one fold angle: mirror-pair query
    /// with the low-field clamp, the monotonicity gate, then the pair
    /// combination.
    fn point_k(
        &self,
        query_magnitude_t: f64,
        fold_deg: f64,
    ) -> Result<(QueryBasis, Option<f64>), RunError> {
        let mirror_deg = tape_frame::theta_mirror_deg(fold_deg);
        let clamped = tape_frame::query_mirror_pair_with_clamp(
            &self.runtime.interpolator,
            self.temperature_k,
            query_magnitude_t,
            fold_deg,
            mirror_deg,
            self.clamp_t,
        )?;
        let (mut folded, mut mirror) = (clamped.folded, clamped.mirror);
        if !self.runtime.monotonicity_ok {
            for angle in [&mut folded, &mut mirror] {
                if angle.basis == QueryBasis::LowerBound {
                    angle.basis = QueryBasis::Unsupported;
                    angle.k_a_per_m = None;
                }
            }
        }
        let (basis, k, _) = combine_mirror_pair(&folded, &mirror, query_magnitude_t);
        Ok((basis, k))
    }

    /// The block the engine itself reports at offset 0, if any width point
    /// has no determined capacity there.
    fn engine_block(&self, station: &str, tape: &TapeCandidateResult) -> Option<Blocked> {
        for (index, point) in tape.points.iter().enumerate() {
            if point.basis == PointBasis::Unsupported {
                return Some(Blocked {
                    status: DemandStatus::OutsideMapDomain,
                    explanation: format!(
                        "station '{station}', width point {index}: the product map '{}' has no estimate at |B| = {:.4} T, {:.2} degrees from the tape normal. Only measured cells are used and nothing is extrapolated.",
                        self.map_id, self.query_magnitude_t(point), point.angle_folded_deg
                    ),
                    next_step: "Supply a product map that measures this field and angle, or change the design so this position stays inside the map.".into(),
                });
            }
        }
        for (index, point) in tape.points.iter().enumerate() {
            if point.basis == PointBasis::AlongCurrentExcluded {
                return Some(Blocked {
                    status: DemandStatus::AlongCurrentExcluded,
                    explanation: format!(
                        "station '{station}', width point {index}: the along-current field fraction {:.3} exceeds the case's limit, so the measured transverse law does not apply.",
                        point.along_current_fraction
                    ),
                    next_step: "Declare the transverse_bound along-current model in the search case, or change the design so the field at this position is mostly transverse.".into(),
                });
            }
        }
        if tape.points.iter().any(|p| p.k_used_a_per_m.is_none()) {
            return Some(Blocked {
                status: DemandStatus::OutsideMapDomain,
                explanation: format!(
                    "station '{station}': a width point has no determined capacity in the product map '{}'.",
                    self.map_id
                ),
                next_step: "Supply a product map that measures this field and angle.".into(),
            });
        }
        None
    }

    /// Tape-level allowed current at each offset bound, formed from the
    /// width points exactly as the engine forms it: the minimum K over the
    /// five Lobatto points, times width, `(1 - budget)` and strands.
    fn tape_outcomes(
        &self,
        station: &str,
        tape: &TapeCandidateResult,
        offsets: &[f64],
    ) -> Result<Vec<TapeOutcome>, RunError> {
        if tape.points.len() != LOBATTO_POINT_COUNT {
            return Err(invalid(format!(
                "tape at station '{station}' has {} width points; expected {LOBATTO_POINT_COUNT}",
                tape.points.len()
            )));
        }
        if let Some(block) = self.engine_block(station, tape) {
            return Ok(offsets
                .iter()
                .map(|_| TapeOutcome::Blocked(block.clone()))
                .collect());
        }
        let query_magnitudes: Vec<f64> = tape
            .points
            .iter()
            .map(|p| self.query_magnitude_t(p))
            .collect();
        // Per-point cache of the pair query by fold-angle bits: integer
        // degrees recur across offset bounds.
        let mut caches: Vec<BTreeMap<u64, (QueryBasis, Option<f64>)>> =
            vec![BTreeMap::new(); tape.points.len()];
        let mut outcomes = Vec::with_capacity(offsets.len());
        // Offsets ascend and each interval contains the previous one, so the
        // samples of every smaller bound are carried into the larger ones:
        // the sampled minimum is then non-increasing in the bound, as the
        // true minimum over the nested intervals is.
        let mut running_k = [f64::INFINITY; LOBATTO_POINT_COUNT];
        let mut blocked_at: Option<(usize, f64)> = None;
        for &delta in offsets {
            if blocked_at.is_none() {
                'points: for (index, point) in tape.points.iter().enumerate() {
                    for fold_deg in angle_samples(point.angle_folded_deg, delta) {
                        let entry = match caches[index].get(&fold_deg.to_bits()) {
                            Some(entry) => *entry,
                            None => {
                                let entry = self.point_k(query_magnitudes[index], fold_deg)?;
                                caches[index].insert(fold_deg.to_bits(), entry);
                                entry
                            }
                        };
                        match entry {
                            (QueryBasis::Unsupported, _) | (_, None) => {
                                blocked_at = Some((index, fold_deg));
                                break 'points;
                            }
                            (_, Some(value)) => running_k[index] = running_k[index].min(value),
                        }
                    }
                }
            }
            if let Some((index, fold_deg)) = blocked_at {
                let point = &tape.points[index];
                outcomes.push(TapeOutcome::Blocked(Blocked {
                    status: DemandStatus::OutsideMapDomain,
                    explanation: format!(
                        "station '{station}', width point {index}: with an ab-plane offset bound of {delta} degrees the tape angle ranges over {:.2} to {:.2} degrees from the normal, and the product map '{}' has no estimate at |B| = {:.4} T and {fold_deg:.2} degrees. Nothing is extrapolated.",
                        point.angle_folded_deg - delta,
                        point.angle_folded_deg + delta,
                        self.map_id,
                        query_magnitudes[index],
                    ),
                    next_step: "Use a reel with a smaller offset bound, supply a product map measured over this angle range, or change the design.".into(),
                }));
                continue;
            }
            let k = running_k;
            let i_min = tape_frame::i_min_a(self.width_m, k);
            let allowed = (1.0 - self.budget) * i_min * self.strands;
            if allowed.is_finite() && allowed > 0.0 {
                outcomes.push(TapeOutcome::Allowed(allowed));
            } else {
                outcomes.push(TapeOutcome::Blocked(Blocked {
                    status: DemandStatus::OutsideMapDomain,
                    explanation: format!(
                        "station '{station}': the product map '{}' gives no positive capacity at this position (allowed current {allowed}).",
                        self.map_id
                    ),
                    next_step: "Supply a product map with positive measured capacity here, or change the design.".into(),
                }));
            }
        }
        Ok(outcomes)
    }
}

/// Running maximum of s_req at one (turn, module).
struct ModuleAccumulator {
    offset_sensitive: bool,
    s_req: Vec<Option<f64>>,
    blocked: Vec<Option<Blocked>>,
    stations_seen: usize,
}

impl ModuleAccumulator {
    fn new(offsets: usize) -> Self {
        Self {
            offset_sensitive: false,
            s_req: vec![None; offsets],
            blocked: vec![None; offsets],
            stations_seen: 0,
        }
    }

    fn fold(&mut self, index: usize, outcome: &TapeOutcome, operating_a: f64, u: f64) {
        match outcome {
            TapeOutcome::Allowed(allowed) => {
                let s = operating_a / (u * allowed);
                self.s_req[index] = Some(self.s_req[index].map_or(s, |c| c.max(s)));
            }
            TapeOutcome::Blocked(block) => {
                // An outside-map-domain block outranks an along-current one.
                let replace = match &self.blocked[index] {
                    None => true,
                    Some(current) => {
                        current.status == DemandStatus::AlongCurrentExcluded
                            && block.status == DemandStatus::OutsideMapDomain
                    }
                };
                if replace {
                    self.blocked[index] = Some(block.clone());
                }
            }
        }
    }
}

fn build_with_chunk(
    record_json: &str,
    options: &DemandOptions,
    supplied: &[MaterialDataset],
    chunk: u32,
) -> Result<AllocationDemand, RunError> {
    let offsets = validated_offsets(&options.angle_offsets_deg)?;
    let record_sha256 = hash(record_json.as_bytes());
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| invalid(format!("run record does not parse: {e}")))?;
    let search = &record.case;

    // Step 1: the selected optimum must pass its own screening.
    let best_index = record
        .best_index
        .ok_or_else(|| invalid("record has no PASS optimum; there is no design to allocate"))?;
    let best = record
        .candidates
        .get(best_index)
        .ok_or_else(|| invalid("record best_index is outside its candidate list"))?;
    let screening_status = best.screening.as_ref().map(|s| s.status);
    if screening_status != Some(Status::Pass) {
        return Err(invalid(format!(
            "the selected optimum's screening status is {screening_status:?}, not PASS; a demand table is only built for a design that passes"
        )));
    }
    if !best.pack_geometry_valid {
        return Err(invalid("the selected optimum's pack geometry is not valid"));
    }
    let operating_a = best.operating_current_a;
    if !operating_a.is_finite() || operating_a <= 0.0 {
        return Err(invalid(
            "the selected optimum has no valid operating current",
        ));
    }

    // Geometry restriction of v1.
    let fixed = &search.fixed_geometry;
    if fixed.tape_normal != TapeNormal::Radial {
        return Err(invalid(
            "allocation demand v1 supports the radial tape normal only; the axial normal is refused",
        ));
    }
    if fixed.path3d.is_some() {
        return Err(invalid(
            "allocation demand v1 supports the legacy racetrack and planar path geometries only; a non-planar path3d centerline is refused",
        ));
    }
    let geometry = &best.geometry;
    let (n, p, strands) = (
        geometry.turns_along_normal,
        geometry.tapes_along_width,
        geometry.strands_parallel,
    );
    if n == 0 || p == 0 || strands == 0 {
        return Err(invalid(
            "turns, modules and strands must each be at least 1",
        ));
    }
    // Width and strands bookkeeping against the record's own counts.
    if geometry.total_turns != u64::from(n) * u64::from(p) {
        return Err(invalid(format!(
            "bookkeeping: total_turns {} does not equal turns {n} x modules {p}",
            geometry.total_turns
        )));
    }
    if geometry.total_conductors != 0
        && geometry.total_conductors != geometry.total_turns * u64::from(strands)
    {
        return Err(invalid(format!(
            "bookkeeping: total_conductors {} does not equal total_turns {} x strands {strands}",
            geometry.total_conductors, geometry.total_turns
        )));
    }
    let assignment: Vec<String> = geometry.tape_spec_ids.clone().unwrap_or_default();

    // Step 2: product map identity and substitution.
    let map_ref = options
        .product_map
        .clone()
        .unwrap_or_else(|| ProductMapRef {
            dataset_id: search.material.dataset_id.clone(),
            csv_sha256: search.material.csv_sha256.clone(),
        });
    let (dataset, source) = match supplied
        .iter()
        .find(|d| d.metadata.id == map_ref.dataset_id)
    {
        Some(dataset) => (dataset.clone(), "supplied"),
        None => (
            MaterialDataset::embedded_by_id(&map_ref.dataset_id).map_err(|_| {
                invalid(format!(
                    "product map '{}' was not supplied and is not an embedded dataset",
                    map_ref.dataset_id
                ))
            })?,
            "embedded",
        ),
    };
    if dataset.metadata.csv_sha256 != map_ref.csv_sha256 {
        return Err(invalid(format!(
            "product map '{}' ({source}) has csv_sha256 {} but {} was pinned",
            map_ref.dataset_id, dataset.metadata.csv_sha256, map_ref.csv_sha256
        )));
    }
    let map_material = MaterialRef {
        dataset_id: map_ref.dataset_id.clone(),
        csv_sha256: map_ref.csv_sha256.clone(),
    };
    let mut screened: Vec<MaterialRef> = Vec::new();
    for turn in 1..=n {
        let spec_id = search.spec_id_for_turn(n, turn, &assignment);
        let material = if spec_id == BASE_TAPE_SPEC_ID {
            &search.material
        } else {
            &search
                .tape_specs
                .as_ref()
                .and_then(|specs| specs.get(spec_id))
                .ok_or_else(|| {
                    invalid(format!("turn {turn} resolves to unknown spec '{spec_id}'"))
                })?
                .material
        };
        let reference = MaterialRef {
            dataset_id: material.dataset_id.clone(),
            csv_sha256: material.csv_sha256.clone(),
        };
        if !screened.contains(&reference) {
            screened.push(reference);
        }
    }
    screened.sort();
    let map_substituted = screened.iter().any(|m| *m != map_material);
    if map_substituted && !options.allow_map_substitution {
        let names: Vec<String> = screened
            .iter()
            .map(|m| {
                format!(
                    "{} ({}...)",
                    m.dataset_id,
                    &m.csv_sha256[..m.csv_sha256.len().min(12)]
                )
            })
            .collect();
        return Err(invalid(format!(
            "the product map {} ({}...) differs from the material the record screened with: {}. Pass --allow-map-substitution to compute the demand with the product map anyway; the substitution is recorded.",
            map_material.dataset_id,
            &map_material.csv_sha256[..map_material.csv_sha256.len().min(12)],
            names.join(", ")
        )));
    }

    // Every turn's material is the product map; the base binding's method
    // and policies carry over.
    let mut map_settings = search.material.clone();
    map_settings.dataset_id = map_ref.dataset_id.clone();
    map_settings.csv_sha256 = map_ref.csv_sha256.clone();

    // Step 3: build and run coupled cases over all turns and modules.
    let dims = geometry.dims();
    let stations = search.sampling.stations.clone();
    let station_ids: Vec<String> = stations.iter().map(|s| s.id().to_owned()).collect();
    let n_usize = n as usize;
    let p_usize = p as usize;
    let mut accumulators: Vec<ModuleAccumulator> = (0..n_usize * p_usize)
        .map(|_| ModuleAccumulator::new(offsets.len()))
        .collect();
    let mut width_m = f64::NAN;
    let mut tapes_compared = 0_u64;
    let mut max_relative_difference = 0.0_f64;
    let mut critical_state = false;
    let mut runtime_cache: Option<SpecRuntime> = None;

    let chunk = chunk.max(1);
    let mut turn_start = 1_u32;
    while turn_start <= n {
        let turn_end = (turn_start + chunk - 1).min(n);
        let mut module_start = 1_u32;
        while module_start <= p {
            let module_end = (module_start + chunk - 1).min(p);
            let mut case = optcoil_model::coupled_search::build_coupled_case(
                search,
                n,
                p,
                strands,
                operating_a,
                stations.clone(),
                (turn_start..=turn_end).collect(),
                (module_start..=module_end).collect(),
                None,
                dims,
            )?;
            case.material = map_settings.clone();
            case.tape_specs = None;
            let case_json = serde_json::to_string(&case)?;
            let run = run_coupled_case_with_datasets(
                &case_json,
                None,
                &CoupledOptions::default(),
                &[&dataset],
            )?;
            if runtime_cache.is_none() {
                critical_state =
                    case.limits.self_field_correction.as_deref() == Some("critical_state_strip");
                runtime_cache = Some(spec_runtime(
                    &case.material,
                    &dataset,
                    case.operating.temperature_k,
                    critical_state,
                )?);
                width_m = case.winding.tape_width_m;
            }
            let runtime = runtime_cache.as_ref().expect("runtime built above");
            let context = point_context(&case, runtime, &map_ref.dataset_id);

            for group in &run.stations {
                for tape in &group.tapes {
                    let candidate = tape
                        .per_candidate
                        .first()
                        .ok_or_else(|| invalid("coupled run returned a tape with no candidate"))?;
                    let outcomes = context.tape_outcomes(&group.station, candidate, &offsets)?;

                    // The offset-0 gate against the engine.
                    tapes_compared += 1;
                    match (&outcomes[0], candidate.allowed_screening_a) {
                        (TapeOutcome::Allowed(ours), Some(engine)) => {
                            let difference = (ours - engine).abs() / ours.abs().max(engine.abs());
                            max_relative_difference = max_relative_difference.max(difference);
                            if difference > GATE_RELATIVE_TOLERANCE {
                                return Err(invalid(format!(
                                    "offset-0 gate failed at station '{}', module {}, turn {}: recomputed allowed {ours} A against the engine's {engine} A (relative difference {difference:e}, limit {GATE_RELATIVE_TOLERANCE:e})",
                                    group.station, tape.tape_index, tape.turn_index
                                )));
                            }
                        }
                        (TapeOutcome::Blocked(_), None) => {}
                        _ => {
                            return Err(invalid(format!(
                                "offset-0 gate failed at station '{}', module {}, turn {}: the recomputation and the engine disagree on whether the tape has a determined capacity",
                                group.station, tape.tape_index, tape.turn_index
                            )));
                        }
                    }

                    let slot =
                        (tape.turn_index as usize - 1) * p_usize + (tape.tape_index as usize - 1);
                    let accumulator = accumulators.get_mut(slot).ok_or_else(|| {
                        invalid("coupled run returned a tape outside the winding")
                    })?;
                    accumulator.stations_seen += 1;
                    if candidate
                        .points
                        .iter()
                        .any(|point| is_offset_sensitive(point.angle_folded_deg))
                    {
                        accumulator.offset_sensitive = true;
                    }
                    for (index, outcome) in outcomes.iter().enumerate() {
                        accumulator.fold(
                            index,
                            outcome,
                            operating_a,
                            case.limits.utilization_limit,
                        );
                    }
                }
            }
            module_start = module_end + 1;
        }
        turn_start = turn_end + 1;
    }
    for (slot, accumulator) in accumulators.iter().enumerate() {
        if accumulator.stations_seen != stations.len() {
            return Err(invalid(format!(
                "turn {} module {} was sampled at {} of {} stations",
                slot / p_usize + 1,
                slot % p_usize + 1,
                accumulator.stations_seen,
                stations.len()
            )));
        }
    }

    // Step 7: turn lengths from the cost ledger's own walk.
    let lengths = turn_strand_lengths_m(search, n, p, &assignment, dims);
    if lengths.len() != n_usize {
        return Err(invalid(format!(
            "the ledger walk returned {} turn lengths for {n} turns",
            lengths.len()
        )));
    }
    let stream_length_m: f64 = lengths.iter().sum();
    let installed_from_turns = stream_length_m * f64::from(p) * f64::from(strands);
    let installed_ledger = best.cost.installed_length_m;
    let ledger_difference = (installed_from_turns - installed_ledger).abs()
        / installed_from_turns
            .abs()
            .max(installed_ledger.abs())
            .max(1.0);
    if ledger_difference > LEDGER_RELATIVE_TOLERANCE {
        return Err(invalid(format!(
            "bookkeeping: turn lengths x modules {p} x strands {strands} give {installed_from_turns} m, but the record's ledger installed {installed_ledger} m"
        )));
    }

    // Assemble.
    let utilization_limit = search.limits.utilization_limit;
    let mut turns = Vec::with_capacity(n_usize);
    let mut start_m = 0.0_f64;
    let mut peak: Vec<Option<f64>> = vec![None; offsets.len()];
    let mut blocked_positions = 0_u64;
    let mut sensitive_turns = 0_u32;
    for (turn_index, length_m) in lengths.iter().enumerate() {
        let mut modules = Vec::with_capacity(p_usize);
        for module_index in 0..p_usize {
            let accumulator = &accumulators[turn_index * p_usize + module_index];
            let entries: Vec<SReqAtOffset> = offsets
                .iter()
                .enumerate()
                .map(|(index, &offset_deg)| match &accumulator.blocked[index] {
                    Some(block) => SReqAtOffset {
                        offset_deg,
                        s_req: None,
                        status: block.status,
                        explanation: Some(block.explanation.clone()),
                        next_step: Some(block.next_step.clone()),
                    },
                    None => SReqAtOffset {
                        offset_deg,
                        s_req: accumulator.s_req[index],
                        status: DemandStatus::Ok,
                        explanation: None,
                        next_step: None,
                    },
                })
                .collect();
            for (index, entry) in entries.iter().enumerate() {
                if let Some(s) = entry.s_req {
                    peak[index] = Some(peak[index].map_or(s, |c: f64| c.max(s)));
                }
            }
            if entries[0].status != DemandStatus::Ok {
                blocked_positions += 1;
            }
            modules.push(ModuleDemand {
                module: module_index as u32 + 1,
                offset_sensitive: accumulator.offset_sensitive,
                status: entries[0].status,
                s_req: entries,
            });
        }
        let offset_sensitive = modules.iter().any(|m| m.offset_sensitive);
        if offset_sensitive {
            sensitive_turns += 1;
        }
        let status = modules
            .iter()
            .map(|m| m.status)
            .find(|s| *s == DemandStatus::OutsideMapDomain)
            .or_else(|| {
                modules
                    .iter()
                    .map(|m| m.status)
                    .find(|s| *s == DemandStatus::AlongCurrentExcluded)
            })
            .unwrap_or(DemandStatus::Ok);
        turns.push(TurnDemand {
            turn: turn_index as u32 + 1,
            length_m: *length_m,
            start_m,
            offset_sensitive,
            status,
            modules,
        });
        start_m += length_m;
    }

    let runtime = runtime_cache.as_ref().expect("at least one chunk ran");
    Ok(AllocationDemand {
        schema: ALLOCATION_DEMAND_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: ALLOCATION_DEMAND_MODEL_ID.into(),
        ic_model_id: LOG_IC_MODEL_ID.into(),
        implementation_sha256: implementation_hash()?,
        identities: DemandIdentities {
            record_sha256,
            case_sha256: record.case_sha256.clone(),
            search_case_id: search.id.clone(),
            record_search_status: record.search_status,
            optimum_index: best_index,
            product_map: ProductMapIdentity {
                dataset_id: map_ref.dataset_id.clone(),
                csv_sha256: map_ref.csv_sha256.clone(),
                source: source.into(),
                data_class: dataset.metadata.data_class,
                interpolation_method: runtime.material.method.clone(),
                low_field_clamp_t: runtime.material.low_field_clamp_t,
            },
            screened_materials: screened,
            map_substituted,
        },
        geometry: DemandGeometry {
            turns_along_normal: n,
            modules: p,
            strands,
            streams: p * strands,
            tape_width_m: width_m,
            tape_normal: "radial".into(),
            stream_length_m,
            operating_current_a: operating_a,
            operating_temperature_k: search.operating.temperature_k,
            electric_field_criterion_v_per_m: search.operating.electric_field_criterion_v_per_m,
            stations: station_ids,
        },
        utilization_limit,
        budget: search.limits.interpolation_overprediction_budget,
        angle_offsets_deg: offsets.clone(),
        offset_window_deg: ORIENTATION_WINDOW_DEG,
        gate: DemandGate {
            status: Status::Pass,
            relative_tolerance: GATE_RELATIVE_TOLERANCE,
            tapes_compared,
            max_relative_difference,
        },
        ledger_check: LedgerCheck {
            installed_length_ledger_m: installed_ledger,
            installed_length_from_turns_m: installed_from_turns,
            relative_difference: ledger_difference,
        },
        turns,
        summary: DemandSummary {
            peak_s_req: peak,
            blocked_positions,
            offset_sensitive_turns: sensitive_turns,
        },
        limitations: limitations(map_substituted, critical_state, &record),
    })
}

fn point_context<'a>(
    case: &'a CoupledCase,
    runtime: &'a SpecRuntime,
    map_id: &'a str,
) -> PointContext<'a> {
    let strands = f64::from(case.winding.strands_parallel.max(1));
    PointContext {
        runtime,
        temperature_k: case.operating.temperature_k,
        clamp_t: case.material.low_field_clamp_t,
        correction: case.limits.self_field_correction.as_deref(),
        k_transport_a_per_m: case.operating.current_candidates_a[0]
            / (case.winding.tape_width_m * strands),
        width_m: case.winding.tape_width_m,
        strands,
        budget: case.limits.interpolation_overprediction_budget,
        map_id,
    }
}

fn limitations(
    map_substituted: bool,
    critical_state: bool,
    record: &CoupledSearchRunRecord,
) -> Vec<String> {
    let mut list = vec![
        "One product map. Every reel in an inventory must name the same product map. The demand is computed with that map at every turn, regardless of the case's grading. Mixed-product allocation is later work.".to_string(),
        "Radial tape normal, legacy racetrack or planar path geometry only. Other geometries are refused with a reason.".to_string(),
        "Width. A reel's width_m must equal the case's tape width within 1e-9 m.".to_string(),
        "Streams. Each (module, strand) is an independent conductor stream running through turns 1 to n in winding order. Module joints are structural and counted. Pieces within a stream are joined by splices. Every strand of a module carries the same requirement; it is stated per strand and equal strand scale is not assumed beyond each strand meeting it.".to_string(),
        "Sampling basis. A turn's requirement is the maximum over the case's declared stations and the five width points. Positions between stations are not sampled, as in the search's own screening. The search itself sampled only some turns; this table covers all of them with the same stations.".to_string(),
        "Orientation. The engine takes the minimum over each mirror pair of angles, so which face of a reel points inward cannot gain anything; v1 makes no flip choice. ab-plane offsets are applied conservatively: a reel's requirement is evaluated over the tape-angle interval [theta - o, theta + o] (the pair queried at theta - o, theta, theta + o and every integer degree between; the samples of every smaller tabulated bound are included too, so the requirement never falls as the bound grows), where o is the reel's largest |offset| plus uncertainty, rounded up to the next tabulated step.".to_string(),
        "A reel with no ab offsets cannot be placed at a position whose angle lies within 15 degrees of the tape plane (offset_sensitive). A reel whose bound exceeds the largest tabulated step cannot be placed at offset-sensitive positions.".to_string(),
        "The requirement is a ratio to the product map, so it assumes a reel's deviation at its profile condition carries over to the operating point (CR-02 rule 2). The within-product transfer scatter is about 15 percent (1 sigma) at 20 K (Molodyk et al. 2021); the allocation step must apply a declared derate.".to_string(),
        "This table is a model result for allocation planning. It is not an engineering acceptance and not a current allowance.".to_string(),
    ];
    if map_substituted {
        list.push(
            "The product map differs from the material the record screened with. The record's screening verdict does not apply to this demand table.".into(),
        );
    }
    if critical_state {
        list.push(
            "The case declares the critical_state_strip self-field correction; the recorded edge field of each engine point is used for the offset recomputation.".into(),
        );
    }
    if record.case.grading.is_some() {
        list.push(
            "The record's case is graded. The demand ignores the grading and uses the product map at every turn.".into(),
        );
    }
    list
}

fn implementation_hash() -> Result<String, RunError> {
    Ok(hash(&serde_json::to_vec(&(
        include_str!("allocation.rs"),
        include_str!("coupled.rs"),
        include_str!("reel.rs"),
        include_str!("../../optcoil-physics/src/tape_frame.rs"),
        include_str!("../../optcoil-physics/src/critical_current.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?))
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::{Value, json};

    use super::*;
    use crate::coupled_search::{
        CoupledSearchOptions, run_coupled_search_case, tests::reduced_case_json,
    };

    const MAP_ID: &str = "robinson-superpower-ap-v3";

    /// One real search record shared by the tests: 70 turns (past the 64
    /// sampling cap), 2 modules, 1 station.
    fn record_json() -> &'static str {
        static RECORD: OnceLock<String> = OnceLock::new();
        RECORD.get_or_init(|| {
            let case = reduced_case_json("[70]", 30.0).replace(
                r#""baseline": {"turns_along_normal": 3"#,
                r#""baseline": {"turns_along_normal": 70"#,
            );
            let record =
                run_coupled_search_case(&case, &CoupledSearchOptions { threads: None }).unwrap();
            assert!(record.best_index.is_some(), "fixture must have an optimum");
            serde_json::to_string(&record).unwrap()
        })
    }

    fn demand() -> &'static AllocationDemand {
        static DEMAND: OnceLock<AllocationDemand> = OnceLock::new();
        DEMAND.get_or_init(|| {
            build_allocation_demand(record_json(), &DemandOptions::default(), &[]).unwrap()
        })
    }

    fn tampered(edit: impl FnOnce(&mut Value)) -> String {
        let mut value: Value = serde_json::from_str(record_json()).unwrap();
        edit(&mut value);
        serde_json::to_string(&value).unwrap()
    }

    fn best_index() -> usize {
        let value: Value = serde_json::from_str(record_json()).unwrap();
        value["best_index"].as_u64().unwrap() as usize
    }

    fn error_text(result: Result<AllocationDemand, RunError>) -> String {
        result.expect_err("expected a refusal").to_string()
    }

    #[test]
    fn offset_zero_gate_equals_the_engine_for_every_tape() {
        let d = demand();
        assert_eq!(d.gate.status, Status::Pass);
        // 70 turns x 2 modules x 1 station.
        assert_eq!(d.gate.tapes_compared, 140);
        assert!(
            d.gate.max_relative_difference <= GATE_RELATIVE_TOLERANCE,
            "gate difference {}",
            d.gate.max_relative_difference
        );
        assert_eq!(d.schema, ALLOCATION_DEMAND_SCHEMA);
        assert_eq!(d.angle_offsets_deg, default_angle_offsets_deg());
        assert_eq!(d.identities.record_sha256, hash(record_json().as_bytes()));
        assert!(!d.identities.map_substituted);
        assert_eq!(d.identities.product_map.dataset_id, MAP_ID);
    }

    #[test]
    fn demand_matches_the_search_screening_at_the_sampled_turns() {
        // The search sampled turns 1 and n at the same stations; with the
        // record's own map the demand there is the screening utilization
        // divided by the utilization limit.
        let record: CoupledSearchRunRecord = serde_json::from_str(record_json()).unwrap();
        let screening = record.candidates[best_index()].screening.as_ref().unwrap();
        let expected = screening.max_utilization.unwrap() / record.case.limits.utilization_limit;
        let d = demand();
        let sampled = [&d.turns[0], d.turns.last().unwrap()];
        let observed = sampled
            .iter()
            .flat_map(|t| &t.modules)
            .map(|m| m.s_req[0].s_req.unwrap())
            .fold(0.0_f64, f64::max);
        assert!(
            (observed - expected).abs() <= 1e-9 * expected,
            "{observed} vs {expected}"
        );
        assert!(observed <= 1.0, "a PASS optimum needs no more than the map");
    }

    #[test]
    fn s_req_is_monotone_non_decreasing_in_the_offset_bound() {
        let d = demand();
        let mut checked = 0;
        for turn in &d.turns {
            for module in &turn.modules {
                let values: Vec<Option<f64>> = module.s_req.iter().map(|e| e.s_req).collect();
                for pair in values.windows(2) {
                    match (pair[0], pair[1]) {
                        (Some(a), Some(b)) => {
                            assert!(b >= a * (1.0 - 1e-12), "{a} then {b}");
                            checked += 1;
                        }
                        // Once blocked at a larger offset it stays blocked.
                        (None, Some(_)) => panic!("blocked then unblocked"),
                        _ => {}
                    }
                }
            }
        }
        assert!(checked > 0);
        // The offset widens the interval, so the requirement must grow
        // somewhere in a case whose angles sit on the tape plane.
        let grows = d.turns.iter().flat_map(|t| &t.modules).any(|m| {
            m.s_req
                .last()
                .unwrap()
                .s_req
                .zip(m.s_req[0].s_req)
                .is_some_and(|(l, f)| l > f)
        });
        assert!(grows, "offsets never raised the requirement");
    }

    #[test]
    fn pancake_case_positions_are_flagged_offset_sensitive() {
        // The axial bore field lies along the tape width of a radial-normal
        // winding, i.e. in the tape plane.
        let d = demand();
        assert!(d.turns.iter().all(|t| t.offset_sensitive));
        assert!(d.summary.offset_sensitive_turns as usize == d.turns.len());
        assert!(is_offset_sensitive(75.1));
        assert!(!is_offset_sensitive(75.0));
        assert!(!is_offset_sensitive(105.0));
        assert!(is_offset_sensitive(104.9));
        assert!(!is_offset_sensitive(0.0));
    }

    #[test]
    fn chunking_across_the_sampling_cap_is_identical_to_other_chunk_sizes() {
        // 70 turns need two chunks at the real cap of 64; chunks of 9 and of 1
        // slice the same case differently.
        let options = DemandOptions::default();
        let capped = demand();
        let small = build_with_chunk(record_json(), &options, &[], 9).unwrap();
        let one_module = build_with_chunk(record_json(), &options, &[], 1).unwrap();
        let a = serde_json::to_value(capped).unwrap();
        assert_eq!(a, serde_json::to_value(&small).unwrap());
        assert_eq!(a, serde_json::to_value(&one_module).unwrap());
    }

    #[test]
    fn turn_lengths_sum_to_the_ledgers_installed_length() {
        let d = demand();
        let total: f64 = d.turns.iter().map(|t| t.length_m).sum();
        assert!((total - d.geometry.stream_length_m).abs() < 1e-9);
        let installed = total * f64::from(d.geometry.modules) * f64::from(d.geometry.strands);
        assert!(
            (installed - d.ledger_check.installed_length_ledger_m).abs() <= 1e-9 * installed.abs()
        );
        // Start coordinates run from 0 and accumulate.
        assert_eq!(d.turns[0].start_m, 0.0);
        for pair in d.turns.windows(2) {
            assert!((pair[1].start_m - (pair[0].start_m + pair[0].length_m)).abs() < 1e-12);
        }
        assert!(d.turns.windows(2).all(|p| p[1].length_m > p[0].length_m));
    }

    #[test]
    fn refuses_a_record_whose_optimum_does_not_pass() {
        let index = best_index();
        let text = tampered(|v| {
            v["candidates"][index]["screening"]["status"] = json!("FAIL");
        });
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("not PASS"), "{message}");
        let text = tampered(|v| v["best_index"] = Value::Null);
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("no PASS optimum"), "{message}");
    }

    #[test]
    fn refuses_unsupported_geometry_with_a_reason() {
        let text = tampered(|v| v["case"]["fixed_geometry"]["tape_normal"] = json!("axial"));
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("radial tape normal"), "{message}");
    }

    #[test]
    fn refuses_inconsistent_width_and_strand_bookkeeping() {
        let index = best_index();
        let text = tampered(|v| v["candidates"][index]["geometry"]["strands_parallel"] = json!(2));
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("bookkeeping"), "{message}");
        let text = tampered(|v| v["candidates"][index]["geometry"]["total_turns"] = json!(139));
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("bookkeeping"), "{message}");
        // Doubling the ledger's installed length breaks the turn-length sum.
        let text = tampered(|v| {
            let installed = v["candidates"][index]["cost"]["installed_length_m"]
                .as_f64()
                .unwrap();
            v["candidates"][index]["cost"]["installed_length_m"] = json!(installed * 2.0);
        });
        let message = error_text(build_allocation_demand(
            &text,
            &DemandOptions::default(),
            &[],
        ));
        assert!(message.contains("ledger"), "{message}");
    }

    #[test]
    fn refuses_bad_offset_lists() {
        for bad in [
            vec![0.0, 1.0, 1.0],
            vec![0.0, -1.0],
            vec![f64::NAN],
            vec![0.0, 50.0],
        ] {
            let options = DemandOptions {
                angle_offsets_deg: bad,
                ..DemandOptions::default()
            };
            assert!(build_allocation_demand(record_json(), &options, &[]).is_err());
        }
        // Zero is always tabulated first.
        assert_eq!(validated_offsets(&[2.0, 4.0]).unwrap(), vec![0.0, 2.0, 4.0]);
    }

    #[test]
    fn map_substitution_needs_the_flag_and_is_recorded() {
        let base = MaterialDataset::embedded_by_id(MAP_ID).unwrap();
        let weak = base.scaled_ic(0.725).unwrap();
        let reference = ProductMapRef {
            dataset_id: weak.metadata.id.clone(),
            csv_sha256: weak.metadata.csv_sha256.clone(),
        };
        let refused = DemandOptions {
            product_map: Some(reference.clone()),
            ..DemandOptions::default()
        };
        let message = error_text(build_allocation_demand(
            record_json(),
            &refused,
            std::slice::from_ref(&weak),
        ));
        assert!(message.contains("--allow-map-substitution"), "{message}");

        let allowed = DemandOptions {
            allow_map_substitution: true,
            ..refused
        };
        let substituted = build_allocation_demand(record_json(), &allowed, &[weak]).unwrap();
        assert!(substituted.identities.map_substituted);
        assert_eq!(substituted.identities.product_map.source, "supplied");
        assert!(
            substituted
                .limitations
                .iter()
                .any(|l| l.contains("differs from the material"))
        );
        assert_eq!(substituted.gate.status, Status::Pass);
        // A map with 0.725 of the capacity needs 1/0.725 of the scale.
        let plain = demand();
        for (a, b) in plain.turns.iter().zip(&substituted.turns) {
            let (x, y) = (
                a.modules[0].s_req[0].s_req.unwrap(),
                b.modules[0].s_req[0].s_req.unwrap(),
            );
            assert!((y - x / 0.725).abs() <= 1e-9 * y, "{x} {y}");
        }
    }

    #[test]
    fn unresolvable_or_mispinned_maps_are_refused() {
        let unknown = DemandOptions {
            product_map: Some(ProductMapRef {
                dataset_id: "no-such-map".into(),
                csv_sha256: "0".repeat(64),
            }),
            allow_map_substitution: true,
            ..DemandOptions::default()
        };
        assert!(build_allocation_demand(record_json(), &unknown, &[]).is_err());
        let mispinned = DemandOptions {
            product_map: Some(ProductMapRef {
                dataset_id: MAP_ID.into(),
                csv_sha256: "0".repeat(64),
            }),
            allow_map_substitution: true,
            ..DemandOptions::default()
        };
        let message = error_text(build_allocation_demand(record_json(), &mispinned, &[]));
        assert!(message.contains("pinned"), "{message}");
    }

    #[test]
    fn limitations_state_every_v1_scope_limit() {
        let text = demand().limitations.join("\n");
        for needle in [
            "One product map",
            "Radial tape normal",
            "Width.",
            "Streams.",
            "Sampling basis",
            "Orientation",
            "15 degrees",
        ] {
            assert!(text.contains(needle), "missing limitation '{needle}'");
        }
    }

    #[test]
    fn angle_samples_cover_the_interval_and_every_integer_degree() {
        assert_eq!(angle_samples(88.3, 0.0), vec![88.3]);
        let samples = angle_samples(88.3, 2.0);
        for expected in [88.3, 86.3, 90.3, 87.0, 88.0, 89.0, 90.0] {
            assert!(samples.contains(&expected), "{expected} in {samples:?}");
        }
        assert_eq!(samples.len(), 7);
    }

    #[test]
    fn positions_outside_the_map_domain_carry_no_value_and_explain_themselves() {
        // The low-field extension map has no estimate at the weakest fields
        // of this winding (about 0.02 to 0.05 T in the tape plane), where
        // the original map is rescued by its low-field clamp.
        let lowfield =
            MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
        let options = DemandOptions {
            product_map: Some(ProductMapRef {
                dataset_id: lowfield.metadata.id.clone(),
                csv_sha256: lowfield.metadata.csv_sha256.clone(),
            }),
            allow_map_substitution: true,
            ..DemandOptions::default()
        };
        let d = build_allocation_demand(record_json(), &options, &[]).unwrap();
        assert_eq!(d.gate.status, Status::Pass);
        let blocked: Vec<&TurnDemand> = d
            .turns
            .iter()
            .filter(|t| t.status != DemandStatus::Ok)
            .collect();
        assert!(!blocked.is_empty());
        assert_eq!(
            d.summary.blocked_positions as usize,
            blocked.iter().map(|t| t.modules.len()).sum::<usize>()
        );
        for turn in &blocked {
            assert_eq!(turn.status, DemandStatus::OutsideMapDomain);
            for module in &turn.modules {
                for entry in &module.s_req {
                    assert!(entry.s_req.is_none());
                    assert_eq!(entry.status, DemandStatus::OutsideMapDomain);
                    assert!(
                        entry
                            .explanation
                            .as_deref()
                            .is_some_and(|e| e.contains("no estimate"))
                    );
                    assert!(entry.next_step.as_deref().is_some_and(|e| !e.is_empty()));
                }
            }
        }
        // Positions the map does cover keep their values.
        let ok = d
            .turns
            .iter()
            .find(|t| t.status == DemandStatus::Ok)
            .unwrap();
        assert!(
            ok.modules
                .iter()
                .all(|m| m.s_req.iter().all(|e| e.s_req.is_some()))
        );
    }
}
