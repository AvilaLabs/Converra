//! Headless OC-004 runner: couples a homogenized racetrack pack field
//! (`optcoil_physics::racetrack`) to the measured SuperPower bridge law
//! (`optcoil_physics::critical_current` via `optcoil_physics::tape_frame`)
//! under the declared assumptions of the OC-004 contract. This produces a
//! screening current allowance under stated caveats, never a production
//! operating-current limit; `conductor_qualification_status` is always
//! INCONCLUSIVE and `engineering_status` is always NOT_EVALUATED here.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::time::{Instant, SystemTime, UNIX_EPOCH};

use optcoil_model::{
    Check, Status,
    coupled::{
        CoupledCase, FieldMapComponents, MaterialSettings, ResolvedFieldMap, Station, TapeFrame,
        tape_center_position_m, width_offset_m,
    },
    magnetics::{CurrentModel, Racetrack},
    material::MaterialDataset,
};
use optcoil_physics::{
    critical_current::{
        IcInterpolationMethod, IcInterpolator, IcMeshSummary, LOG_IC_MODEL_ID, MeasurementWeight,
    },
    racetrack::{FIELD_MODEL_ID, MU0_H_PER_M, RacetrackEvaluator, norm, sub},
    tape_frame,
};
use serde::{Deserialize, Serialize};

use crate::{RunError, field::RuntimeInfo, field::runtime_info, hash, write_json_new};

/// Bumped to v6: `critical_state_strip` now solves the field-consistent
/// edge-field bound — the largest fixed point of
/// `B_edge = (μ0/2π)·k_bound(|B_app|+B_edge)·ln(w/d)` where `k_bound` is
/// the suffix-max bounding table over every measured angle level — not the
/// strict low-field-floor value (v5, perpendicular-sector floor). Still a
/// bound at any transport ratio; strictly tighter where the edge zone's
/// converged field sits above the floor (OC-014 Phase 2b).
/// Bumped to v7 (coupled-conductor schema v6): material bindings are
/// resolved per turn — each winding region's spec carries its own
/// interpolator, monotonicity audit, low-field clamp and critical-state
/// table, so graded packs screen each turn under its own declared tape.
pub const COUPLED_MODEL_ID: &str = "coupled-racetrack-pack-bridge-law-screening/v7";
pub const COUPLED_CHECKER_ID: &str =
    "coupled-refinement-reference-and-declared-margin-screening/v3";
pub const COUPLED_REFERENCE_SCHEMA: &str = "optcoil-coupled-reference/v1";
pub const COUPLED_REFERENCE_METHOD: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic/v1";
/// OC-014: method id `reference_oc004.py` emits for coupled-conductor v2
/// cases declaring `limits.self_field_correction` — the reference applies
/// the same uniform-transport bound to its query magnitude.
pub const COUPLED_REFERENCE_METHOD_V2: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic+uniform-transport-self-field/v2";
/// OC-017: method ids for coupled-conductor v3 cases declaring
/// `limits.along_current_model = "transverse_bound"` — without and with
/// the v2 self-field correction stacked.
pub const COUPLED_REFERENCE_METHOD_ALONG_CURRENT: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic+along-current-transverse-bound/v1";
pub const COUPLED_REFERENCE_METHOD_V3: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic+uniform-transport-self-field/v2+along-current-transverse-bound/v1";
/// OC-014 Phase 2: method ids `reference_oc004.py` emits for coupled-
/// conductor v4 cases declaring `self_field_correction =
/// "critical_state_strip"` — the reference applies the same
/// critical-state edge-field bound to its query magnitude. Method /v3:
/// the field-consistent fixed-point bound over the suffix-max
/// all-angle table, matching the v6 model semantics.
pub const COUPLED_REFERENCE_METHOD_CRITICAL_STATE: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic+critical-state-strip-edge-self-field/v3";
pub const COUPLED_REFERENCE_METHOD_CRITICAL_STATE_ALONG_CURRENT: &str = "analytical-z-integral-planar-duffy-gauss-graded+independent-measured-coordinate-tetrahedral-log-ic+critical-state-strip-edge-self-field/v3+along-current-transverse-bound/v1";
pub const OC004_REFERENCE_JSON: &str =
    include_str!("../../../benchmarks/coupled/oc-004.reference.json");
/// Frozen Python reference for the OC-014 critical-state parity fixture —
/// the first dedicated artifact exercising `critical_state_strip`
/// (field-consistent bound) on the modelext dataset.
pub const OC014_REFERENCE_JSON: &str =
    include_str!("../../../benchmarks/coupled/oc-014.reference.json");
const REFERENCE_OC004_SOURCE: &str = include_str!("../../../tools/reference_oc004.py");
const REFERENCE_OC002_SOURCE: &str = include_str!("../../../tools/reference_oc002.py");

/// Runtime override of the case's own declared quadrature orders (`None`
/// keeps `case.numerics.quadrature_orders` as frozen).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledOptions {
    pub orders: Option<[u32; 2]>,
}

/// The basis on which one width point (or one mirror angle) was determined.
/// `AlongCurrentExcluded` only ever appears at the combined-point level (an
/// individual mirror-angle query is never itself along-current excluded).
/// `AlongCurrentBounded` (OC-017, run record v3): the point's along-current
/// fraction exceeded `max_along_current_field_fraction` but sat within the
/// `transverse_bound` model's ceiling, so the measured law was queried at
/// full magnitude and the transverse-plane angle under the declared
/// assumption — a bounded estimate, labeled, not a clean measured-plane one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointBasis {
    Estimate,
    LowerBound,
    Unsupported,
    AlongCurrentExcluded,
    AlongCurrentBounded,
}

// ---------------------------------------------------------------------
// Reference artifact (`optcoil-coupled-reference/v1`, contract §3)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledReference {
    pub schema: String,
    pub case_sha256: String,
    pub method: String,
    pub source_sha256: String,
    pub field_source_sha256: String,
    pub source_path: String,
    pub csv_sha256: String,
    pub python_version: String,
    pub numpy_version: String,
    pub platform: String,
    pub orders: [u32; 2],
    pub component_subdivisions: u32,
    pub mu0_h_per_m: f64,
    pub evaluated_current_a: f64,
    pub points: Vec<ReferencePoint>,
    pub elapsed_seconds: f64,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePoint {
    pub station: String,
    pub tape_index: u32,
    pub turn_index: u32,
    pub width_index: u32,
    pub position_m: [f64; 3],
    pub unit_fields_t_per_ampere_turn: [[f64; 3]; 2],
    pub field_t: [f64; 3],
    pub magnitude_t: f64,
    pub angle_raw_deg: f64,
    pub angle_folded_deg: f64,
    pub mirror_angle_deg: f64,
    pub along_current_fraction: f64,
    pub query_field_t: f64,
    pub basis: PointBasis,
    pub k_folded_a_per_m: Option<f64>,
    pub k_mirror_a_per_m: Option<f64>,
    pub k_used_a_per_m: Option<f64>,
}

// ---------------------------------------------------------------------
// Run record (`optcoil-coupled-run/v8`, contract §4). v2 added
// `transport_self_field_ratio` (per point) and
// `max_transport_self_field_ratio` (per candidate) — OC-014; both are
// serde-defaulted Options, so v1 records still deserialize. v3 adds the
// `along_current_bounded` point basis and its counter (OC-017
// `transverse_bound` model) — serde-defaulted, so v1/v2 records still
// deserialize. v4 adds `critical_state_edge_field_t` (per point) — the
// OC-014 Phase-2 `critical_state_strip` bound — serde-defaulted, so
// v1–v3 records still deserialize. v5 adds no fields: the `critical_state
// _strip` floor is resolved over a near-perpendicular angle sector, so
// deep-null verdicts (previously `cs_unboundable` → unsupported) differ
// by construction (OC-014 Phase 2, perpendicular-angle fallback). v6 adds
// no fields: `critical_state_edge_field_t` is now the field-consistent
// fixed-point bound over the suffix-max all-angle table — strictly
// tighter than v5's strict floor value, so verdicts differ by
// construction (OC-014 Phase 2b). v8 adds `field_map` (per record) —
// the provenance of a case-declared external field map (coupled-case
// schema v7) — serde-defaulted, so earlier records still deserialize.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetIdentity {
    pub id: String,
    pub csv_sha256: String,
    pub point_count: usize,
    pub mesh: IcMeshSummary,
    /// The dataset's own characterization provenance: which specimen was
    /// measured, the patterned bridge width the data actually covers,
    /// and the tape width that specimen was cut from. Surfaced so the
    /// record carries the bridge-to-full-width applicability domain —
    /// `None` on records written before these fields existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_bridge_width_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_tape_width_m: Option<f64>,
    /// The dataset's declared evidence class — `measured`,
    /// `measured_with_model_extension` or `published_model_fit` — so a
    /// record shows whether a verdict leaned on model-generated nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<optcoil_model::material::MaterialDataClass>,
}

impl DatasetIdentity {
    pub(crate) fn new(
        dataset: &optcoil_model::material::MaterialDataset,
        mesh: IcMeshSummary,
    ) -> Self {
        Self {
            id: dataset.metadata.id.clone(),
            csv_sha256: dataset.metadata.csv_sha256.clone(),
            point_count: dataset.points.len(),
            mesh,
            sample_id: Some(dataset.metadata.sample_id.clone()),
            measured_bridge_width_m: Some(dataset.metadata.measured_bridge_width_m),
            original_tape_width_m: Some(dataset.metadata.original_tape_width_m),
            data_class: Some(dataset.metadata.data_class),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonotonicityAuditRecord {
    pub checked_pairs: u64,
    pub violations: u64,
    pub worst_relative_increase: f64,
    pub tolerance: f64,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidthRule {
    pub nodes: [f64; 5],
    pub weights: [f64; 5],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FrameRecord {
    pub t: [f64; 3],
    pub n: [f64; 3],
    pub w: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeometryWidthPoint {
    pub width_index: u32,
    pub offset_m: f64,
    pub position_m: [f64; 3],
    /// Unit-ampere-turn field vectors, one per `case.numerics.quadrature_orders`.
    pub unit_field_t_per_ampere_turn: [[f64; 3]; 2],
    pub refinement_change_t_per_ampere_turn: f64,
}

/// Run-record provenance for a case-declared external field map
/// (coupled-case schema v7). Present iff `case.sampling.field_map` is
/// declared; the map's fields are customer-declared values scaled per
/// ampere-turn, not engine-computed — `kernel_evaluations` reports 0
/// and every per-point `refinement_change_t_per_ampere_turn` is 0 by
/// construction (the map's own mesh is the resolution; there is no
/// in-map detail to refine toward).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMapRecord {
    pub source_sha256: String,
    pub components: FieldMapComponents,
    pub reference_ampere_turns_a: f64,
    /// Cylindrical-grid node counts — present iff `components` is
    /// `cylindrical_br_bz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rho_nodes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_nodes: Option<usize>,
    /// Cartesian-grid node counts — present iff `components` is
    /// `cartesian_bx_by_bz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_nodes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_nodes: Option<usize>,
}

impl FieldMapRecord {
    /// Provenance summary of a declared map: component tag plus the
    /// node counts on whichever axes the grid declares.
    pub(crate) fn from_map(map: &optcoil_model::coupled::FieldMap) -> Self {
        use optcoil_model::coupled::FieldMap;
        let (rho_nodes, z_nodes, x_nodes, y_nodes) = match map {
            FieldMap::CylindricalBrBz {
                rho_levels_m,
                z_levels_m,
                ..
            } => (Some(rho_levels_m.len()), Some(z_levels_m.len()), None, None),
            FieldMap::CartesianBxByBz {
                x_levels_m,
                y_levels_m,
                z_levels_m,
                ..
            } => (
                None,
                Some(z_levels_m.len()),
                Some(x_levels_m.len()),
                Some(y_levels_m.len()),
            ),
        };
        FieldMapRecord {
            source_sha256: map.source_sha256().to_owned(),
            components: map.components(),
            reference_ampere_turns_a: map.reference_ampere_turns_a(),
            rho_nodes,
            z_nodes,
            x_nodes,
            y_nodes,
        }
    }

    /// The field-model identity this map asserts in the record.
    pub(crate) fn field_model_id(&self) -> &'static str {
        match self.components {
            FieldMapComponents::CylindricalBrBz => {
                "customer-declared-field-map/cylindrical-br-bz/v1"
            }
            FieldMapComponents::CartesianBxByBz => {
                "customer-declared-field-map/cartesian-bx-by-bz/v1"
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AngleResult {
    pub basis: PointBasis,
    pub query_field_t: f64,
    /// The angle representation actually queried (the requested angle or
    /// its period-180 equivalent); see `tape_frame::AngleQuery`.
    pub angle_query_deg: f64,
    pub k_a_per_m: Option<f64>,
    pub support: Vec<MeasurementWeight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateWidthPoint {
    pub width_index: u32,
    pub field_t: [f64; 3],
    pub magnitude_t: f64,
    pub angle_raw_deg: f64,
    pub angle_folded_deg: f64,
    pub mirror_angle_deg: f64,
    pub along_current_fraction: f64,
    pub query_field_t: f64,
    pub basis: PointBasis,
    pub k_folded: AngleResult,
    pub k_mirror: AngleResult,
    pub k_used_a_per_m: Option<f64>,
    pub self_field_ratio: Option<f64>,
    /// OC-014: `μ0·K_tape/(2·|B_applied|)` for the *carried* sheet density
    /// `K_tape = current_a/(tape_width·strands)` — the self-field dominance
    /// measure used by the v5 `uniform_transport` gate (valid bound only
    /// while this ratio ≤ 1). Recorded on every evaluated point so the
    /// record shows where the correction stops being a bound.
    #[serde(default)]
    pub transport_self_field_ratio: Option<f64>,
    /// OC-014 Phase 2 (`critical_state_strip`, run v4): the critical-state
    /// edge self-field bound added to the applied field for the query —
    /// `(μ0/2π)·K_c_floor·ln(w/d)`. `Some` on every evaluated point under
    /// the model so the record shows exactly what self-field was applied;
    /// `None` when the floor sheet current did not resolve (point then
    /// unsupported) or under other correction models.
    #[serde(default)]
    pub critical_state_edge_field_t: Option<f64>,
    pub allowed_screening_a: Option<f64>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeCandidateResult {
    pub current_a: f64,
    pub points: Vec<CandidateWidthPoint>,
    pub i_min_a: Option<f64>,
    pub i_strip_a: Option<f64>,
    pub allowed_screening_a: Option<f64>,
    pub utilization: Option<f64>,
    pub limiting_width_index: Option<u32>,
    pub own_tape_edge_field_scale_t: Option<f64>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeResult {
    pub station: String,
    pub tape_index: u32,
    pub turn_index: u32,
    pub center_position_m: [f64; 3],
    pub frame: FrameRecord,
    pub points: Vec<GeometryWidthPoint>,
    pub per_candidate: Vec<TapeCandidateResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationGroup {
    pub station: String,
    pub tapes: Vec<TapeResult>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PointCounts {
    pub estimate: u64,
    pub lower_bound: u64,
    pub unsupported: u64,
    pub along_current_excluded: u64,
    /// OC-017 (run record v3): points determined via the `transverse_bound`
    /// along-current model — bounded estimates, not clean measured-plane
    /// ones. Absent on records from before the model existed.
    #[serde(default)]
    pub along_current_bounded: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitingPoint {
    pub station: String,
    pub tape_index: u32,
    pub turn_index: u32,
    pub width_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateResult {
    pub current_a: f64,
    pub ampere_turns_a: f64,
    pub status: Status,
    pub limiting: Option<LimitingPoint>,
    pub min_allowed_screening_a: Option<f64>,
    pub max_utilization: Option<f64>,
    pub point_counts: PointCounts,
    pub max_self_field_ratio: f64,
    pub limiting_self_field_ratio: Option<f64>,
    /// OC-014 (run record v2): maximum `transport_self_field_ratio` over
    /// all evaluated points for this candidate. Under
    /// `limits.self_field_correction = "uniform_transport"` a value above
    /// 1 gates the candidate to INCONCLUSIVE (self-field-dominated regime:
    /// the uniform-transport bound is no longer a bound). Absent on v1
    /// records.
    #[serde(default)]
    pub max_transport_self_field_ratio: Option<f64>,
    pub max_along_current_fraction: f64,
    pub max_refinement_change_t: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceComparison {
    pub max_field_error_t: f64,
    pub max_angle_error_deg: f64,
    pub max_capacity_relative_error: f64,
    pub max_reference_refinement_t: f64,
    pub compared_points: usize,
    /// Number of reference points whose Rust basis is determined (`Estimate`
    /// or `LowerBound`) that actually contributed a capacity comparison
    /// (both Rust and reference `k_used_a_per_m` present and finite). Must
    /// equal the number of determined points in the subset, or
    /// `capacity_missing` is set and the check fails (contract §9.1).
    pub compared_capacity_points: usize,
    pub capacity_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoupledRunRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub field_model_id: String,
    pub ic_model_id: String,
    pub coupled_model_id: String,
    pub checker_id: String,
    pub case_sha256: String,
    pub reference_sha256: Option<String>,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    pub case: CoupledCase,
    pub dataset: DatasetIdentity,
    /// The base binding's dataset audit — the case-level `material`.
    /// Graded (schema v6) cases additionally record each spec's own audit
    /// in `spec_monotonicity_audits`.
    pub monotonicity_audit: MonotonicityAuditRecord,
    /// Schema v6 only: each `tape_specs` binding's resolved dataset
    /// identity and its own monotonicity audit, keyed by spec id (the
    /// base binding keeps `dataset`/`monotonicity_audit`). Empty — and
    /// absent from the serialized record — on every earlier case.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spec_datasets: BTreeMap<String, DatasetIdentity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spec_monotonicity_audits: BTreeMap<String, MonotonicityAuditRecord>,
    pub total_turns: u64,
    pub pitch_n_m: f64,
    pub pitch_w_m: f64,
    pub width_rule: WidthRule,
    pub kernel_evaluations: u64,
    pub field_timing_ms: f64,
    /// Coupled-case schema v7 only: provenance of the declared external
    /// field map this run evaluated against — absent on every
    /// engine-field run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_map: Option<FieldMapRecord>,
    pub stations: Vec<StationGroup>,
    pub candidates: Vec<CandidateResult>,
    pub reference: Option<CoupledReference>,
    pub reference_comparison: Option<ReferenceComparison>,
    pub numerical_status: Status,
    pub coverage_status: Status,
    pub conductor_qualification_status: Status,
    pub engineering_status: Status,
    pub checks: Vec<Check>,
    pub limitations: Vec<String>,
}

impl CoupledRunRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_oc004(options: &CoupledOptions) -> Result<CoupledRunRecord, RunError> {
    run_coupled_case(
        optcoil_model::coupled::OC004_JSON,
        Some(OC004_REFERENCE_JSON),
        options,
    )
}

/// OC-014 Phase-2 parity fixture: OC-004's geometry and sampling plan under
/// the modelext dataset with `critical_state_strip` — runs the case against
/// its frozen Python reference so the critical-state bound is covered by
/// Rust↔Python parity, not only by shared logic.
pub fn run_oc014(options: &CoupledOptions) -> Result<CoupledRunRecord, RunError> {
    run_coupled_case(
        optcoil_model::coupled::OC014_JSON,
        Some(OC014_REFERENCE_JSON),
        options,
    )
}

/// One material binding's screening state (coupled-conductor schema v6):
/// the interpolator over the binding's *own* dataset, that dataset's
/// monotonicity audit under the binding's own tolerance, and — when the
/// case declares `critical_state_strip` — the binding's bounding table
/// built under its own `low_field_clamp_t`. A graded pack's regions can
/// legitimately mix specs whose clamps and datasets differ; nothing here
/// is shared across bindings.
pub(crate) struct SpecRuntime {
    /// The resolved binding — `MaterialSettings` the point evaluator
    /// reads (`low_field_clamp_t` today; the whole binding travels so the
    /// per-turn choice is never ambiguous).
    pub material: MaterialSettings,
    pub interpolator: IcInterpolator,
    /// This binding's dataset's monotonicity audit — LowerBound points
    /// are demoted to Unsupported per-binding, gated by this audit alone.
    pub monotonicity: tape_frame::MonotonicityAudit,
    pub monotonicity_ok: bool,
    pub critical_state_table: Option<CriticalStateFieldTable>,
}

/// Every material binding a run can evaluate: the case-level `material`
/// (`base`) plus one entry per `tape_specs` id the winding's regions can
/// resolve to. `datasets` keeps the resolved dataset per declared
/// `dataset_id` for record provenance and for generated-case reruns.
pub(crate) struct MaterialRuntimes {
    pub base: SpecRuntime,
    /// Keyed by `tape_specs` id — the same string
    /// `Winding::spec_for_turn` returns for a covered turn.
    pub specs: BTreeMap<String, SpecRuntime>,
    /// `metadata.id` → resolved dataset for every binding the case
    /// declares (base and specs alike; two bindings may share one).
    pub datasets: BTreeMap<String, MaterialDataset>,
}

impl MaterialRuntimes {
    /// The binding a 1-based `turn_index` of `case.winding` screens
    /// under: the covering region's spec runtime, or `base` when no
    /// region covers it (every turn on an ungraded case).
    pub(crate) fn for_turn(&self, case: &CoupledCase, turn_index: u32) -> &SpecRuntime {
        match case.winding.spec_for_turn(turn_index) {
            Some(id) => self.specs.get(id).unwrap_or(&self.base),
            None => &self.base,
        }
    }
}

/// Resolve the dataset each unique `dataset_id` names: a supplied
/// dataset whose `metadata.id` matches wins (the declared `csv_sha256`
/// is then enforced by validation), otherwise the embedded registry.
/// `Err` only when a declared id resolves nowhere — never a silent
/// substitute.
pub(crate) fn resolve_datasets<'a>(
    materials: impl IntoIterator<Item = &'a MaterialSettings>,
    supplied: &[MaterialDataset],
) -> Result<BTreeMap<String, MaterialDataset>, RunError> {
    let mut map = BTreeMap::new();
    for material in materials {
        if map.contains_key(&material.dataset_id) {
            continue;
        }
        let dataset = match supplied
            .iter()
            .find(|d| d.metadata.id == material.dataset_id)
        {
            Some(d) => d.clone(),
            None => MaterialDataset::embedded_by_id(&material.dataset_id)?,
        };
        map.insert(material.dataset_id.clone(), dataset);
    }
    Ok(map)
}

/// Resolve each unique declared binding from an explicitly supplied map
/// keyed by the dataset's actual `metadata.id`, or from the embedded
/// registry. A map key that disagrees with metadata is invalid, and no
/// dataset is substituted for a missing declared id.
pub(crate) fn resolve_datasets_map<'a>(
    materials: impl IntoIterator<Item = &'a MaterialSettings>,
    supplied: &BTreeMap<String, MaterialDataset>,
) -> Result<BTreeMap<String, MaterialDataset>, RunError> {
    for (key, dataset) in supplied {
        if key != &dataset.metadata.id {
            return Err(RunError::Invalid(format!(
                "supplied dataset map key '{}' does not match metadata id '{}'",
                key, dataset.metadata.id
            )));
        }
    }
    let mut map = BTreeMap::new();
    for material in materials {
        if map.contains_key(&material.dataset_id) {
            continue;
        }
        let dataset = match supplied.get(&material.dataset_id) {
            Some(d) => d.clone(),
            None => MaterialDataset::embedded_by_id(&material.dataset_id)?,
        };
        map.insert(material.dataset_id.clone(), dataset);
    }
    Ok(map)
}

/// Build one binding's screening state: interpolator over its resolved
/// dataset, that dataset's monotonicity audit under the binding's own
/// tolerance, and the binding's critical-state table (under its own
/// low-field clamp) when the case declares `critical_state_strip`.
pub(crate) fn spec_runtime(
    material: &MaterialSettings,
    dataset: &MaterialDataset,
    temperature_k: f64,
    critical_state_strip: bool,
) -> Result<SpecRuntime, RunError> {
    let method = IcInterpolationMethod::from_id(&material.method)
        .ok_or_else(|| RunError::Invalid("unsupported material interpolation method".into()))?;
    // The declared angular seam assumption (period-180 fold stitched)
    // rides the binding verbatim: seam cells interpolate between the
    // measured points on both sides of the fold, never past them.
    let interpolator = IcInterpolator::with_method_and_seam(
        &dataset.points,
        dataset.metadata.max_cell_spans,
        method,
        material.angle_mapping.seam_period_deg(),
    )?;
    let monotonicity =
        tape_frame::audit_monotonicity(&dataset.points, material.monotonicity_tolerance);
    let monotonicity_ok = monotonicity.status == Status::Pass;
    let critical_state_table = if critical_state_strip {
        build_critical_state_field_table(&interpolator, temperature_k, material.low_field_clamp_t)
    } else {
        None
    };
    Ok(SpecRuntime {
        material: material.clone(),
        interpolator,
        monotonicity,
        monotonicity_ok,
        critical_state_table,
    })
}

/// Missing or stale reference evidence is rejected before any evaluation
/// (stale/mismatched) or degrades numerical_status to INCONCLUSIVE (missing).
/// This never changes `conductor_qualification_status` (always INCONCLUSIVE)
/// or `engineering_status` (always NOT_EVALUATED).
pub fn run_coupled_case(
    case_json: &str,
    reference_json: Option<&str>,
    options: &CoupledOptions,
) -> Result<CoupledRunRecord, RunError> {
    run_coupled_case_with_datasets(case_json, reference_json, options, &[])
}

/// Dataset-supplying form of [`run_coupled_case`]: `dataset`, when `Some`,
/// replaces the embedded lookup — the case's declared `dataset_id` /
/// `csv_sha256` are still checked against it, so a supplied dataset that
/// doesn't match the declared identity is rejected.
pub fn run_coupled_case_with_dataset(
    case_json: &str,
    reference_json: Option<&str>,
    options: &CoupledOptions,
    dataset: Option<&MaterialDataset>,
) -> Result<CoupledRunRecord, RunError> {
    let datasets: Vec<&MaterialDataset> = dataset.into_iter().collect();
    run_coupled_case_with_datasets(case_json, reference_json, options, &datasets)
}

/// Multi-dataset form of [`run_coupled_case_with_dataset`] (coupled-
/// conductor schema v6): a graded case's bindings may name several
/// datasets, so the customer-data path supplies a slice; each binding's
/// declared `dataset_id` is matched against the supplied datasets first,
/// then the embedded registry, and every supplied dataset must be claimed
/// by some binding — a stray supply is rejected, never silently ignored.
pub fn run_coupled_case_with_datasets(
    case_json: &str,
    reference_json: Option<&str>,
    options: &CoupledOptions,
    datasets: &[&MaterialDataset],
) -> Result<CoupledRunRecord, RunError> {
    run_coupled_case_with_datasets_ticked(case_json, reference_json, options, datasets, None)
}

/// Progress-ticking form of [`run_coupled_case_with_datasets`]: `tick`,
/// when `Some`, is incremented once per field evaluation (evaluator
/// call or declared-map lookup) so a caller can display live progress.
/// Display-only — never read back into the record.
pub(crate) fn run_coupled_case_with_datasets_ticked(
    case_json: &str,
    reference_json: Option<&str>,
    options: &CoupledOptions,
    datasets: &[&MaterialDataset],
    // Progress tick: incremented once per field evaluation.
    tick: Option<&AtomicU64>,
) -> Result<CoupledRunRecord, RunError> {
    run_coupled_case_with_datasets_ticked_cancellable(
        case_json,
        reference_json,
        options,
        datasets,
        tick,
        &AtomicBool::new(false),
    )
}

/// Cancellation-aware form of [`run_coupled_case_with_datasets_ticked`].
/// Checks the shared flag before every bounded field evaluation/map lookup;
/// a cancelled run returns no partial record.
pub(crate) fn run_coupled_case_with_datasets_ticked_cancellable(
    case_json: &str,
    reference_json: Option<&str>,
    options: &CoupledOptions,
    datasets: &[&MaterialDataset],
    tick: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> Result<CoupledRunRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;

    let case = CoupledCase::from_json(case_json)?;
    // Routed by each binding's own declared dataset_id rather than a
    // fixed embedded() call: a supplied dataset matching a declared id
    // wins, otherwise the embedded registry resolves it. A supplied
    // dataset no binding declares is rejected outright — a stray supply
    // must never silently fall back to embedded bytes.
    let bindings = case.material_bindings();
    for supplied in datasets {
        if !bindings
            .iter()
            .any(|(_, m)| m.dataset_id == supplied.metadata.id)
        {
            return Err(RunError::Invalid(format!(
                "supplied dataset '{}' is not declared by any material binding",
                supplied.metadata.id
            )));
        }
    }
    let supplied_owned: Vec<MaterialDataset> = datasets.iter().map(|d| (*d).clone()).collect();
    let resolved = resolve_datasets(bindings.iter().map(|&(_, m)| m), &supplied_owned)?;
    case.validate_against_dataset_map(&resolved)?;
    let critical_state_strip =
        case.limits.self_field_correction.as_deref() == Some("critical_state_strip");
    let runtimes = {
        let base = spec_runtime(
            &case.material,
            &resolved[&case.material.dataset_id],
            case.operating.temperature_k,
            critical_state_strip,
        )?;
        let mut specs = BTreeMap::new();
        if let Some(tape_specs) = &case.tape_specs {
            for (id, material) in tape_specs {
                let dataset = resolved.get(&material.dataset_id).ok_or_else(|| {
                    RunError::Invalid(format!(
                        "no dataset resolves tape_specs['{id}'] declared dataset_id '{}'",
                        material.dataset_id
                    ))
                })?;
                specs.insert(
                    id.clone(),
                    spec_runtime(
                        material,
                        dataset,
                        case.operating.temperature_k,
                        critical_state_strip,
                    )?,
                );
            }
        }
        MaterialRuntimes {
            base,
            specs,
            datasets: resolved,
        }
    };
    // The base binding's dataset stays the record's `dataset` identity —
    // spec bindings' resolved datasets are recorded in `spec_datasets`.
    let dataset = &runtimes.datasets[&case.material.dataset_id];

    let orders = options.orders.unwrap_or(case.numerics.quadrature_orders);
    if orders[0] >= orders[1] || orders.iter().any(|o| !(2..=24).contains(o)) {
        return Err(RunError::Invalid(
            "coupled evaluation requires two strictly increasing quadrature orders in 2..=24"
                .into(),
        ));
    }

    let case_sha256 = hash(case_json.as_bytes());
    let reference: Option<CoupledReference> =
        reference_json.map(serde_json::from_str).transpose()?;
    let expected_subset = expand_reference_subset(&case)?;
    if let Some(reference) = &reference {
        validate_reference(reference, &case, &case_sha256, dataset, &expected_subset)?;
    }

    // The base binding's mesh summary feeds the record's dataset
    // identity; per-binding interpolators/audits/tables live in
    // `runtimes`, resolved per turn inside the sampling loop.
    let mesh = runtimes.base.interpolator.summary().clone();

    // Coupled-case schema v7: a declared external field map replaces the
    // engine's field solve entirely — no evaluators are built and
    // `kernel_evaluations` stays 0. Case validation has already confined
    // the pack's sampled domain to the map's nearest-node hull; the
    // lookup below re-checks each query and fails closed.
    let field_map: Option<ResolvedFieldMap> = case
        .sampling
        .field_map
        .as_ref()
        .map(|m| m.resolve())
        .transpose()?;

    let evaluators = if field_map.is_some() {
        Vec::new()
    } else {
        match &case.pack.path {
            // Coupled-conductor schema v5: a general planar centerline. Cells
            // are generated per path segment — no Racetrack adapter needed.
            Some(path) => orders
                .iter()
                .map(|&order| {
                    RacetrackEvaluator::from_path(
                        path,
                        case.pack.radial_width_m,
                        case.pack.axial_height_m,
                        1.0,
                        order,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => {
                let geometry = Racetrack {
                    straight_half_length_m: case.pack.straight_half_length_m.ok_or_else(|| {
                        RunError::Invalid(
                            "pack declares neither path nor racetrack dimensions".to_owned(),
                        )
                    })?,
                    bend_radius_m: case.pack.bend_radius_m.ok_or_else(|| {
                        RunError::Invalid(
                            "pack declares neither path nor racetrack dimensions".to_owned(),
                        )
                    })?,
                    radial_width_m: case.pack.radial_width_m,
                    axial_height_m: case.pack.axial_height_m,
                    ampere_turns_a: 1.0,
                    current_model: CurrentModel::UniformWindingPack,
                };
                orders
                    .iter()
                    .map(|&order| RacetrackEvaluator::new(&geometry, order))
                    .collect::<Result<Vec<_>, _>>()?
            }
        }
    };

    let total_turns = case.winding.total_turns();
    let pitch_n_m = case.winding.pitch_n_m(&case.pack);
    let pitch_w_m = case.winding.pitch_w_m(&case.pack);
    let nodes = tape_frame::lobatto_nodes();
    let weights = tape_frame::LOBATTO_WEIGHTS;
    let budget = case.limits.interpolation_overprediction_budget;
    let num_candidates = case.operating.current_candidates_a.len();

    let mut kernel_evaluations = 0_u64;
    let mut field_timing_ms = 0.0_f64;
    let mut accumulators: Vec<CandidateAccumulator> = (0..num_candidates)
        .map(|_| CandidateAccumulator::default())
        .collect();
    let mut stations_out = Vec::with_capacity(case.sampling.stations.len());

    for station in &case.sampling.stations {
        let frame = station.frame(case.winding.tape_normal, case.pack.centerline())?;
        let frame_record = FrameRecord {
            t: frame.t,
            n: frame.n,
            w: frame.w,
        };
        let mut tapes = Vec::new();
        for &tape_index in &case.sampling.tape_indices_along_width {
            for &turn_index in &case.sampling.turn_indices_along_normal {
                let center_position_m = tape_center_position_m(
                    &case.pack,
                    &case.winding,
                    station,
                    tape_index,
                    turn_index,
                )?;
                let mut geometry_points = Vec::with_capacity(nodes.len());
                let mut unit_fields = Vec::with_capacity(nodes.len());
                for (width_index, &xi) in nodes.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(RunError::Cancelled);
                    }
                    let offset_m = width_offset_m(case.winding.tape_width_m, xi);
                    let position_m = add(center_position_m, scale(frame.w, offset_m));
                    let mut unit_here = [[0.0_f64; 3]; 2];
                    match &field_map {
                        Some(map) => {
                            // Declared map: the lab point's cylindrical
                            // components, per ampere-turn at the map's
                            // reference winding. Both quadrature slots
                            // carry the declared value — refinement
                            // change is 0 by construction (the map's own
                            // mesh is the field resolution).
                            let unit = map
                                .unit_field_lab_t_per_ampere_turn(position_m)
                                .map_err(RunError::Model)?;
                            if let Some(t) = tick {
                                t.fetch_add(1, Ordering::Relaxed);
                            }
                            unit_here = [unit, unit];
                        }
                        None => {
                            for (order_index, evaluator) in evaluators.iter().enumerate() {
                                let clock = Instant::now();
                                let value = evaluator.evaluate(position_m)?;
                                if let Some(t) = tick {
                                    t.fetch_add(1, Ordering::Relaxed);
                                }
                                field_timing_ms += clock.elapsed().as_secs_f64() * 1000.0;
                                kernel_evaluations += value.kernel_evaluations;
                                unit_here[order_index] = value.field_t;
                            }
                        }
                    }
                    unit_fields.push(unit_here);
                    geometry_points.push(GeometryWidthPoint {
                        width_index: width_index as u32,
                        offset_m,
                        position_m,
                        unit_field_t_per_ampere_turn: unit_here,
                        refinement_change_t_per_ampere_turn: norm(sub(unit_here[1], unit_here[0])),
                    });
                }

                // Graded packs (schema v6): the turn's covering region
                // decides which binding's interpolator, monotonicity audit
                // and critical-state table screen this point.
                let runtime = runtimes.for_turn(&case, turn_index);
                let mut per_candidate = Vec::with_capacity(num_candidates);
                for (candidate_index, &current_a) in
                    case.operating.current_candidates_a.iter().enumerate()
                {
                    let ampere_turns_a = total_turns as f64 * current_a;
                    let mut candidate_points = Vec::with_capacity(nodes.len());
                    for (width_index, unit_here) in unit_fields.iter().enumerate() {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(RunError::Cancelled);
                        }
                        let point = evaluate_candidate_point(
                            &runtime.interpolator,
                            &case,
                            &runtime.material,
                            runtime.monotonicity_ok,
                            &frame,
                            width_index as u32,
                            unit_here[1],
                            current_a,
                            ampere_turns_a,
                            runtime.critical_state_table.as_ref(),
                        )?;
                        let accumulator = &mut accumulators[candidate_index];
                        match point.basis {
                            PointBasis::Estimate => accumulator.point_counts.estimate += 1,
                            PointBasis::LowerBound => accumulator.point_counts.lower_bound += 1,
                            PointBasis::Unsupported => {
                                accumulator.point_counts.unsupported += 1;
                                // Diagnostic trace for coverage-gap
                                // analysis: OPTCOIL_TRACE_UNSUPPORTED=1
                                // names the exact unsupported query.
                                if std::env::var_os("OPTCOIL_TRACE_UNSUPPORTED").is_some() {
                                    eprintln!(
                                        "unsupported point: station={} tape={} turn={} width={} |B|={:.6} T theta_raw={:.3} deg folded={:.3} mirror={:.3}",
                                        station.id(),
                                        tape_index,
                                        turn_index,
                                        width_index,
                                        point.magnitude_t,
                                        point.angle_raw_deg,
                                        point.angle_folded_deg,
                                        point.mirror_angle_deg,
                                    );
                                }
                            }
                            PointBasis::AlongCurrentExcluded => {
                                accumulator.point_counts.along_current_excluded += 1
                            }
                            PointBasis::AlongCurrentBounded => {
                                accumulator.point_counts.along_current_bounded += 1
                            }
                        }
                        if let Some(ratio) = point.self_field_ratio {
                            accumulator.max_self_field_ratio =
                                accumulator.max_self_field_ratio.max(ratio);
                        }
                        if let Some(ratio) = point.transport_self_field_ratio {
                            accumulator.max_transport_self_field_ratio =
                                accumulator.max_transport_self_field_ratio.max(ratio);
                        }
                        accumulator.max_along_current_fraction = accumulator
                            .max_along_current_fraction
                            .max(point.along_current_fraction);
                        let refinement_change_t = geometry_points[width_index]
                            .refinement_change_t_per_ampere_turn
                            * ampere_turns_a;
                        accumulator.max_refinement_change_t =
                            accumulator.max_refinement_change_t.max(refinement_change_t);
                        candidate_points.push(point);
                    }

                    let tape_status = aggregate_status(candidate_points.iter().map(|p| p.status));
                    // `AlongCurrentBounded` counts as determined — it is a
                    // bounded estimate under the case's declared
                    // along-current model, same epistemic class as
                    // `LowerBound`, and the record labels it separately.
                    // Determined also requires the queried value: a point
                    // labeled with a determined basis but no
                    // `k_used_a_per_m` (e.g. a mirror pair where only one
                    // side produced a number) is inconclusive, not a
                    // panic.
                    let all_determined = candidate_points.iter().all(|p| {
                        p.k_used_a_per_m.is_some()
                            && matches!(
                                p.basis,
                                PointBasis::Estimate
                                    | PointBasis::LowerBound
                                    | PointBasis::AlongCurrentBounded
                            )
                    });
                    let (
                        i_min_a,
                        i_strip_a,
                        allowed_screening_a,
                        utilization,
                        limiting_width_index,
                        own_tape_edge_field_scale_t,
                    ) = if all_determined {
                        let k: [f64; 5] = std::array::from_fn(|i| {
                            candidate_points[i]
                                .k_used_a_per_m
                                .expect("determined point")
                        });
                        let i_min = tape_frame::i_min_a(case.winding.tape_width_m, k);
                        let i_strip = tape_frame::i_strip_a(case.winding.tape_width_m, k);
                        // `strands_parallel` conductors share `current_a`,
                        // so the pack's screened capacity is s times one
                        // tape's (same scaling as evaluate_candidate_point).
                        let strands = f64::from(case.winding.strands_parallel.max(1));
                        let allowed = (1.0 - budget) * i_min * strands;
                        let util = current_a / allowed;
                        let limiting_index = (0..k.len())
                            .min_by(|&a, &b| k[a].total_cmp(&k[b]))
                            .expect("nonempty width rule")
                            as u32;
                        let edge_scale = tape_frame::own_tape_edge_field_scale_t(
                            k[limiting_index as usize],
                            case.winding.tape_width_m,
                            pitch_n_m,
                        );
                        (
                            Some(i_min),
                            Some(i_strip),
                            Some(allowed),
                            Some(util),
                            Some(limiting_index),
                            Some(edge_scale),
                        )
                    } else {
                        (None, None, None, None, None, None)
                    };

                    let accumulator = &mut accumulators[candidate_index];
                    accumulator.tape_statuses.push(tape_status);
                    accumulator.tape_summaries.push(TapeSummary {
                        station: station.id().to_owned(),
                        tape_index,
                        turn_index,
                        utilization,
                        allowed_screening_a,
                        limiting_width_index,
                        self_field_ratio_at_limit: limiting_width_index
                            .and_then(|w| candidate_points[w as usize].self_field_ratio),
                    });

                    per_candidate.push(TapeCandidateResult {
                        current_a,
                        points: candidate_points,
                        i_min_a,
                        i_strip_a,
                        allowed_screening_a,
                        utilization,
                        limiting_width_index,
                        own_tape_edge_field_scale_t,
                        status: tape_status,
                    });
                }

                tapes.push(TapeResult {
                    station: station.id().to_owned(),
                    tape_index,
                    turn_index,
                    center_position_m,
                    frame: frame_record,
                    points: geometry_points,
                    per_candidate,
                });
            }
        }
        stations_out.push(StationGroup {
            station: station.id().to_owned(),
            tapes,
        });
    }

    if cancel.load(Ordering::Relaxed) {
        return Err(RunError::Cancelled);
    }

    let mut candidates = Vec::with_capacity(num_candidates);
    for (candidate_index, &current_a) in case.operating.current_candidates_a.iter().enumerate() {
        let accumulator = &accumulators[candidate_index];
        let mut status = aggregate_status(accumulator.tape_statuses.iter().copied());
        let best = accumulator
            .tape_summaries
            .iter()
            .filter(|s| s.utilization.is_some())
            .max_by(|a, b| a.utilization.unwrap().total_cmp(&b.utilization.unwrap()));
        let (limiting, max_utilization, limiting_self_field_ratio) = match best {
            Some(b) => (
                Some(LimitingPoint {
                    station: b.station.clone(),
                    tape_index: b.tape_index,
                    turn_index: b.turn_index,
                    width_index: b
                        .limiting_width_index
                        .expect("limiting tape is fully determined"),
                }),
                b.utilization,
                b.self_field_ratio_at_limit,
            ),
            None => (None, None, None),
        };
        let min_allowed_screening_a = accumulator
            .tape_summaries
            .iter()
            .filter_map(|s| s.allowed_screening_a)
            .fold(f64::INFINITY, f64::min);
        let min_allowed_screening_a =
            (min_allowed_screening_a.is_finite()).then_some(min_allowed_screening_a);
        // Self-field gate (contract A3 / OC-014). The three correction
        // states gate differently — no correction: the `k_used` ratio vs
        // `max_self_field_ratio`; `uniform_transport`: the dominance
        // boundary (ratio > 1, where the perturbation bound is invalid);
        // `critical_state_strip`: the edge-field term is a bound at any
        // ratio, so the gate never fires.
        let gate_exceeded = match case.limits.self_field_correction.as_deref() {
            // `critical_state_strip` queries at the strip's own
            // edge-field bound, which stays a bound at any
            // transport ratio — the dominance gate does not apply.
            Some("critical_state_strip") => false,
            // `uniform_transport`: any evaluated point whose carried
            // sheet self-field exceeds its applied field
            // (`transport_self_field_ratio` > 1) has no valid bound →
            // INCONCLUSIVE.
            Some(_) => accumulator.max_transport_self_field_ratio > 1.0,
            // No correction: the query ignores the tape's own field, so a
            // `k_used`-based ratio above `max_self_field_ratio` makes the
            // query untrustworthy → INCONCLUSIVE.
            None => limiting_self_field_ratio.is_some_and(|r| r > case.limits.max_self_field_ratio),
        };
        if status == Status::Pass && gate_exceeded {
            status = Status::Inconclusive;
        }
        candidates.push(CandidateResult {
            current_a,
            ampere_turns_a: total_turns as f64 * current_a,
            status,
            limiting,
            min_allowed_screening_a,
            max_utilization,
            point_counts: accumulator.point_counts,
            max_self_field_ratio: accumulator.max_self_field_ratio,
            limiting_self_field_ratio,
            max_transport_self_field_ratio: Some(accumulator.max_transport_self_field_ratio),
            max_along_current_fraction: accumulator.max_along_current_fraction,
            max_refinement_change_t: accumulator.max_refinement_change_t,
        });
    }

    let field_scale_t = case.numerics.field_scale_t;
    let overall_max_refinement_change_t = candidates
        .iter()
        .map(|c| c.max_refinement_change_t)
        .fold(0.0, f64::max);
    let field_refinement_status = if overall_max_refinement_change_t / field_scale_t
        <= case.numerics.max_refinement_change_fraction
    {
        Status::Pass
    } else {
        Status::Inconclusive
    };

    let max_reference_refinement_t = reference.as_ref().map(|r| {
        r.points
            .iter()
            .map(|p| {
                let scale_factor = total_turns as f64 * r.evaluated_current_a;
                let scaled: [[f64; 3]; 2] = p
                    .unit_fields_t_per_ampere_turn
                    .map(|u| u.map(|x| x * scale_factor));
                norm(sub(scaled[1], scaled[0]))
            })
            .fold(0.0, f64::max)
    });
    let reference_refinement_status = match max_reference_refinement_t {
        Some(value) if value / field_scale_t <= case.numerics.max_reference_refinement_fraction => {
            Status::Pass
        }
        Some(_) => Status::Inconclusive,
        None => Status::NotEvaluated,
    };

    let comparison_outcome = reference
        .as_ref()
        .map(|r| compare_against_reference(&case, &stations_out, r, &expected_subset))
        .transpose()?;
    let comparison_status = match &comparison_outcome {
        None => Status::NotEvaluated,
        Some(_)
            if field_refinement_status != Status::Pass
                || reference_refinement_status != Status::Pass =>
        {
            Status::Inconclusive
        }
        Some(outcome) => {
            let ok = outcome.max_field_error_t / field_scale_t
                <= case.numerics.max_reference_field_error_fraction
                && outcome.max_angle_error_deg <= case.numerics.max_reference_angle_error_deg
                && !outcome.basis_mismatch
                && !outcome.capacity_missing
                && outcome.max_capacity_relative_error
                    <= case.numerics.max_reference_capacity_relative_error;
            if ok { Status::Pass } else { Status::Fail }
        }
    };
    let numerical_status = match comparison_status {
        Status::Pass | Status::Fail => comparison_status,
        Status::Inconclusive | Status::NotEvaluated => Status::Inconclusive,
    };
    let reference_comparison = comparison_outcome.map(|outcome| ReferenceComparison {
        max_field_error_t: outcome.max_field_error_t,
        max_angle_error_deg: outcome.max_angle_error_deg,
        max_capacity_relative_error: outcome.max_capacity_relative_error,
        max_reference_refinement_t: max_reference_refinement_t.unwrap_or(0.0),
        compared_points: reference.as_ref().map_or(0, |r| r.points.len()),
        compared_capacity_points: outcome.compared_capacity_points,
        capacity_missing: outcome.capacity_missing,
    });

    let coverage_status = if candidates
        .iter()
        .all(|c| c.point_counts.unsupported == 0 && c.point_counts.along_current_excluded == 0)
    {
        Status::Pass
    } else {
        Status::Inconclusive
    };

    let mut checks = vec![
        Check {
            id: "field_refinement".into(),
            status: field_refinement_status,
            detail: format!(
                "Maximum last-two-order unit-ampere-turn field change, scaled to each candidate, {overall_max_refinement_change_t:.9e} T; limit {:.9e} T",
                field_scale_t * case.numerics.max_refinement_change_fraction
            ),
        },
        Check {
            id: "reference_refinement".into(),
            status: reference_refinement_status,
            detail: format!(
                "Recomputed reference last-two-order change {max_reference_refinement_t:?} T; limit {:.9e} T",
                field_scale_t * case.numerics.max_reference_refinement_fraction
            ),
        },
        Check {
            id: "independent_comparison".into(),
            status: comparison_status,
            detail: match &reference_comparison {
                Some(rc) if rc.capacity_missing => format!(
                    "Independent NumPy reference comparison over the reference_subset at the reference's evaluated current; requires both refinement checks to pass; capacity_missing = true: only {} determined-basis points yielded a capacity comparison, at least one determined point had a null or nonfinite Rust or reference k_used_a_per_m",
                    rc.compared_capacity_points
                ),
                Some(rc) => format!(
                    "Independent NumPy reference comparison over the reference_subset at the reference's evaluated current; requires both refinement checks to pass; capacity comparison covered all {} determined-basis points",
                    rc.compared_capacity_points
                ),
                None => "Independent NumPy reference comparison over the reference_subset at the reference's evaluated current; requires both refinement checks to pass".into(),
            },
        },
        Check {
            id: "monotonicity_audit".into(),
            // The base binding's audit; graded cases aggregate the worst
            // verdict across every binding — a spec dataset's audit is as
            // load-bearing as the base's.
            status: aggregate_status(
                std::iter::once(&runtimes.base.monotonicity)
                    .chain(runtimes.specs.values().map(|r| &r.monotonicity))
                    .map(|a| a.status),
            ),
            detail: format!(
                "base: {} pairs checked, {} violations above {:.4}% tolerance, worst relative increase {:.6}%{}",
                runtimes.base.monotonicity.checked_pairs,
                runtimes.base.monotonicity.violations,
                runtimes.base.monotonicity.tolerance * 100.0,
                runtimes.base.monotonicity.worst_relative_increase * 100.0,
                runtimes
                    .specs
                    .iter()
                    .map(|(id, r)| format!(
                        "; spec '{id}': {} pairs, {} violations",
                        r.monotonicity.checked_pairs, r.monotonicity.violations
                    ))
                    .collect::<String>()
            ),
        },
    ];
    for (id, detail) in [
        (
            "strain_state",
            "Dataset strain as measured, unquantified; winding, cool-down and Lorentz-load strain are unmodeled",
        ),
        (
            "current_sharing",
            "One tape per turn is assumed; parallel tapes and current redistribution are unmodeled",
        ),
        (
            "mechanical",
            "Stress, strain, bend radius, support and pack fit are not evaluated",
        ),
        (
            "thermal",
            "Cooling, AC losses and joint heating are not evaluated",
        ),
        (
            "quench",
            "Quench detection and protection are not evaluated",
        ),
        (
            "manufacturing_validation",
            "Actual winding process, stock lengths and joint geometry need engineering evidence",
        ),
    ] {
        checks.push(Check {
            id: id.into(),
            status: Status::NotEvaluated,
            detail: detail.into(),
        });
    }

    let implementation_sha256 = hash(&serde_json::to_vec(&(
        include_str!("../../optcoil-model/src/lib.rs"),
        include_str!("../../optcoil-model/src/magnetics.rs"),
        include_str!("../../optcoil-model/src/material.rs"),
        include_str!("../../optcoil-model/src/coupled.rs"),
        include_str!("../../optcoil-physics/src/racetrack.rs"),
        include_str!("../../optcoil-physics/src/critical_current.rs"),
        include_str!("../../optcoil-physics/src/tape_frame.rs"),
        include_str!("lib.rs"),
        include_str!("coupled.rs"),
        include_str!("../../../tools/reference_oc004.py"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?);
    let reference_sha256 = reference_json.map(|json| hash(json.as_bytes()));
    let field_map_record = case
        .sampling
        .field_map
        .as_ref()
        .map(FieldMapRecord::from_map);
    // Under a declared map the record must not claim the engine's field
    // kernel ran — the declared map is the field model.
    let field_model_id = field_map_record
        .as_ref()
        .map(FieldMapRecord::field_model_id)
        .unwrap_or(FIELD_MODEL_ID);
    let input_sha256 = hash(&serde_json::to_vec(&(
        &case_sha256,
        &reference_sha256,
        &implementation_sha256,
        options,
        field_model_id,
        LOG_IC_MODEL_ID,
        COUPLED_MODEL_ID,
        COUPLED_CHECKER_ID,
        env!("CARGO_PKG_VERSION"),
    ))?);

    let dataset_identity = DatasetIdentity::new(dataset, mesh);
    let audit_record = |audit: &tape_frame::MonotonicityAudit| MonotonicityAuditRecord {
        checked_pairs: audit.checked_pairs,
        violations: audit.violations,
        worst_relative_increase: audit.worst_relative_increase,
        tolerance: audit.tolerance,
        status: audit.status,
    };
    let monotonicity_audit = audit_record(&runtimes.base.monotonicity);
    // Schema v6 provenance: each spec binding's resolved dataset and its
    // own audit — a graded pack's screening is only auditable if every
    // dataset it touched is on the record.
    let mut spec_datasets = BTreeMap::new();
    let mut spec_monotonicity_audits = BTreeMap::new();
    for (id, runtime) in &runtimes.specs {
        let spec_dataset = &runtimes.datasets[&runtime.material.dataset_id];
        spec_datasets.insert(
            id.clone(),
            DatasetIdentity::new(spec_dataset, runtime.interpolator.summary().clone()),
        );
        spec_monotonicity_audits.insert(id.clone(), audit_record(&runtime.monotonicity));
    }

    // Bridge-to-full-width applicability per bound dataset — computed
    // before `spec_datasets`/`dataset_identity` move into the record.
    let applied_width_m = case.winding.tape_width_m;
    let mut width_limitations = Vec::new();
    {
        let mut bindings: Vec<(&str, &DatasetIdentity)> = vec![("base", &dataset_identity)];
        bindings.extend(
            spec_datasets
                .iter()
                .map(|(id, identity)| (id.as_str(), identity)),
        );
        for (binding, identity) in bindings {
            let (Some(bridge), Some(tape)) = (
                identity.measured_bridge_width_m,
                identity.original_tape_width_m,
            ) else {
                continue;
            };
            let mismatch = if (applied_width_m - tape).abs() > tape * 1e-9 {
                format!(
                    " The case applies the data at {applied_width_m} m product width, different from the {tape} m tape the specimen was cut from — cross-product-width transfer is additionally unverified."
                )
            } else {
                String::new()
            };
            width_limitations.push(format!(
                "Width applicability, binding '{binding}' ({}): the specimen was measured as a {bridge} m patterned bridge from a {tape} m tape; K in A/m is bridge-width-normalized.{mismatch} Bridge-to-full-width transfer (Jc uniformity across the real tape width, edge degradation, lot variation) is an unverified modeling assumption, not measured coverage — full-width or lot-representative data at the operating point remains a data gap.",
                identity.id
            ));
        }
    }

    Ok(CoupledRunRecord {
        // v7 adds `spec_datasets`/`spec_monotonicity_audits` (empty and
        // absent on ungraded cases; older records still parse). v8 adds
        // `field_map` (absent on engine-field runs).
        schema: "optcoil-coupled-run/v8".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        field_model_id: field_model_id.into(),
        ic_model_id: LOG_IC_MODEL_ID.into(),
        coupled_model_id: COUPLED_MODEL_ID.into(),
        checker_id: COUPLED_CHECKER_ID.into(),
        case_sha256,
        reference_sha256,
        implementation_sha256,
        input_sha256,
        started_unix_ms,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        runtime: runtime_info(),
        case,
        dataset: dataset_identity,
        monotonicity_audit,
        spec_datasets,
        spec_monotonicity_audits,
        total_turns,
        pitch_n_m,
        pitch_w_m,
        width_rule: WidthRule { nodes, weights },
        kernel_evaluations,
        field_timing_ms,
        field_map: field_map_record.clone(),
        stations: stations_out,
        candidates,
        reference,
        reference_comparison,
        numerical_status,
        coverage_status,
        conductor_qualification_status: Status::Inconclusive,
        engineering_status: Status::NotEvaluated,
        checks,
        limitations: {
            let mut limitations = vec![
                "Screening current allowance under declared assumptions; not a production operating-current limit.".into(),
                "Homogenized racetrack pack with uniform tangential current; no inter-pancake spacer gaps, screening currents, redistribution or iron.".into(),
                "Applied-field bridge law includes the measurement sample's own self-field response; width_transfer basis is none (no Jc nonuniformity or batch variation modeled).".into(),
                "Period-180 angle folding and the minimum-of-mirror-pair policy are declared symmetry assumptions with quantified, nonzero measured asymmetry.".into(),
                "Below 1 T the monotonic lower-bound assumption is not verified by this embedded dataset; see tools/audit_oc004_material_assumptions.py for offline evidence against the archived workbook.".into(),
                "Strain, current sharing, mechanical, thermal, quench and manufacturing acceptance are NOT_EVALUATED.".into(),
            ];
            // Recorded limitation, not a verdict.
            limitations.extend(width_limitations);
            if field_map_record.is_some() {
                limitations.push(
                    "Sampled fields come from the case-declared external field map (field_map.source_sha256), not the engine's field solve; the map's own mesh is the field resolution and its correctness is the customer's, not this run's."
                        .into(),
                );
            }
            limitations
        },
    })
}

#[derive(Debug, Default)]
struct TapeSummary {
    station: String,
    tape_index: u32,
    turn_index: u32,
    utilization: Option<f64>,
    allowed_screening_a: Option<f64>,
    limiting_width_index: Option<u32>,
    self_field_ratio_at_limit: Option<f64>,
}

#[derive(Debug, Default)]
struct CandidateAccumulator {
    point_counts: PointCounts,
    max_self_field_ratio: f64,
    max_transport_self_field_ratio: f64,
    max_along_current_fraction: f64,
    max_refinement_change_t: f64,
    tape_statuses: Vec<Status>,
    tape_summaries: Vec<TapeSummary>,
}

struct ComparisonOutcome {
    max_field_error_t: f64,
    max_angle_error_deg: f64,
    max_capacity_relative_error: f64,
    basis_mismatch: bool,
    /// True when some reference point whose Rust basis is determined
    /// (`Estimate`, `LowerBound` or `AlongCurrentBounded`) is missing a
    /// Rust or reference `k_used_a_per_m` value, or either is present but
    /// nonfinite: capacity comparison must not be silently skipped
    /// (contract §9.1).
    capacity_missing: bool,
    /// Count of determined-basis points that actually contributed a
    /// capacity comparison; must equal the number of determined points in
    /// the subset (i.e. `capacity_missing` false) for the check to pass.
    compared_capacity_points: usize,
}

/// FAIL beats INCONCLUSIVE beats PASS/NOT_EVALUATED (contract A10's shared
/// fail-closed lattice, used for both tape-from-points and
/// candidate-from-tapes aggregation). `pub(crate)`: OC-007's coupled search
/// reuses this for its own coarse-plan pruning aggregation (still the same
/// lattice, not reimplemented).
pub(crate) fn aggregate_status(statuses: impl IntoIterator<Item = Status>) -> Status {
    let mut has_fail = false;
    let mut has_inconclusive = false;
    for status in statuses {
        match status {
            Status::Fail => has_fail = true,
            Status::Inconclusive | Status::NotEvaluated => has_inconclusive = true,
            Status::Pass => {}
        }
    }
    if has_fail {
        Status::Fail
    } else if has_inconclusive {
        Status::Inconclusive
    } else {
        Status::Pass
    }
}

fn to_point_basis(basis: tape_frame::QueryBasis) -> PointBasis {
    match basis {
        tape_frame::QueryBasis::Estimate => PointBasis::Estimate,
        tape_frame::QueryBasis::LowerBound => PointBasis::LowerBound,
        tape_frame::QueryBasis::Unsupported => PointBasis::Unsupported,
    }
}

/// Contract A5/A8's mirror-pair combination: unsupported if either angle is
/// unsupported, estimate iff both are estimates, else lower_bound; the used
/// capacity is the minimum of the two determined values (ties keep the
/// folded angle). Kept as a standalone pure function (mirroring
/// `optcoil_physics::tape_frame`'s private combination) so the monotonicity
/// gate can be re-applied to a mutated angle pair before recombining.
fn combine_mirror_pair(
    folded: &tape_frame::AngleQuery,
    mirror: &tape_frame::AngleQuery,
    magnitude_t: f64,
) -> (tape_frame::QueryBasis, Option<f64>, f64) {
    let basis = match (folded.basis, mirror.basis) {
        (tape_frame::QueryBasis::Unsupported, _) | (_, tape_frame::QueryBasis::Unsupported) => {
            tape_frame::QueryBasis::Unsupported
        }
        (tape_frame::QueryBasis::Estimate, tape_frame::QueryBasis::Estimate) => {
            tape_frame::QueryBasis::Estimate
        }
        _ => tape_frame::QueryBasis::LowerBound,
    };
    match (folded.k_a_per_m, mirror.k_a_per_m) {
        (Some(f), Some(m)) if m < f => (basis, Some(m), mirror.query_field_t),
        (Some(f), Some(_)) => (basis, Some(f), folded.query_field_t),
        _ => (basis, None, magnitude_t),
    }
}

fn to_angle_result(
    interpolator: &IcInterpolator,
    temperature_k: f64,
    angle: &tape_frame::AngleQuery,
) -> Result<AngleResult, RunError> {
    let support = if angle.basis == tape_frame::QueryBasis::Unsupported {
        Vec::new()
    } else {
        let estimate = interpolator
            .evaluate([temperature_k, angle.query_field_t, angle.angle_query_deg])?
            .ok_or_else(|| {
                RunError::Invalid(
                    "interpolator disagreed with the clamp sequence's own determination".into(),
                )
            })?;
        estimate.support
    };
    Ok(AngleResult {
        basis: to_point_basis(angle.basis),
        query_field_t: angle.query_field_t,
        angle_query_deg: angle.angle_query_deg,
        k_a_per_m: angle.k_a_per_m,
        support,
    })
}

/// OC-014 Phase 2b: the strip's bounding critical sheet current as a
/// function of total field — `k_bound(B)` = the largest Ic resolvable
/// over every measured angle level at field `B`, tabulated once per case
/// on the dataset's own nominal field levels. The field-consistent
/// edge-field solve needs `k_bound` at many field values per point; the
/// table keeps that O(1) per query instead of an interpolator sweep per
/// iteration, and taking the max over the *whole* measured angle axis —
/// not only the near-perpendicular sector the edge field itself enters —
/// keeps the bound valid whatever angle the edge zone's total field
/// actually takes (an oblique applied field rotates the edge-zone angle
/// out of the perpendicular sector, where Ic is higher).
///
/// Each entry is `(evaluated_field_t, suffix_max_k_a_per_m)` where the
/// suffix max runs over this and all higher field levels: `k_bound(B)`
/// is the suffix entry at the greatest evaluated level not above `B`.
/// Since an interpolated Ic between two measured levels cannot exceed
/// both endpoints, the suffix max upper-bounds the true maximum at the
/// query field — the table is a *bounding* function, not an
/// interpolation, and stays rigorous even where the measured Ic(B)
/// itself wiggles. Below the first evaluated level the first entry
/// applies (the clamp regime); above the last the last entry applies —
/// points beyond measured coverage are still decided `Unsupported` by
/// the material query this bound only raises.
pub(crate) struct CriticalStateFieldTable {
    entries: Vec<(f64, f64)>,
}

impl CriticalStateFieldTable {
    fn k_bound_at(&self, field_t: f64) -> f64 {
        let index = self
            .entries
            .partition_point(|&(b, _)| b <= field_t)
            .saturating_sub(1)
            .min(self.entries.len() - 1);
        self.entries[index].1
    }

    /// The field-consistent edge-field bound: the largest fixed point of
    ///     B_edge = (μ0/2π)·ln(w/d)·k_bound(|B_app| + B_edge)
    /// Bisection on g(B) = B − f(B): the suffix-max table is
    /// non-increasing in field, so g is non-decreasing; g(0) < 0 and
    /// g(B_floor) ≥ 0 where B_floor = f over the whole table (the strict
    /// low-field bound). The returned `hi` endpoint satisfies
    /// hi ≥ f(hi) — the bound property — and approaches the fixed point
    /// from above.
    fn consistent_edge_field_t(
        &self,
        applied_magnitude_t: f64,
        tape_width_m: f64,
        layer_thickness_m: f64,
        low_field_clamp_t: f64,
    ) -> f64 {
        let bound = |k: f64| {
            tape_frame::critical_state_edge_field_bound_t(k, tape_width_m, layer_thickness_m)
        };
        let mut lo = 0.0_f64;
        let mut hi = bound(self.k_bound_at(low_field_clamp_t));
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            let k = self.k_bound_at((applied_magnitude_t + mid).max(low_field_clamp_t));
            if bound(k) > mid {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi
    }
}

/// Build the case-level table: at each nominal field level (clamped up
/// to the case's low-field floor), the largest Ic resolvable over the
/// whole measured angle axis, then a suffix-max pass so the table is a
/// non-increasing bounding function of field. `None` only when nothing
/// resolves — the caller then marks points unboundable (INCONCLUSIVE),
/// since an unbounded self-field cannot honestly support an uncorrected
/// applied-field query.
pub(crate) fn build_critical_state_field_table(
    interpolator: &IcInterpolator,
    temperature_k: f64,
    low_field_clamp_t: f64,
) -> Option<CriticalStateFieldTable> {
    let [_, field_levels, angle_levels] = interpolator.nominal_axes();
    let mut entries: Vec<(f64, f64)> = Vec::new();
    for &level in field_levels {
        let evaluated = level.max(low_field_clamp_t);
        if entries.last().is_some_and(|&(b, _)| b == evaluated) {
            continue;
        }
        let mut k_max: Option<f64> = None;
        for &angle in angle_levels {
            if let Ok(Some(estimate)) = interpolator.evaluate([temperature_k, evaluated, angle]) {
                k_max = Some(
                    k_max.map_or(estimate.ic_a_per_m, |acc: f64| acc.max(estimate.ic_a_per_m)),
                );
            }
        }
        if let Some(k) = k_max {
            entries.push((evaluated, k));
        }
    }
    for i in (0..entries.len().saturating_sub(1)).rev() {
        entries[i].1 = entries[i].1.max(entries[i + 1].1);
    }
    (!entries.is_empty()).then_some(CriticalStateFieldTable { entries })
}

/// One width point at one candidate current: field decomposition, angle
/// folding, the along-current gate (A5), the mirror-pair/low-field-clamp
/// query sequence (A8) gated by the run's own monotonicity audit, and the
/// declared screening allowance (A9). `pub(crate)`: this is the "per-candidate
/// screening" library function OC-007's coupled search calls directly for its
/// own coarse-plan pruning, so that per-point decision logic is reused
/// rather than reimplemented; behavior here is unchanged.
#[allow(clippy::too_many_arguments)]
/// `material` is the *resolved* binding for the point's turn (the
/// covering region's spec on a v6 graded case, else `case.material`) —
/// the caller resolves it via `MaterialRuntimes::for_turn`, and the
/// `interpolator`/`monotonicity_ok`/`critical_state_table` arguments must
/// come from the same binding's runtime.
pub(crate) fn evaluate_candidate_point(
    interpolator: &IcInterpolator,
    case: &CoupledCase,
    material: &MaterialSettings,
    monotonicity_ok: bool,
    frame: &TapeFrame,
    width_index: u32,
    unit_field_final: [f64; 3],
    current_a: f64,
    ampere_turns_a: f64,
    critical_state_table: Option<&CriticalStateFieldTable>,
) -> Result<CandidateWidthPoint, RunError> {
    let field_t = unit_field_final.map(|x| x * ampere_turns_a);
    let components = tape_frame::decompose(field_t, frame);
    let magnitude_t = tape_frame::magnitude_t(field_t);
    // `strands_parallel` conductors share `current_a` (I_op/s per strand).
    let strands = f64::from(case.winding.strands_parallel.max(1));
    // OC-014 self-field correction on the query magnitude:
    // `"uniform_transport"` adds `μ0·K_tape/2` — the uniform-transport-sheet
    // bound (higher field → lower Ic → conservative), valid only while the
    // added term does not dominate the applied field (the candidate gate
    // moves to that dominance boundary at ratio > 1).
    // `"critical_state_strip"` (Phase 2) adds the strip's critical-state
    // edge self-field bound — the largest fixed point of
    // `B_edge = (μ0/2π)·k_bound(|B_app|+B_edge)·ln(2w/d)`: the penetrated
    // edge zone carries the *critical* sheet current K_c whatever the
    // transport current, evaluated at the field the edge zone itself
    // sees (the applied magnitude plus the bound), maximized over the
    // measured angle axis so the bound holds whatever direction the
    // edge-zone field takes. The fixed point upper-bounds the tape's own
    // self-field at every applied level, including the interior
    // field-null pockets the `uniform_transport` bound cannot cover —
    // while being strictly tighter than the low-field-floor value it
    // replaces (the edge zone at elevated field sustains less than the
    // floor Ic). The dominance gate does not apply under this model (the
    // bound is valid at any ratio).
    let k_transport_a_per_m = current_a / (case.winding.tape_width_m * strands);
    let along_current_fraction = tape_frame::along_current_fraction(&components, magnitude_t);
    let angle_raw_deg = tape_frame::theta_raw_deg(&components);
    let angle_folded_deg = tape_frame::theta_fold_deg(angle_raw_deg);
    let mirror_angle_deg = tape_frame::theta_mirror_deg(angle_folded_deg);
    // Under `critical_state_strip` the self-field bound is only
    // determinable if the low-field-floor sheet current resolves; if it
    // cannot, the tape's own self-field is unbounded and the point is
    // honestly INCONCLUSIVE (we do not fall back to the uncorrected
    // applied field, which would be non-conservative in the dominance
    // regime this model exists to cover).
    let mut critical_state_unboundable = false;
    let mut critical_state_edge_field_t = None;
    let query_magnitude_t = match case.limits.self_field_correction.as_deref() {
        Some("critical_state_strip") => {
            let layer_thickness_m = case
                .limits
                .critical_state_layer_thickness_m
                .expect("v8 validation requires the declared layer thickness");
            match critical_state_table {
                Some(table) => {
                    let edge_field = table.consistent_edge_field_t(
                        magnitude_t,
                        case.winding.tape_width_m,
                        layer_thickness_m,
                        material.low_field_clamp_t,
                    );
                    critical_state_edge_field_t = Some(edge_field);
                    magnitude_t + edge_field
                }
                None => {
                    critical_state_unboundable = true;
                    magnitude_t
                }
            }
        }
        Some(_) => magnitude_t + MU0_H_PER_M * k_transport_a_per_m / 2.0,
        None => magnitude_t,
    };
    let transport_self_field_ratio =
        (magnitude_t > 0.0).then_some(MU0_H_PER_M * k_transport_a_per_m / (2.0 * magnitude_t));

    // OC-017 `transverse_bound`: past the measured-plane fraction limit but
    // within the ceiling, the point is still queried at full magnitude and
    // the transverse-plane angle — a bounded estimate under the declared
    // assumption, labeled `AlongCurrentBounded` below. Past the ceiling (or
    // with no model declared) the point is excluded as before.
    let along_current_bounded = case.limits.along_current_model.is_some()
        && along_current_fraction > case.limits.max_along_current_field_fraction
        && along_current_fraction <= optcoil_model::coupled::ALONG_CURRENT_BOUND_CEILING;
    if along_current_fraction > case.limits.max_along_current_field_fraction
        && !along_current_bounded
    {
        let excluded = AngleResult {
            basis: PointBasis::AlongCurrentExcluded,
            query_field_t: magnitude_t,
            angle_query_deg: 0.0,
            k_a_per_m: None,
            support: Vec::new(),
        };
        return Ok(CandidateWidthPoint {
            width_index,
            field_t,
            magnitude_t,
            angle_raw_deg,
            angle_folded_deg,
            mirror_angle_deg,
            along_current_fraction,
            query_field_t: magnitude_t,
            basis: PointBasis::AlongCurrentExcluded,
            k_folded: excluded.clone(),
            k_mirror: excluded,
            k_used_a_per_m: None,
            self_field_ratio: None,
            transport_self_field_ratio,
            critical_state_edge_field_t,
            allowed_screening_a: None,
            status: Status::Inconclusive,
        });
    }

    let clamped = tape_frame::query_mirror_pair_with_clamp(
        interpolator,
        case.operating.temperature_k,
        query_magnitude_t,
        angle_folded_deg,
        mirror_angle_deg,
        material.low_field_clamp_t,
    )?;
    let mut folded = clamped.folded;
    let mut mirror = clamped.mirror;
    if !monotonicity_ok {
        for angle in [&mut folded, &mut mirror] {
            if angle.basis == tape_frame::QueryBasis::LowerBound {
                angle.basis = tape_frame::QueryBasis::Unsupported;
                angle.k_a_per_m = None;
            }
        }
    }
    let (mut basis, mut k_used_a_per_m, mut query_field_t) =
        combine_mirror_pair(&folded, &mirror, query_magnitude_t);
    // `critical_state_strip` with no resolvable floor sheet current cannot
    // bound the tape's self-field: the applied-field-only query is not a
    // certified capacity, so the point is unsupported, not a pass.
    if critical_state_unboundable {
        basis = tape_frame::QueryBasis::Unsupported;
        k_used_a_per_m = None;
        query_field_t = magnitude_t;
    }
    let k_folded = to_angle_result(interpolator, case.operating.temperature_k, &folded)?;
    let k_mirror = to_angle_result(interpolator, case.operating.temperature_k, &mirror)?;

    let budget = case.limits.interpolation_overprediction_budget;
    let self_field_ratio = k_used_a_per_m.map(|k| tape_frame::self_field_ratio(k, query_field_t));
    // The pack's screened capacity is `s` times one tape's (strands share
    // `current_a`).
    let allowed_screening_a =
        k_used_a_per_m.map(|k| (1.0 - budget) * case.winding.tape_width_m * k * strands);
    let status = match allowed_screening_a {
        Some(allowed) if current_a <= case.limits.utilization_limit * allowed => Status::Pass,
        Some(_) => Status::Fail,
        None => Status::Inconclusive,
    };

    Ok(CandidateWidthPoint {
        width_index,
        field_t,
        magnitude_t,
        angle_raw_deg,
        angle_folded_deg,
        mirror_angle_deg,
        along_current_fraction,
        query_field_t,
        // A bounded point keeps the along-current label when the bound
        // actually produced a value — including when the underlying query
        // was itself a low-field lower bound, since the rarer,
        // less-established modeling assumption is the more important one
        // for the record to show. When the query produced no value
        // (unsupported domain or a missing k), there is nothing to bound:
        // the point keeps the underlying basis and stays INCONCLUSIVE,
        // and `all_determined` never sees a determined basis without a
        // `k_used_a_per_m`.
        basis: if along_current_bounded
            && k_used_a_per_m.is_some()
            && matches!(
                basis,
                tape_frame::QueryBasis::Estimate | tape_frame::QueryBasis::LowerBound
            ) {
            PointBasis::AlongCurrentBounded
        } else {
            to_point_basis(basis)
        },
        k_folded,
        k_mirror,
        k_used_a_per_m,
        self_field_ratio,
        transport_self_field_ratio,
        critical_state_edge_field_t,
        allowed_screening_a,
        status,
    })
}

/// One expanded `(station, tape_index, turn_index, width_index)` identity
/// from `numerics.reference_subset`, plus the pure-geometry position it maps
/// to. Named (rather than a bare tuple) purely to keep clippy's
/// type-complexity lint quiet across the several functions that share it.
struct ReferenceSubsetPoint {
    station: String,
    tape_index: u32,
    turn_index: u32,
    width_index: u32,
    position_m: [f64; 3],
}

fn expand_reference_subset(case: &CoupledCase) -> Result<Vec<ReferenceSubsetPoint>, RunError> {
    let nodes = tape_frame::lobatto_nodes();
    let mut out = Vec::with_capacity(case.numerics.reference_subset.len() * nodes.len());
    for entry in &case.numerics.reference_subset {
        let station: &Station = case
            .sampling
            .stations
            .iter()
            .find(|s| s.id() == entry.station)
            .ok_or_else(|| {
                RunError::Invalid("reference_subset station missing from the sampling plan".into())
            })?;
        let frame = station.frame(case.winding.tape_normal, case.pack.centerline())?;
        let center = tape_center_position_m(
            &case.pack,
            &case.winding,
            station,
            entry.tape_index,
            entry.turn_index,
        )?;
        for (width_index, &xi) in nodes.iter().enumerate() {
            let offset = width_offset_m(case.winding.tape_width_m, xi);
            let position_m = add(center, scale(frame.w, offset));
            out.push(ReferenceSubsetPoint {
                station: entry.station.clone(),
                tape_index: entry.tape_index,
                turn_index: entry.turn_index,
                width_index: width_index as u32,
                position_m,
            });
        }
    }
    Ok(out)
}

fn validate_reference(
    reference: &CoupledReference,
    case: &CoupledCase,
    case_sha256: &str,
    dataset: &MaterialDataset,
    expected: &[ReferenceSubsetPoint],
) -> Result<(), RunError> {
    let invalid = |s: &str| RunError::Invalid(format!("coupled reference: {s}"));
    // OC-014: a corrected case must be checked against a reference that
    // applied the same uniform-transport bound (method /v2); an
    // uncorrected case must keep the /v1 method. Method↔case consistency
    // is required exactly, not just membership in a set. OC-017 extends
    // the same rule to the along-current bound (v3), which stacks on the
    // self-field tag when both are declared.
    let expected_method = match (
        case.limits.self_field_correction.as_deref(),
        case.limits.along_current_model.is_some(),
    ) {
        (None, false) => COUPLED_REFERENCE_METHOD,
        (Some("critical_state_strip"), false) => COUPLED_REFERENCE_METHOD_CRITICAL_STATE,
        (Some("critical_state_strip"), true) => {
            COUPLED_REFERENCE_METHOD_CRITICAL_STATE_ALONG_CURRENT
        }
        (Some(_), false) => COUPLED_REFERENCE_METHOD_V2,
        (None, true) => COUPLED_REFERENCE_METHOD_ALONG_CURRENT,
        (Some(_), true) => COUPLED_REFERENCE_METHOD_V3,
    };
    if reference.schema != COUPLED_REFERENCE_SCHEMA
        || reference.method != expected_method
        || reference.component_subdivisions != 16
        || (reference.mu0_h_per_m - MU0_H_PER_M).abs() > 1e-20
    {
        return Err(invalid(
            "unsupported schema, method, component subdivisions or permeability",
        ));
    }
    if reference.orders[0] >= reference.orders[1]
        || reference.orders.iter().any(|o| !(2..=72).contains(o))
    {
        return Err(invalid(
            "reference orders must be strictly increasing within 2..=72",
        ));
    }
    if reference.case_sha256 != case_sha256 {
        return Err(invalid(
            "case SHA-256 does not match the exact input JSON; regenerate the reference",
        ));
    }
    if reference.source_sha256 != hash(REFERENCE_OC004_SOURCE.as_bytes()) {
        return Err(invalid(
            "source SHA-256 does not match the bundled independent reference implementation",
        ));
    }
    if reference.field_source_sha256 != hash(REFERENCE_OC002_SOURCE.as_bytes()) {
        return Err(invalid(
            "field-source SHA-256 does not match the bundled reference field kernel",
        ));
    }
    if reference.csv_sha256 != dataset.metadata.csv_sha256 {
        return Err(invalid(
            "csv SHA-256 does not match the embedded material dataset",
        ));
    }
    if !case
        .operating
        .current_candidates_a
        .iter()
        .any(|&c| (c - reference.evaluated_current_a).abs() < 1e-9)
    {
        return Err(invalid(
            "evaluated_current_a is not one of the case's current candidates",
        ));
    }
    if !reference.elapsed_seconds.is_finite() || reference.elapsed_seconds < 0.0 {
        return Err(invalid("elapsed_seconds must be finite and nonnegative"));
    }
    if reference.points.len() != expected.len() {
        return Err(invalid(
            "point count does not match the reference_subset expansion",
        ));
    }
    for (point, expected) in reference.points.iter().zip(expected) {
        let identity_matches = point.station == expected.station
            && point.tape_index == expected.tape_index
            && point.turn_index == expected.turn_index
            && point.width_index == expected.width_index;
        if !identity_matches {
            return Err(invalid(
                "point identity or ordering does not match the reference_subset expansion",
            ));
        }
        if (0..3).any(|k| (point.position_m[k] - expected.position_m[k]).abs() > 1e-12) {
            return Err(invalid("point position does not match the case geometry"));
        }
        let finite = point
            .unit_fields_t_per_ampere_turn
            .iter()
            .flatten()
            .all(|x| x.is_finite())
            && point.field_t.iter().all(|x| x.is_finite())
            && point.magnitude_t.is_finite()
            && point.angle_raw_deg.is_finite()
            && point.angle_folded_deg.is_finite()
            && point.mirror_angle_deg.is_finite()
            && point.along_current_fraction.is_finite()
            && point.query_field_t.is_finite()
            && point.k_folded_a_per_m.is_none_or(f64::is_finite)
            && point.k_mirror_a_per_m.is_none_or(f64::is_finite)
            && point.k_used_a_per_m.is_none_or(f64::is_finite);
        if !finite {
            return Err(invalid("point contains a nonfinite value"));
        }
    }
    Ok(())
}

fn compare_against_reference(
    case: &CoupledCase,
    stations: &[StationGroup],
    reference: &CoupledReference,
    expected: &[ReferenceSubsetPoint],
) -> Result<ComparisonOutcome, RunError> {
    let candidate_index = case
        .operating
        .current_candidates_a
        .iter()
        .position(|&c| (c - reference.evaluated_current_a).abs() < 1e-9)
        .ok_or_else(|| {
            RunError::Invalid(
                "reference evaluated_current_a is not one of the case candidates".into(),
            )
        })?;
    let mut max_field_error_t = 0.0_f64;
    let mut max_angle_error_deg = 0.0_f64;
    let mut max_capacity_relative_error = 0.0_f64;
    let mut basis_mismatch = false;
    let mut capacity_missing = false;
    let mut compared_capacity_points: usize = 0;
    for (reference_point, expected) in reference.points.iter().zip(expected) {
        let tape = find_tape(
            stations,
            &expected.station,
            expected.tape_index,
            expected.turn_index,
        )
        .ok_or_else(|| {
            RunError::Invalid(
                "reference_subset tape missing from the computed sampling grid".into(),
            )
        })?;
        let candidate = &tape.per_candidate[candidate_index];
        let rust_point = &candidate.points[expected.width_index as usize];
        max_field_error_t =
            max_field_error_t.max(norm(sub(rust_point.field_t, reference_point.field_t)));
        max_angle_error_deg = max_angle_error_deg.max(circular_diff_deg(
            rust_point.angle_folded_deg,
            reference_point.angle_folded_deg,
        ));
        max_angle_error_deg = max_angle_error_deg.max(circular_diff_deg(
            rust_point.mirror_angle_deg,
            reference_point.mirror_angle_deg,
        ));
        if rust_point.basis != reference_point.basis {
            basis_mismatch = true;
        }
        // Contract §9.1: the capacity comparison must not be silently
        // skipped just because a K value is null. For every point whose
        // Rust basis is determined (estimate, lower_bound or — OC-017 —
        // along_current_bounded), both the Rust and reference
        // k_used_a_per_m must be present and finite, or the comparison is
        // recorded as missing (and the check fails), never treated as
        // passing by omission.
        let rust_determined = matches!(
            rust_point.basis,
            PointBasis::Estimate | PointBasis::LowerBound | PointBasis::AlongCurrentBounded
        );
        if rust_determined {
            match (rust_point.k_used_a_per_m, reference_point.k_used_a_per_m) {
                (Some(rust_k), Some(reference_k))
                    if rust_k.is_finite() && reference_k.is_finite() =>
                {
                    compared_capacity_points += 1;
                    max_capacity_relative_error =
                        max_capacity_relative_error.max((rust_k / reference_k - 1.0).abs());
                }
                _ => {
                    capacity_missing = true;
                }
            }
        }
    }
    Ok(ComparisonOutcome {
        max_field_error_t,
        max_angle_error_deg,
        max_capacity_relative_error,
        basis_mismatch,
        capacity_missing,
        compared_capacity_points,
    })
}

fn find_tape<'a>(
    stations: &'a [StationGroup],
    station: &str,
    tape_index: u32,
    turn_index: u32,
) -> Option<&'a TapeResult> {
    stations
        .iter()
        .find(|s| s.station == station)?
        .tapes
        .iter()
        .find(|t| t.tape_index == tape_index && t.turn_index == turn_index)
}

/// Circular distance between two angles already folded into `[0, 180)`.
fn circular_diff_deg(a: f64, b: f64) -> f64 {
    let raw = (a - b).abs() % 180.0;
    raw.min(180.0 - raw)
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k])
}

fn scale(v: [f64; 3], factor: f64) -> [f64; 3] {
    v.map(|x| x * factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(basis: PointBasis, status: Status) -> CandidateWidthPoint {
        CandidateWidthPoint {
            width_index: 0,
            field_t: [0.0; 3],
            magnitude_t: 1.0,
            angle_raw_deg: 0.0,
            angle_folded_deg: 0.0,
            mirror_angle_deg: 180.0,
            along_current_fraction: 0.0,
            query_field_t: 1.0,
            basis,
            k_folded: AngleResult {
                basis,
                query_field_t: 1.0,
                angle_query_deg: 0.0,
                k_a_per_m: None,
                support: Vec::new(),
            },
            k_mirror: AngleResult {
                basis,
                query_field_t: 1.0,
                angle_query_deg: 180.0,
                k_a_per_m: None,
                support: Vec::new(),
            },
            k_used_a_per_m: None,
            self_field_ratio: None,
            transport_self_field_ratio: None,
            critical_state_edge_field_t: None,
            allowed_screening_a: None,
            status,
        }
    }

    #[test]
    fn status_lattice_puts_fail_over_inconclusive_over_pass() {
        assert_eq!(aggregate_status([Status::Pass, Status::Pass]), Status::Pass);
        assert_eq!(
            aggregate_status([Status::Pass, Status::Inconclusive]),
            Status::Inconclusive
        );
        assert_eq!(
            aggregate_status([Status::Pass, Status::Fail, Status::Inconclusive]),
            Status::Fail
        );
        assert_eq!(aggregate_status([]), Status::Pass);
    }

    /// OC-006 routed the runner's dataset lookup through
    /// `embedded_by_id(case.material.dataset_id)` instead of a fixed
    /// `embedded()` call; OC-004's own case still declares the original
    /// dataset id, so its resolved dataset identity must be unchanged.
    #[test]
    fn run_oc004_still_resolves_the_original_dataset_via_embedded_by_id() {
        let record = run_oc004(&CoupledOptions {
            orders: Some([2, 4]),
        })
        .unwrap();
        assert_eq!(record.dataset.id, "robinson-superpower-ap-v3");
        assert_eq!(record.dataset.point_count, 1505);
        assert_eq!(
            record.dataset.csv_sha256,
            MaterialDataset::embedded().unwrap().metadata.csv_sha256
        );
    }

    /// OC-014 Phase-2 parity: the frozen `oc-014` pair runs the
    /// `critical_state_strip` bound (perpendicular-sector floor) on the
    /// modelext dataset and must agree with the independent Python
    /// reference on every reference-subset point — basis, angles, field,
    /// and capacity — under the case's declared tolerances.
    #[test]
    fn run_oc014_matches_the_frozen_critical_state_reference() {
        let record = run_oc014(&CoupledOptions::default()).unwrap();
        assert_eq!(record.schema, "optcoil-coupled-run/v8");
        assert_eq!(record.dataset.id, "robinson-superpower-ap-v3-modelext");
        let comparison = record
            .checks
            .iter()
            .find(|c| c.id == "independent_comparison")
            .expect("the frozen reference always yields a comparison check");
        assert_eq!(
            comparison.status,
            Status::Pass,
            "Rust and the independent reference must agree: {}",
            comparison.detail
        );
        // Every determined point under critical_state_strip records the
        // edge-field bound that raised its query.
        let mut determined = 0usize;
        for station in &record.stations {
            for tape in &station.tapes {
                for point in tape.per_candidate.iter().flat_map(|c| c.points.iter()) {
                    if matches!(point.basis, PointBasis::Estimate | PointBasis::LowerBound) {
                        determined += 1;
                        assert!(point.critical_state_edge_field_t.unwrap() > 0.0);
                    }
                }
            }
        }
        assert!(determined > 0, "the fixture must produce determined points");
    }

    /// A case whose dataset_id names no embedded dataset must be rejected
    /// explicitly, never silently fall back to either embedded dataset.
    #[test]
    fn coupled_case_with_unknown_dataset_id_is_rejected_explicitly() {
        let mutated = optcoil_model::coupled::OC004_JSON.replacen(
            "robinson-superpower-ap-v3",
            "robinson-superpower-ap-v3-nonexistent",
            1,
        );
        // Only the case's own material.dataset_id occurrence is replaced
        // (the first one in the file); confirm that assumption held.
        assert!(mutated.contains("robinson-superpower-ap-v3-nonexistent"));
        let err = run_coupled_case(
            &mutated,
            None,
            &CoupledOptions {
                orders: Some([2, 4]),
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown embedded material dataset id"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn unsupported_point_makes_the_tape_inconclusive() {
        let points = [
            point(PointBasis::Estimate, Status::Pass),
            point(PointBasis::Estimate, Status::Pass),
            point(PointBasis::Unsupported, Status::Inconclusive),
            point(PointBasis::Estimate, Status::Pass),
            point(PointBasis::Estimate, Status::Pass),
        ];
        assert_eq!(
            aggregate_status(points.iter().map(|p| p.status)),
            Status::Inconclusive
        );
    }

    #[test]
    fn a_fail_point_makes_the_candidate_fail_even_with_unsupported_points_elsewhere() {
        // Tape 1 has a determined FAIL point; tape 2 has an undetermined point.
        let tape_one = aggregate_status(
            [
                point(PointBasis::Estimate, Status::Fail),
                point(PointBasis::Estimate, Status::Pass),
            ]
            .iter()
            .map(|p| p.status),
        );
        let tape_two = aggregate_status(
            [
                point(PointBasis::Unsupported, Status::Inconclusive),
                point(PointBasis::Estimate, Status::Pass),
            ]
            .iter()
            .map(|p| p.status),
        );
        assert_eq!(tape_one, Status::Fail);
        assert_eq!(tape_two, Status::Inconclusive);
        assert_eq!(aggregate_status([tape_one, tape_two]), Status::Fail);
    }

    #[test]
    fn self_field_gate_demotes_an_otherwise_passing_candidate_to_inconclusive() {
        // Mirrors the candidate-level demotion in run_coupled_case: a
        // pre-gate PASS becomes INCONCLUSIVE when the limiting point's
        // self-field ratio exceeds the declared limit.
        let pre_gate_status = Status::Pass;
        let limiting_self_field_ratio = Some(0.42);
        let max_self_field_ratio = 0.1;
        let gated = if pre_gate_status == Status::Pass
            && limiting_self_field_ratio.is_some_and(|r| r > max_self_field_ratio)
        {
            Status::Inconclusive
        } else {
            pre_gate_status
        };
        assert_eq!(gated, Status::Inconclusive);
        // A ratio within the limit never demotes.
        let within_limit = Some(0.05);
        let ungated = if pre_gate_status == Status::Pass
            && within_limit.is_some_and(|r| r > max_self_field_ratio)
        {
            Status::Inconclusive
        } else {
            pre_gate_status
        };
        assert_eq!(ungated, Status::Pass);
    }

    /// A minimal 2x2x2 measured-coordinate grid at (T, B, angle) in
    /// {20,30}x{1.001,2.0}x{0,180} K/T/deg. Choosing the angle levels 0 and
    /// 180 (rather than an arbitrary pair) means a field folded to exactly
    /// 0 deg has its full A5 mirror partner (180 deg) present as an exact
    /// grid node too, so both mirror-angle queries below land on real data.
    fn monotonicity_test_dataset_points() -> Vec<optcoil_model::material::MaterialPoint> {
        [
            (20.0, 1.001, 0.0, 200_000.0),
            (20.0, 1.001, 180.0, 210_000.0),
            (20.0, 2.0, 0.0, 180_000.0),
            (20.0, 2.0, 180.0, 190_000.0),
            (30.0, 1.001, 0.0, 220_000.0),
            (30.0, 1.001, 180.0, 230_000.0),
            (30.0, 2.0, 0.0, 200_000.0),
            (30.0, 2.0, 180.0, 210_000.0),
        ]
        .into_iter()
        .enumerate()
        .map(
            |(i, (t, b, a, ic))| optcoil_model::material::MaterialPoint {
                source_row: i as u32 + 1,
                nominal_temperature_k: t,
                nominal_field_t: b,
                nominal_angle_deg: a,
                temperature_k: t,
                applied_field_t: b,
                angle_from_normal_deg: a,
                ic_a_per_m: ic,
                bridge_ic_a: ic * 0.001,
                n_value: 20.0,
            },
        )
        .collect()
    }

    #[test]
    fn monotonicity_audit_failure_forces_a_lower_bound_point_to_unsupported() {
        // Contract A8: "a Fail here must make every lower-bound point
        // unsupported." The real embedded dataset's audit is always clean
        // (0 violations at >=1 T, per the build log), so evaluate_candidate_
        // point's monotonicity_ok=false branch was otherwise never driven
        // by any test -- a regression that dropped the demotion (e.g.
        // forgetting to null k_a_per_m alongside basis) would pass the
        // suite today.
        let case = CoupledCase::embedded().unwrap();
        let limits = optcoil_model::material::CellSpanLimits {
            temperature_k: 15.0,
            field_ratio: 3.0,
            angle_deg: 200.0,
        };
        let interpolator = IcInterpolator::with_method(
            &monotonicity_test_dataset_points(),
            limits,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let frame = TapeFrame {
            t: [0.0, 0.0, 1.0],
            n: [1.0, 0.0, 0.0],
            w: [0.0, 1.0, 0.0],
        };
        // B_n=0.5, B_w=0, B_t=0: magnitude 0.5 T, below the case's own
        // low_field_clamp_t (1.001 T), theta_fold=0, theta_mirror=180 (both
        // exact grid nodes above), along_current_fraction=0.
        let unit_field = [0.5, 0.0, 0.0];

        let clean = evaluate_candidate_point(
            &interpolator,
            &case,
            &case.material,
            true,
            &frame,
            0,
            unit_field,
            1.0,
            1.0,
            None,
        )
        .unwrap();
        assert_eq!(clean.basis, PointBasis::LowerBound);
        assert!(clean.k_used_a_per_m.is_some());

        let audited_out = evaluate_candidate_point(
            &interpolator,
            &case,
            &case.material,
            false,
            &frame,
            0,
            unit_field,
            1.0,
            1.0,
            None,
        )
        .unwrap();
        assert_eq!(audited_out.basis, PointBasis::Unsupported);
        assert_eq!(audited_out.k_used_a_per_m, None);
        assert_eq!(audited_out.status, Status::Inconclusive);
    }

    /// The same fixture style as `crates/optcoil-cli/tests/cli.rs`'s
    /// `small_custom_case_json`, parameterized on the declared self-field
    /// limit only.
    fn self_field_gate_case_json(max_self_field_ratio: f64) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-conductor/v1",
  "id": "self-field-gate-unit-test-case",
  "provenance": "coupled.rs unit test fixture; not a frozen benchmark.",
  "geometry_data_class": "synthetic",
  "pack": {{
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_width_m": 0.02,
    "axial_height_m": 0.036
  }},
  "winding": {{
    "tape_width_m": 0.012,
    "tape_normal": "radial",
    "turns_along_normal": 5,
    "tapes_along_width": 3
  }},
  "operating": {{
    "temperature_k": 21.0,
    "current_candidates_a": [870.0],
    "electric_field_criterion_v_per_m": 0.0001
  }},
  "material": {{
    "dataset_id": "robinson-superpower-ap-v3",
    "csv_sha256": "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
    "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
    "angle_mapping": "period_180_field_reversal",
    "mirror_policy": "minimum_of_mirror_pair",
    "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
    "field_magnitude_policy": "total_magnitude_with_transverse_angle",
    "low_field_policy": "monotone_field_lower_bound",
    "low_field_clamp_t": 1.001,
    "monotonicity_tolerance": 0.001
  }},
  "sampling": {{
    "stations": [
      {{"id": "s0", "kind": "straight", "x_m": 0.0}}
    ],
    "turn_indices_along_normal": [1, 2],
    "tape_indices_along_width": [1],
    "width_quadrature": "gauss_lobatto",
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": {max_self_field_ratio},
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{
    "quadrature_orders": [2, 4],
    "field_scale_t": 1.0,
    "max_refinement_change_fraction": 0.5,
    "max_reference_refinement_fraction": 0.5,
    "max_reference_field_error_fraction": 0.5,
    "max_reference_angle_error_deg": 90.0,
    "max_reference_capacity_relative_error": 0.5,
    "reference_subset": [
      {{"station": "s0", "tape_index": 1, "turn_index": 1}}
    ]
  }},
  "width_transfer": {{
    "basis": "none"
  }}
}}
"#
        )
    }

    #[test]
    fn self_field_gate_demotes_a_real_run_when_the_declared_limit_is_exceeded() {
        // Drives contract A3's demotion through the real run_coupled_case
        // pipeline, unlike self_field_gate_demotes_an_otherwise_passing_
        // candidate_to_inconclusive above (which only duplicates the
        // boolean condition inline on hand-picked Options). This catches a
        // real wiring bug, e.g. gating on max_self_field_ratio (the max
        // over ALL determined points) instead of limiting_self_field_ratio
        // (the ratio at the highest-utilization point specifically) -- both
        // are f64/Option<f64> fields on the same struct.
        let permissive = run_coupled_case(
            &self_field_gate_case_json(1.0e6),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let baseline = &permissive.candidates[0];
        assert_eq!(
            baseline.status,
            Status::Pass,
            "fixture must be a genuine pre-gate PASS for this test to demonstrate a demotion"
        );
        assert_eq!(baseline.point_counts.unsupported, 0);
        assert_eq!(baseline.point_counts.along_current_excluded, 0);
        let ratio = baseline
            .limiting_self_field_ratio
            .expect("a fully-determined PASS candidate always records a limiting self-field ratio");

        // Set the declared limit strictly below the ratio just observed
        // (self_field_ratio depends only on geometry/material, not on
        // current_a, so this is stable across reruns) and confirm the
        // otherwise-identical run is demoted, with the exceeding ratio
        // recorded on the record.
        let gated_limit = ratio - 1e-6;
        let gated = run_coupled_case(
            &self_field_gate_case_json(gated_limit),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let candidate = &gated.candidates[0];
        assert_eq!(candidate.status, Status::Inconclusive);
        assert!(candidate.limiting_self_field_ratio.unwrap() > gated_limit);
    }

    /// v3 variant of the fixture: schema `optcoil-coupled-conductor/v3`
    /// with `limits.along_current_model` set (or absent when `model` is
    /// `None`). Tight-bend geometry plus an arc station at azimuth 90 —
    /// probed to push tape-edge along-current fractions just past the 0.2
    /// gate so the model's bounded interval is exercised.
    fn along_current_case_json_v3(model: Option<&str>) -> String {
        let model_field = model
            .map(|m| format!(r#", "along_current_model": "{m}""#))
            .unwrap_or_default();
        self_field_gate_case_json(1.0e6)
            .replacen(
                "\"optcoil-coupled-conductor/v1\"",
                "\"optcoil-coupled-conductor/v3\"",
                1,
            )
            .replacen(
                r#""utilization_limit": 0.8"#,
                &format!(r#""utilization_limit": 0.8{model_field}"#),
                1,
            )
            .replacen(
                r#""straight_half_length_m": 0.3"#,
                r#""straight_half_length_m": 0.1"#,
                1,
            )
            .replacen(r#""bend_radius_m": 0.2"#, r#""bend_radius_m": 0.05"#, 1)
            .replacen(
                r#""axial_height_m": 0.036"#,
                r#""axial_height_m": 0.192"#,
                1,
            )
            .replacen(r#""tapes_along_width": 3"#, r#""tapes_along_width": 16"#, 1)
            .replacen(
                r#"{"id": "s0", "kind": "straight", "x_m": 0.0}"#,
                r#"{"id": "s0", "kind": "straight", "x_m": 0.0},
      {"id": "a90", "kind": "arc", "azimuth_deg": 90.0}"#,
                1,
            )
    }

    /// OC-017: drives `transverse_bound` through the real run_coupled_case
    /// pipeline. The arc station's tape-edge points exceed the declared
    /// 0.2 along-current gate; with no model they are excluded, with the
    /// model declared they are bounded estimates — and the record shows
    /// the point basis either way.
    #[test]
    fn transverse_bound_marks_over_limit_along_current_points_on_a_real_run() {
        let plain = run_coupled_case(
            &along_current_case_json_v3(None),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let excluded = plain.candidates[0].point_counts.along_current_excluded;
        assert!(
            excluded > 0,
            "fixture must exercise the along-current gate for this test to mean anything"
        );
        assert_eq!(plain.candidates[0].point_counts.along_current_bounded, 0);

        let bounded = run_coupled_case(
            &along_current_case_json_v3(Some("transverse_bound")),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let counts = &bounded.candidates[0].point_counts;
        // The bounded run resolves at least the points the plain run
        // excluded below the 0.5 ceiling; none remain excluded unless the
        // fixture produced fractions above the ceiling.
        assert!(
            counts.along_current_bounded > 0,
            "declared model must label over-limit points as bounded, not excluded"
        );
        assert!(
            counts.along_current_bounded + counts.along_current_excluded >= excluded,
            "bounded + still-excluded must account for the points the gate caught"
        );
        // Every bounded point is a determined basis: it carries a queried
        // k_used and contributes to capacity like an estimate.
        let bounded_points: Vec<_> = bounded
            .stations
            .iter()
            .flat_map(|station| station.tapes.iter())
            .flat_map(|tape| tape.per_candidate.iter())
            .flat_map(|candidate| candidate.points.iter())
            .filter(|p| p.basis == PointBasis::AlongCurrentBounded)
            .collect();
        assert!(!bounded_points.is_empty());
        for point in bounded_points {
            assert!(
                point.k_used_a_per_m.is_some(),
                "AlongCurrentBounded must carry a queried value"
            );
            assert!(point.along_current_fraction > 0.2);
            assert!(
                point.along_current_fraction <= optcoil_model::coupled::ALONG_CURRENT_BOUND_CEILING
            );
        }
    }

    /// The model's ceiling is hard: a point whose along-current fraction
    /// exceeds `ALONG_CURRENT_BOUND_CEILING` stays `AlongCurrentExcluded`
    /// even with `transverse_bound` declared — the bound's footing ends
    /// there and the evaluator does not reach past it.
    #[test]
    fn transverse_bound_still_excludes_points_above_the_model_ceiling() {
        let case =
            CoupledCase::from_json(&along_current_case_json_v3(Some("transverse_bound"))).unwrap();
        let dataset = MaterialDataset::embedded().unwrap();
        let interpolator = IcInterpolator::with_method(
            &dataset.points,
            dataset.metadata.max_cell_spans,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let frame = TapeFrame {
            t: [0.0, 0.0, 1.0],
            n: [1.0, 0.0, 0.0],
            w: [0.0, 1.0, 0.0],
        };
        // |B| = 2 T with B_n = 1.6, B_t = 1.2: fraction 0.6 > 0.5 ceiling.
        let over = evaluate_candidate_point(
            &interpolator,
            &case,
            &case.material,
            true,
            &frame,
            0,
            [1.6, 0.0, 1.2],
            870.0,
            1.0,
            None,
        )
        .unwrap();
        assert_eq!(over.basis, PointBasis::AlongCurrentExcluded);
        assert_eq!(over.status, Status::Inconclusive);
        assert_eq!(over.k_used_a_per_m, None);

        // |B| ~ 2.03 T with B_n = 1.94, B_t = 0.6: fraction ~0.296, inside
        // the bounded interval (0.2, 0.5] — the same query runs and is
        // labeled bounded.
        let within = evaluate_candidate_point(
            &interpolator,
            &case,
            &case.material,
            true,
            &frame,
            0,
            [1.94, 0.0, 0.6],
            870.0,
            1.0,
            None,
        )
        .unwrap();
        assert_eq!(within.basis, PointBasis::AlongCurrentBounded);
        assert!(within.k_used_a_per_m.is_some());
    }

    /// Regression for the masked-unsupported crash: a point inside the
    /// bounded fraction interval whose *query* is out of domain has
    /// nothing to bound. It must keep the underlying basis (unsupported,
    /// INCONCLUSIVE, no k_used) — labeling it `AlongCurrentBounded` made
    /// `all_determined` unwrap a missing value and panic.
    #[test]
    fn transverse_bound_keeps_underlying_basis_when_the_query_has_no_value() {
        let case =
            CoupledCase::from_json(&along_current_case_json_v3(Some("transverse_bound"))).unwrap();
        let dataset = MaterialDataset::embedded().unwrap();
        let interpolator = IcInterpolator::with_method(
            &dataset.points,
            dataset.metadata.max_cell_spans,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let frame = TapeFrame {
            t: [0.0, 0.0, 1.0],
            n: [1.0, 0.0, 0.0],
            w: [0.0, 1.0, 0.0],
        };
        // |B| ~ 27.5 T with B_n = 26, B_t = 9: fraction ~0.327, inside the
        // bounded interval — but 27.5 T is far past the measured 8 T
        // ceiling, so the query itself is unsupported.
        let point = evaluate_candidate_point(
            &interpolator,
            &case,
            &case.material,
            true,
            &frame,
            0,
            [26.0, 0.0, 9.0],
            870.0,
            1.0,
            None,
        )
        .unwrap();
        assert!(
            point.along_current_fraction > 0.2
                && point.along_current_fraction
                    <= optcoil_model::coupled::ALONG_CURRENT_BOUND_CEILING
        );
        assert_ne!(point.basis, PointBasis::AlongCurrentBounded);
        assert_eq!(point.k_used_a_per_m, None);
        assert_eq!(point.status, Status::Inconclusive);
    }

    /// v2 variant of the fixture: same case, schema
    /// `optcoil-coupled-conductor/v2` with `limits.self_field_correction`
    /// set to `model` (or absent when `model` is `None`).
    fn self_field_gate_case_json_v2(model: Option<&str>, turns_along_normal: u32) -> String {
        let correction = model
            .map(|m| format!(r#", "self_field_correction": "{m}""#))
            .unwrap_or_default();
        self_field_gate_case_json(1.0e6)
            .replacen(
                "\"optcoil-coupled-conductor/v1\"",
                "\"optcoil-coupled-conductor/v2\"",
                1,
            )
            .replacen(
                "\"utilization_limit\": 0.8",
                &format!("\"utilization_limit\": 0.8{correction}"),
                1,
            )
            .replacen(
                "\"turns_along_normal\": 5",
                &format!("\"turns_along_normal\": {turns_along_normal}"),
                1,
            )
            .replacen(
                "\"turn_indices_along_normal\": [1, 2]",
                "\"turn_indices_along_normal\": [1]",
                1,
            )
    }

    // OC-014 Phase 2: coupled-conductor v4 carries the declared layer
    // thickness alongside the critical_state_strip model value.
    fn self_field_gate_case_json_v4(turns_along_normal: u32, layer_thickness_m: f64) -> String {
        self_field_gate_case_json(1.0e6)
            .replacen(
                "\"optcoil-coupled-conductor/v1\"",
                "\"optcoil-coupled-conductor/v4\"",
                1,
            )
            .replacen(
                "\"utilization_limit\": 0.8",
                &format!(
                    "\"utilization_limit\": 0.8, \"self_field_correction\": \"critical_state_strip\", \"critical_state_layer_thickness_m\": {layer_thickness_m}"
                ),
                1,
            )
            .replacen(
                "\"turns_along_normal\": 5",
                &format!("\"turns_along_normal\": {turns_along_normal}"),
                1,
            )
            .replacen(
                "\"turn_indices_along_normal\": [1, 2]",
                "\"turn_indices_along_normal\": [1]",
                1,
            )
    }

    #[test]
    fn critical_state_strip_evaluates_the_dominance_point_uniform_transport_demotes() {
        // OC-014 Phase 2: at a transport ratio > 1 the uniform_transport
        // correction is no longer a bound and the candidate is demoted to
        // INCONCLUSIVE. The critical_state_strip edge-field bound is a strict
        // bound at any ratio, so the same winding resolves to a determined
        // verdict. Same case, same current — only the correction differs.
        let uniform = run_coupled_case(
            &self_field_gate_case_json_v2(Some("uniform_transport"), 1),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let uc = &uniform.candidates[0];
        assert!(
            uc.max_transport_self_field_ratio.unwrap() > 1.0,
            "fixture must reach the dominance boundary"
        );
        assert_eq!(
            uc.status,
            Status::Inconclusive,
            "uniform_transport demotes a dominance point to inconclusive"
        );

        let critical = run_coupled_case(
            &self_field_gate_case_json_v4(1, 1.0e-6),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let cc = &critical.candidates[0];
        assert!(cc.max_transport_self_field_ratio.unwrap() > 1.0);
        assert!(
            matches!(cc.status, Status::Pass | Status::Fail),
            "critical_state_strip must keep a dominance point determined, got {:?}",
            cc.status
        );
        // Every determined point carries the recorded edge-field bound and a
        // query field raised above the applied magnitude.
        for station in &critical.stations {
            for tape in &station.tapes {
                for point in &tape.per_candidate[0].points {
                    if matches!(point.basis, PointBasis::Estimate | PointBasis::LowerBound) {
                        assert!(
                            point.critical_state_edge_field_t.unwrap() > 0.0,
                            "determined point must record the edge-field bound"
                        );
                        assert!(
                            point.query_field_t > point.magnitude_t,
                            "corrected query field must exceed the applied magnitude"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn critical_state_table_bounds_and_field_consistent_solve() {
        // OC-014 Phase 2b (field-consistent bound): the bounding table is
        // the max over every measured angle level at each nominal field
        // level, suffix-maxed into a non-increasing bounding function. At
        // OC-012's operating point (T = 21 K, clamp = 0.0501 T) it must
        // build, its floor value must sit at/above the parallel-field Ic
        // (the angle maximum), and the consistent solve must land below
        // the strict floor bound while staying positive — the edge zone
        // at elevated field sustains less than the floor Ic.
        let dataset =
            MaterialDataset::embedded_by_id(optcoil_model::material::SUPERPOWER_MODELEXT_ID)
                .unwrap();
        let interpolator = IcInterpolator::with_method(
            &dataset.points,
            dataset.metadata.max_cell_spans,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let table = build_critical_state_field_table(&interpolator, 21.0, 0.0501)
            .expect("the measured angle axis must resolve at the declared operating point");
        let floor_k = table.k_bound_at(0.0501);
        let parallel = interpolator
            .evaluate([21.0, 0.0501, 90.0])
            .unwrap()
            .expect("parallel floor query must resolve")
            .ic_a_per_m;
        assert!(
            floor_k >= parallel,
            "the all-angle floor bound {floor_k} must reach the parallel maximum {parallel}"
        );
        let tape_width_m = 0.012;
        let layer_m = 1.0e-6;
        let strict = tape_frame::critical_state_edge_field_bound_t(floor_k, tape_width_m, layer_m);
        // Deep null: applied 0.13 T — the consistent bound must be
        // strictly tighter than the floor bound yet still positive.
        let consistent = table.consistent_edge_field_t(0.13, tape_width_m, layer_m, 0.0501);
        assert!(
            consistent > 0.0 && consistent < strict,
            "consistent bound {consistent} must tighten below strict {strict}"
        );
        // Bound property at the solve: hi end still satisfies
        // B_edge >= (mu0/2pi) k_bound(B_app + B_edge) ln(w/d).
        let residual = tape_frame::critical_state_edge_field_bound_t(
            table.k_bound_at(0.13 + consistent),
            tape_width_m,
            layer_m,
        );
        assert!(
            consistent >= residual - 1e-9,
            "consistent bound {consistent} must remain >= f({consistent}) = {residual}"
        );
        // High applied field tightens further: the edge zone at 6+ T
        // carries far less than at the floor.
        let high_field = table.consistent_edge_field_t(6.0, tape_width_m, layer_m, 0.0501);
        assert!(
            high_field < consistent,
            "edge bound at 6 T applied ({high_field}) must tighten below the null value ({consistent})"
        );
        assert!(
            build_critical_state_field_table(&interpolator, 999.0, 0.0501).is_none(),
            "outside coverage the whole table fails: None, not a guess"
        );
    }

    #[test]
    fn self_field_correction_raises_the_query_magnitude_and_is_recorded() {
        // OC-014: under limits.self_field_correction = "uniform_transport"
        // every evaluated point's query magnitude is |B_applied| +
        // mu0*K_tape/2, transport_self_field_ratio is recorded, and the
        // corrected capacity is never higher than the uncorrected one
        // (higher query field -> lower Ic -> conservative direction).
        let corrected = run_coupled_case(
            &self_field_gate_case_json_v2(Some("uniform_transport"), 5),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let uncorrected = run_coupled_case(
            &self_field_gate_case_json(1.0e6),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let candidate = &corrected.candidates[0];
        assert!(candidate.max_transport_self_field_ratio.unwrap() > 0.0);
        for station in &corrected.stations {
            for tape in &station.tapes {
                for point in &tape.per_candidate[0].points {
                    assert!(point.transport_self_field_ratio.unwrap() >= 0.0);
                    if matches!(point.basis, PointBasis::Estimate) {
                        assert!(
                            point.query_field_t > point.magnitude_t,
                            "corrected query field must exceed the applied magnitude"
                        );
                    }
                }
            }
        }
        // Per-point conservatism: wherever both runs determine a k_used,
        // the corrected one must not exceed the uncorrected one. (A
        // tape-level aggregate like min_allowed_screening_a is not the
        // right comparison: correction can push a point to unsupported,
        // dropping the tape from the min entirely — which is honest
        // INCONCLUSIVE behavior, not an increased allowance.)
        for (cs, us) in corrected.stations.iter().zip(&uncorrected.stations) {
            for (ct, ut) in cs.tapes.iter().zip(&us.tapes) {
                for (cp, up) in ct.per_candidate[0]
                    .points
                    .iter()
                    .zip(&ut.per_candidate[0].points)
                {
                    if let (Some(ck), Some(uk)) = (cp.k_used_a_per_m, up.k_used_a_per_m) {
                        assert!(
                            ck <= uk * (1.0 + 1e-12),
                            "corrected k_used {ck} > uncorrected {uk} at width_index {}",
                            cp.width_index
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn self_field_dominance_demotes_a_corrected_candidate_to_inconclusive() {
        // The transport ratio mu0*K_tape/(2*|B_applied|) is independent of
        // the test current (both sides scale with I), so dominance is a
        // property of the winding: shrink turns_along_normal to shrink the
        // applied field until the carried sheet self-field dominates.
        let corrected = run_coupled_case(
            &self_field_gate_case_json_v2(Some("uniform_transport"), 1),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        let candidate = &corrected.candidates[0];
        assert!(
            candidate.max_transport_self_field_ratio.unwrap() > 1.0,
            "fixture must actually reach the dominance boundary"
        );
        if candidate.status != Status::Fail {
            assert_eq!(candidate.status, Status::Inconclusive);
        }
        // The identical case without the correction keeps the v1 gate:
        // its limiting-point ratio gate still applies, but the candidate
        // result reports the uncorrected semantics (no transport gate).
        let v1 = run_coupled_case(
            &self_field_gate_case_json(1.0e6)
                .replacen("\"turns_along_normal\": 5", "\"turns_along_normal\": 1", 1)
                .replacen(
                    "\"turn_indices_along_normal\": [1, 2]",
                    "\"turn_indices_along_normal\": [1]",
                    1,
                ),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();
        assert!(v1.candidates[0].max_transport_self_field_ratio.unwrap() > 1.0);
    }

    #[test]
    fn v1_coupled_case_rejects_self_field_correction() {
        // The field changes screening semantics, so a v1 case declaring
        // it must fail validation rather than silently applying it.
        let case_json = self_field_gate_case_json(1.0e6).replacen(
            "\"utilization_limit\": 0.8",
            "\"utilization_limit\": 0.8, \"self_field_correction\": \"uniform_transport\"",
            1,
        );
        let err = run_coupled_case(&case_json, None, &CoupledOptions::default())
            .expect_err("v1 case with self_field_correction must be rejected");
        assert!(format!("{err}").contains("self_field_correction"));
    }

    #[test]
    fn linear_scaling_matches_a_direct_evaluation_within_1e12_relative() {
        let case = CoupledCase::embedded().unwrap();
        let geometry = Racetrack {
            straight_half_length_m: case.pack.straight_half_length_m.unwrap(),
            bend_radius_m: case.pack.bend_radius_m.unwrap(),
            radial_width_m: case.pack.radial_width_m,
            axial_height_m: case.pack.axial_height_m,
            ampere_turns_a: 1.0,
            current_model: CurrentModel::UniformWindingPack,
        };
        let unit_evaluator = RacetrackEvaluator::new(&geometry, 10).unwrap();
        let position =
            tape_center_position_m(&case.pack, &case.winding, &case.sampling.stations[0], 3, 1)
                .unwrap();
        let unit_field = unit_evaluator.evaluate(position).unwrap().field_t;
        let scaled: [f64; 3] = unit_field.map(|x| x * 1e3);

        let mut direct_geometry = geometry.clone();
        direct_geometry.ampere_turns_a = 1e3;
        let direct_evaluator = RacetrackEvaluator::new(&direct_geometry, 10).unwrap();
        let direct_field = direct_evaluator.evaluate(position).unwrap().field_t;

        for k in 0..3 {
            let relative = ((scaled[k] - direct_field[k]) / direct_field[k]).abs();
            assert!(relative < 1e-12, "component {k}: relative error {relative}");
        }
    }

    #[test]
    fn combine_mirror_pair_takes_the_minimum_and_the_weakest_basis() {
        let estimate = |k: f64, field: f64| tape_frame::AngleQuery {
            basis: tape_frame::QueryBasis::Estimate,
            query_field_t: field,
            angle_query_deg: 0.0,
            k_a_per_m: Some(k),
            n_value: Some(20.0),
        };
        let (basis, k, field) =
            combine_mirror_pair(&estimate(200_000.0, 5.0), &estimate(150_000.0, 6.0), 5.0);
        assert_eq!(basis, tape_frame::QueryBasis::Estimate);
        assert_eq!(k, Some(150_000.0));
        assert_eq!(field, 6.0);

        let unsupported = tape_frame::AngleQuery {
            basis: tape_frame::QueryBasis::Unsupported,
            query_field_t: 5.0,
            angle_query_deg: 0.0,
            k_a_per_m: None,
            n_value: None,
        };
        let (basis, k, field) = combine_mirror_pair(&unsupported, &estimate(150_000.0, 6.0), 5.0);
        assert_eq!(basis, tape_frame::QueryBasis::Unsupported);
        assert_eq!(k, None);
        assert_eq!(field, 5.0);
    }

    fn reference_and_case() -> (CoupledReference, CoupledCase, String) {
        let case = CoupledCase::embedded().unwrap();
        let case_json = optcoil_model::coupled::OC004_JSON;
        let case_sha256 = hash(case_json.as_bytes());
        let reference: CoupledReference = serde_json::from_str(OC004_REFERENCE_JSON).unwrap();
        (reference, case, case_sha256)
    }

    #[test]
    fn reference_validation_accepts_the_frozen_pair_and_rejects_common_mutations() {
        let (reference, case, case_sha256) = reference_and_case();
        let dataset = MaterialDataset::embedded().unwrap();
        let expected = expand_reference_subset(&case).unwrap();
        validate_reference(&reference, &case, &case_sha256, &dataset, &expected).unwrap();

        // Wrong case hash (a stale reference against an edited case).
        assert!(
            validate_reference(&reference, &case, "not-the-real-hash", &dataset, &expected)
                .is_err()
        );

        // Wrong source hash (a stale/tampered independent reference tool).
        let mut bad = reference.clone();
        bad.source_sha256 = "0".repeat(64);
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // Reordered points.
        let mut bad = reference.clone();
        bad.points.swap(0, 1);
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // Wrong evaluated current (not one of the case's candidates).
        let mut bad = reference.clone();
        bad.evaluated_current_a = 12345.0;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // Method/schema/component_subdivisions/mu0_h_per_m drift: this is
        // exactly the check class that once blocked this pipeline (the
        // build log records COUPLED_REFERENCE_METHOD drifting from a
        // regenerated reference's own method string).
        let mut bad = reference.clone();
        bad.method = "some-other-method/v1".into();
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        let mut bad = reference.clone();
        bad.schema = "optcoil-coupled-reference/v0".into();
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        let mut bad = reference.clone();
        bad.component_subdivisions = 8;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        let mut bad = reference.clone();
        bad.mu0_h_per_m *= 1.01;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // Orders: non-increasing, and out of the declared 2..=72 range.
        let mut bad = reference.clone();
        bad.orders = [10, 5];
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        let mut bad = reference.clone();
        bad.orders = [1, 80];
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // CSV/field-source hash drift (a stale dataset or stale OC-002 field
        // kernel bundled at compile time).
        let mut bad = reference.clone();
        bad.csv_sha256 = "0".repeat(64);
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        let mut bad = reference.clone();
        bad.field_source_sha256 = "0".repeat(64);
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // elapsed_seconds must be finite and nonnegative.
        let mut bad = reference.clone();
        bad.elapsed_seconds = -1.0;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // Point-count mismatch (truncation).
        let mut bad = reference.clone();
        bad.points.pop();
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // A position that drifts from the case's own re-derived geometry
        // by more than the declared 1e-12 m tolerance.
        let mut bad = reference.clone();
        bad.points[0].position_m[0] += 1e-6;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());

        // A nonfinite field value anywhere in the reference is rejected.
        let mut bad = reference;
        bad.points[0].field_t[0] = f64::NAN;
        assert!(validate_reference(&bad, &case, &case_sha256, &dataset, &expected).is_err());
    }

    #[test]
    fn nulled_reference_capacity_values_fail_independent_comparison() {
        // Contract §9.1: a reference whose K values were all removed (basis
        // strings kept) must not silently pass -- the capacity comparison
        // must record capacity_missing and independent_comparison must FAIL.
        // Built on the small, cheap self_field_gate_case_json fixture
        // (quadrature orders [2, 4]; 1 station, 1 tape, a declared
        // reference_subset expanding to 5 width points) rather than the
        // full 900-point embedded OC-004 case, which is deliberately not
        // re-run in debug tests (see the comment above
        // mu0_constant_used_by_reference_validation_matches_the_physics_kernel).
        let case_json = self_field_gate_case_json(1.0);
        let case_sha256 = hash(case_json.as_bytes());
        let dataset = MaterialDataset::embedded().unwrap();

        // A real (if small) run supplies genuine field/angle/capacity
        // values so the hand-built reference below is internally
        // consistent, not fabricated physics.
        let baseline = run_coupled_case(&case_json, None, &CoupledOptions::default()).unwrap();
        let tape = &baseline.stations[0].tapes[0];
        let candidate = &tape.per_candidate[0];
        assert_eq!(candidate.points.len(), 5, "fixture declares 5 width points");

        let points: Vec<ReferencePoint> = tape
            .points
            .iter()
            .zip(candidate.points.iter())
            .map(|(geometry, cand)| ReferencePoint {
                station: tape.station.clone(),
                tape_index: tape.tape_index,
                turn_index: tape.turn_index,
                width_index: geometry.width_index,
                position_m: geometry.position_m,
                unit_fields_t_per_ampere_turn: geometry.unit_field_t_per_ampere_turn,
                field_t: cand.field_t,
                magnitude_t: cand.magnitude_t,
                angle_raw_deg: cand.angle_raw_deg,
                angle_folded_deg: cand.angle_folded_deg,
                mirror_angle_deg: cand.mirror_angle_deg,
                along_current_fraction: cand.along_current_fraction,
                query_field_t: cand.query_field_t,
                basis: cand.basis,
                k_folded_a_per_m: cand.k_folded.k_a_per_m,
                k_mirror_a_per_m: cand.k_mirror.k_a_per_m,
                k_used_a_per_m: cand.k_used_a_per_m,
            })
            .collect();
        assert!(
            points
                .iter()
                .any(|p| matches!(p.basis, PointBasis::Estimate | PointBasis::LowerBound)),
            "fixture must produce at least one determined point for this test to be meaningful"
        );

        let reference = CoupledReference {
            schema: COUPLED_REFERENCE_SCHEMA.into(),
            case_sha256: case_sha256.clone(),
            method: COUPLED_REFERENCE_METHOD.into(),
            source_sha256: hash(REFERENCE_OC004_SOURCE.as_bytes()),
            field_source_sha256: hash(REFERENCE_OC002_SOURCE.as_bytes()),
            source_path: "tools/reference_oc004.py".into(),
            csv_sha256: dataset.metadata.csv_sha256.clone(),
            python_version: "3.14.4".into(),
            numpy_version: "2.3.5".into(),
            platform: "unit-test-fixture".into(),
            orders: [10, 14],
            component_subdivisions: 16,
            mu0_h_per_m: MU0_H_PER_M,
            evaluated_current_a: 870.0,
            points,
            elapsed_seconds: 0.0,
            limitations: Vec::new(),
        };
        let reference_json = serde_json::to_string(&reference).unwrap();

        // Sanity check: the unmodified, internally-consistent reference
        // passes independent_comparison (it agrees with itself exactly),
        // so the FAIL demonstrated below is caused by the mutation, not by
        // a broken fixture.
        let clean = run_coupled_case(
            &case_json,
            Some(reference_json.as_str()),
            &CoupledOptions::default(),
        )
        .unwrap();
        assert_eq!(
            clean
                .checks
                .iter()
                .find(|c| c.id == "independent_comparison")
                .unwrap()
                .status,
            Status::Pass,
            "the unmutated, self-consistent reference must pass before the mutation is meaningful"
        );
        let clean_rc = clean.reference_comparison.as_ref().unwrap();
        assert!(!clean_rc.capacity_missing);
        assert!(clean_rc.compared_capacity_points > 0);

        // Now null every k_*_a_per_m value, basis strings unchanged -- the
        // exact defect described in contract §9.1.
        let mut nulled = reference;
        for point in &mut nulled.points {
            point.k_folded_a_per_m = None;
            point.k_mirror_a_per_m = None;
            point.k_used_a_per_m = None;
        }
        let nulled_json = serde_json::to_string(&nulled).unwrap();
        let record = run_coupled_case(
            &case_json,
            Some(nulled_json.as_str()),
            &CoupledOptions::default(),
        )
        .unwrap();
        let independent_comparison = record
            .checks
            .iter()
            .find(|c| c.id == "independent_comparison")
            .unwrap();
        assert_eq!(
            independent_comparison.status,
            Status::Fail,
            "a reference with every capacity value nulled must FAIL independent_comparison, not silently pass"
        );
        let rc = record.reference_comparison.as_ref().unwrap();
        assert!(rc.capacity_missing);
        assert_eq!(
            rc.compared_capacity_points, 0,
            "no capacity comparison can be made once every K value is null"
        );
    }

    #[test]
    fn circular_diff_wraps_across_the_period_180_boundary() {
        assert!((circular_diff_deg(0.0, 0.0)).abs() < 1e-12);
        assert!((circular_diff_deg(1.0, 179.0) - 2.0).abs() < 1e-9);
        assert!((circular_diff_deg(0.0, 90.0) - 90.0).abs() < 1e-9);
    }

    // The full embedded 900-point grid against three current candidates is
    // deliberately NOT re-run here: it is exercised end-to-end by the CLI
    // integration test (`coupled-benchmark --json`) and by the explicit
    // `cargo run --release -- coupled-benchmark` step, both far cheaper in
    // a release build than repeating it inside `cargo test`'s debug build.
    #[test]
    fn mu0_constant_used_by_reference_validation_matches_the_physics_kernel() {
        assert!((MU0_H_PER_M - 4.0 * std::f64::consts::PI * 1e-7).abs() < 1e-30);
    }

    /// Coupled-case schema v7: a case declaring `sampling.field_map` runs
    /// entirely off the declared grid — zero kernel evaluations, zero
    /// refinement change, map provenance on the record — and the declared
    /// (B_r, B_z) arrive in the lab frame at the right amplitude.
    #[test]
    fn field_map_case_evaluates_declared_fields_without_the_kernel() {
        let mut case: serde_json::Value =
            serde_json::from_str(optcoil_model::coupled::OC014_JSON).unwrap();
        case["schema"] = serde_json::json!("optcoil-coupled-conductor/v7");
        case["pack"] = serde_json::json!({
            "path": {
                "segments": [{"kind": "arc", "radius_m": 0.25, "sweep_deg": 360.0}],
                "start_position_m": [0.25, 0.0],
                "start_heading_deg": 90.0
            },
            "radial_width_m": 0.02,
            "axial_height_m": 0.036
        });
        case["winding"] = serde_json::json!({
            "tape_width_m": 0.01,
            "tape_normal": "axial",
            "turns_along_normal": 4,
            "tapes_along_width": 2
        });
        case["sampling"]["stations"] = serde_json::json!([
            {"id": "p0", "kind": "path", "s_m": 0.0}
        ]);
        case["sampling"]["turn_indices_along_normal"] = serde_json::json!([1, 4]);
        case["sampling"]["tape_indices_along_width"] = serde_json::json!([1, 2]);
        // rho cell centres [0.235, 0.265] m; z centres
        // [-0.02, -0.005, 0.005, 0.02] m — the pack hull
        // r in [0.24, 0.26], z in [-0.018, 0.018] is inside the
        // half-spacing-extended nearest-node hull.
        let mut entries = Vec::new();
        for i in 0..2u32 {
            for j in 0..4u32 {
                entries.push(serde_json::json!({
                    "rho_index": i,
                    "z_index": j,
                    "br_t": 0.1 * f64::from(i) - 0.02 * f64::from(j),
                    "bz_t": 1.0 + 0.5 * f64::from(i),
                }));
            }
        }
        case["sampling"]["field_map"] = serde_json::json!({
            "source_sha256": "f".repeat(64),
            "components": "cylindrical_br_bz",
            "reference_ampere_turns_a": 1.0e6,
            "rho_levels_m": [0.235, 0.265],
            "z_levels_m": [-0.02, -0.005, 0.005, 0.02],
            "entries": entries,
        });
        case["numerics"]["reference_subset"] = serde_json::json!([
            {"station": "p0", "tape_index": 1, "turn_index": 1}
        ]);

        let record = run_coupled_case(
            &serde_json::to_string(&case).unwrap(),
            None,
            &CoupledOptions::default(),
        )
        .unwrap();

        assert_eq!(record.schema, "optcoil-coupled-run/v8");
        assert_eq!(record.kernel_evaluations, 0);
        assert_eq!(
            record.field_model_id,
            "customer-declared-field-map/cylindrical-br-bz/v1"
        );
        let fm = record.field_map.as_ref().expect("map provenance recorded");
        assert_eq!(fm.source_sha256, "f".repeat(64));
        assert_eq!(fm.rho_nodes, Some(2));
        assert_eq!(fm.z_nodes, Some(4));
        assert_eq!(fm.x_nodes, None);
        assert_eq!(fm.reference_ampere_turns_a, 1.0e6);
        assert!(record.limitations.iter().any(|l| l.contains("field map")));

        let group = &record.stations[0];
        assert_eq!(group.station, "p0");
        for tape in &group.tapes {
            for p in &tape.points {
                // Declared map: no in-map detail to refine toward.
                assert_eq!(p.refinement_change_t_per_ampere_turn, 0.0);
                assert_eq!(
                    p.unit_field_t_per_ampere_turn[0],
                    p.unit_field_t_per_ampere_turn[1]
                );
            }
        }

        // Tape 1 sits at rho offset -0.005 -> r_abs = 0.245; its width
        // nodes span r in [0.24, 0.25] -> nearest rho level 0.235 (i=0).
        // Turn 1 sits at z = -0.0135 -> nearest z level -0.02 (j=0):
        // (B_r, B_z) = (0.0, 1.0) -> unit = (0, 0, 1e-6).
        let tape1 = group
            .tapes
            .iter()
            .find(|t| t.tape_index == 1 && t.turn_index == 1)
            .unwrap();
        for p in &tape1.points {
            let expected = [0.0, 0.0, 1.0e-6];
            for k in 0..3 {
                assert!(
                    (p.unit_field_t_per_ampere_turn[1][k] - expected[k]).abs() < 1e-15,
                    "tape1 width {}: {:?} vs {:?}",
                    p.width_index,
                    p.unit_field_t_per_ampere_turn[1],
                    expected
                );
            }
        }
        // Tape 2 sits at r_abs = 0.255; width xi=+1 puts it at r = 0.26
        // -> nearest rho 0.265 (i=1): (B_r, B_z) = (0.1, 1.5). At the
        // p0 station (position +x, normal +x = r-hat), B_r maps to +x.
        let tape2 = group
            .tapes
            .iter()
            .find(|t| t.tape_index == 2 && t.turn_index == 1)
            .unwrap();
        let last = tape2.points.last().unwrap();
        let expected = [0.1e-6, 0.0, 1.5e-6];
        for k in 0..3 {
            assert!(
                (last.unit_field_t_per_ampere_turn[1][k] - expected[k]).abs() < 1e-15,
                "tape2 outer edge: {:?} vs {:?}",
                last.unit_field_t_per_ampere_turn[1],
                expected
            );
        }
        // Turn 4 sits at z = +0.0135 -> nearest z level 0.02 (j=3): the
        // map's declared B_r = 0.1*i - 0.06 differs per rho node.
        let t2t4 = group
            .tapes
            .iter()
            .find(|t| t.tape_index == 2 && t.turn_index == 4)
            .unwrap();
        let center = &t2t4.points[2]; // xi = 0 -> r = 0.255 -> i=1, j=3
        let expected = [0.04e-6, 0.0, 1.5e-6];
        for k in 0..3 {
            assert!(
                (center.unit_field_t_per_ampere_turn[1][k] - expected[k]).abs() < 1e-15,
                "tape2 turn4 centre: {:?} vs {:?}",
                center.unit_field_t_per_ampere_turn[1],
                expected
            );
        }
    }
}
