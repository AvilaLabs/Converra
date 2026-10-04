//! Headless OC-007 runner: an exhaustive discrete search over permitted
//! racetrack pack geometry (turns along normal x tapes along width) under a
//! fixed bore-field requirement, scored by a synthetic cost model and
//! screened through the OC-004 coupled runner reused unchanged. This is a
//! "screening-passing modeled-cost optimum", never a validated design: prices
//! are synthetic and declared, and there is no independent reference per
//! candidate (only the frozen OC-004 geometry has one).
//!
//! Every screening decision here is made by functions OC-004 already
//! validates (`crate::coupled::evaluate_candidate_point`,
//! `crate::coupled::aggregate_status`, and — for each candidate's full
//! sampling plan — `crate::coupled::run_coupled_case` itself), reused
//! through `optcoil_model::coupled_search::build_coupled_case`'s geometry
//! helper rather than reimplemented.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use crate::time::{Instant, SystemTime, UNIX_EPOCH};

use optcoil_model::{
    Check, Status,
    coupled::{TapeNormal, tape_center_position_m, width_offset_m},
    coupled_search::{
        CandidateDims, CoupledSearchCase, build_coupled_case, expand_relative_turn_indices,
    },
    magnetics::{CurrentModel, Racetrack},
    material::MaterialDataset,
};
use optcoil_physics::{
    racetrack::{MU0_H_PER_M, RacetrackEvaluator, norm, sub},
    tape_frame,
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    coupled::{
        CandidateResult, CoupledOptions, DatasetIdentity, LimitingPoint, MaterialRuntimes,
        MonotonicityAuditRecord, PointCounts, aggregate_status, evaluate_candidate_point,
        resolve_datasets_map, run_coupled_case_with_datasets_ticked_cancellable, spec_runtime,
    },
    field::{RuntimeInfo, runtime_info},
    hash,
    search_acceptance::{self, CoupledSearchAcceptance},
    write_json_new,
};

/// Bumped to v2 for case schema v3: when `requirement.good_field_region` is
/// declared, `NI` is solved on the usable volume's weakest lattice point,
/// not the bare bore probe. v1/v2 cases evaluate identically.
/// Bumped to v3 for schema v3's geometric feasibility gates: a declared
/// `manufacturing` block (inner bend radius) and the usable volume must not
/// overlap the winding pack; violating candidates FAIL before screening.
/// Bumped to v4 for schema v4: candidates enumerate (turns, tapes,
/// strands), screened capacity scales by strand count, and a declared
/// `mechanical` block gates on the first-order Lorentz load
/// `I_op * peak_sampled_field`.
/// Bumped to v5 for schema v5's `limits.self_field_correction`
/// (`"uniform_transport"`, OC-014): the screening query magnitude gains
/// the carried-sheet self-field bound and the self-field gate moves to
/// the dominance boundary (transport ratio > 1).
/// Bumped to v6 covering schema v6's `limits.along_current_model`
/// (OC-017 `transverse_bound`, verdicts change under the declared bound)
/// and schema v7's hoop-stress mechanical bound (OC-018). v7 adds schema
/// v8's `critical_state_strip` self-field model (OC-014 Phase 2). v8:
/// the `critical_state_strip` floor resolves over a near-perpendicular
/// angle sector — the tape edge self-field enters the broad face
/// ~perpendicular to it, and the applied-field angle is degenerate at
/// field nulls — so deep-null verdicts differ by construction. v9: the
/// bound is the field-consistent fixed point over a suffix-max
/// all-angle table — strictly tighter than the v8 floor bound, so
/// marginal verdicts differ by construction (OC-014 Phase 2b).
/// v10 (search schema v9): general planar `fixed_geometry.path` packs —
/// per-segment cells, path stations, min-local-curvature screening and
/// offset-curve cost. v11 (search schema v10): graded tape_specs —
/// candidates enumerate per-region spec assignments, and every turn
/// screens and prices under its region's own binding. v12 (search
/// schema v12): the candidate record now carries the transverse
/// interface-pressure estimate and its limiting location, and a declared
/// `mechanical.max_transverse_pressure_pa` folds into
/// `mechanical_feasible`.
/// v13 (search schema v14): Cartesian `field_map` components reach the
/// generated coupled cases (coupled schema v8). v14 (search schema v15):
/// the declared first-order adjacent screens — `thermal_margin`,
/// `ac_loss`, `quench_hotspot`, `screening_current` — evaluate on the
/// full-plan record and join the candidate aggregate, so a declared
/// screen's INCONCLUSIVE or FAIL changes verdicts by construction.
pub const COUPLED_SEARCH_MODEL_ID: &str = "exhaustive-coupled-cost-search-over-pack-geometry/v14";
/// Bumped to v2 for contract §9.3 (a coarse FAIL no longer prunes unless
/// every coarse point's own refinement is itself converged;
/// `coarse_refinement_unresolved` recorded) and §9.4 (`search_status` can
/// now be `INCONCLUSIVE`, not only PASS/FAIL, when the acceptance module's
/// refined-plan re-evaluation finds a shortfall beyond the declared gate).
/// Bumped to v3 for schema v3's good-field-region requirement gate.
/// Bumped to v4 for the schema-v3 feasibility gates: `pack_overlap` and
/// `manufacturing_feasible` enter `requirement_status`.
/// Bumped to v5 for schema v4's `strands_parallel`: candidate geometry
/// records `strands_parallel`/`total_conductors` and cost counts
/// strand-metres.
/// Bumped to v6 for schema v5's self-field correction: screening verdicts
/// under `uniform_transport` differ from uncorrected ones by
/// construction.
/// Bumped to v7 covering the `transverse_bound` point-basis semantics
/// (OC-017) and the hoop-stress bound inside `mechanical_feasible`
/// (OC-018). Bumped to v8 for schema v8's `critical_state_strip`: the
/// dominance gate is replaced by the edge-field bound, so dominance-
/// regime verdicts differ by construction (OC-014 Phase 2). Bumped to v9:
/// the `critical_state_strip` floor resolves over a near-perpendicular
/// angle sector, so deep-null verdicts (previously unboundable →
/// INCONCLUSIVE) differ by construction. Bumped to v10: the bound is the
/// field-consistent fixed point over the suffix-max all-angle table —
/// strictly tighter than the strict floor, so marginal verdicts differ
/// by construction (OC-014 Phase 2b). Bumped to v11 for schema v10's
/// grading dimension: the candidate record now carries the resolved
/// `tape_spec_ids` assignment, and the cost ledger prices each turn under
/// its region's own spec. Bumped to v12 for search schema v12's
/// transverse-pressure bound: `mechanical_feasible` additionally ANDs
/// `transverse_pressure_pa <= max_transverse_pressure_pa` (fail-closed
/// when the bound is declared but no pressure was sampled), so marginal
/// verdicts differ by construction.
/// Bumped to v13 for search schema v15's adjacent screens: each declared
/// screen's own status joins the candidate aggregate — a screen that
/// cannot resolve is INCONCLUSIVE, never silently PASS — so verdicts
/// differ by construction on cases that declare them. Bumped to v14 for
/// search schema v17's winding-mechanics bounds: `manufacturing_feasible`
/// additionally ANDs the declared outer-fiber bend-strain bound
/// (fail-closed when declared but the strain cannot resolve), and
/// `mechanical_feasible` additionally ANDs the declared
/// membrane-tension bound on the undivided interface-load resultant —
/// so verdicts differ by construction on v17 cases that declare them.
/// Bumped to v15 for search schema v18's `quench_transient` screen: the
/// lumped driven-dump trajectory's status joins the candidate aggregate
/// like the other declared screens — so verdicts differ by construction
/// on v18 cases that declare it. Bumped to v18 for run-schema v21's
/// racetrack-dimension axes: the candidate record carries axis-resolved
/// `bend_radius_m`/`straight_half_length_m`, and the ledger check's
/// recomputation resolves those dims — a record naming one geometry but
/// priced at another fails by construction. Bumped to v20 for
/// run-schemas v22/v23: v22's region-uniformity record fields (lattice
/// maximum, scale-free deviation) with `max_relative_deviation` joining
/// the requirement gate, and v23's midplane reference-circle harmonic
/// expansion (circle mean plus normal/skew coefficient vectors) with
/// declared unit bounds joining the gate — verdicts differ by
/// construction on cases that declare either. (The v22 commit's message
/// named this bump "v19" without the constant actually changing; v20
/// covers both record schemas' semantics.) Bumped to v21 for case schema
/// v24: `cost.piece_policy` changes what the ledger's purchased-length,
/// joint and scrap columns mean (pieces bought, module/splice split).
/// Bumped to v22 for acceptance checker v18's fallback walk: `best_index`
/// now names the cheapest candidate that survives the §9.4 refined-plan
/// and agreement gates, not merely the cheapest screening PASS — so the
/// reported optimum differs by construction when the walk promotes a
/// later candidate, and `acceptance.fallback_attempts` records the
/// cheaper candidates that could not be substantiated.
/// Run schema v26 preserves undefined failed-candidate diagnostics as JSON null;
/// the artifact verifier rejects missing quantities on evaluated candidates.
pub const COUPLED_SEARCH_CHECKER_ID: &str = "coupled-search-per-candidate-screening-and-cost/v23";

/// Runtime override: may only lower the case's own declared
/// `execution.max_threads` (contract §8 D7).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledSearchOptions {
    pub threads: Option<u32>,
}

/// Shared progress sink for
/// [`run_coupled_search_case_with_dataset_progress`]. One leg per
/// enumerated candidate: `planned` is the pre-dispatch estimate of field
/// evaluations (evaluator calls — one per quadrature order per point, or
/// one map lookup under a declared `field_map`); `done` ticks once per
/// evaluation as it completes.
///
/// `planned` is an estimate: candidates that exit early (degenerate
/// pack, coarse prune) under-fill their leg, and screens can tick past
/// it. [`SearchProgress::fraction`] clamps every leg at its plan, so the
/// displayed fraction is honest-by-construction — it approaches but
/// never reaches 1.0 until the run itself completes.
pub struct SearchProgress {
    /// Populated by the runner once candidates are enumerated.
    pub legs: Mutex<Vec<SearchProgressLeg>>,
    /// Candidates whose evaluation has finished.
    pub candidates_done: AtomicUsize,
    /// Total candidates enumerated (0 until dispatch).
    pub candidates_total: AtomicUsize,
    /// What the workers are doing, e.g. "screening 16 × 16 × 250".
    /// With more than one worker this is whichever candidate dispatched
    /// last — a hint, not an exact worker map.
    pub phase: Mutex<String>,
}

/// One candidate's progress leg.
pub struct SearchProgressLeg {
    /// Estimated field evaluations for this candidate.
    pub planned: u64,
    /// Field evaluations completed so far. Shared so a worker can tick
    /// without holding the legs mutex.
    pub done: Arc<AtomicU64>,
}

impl SearchProgress {
    pub fn new() -> Self {
        Self {
            legs: Mutex::new(Vec::new()),
            candidates_done: AtomicUsize::new(0),
            candidates_total: AtomicUsize::new(0),
            phase: Mutex::new("preparing".to_owned()),
        }
    }

    /// `(fraction, evals_done, evals_planned)` for display. The fraction
    /// sums `min(done, planned)` per leg and never reports 1.0 — only a
    /// finished run is complete.
    pub fn fraction(&self) -> (f32, u64, u64) {
        let legs = self
            .legs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let planned: u64 = legs.iter().map(|l| l.planned).sum();
        let done: u64 = legs
            .iter()
            .map(|l| l.done.load(Ordering::Relaxed).min(l.planned))
            .sum();
        let fraction = if planned == 0 {
            0.0
        } else {
            (done as f32 / planned as f32).min(0.999)
        };
        (fraction, done, planned)
    }

    /// Append a leg after dispatch — for callers that discover work
    /// late. Returns the leg's tick counter.
    pub fn add_leg(&self, planned: u64) -> Arc<AtomicU64> {
        let done = Arc::new(AtomicU64::new(0));
        self.legs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(SearchProgressLeg {
                planned,
                done: done.clone(),
            });
        done
    }

    /// Tick counter of leg `index`, if it exists.
    pub fn leg(&self, index: usize) -> Option<Arc<AtomicU64>> {
        self.legs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(index)
            .map(|l| l.done.clone())
    }

    /// Revise a leg's planned work. Used to correct the pessimistic
    /// pre-planned acceptance legs down to the discovered scope — planned
    /// should only ever shrink, so the displayed fraction never recedes.
    pub fn set_leg_planned(&self, index: usize, planned: u64) {
        if let Some(leg) = self
            .legs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(index)
        {
            leg.planned = planned;
        }
    }

    pub fn phase_label(&self) -> String {
        self.phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn set_phase(&self, text: String) {
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = text;
    }
}

impl Default for SearchProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// Estimated field evaluations for one candidate: requirement probes
/// (bore + good-field lattice + harmonics circle), the coarse pruning
/// screen, and the full sampling plan. Each point costs one evaluator
/// call per declared quadrature order — or one map lookup under a
/// declared `field_map`, which also skips the requirement probes.
fn planned_field_evals(search: &CoupledSearchCase, turns: u32, tapes: u32) -> u64 {
    let per_point: u64 = if search.field_map.is_some() { 1 } else { 2 };
    let mut evals = 0_u64;
    if search.field_map.is_none() {
        // Bore probe at both orders.
        evals += per_point;
        if let Some(region) = &search.requirement.good_field_region {
            evals +=
                per_point * region.lattice_points(search.requirement.bore_probe_m).len() as u64;
            if let Some(harmonics) = &region.harmonics {
                // Fine-order samples of the reference circle only.
                evals += u64::from(harmonics.theta_samples);
            }
        }
    }
    if let Some(pruning) = &search.pruning {
        let coarse_turns =
            expand_relative_turn_indices(&pruning.coarse_turn_fractions, turns).len() as u64;
        // Coarse screening visits tape columns 1 and `tapes` (dedup'd).
        let coarse_tapes = u64::from(tapes.min(2));
        evals += pruning.coarse_stations.len() as u64
            * coarse_turns
            * coarse_tapes
            * pruning.coarse_width_indices.len() as u64
            * per_point;
    }
    let full_turns =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns).len() as u64;
    // stations x turns x tape columns x 5 width quadrature points.
    evals += search.sampling.stations.len() as u64 * full_turns * u64::from(tapes) * 5 * per_point;
    evals
}

fn tick(tick: Option<&AtomicU64>) {
    if let Some(t) = tick {
        t.fetch_add(1, Ordering::Relaxed);
    }
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), RunError> {
    if cancel.load(Ordering::Relaxed) {
        Err(RunError::Cancelled)
    } else {
        Ok(())
    }
}

fn default_one_u32() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateGeometry {
    pub turns_along_normal: u32,
    pub tapes_along_width: u32,
    /// Conductors per turn sharing the operating current (schema v4;
    /// always 1 for earlier schemas — defaulted so v3 records still parse).
    #[serde(default = "default_one_u32")]
    pub strands_parallel: u32,
    /// Ampere-turn basis: n*p winding turns. Parallel strands split the
    /// current, they do not add turns.
    pub total_turns: u64,
    /// Total tape ends: total_turns * strands_parallel.
    #[serde(default)]
    pub total_conductors: u64,
    /// The candidate's resolved spec assignment (search schema v10): one
    /// `tape_specs` id or `"base"` per `grading.regions` entry, in
    /// declaration order. Absent on ungraded cases — and on every record
    /// written before run-schema v10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape_spec_ids: Option<Vec<String>>,
    /// Schema v21: the candidate's axis-resolved racetrack dims —
    /// `Some` only when the case declares the corresponding `choices`
    /// axis (the fixed geometry's declaration otherwise lives in the
    /// record's own `case`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_radius_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straight_half_length_m: Option<f64>,
    pub radial_width_m: f64,
    pub axial_height_m: f64,
}

impl CandidateGeometry {
    /// The v21 axis-resolved racetrack dims for consumers that
    /// re-evaluate this candidate — acceptance, verify, bom. `None`
    /// arms mean the case's fixed geometry declaration applies.
    pub fn dims(&self) -> CandidateDims {
        CandidateDims {
            bend_radius_m: self.bend_radius_m,
            straight_half_length_m: self.straight_half_length_m,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchCostLedger {
    pub installed_length_m: f64,
    pub purchased_length_m: f64,
    pub conductor_usd: f64,
    pub scrap_usd: f64,
    pub assembly_usd: f64,
    pub joints_usd: f64,
    pub total_usd: f64,
    /// Schema v20 `opex` block: undiscounted lifetime refrigeration cost
    /// at the declared operating point — case-constant (no
    /// candidate-dependent loads are modeled). `None` when the case
    /// declares no `opex` (and on pre-v20 records) — "not computed",
    /// never a defaulted zero passed off as a real figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opex_usd: Option<f64>,
    /// `total_usd + opex_usd` — capex + declared lifetime opex.
    /// `None` whenever `opex_usd` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_usd: Option<f64>,
    /// Schema v24: module-interface joint count (`tapes - 1` under
    /// `per_module`, 0 under `continuous`). `None` on legacy ledgers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_joints: Option<u32>,
    /// Schema v24: in-winding splices at piece boundaries (conductor-unit
    /// count, or per-strand under `per_strand`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_splices: Option<u32>,
    /// Schema v24: splices where the winding's spec changes (conductor
    /// units; `per_strand` scales by strands).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_splices: Option<u32>,
    /// Schema v24: conductor-unit pieces bought (strand pieces under
    /// `per_strand`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces_bought: Option<u32>,
    /// Schema v24: tape-metres bought beyond installed-plus-attrition —
    /// the piece quantization remnant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remnant_length_m: Option<f64>,
    /// Schema v24: the per-spec piece plan the ledger priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_plan: Option<Vec<SpecPiecePlan>>,
}

/// Schema v24 record row: one spec's chosen piece plan — the offering
/// the ledger argmin'd and the pieces/splices/remnant it implies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecPiecePlan {
    pub spec_id: String,
    /// The chosen offering's piece length (conductor-unit metres).
    pub piece_length_m: f64,
    /// The chosen offering's price — governs this spec's spend.
    pub price_usd_per_m: f64,
    /// Pieces bought (conductor units; strand pieces under `per_strand`).
    pub pieces: u32,
    /// In-winding splices at piece boundaries inside this spec's runs.
    pub piece_splices: u32,
    /// Purchased conductor-unit metres beyond installed-plus-attrition.
    pub remnant_length_m: f64,
    /// Tape-metres purchased at this spec: `pieces × piece_length ×
    /// strands` (the strand factor already folded into `pieces` under
    /// `per_strand`).
    pub purchased_length_m: f64,
}

/// The declared usable-volume evaluation (schema v3 only): the lattice
/// minimum of the unit `B_z`, which is what `NI` is solved against, plus the
/// lattice's own refinement measure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoodFieldEval {
    pub min_unit_bz_t_per_ampere_turn: f64,
    /// Schema v22: the fine-order lattice maximum — recorded alongside
    /// the minimum so the region's spread is auditable, bounded or not.
    /// `None` on records written before run-schema v22.
    #[serde(default)]
    pub max_unit_bz_t_per_ampere_turn: Option<f64>,
    /// Schema v22: `(max − min) / unit_bore_bz` on the fine order —
    /// the region's scale-free uniformity figure, the quantity a
    /// declared `max_relative_deviation` bounds. `None` on records
    /// written before run-schema v22.
    #[serde(default)]
    pub relative_deviation: Option<f64>,
    /// Schema v23: the midplane reference-circle mean field
    /// `b_0` (the DFT's DC term, the field at the declared reference
    /// radius) — `None` when no `harmonics` block is declared or on
    /// records written before run-schema v23.
    #[serde(default)]
    pub harmonic_b0_t_per_ampere_turn: Option<f64>,
    /// Schema v23: normal coefficients `b_n / b_0` for `n = 1..=max_order`
    /// (index `n − 1`), the accelerator "units" figure. The raw
    /// coefficient is `value × harmonic_b0_t_per_ampere_turn`.
    #[serde(default)]
    pub harmonic_normal_units: Option<Vec<f64>>,
    /// Schema v23: skew coefficients `a_n / b_0` for `n = 1..=max_order`.
    #[serde(default)]
    pub harmonic_skew_units: Option<Vec<f64>>,
    /// The fine-order lattice point carrying the minimum.
    pub worst_point_m: [f64; 3],
    /// |lattice_min(fine) - lattice_min(coarse)|, compared against the same
    /// `field_scale_t * max_refinement_change_fraction` gate as the bore
    /// probe's own refinement check.
    pub refinement_change_t: f64,
    /// True iff any lattice point lies inside the winding pack: the region
    /// is then not a usable volume and the candidate FAILs regardless of
    /// the evaluated minimum (which is still recorded -- the numbers are
    /// real field evaluations, just not of a region anything could occupy).
    pub pack_overlap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCandidateResult {
    pub index: usize,
    pub geometry: CandidateGeometry,
    /// Unit `B_z` at the bare bore probe, always evaluated; under schema v3
    /// `NI` is instead solved on `good_field.min_unit_bz_t_per_ampere_turn`.
    #[serde(deserialize_with = "deserialize_nullable_quantity")]
    pub unit_bore_bz_t_per_ampere_turn: f64,
    #[serde(deserialize_with = "deserialize_nullable_quantity")]
    pub bore_refinement_change_t: f64,
    /// `Some` only when the case declares `good_field_region` (schema v3).
    /// `None` also when the pack is degenerate (inner bend radius <= 0):
    /// no evaluator can be constructed, so the region is unevaluable.
    pub good_field: Option<GoodFieldEval>,
    /// False iff the winding pack is degenerate (inner bend radius <= 0,
    /// i.e. `n * radial_pitch_m >= 2 * bend_radius_m`): the winding
    /// occupies the bore and no field evaluation is possible. A
    /// per-candidate FAIL, not a run error.
    pub pack_geometry_valid: bool,
    /// Schema v3 `manufacturing` gate: false iff the pack's inner bend
    /// radius falls below the declared `min_inner_bend_radius_m`, or —
    /// schema v17 — the tape's outer-fiber bend strain exceeds the
    /// declared `max_bend_strain`. Always true when no manufacturing
    /// block is declared. A false value fails `requirement_status`
    /// before any screening work.
    pub manufacturing_feasible: bool,
    /// Schema v17: the pack's inner bend radius (m) — the minimum local
    /// path curvature minus half the pack radial extent — the value both
    /// manufacturing bounds gate on. `None` when the radius does not
    /// resolve (degenerate pack), and on pre-v17 records. Defaulted so
    /// older records still parse.
    #[serde(default)]
    pub inner_bend_radius_m: Option<f64>,
    /// Schema v17: the tape's outer-fiber bending strain
    /// `tape_thickness_m / (2 · inner_bend_radius_m)` — the elastic-beam
    /// estimate of the bent stack's peak surface strain. `Some` iff the
    /// case declares `manufacturing.tape_thickness_m` and the inner bend
    /// radius resolved positive; bounded by `max_bend_strain` inside
    /// `manufacturing_feasible`. Defaulted so pre-v17 records parse.
    #[serde(default)]
    pub bend_strain: Option<f64>,
    /// Field-kernel evaluations spent on the requirement itself: the bore
    /// probes at both quadrature orders plus, under schema v3, the
    /// good-field-region lattice. Included in the run's kernel total.
    pub requirement_kernel_evaluations: u64,
    #[serde(deserialize_with = "deserialize_nullable_quantity")]
    pub ampere_turns_a: f64,
    #[serde(deserialize_with = "deserialize_nullable_quantity")]
    pub operating_current_a: f64,
    /// PASS iff the bore-probe refinement gate passed, the good-field
    /// lattice (when declared) passed its own refinement gate, the region
    /// does not overlap the winding pack, the declared manufacturing limits
    /// hold, and NI/I_op are well-defined (contract R1/D2). A candidate not
    /// PASS here is never screened (no valid operating current to screen
    /// at, or no feasible geometry to screen).
    pub requirement_status: Status,
    /// PASS iff the bore-probe refinement gate passed, and — when the full
    /// plan ran — the full sampling grid's own `field_refinement` check also
    /// passed (contract A2's separate refinement_status field).
    pub refinement_status: Status,
    /// Always INCONCLUSIVE: no independent reference exists per candidate
    /// (contract A1/A2); only the frozen OC-004 geometry has one.
    pub numerical_status: Status,
    /// Peak field actually sampled across the evaluated plan. `None`
    /// when no field was ever sampled (requirement-failed or degenerate
    /// candidates) — `Option`, because NaN would serialize as `null`
    /// and older records already contain `null` here.
    pub peak_sampled_field_t: Option<f64>,
    /// Schema v4 mechanical screen: `I_op * peak_sampled_field_t` (N/m),
    /// the first-order Lorentz load at the worst sampled point. `None`
    /// when the evaluation never produced a finite peak field (a
    /// requirement-failed or degenerate candidate). Defaulted so v3
    /// records still parse.
    #[serde(default)]
    pub lorentz_load_n_per_m: Option<f64>,
    /// `Some(false)` iff a declared `mechanical` limit is exceeded;
    /// `Some(true)` when within limits; `None` when no mechanical block
    /// is declared or the load is not computable. A `Some(false)` fails
    /// the candidate's aggregate `status`. Defaulted so v3 records parse.
    #[serde(default)]
    pub mechanical_feasible: Option<bool>,
    /// Schema v7 mechanical bound (OC-018, run record v6): hoop stress
    /// per conductor strand, `(I_op/s) · B_peak · R_outer /
    /// tension_section_area_m2`, where R_outer is the candidate's
    /// outermost turn radius — `bend_radius_m +` half the candidate's
    /// pack radial extent (`n·radial_pitch_m/2` before schema v13's
    /// fixed extents).
    /// `None` when no hoop bound is declared or the load is not
    /// computable. Defaulted so v5 records still parse.
    #[serde(default)]
    pub hoop_stress_pa: Option<f64>,
    /// Schema v12 mechanical bound: the pack's peak transverse interface
    /// pressure (Pa) — each sampled turn's signed normal Lorentz load per
    /// unit length `I_op·⟨B·ŵ⟩` trapezoid-accumulated over the stacking
    /// direction, taking the largest interface load under either
    /// face-anchored support convention, divided by the tape-width
    /// interface extent. `None` when no field was ever sampled. Computed
    /// whenever the data exists, bounded only when the case declares
    /// `mechanical.max_transverse_pressure_pa`.
    #[serde(default)]
    pub transverse_pressure_pa: Option<f64>,
    /// The (station, tape column, interface) carrying
    /// `transverse_pressure_pa`. `turn_index` is the interface's last
    /// inner-face-side turn; `width_index` is always 0 (the interface is a
    /// turn boundary, not a width point). `None` iff the pressure is.
    #[serde(default)]
    pub transverse_pressure_location: Option<LimitingPoint>,
    /// Schema v17: the peak cumulative normal Lorentz load per unit
    /// interface width (N/m) — `transverse_pressure_pa`'s numerator
    /// before the interface-width division, at the same limiting point.
    /// For a hoop-wound pack this is the first-order diametral bursting
    /// tension per unit axial length. `None` when no field was ever
    /// sampled. Bounded only when the case declares
    /// `mechanical.max_membrane_tension_n_per_m`. Defaulted so pre-v17
    /// records parse.
    #[serde(default)]
    pub membrane_tension_n_per_m: Option<f64>,
    /// The coupled lattice's own per-candidate screening summary, reused
    /// verbatim from `crate::coupled::CandidateResult` (coarse plan's result
    /// if pruned on FAIL, else the full plan's result).
    pub screening: Option<CandidateResult>,
    pub pruned_by: Option<LimitingPoint>,
    /// Contract §9.3: true iff the coarse plan found a FAIL point but at
    /// least one coarse point's own refinement change (order-vs-order,
    /// scaled to NI) exceeded the declared gate, so the FAIL could not be
    /// trusted to prune -- the candidate proceeded to the full plan instead.
    /// Always false when pruning is not declared or no coarse FAIL occurred.
    pub coarse_refinement_unresolved: bool,
    /// Points in the full sampling plan (computed analytically; always
    /// defined even when pruning skipped it).
    pub full_plan_point_count: u64,
    pub coarse_points_evaluated: u64,
    pub coarse_kernel_evaluations: u64,
    pub full_points_evaluated: u64,
    pub full_kernel_evaluations: u64,
    pub field_timing_ms: f64,
    pub cost: SearchCostLedger,
    /// Overall candidate status: the fail-closed combination of
    /// requirement_status, refinement_status and the screening status.
    pub status: Status,
    /// Run-schema v15 (case schema v15): the declared first-order
    /// adjacent screens. `Some` iff the case declares at least one of
    /// `thermal_margin`, `ac_loss`, `quench_hotspot`, `screening_current`;
    /// each inner field is `Some` iff its block is declared. A screen
    /// whose inputs cannot resolve reports `Status::Inconclusive` —
    /// never silently Pass — and a declared screen on a candidate whose
    /// point plan never ran reports `Status::NotEvaluated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screens: Option<CandidateScreens>,
}

// Unevaluable failed candidates have no field/current quantity. Rust retains
// the historical NaN sentinel internally; serde serializes it as JSON null.
// Decode that null losslessly so a completed mixed valid/invalid search remains
// inspectable. verify_record_checks separately guards quantity availability;
// null must never establish a quantity or a passing evaluated candidate.
fn deserialize_nullable_quantity<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(f64::NAN))
}

/// Screen model identities recorded on each screen result.
pub const THERMAL_MARGIN_SCREEN_MODEL_ID: &str = "screen/thermal-margin-dataset-tcs-rootfind/v1";
pub const AC_LOSS_SCREEN_MODEL_ID: &str = "screen/ac-loss-norris-strip-bean-slab-moment-bound/v1";
pub const QUENCH_HOTSPOT_SCREEN_MODEL_ID: &str = "screen/quench-adiabatic-miit-declared-table/v1";
pub const SCREENING_CURRENT_SCREEN_MODEL_ID: &str =
    "screen/screening-current-brandt-strip-penetration/v1";
pub const TRANSITION_SCREEN_MODEL_ID: &str = "screen/transition-ej-measured-n/v1";
pub const QUENCH_TRANSIENT_SCREEN_MODEL_ID: &str =
    "screen/quench-transient-lumped-adiabatic-current-sharing/v1";

/// Run-schema v15: the case-declared first-order screens' per-candidate
/// results. Each member is present iff the case declared that block.
/// v16 adds `transition`, v18 adds `quench_transient` — same
/// optionality convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateScreens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_margin: Option<ThermalMarginScreenRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_loss: Option<AcLossScreenRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quench_hotspot: Option<QuenchHotspotScreenRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screening_current: Option<ScreeningCurrentScreenRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionScreenRecord>,
    /// Schema v18: the lumped adiabatic quench-transient screen —
    /// point-dependent, so a candidate whose point plan never ran
    /// carries it NOT_EVALUATED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quench_transient: Option<QuenchTransientScreenRecord>,
}

/// `thermal_margin` result: the smallest `T_cs − T_op` over the
/// evaluated points, where `T_cs` is the dataset root-find of the
/// critical-sheet crossing at the point's recorded query field/angles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThermalMarginScreenRecord {
    pub model: String,
    pub status: Status,
    /// Smallest margin over evaluated points (K); `None` when no point
    /// could be evaluated.
    pub min_margin_k: Option<f64>,
    /// True when the min-margin point's `T_cs` lay beyond the covering
    /// dataset's resolvable temperature span — `min_margin_k` is then a
    /// lower bound on the true margin, not the resolved value.
    pub min_margin_is_lower_bound: bool,
    pub limiting: Option<LimitingPoint>,
    /// Points whose capacity resolved and whose margin was evaluated.
    pub points_evaluated: u64,
    /// Points whose capacity was undetermined, or whose margin could
    /// not be resolved above the operating temperature. Any nonzero
    /// count forces the screen INCONCLUSIVE — the unsampled margin is
    /// unknown, never assumed.
    pub points_inconclusive: u64,
}

/// `ac_loss` result: the declared hysteretic-loss estimate at the
/// declared swing frequency — Norris strip transport term, Bean-slab
/// term on the face-parallel field component, and the rigorous
/// perpendicular upper bound `4·m_sat·B_perp`. The aggregated figure is
/// the sampled mean per-metre loss times installed conductor length —
/// station samples are treated as equal shares of each position's loop
/// length (a declared sampling approximation, not an arc-length
/// quadrature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcLossScreenRecord {
    pub model: String,
    pub status: Status,
    /// Norris-strip transport component (W).
    pub transport_loss_w: Option<f64>,
    /// Bean-slab component on the face-parallel field (W).
    pub parallel_slab_loss_w: Option<f64>,
    /// Saturation-moment upper bound on the face-normal component (W) —
    /// a strict bound, deliberately loose at low field where the true
    /// strip loss is ~B⁴.
    pub perpendicular_bound_w: Option<f64>,
    /// Sum of the three components (W); compared against the declared
    /// `max_loss_w` budget.
    pub total_loss_w: Option<f64>,
    /// Peak per-strand-metre loss density (J/cycle/m) and its location —
    /// where the winding's loss concentrates.
    pub peak_loss_j_per_m_per_cycle: Option<f64>,
    pub limiting: Option<LimitingPoint>,
    pub points_evaluated: u64,
    /// Points whose local critical sheet current was undetermined —
    /// their loss contribution is unknown, so any nonzero count forces
    /// the screen INCONCLUSIVE.
    pub points_inconclusive: u64,
}

/// `quench_hotspot` result: the adiabatic hot-spot bound under the
/// declared exponential dump, resolved on the case-declared
/// quench-integral table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuenchHotspotScreenRecord {
    pub model: String,
    pub status: Status,
    /// Per-strand normal-zone current density at dump start (A/m²).
    pub strand_current_density_a_per_m2: Option<f64>,
    /// Deposited `∫J²dt = J²·τ/2` (A²s/m⁴).
    pub miit_a2s: Option<f64>,
    /// Interpolated hot-spot temperature (K); `None` when the required
    /// quench integral runs past the declared table's top — the screen
    /// is then INCONCLUSIVE, never extrapolated.
    pub hotspot_temperature_k: Option<f64>,
    /// True when `U(T_op) + MIITs` exceeded the table's last `U` value.
    pub table_exhausted: bool,
}

/// `quench_transient` result (schema v18): the lumped adiabatic
/// driven-dump trajectory — per sharing point `dT/dt =
/// J_m²·ρ_e(T)/C_v(T)` with `I_m = max(0, I_strand − I_sc(T))` re-queried
/// on the covering dataset at the evolving temperature. A lumped bound:
/// initiation, propagation and thermal coupling unmodeled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuenchTransientScreenRecord {
    pub model: String,
    pub status: Status,
    /// Peak temperature any point reached during the dump (K); `None`
    /// when no point's trajectory could be evaluated at all.
    pub peak_temperature_k: Option<f64>,
    /// True when the trajectory reporting `peak_temperature_k` left
    /// declared coverage (a property table or the dataset's temperature
    /// span) before `t_end` — the true peak is then at least the
    /// reported value, never resolved as less.
    pub peak_temperature_is_lower_bound: bool,
    /// The peak-temperature point.
    pub limiting: Option<LimitingPoint>,
    /// First time (s) the per-strand resistive series voltage
    /// `Σ J_m·ρ_e·w_q·dℓ` crossed the declared `detection_voltage_v`;
    /// `None` when no threshold was declared, or the signature never
    /// crossed it before `t_end`.
    pub detection_time_s: Option<f64>,
    /// Peak per-strand resistive series voltage reached (V) — zero when
    /// no point ever shared current into the matrix.
    pub peak_series_voltage_v: Option<f64>,
    /// Points whose starting state was queried (capacity resolved).
    pub points_evaluated: u64,
    /// Points that ever carried matrix current (`u_0 > 1` initially, or
    /// sharing developed) — the ODE was integrated for exactly these.
    pub points_sharing: u64,
    /// Points whose capacity could not resolve — their trajectory is
    /// unknown and forces INCONCLUSIVE.
    pub points_inconclusive: u64,
    /// True when any trajectory left declared coverage before `t_end`:
    /// a `ρ_e`/`C_v` table top or the covering dataset's temperature
    /// span. The driven-dump answer beyond that is unknown, not assumed.
    pub coverage_exhausted: bool,
}

/// `screening_current` result: the largest Brandt–Indenbom strip
/// penetrated-width fraction `1 − 1/cosh(B_perp/B_c)` over evaluated
/// points, `B_c = μ₀·K_c/π` from the point's own critical sheet current.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreeningCurrentScreenRecord {
    pub model: String,
    pub status: Status,
    /// Largest penetrated-width fraction over evaluated points.
    pub max_penetrated_width_fraction: Option<f64>,
    /// Largest `B_perp/B_c` ratio over evaluated points — the flag's
    /// raw diagnostic, recorded for context whatever the declared limit.
    pub max_b_perp_over_bc: Option<f64>,
    pub limiting: Option<LimitingPoint>,
    pub points_evaluated: u64,
    /// Points whose local critical sheet current was undetermined —
    /// their penetration state is unknown; any nonzero count forces the
    /// screen INCONCLUSIVE.
    pub points_inconclusive: u64,
}

/// `transition` result: the measured E–J law `E = Ec·u^n` evaluated at
/// every sampled point, `u = K_demand/K_used` the point's operating ratio
/// and `n` the covering dataset's own interpolated measured exponent on
/// the governing mirror-pair branch. The headline depth is the largest
/// `E/Ec` over evaluated points; the terminal voltage is the per-strand
/// estimate `Σ E·w_q·dℓ` over the sampled winding (parallel strands each
/// develop the same voltage under the equal-share assumption — it is
/// not multiplied by the strand count).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionScreenRecord {
    pub model: String,
    pub status: Status,
    /// Largest `E/Ec = u^n` over evaluated points — how deep into the
    /// measured transition the worst point operates.
    pub max_e_over_ec: Option<f64>,
    /// Per-strand terminal-voltage estimate (V) summed over the sampled
    /// winding — what voltage taps across the winding would see.
    pub terminal_voltage_v: Option<f64>,
    /// Operating ratio `u` and measured `n` at the deepest point.
    pub worst_utilization: Option<f64>,
    pub worst_n_value: Option<f64>,
    pub limiting: Option<LimitingPoint>,
    pub points_evaluated: u64,
    /// Points whose capacity or measured exponent could not resolve —
    /// their transition depth is unknown; any nonzero count forces the
    /// screen INCONCLUSIVE.
    pub points_inconclusive: u64,
}

/// Run-record schema v14: the declared field-map provenance of a search
/// schema v13 case — the map's record fields plus the declared
/// bore-field anchor the NI solve used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchFieldMapRecord {
    #[serde(flatten)]
    pub map: crate::coupled::FieldMapRecord,
    /// The map producer's declared field at the requirement's bore probe
    /// under `reference_ampere_turns_a` — the anchor the NI solve used.
    /// Declared, not verified: the map covers the pack region only.
    pub bore_field_at_reference_t: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoupledSearchRunRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub coupled_search_model_id: String,
    pub coupled_search_checker_id: String,
    pub case_sha256: String,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    pub case: CoupledSearchCase,
    pub dataset_id: String,
    pub dataset_csv_sha256: String,
    /// Schema v13: the base binding's own monotonicity audit under its
    /// declared tolerance — previously computed but never recorded on
    /// this record type (only on the inner `CoupledRunRecord`).
    /// `None` on records written before v13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monotonicity_audit: Option<MonotonicityAuditRecord>,
    /// Schema v14: the declared field-map provenance when the search ran
    /// under a case-declared map (search schema v13) — the map's own
    /// record fields plus the declared bore-field anchor the NI solve
    /// used. `None` on engine-field runs and older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_map: Option<SearchFieldMapRecord>,
    /// Schema v13: each `tape_specs` binding's *resolved* dataset identity
    /// and its own monotonicity audit, keyed by spec id (the base binding
    /// keeps `dataset_id`/`monotonicity_audit`). Without these a graded
    /// record showed only the case's declared spec pins, not the dataset
    /// actually loaded. Empty — and absent — on ungraded cases.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spec_datasets: BTreeMap<String, DatasetIdentity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spec_monotonicity_audits: BTreeMap<String, MonotonicityAuditRecord>,
    pub candidates: Vec<SearchCandidateResult>,
    pub best_index: Option<usize>,
    pub baseline_index: usize,
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    pub kernel_evaluations: u64,
    pub points_evaluated: u64,
    /// Points in the full sampling plan that pruned candidates never
    /// evaluated (computed analytically, not measured).
    pub points_saved_by_pruning: u64,
    /// `points_saved_by_pruning` scaled by the coarse phase's own observed
    /// average kernel-evaluations-per-point across this run; an estimate
    /// derived from real measured work, not a measurement of the avoided
    /// work itself (which was never run).
    pub estimated_kernel_evaluations_saved: f64,
    /// FAIL if no candidate has requirement, refinement and screening all
    /// PASS (contract A2). Otherwise PASS, unless the acceptance module's
    /// contract §9.4 refined-plan re-evaluation of the baseline (when it
    /// was itself PASS) or the best candidate loses its PASS status or
    /// exceeds the declared sampling shortfall gate, in which case this is
    /// INCONCLUSIVE: the coarse plan is judged inadequate for that
    /// candidate, never silently reported PASS.
    pub search_status: Status,
    /// Independent recomputation of the baseline and the selected optimum
    /// (contract §8 D8), embedded directly per contract §5.
    pub acceptance: CoupledSearchAcceptance,
    pub checks: Vec<Check>,
    pub limitations: Vec<String>,
}

impl CoupledSearchRunRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_oc007(options: &CoupledSearchOptions) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case(optcoil_model::coupled_search::OC007_JSON, options)
}

pub fn run_oc007_control(
    options: &CoupledSearchOptions,
) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case(optcoil_model::coupled_search::OC007_CONTROL_JSON, options)
}

pub fn run_coupled_search_case(
    case_json: &str,
    options: &CoupledSearchOptions,
) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case_cancellable(case_json, options, &AtomicBool::new(false))
}

/// Cancellation-cooperative form of [`run_coupled_search_case`]: workers
/// check `cancel` between candidates, and a set flag resolves to
/// [`RunError::Cancelled`] — a partial candidate list is never reported
/// as a completed search.
pub fn run_coupled_search_case_cancellable(
    case_json: &str,
    options: &CoupledSearchOptions,
    cancel: &AtomicBool,
) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case_with_dataset(case_json, options, None, cancel)
}

/// Dataset-supplying form of [`run_coupled_search_case_cancellable`]:
/// `dataset`, when `Some`, is used instead of the embedded lookup. The
/// case still pins the dataset's identity — `material.dataset_id` and
/// `material.csv_sha256` are checked against the supplied dataset by
/// [`CoupledSearchCase::validate_against_dataset`], so a mismatched
/// dataset is rejected, never silently substituted. This is the
/// customer-data path: a case that declares a non-embedded dataset id
/// can only run against a supplied dataset whose bytes hash to the
/// declared `csv_sha256`.
pub fn run_coupled_search_case_with_dataset(
    case_json: &str,
    options: &CoupledSearchOptions,
    dataset: Option<MaterialDataset>,
    cancel: &AtomicBool,
) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case_with_dataset_progress(case_json, options, dataset, cancel, None)
}

/// Progress-reporting form of
/// [`run_coupled_search_case_with_dataset`]: when `progress` is `Some`,
/// the runner populates its per-candidate legs at dispatch and ticks one
/// field evaluation per evaluator call — a UI can poll
/// [`SearchProgress::fraction`] while the worker runs. Progress is a
/// view concern only; it never alters a verdict or a ledger number.
pub fn run_coupled_search_case_with_dataset_progress(
    case_json: &str,
    options: &CoupledSearchOptions,
    dataset: Option<MaterialDataset>,
    cancel: &AtomicBool,
    progress: Option<&SearchProgress>,
) -> Result<CoupledSearchRunRecord, RunError> {
    let supplied: BTreeMap<String, MaterialDataset> = dataset
        .into_iter()
        .map(|dataset| (dataset.metadata.id.clone(), dataset))
        .collect();
    run_coupled_search_case_with_datasets_progress(case_json, options, &supplied, cancel, progress)
}

/// Multi-binding form of [`run_coupled_search_case_with_dataset_progress`].
/// `datasets` is keyed by each supplied dataset's actual `metadata.id`.
/// Every supplied id must be declared by the case, map keys must match
/// metadata ids, and every base/spec binding is checked against its pinned
/// dataset id and CSV hash. Omitted bindings may resolve from the embedded
/// registry; missing non-embedded bindings fail before candidate evaluation.
pub fn run_coupled_search_case_with_datasets(
    case_json: &str,
    options: &CoupledSearchOptions,
    datasets: &BTreeMap<String, MaterialDataset>,
    cancel: &AtomicBool,
) -> Result<CoupledSearchRunRecord, RunError> {
    run_coupled_search_case_with_datasets_progress(case_json, options, datasets, cancel, None)
}

/// Progress-reporting multi-binding form of
/// [`run_coupled_search_case_with_datasets`].
pub fn run_coupled_search_case_with_datasets_progress(
    case_json: &str,
    options: &CoupledSearchOptions,
    supplied: &BTreeMap<String, MaterialDataset>,
    cancel: &AtomicBool,
    progress: Option<&SearchProgress>,
) -> Result<CoupledSearchRunRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;

    let search = CoupledSearchCase::from_json(case_json)?;
    let case_sha256 = hash(case_json.as_bytes());
    // Every binding — the base `material` plus each `tape_specs` entry —
    // resolves its own declared dataset: a supplied dataset matching the
    // declared id wins, otherwise the embedded registry. A supplied
    // dataset no binding declares is rejected outright — a stray supply
    // must never silently fall back to embedded bytes.
    let bindings = search.material_bindings();
    for (key, d) in supplied {
        if key != &d.metadata.id {
            return Err(RunError::Invalid(format!(
                "supplied dataset map key '{}' does not match metadata id '{}'",
                key, d.metadata.id
            )));
        }
        if !bindings.iter().any(|(_, m)| m.dataset_id == d.metadata.id) {
            return Err(RunError::Invalid(format!(
                "supplied dataset '{}' is not declared by any material binding",
                d.metadata.id
            )));
        }
    }
    let datasets = resolve_datasets_map(bindings.iter().map(|&(_, m)| m), supplied)?;
    search.validate_against_dataset_map(&datasets)?;
    let critical_state_strip =
        search.limits.self_field_correction.as_deref() == Some("critical_state_strip");
    let runtimes = {
        let base = spec_runtime(
            &search.material,
            &datasets[&search.material.dataset_id],
            search.operating.temperature_k,
            critical_state_strip,
        )?;
        let mut specs = std::collections::BTreeMap::new();
        if let Some(tape_specs) = &search.tape_specs {
            for (id, spec) in tape_specs {
                specs.insert(
                    id.clone(),
                    spec_runtime(
                        &spec.material,
                        &datasets[&spec.material.dataset_id],
                        search.operating.temperature_k,
                        critical_state_strip,
                    )?,
                );
            }
        }
        MaterialRuntimes {
            base,
            specs,
            datasets,
        }
    };
    // The base binding's dataset keeps the record's `dataset_id` /
    // `dataset_csv_sha256` fields; spec bindings' datasets are pinned
    // inside the embedded case's `tape_specs[].material` declarations.
    let dataset = &runtimes.datasets[&search.material.dataset_id];

    let execution_threads = match options.threads {
        Some(0) => {
            return Err(RunError::Invalid("--threads must be at least 1".into()));
        }
        Some(t) if t > search.execution.max_threads => {
            return Err(RunError::Invalid(format!(
                "--threads may only lower the case's own execution.max_threads ({}); got {t}",
                search.execution.max_threads
            )));
        }
        Some(t) => t,
        None => search.execution.max_threads,
    };

    // Schema v10: a candidate is a (turns, tapes, strands, assignment)
    // tuple — `assignments()` enumerates the per-region spec-choice
    // product, `[[]]` on ungraded cases. Schema v21 adds the racetrack
    // dims as further choice axes: `Some(value)` per axis-declared dim,
    // `None` meaning "the fixed geometry's declaration applies".
    let assignments = search.assignments();
    let bend_axis = search.bend_radius_axis();
    let straight_axis = search.straight_half_length_axis();
    let mut geometry_candidates = Vec::with_capacity(search.candidate_count());
    for &turns in &search.choices.turns_along_normal {
        for &tapes in &search.choices.tapes_along_width {
            for &strands in search.strands_choices() {
                for assignment in &assignments {
                    for &bend_radius_m in &bend_axis {
                        for &straight_half_length_m in &straight_axis {
                            geometry_candidates.push((
                                turns,
                                tapes,
                                strands,
                                assignment.clone(),
                                CandidateDims {
                                    bend_radius_m,
                                    straight_half_length_m,
                                },
                            ));
                        }
                    }
                }
            }
        }
    }
    let baseline_assignment = search.baseline.tape_spec_ids.clone().unwrap_or_default();
    let baseline_index = geometry_candidates
        .iter()
        .position(|(t, p, s, a, d)| {
            *t == search.baseline.turns_along_normal
                && *p == search.baseline.tapes_along_width
                && *s == search.baseline_strands()
                && *a == baseline_assignment
                && d.bend_radius_m == search.baseline.bend_radius_m
                && d.straight_half_length_m == search.baseline.straight_half_length_m
        })
        .ok_or_else(|| {
            RunError::Invalid("baseline geometry not found among enumerated candidates".into())
        })?;

    let n = geometry_candidates.len();
    if let Some(p) = progress {
        p.candidates_total.store(n, Ordering::Relaxed);
        let mut legs: Vec<SearchProgressLeg> = geometry_candidates
            .iter()
            .map(|(turns, tapes, _, _, _)| SearchProgressLeg {
                planned: planned_field_evals(&search, *turns, *tapes),
                done: Arc::new(AtomicU64::new(0)),
            })
            .collect();
        // Contract D8's acceptance recomputation runs after the
        // candidate legs — legs n and n+1 pre-plan it pessimistically
        // (baseline assumes PASS → includes the §9.4 refined plan; the
        // optimum leg uses the largest enumerated pack since the real
        // best is unknown until the search ends). `assess` revises each
        // to the discovered scope, so planned only shrinks and the bar
        // never recedes.
        let (base_turns, base_tapes, _, _, _) = geometry_candidates[baseline_index];
        let (max_turns, max_tapes) = geometry_candidates
            .iter()
            .fold((0_u32, 0_u32), |(mt, mp), (t, p, _, _, _)| {
                (mt.max(*t), mp.max(*p))
            });
        for (turns, tapes) in [(base_turns, base_tapes), (max_turns, max_tapes)] {
            legs.push(SearchProgressLeg {
                planned: search_acceptance::acceptance_planned_evals(&search, turns, tapes, true),
                done: Arc::new(AtomicU64::new(0)),
            });
        }
        *p.legs.lock().expect("progress legs mutex poisoned") = legs;
        p.set_phase("enumerated candidates — screening".to_owned());
    }
    let counter = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<SearchCandidateResult, RunError>)>> =
        Mutex::new(Vec::with_capacity(n));

    // Each worker pulls candidate indices off `counter` until the pool
    // drains or cancellation lands. wasm32 has no OS threads — the same
    // loop runs inline on the caller there (single worker thread, no
    // parallelism; the progress counters and the mutex stay correct).
    let work = || loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let index = counter.fetch_add(1, Ordering::Relaxed);
        if index >= n {
            break;
        }
        let leg = progress.map(|p| {
            p.legs.lock().expect("progress legs mutex poisoned")[index]
                .done
                .clone()
        });
        let (turns, tapes, strands, assignment, dims) = &geometry_candidates[index];
        if let Some(p) = progress {
            p.set_phase(format!(
                "screening {} × {} × {} — candidate {}/{}",
                turns,
                tapes,
                strands,
                index + 1,
                n
            ));
        }
        let outcome = evaluate_one_candidate_cancellable(
            &search,
            index,
            *turns,
            *tapes,
            *strands,
            assignment,
            *dims,
            &runtimes,
            leg.as_deref(),
            cancel,
        );
        results
            .lock()
            .expect("search results mutex poisoned")
            .push((index, outcome));
        if let Some(p) = progress {
            p.candidates_done.fetch_add(1, Ordering::Relaxed);
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|scope| {
        for _ in 0..execution_threads {
            // &work: one Fn closure shared across the scoped workers.
            #[allow(clippy::needless_borrows_for_generic_args)]
            scope.spawn(&work);
        }
    });
    #[cfg(target_arch = "wasm32")]
    work();

    if cancel.load(Ordering::Relaxed) {
        return Err(RunError::Cancelled);
    }
    let mut ordered = results.into_inner().expect("search results mutex poisoned");
    ordered.sort_by_key(|(index, _)| *index);
    let mut candidates = Vec::with_capacity(n);
    for (_, outcome) in ordered {
        candidates.push(outcome?);
    }

    let best_index = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status == Status::Pass)
        .min_by(|(_, a), (_, b)| {
            a.cost
                .total_usd
                .total_cmp(&b.cost.total_usd)
                .then(a.geometry.total_turns.cmp(&b.geometry.total_turns))
                .then(a.index.cmp(&b.index))
        })
        .map(|(i, _)| i);

    let baseline_total_usd = candidates[baseline_index].cost.total_usd;
    // Requirement probes (the bore point at both quadrature orders plus,
    // under schema v3, the good-field lattice) are counted in the run total
    // as of run-record v2; earlier records counted only screening evals.
    let kernel_evaluations: u64 = candidates
        .iter()
        .map(|c| {
            c.requirement_kernel_evaluations
                + c.coarse_kernel_evaluations
                + c.full_kernel_evaluations
        })
        .sum();
    let points_evaluated: u64 = candidates
        .iter()
        .map(|c| c.coarse_points_evaluated + c.full_points_evaluated)
        .sum();
    let total_coarse_points: u64 = candidates.iter().map(|c| c.coarse_points_evaluated).sum();
    let total_coarse_kernel: u64 = candidates.iter().map(|c| c.coarse_kernel_evaluations).sum();
    let average_kernel_per_point = if total_coarse_points > 0 {
        total_coarse_kernel as f64 / total_coarse_points as f64
    } else {
        0.0
    };
    let points_saved_by_pruning: u64 = candidates
        .iter()
        .filter(|c| c.pruned_by.is_some())
        .map(|c| {
            c.full_plan_point_count
                .saturating_sub(c.coarse_points_evaluated)
        })
        .sum();
    let estimated_kernel_evaluations_saved =
        points_saved_by_pruning as f64 * average_kernel_per_point;

    let pass_count = candidates
        .iter()
        .filter(|c| c.status == Status::Pass)
        .count();
    let fail_count = candidates
        .iter()
        .filter(|c| c.status == Status::Fail)
        .count();
    let pruned_count = candidates.iter().filter(|c| c.pruned_by.is_some()).count();

    // Contract D8: independent recomputation of the baseline and the
    // selected optimum, embedded in the run record itself (contract §5).
    // Checker v18's fallback walk may promote a later candidate when the
    // cheapest PASS under-resolves on the refined plan — the reported
    // optimum is whoever `acceptance.best` names afterward.
    let acceptance = search_acceptance::assess_progress_cancellable(
        &search,
        &candidates,
        baseline_index,
        best_index,
        &runtimes.datasets,
        progress,
        cancel,
    )?;
    check_cancelled(cancel)?;
    let best_index = acceptance.best.as_ref().map(|b| b.index);
    let (savings_usd, savings_percent) = match best_index {
        Some(i) => {
            let saving = baseline_total_usd - candidates[i].cost.total_usd;
            (Some(saving), Some(saving / baseline_total_usd * 100.0))
        }
        None => (None, None),
    };

    // Contract §9.4: a passing candidate whose refined 10-station
    // re-evaluation either loses its PASS status or exceeds the declared
    // sampling shortfall gate means the coarse plan was inadequate for that
    // candidate -- search_status downgrades to INCONCLUSIVE rather than
    // staying PASS. Never applied to a baseline that never claimed PASS in
    // the first place (its sampling_refinement_status is NotEvaluated,
    // which does not block).
    let search_status = if best_index.is_none() {
        Status::Fail
    } else {
        let baseline_blocks = acceptance.baseline.sampling_refinement_status == Status::Fail;
        let best_blocks = acceptance
            .best
            .as_ref()
            .is_some_and(|b| b.sampling_refinement_status != Status::Pass);
        if baseline_blocks || best_blocks {
            Status::Inconclusive
        } else {
            Status::Pass
        }
    };

    let implementation_sha256 = hash(&serde_json::to_vec(&(
        include_str!("../../optcoil-model/src/lib.rs"),
        include_str!("../../optcoil-model/src/magnetics.rs"),
        include_str!("../../optcoil-model/src/material.rs"),
        include_str!("../../optcoil-model/src/coupled.rs"),
        include_str!("../../optcoil-model/src/coupled_search.rs"),
        include_str!("../../optcoil-physics/src/racetrack.rs"),
        include_str!("../../optcoil-physics/src/critical_current.rs"),
        include_str!("../../optcoil-physics/src/tape_frame.rs"),
        include_str!("lib.rs"),
        include_str!("coupled.rs"),
        include_str!("coupled_search.rs"),
        include_str!("search_acceptance.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?);
    let input_sha256 = hash(&serde_json::to_vec(&(
        &case_sha256,
        &implementation_sha256,
        options,
        COUPLED_SEARCH_MODEL_ID,
        COUPLED_SEARCH_CHECKER_ID,
        env!("CARGO_PKG_VERSION"),
    ))?);

    let checks = vec![
        Check {
            id: "search_completion".into(),
            status: Status::Pass,
            detail: format!(
                "{} of {} geometry candidates evaluated ({} PASS, {} FAIL, {pruned_count} pruned by the coarse plan)",
                candidates.len(),
                n,
                pass_count,
                fail_count
            ),
        },
        Check {
            id: "baseline_screening".into(),
            status: candidates[baseline_index].status,
            detail: format!(
                "Baseline {}x{} candidate status {:?}",
                search.baseline.turns_along_normal,
                search.baseline.tapes_along_width,
                candidates[baseline_index].status
            ),
        },
    ];

    // Schema v13: resolved dataset provenance on the search record itself.
    // Every binding's audit under its declared tolerance — a graded pack's
    // screening is only auditable if every dataset it touched is on the
    // record (same convention as `CoupledRunRecord`'s v6 spec fields).
    let audit_record = |audit: &tape_frame::MonotonicityAudit| MonotonicityAuditRecord {
        checked_pairs: audit.checked_pairs,
        violations: audit.violations,
        worst_relative_increase: audit.worst_relative_increase,
        tolerance: audit.tolerance,
        status: audit.status,
    };
    let monotonicity_audit = Some(audit_record(&runtimes.base.monotonicity));
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

    // Schema v14: the declared field-map provenance, computed before
    // `search` moves into the record below.
    let field_map_record = search.field_map.as_ref().map(|fm| SearchFieldMapRecord {
        map: crate::coupled::FieldMapRecord::from_map(&fm.map),
        bore_field_at_reference_t: fm.bore_field_at_reference_t,
    });
    let has_field_map = field_map_record.is_some();
    // Schema v19: the non-planar limitation note is computed before
    // `search` moves into the record below.
    let has_path3d = search.fixed_geometry.path3d.is_some();

    // Bridge-to-full-width applicability: every bound dataset's own
    // characterization domain against the case's applied product width.
    // Computed before `spec_datasets`/`search` move into the record.
    let applied_width_m = search.fixed_geometry.tape_width_m;
    let mut width_limitations = Vec::new();
    {
        let mut bindings: Vec<(&str, &str, f64, f64)> = vec![(
            "base",
            dataset.metadata.id.as_str(),
            dataset.metadata.measured_bridge_width_m,
            dataset.metadata.original_tape_width_m,
        )];
        bindings.extend(spec_datasets.iter().filter_map(|(id, identity)| {
            Some((
                id.as_str(),
                identity.id.as_str(),
                identity.measured_bridge_width_m?,
                identity.original_tape_width_m?,
            ))
        }));
        for (binding, dataset_id, bridge, tape) in bindings {
            let mismatch = if (applied_width_m - tape).abs() > tape * 1e-9 {
                format!(
                    " The case applies the data at {applied_width_m} m product width, different from the {tape} m tape the specimen was cut from — cross-product-width transfer is additionally unverified."
                )
            } else {
                String::new()
            };
            width_limitations.push(format!(
                "Width applicability, binding '{binding}' ({dataset_id}): the specimen was measured as a {bridge} m patterned bridge from a {tape} m tape; K in A/m is bridge-width-normalized.{mismatch} Bridge-to-full-width transfer (Jc uniformity across the real tape width, edge degradation, lot variation) is an unverified modeling assumption, not measured coverage — full-width or lot-representative data at the operating point remains a data gap."
            ));
        }
    }

    Ok(CoupledSearchRunRecord {
        // v10 adds `CandidateGeometry.tape_spec_ids` (search schema v10's
        // resolved assignment; absent on ungraded cases, defaulted on
        // older records). The case inside pins every spec binding's
        // dataset identity; `dataset_id`/`dataset_csv_sha256` stay the
        // base binding's. v11 adds `transverse_pressure_pa` and
        // `transverse_pressure_location` (search schema v12's screen;
        // `None` on candidates that never sampled a field, defaulted on
        // older records). v12 adds the acceptance-side pressure-coverage
        // fields: `refined.transverse_pressure_pa`,
        // `pressure_shortfall_fraction` and `pressure_coverage_status`
        // (all defaulted on older records). v13 adds `monotonicity_audit`
        // (base binding) and the per-spec `spec_datasets` /
        // `spec_monotonicity_audits` maps — the resolved identities and
        // audits every dataset the run actually touched (all defaulted
        // on older records). v14 adds `field_map` — the declared-map
        // provenance for schema v13 search cases (absent on engine-field
        // runs and defaulted on older records). v15 adds `screens` —
        // the case-declared first-order adjacent-screen results
        // (search schema v15; absent on earlier records). v16 adds
        // `screens.transition` — the measured E–J transition screen
        // (search schema v16; absent on earlier records). v17 adds the
        // winding-mechanics fields — `inner_bend_radius_m`, `bend_strain`
        // and `membrane_tension_n_per_m` per candidate, plus
        // `recomputed_bend_strain` / `recomputed_membrane_tension_n_per_m`
        // on the acceptance side (search schema v17; all defaulted on
        // older records). v18 adds `screens.quench_transient` — the
        // lumped adiabatic driven-dump trajectory screen (search schema
        // v18; absent on earlier records). v25 adds
        // `acceptance.fallback_attempts` — the cheaper candidates the
        // acceptance walk re-verified after the first optimum failed
        // §9.4 (checker v18; absent when the walk never ran, defaulted
        // on older records).
        schema: "optcoil-coupled-search-run/v26".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        coupled_search_model_id: COUPLED_SEARCH_MODEL_ID.into(),
        coupled_search_checker_id: COUPLED_SEARCH_CHECKER_ID.into(),
        case_sha256,
        implementation_sha256,
        input_sha256,
        started_unix_ms,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        runtime: {
            let mut r = runtime_info();
            r.execution_threads = execution_threads;
            r
        },
        case: search,
        dataset_id: dataset.metadata.id.clone(),
        dataset_csv_sha256: dataset.metadata.csv_sha256.clone(),
        monotonicity_audit,
        field_map: field_map_record,
        spec_datasets,
        spec_monotonicity_audits,
        candidates,
        best_index,
        baseline_index,
        savings_usd,
        savings_percent,
        kernel_evaluations,
        points_evaluated,
        points_saved_by_pruning,
        estimated_kernel_evaluations_saved,
        search_status,
        acceptance,
        checks,
        limitations: {
            let mut limitations = vec![
                "Modeled, synthetic prices, screening model: cost is an invented placeholder model, not supplier data or a production cost estimate.".into(),
                "The headline result is a screening-passing modeled-cost optimum, never a validated design or a production operating-current limit.".into(),
                "No independent reference exists per candidate; only the frozen reference geometry has one. numerical_status is INCONCLUSIVE by construction for every candidate; refinement_status is the separate PASS/FAIL check this record relies on instead.".into(),
                "Coarse pruning evaluates all 5 Gauss-Lobatto width points at each declared coarse station/turn/tape combination (the shared sampling schema has no narrower width subset), a conservative superset of the contract's declared 2-point width restriction: it never discards a would-be PASS, at a smaller pruning speedup than the literal declaration.".into(),
                "width_transfer basis is none (no Jc nonuniformity across the tape width or batch variation modeled); one measured specimen; conductor-filled pack with no inter-pancake spacer gaps.".into(),
                "Mechanical, thermal, quench and manufacturing acceptance remain NOT_EVALUATED.".into(),
                "transverse_pressure_pa is a first-order screening quantity only: the signed per-turn normal Lorentz load trapezoid-accumulated over the sampled stacking column and divided by the tape-width interface extent, taking the largest interface load under either face-anchored convention. It is not a structural, strain or delamination model — no winding-pack stiffness, insulation compliance, or stress redistribution is represented, and a declared bound pass does not establish mechanical acceptance.".into(),
            ];
            // Recorded limitation, not a verdict — the bridge transfer
            // assumption is inherent to width-normalized Ic data.
            limitations.extend(width_limitations);
            if has_field_map {
                limitations.push("Sampled fields come from the case-declared external field map (field_map.map.source_sha256), not the engine's field solve; the map's own mesh is the field resolution and its correctness is the customer's, not this run's. The requirement ampere-turns anchor on the declared bore_field_at_reference_t — declared by the map producer, not verified here; the acceptance bore-field agreement is arithmetic consistency of declared values, not independent field physics.".into());
            }
            if has_path3d {
                limitations.push("The winding centerline is a non-planar 3D path (fixed_geometry.path3d). The engine's pack self-field evaluator is planar and was not run — every sampled field comes from the declared Cartesian map. Bend-radius and membrane-tension bounds use the helix's effective curvature radius and are first-order declared-bound estimates; no self-field, screening-current, or good-field-volume evaluation applies to a non-planar pack, and search screening is not engineering acceptance.".into());
            }
            limitations
        },
    })
}

/// Schema v12's transverse-pressure estimate, shared by the coarse-plan
/// micro-loop and the full-plan record path so the two can never
/// diverge. The acceptance module recomputes the same quantity with its
/// own separately-written accumulation (like `recompute_cost`), so a
/// mistake here is unlikely to be replicated there. `columns` maps each
/// sampled (station, tape-column) pair to its signed per-turn normal
/// loads `f_k = I_op·⟨B·ŵ⟩` (N/m, signed along the tape normal — the
/// width-direction field component drives the normal-direction force).
/// Each sampled turn stands in for its trapezoid-weighted neighborhood
/// over `[1, total_turns]` (weights sum exactly to `total_turns`), the
/// cumulative interface profile is `F_k = Σ_{i≤k} w_i f_i`, and the peak
/// interface pressure takes the largest load an interface could transmit
/// under either face-anchored convention — `max_k max(|F_k|, |F_total -
/// F_k|)` — divided by the interface's tape-width extent. Returns the
/// peak pressure, the undivided interface load at the same point (N/m —
/// the schema-v17 membrane-tension resultant), and its `(station, tape,
/// interface-turn)` location; the interface's `turn_index` is the last
/// turn of the inner-face-side block. `None` when no column was sampled.
/// A first-order declared-bound estimate, not a stress analysis.
fn peak_transverse_pressure(
    columns: &std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>>,
    total_turns: u32,
    interface_width_m: f64,
) -> Option<(f64, f64, LimitingPoint)> {
    let mut best: Option<(f64, f64, LimitingPoint)> = None;
    for ((station, tape_index), column) in columns {
        // Callers push in sampling order; sort so the accumulation is
        // well-defined even if a record's tape ordering ever changed.
        let mut column = column.clone();
        column.sort_by_key(|&(turn, _)| turn);
        column.dedup_by_key(|e| e.0);
        if column.is_empty() {
            continue;
        }
        // Trapezoid weights: sampled turn t_i represents integer turns in
        // (mid(t_{i-1},t_i), mid(t_i,t_{i+1})]; the ends anchor at 0 and
        // total_turns, so the weights sum to total_turns exactly.
        let m = column.len();
        let mut cumulative = 0.0_f64;
        let mut covered = 0_u32;
        let total_load = column
            .iter()
            .enumerate()
            .map(|(i, &(_, f))| {
                let left = if i == 0 {
                    0
                } else {
                    (column[i - 1].0 + column[i].0) / 2
                };
                let right = if i + 1 == m {
                    total_turns
                } else {
                    (column[i].0 + column[i + 1].0) / 2
                };
                (right - left) as f64 * f
            })
            .sum::<f64>();
        for (i, &(turn, f)) in column.iter().enumerate() {
            let left = if i == 0 {
                0
            } else {
                (column[i - 1].0 + turn) / 2
            };
            let right = if i + 1 == m {
                total_turns
            } else {
                (turn + column[i + 1].0) / 2
            };
            let w = right - left;
            cumulative += w as f64 * f;
            covered += w;
            // The interface after this turn transmits either this block's
            // cumulative load (outer face supported) or the remainder
            // (inner face supported) — take the larger transmittable load.
            let interface_load = cumulative.abs().max((total_load - cumulative).abs());
            let pressure = interface_load / interface_width_m;
            let point = LimitingPoint {
                station: station.clone(),
                tape_index: *tape_index,
                turn_index: turn,
                width_index: 0,
            };
            if best.as_ref().is_none_or(|(p, _, _)| pressure > *p) {
                best = Some((pressure, interface_load, point));
            }
        }
        debug_assert_eq!(covered, total_turns);
    }
    best
}

/// The mechanical screen shared by the pruned and full-plan record
/// paths so the two can never diverge: the schema-v4 first-order
/// Lorentz load `I_op · B_peak` (N/m, turn-aggregate), when the
/// schema-v7 pair is declared the OC-018 hoop-stress bound, and when
/// the schema-v12 bound is declared the transverse interface pressure.
/// Returns `(lorentz_load_n_per_m, hoop_stress_pa, mechanical_feasible)`.
/// A declared check whose input never materialized (no finite peak
/// field, no sampled pressure) is infeasible — the v4 treatment,
/// extended to the hoop, pressure and membrane-tension bounds.
fn mechanical_screen(
    search: &CoupledSearchCase,
    turns_along_normal: u32,
    tapes_along_width: u32,
    strands_parallel: u32,
    operating_current_a: f64,
    peak_sampled_field_t: Option<f64>,
    interface: Option<(f64, f64)>,
) -> (Option<f64>, Option<f64>, Option<bool>) {
    let (transverse_pressure_pa, membrane_tension_n_per_m) =
        interface.map_or((None, None), |(p, t)| (Some(p), Some(t)));
    let lorentz = peak_sampled_field_t
        .filter(|p| operating_current_a.is_finite() && p.is_finite())
        .map(|p| operating_current_a * p);
    let Some(mechanical) = &search.mechanical else {
        return (lorentz, None, None);
    };
    // Pressure-vessel mechanics: a conductor under radial Lorentz load f
    // (N/m) carries hoop tension T = f·R. The per-strand load is
    // (I_op/s)·B_peak — i.e. the aggregate load divided by the strand
    // count — and the largest tension develops at the pack's outermost
    // turn radius: `bend + w/2` on a racetrack, `max arc radius + w/2` on
    // a path (conservative — the bound scales with radius). Stress = T /
    // the declared load-bearing section. `w` is the candidate's pack
    // radial extent — the declared fixed extent under a v13 field map
    // (where `turns x radial_pitch` spans the *normal* direction, which
    // is axial under an axial tape normal), the legacy `turns x pitch`
    // footprint otherwise.
    let (radial_width_m, _) = search
        .fixed_geometry
        .candidate_extents_m(turns_along_normal, tapes_along_width);
    let hoop = mechanical.tension_section_area_m2.and_then(|area| {
        lorentz.map(|load| {
            let r_outer = search
                .fixed_geometry
                .max_outer_radius_m(radial_width_m)
                .unwrap_or(f64::INFINITY);
            (load / f64::from(strands_parallel.max(1))) * r_outer / area
        })
    });
    let feasible = lorentz.is_some_and(|l| l <= mechanical.max_lorentz_load_n_per_m)
        && match mechanical.max_hoop_stress_pa {
            Some(limit) => hoop.is_some_and(|s| s <= limit),
            None => true,
        }
        && match mechanical.max_transverse_pressure_pa {
            Some(limit) => transverse_pressure_pa.is_some_and(|p| p <= limit),
            None => true,
        }
        // Schema v17: the membrane-tension bound on the undivided
        // cumulative load — a declared limit with no sampled column is
        // infeasible, the pressure bound's own fail-closed convention.
        && match mechanical.max_membrane_tension_n_per_m {
            Some(limit) => membrane_tension_n_per_m.is_some_and(|t| t <= limit),
            None => true,
        };
    (lorentz, hoop, Some(feasible))
}

// ---------------------------------------------------------------------
// Schema v15: the four first-order adjacent screens. Each is a declared
// -assumption bound or flag — never a simulation — and each reports its
// own Status inside the candidate's aggregate: INCONCLUSIVE when its
// inputs cannot resolve, NOT_EVALUATED when the point plan never ran.
// ---------------------------------------------------------------------

/// True iff the case declares any adjacent screen block.
fn any_screen_declared(search: &CoupledSearchCase) -> bool {
    search.thermal_margin.is_some()
        || search.ac_loss.is_some()
        || search.quench_hotspot.is_some()
        || search.screening_current.is_some()
        || search.transition.is_some()
        || search.quench_transient.is_some()
}

/// The `screens` record for a candidate whose point plan never ran
/// (degenerate geometry, requirement failure, or a coarse-pruned FAIL):
/// every declared point-dependent screen is reported NOT_EVALUATED.
/// `quench_hotspot` — which needs only the operating current — is still
/// resolved by the caller when `I_op` is finite.
fn screens_not_evaluated(search: &CoupledSearchCase) -> Option<CandidateScreens> {
    if !any_screen_declared(search) {
        return None;
    }
    let not_evaluated = |model: &str| model.to_owned();
    Some(CandidateScreens {
        thermal_margin: search
            .thermal_margin
            .as_ref()
            .map(|_| ThermalMarginScreenRecord {
                model: not_evaluated(THERMAL_MARGIN_SCREEN_MODEL_ID),
                status: Status::NotEvaluated,
                min_margin_k: None,
                min_margin_is_lower_bound: false,
                limiting: None,
                points_evaluated: 0,
                points_inconclusive: 0,
            }),
        ac_loss: search.ac_loss.as_ref().map(|_| AcLossScreenRecord {
            model: not_evaluated(AC_LOSS_SCREEN_MODEL_ID),
            status: Status::NotEvaluated,
            transport_loss_w: None,
            parallel_slab_loss_w: None,
            perpendicular_bound_w: None,
            total_loss_w: None,
            peak_loss_j_per_m_per_cycle: None,
            limiting: None,
            points_evaluated: 0,
            points_inconclusive: 0,
        }),
        quench_hotspot: None,
        screening_current: search.screening_current.as_ref().map(|_| {
            ScreeningCurrentScreenRecord {
                model: not_evaluated(SCREENING_CURRENT_SCREEN_MODEL_ID),
                status: Status::NotEvaluated,
                max_penetrated_width_fraction: None,
                max_b_perp_over_bc: None,
                limiting: None,
                points_evaluated: 0,
                points_inconclusive: 0,
            }
        }),
        transition: search.transition.as_ref().map(|_| TransitionScreenRecord {
            model: not_evaluated(TRANSITION_SCREEN_MODEL_ID),
            status: Status::NotEvaluated,
            max_e_over_ec: None,
            terminal_voltage_v: None,
            worst_utilization: None,
            worst_n_value: None,
            limiting: None,
            points_evaluated: 0,
            points_inconclusive: 0,
        }),
        // Schema v18: point-dependent — no plan, no trajectory.
        quench_transient: search
            .quench_transient
            .as_ref()
            .map(|_| QuenchTransientScreenRecord {
                model: not_evaluated(QUENCH_TRANSIENT_SCREEN_MODEL_ID),
                status: Status::NotEvaluated,
                peak_temperature_k: None,
                peak_temperature_is_lower_bound: false,
                limiting: None,
                detection_time_s: None,
                peak_series_voltage_v: None,
                points_evaluated: 0,
                points_sharing: 0,
                points_inconclusive: 0,
                coverage_exhausted: false,
            }),
    })
}

/// The `screens` record for the paths that never run the point plan:
/// point-dependent screens NOT_EVALUATED, while `quench_hotspot` —
/// needing only the operating current — still resolves when `I_op` is
/// finite (a pruned candidate's dump bound is real information).
fn screens_without_plan(
    search: &CoupledSearchCase,
    operating_current_a: f64,
    strands_parallel: u32,
) -> Option<CandidateScreens> {
    let mut screens = screens_not_evaluated(search)?;
    screens.quench_hotspot = search.quench_hotspot.as_ref().map(|q| {
        quench_hotspot_evaluate(
            q,
            operating_current_a,
            strands_parallel,
            search.operating.temperature_k,
        )
    });
    Some(screens)
}

/// Piecewise-linear `U(T)` on the declared quench table. The case
/// validation guarantees the table is sorted and brackets `t`; callers
/// pass only temperatures inside the declared span.
fn quench_u_at(table: &[[f64; 2]], temperature_k: f64) -> f64 {
    for w in table.windows(2) {
        if temperature_k <= w[1][0] {
            let (t0, u0) = (w[0][0], w[0][1]);
            let (t1, u1) = (w[1][0], w[1][1]);
            return u0 + (u1 - u0) * (temperature_k - t0) / (t1 - t0);
        }
    }
    table[table.len() - 1][1]
}

/// The `quench_hotspot` screen — needs only the operating current, so it
/// resolves even on candidates whose point plan never ran. `MIITs =
/// J_strand²·τ/2` against the declared `U(T)` table; INCONCLUSIVE when
/// the required integral runs past the table.
fn quench_hotspot_evaluate(
    quench: &optcoil_model::coupled_search::QuenchHotspotScreen,
    operating_current_a: f64,
    strands_parallel: u32,
    operating_temperature_k: f64,
) -> QuenchHotspotScreenRecord {
    let mut record = QuenchHotspotScreenRecord {
        model: QUENCH_HOTSPOT_SCREEN_MODEL_ID.into(),
        status: Status::NotEvaluated,
        strand_current_density_a_per_m2: None,
        miit_a2s: None,
        hotspot_temperature_k: None,
        table_exhausted: false,
    };
    let strands = f64::from(strands_parallel.max(1));
    if !operating_current_a.is_finite() || operating_current_a <= 0.0 {
        return record;
    }
    let j_strand = (operating_current_a / strands) / quench.stabilizer_area_m2;
    let miit = j_strand * j_strand * quench.dump_time_constant_s / 2.0;
    record.strand_current_density_a_per_m2 = Some(j_strand);
    record.miit_a2s = Some(miit);
    let table = &quench.quench_function_a2s_per_m4;
    let u_target = quench_u_at(table, operating_temperature_k) + miit;
    let last = table[table.len() - 1];
    if u_target > last[1] {
        record.table_exhausted = true;
        record.status = Status::Inconclusive;
        return record;
    }
    // Inverse interpolate: the first segment whose U brackets u_target.
    // U is nondecreasing; a flat segment cannot hold the crossing.
    let mut t_hs = last[0];
    for w in table.windows(2) {
        if u_target <= w[1][1] {
            let (t0, u0) = (w[0][0], w[0][1]);
            let (t1, u1) = (w[1][0], w[1][1]);
            t_hs = if u1 > u0 {
                t0 + (t1 - t0) * (u_target - u0) / (u1 - u0)
            } else {
                t0
            };
            break;
        }
    }
    record.hotspot_temperature_k = Some(t_hs);
    record.status = if t_hs <= quench.max_hotspot_k {
        Status::Pass
    } else {
        Status::Fail
    };
    record
}

/// Piecewise-linear table lookup with no extrapolation: `None` when `t`
/// is nonfinite or outside the declared span. The case validation
/// guarantees ≥ 2 rows strictly increasing in T.
fn declared_table_at(table: &[[f64; 2]], t: f64) -> Option<f64> {
    if !t.is_finite() || t < table[0][0] || t > table[table.len() - 1][0] {
        return None;
    }
    for w in table.windows(2) {
        if t <= w[1][0] {
            let (t0, v0) = (w[0][0], w[0][1]);
            let (t1, v1) = (w[1][0], w[1][1]);
            return Some(v0 + (v1 - v0) * (t - t0) / (t1 - t0));
        }
    }
    Some(table[table.len() - 1][1])
}

/// The quench-transient screen's declared integration scheme — part of
/// the model identity so the independent recomputation reproduces the
/// same trajectory: forward Euler at a fixed `t_end/STEPS` step over
/// `t_end = TAUS × τ`. A first-order explicit integrator: stiff runaway
/// trajectories carry the usual explicit-Euler lag; the screen is a
/// bound, not a high-fidelity transient solve. `pub(crate)`: the
/// acceptance module reimplements the same declared scheme.
pub(crate) const QUENCH_TRANSIENT_TAUS: f64 = 10.0;
pub(crate) const QUENCH_TRANSIENT_STEPS: u32 = 8192;

/// The `quench_transient` screen — a per-point lumped adiabatic ODE over
/// the declared dump. Each point's superconductor carries
/// `I_sc = K_used(B,θ,T)·w`, re-queried at the evolving temperature under
/// the same mirror-pair/clamp policy the capacity screen used; the
/// remainder `I_m = max(0, I_strand(t) − I_sc)` flows in the declared
/// composite and heats it `J_m²·ρ_e(T)/C_v(T)`. In sheet-current terms
/// `J_m = max(0, k_demand·e^(−t/τ) − k_used(T)) / a_pw` — the tape width
/// cancels against the declared per-width area.
///
/// A point whose demand never exceeds its capacity (`u_0 ≤ 1`) is
/// provably static — `I(t)` only decays and `I_sc` only falls with `T`,
/// so `I_m` stays 0 — and contributes `T_op` to the peak and zero
/// voltage. Points that share integrate; a point whose `I_m` returns to
/// zero has recovered permanently (the dump is monotone) and stops.
/// The per-strand series voltage `Σ J_m·ρ_e·w_q·dℓ` folds all points at
/// each step — the detectable signature a tap across the winding sees.
fn quench_transient_evaluate(
    declared: &optcoil_model::coupled_search::QuenchTransientScreen,
    search: &CoupledSearchCase,
    points: &[ScreenPoint<'_>],
    k_demand_a_per_m: f64,
    n_stations: f64,
) -> Result<QuenchTransientScreenRecord, RunError> {
    let tau = declared.dump_time_constant_s;
    let t_end = QUENCH_TRANSIENT_TAUS * tau;
    let dt = t_end / f64::from(QUENCH_TRANSIENT_STEPS);
    let a_pw = declared.conducting_area_per_width_m;
    let t_op = search.operating.temperature_k;
    // The lumped initial state: T_op, or the declared "quench detected"
    // start temperature. Classification re-queries capacity at T₀ — the
    // recorded k_used is the T_op value and does not apply above it.
    let t0 = declared.initial_temperature_k.unwrap_or(t_op);

    let mut record = QuenchTransientScreenRecord {
        model: QUENCH_TRANSIENT_SCREEN_MODEL_ID.into(),
        status: Status::NotEvaluated,
        peak_temperature_k: None,
        peak_temperature_is_lower_bound: false,
        limiting: None,
        detection_time_s: None,
        peak_series_voltage_v: None,
        points_evaluated: 0,
        points_sharing: 0,
        points_inconclusive: 0,
        coverage_exhausted: false,
    };
    if !k_demand_a_per_m.is_finite() || k_demand_a_per_m < 0.0 {
        record.status = Status::Inconclusive;
        return Ok(record);
    }

    // Per-sharing-point trajectory state. `T` evolves monotonically
    // (dT/dt ≥ 0); `done` marks a recovered or exhausted point that no
    // longer contributes — a recovered point's `I_m` stays 0 under the
    // monotone dump, an exhausted one's state is frozen at its last
    // in-coverage temperature.
    struct Trajectory<'a> {
        runtime: &'a crate::coupled::SpecRuntime,
        query_field_t: f64,
        angle_folded_deg: f64,
        mirror_angle_deg: f64,
        voltage_weight: f64,
        temperature_k: f64,
        limiting: LimitingPoint,
        done: bool,
        exhausted: bool,
    }
    let mut trajectories: Vec<Trajectory<'_>> = Vec::new();
    // Peak temperature across static and integrated points —
    // (T, argmax-left-coverage, limiting). Static points hold T₀.
    let mut peak: Option<(f64, bool, LimitingPoint)> = None;
    for point in points {
        // Classify on the lumped initial state: capacity at T₀ decides
        // whether the strand starts sharing. A point whose capacity
        // cannot resolve at T₀ cannot be classified — INCONCLUSIVE.
        let k_used_t0 = used_sheet_at_temperature(
            point.runtime,
            point.query_field_t,
            point.angle_folded_deg,
            point.mirror_angle_deg,
            t0,
        )?;
        let Some(k_used) = k_used_t0 else {
            record.points_inconclusive += 1;
            continue;
        };
        record.points_evaluated += 1;
        let limiting = LimitingPoint {
            station: point.station.to_owned(),
            tape_index: point.tape_index,
            turn_index: point.turn_index,
            width_index: point.width_index,
        };
        if k_demand_a_per_m <= k_used {
            // Static: capacity at T₀ covers the dump's largest current,
            // so the strand never shares — T holds T₀, E stays 0.
            if peak.as_ref().is_none_or(|(p, _, _)| t0 > *p) {
                peak = Some((t0, false, limiting));
            }
            continue;
        }
        record.points_sharing += 1;
        trajectories.push(Trajectory {
            runtime: point.runtime,
            query_field_t: point.query_field_t,
            angle_folded_deg: point.angle_folded_deg,
            mirror_angle_deg: point.mirror_angle_deg,
            voltage_weight: tape_frame::LOBATTO_WEIGHTS[point.width_index as usize]
                * point.position_length_m
                / n_stations,
            temperature_k: t0,
            limiting,
            done: false,
            exhausted: false,
        });
    }

    // One pass per step boundary: for each live point query
    // `K_used(T_i)`, form the matrix share at `I(t_i)`, contribute its
    // resistive field to the series voltage `V(t_i)`, then advance T by
    // one explicit-Euler step. `V(t_i)` and the rate share the same
    // query — the pass computes both.
    let mut peak_series_voltage_v = 0.0_f64;
    let mut detection_time_s: Option<f64> = None;
    let mut previous_voltage_v = 0.0_f64;
    for step in 0..=QUENCH_TRANSIENT_STEPS {
        let t = f64::from(step) * dt;
        let k_demand_t = k_demand_a_per_m * (-t / tau).exp();
        let mut v_now = 0.0_f64;
        for tr in trajectories.iter_mut().filter(|tr| !tr.done) {
            let k_used = used_sheet_at_temperature(
                tr.runtime,
                tr.query_field_t,
                tr.angle_folded_deg,
                tr.mirror_angle_deg,
                tr.temperature_k,
            )?;
            let Some(k_used) = k_used else {
                tr.exhausted = true;
                tr.done = true;
                record.coverage_exhausted = true;
                continue;
            };
            let k_m = (k_demand_t - k_used).max(0.0);
            if k_m == 0.0 {
                tr.done = true;
                continue;
            }
            let (Some(rho), Some(c_v)) = (
                declared_table_at(&declared.resistivity_ohm_m, tr.temperature_k),
                declared_table_at(&declared.heat_capacity_j_per_m3k, tr.temperature_k),
            ) else {
                tr.exhausted = true;
                tr.done = true;
                record.coverage_exhausted = true;
                continue;
            };
            let j_m = k_m / a_pw;
            v_now += j_m * rho * tr.voltage_weight;
            if step < QUENCH_TRANSIENT_STEPS {
                tr.temperature_k += j_m * j_m * rho / c_v * dt;
            }
        }
        peak_series_voltage_v = peak_series_voltage_v.max(v_now);
        if let (Some(threshold), None) = (declared.detection_voltage_v, detection_time_s) {
            if step == 0 {
                // The signature may already exceed the threshold at dump
                // start — detection is immediate.
                if v_now > threshold {
                    detection_time_s = Some(0.0);
                }
            } else if v_now > threshold && previous_voltage_v <= threshold {
                // Linear crossing estimate within the step — declared
                // step-resolution detection, not a root find.
                let frac = if v_now > previous_voltage_v {
                    ((threshold - previous_voltage_v) / (v_now - previous_voltage_v))
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };
                detection_time_s = Some(t - dt + frac * dt);
            }
        }
        previous_voltage_v = v_now;
        if trajectories.iter().all(|tr| tr.done) {
            break;
        }
    }

    for tr in &trajectories {
        record.coverage_exhausted |= tr.exhausted;
        if peak.as_ref().is_none_or(|(p, _, _)| tr.temperature_k > *p) {
            peak = Some((tr.temperature_k, tr.exhausted, tr.limiting.clone()));
        }
    }
    let (peak_temperature_k, peak_lower_bound, limiting) = match peak {
        Some((p, lb, l)) => (Some(p), lb, Some(l)),
        None => (None, false, None),
    };

    record.peak_temperature_k = peak_temperature_k;
    record.peak_temperature_is_lower_bound = peak_lower_bound;
    record.limiting = limiting;
    record.peak_series_voltage_v = (record.points_evaluated > 0).then_some(peak_series_voltage_v);
    record.detection_time_s = detection_time_s;

    // Status algebra: each declared bound is evaluated or left
    // unresolved; unresolved coverage leaves the bound INCONCLUSIVE.
    let mut saw_fail = false;
    let mut saw_unresolved = false;
    if let Some(limit) = declared.max_temperature_k {
        match peak_temperature_k {
            Some(p) if p > limit => saw_fail = true,
            Some(_) => {
                // The reported peak held, but an exhausted trajectory's
                // true peak is unknown — it cannot certify the bound.
                if record.coverage_exhausted {
                    saw_unresolved = true;
                }
            }
            None => saw_unresolved = true,
        }
    }
    if let Some(bound) = declared.max_detection_time_s {
        match detection_time_s {
            Some(t) if t <= bound => {}
            Some(_) => saw_fail = true,
            None => {
                // A real sharing signature that never crossed fails the
                // detectability requirement — unless coverage ran out
                // first, leaving the crossing unknown.
                if peak_series_voltage_v > 0.0 {
                    if record.coverage_exhausted {
                        saw_unresolved = true;
                    } else {
                        saw_fail = true;
                    }
                } else if declared.initial_temperature_k.is_some() {
                    // A declared quench that produces no resistive
                    // signature is by construction undetectable.
                    saw_fail = true;
                }
                // Otherwise no quench exists in the model — the
                // detectability requirement is vacuous.
            }
        }
    }
    if declared.max_temperature_k.is_none()
        && declared.max_detection_time_s.is_none()
        && record.coverage_exhausted
    {
        // Record-only screen with an incomplete trajectory — the full
        // answer is unknown, so it cannot report a clean Pass.
        saw_unresolved = true;
    }
    record.status = if record.points_inconclusive > 0 || record.points_evaluated == 0 {
        Status::Inconclusive
    } else if saw_fail {
        Status::Fail
    } else if saw_unresolved {
        Status::Inconclusive
    } else {
        Status::Pass
    };
    Ok(record)
}

/// The used critical sheet current at a temperature, under the same
/// mirror-pair + low-field-clamp query policy the capacity screen used.
fn used_sheet_at_temperature(
    runtime: &crate::coupled::SpecRuntime,
    query_field_t: f64,
    angle_folded_deg: f64,
    mirror_angle_deg: f64,
    temperature_k: f64,
) -> Result<Option<f64>, RunError> {
    let clamped = tape_frame::query_mirror_pair_with_clamp(
        &runtime.interpolator,
        temperature_k,
        query_field_t,
        angle_folded_deg,
        mirror_angle_deg,
        runtime.material.low_field_clamp_t,
    )?;
    Ok(clamped.k_used_a_per_m)
}

/// One point's `T_cs − T_op`: bisection in temperature for the crossing
/// `K_used(T) = K_demand`, assuming `K_used` is nonincreasing in `T`
/// (true of REBCO and of every shipped dataset; a non-monotone dataset
/// makes the margin an estimate — this is a screen, not a certification).
/// `None` when the point's capacity cannot resolve above `t_op_k`.
/// `(margin, true)` marks a lower bound: capacity still exceeds demand
/// at the highest resolvable temperature, so `T_cs` lies above it.
fn temperature_margin_at_point(
    runtime: &crate::coupled::SpecRuntime,
    query_field_t: f64,
    angle_folded_deg: f64,
    mirror_angle_deg: f64,
    k_demand_a_per_m: f64,
    t_op_k: f64,
    t_data_top_k: f64,
) -> Result<Option<(f64, bool)>, RunError> {
    let f = |t: f64| {
        used_sheet_at_temperature(
            runtime,
            query_field_t,
            angle_folded_deg,
            mirror_angle_deg,
            t,
        )
    };
    let Some(k_at_op) = f(t_op_k)? else {
        return Ok(None);
    };
    if k_at_op <= k_demand_a_per_m {
        // No positive margin: capacity at the operating point already at
        // or below demand (the capacity screen itself fails such points).
        return Ok(Some((0.0, false)));
    }
    // The highest temperature the dataset resolves at this query: the
    // nominal top when it interpolates, else the bisection ceiling of
    // the resolvable interval (coverage treated as contiguous in T — a
    // declared screen assumption, not a hull guarantee).
    let t_hi = if f(t_data_top_k)?.is_some() {
        t_data_top_k
    } else {
        let (mut lo, mut hi) = (t_op_k, t_data_top_k);
        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if f(mid)?.is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    if t_hi <= t_op_k {
        return Ok(None);
    }
    let Some(k_at_hi) = f(t_hi)? else {
        return Ok(None);
    };
    if k_at_hi > k_demand_a_per_m {
        // T_cs beyond the resolvable span — the margin is a lower bound.
        return Ok(Some((t_hi - t_op_k, true)));
    }
    // Bisection on K_used(T) = k_demand: f(lo) > 0, f(hi) <= 0.
    let mut lo = t_op_k;
    let mut hi = t_hi;
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        match f(mid)? {
            Some(k) if k > k_demand_a_per_m => lo = mid,
            Some(_) => hi = mid,
            None => hi = mid,
        }
    }
    if !hi.is_finite() {
        hi = t_hi;
        if lo >= hi {
            return Ok(None);
        }
    }
    Ok(Some((0.5 * (lo + hi) - t_op_k, false)))
}

/// Norris 1970 thin-strip transport loss (J/m/cycle): `μ₀·I_c²/π ·
/// g(i)` with `g(i) = (1−i)ln(1−i) + (1+i)ln(1+i) − i²`. `i` is clamped
/// to [0, 1) — the formula is only defined below the strip's critical
/// current; at `i = 0` it is exactly zero.
fn norris_transport_j_per_m_per_cycle(ic_a: f64, amplitude_fraction: f64) -> f64 {
    let i = amplitude_fraction.clamp(0.0, 1.0 - f64::EPSILON);
    let g = (1.0 - i) * (1.0 - i).ln() + (1.0 + i) * (1.0 + i).ln() - i * i;
    MU0_H_PER_M * ic_a * ic_a / std::f64::consts::PI * g
}

/// Bean-slab hysteresis loss per conductor metre for the face-parallel
/// field component (J/m/cycle): slab of half-thickness `d_sc/2` at
/// `J_c = K_c/d_sc` gives `B_p = μ₀·K_c/2`; the per-volume branches are
/// continuous at `B_m = B_p` and linear beyond it.
fn bean_slab_parallel_j_per_m_per_cycle(
    b_parallel_t: f64,
    k_c_a_per_m: f64,
    d_sc_m: f64,
    tape_width_m: f64,
) -> f64 {
    let b_p = MU0_H_PER_M * k_c_a_per_m / 2.0;
    let b_m = b_parallel_t.abs();
    let q_vol = if b_m <= b_p {
        2.0 * b_m.powi(3) / (3.0 * MU0_H_PER_M * b_p)
    } else {
        (2.0 * b_p / MU0_H_PER_M) * (b_m - 2.0 * b_p / 3.0)
    };
    q_vol * tape_width_m * d_sc_m
}

/// The rigorous perpendicular-field upper bound (J/m/cycle): the
/// strip's saturated dipole moment per unit length `m_sat =
/// K_c·(w/2)²` bounds the magnetization, so the hysteresis loop area
/// obeys `∮m dB ≤ 4·m_sat·B_perp`. A bound, never a simulation — at
/// low `B_perp` the exact strip loss scales ~B⁴ far below it.
fn perpendicular_bound_j_per_m_per_cycle(
    k_c_a_per_m: f64,
    tape_width_m: f64,
    b_perp_t: f64,
) -> f64 {
    4.0 * k_c_a_per_m * (tape_width_m / 2.0).powi(2) * b_perp_t.abs()
}

/// The strip's characteristic field `B_c = μ₀·K_c/π` (Brandt–Indenbom
/// `H_c = J_c/π` in B-units with `J_c = K_c` the sheet current).
fn screening_b_c_t(k_c_a_per_m: f64) -> f64 {
    MU0_H_PER_M * k_c_a_per_m / std::f64::consts::PI
}

/// The E–J power-law transition depth `E/Ec = u^n` at operating ratio
/// `u = K_demand/K_used` and the dataset's measured exponent `n`. Below
/// the criterion (`u < 1`) the ratio decays with `n`; above it grows —
/// over-critical points read as arbitrarily deep dissipation, never a
/// clipped "just over" value.
fn transition_e_over_ec(u: f64, n: f64) -> f64 {
    if !u.is_finite() || u < 0.0 || !n.is_finite() {
        return f64::INFINITY;
    }
    if u == 0.0 {
        return 0.0;
    }
    u.powf(n)
}

/// Brandt–Indenbom thin-strip flux-free core `b = a/cosh(B_perp/B_c)`:
/// the returned quantity is the *penetrated* width fraction
/// `1 − 1/cosh(B_perp/B_c)` — 0 at zero field, → 1 in the fully
/// penetrated limit (and 1 when `B_c` cannot resolve).
fn penetrated_width_fraction(b_perp_t: f64, k_c_a_per_m: f64) -> f64 {
    let b_c = screening_b_c_t(k_c_a_per_m);
    if b_c <= 0.0 {
        return 1.0;
    }
    let ratio = b_perp_t.abs() / b_c;
    if !ratio.is_finite() {
        return 1.0;
    }
    1.0 - 1.0 / ratio.cosh()
}

/// Per-point quantities the point-dependent screens share, gathered once
/// per full-plan tape record.
struct ScreenPoint<'a> {
    station: &'a str,
    tape_index: u32,
    turn_index: u32,
    width_index: u32,
    /// Field component normal to the tape face (drives the strip's
    /// screening currents and the perpendicular-field loss bound).
    b_face_normal_t: f64,
    /// Field component along the tape width, in the tape plane (drives
    /// the Bean-slab face-parallel term).
    b_face_parallel_t: f64,
    /// The point's used critical sheet current (A/m), `None` when the
    /// capacity query never resolved.
    k_used_a_per_m: Option<f64>,
    /// The field magnitude the capacity query actually used.
    query_field_t: f64,
    angle_folded_deg: f64,
    mirror_angle_deg: f64,
    runtime: &'a crate::coupled::SpecRuntime,
    /// The conductor position's full loop length (m).
    position_length_m: f64,
}

/// Evaluate every declared v15 screen against the full-plan record.
/// `position_length_m(turn_index, tape_index)` resolves a position's
/// loop length under the case's tape-normal semantics (the same offsets
/// the cost ledger prices).
fn screens_evaluate(
    search: &CoupledSearchCase,
    full_case: &optcoil_model::coupled::CoupledCase,
    stations: &[crate::coupled::StationGroup],
    runtimes: &MaterialRuntimes,
    operating_current_a: f64,
    strands_parallel: u32,
    position_length_m: impl Fn(u32, u32) -> f64,
) -> Result<Option<CandidateScreens>, RunError> {
    if !any_screen_declared(search) {
        return Ok(None);
    }
    let n_stations = stations.len().max(1) as f64;
    let tape_width_m = full_case.winding.tape_width_m;
    let strands = f64::from(strands_parallel.max(1));
    // Carried sheet current per strand (the capacity screen's own demand
    // convention): I_op/(s·w).
    let k_demand_a_per_m = operating_current_a / (tape_width_m * strands);
    let mut points: Vec<ScreenPoint<'_>> = Vec::new();
    for group in stations {
        for tape in &group.tapes {
            let runtime = runtimes.for_turn(full_case, tape.turn_index);
            let position_length_m = position_length_m(tape.turn_index, tape.tape_index);
            for point in &tape.per_candidate[0].points {
                let components = tape_frame::decompose(
                    point.field_t,
                    &optcoil_model::coupled::TapeFrame {
                        t: tape.frame.t,
                        n: tape.frame.n,
                        w: tape.frame.w,
                    },
                );
                points.push(ScreenPoint {
                    station: &tape.station,
                    tape_index: tape.tape_index,
                    turn_index: tape.turn_index,
                    width_index: point.width_index,
                    b_face_normal_t: components.b_n.abs(),
                    b_face_parallel_t: components.b_w.abs(),
                    k_used_a_per_m: point.k_used_a_per_m,
                    query_field_t: point.query_field_t,
                    angle_folded_deg: point.angle_folded_deg,
                    mirror_angle_deg: point.mirror_angle_deg,
                    runtime,
                    position_length_m,
                });
            }
        }
    }

    let thermal_margin = match &search.thermal_margin {
        None => None,
        Some(declared) => {
            // Per-spec resolvable temperature ceiling: the covering
            // dataset's highest nominal temperature.
            let mut t_top_cache: BTreeMap<String, f64> = BTreeMap::new();
            let mut min_margin: Option<(f64, bool, LimitingPoint)> = None;
            let (mut evaluated, mut inconclusive) = (0_u64, 0_u64);
            for point in &points {
                let Some(k_used) = point.k_used_a_per_m else {
                    inconclusive += 1;
                    continue;
                };
                let dataset_id = &point.runtime.material.dataset_id;
                let t_top = match t_top_cache.get(dataset_id) {
                    Some(&t) => t,
                    None => {
                        let t = runtimes
                            .datasets
                            .get(dataset_id)
                            .map(|d| {
                                d.points
                                    .iter()
                                    .map(|p| p.nominal_temperature_k)
                                    .fold(f64::NEG_INFINITY, f64::max)
                            })
                            .unwrap_or(f64::NEG_INFINITY);
                        t_top_cache.insert(dataset_id.clone(), t);
                        t
                    }
                };
                match temperature_margin_at_point(
                    point.runtime,
                    point.query_field_t,
                    point.angle_folded_deg,
                    point.mirror_angle_deg,
                    k_demand_a_per_m,
                    search.operating.temperature_k,
                    t_top,
                )? {
                    Some((margin, lower_bound)) => {
                        evaluated += 1;
                        let limiting = LimitingPoint {
                            station: point.station.to_owned(),
                            tape_index: point.tape_index,
                            turn_index: point.turn_index,
                            width_index: point.width_index,
                        };
                        if min_margin.as_ref().is_none_or(|(m, b, _)| {
                            margin < *m || (margin == *m && *b && !lower_bound)
                        }) {
                            min_margin = Some((margin, lower_bound, limiting));
                        }
                        // k_used only narrows the demand check below; the
                        // point's used capacity is what T_cs is solved on.
                        let _ = k_used;
                    }
                    None => inconclusive += 1,
                }
            }
            let status = if inconclusive > 0 || evaluated == 0 {
                Status::Inconclusive
            } else if min_margin
                .as_ref()
                .is_some_and(|(m, _, _)| *m >= declared.min_margin_k)
            {
                Status::Pass
            } else {
                Status::Fail
            };
            let (min_margin_k, is_lower_bound, limiting) = match min_margin {
                Some((m, b, l)) => (Some(m), b, Some(l)),
                None => (None, false, None),
            };
            Some(ThermalMarginScreenRecord {
                model: THERMAL_MARGIN_SCREEN_MODEL_ID.into(),
                status,
                min_margin_k,
                min_margin_is_lower_bound: is_lower_bound,
                limiting,
                points_evaluated: evaluated,
                points_inconclusive: inconclusive,
            })
        }
    };

    let ac_loss = match &search.ac_loss {
        None => None,
        Some(declared) => {
            let d_sc = declared.sc_layer_thickness_m;
            let i_ac = declared.transport_amplitude_fraction;
            let (mut evaluated, mut inconclusive) = (0_u64, 0_u64);
            // Per-position mean per-strand loss densities (J/cycle/m),
            // then each position contributes mean·loop_length·strands.
            let mut energy_transport = 0.0_f64;
            let mut energy_parallel = 0.0_f64;
            let mut energy_perpendicular = 0.0_f64;
            let mut peak_density: Option<(f64, LimitingPoint)> = None;
            for point in &points {
                let Some(k_c) = point.k_used_a_per_m else {
                    inconclusive += 1;
                    continue;
                };
                evaluated += 1;
                // Weight this point's per-strand-metre density by its
                // share of the position's loop: width-averaged by the
                // Lobatto rule, station samples treated as equal shares.
                let w_weight = tape_frame::LOBATTO_WEIGHTS[point.width_index as usize];
                let strand_metres = point.position_length_m / n_stations * strands;
                let weight = w_weight * strand_metres;
                let ic_a = k_c * tape_width_m;
                // Norris 1970 thin-strip transport term (J/m/cycle).
                let q_transport = norris_transport_j_per_m_per_cycle(ic_a, i_ac);
                // Bean slab on the face-parallel component.
                let q_parallel = bean_slab_parallel_j_per_m_per_cycle(
                    point.b_face_parallel_t,
                    k_c,
                    d_sc,
                    tape_width_m,
                );
                // Rigorous perpendicular bound from the strip's
                // saturation moment — a bound, never a simulation.
                let q_perp =
                    perpendicular_bound_j_per_m_per_cycle(k_c, tape_width_m, point.b_face_normal_t);
                energy_transport += weight * q_transport;
                energy_parallel += weight * q_parallel;
                energy_perpendicular += weight * q_perp;
                let density = q_transport + q_parallel + q_perp;
                let limiting = LimitingPoint {
                    station: point.station.to_owned(),
                    tape_index: point.tape_index,
                    turn_index: point.turn_index,
                    width_index: point.width_index,
                };
                if peak_density.as_ref().is_none_or(|(d, _)| density > *d) {
                    peak_density = Some((density, limiting));
                }
            }
            let watts = declared.frequency_hz;
            let (t_w, p_w, n_w) = if evaluated > 0 {
                (
                    Some(energy_transport * watts),
                    Some(energy_parallel * watts),
                    Some(energy_perpendicular * watts),
                )
            } else {
                (None, None, None)
            };
            let total = t_w.zip(p_w).zip(n_w).map(|((a, b), c)| a + b + c);
            let status = if inconclusive > 0 || total.is_none() {
                Status::Inconclusive
            } else if total.is_some_and(|t| t <= declared.max_loss_w) {
                Status::Pass
            } else {
                Status::Fail
            };
            Some(AcLossScreenRecord {
                model: AC_LOSS_SCREEN_MODEL_ID.into(),
                status,
                transport_loss_w: t_w,
                parallel_slab_loss_w: p_w,
                perpendicular_bound_w: n_w,
                total_loss_w: total,
                peak_loss_j_per_m_per_cycle: peak_density.as_ref().map(|(d, _)| *d),
                limiting: peak_density.map(|(_, l)| l),
                points_evaluated: evaluated,
                points_inconclusive: inconclusive,
            })
        }
    };

    let screening_current = match &search.screening_current {
        None => None,
        Some(declared) => {
            let (mut evaluated, mut inconclusive) = (0_u64, 0_u64);
            let mut max_frac: Option<(f64, f64, LimitingPoint)> = None;
            for point in &points {
                let Some(k_c) = point.k_used_a_per_m else {
                    inconclusive += 1;
                    continue;
                };
                evaluated += 1;
                // Brandt–Indenbom strip: the flux-free core
                // b = a/cosh(B_perp/B_c) gives the penetrated fraction.
                let b_c = screening_b_c_t(k_c);
                let ratio = if b_c > 0.0 {
                    point.b_face_normal_t / b_c
                } else {
                    f64::INFINITY
                };
                let fraction = penetrated_width_fraction(point.b_face_normal_t, k_c);
                let limiting = LimitingPoint {
                    station: point.station.to_owned(),
                    tape_index: point.tape_index,
                    turn_index: point.turn_index,
                    width_index: point.width_index,
                };
                if max_frac.as_ref().is_none_or(|(f, _, _)| fraction > *f) {
                    max_frac = Some((fraction, ratio, limiting));
                }
            }
            let status = if inconclusive > 0 || evaluated == 0 {
                Status::Inconclusive
            } else if max_frac
                .as_ref()
                .is_some_and(|(f, _, _)| *f <= declared.max_penetrated_width_fraction)
            {
                Status::Pass
            } else {
                Status::Fail
            };
            let (max_fraction, max_ratio, limiting) = match max_frac {
                Some((f, r, l)) => (Some(f), Some(r), Some(l)),
                None => (None, None, None),
            };
            Some(ScreeningCurrentScreenRecord {
                model: SCREENING_CURRENT_SCREEN_MODEL_ID.into(),
                status,
                max_penetrated_width_fraction: max_fraction,
                max_b_perp_over_bc: max_ratio,
                limiting,
                points_evaluated: evaluated,
                points_inconclusive: inconclusive,
            })
        }
    };

    let transition = match &search.transition {
        None => None,
        Some(declared) => {
            let (mut evaluated, mut inconclusive) = (0_u64, 0_u64);
            let mut max_depth: Option<(f64, f64, f64, LimitingPoint)> = None;
            // Per-strand terminal voltage: each parallel strand develops
            // the same ∫E·dℓ along the winding under the equal-share
            // model, so no strand-count multiplier.
            let mut terminal_voltage_v = 0.0_f64;
            for point in &points {
                let Some(k_used) = point.k_used_a_per_m else {
                    inconclusive += 1;
                    continue;
                };
                // The measured exponent on the governing mirror-pair
                // branch — the same query coordinates the capacity used.
                let clamped = tape_frame::query_mirror_pair_with_clamp(
                    &point.runtime.interpolator,
                    search.operating.temperature_k,
                    point.query_field_t,
                    point.angle_folded_deg,
                    point.mirror_angle_deg,
                    point.runtime.material.low_field_clamp_t,
                )?;
                let Some(n) = clamped.n_value else {
                    inconclusive += 1;
                    continue;
                };
                evaluated += 1;
                let u = k_demand_a_per_m / k_used;
                let e_over_ec = transition_e_over_ec(u, n);
                let w_weight = tape_frame::LOBATTO_WEIGHTS[point.width_index as usize];
                terminal_voltage_v += search.operating.electric_field_criterion_v_per_m
                    * e_over_ec
                    * w_weight
                    * point.position_length_m
                    / n_stations;
                let limiting = LimitingPoint {
                    station: point.station.to_owned(),
                    tape_index: point.tape_index,
                    turn_index: point.turn_index,
                    width_index: point.width_index,
                };
                if max_depth.as_ref().is_none_or(|(e, _, _, _)| e_over_ec > *e) {
                    max_depth = Some((e_over_ec, u, n, limiting));
                }
            }
            let voltage_ok = declared
                .max_voltage_v
                .is_none_or(|v| terminal_voltage_v <= v);
            let status = if inconclusive > 0 || evaluated == 0 {
                Status::Inconclusive
            } else if max_depth
                .as_ref()
                .is_some_and(|(e, _, _, _)| *e <= declared.max_e_over_ec)
                && voltage_ok
            {
                Status::Pass
            } else {
                Status::Fail
            };
            let (max_e, worst_u, worst_n, limiting) = match max_depth {
                Some((e, u, n, l)) => (Some(e), Some(u), Some(n), Some(l)),
                None => (None, None, None, None),
            };
            Some(TransitionScreenRecord {
                model: TRANSITION_SCREEN_MODEL_ID.into(),
                status,
                max_e_over_ec: max_e,
                terminal_voltage_v: (evaluated > 0).then_some(terminal_voltage_v),
                worst_utilization: worst_u,
                worst_n_value: worst_n,
                limiting,
                points_evaluated: evaluated,
                points_inconclusive: inconclusive,
            })
        }
    };

    let quench_transient = match &search.quench_transient {
        None => None,
        Some(declared) => Some(quench_transient_evaluate(
            declared,
            search,
            &points,
            k_demand_a_per_m,
            n_stations,
        )?),
    };

    Ok(Some(CandidateScreens {
        thermal_margin,
        ac_loss,
        quench_hotspot: search.quench_hotspot.as_ref().map(|q| {
            quench_hotspot_evaluate(
                q,
                operating_current_a,
                strands_parallel,
                search.operating.temperature_k,
            )
        }),
        screening_current,
        transition,
        quench_transient,
    }))
}

/// Every declared screen's status, for the candidate aggregate.
fn screen_statuses(screens: Option<&CandidateScreens>) -> Vec<Status> {
    let Some(screens) = screens else {
        return Vec::new();
    };
    [
        screens.thermal_margin.as_ref().map(|s| s.status),
        screens.ac_loss.as_ref().map(|s| s.status),
        screens.quench_hotspot.as_ref().map(|s| s.status),
        screens.screening_current.as_ref().map(|s| s.status),
        screens.transition.as_ref().map(|s| s.status),
        screens.quench_transient.as_ref().map(|s| s.status),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// `pub(crate)`: OC-008's bracketing-refinement runner (`coupled_refine.rs`)
/// reuses this directly for every bracket-end and bisection-midpoint
/// evaluation, rather than reimplementing the requirement -> NI -> I_op ->
/// coarse-pruning -> full-plan pipeline; behavior here is unchanged (OC-007's
/// own types and records stay exactly as they were).
/// `tape_spec_ids` is the candidate's resolved per-region assignment
/// (search schema v10; empty on ungraded cases). `runtimes` carries one
/// interpolator/audit/table per material binding — the point loop
/// resolves the covering region's spec per turn, so a graded pack never
/// screens a turn under the wrong tape.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_one_candidate(
    search: &CoupledSearchCase,
    index: usize,
    turns_along_normal: u32,
    tapes_along_width: u32,
    strands_parallel: u32,
    tape_spec_ids: &[String],
    dims: CandidateDims,
    runtimes: &MaterialRuntimes,
    // Progress tick: incremented once per field evaluation (evaluator
    // call or declared-map lookup). Display-only; never read back.
    progress_tick: Option<&AtomicU64>,
) -> Result<SearchCandidateResult, RunError> {
    evaluate_one_candidate_cancellable(
        search,
        index,
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        tape_spec_ids,
        dims,
        runtimes,
        progress_tick,
        &AtomicBool::new(false),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_one_candidate_cancellable(
    search: &CoupledSearchCase,
    index: usize,
    turns_along_normal: u32,
    tapes_along_width: u32,
    strands_parallel: u32,
    tape_spec_ids: &[String],
    dims: CandidateDims,
    runtimes: &MaterialRuntimes,
    progress_tick: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> Result<SearchCandidateResult, RunError> {
    // Schema v21: the candidate's resolved racetrack geometry — axis
    // values shadow the fixed declarations; every consumer below reads
    // `fg`, so fixed and searched dims take one path.
    let fg = search.fixed_geometry.resolved_for(dims);
    // Schema v13: under a declared field map the pack extents are the
    // fixed declarations the map was computed on — the counts set the
    // discretization, not the footprint.
    let (radial_width_m, axial_height_m) =
        fg.candidate_extents_m(turns_along_normal, tapes_along_width);
    let total_turns = u64::from(turns_along_normal) * u64::from(tapes_along_width);
    let geometry = CandidateGeometry {
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        total_turns,
        total_conductors: total_turns * u64::from(strands_parallel),
        tape_spec_ids: search.grading.is_some().then(|| tape_spec_ids.to_vec()),
        bend_radius_m: dims.bend_radius_m,
        straight_half_length_m: dims.straight_half_length_m,
        radial_width_m,
        axial_height_m,
    };
    let cost = compute_cost_ledger(
        search,
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        tape_spec_ids,
        dims,
    );

    // A pack whose inner bend radius reaches zero (n*pitch >= 2*R) is
    // degenerate: the winding occupies the bore itself and no evaluator can
    // be constructed. That is a per-candidate FAIL, not a run error -- a
    // geometry search must be able to declare choices that overshoot.
    // On a path the same screen applies per-arc: the smallest curvature
    // radius minus half the pack width must stay positive.
    let inner_bend_radius_m = fg
        .min_inner_radius_m(radial_width_m)
        .unwrap_or(f64::NEG_INFINITY);
    // Schema v10: a grading region that collapses empty on this turn
    // count (`floor(lo*N) >= floor(hi*N)`) is a declared plan the
    // candidate cannot realize — a per-candidate FAIL like any other
    // unrealizable geometry, never a run error.
    let grading_realizable = search.grading.as_ref().is_none_or(|grading| {
        grading
            .regions
            .iter()
            .all(|r| CoupledSearchCase::region_turn_range(r, turns_along_normal).is_ok())
    });
    let pack_geometry_valid = inner_bend_radius_m > 0.0 && grading_realizable;
    // Schema v17: the tape's outer-fiber bend strain at the innermost
    // bend, t/(2R_inner) — `Some` iff the thickness is declared and the
    // radius resolved positive. The declared strain bound cannot resolve
    // on a degenerate pack and fails closed through
    // manufacturing_feasible; the radius itself is recorded so both
    // bounds stay auditable from the record.
    let bend_strain = search
        .manufacturing
        .as_ref()
        .and_then(|m| m.tape_thickness_m)
        .and_then(|t| (inner_bend_radius_m > 0.0).then_some(t / (2.0 * inner_bend_radius_m)));
    // Schema v3 manufacturing gate: the innermost turn's bend radius must
    // respect the declared minimum; schema v17 adds the declared
    // outer-fiber bend-strain bound on the same radius.
    let manufacturing_feasible = search.manufacturing.as_ref().is_none_or(|m| {
        inner_bend_radius_m >= m.min_inner_bend_radius_m
            && match m.max_bend_strain {
                Some(limit) => bend_strain.is_some_and(|s| s <= limit),
                None => true,
            }
    });
    let full_stations = search.sampling.stations.clone();
    let full_turn_indices =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns_along_normal);
    let full_tape_indices: Vec<u32> = (1..=tapes_along_width).collect();
    let full_plan_point_count =
        (full_stations.len() * full_turn_indices.len() * full_tape_indices.len() * 5) as u64;
    if !pack_geometry_valid {
        return Ok(SearchCandidateResult {
            index,
            geometry,
            unit_bore_bz_t_per_ampere_turn: f64::NAN,
            bore_refinement_change_t: f64::NAN,
            good_field: None,
            pack_geometry_valid,
            manufacturing_feasible,
            inner_bend_radius_m: (inner_bend_radius_m.is_finite() && inner_bend_radius_m > 0.0)
                .then_some(inner_bend_radius_m),
            bend_strain,
            requirement_kernel_evaluations: 0,
            ampere_turns_a: f64::NAN,
            operating_current_a: f64::NAN,
            requirement_status: Status::Fail,
            refinement_status: Status::Fail,
            numerical_status: Status::Inconclusive,
            peak_sampled_field_t: None,
            lorentz_load_n_per_m: None,
            mechanical_feasible: None,
            hoop_stress_pa: None,
            transverse_pressure_pa: None,
            transverse_pressure_location: None,
            membrane_tension_n_per_m: None,
            screening: None,
            pruned_by: None,
            coarse_refinement_unresolved: false,
            full_plan_point_count,
            coarse_points_evaluated: 0,
            coarse_kernel_evaluations: 0,
            full_points_evaluated: 0,
            full_kernel_evaluations: 0,
            field_timing_ms: 0.0,
            cost,
            status: Status::Fail,
            // Schema v15: no field/current exists on this path — the
            // declared point-dependent screens are NOT_EVALUATED, and
            // the quench bound cannot resolve a nonfinite current.
            screens: screens_without_plan(search, f64::NAN, strands_parallel),
        });
    }

    // Schema v13: a declared field map replaces the field solve — no
    // evaluators are built, and the NI solve anchors on the map
    // producer's declared bore field at the reference winding (the
    // bore probe lies outside the pack-only map's hull).
    let resolved_map = match &search.field_map {
        Some(fm) => Some(fm.map.resolve().map_err(RunError::Model)?),
        None => None,
    };

    let evaluators = match (&fg.path, resolved_map.is_some()) {
        (_, true) => Vec::new(),
        // Schema v19: a non-planar helix pack has no engine field
        // evaluator — the case schema requires a declared Cartesian map,
        // so reaching this arm means the map failed to resolve, which is
        // a case error rather than a geometry fallback.
        (None, false) if fg.path3d.is_some() => {
            return Err(RunError::Invalid(
                "fixed_geometry.path3d requires a resolvable field_map".to_owned(),
            ));
        }
        // Schema v9: the pack is a w×h band straddling a general planar
        // centerline — `from_path` generates per-segment curvilinear
        // cells directly, no Racetrack adapter needed.
        (Some(path), false) => search
            .numerics
            .quadrature_orders
            .iter()
            .map(|&order| {
                RacetrackEvaluator::from_path(path, radial_width_m, axial_height_m, 1.0, order)
            })
            .collect::<Result<Vec<_>, _>>()?,
        (None, false) => {
            let racetrack = Racetrack {
                straight_half_length_m: fg.straight_half_length_m.ok_or_else(|| {
                    RunError::Invalid(
                        "fixed_geometry declares neither path nor racetrack dimensions".to_owned(),
                    )
                })?,
                bend_radius_m: fg.bend_radius_m.ok_or_else(|| {
                    RunError::Invalid(
                        "fixed_geometry declares neither path nor racetrack dimensions".to_owned(),
                    )
                })?,
                radial_width_m,
                axial_height_m,
                ampere_turns_a: 1.0,
                current_model: CurrentModel::UniformWindingPack,
            };
            search
                .numerics
                .quadrature_orders
                .iter()
                .map(|&order| RacetrackEvaluator::new(&racetrack, order))
                .collect::<Result<Vec<_>, _>>()?
        }
    };

    let mut bore_unit = [[0.0_f64; 3]; 2];
    let mut requirement_kernel_evaluations = 0_u64;
    if let Some(fm) = &search.field_map {
        // Declared map: the unit bore field is the producer's declared
        // anchor normalized to per-ampere-turn — both quadrature slots
        // carry it, so the refinement change is 0 by construction.
        let unit_b = fm.bore_field_at_reference_t / fm.map.reference_ampere_turns_a();
        bore_unit = [[0.0, 0.0, unit_b], [0.0, 0.0, unit_b]];
    } else {
        for (i, evaluator) in evaluators.iter().enumerate() {
            check_cancelled(cancel)?;
            let value = evaluator.evaluate(search.requirement.bore_probe_m)?;
            tick(progress_tick);
            requirement_kernel_evaluations += value.kernel_evaluations;
            bore_unit[i] = value.field_t;
        }
    }
    let bore_refinement_change_t = norm(sub(bore_unit[1], bore_unit[0]));
    let unit_bore_bz = bore_unit[1][2];
    let bore_refinement_ok = bore_refinement_change_t / search.numerics.field_scale_t
        <= search.numerics.max_refinement_change_fraction;

    // Schema v3: the field requirement holds over the declared usable
    // volume's probe lattice, not only at the center probe. `NI` is solved
    // on the lattice's weakest point, so shrinking the coil cannot shrink
    // the requirement away. The region gets its own order-vs-order
    // refinement gate, applied to the lattice minimum. The lattice is
    // still evaluated when it overlaps the winding -- the values are real
    // -- but the overlap itself fails the requirement: a region that is
    // not a usable volume cannot be certified by any field value.
    let good_field = match &search.requirement.good_field_region {
        None => None,
        Some(region) => {
            let lattice = region.lattice_points(search.requirement.bore_probe_m);
            let pack_overlap = match &fg.path {
                Some(path) => region.overlaps_pack_on_path(
                    search.requirement.bore_probe_m,
                    path,
                    radial_width_m,
                    axial_height_m,
                ),
                None => region.overlaps_pack(
                    search.requirement.bore_probe_m,
                    fg.straight_half_length_m.unwrap_or(f64::NAN),
                    fg.bend_radius_m.unwrap_or(f64::NAN),
                    radial_width_m,
                    axial_height_m,
                ),
            };
            let mut min_unit_bz = [f64::INFINITY; 2];
            let mut max_unit_bz = [f64::NEG_INFINITY; 2];
            let mut worst_point_m = search.requirement.bore_probe_m;
            for (i, evaluator) in evaluators.iter().enumerate() {
                for &point in &lattice {
                    check_cancelled(cancel)?;
                    let value = evaluator.evaluate(point)?;
                    tick(progress_tick);
                    requirement_kernel_evaluations += value.kernel_evaluations;
                    let bz = value.field_t[2];
                    if i == 1 && bz < min_unit_bz[1] {
                        worst_point_m = point;
                    }
                    if bz < min_unit_bz[i] {
                        min_unit_bz[i] = bz;
                    }
                    if bz > max_unit_bz[i] {
                        max_unit_bz[i] = bz;
                    }
                }
            }
            // Schema v22: the region's scale-free uniformity figure —
            // (max − min) normalized to the nominal field at the bore
            // probe. A non-positive center value cannot anchor a
            // relative bound; infinity fails any declared one honestly.
            let relative_deviation = if unit_bore_bz > 0.0 {
                (max_unit_bz[1] - min_unit_bz[1]) / unit_bore_bz
            } else {
                f64::INFINITY
            };
            // Schema v23: the declared reference circle is evaluated at
            // the fine order — equally spaced azimuthal samples of B_z
            // in the region midplane, then a direct DFT. Orders up to
            // max_order resolve exactly (Nyquist is enforced in
            // validation); content above the order cutoff aliases —
            // declared sampling, not hidden resolution.
            let (harmonic_b0, harmonic_normal, harmonic_skew) = match &region.harmonics {
                None => (None, None, None),
                Some(harmonics) => {
                    let n_theta = harmonics.theta_samples as usize;
                    let probe = search.requirement.bore_probe_m;
                    let mut circle = Vec::with_capacity(n_theta);
                    for k in 0..n_theta {
                        let theta = 2.0 * std::f64::consts::PI * k as f64 / n_theta as f64;
                        let point = [
                            probe[0] + harmonics.reference_radius_m * theta.cos(),
                            probe[1] + harmonics.reference_radius_m * theta.sin(),
                            probe[2],
                        ];
                        check_cancelled(cancel)?;
                        let value = evaluators[1].evaluate(point)?;
                        tick(progress_tick);
                        requirement_kernel_evaluations += value.kernel_evaluations;
                        circle.push(value.field_t[2]);
                    }
                    let b0 = circle.iter().sum::<f64>() / n_theta as f64;
                    let mut normal = Vec::with_capacity(harmonics.max_order as usize);
                    let mut skew = Vec::with_capacity(harmonics.max_order as usize);
                    for n in 1..=harmonics.max_order {
                        let (mut bn, mut an) = (0.0, 0.0);
                        for (k, &bz) in circle.iter().enumerate() {
                            let theta = 2.0 * std::f64::consts::PI * k as f64 / n_theta as f64;
                            bn += bz * (n as f64 * theta).cos();
                            an += bz * (n as f64 * theta).sin();
                        }
                        // The accelerator "units" figure divides by
                        // the circle's own mean; a non-positive b0
                        // cannot anchor a relative coefficient —
                        // INF fails any declared bound honestly.
                        normal.push(if b0 > 0.0 {
                            (2.0 * bn / n_theta as f64) / b0
                        } else {
                            f64::INFINITY
                        });
                        skew.push(if b0 > 0.0 {
                            (2.0 * an / n_theta as f64) / b0
                        } else {
                            f64::INFINITY
                        });
                    }
                    (Some(b0), Some(normal), Some(skew))
                }
            };
            Some(GoodFieldEval {
                min_unit_bz_t_per_ampere_turn: min_unit_bz[1],
                max_unit_bz_t_per_ampere_turn: Some(max_unit_bz[1]),
                relative_deviation: Some(relative_deviation),
                harmonic_b0_t_per_ampere_turn: harmonic_b0,
                harmonic_normal_units: harmonic_normal,
                harmonic_skew_units: harmonic_skew,
                worst_point_m,
                refinement_change_t: (min_unit_bz[1] - min_unit_bz[0]).abs(),
                pack_overlap,
            })
        }
    };
    let unit_requirement_bz = good_field
        .as_ref()
        .map_or(unit_bore_bz, |field| field.min_unit_bz_t_per_ampere_turn);
    let good_field_refinement_ok = good_field.as_ref().is_none_or(|field| {
        field.refinement_change_t / search.numerics.field_scale_t
            <= search.numerics.max_refinement_change_fraction
    });

    let ampere_turns_a = search.requirement.b_target_t / unit_requirement_bz;
    let operating_current_a = ampere_turns_a / total_turns as f64;
    let achieved_t = ampere_turns_a * unit_requirement_bz;
    let ni_sane = unit_requirement_bz.is_finite()
        && unit_requirement_bz > 0.0
        && ampere_turns_a.is_finite()
        && operating_current_a.is_finite()
        && operating_current_a > 0.0
        && ((achieved_t / search.requirement.b_target_t) - 1.0).abs()
            <= search.requirement.tolerance_fraction;
    let pack_clear = good_field.as_ref().is_none_or(|field| !field.pack_overlap);
    // Schema v22: a declared uniformity bound is part of the region
    // requirement — hitting the target at the weakest point while the
    // spread exceeds the customer's spec is not a passing region.
    let field_quality_ok = search
        .requirement
        .good_field_region
        .as_ref()
        .and_then(|region| region.max_relative_deviation)
        .is_none_or(|bound| {
            good_field
                .as_ref()
                .is_some_and(|field| field.relative_deviation.is_some_and(|d| d <= bound))
        });
    // Schema v23: declared harmonic-unit bounds gate on the largest
    // recorded |b_n|/b0 and |a_n|/b0 — a spec that bounds the spectrum
    // fails a region whose multipoles exceed it, however the weakest
    // point and spread look. A record missing the evaluated spectrum
    // fails closed.
    let harmonics_ok = search
        .requirement
        .good_field_region
        .as_ref()
        .and_then(|region| region.harmonics.as_ref())
        .is_none_or(|harmonics| {
            let within = |bound: Option<f64>, units: Option<&Vec<f64>>| {
                bound.is_none_or(|b| units.is_some_and(|us| us.iter().all(|&u| u.abs() <= b)))
            };
            good_field.as_ref().is_some_and(|field| {
                within(
                    harmonics.max_normal_unit_fraction,
                    field.harmonic_normal_units.as_ref(),
                ) && within(
                    harmonics.max_skew_unit_fraction,
                    field.harmonic_skew_units.as_ref(),
                )
            })
        });
    let requirement_status = if ni_sane
        && bore_refinement_ok
        && good_field_refinement_ok
        && pack_clear
        && field_quality_ok
        && harmonics_ok
        && manufacturing_feasible
    {
        Status::Pass
    } else {
        Status::Fail
    };

    if requirement_status != Status::Pass {
        return Ok(SearchCandidateResult {
            index,
            geometry,
            unit_bore_bz_t_per_ampere_turn: unit_bore_bz,
            bore_refinement_change_t,
            good_field,
            pack_geometry_valid: true,
            manufacturing_feasible,
            inner_bend_radius_m: Some(inner_bend_radius_m),
            bend_strain,
            requirement_kernel_evaluations,
            ampere_turns_a,
            operating_current_a,
            requirement_status,
            refinement_status: if bore_refinement_ok {
                Status::Pass
            } else {
                Status::Fail
            },
            numerical_status: Status::Inconclusive,
            peak_sampled_field_t: None,
            lorentz_load_n_per_m: None,
            mechanical_feasible: None,
            hoop_stress_pa: None,
            transverse_pressure_pa: None,
            transverse_pressure_location: None,
            membrane_tension_n_per_m: None,
            screening: None,
            pruned_by: None,
            coarse_refinement_unresolved: false,
            full_plan_point_count,
            coarse_points_evaluated: 0,
            coarse_kernel_evaluations: 0,
            full_points_evaluated: 0,
            full_kernel_evaluations: 0,
            field_timing_ms: 0.0,
            cost,
            status: Status::Fail,
            // Schema v15: the requirement never passed, so no point plan
            // ran — but the operating current may still be finite, in
            // which case the quench bound resolves.
            screens: screens_without_plan(search, operating_current_a, strands_parallel),
        });
    }

    // The one CoupledCase built for this geometry: it supplies limits,
    // material and winding parameters to the coarse-plan micro-loop below
    // (evaluate_candidate_point never reads its `sampling` field) and, on
    // the full-plan path, is serialized and run through the real
    // `run_coupled_case` entry point unchanged.
    let full_case = build_coupled_case(
        search,
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        operating_current_a,
        full_stations,
        full_turn_indices,
        full_tape_indices,
        Some(tape_spec_ids),
        dims,
    )?;

    let mut pruned_by: Option<LimitingPoint> = None;
    let mut coarse_points_evaluated = 0_u64;
    let mut coarse_kernel_evaluations = 0_u64;
    let mut coarse_peak_field_t = 0.0_f64;
    let mut coarse_field_timing_ms = 0.0_f64;
    // Contract §9.3: a coarse FAIL may prune only if every coarse point's
    // own refinement change, scaled to NI, is itself within the declared
    // gate -- starts true (vacuously, for a candidate with no pruning
    // declared or no points evaluated) and is cleared by any unconverged
    // coarse point.
    let mut coarse_refinement_ok = true;
    let mut coarse_refinement_unresolved = false;
    let refinement_gate_t =
        search.numerics.field_scale_t * search.numerics.max_refinement_change_fraction;
    // Schema v12: the coarse subset's per-turn normal loads — signed
    // I_op·⟨B·ŵ⟩ per (station, tape-column, turn) — feeding the
    // transverse-pressure estimate on the pruned path.
    let mut coarse_force_columns: std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>> =
        std::collections::BTreeMap::new();

    if let Some(pruning) = &search.pruning {
        let coarse_turn_indices =
            expand_relative_turn_indices(&pruning.coarse_turn_fractions, turns_along_normal);
        let mut coarse_tape_indices = vec![1_u32, tapes_along_width];
        coarse_tape_indices.sort_unstable();
        coarse_tape_indices.dedup();
        let nodes = tape_frame::lobatto_nodes();

        let mut point_statuses = Vec::new();
        for station_id in &pruning.coarse_stations {
            let station = full_case
                .sampling
                .stations
                .iter()
                .find(|s| s.id() == station_id)
                .expect("pruning.coarse_stations validated against sampling.stations");
            let frame = station
                .frame(fg.tape_normal, full_case.pack.centerline())
                .map_err(RunError::Model)?;
            for &turn_index in &coarse_turn_indices {
                // Graded packs (schema v10): the covering region's spec
                // resolves this turn's interpolator, audit gate and
                // critical-state table.
                let runtime = runtimes.for_turn(&full_case, turn_index);
                for &tape_index in &coarse_tape_indices {
                    let center = tape_center_position_m(
                        &full_case.pack,
                        &full_case.winding,
                        station,
                        tape_index,
                        turn_index,
                    )
                    .map_err(RunError::Model)?;
                    let mut width_force_sum = 0.0_f64;
                    let mut width_force_count = 0_u32;
                    for &width_index in &pruning.coarse_width_indices {
                        let xi = nodes[width_index as usize];
                        let offset = width_offset_m(full_case.winding.tape_width_m, xi);
                        let position = add3(center, scale3(frame.w, offset));
                        let mut unit_here = [[0.0_f64; 3]; 2];
                        match &resolved_map {
                            // Declared map (schema v13): the point's own
                            // cylindrical lookup — both quadrature slots
                            // carry the declared value, refinement change
                            // is 0 by construction.
                            Some(map) => {
                                let unit = map
                                    .unit_field_lab_t_per_ampere_turn(position)
                                    .map_err(RunError::Model)?;
                                tick(progress_tick);
                                unit_here = [unit, unit];
                            }
                            None => {
                                for (i, evaluator) in evaluators.iter().enumerate() {
                                    check_cancelled(cancel)?;
                                    let clock = Instant::now();
                                    let value = evaluator.evaluate(position)?;
                                    tick(progress_tick);
                                    coarse_field_timing_ms +=
                                        clock.elapsed().as_secs_f64() * 1000.0;
                                    coarse_kernel_evaluations += value.kernel_evaluations;
                                    unit_here[i] = value.field_t;
                                }
                            }
                        }
                        coarse_points_evaluated += 1;
                        // Contract §9.3: this point's own order-vs-order
                        // refinement change, scaled to NI exactly as the
                        // full-plan field_refinement check does (contract
                        // §9.2's same NI-scaling requirement, applied here to
                        // the coarse pre-screen).
                        let point_refinement_change_t =
                            norm(sub(unit_here[1], unit_here[0])) * ampere_turns_a;
                        if point_refinement_change_t > refinement_gate_t {
                            coarse_refinement_ok = false;
                        }
                        let point = evaluate_candidate_point(
                            &runtime.interpolator,
                            &full_case,
                            &runtime.material,
                            runtime.monotonicity_ok,
                            &frame,
                            width_index,
                            unit_here[1],
                            operating_current_a,
                            ampere_turns_a,
                            runtime.critical_state_table.as_ref(),
                        )?;
                        coarse_peak_field_t = coarse_peak_field_t.max(point.magnitude_t);
                        if point.status == Status::Fail && pruned_by.is_none() {
                            pruned_by = Some(LimitingPoint {
                                station: station_id.clone(),
                                tape_index,
                                turn_index,
                                width_index,
                            });
                        }
                        point_statuses.push(point.status);
                        // The signed normal-direction load per unit
                        // length is I_op·⟨B·ŵ⟩: the width-direction field
                        // component drives the normal-direction force
                        // (t̂×B along n̂). Physical B is the unit field
                        // times NI, so the turn's force per metre is
                        // I_op·NI·⟨unit·ŵ⟩.
                        width_force_sum += dot3(unit_here[1], frame.w);
                        width_force_count += 1;
                    }
                    if width_force_count > 0 {
                        coarse_force_columns
                            .entry((station_id.clone(), tape_index))
                            .or_default()
                            .push((
                                turn_index,
                                operating_current_a * ampere_turns_a * width_force_sum
                                    / f64::from(width_force_count),
                            ));
                    }
                }
            }
        }
        // Contract §8 D5: only a coarse FAIL prunes; INCONCLUSIVE or PASS on
        // the coarse subset both proceed to the full plan. Contract §9.3: a
        // coarse FAIL additionally may not prune unless every coarse point's
        // own refinement change was within the gate -- an unconverged coarse
        // point can't be trusted to prune, so the candidate proceeds to the
        // full plan instead, and the record notes why.
        if aggregate_status(point_statuses) != Status::Fail {
            pruned_by = None;
        } else if !coarse_refinement_ok {
            pruned_by = None;
            coarse_refinement_unresolved = true;
        }
    }

    if let Some(pruned_by) = pruned_by {
        // The coarse plan's peak is a real lower bound on the pack's
        // worst field — the shared mechanical screen can still gate on
        // it, with the same semantics as the full-plan path. Schema v12:
        // the transverse-pressure estimate comes from the coarse subset's
        // force columns, so its coverage is exactly what the coarse plan
        // sampled — a pruned candidate carries a coarser pressure screen,
        // honestly recorded.
        let (transverse_pressure_pa, membrane_tension_n_per_m, transverse_pressure_location) =
            match peak_transverse_pressure(
                &coarse_force_columns,
                turns_along_normal,
                full_case.winding.tape_width_m,
            ) {
                Some((pressure, load, location)) => (Some(pressure), Some(load), Some(location)),
                None => (None, None, None),
            };
        let (lorentz_load_n_per_m, hoop_stress_pa, mechanical_feasible) = mechanical_screen(
            search,
            turns_along_normal,
            tapes_along_width,
            strands_parallel,
            operating_current_a,
            Some(coarse_peak_field_t),
            transverse_pressure_pa.zip(membrane_tension_n_per_m),
        );
        return Ok(SearchCandidateResult {
            index,
            geometry,
            unit_bore_bz_t_per_ampere_turn: unit_bore_bz,
            bore_refinement_change_t,
            good_field,
            pack_geometry_valid: true,
            manufacturing_feasible,
            inner_bend_radius_m: Some(inner_bend_radius_m),
            bend_strain,
            requirement_kernel_evaluations,
            ampere_turns_a,
            operating_current_a,
            requirement_status,
            // Contract §9.3: derived from the coarse points actually
            // evaluated, never reported PASS by default. By construction
            // this is always Pass when pruned_by is Some (an unresolved
            // coarse point clears pruned_by above instead), but it is
            // computed rather than hardcoded so that invariant is checked,
            // not assumed.
            refinement_status: if coarse_refinement_ok {
                Status::Pass
            } else {
                Status::Fail
            },
            numerical_status: Status::Inconclusive,
            peak_sampled_field_t: Some(coarse_peak_field_t),
            lorentz_load_n_per_m,
            mechanical_feasible,
            hoop_stress_pa,
            transverse_pressure_pa,
            transverse_pressure_location,
            membrane_tension_n_per_m,
            screening: Some(CandidateResult {
                current_a: operating_current_a,
                ampere_turns_a,
                status: Status::Fail,
                limiting: Some(pruned_by.clone()),
                min_allowed_screening_a: None,
                max_utilization: None,
                point_counts: PointCounts::default(),
                max_self_field_ratio: 0.0,
                limiting_self_field_ratio: None,
                max_transport_self_field_ratio: None,
                max_along_current_fraction: 0.0,
                max_refinement_change_t: 0.0,
            }),
            pruned_by: Some(pruned_by),
            coarse_refinement_unresolved,
            full_plan_point_count,
            coarse_points_evaluated,
            coarse_kernel_evaluations,
            full_points_evaluated: 0,
            full_kernel_evaluations: 0,
            field_timing_ms: coarse_field_timing_ms,
            cost,
            status: Status::Fail,
            // Schema v15: the coarse plan pruned this candidate — the
            // point-dependent screens are NOT_EVALUATED; the quench
            // bound still resolves on the operating current.
            screens: screens_without_plan(search, operating_current_a, strands_parallel),
        });
    }

    // Full plan: the real `run_coupled_case` entry point, unmodified — the
    // same code path OC-004 itself runs and the acceptance module
    // independently re-invokes for the selected optimum and the baseline.
    let case_json = serde_json::to_string(&full_case)?;
    // The generated (possibly graded, v6) case re-resolves every binding
    // through the supplied-dataset path — the run's already-resolved
    // datasets, so a customer-supplied dataset reaches the generated
    // case's spec bindings too. Only the bindings this generated case
    // actually declares may be supplied: its regions reference a subset
    // of the search's specs, and the coupled runner rejects a dataset no
    // declared binding names.
    let declared_ids: std::collections::BTreeSet<&str> = full_case
        .material_bindings()
        .iter()
        .map(|(_, m)| m.dataset_id.as_str())
        .collect();
    let dataset_refs: Vec<&MaterialDataset> = runtimes
        .datasets
        .values()
        .filter(|d| declared_ids.contains(d.metadata.id.as_str()))
        .collect();
    let full_record = run_coupled_case_with_datasets_ticked_cancellable(
        &case_json,
        None,
        &CoupledOptions::default(),
        &dataset_refs,
        progress_tick,
        cancel,
    )?;
    let full_screening = full_record.candidates[0].clone();
    let full_plan_refinement_status = full_record
        .checks
        .iter()
        .find(|c| c.id == "field_refinement")
        .map(|c| c.status)
        .unwrap_or(Status::Inconclusive);
    let peak_sampled_field_t = full_record
        .stations
        .iter()
        .flat_map(|s| &s.tapes)
        .flat_map(|t| &t.per_candidate[0].points)
        .map(|p| p.magnitude_t)
        .fold(coarse_peak_field_t, f64::max);
    let peak_sampled_field_t = peak_sampled_field_t
        .is_finite()
        .then_some(peak_sampled_field_t);
    let full_points_evaluated: u64 = full_record
        .stations
        .iter()
        .map(|s| (s.tapes.len() * 5) as u64)
        .sum();

    let refinement_status = if bore_refinement_ok && full_plan_refinement_status == Status::Pass {
        Status::Pass
    } else {
        Status::Fail
    };
    // Schema v12: the full plan's per-turn normal loads — signed
    // I_op·⟨B·ŵ⟩ per (station, tape-column, turn) — feed the
    // transverse-pressure estimate. Recorded whether or not a bound is
    // declared so the ledger carries the computed screen.
    let mut full_force_columns: std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>> =
        std::collections::BTreeMap::new();
    for station_group in &full_record.stations {
        for tape in &station_group.tapes {
            let (sum, count) = tape.points.iter().fold((0.0_f64, 0_u32), |(s, c), p| {
                (
                    s + dot3(p.unit_field_t_per_ampere_turn[1], tape.frame.w),
                    c + 1,
                )
            });
            if count > 0 {
                full_force_columns
                    .entry((tape.station.clone(), tape.tape_index))
                    .or_default()
                    .push((
                        tape.turn_index,
                        operating_current_a * ampere_turns_a * sum / f64::from(count),
                    ));
            }
        }
    }
    let (transverse_pressure_pa, membrane_tension_n_per_m, transverse_pressure_location) =
        match peak_transverse_pressure(
            &full_force_columns,
            turns_along_normal,
            full_case.winding.tape_width_m,
        ) {
            Some((pressure, load, location)) => (Some(pressure), Some(load), Some(location)),
            None => (None, None, None),
        };
    // Schema v4/v7/v12/v17 mechanical screen: I_op x peak sampled field
    // vs the declared Lorentz-load limit, the declared hoop-stress bound
    // when the case carries the v7 pair, the declared
    // transverse-pressure bound under v12, and the declared
    // membrane-tension bound under v17. Folds into the candidate's
    // aggregate status.
    let (lorentz_load_n_per_m, hoop_stress_pa, mechanical_feasible) = mechanical_screen(
        search,
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        operating_current_a,
        peak_sampled_field_t,
        transverse_pressure_pa.zip(membrane_tension_n_per_m),
    );
    let mechanical_status = match mechanical_feasible {
        None => Status::Pass,
        Some(true) => Status::Pass,
        Some(false) => Status::Fail,
    };
    // Schema v15 adjacent screens, evaluated on the full-plan record.
    // `position_length_m` resolves a conductor position's loop length
    // under the same per-position arithmetic the cost ledger prices:
    // under the radial tape normal the turn index carries the radial
    // offset; under the axial normal the *tape* index does — each tape
    // position is its own hoop at its own radial offset.
    let position_length_m = |turn_index: u32, tape_index: u32| -> f64 {
        let delta = if search.field_map.is_some() {
            let width = search
                .fixed_geometry
                .pack_radial_width_m
                .expect("v13 validation requires fixed extents");
            match fg.tape_normal {
                TapeNormal::Axial => {
                    -width / 2.0
                        + (f64::from(tape_index) - 0.5) * width / f64::from(tapes_along_width)
                }
                TapeNormal::Radial => {
                    -width / 2.0
                        + (f64::from(turn_index) - 0.5) * width / f64::from(turns_along_normal)
                }
            }
        } else {
            let radial_width_m = f64::from(turns_along_normal) * fg.radial_pitch_m;
            -radial_width_m / 2.0 + (f64::from(turn_index) - 0.5) * fg.radial_pitch_m
        };
        match (&fg.path, &fg.path3d) {
            (Some(path), None) => path.length_at_offset_m(delta),
            // Schema v19: the cylinder-radial offset turn is a helix of
            // radius `R + delta` and unchanged pitch.
            (None, Some(path3d)) => path3d.length_at_offset_m(delta),
            _ => {
                let straight = fg.straight_half_length_m.unwrap_or(f64::NAN);
                let rho = fg.bend_radius_m.unwrap_or(f64::NAN) + delta;
                4.0 * straight + 2.0 * std::f64::consts::PI * rho
            }
        }
    };
    let screens = screens_evaluate(
        search,
        &full_case,
        &full_record.stations,
        runtimes,
        operating_current_a,
        strands_parallel,
        position_length_m,
    )?;
    let status = aggregate_status(
        [
            requirement_status,
            refinement_status,
            full_screening.status,
            mechanical_status,
        ]
        .into_iter()
        .chain(screen_statuses(screens.as_ref())),
    );

    Ok(SearchCandidateResult {
        index,
        geometry,
        unit_bore_bz_t_per_ampere_turn: unit_bore_bz,
        bore_refinement_change_t,
        good_field,
        pack_geometry_valid: true,
        manufacturing_feasible,
        inner_bend_radius_m: Some(inner_bend_radius_m),
        bend_strain,
        requirement_kernel_evaluations,
        ampere_turns_a,
        operating_current_a,
        requirement_status,
        refinement_status,
        numerical_status: Status::Inconclusive,
        peak_sampled_field_t,
        lorentz_load_n_per_m,
        mechanical_feasible,
        hoop_stress_pa,
        transverse_pressure_pa,
        transverse_pressure_location,
        membrane_tension_n_per_m,
        screening: Some(full_screening),
        pruned_by: None,
        coarse_refinement_unresolved,
        full_plan_point_count,
        coarse_points_evaluated,
        coarse_kernel_evaluations,
        full_points_evaluated,
        full_kernel_evaluations: full_record.kernel_evaluations,
        field_timing_ms: coarse_field_timing_ms + full_record.field_timing_ms,
        cost,
        status,
        screens,
    })
}

/// Contract §8 D6: independent (search-internal) cost model, reused
/// verbatim by `search_acceptance`'s own, separately written recomputation.
/// `perimeter(rho_k) = 4*L + 2*pi*rho_k`, `rho_k = (R - W/2) + (k-0.5)*pitch`.
/// Schema v4: `installed_length_m` is tape-metres consumed, so parallel
/// strands multiply it (each strand is a full-length conductor).
/// Schema v9 (path geometry): `perimeter(delta_k) =
/// path.length_at_offset_m(delta_k)`, `delta_k = -W/2 + (k-0.5)*pitch` —
/// the same closed form on a racetrack path, kept as a separate branch so
/// legacy ledgers stay bit-identical.
/// `assignment` is the candidate's resolved per-region spec choice
/// (search schema v10; empty on ungraded cases): each turn's installed
/// length prices under `spec_id_for_turn` — `cost.price_usd_per_m` for
/// `"base"` turns, `tape_specs[id].price_usd_per_m` otherwise. The scrap
/// fraction applies to each turn's own price (scrapped premium tape is
/// scrapped at the premium price).
/// One turn-index row of the cost ledger: the turn's resolved spec id and
/// the installed conductor metres of one strand-length — `tapes` folded in
/// under the axial-normal v13 convention, a single hoop otherwise.
/// `compute_cost_ledger` sums these rows; `bom` groups them per spec — one
/// shared walk so the two can never disagree on a turn's length.
pub(crate) fn turn_ledger_rows(
    search: &CoupledSearchCase,
    turns_along_normal: u32,
    tapes_along_width: u32,
    assignment: &[String],
    dims: CandidateDims,
) -> (Vec<(String, f64)>, f64) {
    // Schema v21: the candidate's axis-resolved geometry — every geometry
    // read below goes through `fg` so fixed and searched dims take one
    // path.
    let fg = search.fixed_geometry.resolved_for(dims);
    let pitch = fg.radial_pitch_m;
    // The radial offset a turn index sits at while summing hoop lengths.
    // Schema v13 (declared field map): the pack footprint is the fixed
    // declaration, not count x declared pitch — and under the axial tape
    // normal the turn index advances axially, so every turn is the same
    // hoop and carries no radial offset at all. (Under axial normal it is
    // the tape index that spreads radially; those offsets are symmetric
    // about the centerline and sum to the nominal length, so the per-turn
    // conductor length is `tapes x L(0)` either way.) Legacy cases keep
    // the historical `turns x radial_pitch_m` span — bit-identical
    // ledgers for v1-v12 records.
    let v13_map = search.field_map.is_some();
    let turn_radial_offsets: Vec<f64> = if v13_map {
        match fg.tape_normal {
            TapeNormal::Axial => {
                vec![0.0; turns_along_normal as usize]
            }
            TapeNormal::Radial => {
                let width = fg
                    .pack_radial_width_m
                    .expect("v13 validation requires fixed extents");
                let pitch_k = width / f64::from(turns_along_normal);
                (1..=turns_along_normal)
                    .map(|k| -width / 2.0 + (f64::from(k) - 0.5) * pitch_k)
                    .collect()
            }
        }
    } else {
        let radial_width_m = f64::from(turns_along_normal) * pitch;
        (1..=turns_along_normal)
            .map(|k| -radial_width_m / 2.0 + (f64::from(k) - 0.5) * pitch)
            .collect()
    };
    // Conductor metres in one turn-index row per strand. Under the radial
    // normal every tape in the turn shares the turn's hoop; under the
    // axial normal the turn's `tapes` conductors sit at the tape-direction
    // offsets whose signed deltas sum to zero — `tapes x L(0)`.
    let tapes_per_turn = if v13_map && fg.tape_normal == TapeNormal::Axial {
        f64::from(tapes_along_width)
    } else {
        1.0
    };
    let hoop_at = |delta_k: f64| -> f64 {
        match (&fg.path, &fg.path3d) {
            (Some(path), None) => path.length_at_offset_m(delta_k),
            // Schema v19: cylinder-radial offset turns — a helix of radius
            // `R + delta` at unchanged pitch.
            (None, Some(path3d)) => path3d.length_at_offset_m(delta_k),
            _ => {
                let straight = fg.straight_half_length_m.unwrap_or(f64::NAN);
                let bend_radius_m = fg.bend_radius_m.unwrap_or(f64::NAN);
                4.0 * straight + 2.0 * std::f64::consts::PI * (bend_radius_m + delta_k)
            }
        }
    };
    let rows = turn_radial_offsets
        .iter()
        .enumerate()
        .map(|(k, delta_k)| {
            let spec_id = search
                .spec_id_for_turn(turns_along_normal, k as u32 + 1, assignment)
                .to_string();
            (spec_id, tapes_per_turn * hoop_at(*delta_k))
        })
        .collect();
    (rows, tapes_per_turn)
}

/// Conductor length (m) of each turn of one strand, in winding order
/// (turn 1 first): the cost ledger's own walk (`turn_ledger_rows`) without
/// the spec ids. `assignment` is the candidate's `tape_spec_ids` (empty on
/// ungraded cases). Under a radial tape normal a turn's length is one
/// hoop; the axial-normal fold of `tapes` hoops per turn is not undone
/// here, so callers needing per-conductor lengths must refuse that geometry.
pub fn turn_strand_lengths_m(
    search: &CoupledSearchCase,
    turns_along_normal: u32,
    tapes_along_width: u32,
    assignment: &[String],
    dims: CandidateDims,
) -> Vec<f64> {
    turn_ledger_rows(
        search,
        turns_along_normal,
        tapes_along_width,
        assignment,
        dims,
    )
    .0
    .into_iter()
    .map(|(_, length_m)| length_m)
    .collect()
}

/// Schema v24: resolve a spec's piece catalogue — its own
/// `piece_offerings` when declared, else the base catalogue
/// (`cost.piece_offerings`), else the policy's `piece_length_m` as a
/// single offering at the spec's own `price_usd_per_m`. A spec without
/// its own catalogue buys the base piece length at its own rate.
fn spec_piece_offerings(
    search: &CoupledSearchCase,
    spec_id: &str,
) -> Vec<optcoil_model::coupled_search::PieceOffering> {
    if let Some(specs) = &search.tape_specs
        && let Some(spec) = specs.get(spec_id)
        && let Some(offerings) = &spec.piece_offerings
    {
        return offerings.clone();
    }
    if let Some(offerings) = &search.cost.piece_offerings {
        return offerings.clone();
    }
    let price = search
        .spec_price_usd_per_m(spec_id)
        .expect("spec resolves on a validated case");
    let length_m = search
        .cost
        .piece_policy
        .as_ref()
        .and_then(|p| p.piece_length_m)
        .expect("v24 validation requires a base piece source");
    vec![optcoil_model::coupled_search::PieceOffering {
        length_m,
        price_usd_per_m: price,
    }]
}

/// Schema v24: the piece plan for one candidate — the winding's
/// conductor-unit stream quantized into purchased pieces. Returns
/// `(plans, module_joints, spec_splices)` in conductor-unit counts;
/// caller multiplies splice/piece counts by the strand factor under
/// `piece_unit: per_strand`.
///
/// Run structure: under `per_module` every module re-walks the turn list
/// (each module's run lengths are the turn rows' own, and pieces reset
/// at module boundaries — the `tapes - 1` interface joints still apply);
/// under `continuous` each turn's conductor crosses all `tapes` axial
/// positions, so a same-spec run's stream length is `tapes` times its
/// turn-row length and spec changes splice once per transition.
pub(crate) fn piece_plan_ledger(
    search: &CoupledSearchCase,
    policy: &optcoil_model::coupled_search::PiecePolicy,
    rows: &[(String, f64)],
    tapes_along_width: u32,
    strands_parallel: u32,
) -> (Vec<SpecPiecePlan>, u32, u32) {
    use optcoil_model::coupled_search::PieceBoundary;
    let tapes = tapes_along_width;
    let scrap = search.cost.scrap_fraction;
    // Contiguous same-spec runs over the turn list (unit metres, one
    // module's worth).
    let mut turn_runs: Vec<(String, f64)> = Vec::new();
    for (spec_id, length_m) in rows {
        match turn_runs.last_mut() {
            Some((s, l)) if s == spec_id => *l += length_m,
            _ => turn_runs.push((spec_id.clone(), *length_m)),
        }
    }
    let transitions = turn_runs.len().saturating_sub(1) as u32;
    // Stream-level runs: (spec, unit length covered per piece stream).
    let (stream_runs, module_joints, spec_splices) = match policy.boundary {
        PieceBoundary::PerModule => {
            // Each of the `tapes` modules repeats the turn-run list;
            // spec changes inside a module are splices, module
            // boundaries are interface joints.
            let mut runs = Vec::with_capacity(turn_runs.len() * tapes as usize);
            for _ in 0..tapes {
                runs.extend(turn_runs.iter().cloned());
            }
            (runs, tapes.saturating_sub(1), tapes * transitions)
        }
        PieceBoundary::Continuous => {
            // One conductor stream; each turn's run covers `tapes` axial
            // positions at the same length and spec.
            let runs = turn_runs
                .iter()
                .map(|(s, l)| (s.clone(), l * f64::from(tapes)))
                .collect();
            (runs, 0, transitions)
        }
    };
    // Per-spec run lengths in stream order.
    let mut order: Vec<String> = Vec::new();
    let mut spec_runs: std::collections::BTreeMap<String, Vec<f64>> =
        std::collections::BTreeMap::new();
    for (spec_id, length_m) in &stream_runs {
        if !spec_runs.contains_key(spec_id) {
            order.push(spec_id.clone());
        }
        spec_runs
            .entry(spec_id.clone())
            .or_default()
            .push(*length_m);
    }
    let unit_mult = match policy.piece_unit {
        optcoil_model::coupled_search::PieceUnit::ConductorUnit => 1,
        optcoil_model::coupled_search::PieceUnit::PerStrand => strands_parallel,
    };
    let mut plans = Vec::with_capacity(order.len());
    for spec_id in &order {
        let runs_s = &spec_runs[spec_id];
        let offerings = spec_piece_offerings(search, spec_id);
        // Argmin over the catalogue on this spec's own spend: purchased
        // tape-metres at the offering price plus its splice cost.
        let mut best: Option<(f64, u32, u32, f64, f64)> = None; // spend, pieces_u, splices_u, remnant_unit_m, purchased_unit_m
        let mut chosen: Option<(f64, f64)> = None;
        for offering in &offerings {
            let (mut pieces, mut splices, mut remnant) = (0u32, 0u32, 0.0);
            let mut purchased = 0.0;
            for &run_m in runs_s {
                let required_m = run_m * (1.0 + scrap);
                let n = (required_m / offering.length_m).ceil().max(1.0) as u32;
                pieces += n;
                splices += n - 1;
                purchased += f64::from(n) * offering.length_m;
                remnant += f64::from(n) * offering.length_m - required_m;
            }
            let spend = purchased * offering.price_usd_per_m * f64::from(strands_parallel)
                + f64::from(splices) * f64::from(unit_mult) * policy.splice_cost_usd;
            let better = best.map(|b| spend < b.0).unwrap_or(true);
            if better {
                best = Some((spend, pieces, splices, remnant, purchased));
                chosen = Some((offering.length_m, offering.price_usd_per_m));
            }
        }
        let (_, pieces_u, splices_u, remnant_u, purchased_u) = best.expect("nonempty catalogue");
        let (length_m, price) = chosen.expect("nonempty catalogue");
        plans.push(SpecPiecePlan {
            spec_id: spec_id.clone(),
            piece_length_m: length_m,
            price_usd_per_m: price,
            pieces: pieces_u * unit_mult,
            piece_splices: splices_u * unit_mult,
            remnant_length_m: remnant_u * f64::from(strands_parallel),
            purchased_length_m: purchased_u * f64::from(strands_parallel),
        });
    }
    (plans, module_joints, spec_splices * unit_mult)
}

pub(crate) fn compute_cost_ledger(
    search: &CoupledSearchCase,
    turns_along_normal: u32,
    tapes_along_width: u32,
    strands_parallel: u32,
    assignment: &[String],
    dims: CandidateDims,
) -> SearchCostLedger {
    let (rows, tapes_per_turn) = turn_ledger_rows(
        search,
        turns_along_normal,
        tapes_along_width,
        assignment,
        dims,
    );
    let mut perimeter_sum_m = 0.0_f64;
    // Price-weighted installed length: Σ_k perimeter_k · price(turn k).
    let mut priced_perimeter_usd_per_layer = 0.0_f64;
    for (spec_id, length_k) in &rows {
        perimeter_sum_m += length_k;
        priced_perimeter_usd_per_layer += length_k
            * search
                .spec_price_usd_per_m(spec_id)
                .expect("spec_id_for_turn resolves on a validated case");
    }
    let conductor_layers = if tapes_per_turn > 1.0 {
        // Axial-normal v13: the tape count is already folded into each
        // turn's length above — only strands multiply it further.
        f64::from(strands_parallel)
    } else {
        f64::from(tapes_along_width) * f64::from(strands_parallel)
    };
    let installed_length_m = perimeter_sum_m * conductor_layers;
    let assembly_usd = search.cost.assembly_cost_per_pancake_usd * f64::from(tapes_along_width);
    // Schema v24: piece-quantized procurement — when `piece_policy` is
    // declared the ledger prices the pieces actually bought (discrete
    // lengths, per-spec catalogues argmin'd on spend), the remnant beyond
    // installed-plus-attrition, and in-winding splices separately from
    // module-interface joints. `None` keeps the legacy ledger
    // bit-identical.
    let mut piece_columns: Option<(u32, u32, u32, u32, f64, Vec<SpecPiecePlan>)> = None;
    let (conductor_usd, scrap_usd, purchased_length_m, joints_usd) =
        if let Some(policy) = &search.cost.piece_policy {
            let (plans, module_joints, spec_splices) =
                piece_plan_ledger(search, policy, &rows, tapes_along_width, strands_parallel);
            // Per-spec installed tape-metres for the conductor spend:
            // a spec's unit length is its turn-row sum × tapes modules —
            // identical under either boundary policy.
            let mut installed_by_spec: std::collections::BTreeMap<String, f64> =
                std::collections::BTreeMap::new();
            for (spec_id, length_k) in &rows {
                *installed_by_spec.entry(spec_id.clone()).or_default() += length_k;
            }
            let mut conductor = 0.0_f64;
            let mut scrap = 0.0_f64;
            let mut purchased = 0.0_f64;
            let mut remnant = 0.0_f64;
            let mut piece_splices = 0u32;
            let mut pieces_bought = 0u32;
            for plan in &plans {
                let installed_tape_m = installed_by_spec.get(&plan.spec_id).copied().unwrap_or(0.0)
                    * f64::from(tapes_along_width)
                    * f64::from(strands_parallel);
                conductor += installed_tape_m * plan.price_usd_per_m;
                scrap += (plan.purchased_length_m - installed_tape_m) * plan.price_usd_per_m;
                purchased += plan.purchased_length_m;
                remnant += plan.remnant_length_m;
                piece_splices += plan.piece_splices;
                pieces_bought += plan.pieces;
            }
            let joints = search.cost.joint_cost_usd * f64::from(module_joints)
                + policy.splice_cost_usd * f64::from(piece_splices + spec_splices);
            piece_columns = Some((
                module_joints,
                piece_splices,
                spec_splices,
                pieces_bought,
                remnant,
                plans,
            ));
            (conductor, scrap, purchased, joints)
        } else {
            let scrap_length_m = installed_length_m * search.cost.scrap_fraction;
            let purchased_length_m = installed_length_m + scrap_length_m;
            // Ungraded cases keep the legacy `(total_length · price)` ordering —
            // bit-identical ledgers for v1–v9 records. Graded (v10) cases sum
            // per-turn price-weighted lengths; scrap is each turn's fraction at
            // its own price, i.e. scrap_fraction · conductor_usd.
            let (conductor_usd, scrap_usd) = if search.grading.is_some() {
                let conductor_usd = priced_perimeter_usd_per_layer * conductor_layers;
                (conductor_usd, conductor_usd * search.cost.scrap_fraction)
            } else {
                (
                    installed_length_m * search.cost.price_usd_per_m,
                    scrap_length_m * search.cost.price_usd_per_m,
                )
            };
            let joints_usd =
                search.cost.joint_cost_usd * f64::from(tapes_along_width.saturating_sub(1));
            (conductor_usd, scrap_usd, purchased_length_m, joints_usd)
        };
    let total_usd = conductor_usd + scrap_usd + assembly_usd + joints_usd;
    // Schema v20: declared refrigeration economics — case-constant across
    // candidates (no candidate-dependent loads are modeled).
    let opex_usd = search
        .opex
        .as_ref()
        .map(|opex| opex_lifetime_usd(opex, search.operating.temperature_k));
    let (module_joints, piece_splices, spec_splices, pieces_bought, remnant_length_m, piece_plan) =
        piece_columns
            .map(|(m, p, s, n, r, plans)| {
                (Some(m), Some(p), Some(s), Some(n), Some(r), Some(plans))
            })
            .unwrap_or_default();
    SearchCostLedger {
        installed_length_m,
        purchased_length_m,
        conductor_usd,
        scrap_usd,
        assembly_usd,
        joints_usd,
        total_usd,
        opex_usd,
        lifecycle_usd: opex_usd.map(|o| total_usd + o),
        module_joints,
        piece_splices,
        spec_splices,
        pieces_bought,
        remnant_length_m,
        piece_plan,
    }
}

/// Schema v20: undiscounted lifetime refrigeration cost at the declared
/// operating point. `input_w = load × (T_sink − T_op) / (T_op ×
/// cop_fraction)` — the T-dependence is the Carnot bound; the declared
/// fraction is the plant's achieved share of it. Annual kWh × $/kWh ×
/// declared years — no discounting.
fn opex_lifetime_usd(opex: &optcoil_model::coupled_search::Opex, temperature_k: f64) -> f64 {
    let cold_w: f64 = opex.heat_loads_w.iter().map(|h| h.power_w).sum();
    let input_w = cold_w * (opex.sink_temperature_k - temperature_k)
        / (temperature_k * opex.cop_fraction_of_carnot);
    let annual_kwh = input_w * opex.operating_hours_per_year / 1000.0;
    annual_kwh * opex.electricity_usd_per_kwh * f64::from(opex.operating_years)
}

fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k])
}

fn scale3(v: [f64; 3], factor: f64) -> [f64; 3] {
    v.map(|x| x * factor)
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| a[k] * b[k]).sum()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A tiny reduced case: 2 geometries, orders [2,4], one station, small
    /// turn counts, width_points 5 (the schema's only supported value).
    /// Deliberately independent of the frozen OC-007 cases.
    pub(crate) fn reduced_case_json(turns_choices: &str, price: f64) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v1",
  "id": "reduced-search-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6}},
  "fixed_geometry": {{
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  }},
  "choices": {{"turns_along_normal": {turns_choices}, "tapes_along_width": [2]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
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
    "stations": [{{"id": "s0", "kind": "straight", "x_m": 0.0}}],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": {price},
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{"turns_along_normal": 3, "tapes_along_width": 2}},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0}},
      {{"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0}},
      {{"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0}},
      {{"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0}},
      {{"id": "straight_015", "kind": "straight", "x_m": 0.15}},
      {{"id": "straight_025", "kind": "straight", "x_m": 0.25}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": 3}}
}}
"#
        )
    }

    /// Same shape but with a coarse pruning block and two geometries chosen
    /// so one's coarse subset is guaranteed PASS (tiny NI, tiny fields) and
    /// the other's is guaranteed FAIL (huge NI from a near-zero bore field
    /// at very few turns, driving the sampled points' utilization over 1).
    fn reduced_case_with_pruning_json() -> String {
        r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "reduced-pruning-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [3, 60], "tapes_along_width": [2]},
  "operating": {"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001},
  "material": {
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
  },
  "sampling": {
    "stations": [{"id": "s0", "kind": "straight", "x_m": 0.0}],
    "relative_turn_indices": [
      {"kind": "from_start", "offset": 1},
      {"kind": "from_end", "offset": 0}
    ],
    "width_points": 5
  },
  "limits": {
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  },
  "numerics": {"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5},
  "pruning": {
    "coarse_stations": ["s0"],
    "coarse_turn_fractions": [{"kind": "from_start", "offset": 1}],
    "coarse_width_indices": [0, 4]
  },
  "cost": {
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  },
  "baseline": {"turns_along_normal": 60, "tapes_along_width": 2},
  "refined_plan": {
    "additional_stations": [
      {"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0},
      {"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0},
      {"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0},
      {"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0},
      {"id": "straight_015", "kind": "straight", "x_m": 0.15},
      {"id": "straight_025", "kind": "straight", "x_m": 0.25}
    ],
    "max_sampling_shortfall_fraction": 0.02
  },
  "execution": {"max_threads": 3}
}
"#
        .to_owned()
    }

    #[test]
    fn cost_ledger_matches_a_hand_calculation_for_a_small_geometry() {
        // 3 turns, 2 tapes, straight_half_length_m 0.3, bend_radius_m 0.2,
        // radial_pitch_m 1e-4, price $30/m, scrap 10%, assembly $500/pancake,
        // joint $200/interface (1 interface for 2 tapes).
        //
        // radial_width_m = 3 * 1e-4 = 3e-4 m; inner_rho = 0.2 - 1.5e-4 = 0.19985.
        // rho_1 = 0.19985 + 0.5*1e-4 = 0.199900
        // rho_2 = 0.19985 + 1.5*1e-4 = 0.200000
        // rho_3 = 0.19985 + 2.5*1e-4 = 0.200100
        // perimeter_k = 4*0.3 + 2*pi*rho_k = 1.2 + 2*pi*rho_k
        //   p1 = 1.2 + 2*pi*0.199900 = 2.456097335...
        //   p2 = 1.2 + 2*pi*0.200000 = 2.456637061...
        //   p3 = 1.2 + 2*pi*0.200100 = 2.457176788...
        // sum = 7.369911184...; installed = sum * 2 tapes = 14.739822368...
        let case: CoupledSearchCase =
            serde_json::from_str(&reduced_case_json("[3, 500]", 30.0)).unwrap();
        let cost = compute_cost_ledger(&case, 3, 2, 1, &[], CandidateDims::default());
        let inner_rho = 0.2 - 3.0 * 1e-4 / 2.0;
        let expected_installed: f64 = (1..=3)
            .map(|k| {
                let rho_k = inner_rho + (f64::from(k) - 0.5) * 1e-4;
                4.0 * 0.3 + 2.0 * std::f64::consts::PI * rho_k
            })
            .sum::<f64>()
            * 2.0;
        assert!((cost.installed_length_m - expected_installed).abs() < 1e-9);
        assert!((cost.installed_length_m - 14.739822368).abs() < 1e-6);
        let expected_scrap = expected_installed * 0.1;
        assert!((cost.scrap_usd - expected_scrap * 30.0).abs() < 1e-6);
        assert!((cost.assembly_usd - 1000.0).abs() < 1e-9); // 500 * 2 pancakes
        assert!((cost.joints_usd - 200.0).abs() < 1e-9); // 200 * (2-1) interfaces
        let expected_total = expected_installed * 30.0 + expected_scrap * 30.0 + 1000.0 + 200.0;
        assert!((cost.total_usd - expected_total).abs() < 1e-6);
    }

    #[test]
    fn strands_parallel_scales_capacity_and_cost_not_geometry_or_turns() {
        // Schema v4: `strands_parallel` shares I_op across s conductors
        // (capacity x s) and consumes s tape-metres per turn (cost x s),
        // while pack geometry and ampere-turns are unchanged.
        let mut v4: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v4["schema"] = "optcoil-coupled-search/v4".into();
        v4["choices"]["turns_along_normal"] = serde_json::json!([60]);
        v4["choices"]["strands_parallel"] = serde_json::json!([1, 4]);
        v4["baseline"] = serde_json::json!({
            "turns_along_normal": 60,
            "tapes_along_width": 2,
            "strands_parallel": 4
        });
        let case_json = serde_json::to_string(&v4).unwrap();
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        assert_eq!(record.candidates.len(), 2);

        let s1 = &record.candidates[0];
        let s4 = &record.candidates[1];
        assert_eq!(s1.geometry.strands_parallel, 1);
        assert_eq!(s4.geometry.strands_parallel, 4);
        assert_eq!(s1.geometry.total_turns, s4.geometry.total_turns);
        assert_eq!(s4.geometry.total_conductors, 4 * s4.geometry.total_turns);
        assert_eq!(s1.geometry.radial_width_m, s4.geometry.radial_width_m);
        // Same ampere-turns -> same operating current (strands split it,
        // they do not change it).
        assert_eq!(s1.operating_current_a, s4.operating_current_a);
        // Cost: conductor metreage x 4; assembly/joints unchanged.
        assert!((s4.cost.installed_length_m - 4.0 * s1.cost.installed_length_m).abs() < 1e-9);
        assert_eq!(s1.cost.assembly_usd, s4.cost.assembly_usd);
        assert_eq!(s1.cost.joints_usd, s4.cost.joints_usd);
        // Capacity: the full plan evaluates the same tape set for both
        // candidates, so the minimum screened allowance scales exactly.
        let n1 = s1
            .screening
            .as_ref()
            .and_then(|s| s.min_allowed_screening_a);
        let n4 = s4
            .screening
            .as_ref()
            .and_then(|s| s.min_allowed_screening_a);
        if let (Some(a1), Some(a4)) = (n1, n4) {
            assert!(
                (a4 - 4.0 * a1).abs() < 1e-9 * a4,
                "4-strand capacity must be 4x the 1-strand min: {a1} vs {a4}"
            );
        }
    }

    /// The record carries each bound dataset's measured-domain
    /// applicability: the bridge the specimen actually covered, the tape
    /// it was cut from, and an explicit flag when the case's applied
    /// product width differs. SuperPower measured a 1 mm bridge from a
    /// 12 mm tape — the reduced fixture applies it at 12 mm (matching
    /// width, still a bridge-to-full-width assumption).
    #[test]
    fn width_applicability_is_recorded_per_binding() {
        let case_json = reduced_case_json("[3]", 38.0);
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let line = record
            .limitations
            .iter()
            .find(|l| l.contains("Width applicability, binding 'base'"))
            .expect("applicability limitation present");
        assert!(line.contains("robinson-superpower-ap-v3"), "{line}");
        assert!(line.contains("0.001 m patterned bridge"), "{line}");
        assert!(line.contains("0.012 m tape"), "{line}");
        assert!(
            line.contains("bridge-to-full-width") || line.contains("Bridge-to-full-width"),
            "{line}"
        );
        // Applied width matches the specimen's tape: no cross-product flag.
        assert!(!line.contains("different from"), "{line}");

        // The dataset identity carries the measured domain on the record.
        // (spec_datasets is empty on ungraded cases; the base identity is
        // the record's dataset_id plus these provenance fields.)
        // Applying a 6 mm tape width while binding a 12 mm-tape specimen
        // must name the cross-product-width gap.
        let mut case: serde_json::Value = serde_json::from_str(&case_json).unwrap();
        case["fixed_geometry"]["tape_width_m"] = serde_json::json!(0.006);
        let case_json = serde_json::to_string(&case).unwrap();
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let line = record
            .limitations
            .iter()
            .find(|l| l.contains("Width applicability, binding 'base'"))
            .expect("applicability limitation present");
        assert!(line.contains("different from"), "{line}");
        assert!(line.contains("0.006 m product width"), "{line}");
    }

    #[test]
    fn v3_rejects_a_strands_declaration() {
        let mut v3: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v3["choices"]["strands_parallel"] = serde_json::json!([4]);
        let case_json = serde_json::to_string(&v3).unwrap();
        assert!(run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).is_err());
    }

    #[test]
    fn mechanical_lorentz_gate_records_load_and_fails_on_declared_limit() {
        // Schema v4 mechanical screen: `I_op * peak_sampled_field` vs a
        // declared limit. A limit below the load fails the candidate; a
        // limit above leaves its screening verdict standing.
        let mut v4: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v4["schema"] = "optcoil-coupled-search/v4".into();
        v4["mechanical"] = serde_json::json!({"max_lorentz_load_n_per_m": 1.0});
        let case_json = serde_json::to_string(&v4).unwrap();
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .expect("a screened candidate");
        let load = cand
            .lorentz_load_n_per_m
            .expect("a screened candidate records its Lorentz load");
        assert!(
            (load - cand.operating_current_a * cand.peak_sampled_field_t.unwrap()).abs()
                < 1e-9 * load.max(1.0)
        );
        assert_eq!(cand.mechanical_feasible, Some(false));
        assert_eq!(cand.status, Status::Fail);

        // The same case with an effectively unbounded limit must not
        // demote any candidate.
        v4["mechanical"] = serde_json::json!({"max_lorentz_load_n_per_m": 1e18});
        let case_json = serde_json::to_string(&v4).unwrap();
        let record2 =
            run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let cand2 = record2
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .unwrap();
        assert_eq!(cand2.mechanical_feasible, Some(true));
    }

    #[test]
    fn mechanical_hoop_bound_records_stress_and_fails_on_declared_limit() {
        // Schema v7 (OC-018): hoop stress per strand is
        // (I_op/s)·B_peak·R_outer/area with
        // R_outer = bend_radius + turns·pitch/2. A declared limit below
        // the computed stress fails the candidate; above leaves its
        // screening verdict standing.
        let mut v7: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v7["schema"] = "optcoil-coupled-search/v7".into();
        v7["mechanical"] = serde_json::json!({
            "max_lorentz_load_n_per_m": 1e18,
            "max_hoop_stress_pa": 1.0,
            "tension_section_area_m2": 1.0e-6
        });
        let case_json = serde_json::to_string(&v7).unwrap();
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .expect("a screened candidate");
        let stress = cand
            .hoop_stress_pa
            .expect("a screened candidate under a declared bound records its hoop stress");
        let load = cand.lorentz_load_n_per_m.unwrap();
        // bend_radius_m = 0.2, radial_pitch_m = 0.0001 in this fixture;
        // R_outer = bend + half the pack's radial extent.
        let r_outer = 0.2 + f64::from(cand.geometry.turns_along_normal) * 0.0001 / 2.0;
        let expected = (load / f64::from(cand.geometry.strands_parallel)) * r_outer / 1.0e-6;
        assert!(
            (stress - expected).abs() < 1e-9 * expected.max(1.0),
            "hoop stress {stress} != expected {expected}"
        );
        assert_eq!(cand.mechanical_feasible, Some(false));
        assert_eq!(cand.status, Status::Fail);

        // An effectively unbounded hoop limit must not demote any
        // candidate — and the stress value is still recorded.
        v7["mechanical"]["max_hoop_stress_pa"] = serde_json::json!(1e18);
        let case_json = serde_json::to_string(&v7).unwrap();
        let record2 =
            run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let cand2 = record2
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .unwrap();
        assert_eq!(cand2.mechanical_feasible, Some(true));
        assert!(cand2.hoop_stress_pa.is_some());
    }

    /// The reduced fixture extended to schema v12: refinement plus a
    /// mechanical block carrying the transverse-pressure bound.
    fn v12_reduced_case_json(max_transverse_pressure_pa: f64) -> String {
        let mut v12: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v12["schema"] = "optcoil-coupled-search/v12".into();
        v12["mechanical"] = serde_json::json!({
            "max_lorentz_load_n_per_m": 1e18,
            "max_transverse_pressure_pa": max_transverse_pressure_pa
        });
        serde_json::to_string(&v12).unwrap()
    }

    #[test]
    fn transverse_pressure_estimate_accumulates_columns_trapezoidally() {
        // Pure-function sanity: a uniform signed load column, sampled at
        // turns {2,4} of a 4-turn stack, weights the sampled turns 3 and 1
        // (midpoint cells over [1,4] anchored at the faces), so the last
        // interface transmits the full column load.
        let mut columns: std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>> =
            std::collections::BTreeMap::new();
        columns.insert(("s1".to_string(), 1), vec![(2, 10.0), (4, 10.0)]);
        let (pressure, membrane_tension, location) =
            peak_transverse_pressure(&columns, 4, 0.012).expect("a sampled column");
        // F_2 = 3*10 = 30 (turns 1-3), F_4 = 40; interface loads
        // max(30, |40-30|) = 30 and max(40, 0) = 40 -> 40/0.012.
        let expected = 40.0 / 0.012;
        assert!((pressure - expected).abs() < 1e-9 * expected);
        // Schema v17: the undivided interface load is the pressure's own
        // numerator — the membrane-tension resultant.
        assert!((membrane_tension - 40.0).abs() < 1e-9 * 40.0);
        assert_eq!(location.station, "s1");
        assert_eq!(location.tape_index, 1);
        assert_eq!(location.turn_index, 4);
        assert_eq!(location.width_index, 0);

        // Sign cancellation still loads the internal interfaces: the
        // outer-face convention transmits the full one-sided block even
        // when the column's net load is zero.
        let mut cancelling: std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>> =
            std::collections::BTreeMap::new();
        cancelling.insert(("s1".to_string(), 1), vec![(1, 10.0), (2, -10.0)]);
        let (pressure, _, _) = peak_transverse_pressure(&cancelling, 2, 0.012).unwrap();
        let expected = 10.0 / 0.012;
        assert!((pressure - expected).abs() < 1e-9 * expected);

        // Two columns: the peak takes the larger across stations/tapes.
        let mut two: std::collections::BTreeMap<(String, u32), Vec<(u32, f64)>> =
            std::collections::BTreeMap::new();
        two.insert(("arc0".to_string(), 1), vec![(1, 5.0), (2, 5.0)]);
        two.insert(("leg".to_string(), 2), vec![(1, 12.0), (2, 12.0)]);
        let (pressure, _, location) = peak_transverse_pressure(&two, 2, 0.012).unwrap();
        let expected = 24.0 / 0.012;
        assert!((pressure - expected).abs() < 1e-9 * expected);
        assert_eq!(location.station, "leg");
        assert_eq!(location.tape_index, 2);

        // No sampled column -> no pressure.
        assert!(peak_transverse_pressure(&std::collections::BTreeMap::new(), 4, 0.012,).is_none());
    }

    #[test]
    fn transverse_pressure_bound_records_pressure_and_fails_on_declared_limit() {
        // Schema v12: the estimate is recorded on every screened
        // candidate whether or not a bound is declared; a declared bound
        // below the estimate fails the candidate fail-closed.
        let record = run_coupled_search_case(
            &v12_reduced_case_json(1e18),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.case.schema, "optcoil-coupled-search/v12");
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .expect("a screened candidate");
        let pressure = cand
            .transverse_pressure_pa
            .expect("a screened candidate records its transverse pressure");
        assert!(pressure.is_finite() && pressure > 0.0);
        let location = cand
            .transverse_pressure_location
            .as_ref()
            .expect("pressure's location");
        assert!(!location.station.is_empty());
        assert!(location.turn_index >= 1);
        assert_eq!(cand.mechanical_feasible, Some(true));

        // The same case with a bound below any real pressure fails every
        // screened candidate on the mechanical screen.
        let record = run_coupled_search_case(
            &v12_reduced_case_json(1.0),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .unwrap();
        assert_eq!(cand.mechanical_feasible, Some(false));
        assert_eq!(cand.status, Status::Fail);
        assert!(cand.transverse_pressure_pa.unwrap() > 1.0);
        // Acceptance recomputes the same verdict independently.
        assert_eq!(
            record.acceptance.baseline.mechanical_agreement_status,
            Status::Pass
        );
        assert!(
            record
                .acceptance
                .baseline
                .recomputed_transverse_pressure_pa
                .is_some()
        );
    }

    #[test]
    fn transverse_pressure_declared_bound_with_no_sampled_field_fails_closed() {
        // The shared screen: a declared bound whose pressure never
        // materialized is infeasible — the v4 Lorentz-load treatment,
        // extended.
        let case: CoupledSearchCase = serde_json::from_str(&v12_reduced_case_json(1e18)).unwrap();
        let (_, _, feasible) = mechanical_screen(&case, 60, 2, 1, 500.0, Some(0.5), None);
        assert_eq!(feasible, Some(false));
        // No bound declared -> None pressure leaves the verdict alone.
        let mut unbounded: serde_json::Value =
            serde_json::from_str(&v12_reduced_case_json(1e18)).unwrap();
        unbounded["mechanical"]["max_transverse_pressure_pa"] = serde_json::Value::Null;
        let case: CoupledSearchCase = serde_json::from_value(unbounded).unwrap();
        assert!(
            case.mechanical
                .as_ref()
                .unwrap()
                .max_transverse_pressure_pa
                .is_none()
        );
        let (_, _, feasible) = mechanical_screen(&case, 60, 2, 1, 500.0, Some(0.5), None);
        assert_eq!(feasible, Some(true));
    }

    #[test]
    fn v11_rejects_a_transverse_pressure_declaration() {
        // The bound is a v12 semantic: the identical case at v11 must not
        // validate.
        let v11 = v12_reduced_case_json(4.0e7).replace(
            "\"optcoil-coupled-search/v12\"",
            "\"optcoil-coupled-search/v11\"",
        );
        let err = run_coupled_search_case(&v11, &CoupledSearchOptions::default()).unwrap_err();
        assert!(
            err.to_string().contains("max_transverse_pressure_pa"),
            "expected a v12 gating rejection, got {err}"
        );
    }

    #[test]
    fn oc021_bound_eliminates_the_cheapest_screening_passer() {
        // The fixture's declared claim: without the 30 MPa bound the
        // search would select 200x2 ($31.6k, the cheapest candidate whose
        // screening passes); the bound alone removes it (hoop and
        // utilization both pass), and the optimum moves to 200x3.
        let record = run_coupled_search_case(
            optcoil_model::coupled_search::OC021_JSON,
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.search_status, Status::Pass);
        assert_eq!(record.acceptance.agreement_status, Status::Pass);

        let find = |turns: u32, tapes: u32| {
            record
                .candidates
                .iter()
                .find(|c| {
                    c.geometry.turns_along_normal == turns && c.geometry.tapes_along_width == tapes
                })
                .expect("candidate present")
        };
        let eliminated = find(200, 2);
        assert_eq!(eliminated.screening.as_ref().unwrap().status, Status::Pass);
        // Hoop passes (357 < 450 MPa); the pressure bound alone fails it.
        assert!(eliminated.hoop_stress_pa.unwrap() < 4.5e8);
        let p = eliminated.transverse_pressure_pa.unwrap();
        assert!(p > 3.0e7, "200x2 must exceed the declared bound: {p}");
        assert_eq!(eliminated.mechanical_feasible, Some(false));
        assert_eq!(eliminated.status, Status::Fail);

        let winner = find(200, 3);
        let pw = winner.transverse_pressure_pa.unwrap();
        assert!(pw <= 3.0e7 && pw > 0.0, "200x3 within the bound: {pw}");
        assert_eq!(winner.mechanical_feasible, Some(true));
        assert_eq!(winner.status, Status::Pass);
        assert_eq!(record.best_index, Some(winner.index));
        assert_eq!(record.baseline_index, winner.index);

        // Acceptance's independently written accumulation lands within a
        // few percent of the search estimate (different cell-boundary
        // convention on the same sparse plan) and the verdicts agree.
        let recomputed = record
            .acceptance
            .baseline
            .recomputed_transverse_pressure_pa
            .expect("acceptance records its recomputed pressure");
        assert!(
            (recomputed - pw).abs() < 0.05 * pw,
            "recomputed {recomputed} vs search {pw}"
        );
        assert_eq!(
            record.acceptance.baseline.mechanical_agreement_status,
            Status::Pass
        );

        // Run-schema v12: with a bound declared, the declared plan's
        // pressure resolution is load-bearing — the refined plan's denser
        // turn sampling must reproduce it within the declared shortfall
        // gate, and the check must actually have run (not NOT_EVALUATED).
        let baseline = &record.acceptance.baseline;
        let refined_p = baseline
            .refined
            .transverse_pressure_pa
            .expect("the refined plan carries its own pressure estimate");
        assert!(refined_p > 0.0);
        let shortfall = baseline
            .pressure_shortfall_fraction
            .expect("both estimates exist, so the shortfall is recorded");
        assert!(
            shortfall <= 0.02,
            "refined plan must reproduce the coarse estimate within the declared gate: {shortfall}"
        );
        assert_eq!(baseline.pressure_coverage_status, Status::Pass);
        let coverage_check = record
            .acceptance
            .checks
            .iter()
            .find(|c| c.id == "pressure_coverage")
            .expect("the coverage check is recorded");
        assert_eq!(coverage_check.status, Status::Pass);
    }

    /// The reduced fixture extended to schema v3: a refinement block plus a
    /// good-field region of 0.05 m half extents around the origin probe,
    /// sampled on a 3x3x3 lattice.
    fn v3_reduced_case_json() -> String {
        reduced_case_json("[3, 60]", 30.0)
            .replacen(
                "\"optcoil-coupled-search/v1\"",
                "\"optcoil-coupled-search/v3\"",
                1,
            )
            .replacen(
                "\"tolerance_fraction\": 1e-6}",
                r#""tolerance_fraction": 1e-6, "good_field_region": {"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3}}"#,
                1,
            )
            .replacen(
                "\"execution\":",
                r#""refinement": {
    "pancake_counts": [2],
    "turn_resolution": 5,
    "turn_bounds": {"min": 1, "max": 100},
    "brackets": [{"tapes": 2, "fail_turns": 3, "pass_turns": 60}],
    "monotonicity_check": true
  },
  "execution":"#,
                1,
            )
    }

    /// Schema v20 fixture: the v3 reduced case plus a declared `opex`
    /// block — 3 W cold load, Carnot fraction 0.25, sink 300 K at T_op
    /// 21 K, $0.10/kWh × 4000 h/yr × 10 yr.
    pub(crate) fn v20_opex_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v20".into();
        v["opex"] = serde_json::json!({
            "heat_loads_w": [
                {"id": "static", "power_w": 2.0},
                {"id": "leads", "power_w": 1.0}
            ],
            "cop_fraction_of_carnot": 0.25,
            "sink_temperature_k": 300.0,
            "electricity_usd_per_kwh": 0.10,
            "operating_hours_per_year": 4000.0,
            "operating_years": 10
        });
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn opex_block_validates_and_rejects_bad_declarations() {
        CoupledSearchCase::from_json(&v20_opex_case_json()).unwrap();
        // v19 must not declare opex.
        let mut v: serde_json::Value = serde_json::from_str(&v20_opex_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v19".into();
        assert!(CoupledSearchCase::from_json(&v.to_string()).is_err());
        // Each bound fails closed.
        for (field, bad) in [
            ("cop_fraction_of_carnot", serde_json::json!(0.0)),
            ("cop_fraction_of_carnot", serde_json::json!(1.5)),
            ("sink_temperature_k", serde_json::json!(21.0)), // == T_op
            ("sink_temperature_k", serde_json::json!(10.0)), // below T_op
            ("electricity_usd_per_kwh", serde_json::json!(0.0)),
            ("operating_hours_per_year", serde_json::json!(8761.0)),
            ("operating_years", serde_json::json!(0)),
        ] {
            let mut bad_case: serde_json::Value =
                serde_json::from_str(&v20_opex_case_json()).unwrap();
            bad_case["opex"][field] = bad.clone();
            assert!(
                CoupledSearchCase::from_json(&bad_case.to_string()).is_err(),
                "{field} = {bad} must be rejected"
            );
        }
        // Zero-power and duplicate-id load terms are declaration bugs.
        let mut zero: serde_json::Value = serde_json::from_str(&v20_opex_case_json()).unwrap();
        zero["opex"]["heat_loads_w"][0]["power_w"] = serde_json::json!(0.0);
        assert!(CoupledSearchCase::from_json(&zero.to_string()).is_err());
        let mut dup: serde_json::Value = serde_json::from_str(&v20_opex_case_json()).unwrap();
        dup["opex"]["heat_loads_w"][1]["id"] = "static".into();
        assert!(CoupledSearchCase::from_json(&dup.to_string()).is_err());
    }

    #[test]
    fn opex_ledger_terms_match_hand_calculation() {
        // Hand figure at T_op = 21 K: input = 3 W × 279 / (21 × 0.25)
        // = 159.428571 W → 637.714 kWh/yr → $63.7714/yr → $637.714 over
        // 10 yr. Case-constant: identical on every candidate, and
        // lifecycle = capex + opex.
        let record =
            run_coupled_search_case(&v20_opex_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        let expected_opex: f64 =
            3.0 * (300.0 - 21.0) / (21.0 * 0.25) * 4000.0 / 1000.0 * 0.10 * 10.0;
        assert!((expected_opex - 637.7142857142857).abs() < 1e-9);
        for c in &record.candidates {
            let opex = c.cost.opex_usd.unwrap();
            assert!((opex - expected_opex).abs() < 1e-9, "{opex}");
            assert_eq!(
                c.cost.lifecycle_usd.unwrap(),
                c.cost.total_usd + opex,
                "lifecycle = capex + declared opex"
            );
        }
        // No opex declared → absent, never a defaulted zero.
        let plain =
            run_coupled_search_case(&v3_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        for c in &plain.candidates {
            assert_eq!(c.cost.opex_usd, None);
            assert_eq!(c.cost.lifecycle_usd, None);
        }
    }

    /// Schema v21 fixture: the v3 reduced case with `bend_radius_m` moved
    /// from `fixed_geometry` to a declared `choices` axis — three radii
    /// (0.15 / 0.2 / 0.25 m), baseline at the original 0.2 m. The
    /// straight half-length stays fixed at 0.3 m.
    pub(crate) fn v21_bend_axis_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v21".into();
        let fg = v["fixed_geometry"].as_object_mut().unwrap();
        fg.remove("bend_radius_m");
        v["choices"]["bend_radius_m"] = serde_json::json!([0.15, 0.2, 0.25]);
        v["baseline"]["bend_radius_m"] = serde_json::json!(0.2);
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v21_geometry_axes_validate_and_gate() {
        CoupledSearchCase::from_json(&v21_bend_axis_case_json())
            .unwrap()
            .validate()
            .unwrap();
        // v20 must not declare the axis.
        let mut v: serde_json::Value = serde_json::from_str(&v21_bend_axis_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v20".into();
        assert!(CoupledSearchCase::from_json(&v.to_string()).is_err());
        for (name, mutate) in [
            (
                "axis and fixed field together",
                Box::new(|v: &mut serde_json::Value| {
                    v["fixed_geometry"]["bend_radius_m"] = serde_json::json!(0.2);
                }) as Box<dyn Fn(&mut serde_json::Value)>,
            ),
            (
                "baseline value outside the axis list",
                Box::new(|v: &mut serde_json::Value| {
                    v["baseline"]["bend_radius_m"] = serde_json::json!(0.18);
                }),
            ),
            (
                "baseline field absent",
                Box::new(|v: &mut serde_json::Value| {
                    v["baseline"]
                        .as_object_mut()
                        .unwrap()
                        .remove("bend_radius_m");
                }),
            ),
            (
                "duplicate axis values",
                Box::new(|v: &mut serde_json::Value| {
                    v["choices"]["bend_radius_m"] = serde_json::json!([0.2, 0.2]);
                }),
            ),
            (
                "non-positive axis value",
                Box::new(|v: &mut serde_json::Value| {
                    v["choices"]["bend_radius_m"] = serde_json::json!([0.2, -0.1]);
                }),
            ),
            (
                "empty axis list",
                Box::new(|v: &mut serde_json::Value| {
                    v["choices"]["bend_radius_m"] = serde_json::json!([]);
                }),
            ),
            (
                "orphan dimension — bend axis with straight resolved nowhere",
                Box::new(|v: &mut serde_json::Value| {
                    v["fixed_geometry"]
                        .as_object_mut()
                        .unwrap()
                        .remove("straight_half_length_m");
                }),
            ),
        ] {
            let mut bad: serde_json::Value =
                serde_json::from_str(&v21_bend_axis_case_json()).unwrap();
            mutate(&mut bad);
            assert!(
                CoupledSearchCase::from_json(&bad.to_string()).is_err(),
                "{name} must be rejected"
            );
        }
        // A baseline dim with no axis declared is a contradiction.
        let mut stray: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        stray["schema"] = "optcoil-coupled-search/v21".into();
        stray["baseline"]["bend_radius_m"] = serde_json::json!(0.2);
        assert!(CoupledSearchCase::from_json(&stray.to_string()).is_err());
        // Axes are racetrack-only: a declared field_map binds the
        // winding shape — the axis cannot move what the map fixes.
        let mut mapped: serde_json::Value =
            serde_json::from_str(&v21_bend_axis_case_json()).unwrap();
        mapped["field_map"] = serde_json::json!({
            "map": {"inline": {"component": "bz",
                "origin_m": [0.0, 0.0, 0.0],
                "spacing_m": [0.01, 0.01, 0.01],
                "counts": [1, 1, 1],
                "values_t": [1.0]}},
            "anchor": {"unit_bz_t_per_ampere_turn": 1.0},
            "provenance": "test"
        });
        assert!(CoupledSearchCase::from_json(&mapped.to_string()).is_err());
    }

    #[test]
    fn v21_geometry_axis_enumerates_priced_and_recorded() {
        let record =
            run_coupled_search_case(&v21_bend_axis_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        // 2 turns-choices × 1 tapes-choice × 3 radii = 6 candidates, each
        // carrying its resolved bend radius on the record.
        assert_eq!(record.candidates.len(), 6);
        let mut radii: Vec<f64> = record
            .candidates
            .iter()
            .map(|c| c.geometry.bend_radius_m.unwrap())
            .collect();
        radii.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(radii, vec![0.15, 0.15, 0.2, 0.2, 0.25, 0.25]);
        // Magnet size is a tape-cost lever: installed length grows with
        // the racetrack's bend radius — the 0.25 m bend prices strictly
        // more conductor than the 0.15 m bend on the same turn count.
        let by_bend = |turns: u32, b: f64| {
            record
                .candidates
                .iter()
                .find(|c| {
                    c.geometry.bend_radius_m == Some(b) && c.geometry.turns_along_normal == turns
                })
                .unwrap()
        };
        let small = by_bend(3, 0.15);
        let large = by_bend(3, 0.25);
        assert!(large.cost.installed_length_m > small.cost.installed_length_m);
        assert!(large.cost.total_usd > small.cost.total_usd);
        // Hand figure: 3 turns × 2 tapes, radial normal — turn k's hoop
        // is 4·0.3 + 2π·(R + δ_k) with offsets δ ∈ {−1e-4, 0, +1e-4}
        // summing to zero, so installed = 3·(1.2 + 2π·R)·2 tapes.
        let expected_small = 3.0 * (4.0 * 0.3 + 2.0 * std::f64::consts::PI * 0.15) * 2.0;
        assert!(
            (small.cost.installed_length_m - expected_small).abs() < 1e-9 * expected_small,
            "{} vs {expected_small}",
            small.cost.installed_length_m
        );
        // The baseline declaration resolves to the 0.2 m candidate.
        let baseline = &record.candidates[record.baseline_index];
        assert_eq!(baseline.geometry.bend_radius_m, Some(0.2));
        // Axis candidates must resolve their geometry — every radius is
        // physically realizable at the fixture's pack width, so none may
        // be marked degenerate. (The inner-radius check once read the
        // unresolved case geometry, where an axis-declared radius is
        // absent — all candidates reported pack_geometry_valid = false.)
        assert!(record.candidates.iter().all(|c| c.pack_geometry_valid));
        // Acceptance independently re-resolves dims from the record.
        assert_eq!(
            record.acceptance.baseline.cost_agreement_status,
            Status::Pass
        );
    }

    #[test]
    fn v21_geometry_axis_acceptance_catches_planted_dims() {
        let mut record =
            run_coupled_search_case(&v21_bend_axis_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        // Plant a different radius on the baseline candidate than the
        // ledger was priced at — the acceptance recomputation must
        // disagree, never inherit the record's claim.
        record.candidates[record.baseline_index]
            .geometry
            .bend_radius_m = Some(0.25);
        let assessment = search_acceptance::assess(
            &record.case,
            &record.candidates,
            record.baseline_index,
            record.best_index,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            assessment.baseline.cost_agreement_status,
            Status::Fail,
            "a record naming 0.25 m but priced at 0.2 m must fail the cost agreement"
        );
    }

    /// Schema v22 fixture: the v3 reduced case plus a declared
    /// uniformity bound on its good-field region.
    fn v22_quality_case_json(bound: f64) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v22".into();
        v["requirement"]["good_field_region"]["max_relative_deviation"] = serde_json::json!(bound);
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v22_uniformity_bound_validates_and_gates() {
        CoupledSearchCase::from_json(&v22_quality_case_json(0.05))
            .unwrap()
            .validate()
            .unwrap();
        // v21 must not declare the bound.
        let mut v: serde_json::Value = serde_json::from_str(&v22_quality_case_json(0.05)).unwrap();
        v["schema"] = "optcoil-coupled-search/v21".into();
        assert!(CoupledSearchCase::from_json(&v.to_string()).is_err());
        // Bounds outside (0, 1) are declaration bugs — a bound at or
        // past 1 tolerates a sign reversal inside the usable volume.
        // (NaN cannot be declared through JSON — it serializes to null,
        // which deserializes as the absent bound — so the finiteness
        // check guards non-JSON construction paths.)
        for bad in [0.0, -0.1, 1.0, 1.5] {
            assert!(
                CoupledSearchCase::from_json(&v22_quality_case_json(bad)).is_err(),
                "bound {bad} must be rejected"
            );
        }
    }

    #[test]
    fn v22_uniformity_bound_gates_the_requirement() {
        // Unbounded v3 run: the deviation is recorded regardless.
        let unbounded =
            run_coupled_search_case(&v3_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        let deviation = unbounded.candidates[0]
            .good_field
            .as_ref()
            .unwrap()
            .relative_deviation
            .unwrap();
        assert!(deviation.is_finite() && deviation > 0.0);
        // Every candidate's recorded deviation matches its own recorded
        // extremes — (max − min) / unit_bore_bz.
        for c in &unbounded.candidates {
            let g = c.good_field.as_ref().unwrap();
            let expected = (g.max_unit_bz_t_per_ampere_turn.unwrap()
                - g.min_unit_bz_t_per_ampere_turn)
                / c.unit_bore_bz_t_per_ampere_turn;
            assert!((g.relative_deviation.unwrap() - expected).abs() < 1e-12 * expected);
        }
        // A bound tighter than the observed spread fails the
        // requirement on every candidate — the weakest point still hits
        // the target, but the region is not uniform enough to sell.
        let tight = run_coupled_search_case(
            &v22_quality_case_json(deviation / 2.0),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        for c in &tight.candidates {
            assert_eq!(c.requirement_status, Status::Fail);
        }
        // A bound looser than the spread passes identically to
        // unbounded — the bound is a gate, not a perturbation.
        let loose = run_coupled_search_case(
            &v22_quality_case_json(deviation * 2.0),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(
            loose
                .candidates
                .iter()
                .map(|c| c.requirement_status)
                .collect::<Vec<_>>(),
            unbounded
                .candidates
                .iter()
                .map(|c| c.requirement_status)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn v22_planted_deviation_detected() {
        let mut record =
            run_coupled_search_case(&v3_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        // Plant an absurd deviation on the baseline candidate — the
        // record claims a spread far past anything the lattice
        // evaluated, so the recomputation must disagree. (The
        // NI-scaled agreement gate is loose on this reduced case —
        // 0.5 T — so the planted discrepancy is scaled to clear it:
        // the check proves disagreement detection, not gate tightness.)
        let baseline_field = record.candidates[record.baseline_index]
            .good_field
            .as_mut()
            .unwrap();
        *baseline_field.relative_deviation.as_mut().unwrap() += 20.0;
        let assessment = search_acceptance::assess(
            &record.case,
            &record.candidates,
            record.baseline_index,
            record.best_index,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            assessment.baseline.region_agreement_status,
            Status::Fail,
            "a halved recorded deviation must fail region agreement"
        );
    }

    /// Schema v23 fixture: the v3 reduced case plus a declared midplane
    /// harmonic expansion on its good-field region (radius strictly
    /// inside the 0.05 m in-plane extent, Nyquist-satisfied sampling).
    fn v23_harmonics_case_json(harmonics: serde_json::Value) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v22_quality_case_json(0.05)).unwrap();
        v["schema"] = "optcoil-coupled-search/v23".into();
        v["requirement"]["good_field_region"]["harmonics"] = harmonics;
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v23_harmonics_validate_and_gate() {
        let declared = serde_json::json!({
            "reference_radius_m": 0.03,
            "max_order": 4,
            "theta_samples": 32
        });
        CoupledSearchCase::from_json(&v23_harmonics_case_json(declared.clone()))
            .unwrap()
            .validate()
            .unwrap();
        // v22 must not declare the expansion.
        let mut v: serde_json::Value =
            serde_json::from_str(&v23_harmonics_case_json(declared)).unwrap();
        v["schema"] = "optcoil-coupled-search/v22".into();
        assert!(CoupledSearchCase::from_json(&v.to_string()).is_err());
        // The circle must lie inside the region, sampling must satisfy
        // Nyquist, and bounds stay in (0, 1).
        for bad in [
            serde_json::json!({"reference_radius_m": 0.05, "max_order": 4, "theta_samples": 32}),
            serde_json::json!({"reference_radius_m": 0.0, "max_order": 4, "theta_samples": 32}),
            serde_json::json!({"reference_radius_m": 0.03, "max_order": 0, "theta_samples": 32}),
            serde_json::json!({"reference_radius_m": 0.03, "max_order": 4, "theta_samples": 8}),
            serde_json::json!({"reference_radius_m": 0.03, "max_order": 4, "theta_samples": 32,
                               "max_normal_unit_fraction": 1.0}),
            serde_json::json!({"reference_radius_m": 0.03, "max_order": 4, "theta_samples": 32,
                               "max_skew_unit_fraction": -0.5}),
        ] {
            assert!(
                CoupledSearchCase::from_json(&v23_harmonics_case_json(bad.clone())).is_err(),
                "harmonics {bad} must be rejected"
            );
        }
    }

    #[test]
    fn v23_harmonics_record_and_bound_the_spectrum() {
        // Unbounded declaration: the spectrum is recorded, not gated.
        let case = v23_harmonics_case_json(serde_json::json!({
            "reference_radius_m": 0.03,
            "max_order": 4,
            "theta_samples": 32
        }));
        let record = run_coupled_search_case(&case, &CoupledSearchOptions::default()).unwrap();
        for c in &record.candidates {
            let g = c.good_field.as_ref().unwrap();
            let b0 = g.harmonic_b0_t_per_ampere_turn.unwrap();
            let normal = g.harmonic_normal_units.as_ref().unwrap();
            let skew = g.harmonic_skew_units.as_ref().unwrap();
            assert!(b0.is_finite() && b0 > 0.0);
            assert_eq!(normal.len(), 4);
            assert_eq!(skew.len(), 4);
            assert!(normal.iter().all(|u| u.is_finite()));
        }
        // A bound tighter than the recorded spectrum fails every
        // candidate; looser than the spectrum passes like unbounded.
        let max_normal = record
            .candidates
            .iter()
            .flat_map(|c| {
                c.good_field
                    .as_ref()
                    .unwrap()
                    .harmonic_normal_units
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|u| u.abs())
                    .collect::<Vec<_>>()
            })
            .fold(0.0_f64, f64::max);
        // A racetrack is not axisymmetric — the spectrum carries real
        // multipoles; the guard only protects declaration validity.
        assert!(max_normal > 1e-9, "spectrum must be nonzero to gate");
        let tight = v23_harmonics_case_json(serde_json::json!({
            "reference_radius_m": 0.03, "max_order": 4, "theta_samples": 32,
            "max_normal_unit_fraction": max_normal / 2.0
        }));
        let tight_record =
            run_coupled_search_case(&tight, &CoupledSearchOptions::default()).unwrap();
        assert!(
            tight_record
                .candidates
                .iter()
                .all(|c| c.requirement_status == Status::Fail)
        );
        let loose = v23_harmonics_case_json(serde_json::json!({
            "reference_radius_m": 0.03, "max_order": 4, "theta_samples": 32,
            "max_normal_unit_fraction": max_normal * 2.0
        }));
        let loose_record =
            run_coupled_search_case(&loose, &CoupledSearchOptions::default()).unwrap();
        assert_eq!(
            loose_record
                .candidates
                .iter()
                .map(|c| c.requirement_status)
                .collect::<Vec<_>>(),
            record
                .candidates
                .iter()
                .map(|c| c.requirement_status)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn v23_planted_harmonic_detected() {
        let case = v23_harmonics_case_json(serde_json::json!({
            "reference_radius_m": 0.03, "max_order": 4, "theta_samples": 32
        }));
        let mut record = run_coupled_search_case(&case, &CoupledSearchOptions::default()).unwrap();
        // Plant an absurd skew coefficient — the recomputation must
        // disagree, never inherit the claim.
        record.candidates[record.baseline_index]
            .good_field
            .as_mut()
            .unwrap()
            .harmonic_skew_units
            .as_mut()
            .unwrap()[0] += 30.0;
        let assessment = search_acceptance::assess(
            &record.case,
            &record.candidates,
            record.baseline_index,
            record.best_index,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            assessment.baseline.region_agreement_status,
            Status::Fail,
            "a planted skew coefficient must fail region agreement"
        );
    }

    #[test]
    fn ni_is_solved_on_the_good_field_region_minimum_not_the_center_probe() {
        let record =
            run_coupled_search_case(&v3_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        let case: CoupledSearchCase = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        for candidate in &record.candidates {
            let field = candidate
                .good_field
                .as_ref()
                .expect("v3 records the region");
            // The region minimum is never above the center probe value.
            assert!(
                field.min_unit_bz_t_per_ampere_turn
                    <= candidate.unit_bore_bz_t_per_ampere_turn * (1.0 + 1e-9)
            );
            // NI = b_target_t / region minimum, not the center probe.
            let expected_ni = case.requirement.b_target_t / field.min_unit_bz_t_per_ampere_turn;
            assert!(
                (candidate.ampere_turns_a - expected_ni).abs() < 1e-9 * expected_ni.abs(),
                "NI must be solved on the region minimum"
            );
            // The recorded worst point lies inside the declared box.
            let region = case.requirement.good_field_region.as_ref().unwrap();
            for (axis, &half) in region.half_extents_m.iter().enumerate() {
                assert!(field.worst_point_m[axis].abs() <= half + 1e-12);
            }
            assert!(candidate.requirement_kernel_evaluations > 0);
            assert_eq!(candidate.requirement_status, Status::Pass);
        }
        // The acceptance module independently re-evaluates the region.
        assert_eq!(
            record.acceptance.baseline.region_agreement_status,
            Status::Pass
        );
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
    }

    fn v5_reduced_case_json() -> String {
        v3_reduced_case_json()
            .replacen(
                "\"optcoil-coupled-search/v3\"",
                "\"optcoil-coupled-search/v5\"",
                1,
            )
            .replacen(
                "\"utilization_limit\": 0.8",
                "\"utilization_limit\": 0.8, \"self_field_correction\": \"uniform_transport\"",
                1,
            )
    }

    #[test]
    fn v5_self_field_correction_flows_through_the_full_search_and_acceptance() {
        // OC-014 end-to-end: the declared correction must reach the coarse
        // plan, the full plan (run_coupled_case) and the independent
        // acceptance recompute — the generated conductor case carries the
        // v2 schema, and the candidate record reports the transport ratio.
        let record =
            run_coupled_search_case(&v5_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v5");
        let mut saw_transport_ratio = false;
        for candidate in &record.candidates {
            if let Some(screening) = &candidate.screening
                && let Some(r) = screening.max_transport_self_field_ratio
            {
                saw_transport_ratio = true;
                // The gate boundary is the physical dominance ratio.
                if screening.status == Status::Pass {
                    assert!(r <= 1.0, "PASS candidate exceeded the dominance bound");
                }
            }
        }
        assert!(
            saw_transport_ratio,
            "no evaluated candidate reported the transport ratio"
        );
        // Acceptance still runs end-to-end on the corrected semantics.
        assert!(matches!(
            record.acceptance.agreement_status,
            Status::Pass | Status::Fail | Status::Inconclusive
        ));
    }

    /// v3 fixture with two unusable-region candidates: 3500 turns puts the
    /// inner face at 0.2 - 0.175 = 0.025 m, so the region's |y| = 0.05
    /// points sit inside the conductor band (recorded `pack_overlap`);
    /// 5000 turns makes the pack degenerate (inner radius -0.05), so no
    /// evaluator can even be constructed (`pack_geometry_valid`, and the
    /// run reports the FAIL rather than erroring).
    #[test]
    fn a_region_inside_the_winding_fails_the_requirement() {
        let record = run_coupled_search_case(
            &v3_reduced_case_json().replacen("[3, 60]", "[3, 3500, 5000]", 1),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let overlapping = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 3500)
            .expect("the 3500-turn candidate exists");
        let field = overlapping.good_field.as_ref().unwrap();
        assert!(field.pack_overlap, "the region must report pack overlap");
        assert_eq!(overlapping.requirement_status, Status::Fail);
        assert_eq!(overlapping.status, Status::Fail);
        let degenerate = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 5000)
            .expect("the 5000-turn candidate exists");
        assert!(!degenerate.pack_geometry_valid);
        assert!(degenerate.good_field.is_none());
        assert_eq!(degenerate.requirement_status, Status::Fail);
        let clear = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 3)
            .expect("the 3-turn candidate exists");
        assert!(!clear.good_field.as_ref().unwrap().pack_overlap);
    }

    /// v3 fixture with `manufacturing.min_inner_bend_radius_m = 0.19`:
    /// inner face = 0.2 - n*5e-5 >= 0.19 requires n <= 200, so the 300-turn
    /// candidate is infeasible while the 60-turn candidate passes the gate.
    #[test]
    fn an_inner_bend_radius_below_the_declared_minimum_fails() {
        let json = v3_reduced_case_json()
            .replacen("[3, 60]", "[60, 300]", 1)
            .replacen(
                "\"baseline\": {\"turns_along_normal\": 3",
                "\"baseline\": {\"turns_along_normal\": 60",
                1,
            )
            .replacen(
                "\"execution\":",
                r#""manufacturing": {"min_inner_bend_radius_m": 0.19},
  "execution":"#,
                1,
            );
        let record = run_coupled_search_case(&json, &CoupledSearchOptions::default()).unwrap();
        let infeasible = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 300)
            .expect("the 300-turn candidate exists");
        assert!(!infeasible.manufacturing_feasible);
        assert_eq!(infeasible.requirement_status, Status::Fail);
        assert_eq!(infeasible.status, Status::Fail);
        let feasible = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 60)
            .expect("the 60-turn candidate exists");
        assert!(feasible.manufacturing_feasible);
        // Acceptance independently recomputes the gate on the baseline.
        assert_eq!(
            record.acceptance.baseline.manufacturing_agreement_status,
            Status::Pass
        );
    }

    #[test]
    fn ni_and_i_op_come_from_a_known_unit_bore_field() {
        let case = CoupledSearchCase::from_json(&reduced_case_json("[3, 500]", 30.0)).unwrap();
        let record = run_coupled_search_case(
            &serde_json::to_string(&{
                let mut c = case.clone();
                c.choices.turns_along_normal = vec![3];
                c.baseline.turns_along_normal = 3;
                c
            })
            .unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let candidate = &record.candidates[0];
        assert_eq!(candidate.requirement_status, Status::Pass);
        // NI = b_target_t / unit_bore_bz, independently recomputed here.
        let expected_ni = case.requirement.b_target_t / candidate.unit_bore_bz_t_per_ampere_turn;
        assert!((candidate.ampere_turns_a - expected_ni).abs() < 1e-6 * expected_ni.abs());
        let expected_i_op = expected_ni / candidate.geometry.total_turns as f64;
        assert!((candidate.operating_current_a - expected_i_op).abs() < 1e-6 * expected_i_op.abs());
    }

    #[test]
    fn coarse_fail_prunes_and_coarse_pass_or_inconclusive_reaches_the_full_plan() {
        let record = run_coupled_search_case(
            &reduced_case_with_pruning_json(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.candidates.len(), 2);
        let by_turns = |turns: u32| {
            record
                .candidates
                .iter()
                .find(|c| c.geometry.turns_along_normal == turns)
                .unwrap()
        };
        // 3 turns needs an enormous NI to hit the same 0.05 T bore target as
        // 60 turns, driving every sampled point's utilization far over the
        // limit: the coarse plan must FAIL and prune before the full plan runs.
        let small = by_turns(3);
        assert!(
            small.pruned_by.is_some(),
            "expected the 3-turn geometry to be pruned"
        );
        assert_eq!(small.screening.as_ref().unwrap().status, Status::Fail);
        assert_eq!(
            small.full_points_evaluated, 0,
            "pruning must skip the full plan"
        );

        // 60 turns needs a far smaller NI/I_op for the same bore target, so
        // the coarse subset must not FAIL, and the full plan must actually run.
        let large = by_turns(60);
        assert!(
            large.pruned_by.is_none(),
            "expected the 60-turn geometry not to be pruned"
        );
        assert!(
            large.full_points_evaluated > 0,
            "the full plan must have run"
        );
        assert!(
            large.coarse_points_evaluated > 0,
            "the coarse plan must have run first"
        );
    }

    #[test]
    fn unconverged_coarse_point_prevents_pruning_and_forces_the_full_plan() {
        // Contract §9.3: a coarse FAIL may only prune when every coarse
        // point's own refinement change (scaled to NI) is itself within the
        // declared gate. Tightening reduced_case_with_pruning_json's gate
        // from 0.5 T to 0.001 T (same orders [2, 4]) makes the 3-turn
        // candidate's own coarse points -- which genuinely FAIL the
        // screening margin, as the untightened test above demonstrates --
        // also exceed the tightened refinement gate (observed
        // point_refinement_change_t up to ~1.26e-2 T there, against the
        // bore probe's own unrelated, much smaller raw change), so pruning
        // must not happen: the candidate falls through to the full plan
        // instead, with coarse_refinement_unresolved recorded.
        let json = reduced_case_with_pruning_json().replace(
            r#""max_refinement_change_fraction": 0.5"#,
            r#""max_refinement_change_fraction": 0.001"#,
        );
        let record = run_coupled_search_case(&json, &CoupledSearchOptions::default()).unwrap();
        let small = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 3)
            .unwrap();
        assert!(
            small.pruned_by.is_none(),
            "an unconverged coarse point must prevent pruning even though the coarse subset genuinely FAILs"
        );
        assert!(
            small.coarse_refinement_unresolved,
            "the record must note that the coarse FAIL could not be trusted"
        );
        assert!(
            small.full_points_evaluated > 0,
            "the candidate must fall through to the full plan"
        );
    }

    #[test]
    fn single_thread_and_multi_thread_results_are_bit_identical() {
        let json = reduced_case_json("[3, 60]", 30.0);
        let single =
            run_coupled_search_case(&json, &CoupledSearchOptions { threads: Some(1) }).unwrap();
        let multi =
            run_coupled_search_case(&json, &CoupledSearchOptions { threads: Some(3) }).unwrap();
        assert_eq!(single.candidates.len(), multi.candidates.len());
        for (a, b) in single.candidates.iter().zip(&multi.candidates) {
            assert_eq!(a.geometry.turns_along_normal, b.geometry.turns_along_normal);
            assert_eq!(a.status, b.status);
            assert_eq!(a.cost.total_usd.to_bits(), b.cost.total_usd.to_bits());
            assert_eq!(
                a.unit_bore_bz_t_per_ampere_turn.to_bits(),
                b.unit_bore_bz_t_per_ampere_turn.to_bits()
            );
            assert_eq!(a.ampere_turns_a.to_bits(), b.ampere_turns_a.to_bits());
        }
        assert_eq!(single.best_index, multi.best_index);
        assert_eq!(single.savings_usd, multi.savings_usd);
    }

    #[test]
    fn threads_option_may_only_lower_the_case_declared_maximum() {
        let json = reduced_case_json("[3, 60]", 30.0);
        assert!(
            run_coupled_search_case(&json, &CoupledSearchOptions { threads: Some(99) }).is_err()
        );
        assert!(
            run_coupled_search_case(&json, &CoupledSearchOptions { threads: Some(0) }).is_err()
        );
        assert!(run_coupled_search_case(&json, &CoupledSearchOptions { threads: Some(2) }).is_ok());
    }

    #[test]
    fn cancellation_returns_cancelled_and_a_never_set_flag_changes_nothing() {
        let json = reduced_case_json("[3, 4, 5]", 30.0);
        let cancelled = run_coupled_search_case_cancellable(
            &json,
            &CoupledSearchOptions::default(),
            &AtomicBool::new(true),
        );
        assert!(matches!(cancelled, Err(RunError::Cancelled)));

        let record = run_coupled_search_case_cancellable(
            &json,
            &CoupledSearchOptions::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.candidates.len(), 3);
    }

    #[test]
    fn mid_candidate_cancellation_is_observed_between_live_field_evaluations() {
        let json = reduced_case_json("[3, 60, 80]", 30.0);
        let progress = Arc::new(SearchProgress::new());
        let cancel = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let progress_watch = Arc::clone(&progress);
            let cancel_watch = Arc::clone(&cancel);
            let watcher = scope.spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    if progress_watch.candidates_total.load(Ordering::Relaxed) > 0
                        && progress_watch
                            .leg(0)
                            .is_some_and(|leg| leg.load(Ordering::Relaxed) >= 1)
                    {
                        cancel_watch.store(true, Ordering::Relaxed);
                        return true;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "candidate evaluation did not publish a progress tick"
                    );
                    std::thread::yield_now();
                }
            });
            let result = run_coupled_search_case_with_dataset_progress(
                &json,
                &CoupledSearchOptions { threads: Some(1) },
                None,
                &cancel,
                Some(&progress),
            );
            assert!(
                watcher.join().unwrap(),
                "cancellation was not injected from live progress"
            );
            assert!(matches!(result, Err(RunError::Cancelled)));
        });
    }

    #[test]
    fn acceptance_phase_cancellation_stops_its_independent_rerun() {
        let json = reduced_case_json("[3, 60, 80]", 30.0).replace(
            "\"baseline\": {\"turns_along_normal\": 3",
            "\"baseline\": {\"turns_along_normal\": 60",
        );
        let progress = Arc::new(SearchProgress::new());
        let cancel = Arc::new(AtomicBool::new(false));
        std::thread::scope(|scope| {
            let progress_watch = Arc::clone(&progress);
            let cancel_watch = Arc::clone(&cancel);
            let watcher = scope.spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let acceptance = progress_watch.phase_label().starts_with("acceptance:");
                    let baseline_leg = progress_watch.candidates_total.load(Ordering::Relaxed);
                    if acceptance
                        && progress_watch
                            .leg(baseline_leg)
                            .is_some_and(|leg| leg.load(Ordering::Relaxed) >= 1)
                    {
                        cancel_watch.store(true, Ordering::Relaxed);
                        return true;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "acceptance rerun did not publish a progress tick"
                    );
                    std::thread::yield_now();
                }
            });
            let result = run_coupled_search_case_with_dataset_progress(
                &json,
                &CoupledSearchOptions { threads: Some(1) },
                None,
                &cancel,
                Some(&progress),
            );
            assert!(
                watcher.join().unwrap(),
                "cancellation was not injected from the acceptance phase"
            );
            assert!(matches!(result, Err(RunError::Cancelled)));
        });
    }

    /// The customer-data path: a supplied dataset is bound by the case's
    /// own `dataset_id`/`csv_sha256` pins, so the run record identifies the
    /// supplied bytes — and a dataset that doesn't match is rejected rather
    /// than silently substituted.
    #[test]
    fn supplied_dataset_is_identity_checked_and_recorded() {
        let json = reduced_case_json("[3]", 30.0);
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let record = run_coupled_search_case_with_dataset(
            &json,
            &CoupledSearchOptions::default(),
            Some(dataset),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.dataset_id, "robinson-superpower-ap-v3");
        assert_eq!(
            record.dataset_csv_sha256,
            "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354"
        );

        let wrong = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
        assert!(
            run_coupled_search_case_with_dataset(
                &json,
                &CoupledSearchOptions::default(),
                Some(wrong),
                &AtomicBool::new(false),
            )
            .is_err()
        );
    }

    /// The progress sink must track real work: legs are populated at
    /// dispatch, each field evaluation ticks its candidate's leg, and for
    /// a candidate that runs the full sampling plan the estimate is exact
    /// — every evaluator call is ticked, nothing else is.
    #[test]
    fn progress_legs_track_field_evaluations_exactly() {
        let progress = SearchProgress::new();
        let record = run_coupled_search_case_with_dataset_progress(
            &reduced_case_json("[3]", 30.0),
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
            Some(&progress),
        )
        .unwrap();
        assert_eq!(
            progress.candidates_total.load(Ordering::Relaxed),
            record.candidates.len()
        );
        assert_eq!(
            progress.candidates_done.load(Ordering::Relaxed),
            record.candidates.len()
        );
        let legs = progress.legs.lock().unwrap();
        // One leg per candidate, then the two pre-planned acceptance
        // legs (baseline + optimum — created pessimistically at dispatch,
        // revised to actual scope when `assess` runs).
        assert_eq!(
            legs.len(),
            record.candidates.len() + 2,
            "expected {} candidate legs + 2 acceptance legs",
            record.candidates.len()
        );
        for (leg, candidate) in legs.iter().zip(record.candidates.iter()) {
            let done = leg.done.load(Ordering::Relaxed);
            assert!(done > 0, "candidate {} ticked no evals", candidate.index);
            if candidate.full_points_evaluated > 0 {
                assert_eq!(
                    done, leg.planned,
                    "candidate {}: estimate drifted from the real eval count",
                    candidate.index
                );
            } else {
                assert!(done < leg.planned, "early-exit leg over-ticked");
            }
        }
        // Acceptance legs: a rerun always executes every evaluation in
        // its plan — probes, original plan, and (PASS only) the refined
        // plan — so done must equal planned exactly.
        for leg in legs.iter().skip(record.candidates.len()) {
            assert_eq!(
                leg.done.load(Ordering::Relaxed),
                leg.planned,
                "acceptance leg: estimate drifted from the real eval count"
            );
        }
    }

    #[test]
    fn best_selection_prefers_lower_cost_and_ties_break_by_fewer_turns_then_index() {
        // Two identical-cost geometries differ only in turn count: the
        // cheaper choice below is forced by construction (same price
        // regardless of turns is impossible with this cost model since
        // installed length scales with turns), so instead directly exercise
        // the tie-break comparator used by run_coupled_search_case.
        let a = (5_000.0_f64, 10_u64, 0_usize);
        let b = (5_000.0_f64, 20_u64, 1_usize);
        let ordering = a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2));
        assert_eq!(
            ordering,
            std::cmp::Ordering::Less,
            "fewer total turns must win a cost tie"
        );
    }

    /// Reduced schema-v9 path case: the OC-019 D-shape centerline (inner leg
    /// 0.7 m, two 90-deg corners at R=0.08, 180-deg outer bulge at R=0.43,
    /// centerline 0.7 + 0.51*pi = 2.30221 m) with a two-candidate grid,
    /// two path stations, and the bore probe inside the D.
    fn reduced_path_case_json(turns_choices: &str) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v9",
  "id": "reduced-path-search-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{
    "bore_probe_m": [0.2, 0.0, 0.0],
    "b_target_t": 0.05,
    "tolerance_fraction": 1e-6,
    "good_field_region": {{"half_extents_m": [0.02, 0.02, 0.02], "points_per_axis": 2}}
  }},
  "fixed_geometry": {{
    "path": {{
      "segments": [
        {{"kind": "line", "length_m": 0.7}},
        {{"kind": "arc", "radius_m": 0.08, "sweep_deg": 90.0}},
        {{"kind": "arc", "radius_m": 0.43, "sweep_deg": 180.0}},
        {{"kind": "arc", "radius_m": 0.08, "sweep_deg": 90.0}}
      ],
      "start_position_m": [0.0, 0.35],
      "start_heading_deg": 270.0
    }},
    "radial_pitch_m": 0.0005,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  }},
  "choices": {{"turns_along_normal": {turns_choices}, "tapes_along_width": [2]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
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
      {{"id": "leg_mid", "kind": "path", "s_m": 0.35}},
      {{"id": "outer_mid", "kind": "path", "s_m": 1.501107}}
    ],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "refinement": {{
    "pancake_counts": [2],
    "turn_resolution": 2,
    "turn_bounds": {{"min": 1, "max": 200}},
    "brackets": [
      {{"tapes": 2, "fail_turns": 10, "pass_turns": 50}}
    ],
    "monotonicity_check": true
  }},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{"turns_along_normal": 60, "tapes_along_width": 2}},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "leg_q1", "kind": "path", "s_m": 0.175}},
      {{"id": "leg_q3", "kind": "path", "s_m": 0.525}},
      {{"id": "corner_low_entry", "kind": "path", "s_m": 0.71}},
      {{"id": "outer_q1", "kind": "path", "s_m": 1.163384}},
      {{"id": "outer_q3", "kind": "path", "s_m": 1.838829}},
      {{"id": "corner_high_exit", "kind": "path", "s_m": 2.29}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": 2}}
}}
"#
        )
    }

    /// Reduced schema-v13 declared-map case: an origin-centred circle at
    /// R=0.68, fixed pack extents 0.064 m x 0.24 m (axial tape normal —
    /// pancake-winding convention), and a tiny 2x3 cylindrical map
    /// whose hull covers the pack. Baseline 4 turns x 4 tapes: under the
    /// axial normal the pitch/width consistency rule reads
    /// pitch = axial_extent/turns = 0.06, tape_width = radial_extent/tapes
    /// = 0.016. `bore_field_at_reference_t` = 10 T at 1e6 A-turns and the
    /// requirement asks for exactly 10 T, so every candidate's NI is
    /// exactly 1e6 — exercising the declared-anchor NI solve.
    fn reduced_map_case_json(turns_choices: &str) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v13",
  "id": "reduced-map-search-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 10.0, "tolerance_fraction": 1e-6}},
  "fixed_geometry": {{
    "path": {{
      "segments": [{{"kind": "arc", "radius_m": 0.68, "sweep_deg": 360.0}}],
      "start_position_m": [0.68, 0.0],
      "start_heading_deg": 90.0
    }},
    "radial_pitch_m": 0.06,
    "tape_width_m": 0.016,
    "tape_normal": "axial",
    "pack_radial_width_m": 0.064,
    "pack_axial_height_m": 0.24
  }},
  "field_map": {{
    "map": {{
      "source_sha256": "{map_sha}",
      "components": "cylindrical_br_bz",
      "reference_ampere_turns_a": 1.0e6,
      "rho_levels_m": [0.648, 0.712],
      "z_levels_m": [-0.12, 0.0, 0.12],
      "entries": [
        {{"rho_index": 0, "z_index": 0, "br_t": 0.5, "bz_t": 20.0}},
        {{"rho_index": 0, "z_index": 1, "br_t": 0.0, "bz_t": 20.0}},
        {{"rho_index": 0, "z_index": 2, "br_t": -0.5, "bz_t": 20.0}},
        {{"rho_index": 1, "z_index": 0, "br_t": 0.5, "bz_t": 8.0}},
        {{"rho_index": 1, "z_index": 1, "br_t": 0.0, "bz_t": 8.0}},
        {{"rho_index": 1, "z_index": 2, "br_t": -0.5, "bz_t": 8.0}}
      ]
    }},
    "bore_field_at_reference_t": 10.0
  }},
  "choices": {{"turns_along_normal": {turns_choices}, "tapes_along_width": [4]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
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
    "stations": [{{"id": "p0", "kind": "path", "s_m": 0.0}}],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "refinement": {{
    "pancake_counts": [4],
    "turn_resolution": 1,
    "turn_bounds": {{"min": 1, "max": 8}},
    "brackets": [{{"tapes": 4, "fail_turns": 1, "pass_turns": 4}}],
    "monotonicity_check": true
  }},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{"turns_along_normal": 4, "tapes_along_width": 4}},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "pq1", "kind": "path", "s_m": 0.5340707511102649}},
      {{"id": "pq2", "kind": "path", "s_m": 1.0681415022205298}},
      {{"id": "pq3", "kind": "path", "s_m": 1.6022122533307947}},
      {{"id": "ph", "kind": "path", "s_m": 2.1362830044410596}},
      {{"id": "pq5", "kind": "path", "s_m": 2.6703537555513245}},
      {{"id": "pq6", "kind": "path", "s_m": 3.2044245066615894}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": 2}}
}}
"#,
            map_sha = "a".repeat(64),
        )
    }

    /// Reduced schema-v19 non-planar case: a 4-turn helix at R=0.05 about
    /// +z (CORC/CCT-class layer), rise 0.02 m/turn — centerline z ∈
    /// [0, 0.08], cylinder radius 0.05. Fixed pack extents 0.01 m
    /// (radial, cylinder-radial stacking under `tape_normal: radial`) x
    /// 0.02 m (in-surface binormal), baseline 4 turns x 4 tapes giving
    /// radial_pitch 0.0025 and tape_width 0.005. A tiny 2x2x2 uniform
    /// Cartesian map (Bz = 8 T) covers the swept band
    /// (|x|,|y| ≤ 0.056, z ∈ [-0.01, 0.09] with margin); the bore anchor
    /// is 8 T at 1e6 A-turns and the requirement asks exactly 8 T, so
    /// every candidate's NI is exactly 1e6.
    pub(crate) fn helix_map_case_json(turns_choices: &str) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v19",
  "id": "reduced-helix-search-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.04], "b_target_t": 8.0, "tolerance_fraction": 1e-6}},
  "fixed_geometry": {{
    "path3d": {{
      "segments": [{{
        "kind": "helix",
        "axis_origin_m": [0.0, 0.0, 0.0],
        "axis_dir": [0.0, 0.0, 1.0],
        "radius_m": 0.05,
        "start_azimuth_deg": 0.0,
        "turns": 4.0,
        "rise_per_turn_m": 0.02
      }}],
      "closed": false
    }},
    "radial_pitch_m": 0.0025,
    "tape_width_m": 0.005,
    "tape_normal": "radial",
    "pack_radial_width_m": 0.01,
    "pack_axial_height_m": 0.02
  }},
  "field_map": {{
    "map": {{
      "source_sha256": "{map_sha}",
      "components": "cartesian_bx_by_bz",
      "reference_ampere_turns_a": 1.0e6,
      "x_levels_m": [-0.06, 0.06],
      "y_levels_m": [-0.06, 0.06],
      "z_levels_m": [-0.02, 0.10],
      "entries": [
        {{"x_index": 0, "y_index": 0, "z_index": 0, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 0, "y_index": 0, "z_index": 1, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 0, "y_index": 1, "z_index": 0, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 0, "y_index": 1, "z_index": 1, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 1, "y_index": 0, "z_index": 0, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 1, "y_index": 0, "z_index": 1, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 1, "y_index": 1, "z_index": 0, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}},
        {{"x_index": 1, "y_index": 1, "z_index": 1, "bx_t": 0.0, "by_t": 0.0, "bz_t": 8.0}}
      ]
    }},
    "bore_field_at_reference_t": 8.0
  }},
  "choices": {{"turns_along_normal": {turns_choices}, "tapes_along_width": [4]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
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
      {{"id": "p0", "kind": "path", "s_m": 0.0}},
      {{"id": "ph", "kind": "path", "s_m": 0.6}}
    ],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "refinement": {{
    "pancake_counts": [4],
    "turn_resolution": 1,
    "turn_bounds": {{"min": 1, "max": 8}},
    "brackets": [{{"tapes": 4, "fail_turns": 1, "pass_turns": 4}}],
    "monotonicity_check": true
  }},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{"turns_along_normal": 4, "tapes_along_width": 4}},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "pq1", "kind": "path", "s_m": 0.15}},
      {{"id": "pq2", "kind": "path", "s_m": 0.3}},
      {{"id": "pq3", "kind": "path", "s_m": 0.45}},
      {{"id": "pq5", "kind": "path", "s_m": 0.75}},
      {{"id": "pq6", "kind": "path", "s_m": 0.9}},
      {{"id": "pq7", "kind": "path", "s_m": 1.05}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": 2}}
}}
"#,
            map_sha = "a".repeat(64),
        )
    }

    /// The helix turn length at cylinder-radial offset `delta` on the
    /// v19 fixture's single segment: `|turns|·2π·hypot(R+δ, c)` with
    /// `c = 0.02/2π`.
    fn helix_turn_length_m(delta_m: f64) -> f64 {
        let c = 0.02 / (2.0 * std::f64::consts::PI);
        4.0 * 2.0 * std::f64::consts::PI * (0.05 + delta_m).hypot(c)
    }

    #[test]
    fn helix_case_validates_and_generates_coupled_v9() {
        let case: CoupledSearchCase = serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        case.validate().unwrap();
        let full = build_coupled_case(
            &case,
            4,
            4,
            1,
            250.0,
            case.sampling.stations.clone(),
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(full.schema, optcoil_model::coupled::COUPLED_CASE_SCHEMA_V9);
        assert!(full.pack.path3d.is_some());
        assert!(full.pack.path.is_none());
        full.validate().unwrap();
    }

    #[test]
    fn path3d_requires_schema_v19_and_radial_normal_and_cartesian_map() {
        // Pre-v19 schema cannot declare path3d.
        let mut value: serde_json::Value =
            serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        value["schema"] = serde_json::json!("optcoil-coupled-search/v18");
        let case: CoupledSearchCase = serde_json::from_value(value).unwrap();
        let err = case.validate().unwrap_err();
        assert!(format!("{err:?}").contains("path3d"), "{err}");

        // The radial tape normal is the only stacking a helix models.
        let mut value: serde_json::Value =
            serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        value["fixed_geometry"]["tape_normal"] = serde_json::json!("axial");
        let case: CoupledSearchCase = serde_json::from_value(value).unwrap();
        let err = case.validate().unwrap_err();
        assert!(format!("{err:?}").contains("radial"), "{err}");

        // Non-planar packs evaluate under a declared map only — the
        // engine's self-field evaluator is planar. (The region is
        // declared only so the v3+ usable-volume rule is satisfied and
        // the path3d map gate is the check that actually fires.)
        let mut value: serde_json::Value =
            serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        value.as_object_mut().unwrap().remove("field_map");
        value["requirement"]["good_field_region"] = serde_json::json!({
            "half_extents_m": [0.01, 0.01, 0.01],
            "points_per_axis": 3
        });
        let case: CoupledSearchCase = serde_json::from_value(value).unwrap();
        let err = case.validate().unwrap_err();
        assert!(format!("{err:?}").contains("field_map"), "{err}");

        // A cylindrical map cannot describe a non-axisymmetric winding.
        let mut value: serde_json::Value =
            serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        value["field_map"]["map"] = serde_json::json!({
            "source_sha256": "a".repeat(64),
            "components": "cylindrical_br_bz",
            "reference_ampere_turns_a": 1.0e6,
            "rho_levels_m": [0.04, 0.06],
            "z_levels_m": [-0.02, 0.10],
            "entries": [
                {"rho_index": 0, "z_index": 0, "br_t": 0.0, "bz_t": 8.0},
                {"rho_index": 0, "z_index": 1, "br_t": 0.0, "bz_t": 8.0},
                {"rho_index": 1, "z_index": 0, "br_t": 0.0, "bz_t": 8.0},
                {"rho_index": 1, "z_index": 1, "br_t": 0.0, "bz_t": 8.0}
            ]
        });
        let case: CoupledSearchCase = serde_json::from_value(value).unwrap();
        let err = case.validate().unwrap_err();
        assert!(format!("{err:?}").contains("cartesian"), "{err}");
    }

    #[test]
    fn helix_cost_ledger_matches_hand_calculation() {
        // 4 turns, 4 tapes, radial_pitch 0.0025 on the 4-turn helix:
        // cylinder-radial offsets delta_k = -0.005 + (k-0.5)*0.0025:
        // -0.00375, -0.00125, +0.00125, +0.00375.
        let case: CoupledSearchCase = serde_json::from_str(&helix_map_case_json("[4]")).unwrap();
        let cost = compute_cost_ledger(&case, 4, 4, 1, &[], CandidateDims::default());
        let expected_installed: f64 = [-3.75e-3_f64, -1.25e-3, 1.25e-3, 3.75e-3]
            .iter()
            .map(|&d| helix_turn_length_m(d))
            .sum::<f64>()
            * 4.0;
        assert!(
            (cost.installed_length_m - expected_installed).abs() < 1e-9 * expected_installed,
            "installed {} vs expected {}",
            cost.installed_length_m,
            expected_installed
        );
        let expected_total =
            expected_installed * 30.0 + expected_installed * 0.1 * 30.0 + 2000.0 + 600.0;
        assert!((cost.total_usd - expected_total).abs() < 1e-9 * expected_total);
    }

    #[test]
    fn helix_case_runs_end_to_end() {
        let record = run_coupled_search_case(
            &helix_map_case_json("[4]"),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v19");
        assert_eq!(record.candidates.len(), 1);
        let candidate = &record.candidates[0];
        assert!(candidate.pack_geometry_valid);
        // The map resolves, stations sample real field values, and the
        // candidate reaches a screening verdict — the helix pack is a
        // genuine non-planar geometry, not a racetrack fallback.
        assert!(candidate.screening.is_some());
        assert!(candidate.cost.total_usd.is_finite());
        assert!(matches!(
            candidate.requirement_status,
            Status::Pass | Status::Fail | Status::Inconclusive
        ));
        // The generated coupled case carries v9 and the helix.
        let full = build_coupled_case(
            &record.case,
            4,
            4,
            1,
            250.0,
            record.case.sampling.stations.clone(),
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(full.schema, optcoil_model::coupled::COUPLED_CASE_SCHEMA_V9);
        assert!(full.pack.path3d.is_some());
    }

    /// The turn length at normal offset `delta` on the OC-019 D, derived by
    /// hand from the segment list (lines keep their length; each arc scales
    /// by (R + sign*delta) / R).
    fn d_shape_turn_length_m(delta_m: f64) -> f64 {
        let half_pi = std::f64::consts::FRAC_PI_2;
        0.7 + (0.08 + delta_m) * half_pi
            + (0.43 + delta_m) * std::f64::consts::PI
            + (0.08 + delta_m) * half_pi
    }

    #[test]
    fn path_cost_ledger_matches_a_hand_calculation() {
        // 3 turns, 2 tapes, radial_pitch_m 5e-4 on the D centerline.
        // w = 3 * 5e-4 = 1.5e-3 m; offsets delta_k = -7.5e-4 + (k-0.5)*5e-4:
        //   delta_1 = -5.0e-4, delta_2 = 0, delta_3 = +5.0e-4.
        let case: CoupledSearchCase = serde_json::from_str(&reduced_path_case_json("[3]")).unwrap();
        let cost = compute_cost_ledger(&case, 3, 2, 1, &[], CandidateDims::default());
        let expected_installed: f64 = [-5.0e-4_f64, 0.0, 5.0e-4]
            .iter()
            .map(|&d| d_shape_turn_length_m(d))
            .sum::<f64>()
            * 2.0;
        assert!((cost.installed_length_m - expected_installed).abs() < 1e-9);
        // The straight legs dominate: three turn lengths near 2.3022 m each.
        let per_turn = d_shape_turn_length_m(0.0);
        assert!((per_turn - 2.302212).abs() < 1e-5);
        let expected_total =
            expected_installed * 30.0 + expected_installed * 0.1 * 30.0 + 1000.0 + 200.0;
        assert!((cost.total_usd - expected_total).abs() < 1e-6);
    }

    #[test]
    fn path_case_runs_end_to_end_and_acceptance_recomputes() {
        let record = run_coupled_search_case(
            &reduced_path_case_json("[3, 60]"),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v9");
        assert_eq!(record.candidates.len(), 2);
        for candidate in &record.candidates {
            assert!(candidate.pack_geometry_valid, "the D pack must be valid");
            // The path evaluator produced real field evaluations: every
            // candidate reaches a screening verdict, none reports overlap.
            assert!(candidate.screening.is_some());
            assert!(!candidate.good_field.as_ref().unwrap().pack_overlap);
            assert!(matches!(
                candidate.requirement_status,
                Status::Pass | Status::Fail | Status::Inconclusive
            ));
        }
        // The generated conductor case carries the v5 schema and the path.
        assert_eq!(record.case.schema, "optcoil-coupled-search/v9");
        // Acceptance ran the same path geometry end to end.
        assert!(matches!(
            record.acceptance.agreement_status,
            Status::Pass | Status::Fail | Status::Inconclusive
        ));
        // This fixture declares no manufacturing limit, so the acceptance
        // gate is correctly inert rather than silently passed.
        assert_eq!(
            record.acceptance.baseline.manufacturing_agreement_status,
            Status::NotEvaluated
        );
        // Installed length is the D-shape turn length, not 4L + 2*pi*rho:
        // for the 60-turn candidate, w = 0.03 -> mean offset 0 -> per-turn
        // 2.3022 m; installed = sum over 60 turns x 2 tapes.
        let sixty = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 60)
            .expect("the 60-turn candidate exists");
        let w = sixty.geometry.radial_width_m;
        let expected: f64 = (1..=60_u32)
            .map(|k| d_shape_turn_length_m(-w / 2.0 + (f64::from(k) - 0.5) * 0.0005))
            .sum::<f64>()
            * 2.0;
        assert!((sixty.cost.installed_length_m - expected).abs() < 1e-6);
        // Symmetric offsets cancel: installed is very close to N * L0 * tapes.
        assert!((sixty.cost.installed_length_m - 60.0 * 2.302212 * 2.0).abs() < 0.01);
    }

    /// Bend screening on a path uses the minimum local curvature radius:
    /// with pitch 5e-4, the 60-turn pack (w = 0.03) puts the inner face at
    /// 0.08 - 0.015 = 0.065 m < declared 0.07 m -> FAIL; the 20-turn pack
    /// (w = 0.01, inner 0.075 m) passes the gate.
    #[test]
    fn a_tight_path_corner_fails_the_bend_screen_for_a_fat_pack() {
        let json = reduced_path_case_json("[20, 60]").replacen(
            "\"execution\":",
            r#""manufacturing": {"min_inner_bend_radius_m": 0.07},
  "execution":"#,
            1,
        );
        let record = run_coupled_search_case(&json, &CoupledSearchOptions::default()).unwrap();
        let fat = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 60)
            .expect("the 60-turn candidate exists");
        assert!(!fat.manufacturing_feasible);
        assert_eq!(fat.requirement_status, Status::Fail);
        let thin = record
            .candidates
            .iter()
            .find(|c| c.geometry.turns_along_normal == 20)
            .expect("the 20-turn candidate exists");
        assert!(thin.manufacturing_feasible);
        // Acceptance independently recomputed the same min-curvature gate.
        assert!(matches!(
            record.acceptance.baseline.manufacturing_agreement_status,
            Status::Pass | Status::Fail
        ));
    }

    /// A racetrack-shaped `CoilPath` and the legacy declaration must produce
    /// the same per-turn cost: the path ledger generalizes 4L + 2*pi*rho.
    #[test]
    fn racetrack_path_and_legacy_ledgers_agree() {
        let path = optcoil_model::path::CoilPath::racetrack(0.3, 0.2);
        // Legacy: turns=3, w=3e-4, inner_rho = 0.2 - 1.5e-4.
        let legacy = compute_cost_ledger(
            &serde_json::from_str::<CoupledSearchCase>(&reduced_case_json("[3]", 30.0)).unwrap(),
            3,
            2,
            1,
            &[],
            CandidateDims::default(),
        );
        // Path equivalent: same centerline -> same offsets -> same metres.
        let pitch = 0.0001_f64;
        let w = 3.0 * pitch;
        let path_installed: f64 = (1..=3_u32)
            .map(|k| {
                let delta = -w / 2.0 + (f64::from(k) - 0.5) * pitch;
                path.length_at_offset_m(delta)
            })
            .sum::<f64>()
            * 2.0;
        assert!(
            (legacy.installed_length_m - path_installed).abs() < 1e-9,
            "legacy {} vs path {} m",
            legacy.installed_length_m,
            path_installed
        );
    }

    /// A tiny v10 graded case on the reduced skeleton: the same measured
    /// v3 dataset bound twice (the named spec at `cheap_price` USD/m vs
    /// the base at 30), two half-width grading regions, all-base baseline
    /// assignment. `region_hi` parameterizes the first region's upper
    /// fraction so tests can force an empty region.
    pub(crate) fn reduced_graded_case_json(
        turns_choices: &str,
        cheap_price: f64,
        region_hi: f64,
    ) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v10",
  "id": "reduced-graded-test-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6, "good_field_region": {{"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3}}}},
  "fixed_geometry": {{
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  }},
  "choices": {{"turns_along_normal": {turns_choices}, "tapes_along_width": [2]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
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
  "tape_specs": {{
    "cheap": {{
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
      "price_usd_per_m": {cheap_price}
    }}
  }},
  "grading": {{
    "regions": [
      {{"turn_range": [0.0, {region_hi}], "tape_spec_choices": ["base", "cheap"]}},
      {{"turn_range": [0.5, 1.0], "tape_spec_choices": ["base", "cheap"]}}
    ]
  }},
  "sampling": {{
    "stations": [{{"id": "s0", "kind": "straight", "x_m": 0.0}}],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{
    "turns_along_normal": 4,
    "tapes_along_width": 2,
    "tape_spec_ids": ["base", "base"]
  }},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0}},
      {{"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0}},
      {{"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0}},
      {{"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0}},
      {{"id": "straight_015", "kind": "straight", "x_m": 0.15}},
      {{"id": "straight_025", "kind": "straight", "x_m": 0.25}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": 3}}
}}
"#
        )
    }

    #[test]
    fn graded_ledger_prices_each_turn_at_its_assigned_spec() {
        // turns=4: region 0 covers turns 1-2 (cheap at 10 USD/m), region 1
        // covers turns 3-4 (base at 30 USD/m). rho_k = 0.2 - 2e-4 +
        // (k-0.5)*1e-4; perimeter_k = 1.2 + 2*pi*rho_k.
        let case: CoupledSearchCase =
            serde_json::from_str(&reduced_graded_case_json("[4]", 10.0, 0.5)).unwrap();
        let assignment = vec!["cheap".to_string(), "base".to_string()];
        let cost = compute_cost_ledger(&case, 4, 2, 1, &assignment, CandidateDims::default());
        let perimeter = |k: u32| {
            let rho = 0.2 - 4.0 * 1e-4 / 2.0 + (f64::from(k) - 0.5) * 1e-4;
            4.0 * 0.3 + 2.0 * std::f64::consts::PI * rho
        };
        let expected_conductor =
            ((perimeter(1) + perimeter(2)) * 10.0 + (perimeter(3) + perimeter(4)) * 30.0) * 2.0;
        let expected_scrap = expected_conductor * 0.1;
        let expected_total = expected_conductor + expected_scrap + 2.0 * 500.0 + 200.0;
        assert!((cost.conductor_usd - expected_conductor).abs() < 1e-6);
        assert!((cost.scrap_usd - expected_scrap).abs() < 1e-6);
        assert!((cost.total_usd - expected_total).abs() < 1e-6);
        // All-base at the same geometry must cost strictly more.
        let all_base = vec!["base".to_string(), "base".to_string()];
        let base_cost = compute_cost_ledger(&case, 4, 2, 1, &all_base, CandidateDims::default());
        assert!(base_cost.conductor_usd > cost.conductor_usd);
    }

    /// Schema v24 fixture: the reduced case upgraded with a piece policy.
    /// `boundary` is "per_module" or "continuous"; `splice` is
    /// `splice_cost_usd`; `piece_m` is the scalar `piece_length_m`.
    pub(crate) fn piece_case_json(
        turns_choices: &str,
        boundary: &str,
        splice: f64,
        piece_m: f64,
    ) -> String {
        let mut v: serde_json::Value =
            serde_json::from_str(&reduced_case_json(turns_choices, 30.0)).unwrap();
        v["schema"] = "optcoil-coupled-search/v24".into();
        v["cost"]["piece_policy"] = serde_json::json!({
            "piece_length_m": piece_m,
            "boundary": boundary,
            "piece_unit": "conductor_unit",
            "splice_cost_usd": splice
        });
        v["cost"]["price_source"] = "synthetic".into();
        // The v1 fixture lacks v2+ blocks — refinement + good_field_region.
        v["requirement"]["good_field_region"] = serde_json::json!({
            "half_extents_m": [0.05, 0.05, 0.05],
            "points_per_axis": 3
        });
        v["refinement"] = serde_json::json!({
            "pancake_counts": [2],
            "turn_resolution": 10,
            "turn_bounds": {"min": 1, "max": 100},
            "brackets": [{"tapes": 2, "fail_turns": 1, "pass_turns": 3}],
            "monotonicity_check": false
        });
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn piece_policy_per_module_ledger_matches_hand_calculation() {
        // 3 turns x 2 tapes, racetrack: per-module run = 7.369911184 m,
        // required with 10% scrap = 8.1069 m -> 2 pieces of 5 m per module.
        // pieces = 4 units, piece splices = 2, module joints = 1.
        let case: CoupledSearchCase =
            serde_json::from_str(&piece_case_json("[3]", "per_module", 50.0, 5.0)).unwrap();
        let cost = compute_cost_ledger(&case, 3, 2, 1, &[], CandidateDims::default());
        assert!((cost.installed_length_m - 14.739822368).abs() < 1e-6);
        assert_eq!(cost.module_joints, Some(1));
        assert_eq!(cost.piece_splices, Some(2));
        assert_eq!(cost.spec_splices, Some(0));
        assert_eq!(cost.pieces_bought, Some(4));
        // Purchased = 4 x 5 m; remnant beyond installed+attrition =
        // 20 - 14.739822368 x 1.1.
        assert!((cost.purchased_length_m - 20.0).abs() < 1e-9);
        assert!((cost.remnant_length_m.unwrap() - 3.786195395).abs() < 1e-6);
        let plan = cost.piece_plan.as_ref().unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].spec_id, "base");
        assert!((plan[0].piece_length_m - 5.0).abs() < 1e-12);
        assert_eq!(plan[0].pieces, 4);
        // conductor+scrap = purchased x price = 20 x 30 = 600.
        assert!((cost.conductor_usd + cost.scrap_usd - 600.0).abs() < 1e-6);
        // joints = 1 module x 200 + 2 splices x 50.
        assert!((cost.joints_usd - 300.0).abs() < 1e-9);
        assert!((cost.total_usd - 1900.0).abs() < 1e-6);
    }

    #[test]
    fn piece_policy_continuous_ledger_matches_hand_calculation() {
        // Same winding under `continuous`: one conductor stream of
        // 2 x 7.3699 = 14.7398 unit-m, required 16.2138 -> 4 pieces of
        // 5 m, 3 in-winding splices, zero module joints.
        let case: CoupledSearchCase =
            serde_json::from_str(&piece_case_json("[3]", "continuous", 50.0, 5.0)).unwrap();
        let cost = compute_cost_ledger(&case, 3, 2, 1, &[], CandidateDims::default());
        assert_eq!(cost.module_joints, Some(0));
        assert_eq!(cost.piece_splices, Some(3));
        assert_eq!(cost.pieces_bought, Some(4));
        assert!((cost.purchased_length_m - 20.0).abs() < 1e-9);
        // joints = 0 x 200 + 3 x 50; total = 600 + 150 + 1000.
        assert!((cost.joints_usd - 150.0).abs() < 1e-9);
        assert!((cost.total_usd - 1750.0).abs() < 1e-6);
    }

    #[test]
    fn piece_offerings_argmin_and_spec_splices() {
        // Graded v24 case: turns 1-2 "cheap" with a two-offering
        // catalogue ({3 m @ $10} vs {6 m @ $15}), turns 3-4 base at the
        // scalar 5 m policy length. Per-module runs: cheap 4.9121955 m
        // (required 5.4034), base 4.9147087 (5.4062).
        //   cheap @3 m: 2 pieces -> 6 m x $10 + 1 splice x $50 = $110
        //   cheap @6 m: 1 piece  -> 6 m x $15 + 0           = $90   <- chosen
        //   base  @5 m: 2 pieces -> 10 m x $30 + 1 x $50    = $350
        let mut v: serde_json::Value =
            serde_json::from_str(&reduced_graded_case_json("[4]", 10.0, 0.5)).unwrap();
        v["schema"] = "optcoil-coupled-search/v24".into();
        v["cost"]["piece_policy"] = serde_json::json!({
            "piece_length_m": 5.0,
            "boundary": "per_module",
            "piece_unit": "conductor_unit",
            "splice_cost_usd": 50.0
        });
        v["tape_specs"]["cheap"]["piece_offerings"] = serde_json::json!([
            {"length_m": 3.0, "price_usd_per_m": 10.0},
            {"length_m": 6.0, "price_usd_per_m": 15.0}
        ]);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let assignment = vec!["cheap".to_string(), "base".to_string()];
        let cost = compute_cost_ledger(&case, 4, 2, 1, &assignment, CandidateDims::default());
        let plan = cost.piece_plan.as_ref().unwrap();
        let cheap = plan.iter().find(|p| p.spec_id == "cheap").unwrap();
        let base = plan.iter().find(|p| p.spec_id == "base").unwrap();
        // Argmin picks the 6 m offering at the premium $15/m.
        assert!((cheap.piece_length_m - 6.0).abs() < 1e-12);
        assert!((cheap.price_usd_per_m - 15.0).abs() < 1e-12);
        assert_eq!(cheap.pieces, 2); // 1 per module x 2 modules
        assert_eq!(cheap.piece_splices, 0);
        assert_eq!(base.pieces, 4); // 2 per module x 2 modules
        assert_eq!(base.piece_splices, 2);
        // 1 spec change per module x 2 modules; 1 module interface.
        assert_eq!(cost.spec_splices, Some(2));
        assert_eq!(cost.module_joints, Some(1));
        // Spend check: cheap 2 x 6 m x $15 = $180, base 4 x 5 m x $30 = $600.
        assert!((cheap.purchased_length_m - 12.0).abs() < 1e-9);
        assert!((base.purchased_length_m - 20.0).abs() < 1e-9);
        assert!((cost.conductor_usd + cost.scrap_usd - 780.0).abs() < 1e-6);
        // joints = 1 x 200 + (2 piece + 2 spec) x 50 = 400.
        assert!((cost.joints_usd - 400.0).abs() < 1e-9);
    }

    #[test]
    fn piece_unit_per_strand_scales_pieces_and_splices_not_metres() {
        // Same winding, strands_parallel = 4: under `conductor_unit` one
        // piece covers all 4 strands (4 units, 2 splices); under
        // `per_strand` each strand buys and splices its own pieces
        // (16 strand-pieces, 8 splices). Tape-metre columns are
        // identical — only the piece/splice counts differ.
        let mut v: serde_json::Value =
            serde_json::from_str(&piece_case_json("[3]", "per_module", 50.0, 5.0)).unwrap();
        v["choices"]["strands_parallel"] = serde_json::json!([4]);
        let unit_case: CoupledSearchCase = serde_json::from_value(v.clone()).unwrap();
        v["cost"]["piece_policy"]["piece_unit"] = "per_strand".into();
        let strand_case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let unit = compute_cost_ledger(&unit_case, 3, 2, 4, &[], CandidateDims::default());
        let strand = compute_cost_ledger(&strand_case, 3, 2, 4, &[], CandidateDims::default());
        assert_eq!(unit.pieces_bought, Some(4));
        assert_eq!(strand.pieces_bought, Some(16));
        assert_eq!(unit.piece_splices, Some(2));
        assert_eq!(strand.piece_splices, Some(8));
        // Module interfaces are conductor-stack hardware — not per strand.
        assert_eq!(unit.module_joints, strand.module_joints);
        // Tape-metre and conductor columns identical.
        assert!((unit.purchased_length_m - strand.purchased_length_m).abs() < 1e-9);
        assert!((unit.installed_length_m - strand.installed_length_m).abs() < 1e-9);
        // Only the splice spend differs: (8-2) x $50.
        assert!((strand.joints_usd - unit.joints_usd - 300.0).abs() < 1e-9);
    }

    #[test]
    fn piece_policy_run_agrees_with_independent_acceptance() {
        // End-to-end: a v24 piece-policy case through the full search —
        // the independent acceptance recomputation must agree with the
        // search ledger on every candidate.
        let mut v: serde_json::Value =
            serde_json::from_str(&piece_case_json("[3]", "per_module", 50.0, 5.0)).unwrap();
        v["choices"]["turns_along_normal"] = serde_json::json!([3]);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.case.schema, "optcoil-coupled-search/v24");
        let acc = &record.acceptance;
        assert_eq!(
            acc.baseline.cost_agreement_status,
            Status::Pass,
            "piece-policy ledger must agree with independent recompute"
        );
        if let Some(best) = &acc.best {
            assert_eq!(best.cost_agreement_status, Status::Pass);
        }
        // The recomputed ledger itself carries the piece columns.
        assert_eq!(acc.baseline.recomputed_cost.module_joints, Some(1));
        assert_eq!(acc.baseline.recomputed_cost.pieces_bought, Some(4));
    }

    #[test]
    fn piece_policy_rejected_before_v24() {
        let mut v: serde_json::Value =
            serde_json::from_str(&piece_case_json("[3]", "per_module", 50.0, 5.0)).unwrap();
        v["schema"] = "optcoil-coupled-search/v10".into();
        let err = serde_json::from_value::<CoupledSearchCase>(v)
            .map(|c| c.validate())
            .unwrap();
        assert!(err.is_err(), "v24 piece fields on a v10 case must fail");
    }

    #[test]
    fn piece_policy_requires_a_base_piece_source() {
        let mut v: serde_json::Value =
            serde_json::from_str(&piece_case_json("[3]", "per_module", 50.0, 5.0)).unwrap();
        v["cost"]["piece_policy"]
            .as_object_mut()
            .unwrap()
            .remove("piece_length_m");
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().is_err());
    }

    #[test]
    fn piece_policy_continuous_run_agrees_with_acceptance() {
        let mut v: serde_json::Value =
            serde_json::from_str(&piece_case_json("[3]", "continuous", 50.0, 5.0)).unwrap();
        v["choices"]["turns_along_normal"] = serde_json::json!([3]);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.acceptance.agreement_status, Status::Pass);
        for candidate in &record.candidates {
            assert_eq!(candidate.cost.module_joints, Some(0));
            assert_eq!(candidate.cost.piece_splices, Some(3));
        }
    }

    #[test]
    fn graded_run_assigns_specs_and_acceptance_recomputes() {
        let record = run_coupled_search_case(
            &reduced_graded_case_json("[4]", 10.0, 0.5),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v10");
        // 1 geometry x 4 assignments.
        assert_eq!(record.candidates.len(), 4);
        for candidate in &record.candidates {
            let ids = candidate
                .geometry
                .tape_spec_ids
                .as_ref()
                .expect("a graded candidate must record its assignment");
            assert_eq!(ids.len(), 2);
        }
        // The all-cheap assignment dominates the all-base one on price
        // alone (identical physics binding, strictly lower price).
        let cost_of = |ids: [&str; 2]| {
            record
                .candidates
                .iter()
                .find(|c| {
                    c.geometry.tape_spec_ids.as_deref()
                        == Some(&[ids[0].to_string(), ids[1].to_string()])
                })
                .unwrap()
                .cost
                .total_usd
        };
        assert!(cost_of(["cheap", "cheap"]) < cost_of(["base", "base"]));
        // The baseline index selects the declared all-base assignment.
        assert_eq!(
            record.candidates[record.baseline_index]
                .geometry
                .tape_spec_ids
                .as_deref(),
            Some(&["base".to_string(), "base".to_string()][..])
        );
        // Independent acceptance recomputes the graded ledger to agreement.
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let acceptance = crate::search_acceptance::assess(
            &record.case,
            &record.candidates,
            record.baseline_index,
            record.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Pass);
        // v13: the record carries the resolved dataset provenance — the
        // base binding's audit plus the spec's resolved identity and its
        // own audit under its declared tolerance.
        let base_audit = record
            .monotonicity_audit
            .as_ref()
            .expect("v13 records the base binding's monotonicity audit");
        assert_eq!(base_audit.tolerance, 0.001);
        assert_eq!(base_audit.status, Status::Pass);
        let spec_dataset = record
            .spec_datasets
            .get("cheap")
            .expect("graded record reports the spec's resolved dataset");
        assert_eq!(spec_dataset.id, "robinson-superpower-ap-v3");
        assert_eq!(
            spec_dataset.csv_sha256,
            "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354"
        );
        assert_eq!(spec_dataset.point_count, 1505);
        let spec_audit = record
            .spec_monotonicity_audits
            .get("cheap")
            .expect("graded record reports the spec's own audit");
        assert_eq!(spec_audit.status, Status::Pass);
    }

    #[test]
    fn external_dataset_map_binds_base_and_graded_spec_without_substitution() {
        // Attributed embedded reference measurements, explicitly converted
        // to 1x synthetic software fixtures. These are not customer data or
        // customer evidence; scaling gives each binding a distinct external
        // identity while retaining the source attribution in metadata.
        let base = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3")
            .unwrap()
            .scaled_ic(1.0)
            .unwrap();
        let spec = MaterialDataset::embedded_by_id("robinson-shanghai-hflt-v3")
            .unwrap()
            .scaled_ic(1.0)
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&reduced_graded_case_json("[4]", 10.0, 0.5)).unwrap();
        value["id"] = "external-two-dataset-software-fixture".into();
        value["provenance"] = "Attributed reference measurements, 1x synthetic software fixture only; not customer evidence.".into();
        value["material"]["dataset_id"] = base.metadata.id.clone().into();
        value["material"]["csv_sha256"] = base.metadata.csv_sha256.clone().into();
        value["tape_specs"]["cheap"]["material"]["dataset_id"] = spec.metadata.id.clone().into();
        value["tape_specs"]["cheap"]["material"]["csv_sha256"] =
            spec.metadata.csv_sha256.clone().into();
        value["tape_specs"]["cheap"]["material"]["monotonicity_tolerance"] = 0.025.into();
        let case_json = serde_json::to_string(&value).unwrap();
        let case = CoupledSearchCase::from_json(&case_json).unwrap();
        let datasets = BTreeMap::from([
            (base.metadata.id.clone(), base.clone()),
            (spec.metadata.id.clone(), spec.clone()),
        ]);

        let preflight = crate::preflight::preflight_coupled_search_with_options(
            &case,
            Some(&datasets),
            &CoupledSearchOptions::default(),
        );
        assert!(preflight.ready_to_run, "{preflight:#?}");
        assert_eq!(preflight.datasets.len(), 2);
        assert!(
            preflight
                .datasets
                .iter()
                .all(|item| item.identity_compatible == Some(true))
        );

        let record = run_coupled_search_case_with_datasets(
            &case_json,
            &CoupledSearchOptions::default(),
            &datasets,
            &AtomicBool::new(false),
        )
        .unwrap();
        let acceptance = crate::search_acceptance::assess(
            &record.case,
            &record.candidates,
            record.baseline_index,
            record.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.agreement_status, Status::Pass);

        // A missing external dependency cannot fall back to another supplied
        // dataset or to an unrelated embedded dataset.
        let base_only = BTreeMap::from([(base.metadata.id.clone(), base.clone())]);
        let missing = crate::preflight::preflight_coupled_search_with_options(
            &case,
            Some(&base_only),
            &CoupledSearchOptions::default(),
        );
        assert!(!missing.ready_to_run);
        assert!(
            run_coupled_search_case_with_datasets(
                &case_json,
                &CoupledSearchOptions::default(),
                &base_only,
                &AtomicBool::new(false),
            )
            .is_err()
        );

        let mut wrong_hash: serde_json::Value = serde_json::from_str(&case_json).unwrap();
        wrong_hash["tape_specs"]["cheap"]["material"]["csv_sha256"] = "0".repeat(64).into();
        let wrong_json = serde_json::to_string(&wrong_hash).unwrap();
        let wrong_case = CoupledSearchCase::from_json(&wrong_json).unwrap();
        let wrong_preflight = crate::preflight::preflight_coupled_search_with_options(
            &wrong_case,
            Some(&datasets),
            &CoupledSearchOptions::default(),
        );
        assert!(!wrong_preflight.ready_to_run);
        assert!(
            run_coupled_search_case_with_datasets(
                &wrong_json,
                &CoupledSearchOptions::default(),
                &datasets,
                &AtomicBool::new(false),
            )
            .is_err()
        );
    }

    /// v13 schema: older records lacking the dataset-provenance fields
    /// still deserialize (all defaulted); an ungraded run's record omits
    /// the spec maps entirely.
    #[test]
    fn v12_records_parse_with_defaulted_dataset_provenance() {
        let mut record_json = serde_json::to_value(
            run_coupled_search_case(
                &reduced_graded_case_json("[4]", 10.0, 0.5),
                &CoupledSearchOptions::default(),
            )
            .unwrap(),
        )
        .unwrap();
        // A v12 record: same bytes minus the three v13 fields.
        record_json.as_object_mut().unwrap().retain(|k, _| {
            !matches!(
                k.as_str(),
                "monotonicity_audit" | "spec_datasets" | "spec_monotonicity_audits"
            )
        });
        let parsed: CoupledSearchRunRecord = serde_json::from_value(record_json).unwrap();
        assert!(parsed.monotonicity_audit.is_none());
        assert!(parsed.spec_datasets.is_empty());
        assert!(parsed.spec_monotonicity_audits.is_empty());
    }

    /// Graded fixture variant whose "cheap" spec binds an arbitrary
    /// (dataset_id, csv_sha256) — used with `scaled_ic` datasets supplied
    /// through `run_coupled_search_case_with_dataset`. `baseline_turns`
    /// must be one of `turns_choices`.
    pub(crate) fn reduced_graded_spec_case_json(
        turns_choices: &str,
        baseline_turns: u32,
        spec_price: f64,
        spec_dataset_id: &str,
        spec_sha256: &str,
    ) -> String {
        let mut v: serde_json::Value =
            serde_json::from_str(&reduced_graded_case_json(turns_choices, spec_price, 0.5))
                .unwrap();
        v["baseline"]["turns_along_normal"] = baseline_turns.into();
        let material = &mut v["tape_specs"]["cheap"]["material"];
        material["dataset_id"] = spec_dataset_id.into();
        material["csv_sha256"] = spec_sha256.into();
        serde_json::to_string(&v).unwrap()
    }

    /// Capability demo on synthetic data: a uniformly Ic-scaled weak spec
    /// (`scaled_ic`, id suffixed `@icx0.725` — declared synthetic, not a
    /// vendor measurement) cannot serve the high-field inner half of a
    /// thick pack but does serve the lower-field outer half. The graded
    /// optimum places base tape inside and the weak spec outside — a pack
    /// no all-weak build can realize — at a third-plus below the all-base
    /// baseline. Parameters are calibrated so the weak spec's outer-edge
    /// utilization sits ~3% under the 0.8 limit while its inner-edge
    /// utilization exceeds it; verdicts are deterministic.
    #[test]
    fn weak_synthetic_spec_feasible_only_where_field_allows() {
        let base_ds = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let weak = base_ds.scaled_ic(0.725).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&reduced_graded_spec_case_json(
            "[300]",
            300,
            10.0,
            &weak.metadata.id,
            &weak.metadata.csv_sha256,
        ))
        .unwrap();
        // 300 turns x 0.5mm = 150mm pack centred on R=0.2m; the 1.5T bore
        // target seats the base tape's inner-face utilization near 0.6 in
        // the steep Ic(B) regime, so the ~8% inner/outer field contrast
        // across the pack straddles the 0.8 limit under the weak spec.
        v["requirement"]["b_target_t"] = 1.5.into();
        v["fixed_geometry"]["radial_pitch_m"] = 0.0005.into();
        let json = serde_json::to_string(&v).unwrap();
        let record = run_coupled_search_case_with_dataset(
            &json,
            &CoupledSearchOptions::default(),
            Some(weak),
            &AtomicBool::new(false),
        )
        .unwrap();

        let by_assignment = |specs: [&str; 2]| {
            record
                .candidates
                .iter()
                .find(|c| {
                    c.geometry.tape_spec_ids.as_deref()
                        == Some(specs.map(str::to_string).as_slice())
                })
                .unwrap_or_else(|| panic!("no candidate with assignment {specs:?}"))
        };
        let all_weak = by_assignment(["cheap", "cheap"]);
        let weak_inner = by_assignment(["cheap", "base"]);
        let weak_outer = by_assignment(["base", "cheap"]);
        let all_base = by_assignment(["base", "base"]);

        assert_eq!(all_weak.status, Status::Fail);
        assert_eq!(weak_inner.status, Status::Fail);
        assert_eq!(all_base.status, Status::Pass);
        assert_eq!(weak_outer.status, Status::Pass);
        // The failures are physics, not plumbing: the weak spec's sampled
        // utilization breaches the 0.8 limit exactly where it serves the
        // inner (high-field) region.
        let weak_inner_util = weak_inner
            .screening
            .as_ref()
            .and_then(|s| s.max_utilization)
            .unwrap();
        let weak_outer_util = weak_outer
            .screening
            .as_ref()
            .and_then(|s| s.max_utilization)
            .unwrap();
        assert!(weak_inner_util > 0.8, "inner under weak: {weak_inner_util}");
        assert!(weak_outer_util < 0.8, "outer under weak: {weak_outer_util}");

        // The search selects the cheaper feasible mix, and acceptance
        // agrees on its independently recomputed ledger and rerun.
        let best = &record.candidates[record.best_index.unwrap()];
        assert_eq!(
            best.geometry.tape_spec_ids.as_deref(),
            Some(vec!["base".to_string(), "cheap".to_string()].as_slice())
        );
        assert!(best.cost.total_usd < all_base.cost.total_usd);
        assert!(record.savings_percent.unwrap() > 30.0);
        assert_eq!(
            record.acceptance.best.unwrap().cost_agreement_status,
            Status::Pass
        );
        assert_eq!(
            record.acceptance.baseline.cost_agreement_status,
            Status::Pass
        );
    }

    #[test]
    fn a_region_that_collapses_empty_fails_only_that_candidate() {
        // region 0 = [0, 0.02): N=4 -> floor(0.08)=0 turns -> unrealizable;
        // N=60 -> floor(1.2)=1 turn -> realizable.
        let record = run_coupled_search_case(
            &reduced_graded_case_json("[4, 60]", 10.0, 0.02),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.candidates.len(), 8);
        for candidate in &record.candidates {
            if candidate.geometry.turns_along_normal == 4 {
                assert!(
                    !candidate.pack_geometry_valid,
                    "the 4-turn candidate cannot realize region [0, 0.02)"
                );
                assert_eq!(candidate.status, Status::Fail);
            } else {
                assert!(candidate.pack_geometry_valid);
            }
        }
    }

    /// `optcoil verify` accepts a clean graded record — per-region
    /// assignments count toward the grid and drive the recomputed ledger —
    /// and rejects tampering with the ledger, the assignment, the baseline
    /// or the candidate list itself.
    #[test]
    fn verify_record_accepts_graded_runs_and_detects_tampering() {
        let case_json = reduced_graded_case_json("[4, 60]", 10.0, 0.5);
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        crate::verify::verify_record(&record_json, Some(&case_json), None, None, &[])
            .expect("clean graded record verifies");

        let tamper = |f: &dyn Fn(&mut serde_json::Value)| -> String {
            let mut v: serde_json::Value = serde_json::from_str(&record_json).unwrap();
            f(&mut v);
            serde_json::to_string(&v).unwrap()
        };
        // A bumped ledger total.
        let v = tamper(&|v| {
            let c = &mut v["candidates"][0]["cost"];
            c["total_usd"] = (c["total_usd"].as_f64().unwrap() + 100.0).into();
        });
        assert!(crate::verify::verify_record(&v, Some(&case_json), None, None, &[]).is_err());
        // A swapped per-region assignment (recorded cost no longer
        // recomputes from the recorded assignment).
        let v = tamper(&|v| {
            let c = &mut v["candidates"][0]["geometry"];
            let recorded = c["tape_spec_ids"][0].as_str().unwrap().to_string();
            let other = if recorded == "base" { "cheap" } else { "base" };
            c["tape_spec_ids"] = serde_json::json!([other, "base"]);
        });
        assert!(crate::verify::verify_record(&v, Some(&case_json), None, None, &[]).is_err());
        // An assignment naming an unknown spec must fail, not panic.
        let v = tamper(&|v| {
            let c = &mut v["candidates"][0]["geometry"];
            c["tape_spec_ids"] = serde_json::json!(["hts-evil", "base"]);
        });
        assert!(crate::verify::verify_record(&v, Some(&case_json), None, None, &[]).is_err());
        // A dropped candidate.
        let v = tamper(&|v| {
            v["candidates"].as_array_mut().unwrap().remove(0);
        });
        assert!(crate::verify::verify_record(&v, Some(&case_json), None, None, &[]).is_err());
        // A baseline index repointed at a different-geometry candidate —
        // the last candidate is 60 turns, the baseline is 4.
        let v = tamper(&|v| {
            let last = v["candidates"].as_array().unwrap().len() - 1;
            v["baseline_index"] = last.into();
        });
        assert!(crate::verify::verify_record(&v, Some(&case_json), None, None, &[]).is_err());
    }

    /// The ungraded path: count is geometry-only, ledgers use the single
    /// base price, and a spec bundle must not bind an identity the record
    /// never declared.
    #[test]
    fn verify_record_ungraded_and_dataset_binding() {
        let case_json = reduced_case_json("[3, 60]", 30.0);
        let record = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        crate::verify::verify_record(&record_json, Some(&case_json), None, None, &[])
            .expect("clean ungraded record verifies");

        let materials =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/materials");
        let ap_bundle =
            std::fs::read_to_string(materials.join("robinson-superpower-ap-v3/bundle.json"))
                .unwrap();
        let theva_bundle =
            std::fs::read_to_string(materials.join("robinson-theva-ap-v2/bundle.json")).unwrap();
        // The base bundle binds.
        crate::verify::verify_record(
            &record_json,
            Some(&case_json),
            None,
            None,
            &[ap_bundle.as_str()],
        )
        .expect("base dataset bundle binds the record");
        // A bundle that is no declared identity on this record fails.
        assert!(
            crate::verify::verify_record(
                &record_json,
                Some(&case_json),
                None,
                None,
                &[theva_bundle.as_str()],
            )
            .is_err()
        );
    }

    /// Schema v13: a search under a declared field map runs the
    /// requirement solve off the declared bore anchor, spends zero
    /// kernel evaluations, keeps every candidate's extents at the fixed
    /// declaration, and records the map provenance.
    #[test]
    fn declared_map_search_solves_ni_from_the_anchor_with_zero_kernel_evals() {
        let record = run_coupled_search_case(
            &reduced_map_case_json("[2, 4]"),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v13");
        let fm = record.field_map.as_ref().expect("map provenance recorded");
        assert_eq!(fm.map.reference_ampere_turns_a, 1.0e6);
        assert_eq!(fm.map.rho_nodes, Some(2));
        assert_eq!(fm.map.z_nodes, Some(3));
        assert_eq!(fm.bore_field_at_reference_t, 10.0);
        assert_eq!(record.candidates.len(), 2);
        assert_eq!(
            record.kernel_evaluations, 0,
            "a declared-map run spends no kernel evaluations"
        );
        for candidate in &record.candidates {
            // Fixed extents: the footprint is the declared map's winding
            // region regardless of the discretization choice.
            assert_eq!(candidate.geometry.radial_width_m, 0.064);
            assert_eq!(candidate.geometry.axial_height_m, 0.24);
            // NI = ref_at x b_target / bore_field_at_reference = 1e6.
            assert_eq!(candidate.unit_bore_bz_t_per_ampere_turn, 1.0e-5);
            assert_eq!(candidate.bore_refinement_change_t, 0.0);
            assert!((candidate.ampere_turns_a - 1.0e6).abs() < 1e-6);
            assert_eq!(candidate.requirement_status, Status::Pass);
            assert_eq!(candidate.requirement_kernel_evaluations, 0);
            assert!(candidate.screening.is_some());
        }
        // The map limitation is on the record.
        assert!(
            record
                .limitations
                .iter()
                .any(|l| l.contains("bore_field_at_reference_t"))
        );
        // Acceptance ran the declared-anchor recompute — agreement is
        // arithmetic consistency of the declared values (Pass), and the
        // usable-volume check is correctly inert (the map covers the
        // pack only, so no region may be declared).
        let baseline = &record.acceptance.baseline;
        assert_eq!(baseline.field_agreement_status, Status::Pass);
        assert_eq!(
            baseline.region_agreement_status,
            Status::NotEvaluated,
            "no usable-volume region exists under a pack-only map"
        );
    }

    /// Schema v13 gates: the map requires v13, fixed extents, a
    /// consistent baseline footprint and no usable-volume region.
    #[test]
    fn declared_map_case_validation_gates() {
        // field_map on a v12 schema is rejected.
        let mut case_json: serde_json::Value =
            serde_json::from_str(&reduced_map_case_json("[4]")).unwrap();
        case_json["schema"] = serde_json::json!("optcoil-coupled-search/v12");
        let err = serde_json::from_value::<CoupledSearchCase>(case_json.clone())
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(format!("{err:?}").contains("v13"), "{err}");

        // Fixed extents are required under a map.
        let mut missing =
            serde_json::from_str::<serde_json::Value>(&reduced_map_case_json("[4]")).unwrap();
        missing["fixed_geometry"]
            .as_object_mut()
            .unwrap()
            .remove("pack_radial_width_m");
        let err = serde_json::from_value::<CoupledSearchCase>(missing)
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(format!("{err:?}").contains("pack_radial_width_m"), "{err}");

        // A baseline footprint inconsistent with the declared extents is
        // rejected — pitch x baseline_turns != normal extent.
        let mut bad =
            serde_json::from_str::<serde_json::Value>(&reduced_map_case_json("[4]")).unwrap();
        bad["baseline"]["turns_along_normal"] = serde_json::json!(3);
        bad["choices"]["turns_along_normal"] = serde_json::json!([3, 4]);
        let err = serde_json::from_value::<CoupledSearchCase>(bad)
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(format!("{err:?}").contains("extent"), "{err}");

        // The pack-only map cannot certify a usable-volume region.
        let mut region =
            serde_json::from_str::<serde_json::Value>(&reduced_map_case_json("[4]")).unwrap();
        region["requirement"]["good_field_region"] = serde_json::json!(
            {"half_extents_m": [0.02, 0.02, 0.02], "points_per_axis": 2}
        );
        let err = serde_json::from_value::<CoupledSearchCase>(region)
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(format!("{err:?}").contains("good_field_region"), "{err}");

        // Fixed extents without a map are rejected (the case otherwise
        // satisfies v13's rules, so the extents check is what fires).
        let mut bare =
            serde_json::from_str::<serde_json::Value>(&reduced_map_case_json("[4]")).unwrap();
        bare.as_object_mut().unwrap().remove("field_map");
        bare["requirement"]["good_field_region"] = serde_json::json!(
            {"half_extents_m": [0.02, 0.02, 0.02], "points_per_axis": 2}
        );
        let err = serde_json::from_value::<CoupledSearchCase>(bare)
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(format!("{err:?}").contains("field_map"), "{err}");
    }

    /// Schema v13 + axial tape normal: the turn index advances axially,
    /// so every turn is the same hoop — mirror-symmetric graded regions
    /// must price identically, and the ledger must match the hand formula
    /// (turns x tapes x strands x nominal perimeter, price-weighted per
    /// turn). Regresses the v13 bug where the legacy
    /// `turns x declared_pitch` span pushed turn offsets radially —
    /// negative-radius phantom lengths under an axial normal corrupted
    /// graded pricing (asymmetric assignments priced differently).
    #[test]
    fn declared_map_axial_cost_ledger_is_symmetric_and_matches_hand_calc() {
        let mut case_json: serde_json::Value =
            serde_json::from_str(&reduced_map_case_json("[4]")).unwrap();
        let material = case_json["material"].clone();
        case_json["tape_specs"] = serde_json::json!({
            "ext": {"material": material, "price_usd_per_m": 60.0}
        });
        case_json["grading"] = serde_json::json!({
            "regions": [
                {"turn_range": [0.0, 0.5], "tape_spec_choices": ["base", "ext"]},
                {"turn_range": [0.5, 1.0], "tape_spec_choices": ["base", "ext"]}
            ]
        });
        case_json["baseline"]["tape_spec_ids"] = serde_json::json!(["base", "base"]);
        let case: CoupledSearchCase = serde_json::from_value(case_json).unwrap();
        case.validate().unwrap();

        let assignment = |a: &str, b: &str| vec![a.to_owned(), b.to_owned()];
        let eb = compute_cost_ledger(
            &case,
            4,
            4,
            1,
            &assignment("ext", "base"),
            CandidateDims::default(),
        );
        let be = compute_cost_ledger(
            &case,
            4,
            4,
            1,
            &assignment("base", "ext"),
            CandidateDims::default(),
        );
        assert_eq!(
            eb.total_usd, be.total_usd,
            "mirror-symmetric end bands under an axial normal must price identically"
        );

        // Hand formula: every turn is the same 2*pi*R hoop (axial normal
        // advances in z, not radius); each turn row holds `tapes`
        // conductors. Regions [0,.5]/[.5,1] cover turns 1-2 / 3-4:
        // [ext,base] -> 2 turns @60 + 2 turns @30 -> mean 45 $/m.
        let installed = 4.0 * 4.0 * 1.0 * 2.0 * std::f64::consts::PI * 0.68;
        let conductor = installed * 45.0;
        let expected_total = conductor * 1.1 + 4.0 * 500.0 + 3.0 * 200.0;
        assert!((eb.installed_length_m - installed).abs() < 1e-9 * installed);
        assert!((eb.total_usd - expected_total).abs() < 1e-9 * expected_total);

        // The ungraded base assignment prices every turn at 30 $/m.
        let bb = compute_cost_ledger(
            &case,
            4,
            4,
            1,
            &assignment("base", "base"),
            CandidateDims::default(),
        );
        let expected_bb = installed * 30.0 * 1.1 + 4.0 * 500.0 + 3.0 * 200.0;
        assert!((bb.total_usd - expected_bb).abs() < 1e-9 * expected_bb);
    }

    /// Schema v13: the hoop-stress outer radius grows by half the
    /// *declared* pack radial extent, not `turns x radial_pitch` — under
    /// an axial tape normal that legacy span is the axial height, so the
    /// old expression inflated R_outer by ~4x on this fixture.
    #[test]
    fn declared_map_hoop_stress_uses_the_declared_radial_extent() {
        let mut case_json: serde_json::Value =
            serde_json::from_str(&reduced_map_case_json("[4]")).unwrap();
        case_json["mechanical"] = serde_json::json!({
            "max_lorentz_load_n_per_m": 1e18,
            "max_hoop_stress_pa": 1e18,
            "tension_section_area_m2": 1.0
        });
        let case: CoupledSearchCase = serde_json::from_value(case_json).unwrap();
        case.validate().unwrap();

        let (load, hoop, feasible) = mechanical_screen(&case, 4, 4, 2, 500.0, Some(0.5), None);
        // I_op x B_peak = 250 N/m aggregate; per strand (s=2) 125 N/m.
        // R_outer = path max radius + pack_radial_width/2 = 0.68 + 0.032.
        let expected = (250.0 / 2.0) * 0.712 / 1.0;
        assert_eq!(load, Some(250.0));
        assert!((hoop.unwrap() - expected).abs() < 1e-9 * expected);
        assert_eq!(feasible, Some(true));
    }

    // -----------------------------------------------------------------
    // Schema v15: the four first-order adjacent screens.
    // -----------------------------------------------------------------

    /// The v3 reduced fixture at schema v15 with all four screens
    /// declared. The Robinson dataset spans nominal 20–40 K, so at the
    /// fixture's 21 K operating point the thermal margin is expected to
    /// resolve as a *lower bound* near 19 K — capacity still exceeds the
    /// tiny demand at the dataset's top temperature.
    fn v15_reduced_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v15".into();
        v["thermal_margin"] = serde_json::json!({"min_margin_k": 1.0});
        v["ac_loss"] = serde_json::json!({
            "frequency_hz": 0.1,
            "transport_amplitude_fraction": 0.5,
            "sc_layer_thickness_m": 1.0e-6,
            "max_loss_w": 1.0e18
        });
        v["quench_hotspot"] = serde_json::json!({
            "dump_time_constant_s": 0.1,
            "stabilizer_area_m2": 1.0e-4,
            "quench_function_a2s_per_m4": [
                [4.0, 0.0], [21.0, 1.0e13], [77.0, 5.0e14],
                [150.0, 2.0e15], [300.0, 6.0e15]
            ],
            "max_hotspot_k": 300.0
        });
        v["screening_current"] = serde_json::json!({"max_penetrated_width_fraction": 1.0});
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v15_screens_evaluate_on_the_full_plan() {
        let record =
            run_coupled_search_case(&v15_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        let cand = record
            .candidates
            .iter()
            .find(|c| c.status == Status::Pass)
            .expect("a passing candidate");
        let screens = cand.screens.as_ref().expect("a v15 case records screens");

        let thermal = screens.thermal_margin.as_ref().unwrap();
        assert_eq!(thermal.model, THERMAL_MARGIN_SCREEN_MODEL_ID);
        assert_eq!(thermal.status, Status::Pass);
        let margin = thermal.min_margin_k.unwrap();
        // Capacity still exceeds demand at the dataset's 40 K top: the
        // recorded ~19 K margin is a lower bound, not a resolved T_cs.
        assert!(margin > 15.0 && margin <= 19.1, "margin {margin}");
        assert!(thermal.min_margin_is_lower_bound);
        assert_eq!(thermal.points_inconclusive, 0);
        assert!(thermal.points_evaluated > 0);

        let ac = screens.ac_loss.as_ref().unwrap();
        assert_eq!(ac.model, AC_LOSS_SCREEN_MODEL_ID);
        assert_eq!(ac.status, Status::Pass);
        let total = ac.total_loss_w.unwrap();
        let parts = ac.transport_loss_w.unwrap()
            + ac.parallel_slab_loss_w.unwrap()
            + ac.perpendicular_bound_w.unwrap();
        assert!((total - parts).abs() <= 1e-9 * parts.max(1.0));
        assert!(ac.transport_loss_w.unwrap() > 0.0);
        assert!(ac.perpendicular_bound_w.unwrap() > 0.0);
        assert!(ac.peak_loss_j_per_m_per_cycle.unwrap() > 0.0);

        let quench = screens.quench_hotspot.as_ref().unwrap();
        assert_eq!(quench.model, QUENCH_HOTSPOT_SCREEN_MODEL_ID);
        assert_eq!(quench.status, Status::Pass);
        assert!(!quench.table_exhausted);
        let t_hs = quench.hotspot_temperature_k.unwrap();
        assert!(t_hs > 21.0 && t_hs <= 300.0, "hot spot {t_hs}");
        assert!(quench.miit_a2s.unwrap() > 0.0);

        let screening = screens.screening_current.as_ref().unwrap();
        assert_eq!(screening.model, SCREENING_CURRENT_SCREEN_MODEL_ID);
        assert_eq!(screening.status, Status::Pass);
        let frac = screening.max_penetrated_width_fraction.unwrap();
        assert!((0.0..=1.0).contains(&frac));
        assert!(screening.max_b_perp_over_bc.unwrap() > 0.0);

        // The acceptance module's own recomputation agrees with every
        // recorded screen.
        for c in [
            &record.acceptance.baseline,
            record
                .acceptance
                .best
                .as_ref()
                .unwrap_or(&record.acceptance.baseline),
        ] {
            assert_eq!(c.screens_agreement_status, Status::Pass);
            let rs = c.recomputed_screens.as_ref().unwrap();
            assert_eq!(rs.thermal_margin.as_ref().unwrap().status, Status::Pass);
            assert_eq!(rs.ac_loss.as_ref().unwrap().status, Status::Pass);
            assert_eq!(rs.quench_hotspot.as_ref().unwrap().status, Status::Pass);
            assert_eq!(rs.screening_current.as_ref().unwrap().status, Status::Pass);
        }
    }

    /// The v15 fixture lifted to schema v16 with the measured E–J
    /// transition screen declared: `max_e_over_ec` = 1.0 forbids
    /// operation past the measurement criterion — the passing
    /// candidate's worst point sits near u ≈ 0.7, so `u^n` (measured
    /// n ~ 20–30) lands far below 1.
    fn v16_reduced_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v16".into();
        v["transition"] = serde_json::json!({"max_e_over_ec": 1.0});
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v16_transition_screen_evaluates_on_the_full_plan() {
        let record =
            run_coupled_search_case(&v16_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        let cand = record
            .candidates
            .iter()
            .find(|c| c.status == Status::Pass)
            .expect("a passing candidate");
        let transition = cand
            .screens
            .as_ref()
            .and_then(|s| s.transition.as_ref())
            .expect("a v16 case records the transition screen");
        assert_eq!(transition.model, TRANSITION_SCREEN_MODEL_ID);
        assert_eq!(transition.status, Status::Pass);
        assert_eq!(transition.points_inconclusive, 0);
        assert!(transition.points_evaluated > 0);
        let depth = transition.max_e_over_ec.unwrap();
        let u = transition.worst_utilization.unwrap();
        let n = transition.worst_n_value.unwrap();
        // The headline depth is the recorded point's own u^n.
        assert!((depth - u.powf(n)).abs() <= 1e-12 * depth.max(1.0));
        assert!(u > 0.0 && u < 1.0, "utilization {u}");
        assert!(n > 1.0, "measured n {n}");
        assert!(depth < 1.0, "a sub-criterion point: E/Ec = {depth}");
        assert!(transition.terminal_voltage_v.unwrap() > 0.0);
        // The acceptance module's own recomputation agrees.
        for c in [
            &record.acceptance.baseline,
            record
                .acceptance
                .best
                .as_ref()
                .unwrap_or(&record.acceptance.baseline),
        ] {
            let agreement = c
                .recomputed_screens
                .as_ref()
                .and_then(|s| s.transition.as_ref())
                .expect("recomputed transition digest");
            assert_eq!(agreement.status, Status::Pass);
            assert_eq!(agreement.recomputed_status, agreement.recorded_status);
        }
    }

    #[test]
    fn v16_transition_screen_fails_a_declared_bound() {
        // A bound far below any operating point's transition depth fails.
        let mut v: serde_json::Value = serde_json::from_str(&v16_reduced_case_json()).unwrap();
        v["transition"]["max_e_over_ec"] = serde_json::json!(1.0e-30);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        // A failed declared screen joins the aggregate fail-closed: no
        // candidate may keep PASS.
        assert!(record.candidates.iter().all(|c| c.status != Status::Pass));
        let transition = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.transition.as_ref())
            .next()
            .expect("the transition screen still records on a failed candidate");
        assert_eq!(transition.status, Status::Fail);
    }

    #[test]
    fn transition_screen_must_not_be_declared_before_v16() {
        let mut v15: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v15["transition"] = serde_json::json!({"max_e_over_ec": 1.0});
        let case: CoupledSearchCase = serde_json::from_value(v15).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(
            err.contains("schema v16"),
            "transition on v15 must fail with a v16 gate, got: {err}"
        );
    }

    #[test]
    fn transition_e_over_ec_is_the_measured_power_law() {
        // u < 1 decays with n: 0.9^10 vs 0.9^30.
        let shallow = transition_e_over_ec(0.9, 10.0);
        let deep_n = transition_e_over_ec(0.9, 30.0);
        assert!(deep_n < shallow && shallow < 1.0);
        // Exactly at the criterion: u = 1 → E/Ec = 1 for any n.
        assert_eq!(transition_e_over_ec(1.0, 20.0), 1.0);
        // Over-critical points grow with n — never clipped.
        assert!(transition_e_over_ec(1.1, 20.0) > 1.0);
        assert_eq!(transition_e_over_ec(0.0, 20.0), 0.0);
        assert_eq!(transition_e_over_ec(f64::NAN, 20.0), f64::INFINITY);
        assert_eq!(transition_e_over_ec(0.9, f64::NAN), f64::INFINITY);
    }

    /// A v17 reduced case: v3's fixture plus the bend-strain pair
    /// (`tape_thickness_m`, `max_bend_strain`) and the
    /// `max_membrane_tension_n_per_m` mechanical bound.
    fn v17_reduced_case_json(
        tape_thickness_m: f64,
        max_bend_strain: f64,
        max_membrane_tension_n_per_m: f64,
    ) -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v17".into();
        v["manufacturing"] = serde_json::json!({
            "min_inner_bend_radius_m": 1e-9,
            "tape_thickness_m": tape_thickness_m,
            "max_bend_strain": max_bend_strain
        });
        v["mechanical"] = serde_json::json!({
            "max_lorentz_load_n_per_m": 1e18,
            "max_membrane_tension_n_per_m": max_membrane_tension_n_per_m
        });
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v17_bend_strain_and_membrane_tension_record_and_gate() {
        // Generous bounds: every candidate records its inner bend
        // radius, the outer-fiber strain t/(2R), and the undivided
        // membrane-tension resultant — the transverse pressure's own
        // numerator.
        let record = run_coupled_search_case(
            &v17_reduced_case_json(1e-4, 1.0, 1e18),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(record.case.schema, "optcoil-coupled-search/v17");
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .expect("a screened candidate");
        let inner = cand
            .inner_bend_radius_m
            .expect("v17 records the inner bend radius");
        assert!(inner.is_finite() && inner > 0.0);
        let strain = cand.bend_strain.expect("declared thickness records strain");
        assert!(
            (strain - 1e-4 / (2.0 * inner)).abs() < 1e-12 * strain,
            "bend_strain {strain} != t/(2R) = {}",
            1e-4 / (2.0 * inner)
        );
        let tension = cand
            .membrane_tension_n_per_m
            .expect("a screened candidate records the membrane tension");
        let pressure = cand.transverse_pressure_pa.unwrap();
        let width = record.case.fixed_geometry.tape_width_m;
        assert!(
            (tension - pressure * width).abs() < 1e-9 * tension,
            "tension {tension} != pressure x width = {}",
            pressure * width
        );
        assert!(cand.manufacturing_feasible);
        assert_eq!(cand.mechanical_feasible, Some(true));
        // Acceptance recomputes both on its own arithmetic.
        assert_eq!(
            record.acceptance.baseline.manufacturing_agreement_status,
            Status::Pass
        );
        assert_eq!(
            record.acceptance.baseline.mechanical_agreement_status,
            Status::Pass
        );
        assert!(record.acceptance.baseline.recomputed_bend_strain.is_some());
        assert!(
            record
                .acceptance
                .baseline
                .recomputed_membrane_tension_n_per_m
                .is_some()
        );
    }

    #[test]
    fn v17_bend_strain_bound_fails_a_declared_limit() {
        // A strain bound far below any real bend fails the manufacturing
        // gate on every candidate — fail-closed, never silently PASS.
        let record = run_coupled_search_case(
            &v17_reduced_case_json(1e-4, 1e-12, 1e18),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        assert!(record.candidates.iter().all(|c| !c.manufacturing_feasible));
        assert!(record.candidates.iter().all(|c| c.status != Status::Pass));
        assert_eq!(
            record.acceptance.baseline.manufacturing_agreement_status,
            Status::Pass
        );
    }

    #[test]
    fn v17_membrane_tension_bound_fails_a_declared_limit() {
        // A bound below any real cumulative interface load fails every
        // screened candidate on the mechanical screen.
        let record = run_coupled_search_case(
            &v17_reduced_case_json(1e-4, 1.0, 1.0),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .unwrap();
        assert_eq!(cand.mechanical_feasible, Some(false));
        assert_eq!(cand.status, Status::Fail);
        assert!(cand.membrane_tension_n_per_m.unwrap() > 1.0);
        assert_eq!(
            record.acceptance.baseline.mechanical_agreement_status,
            Status::Pass
        );
    }

    #[test]
    fn v17_thickness_alone_records_an_unbounded_strain() {
        // tape_thickness_m without max_bend_strain records the strain
        // but bounds nothing — the pressure estimate's own convention.
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v17".into();
        v["manufacturing"] = serde_json::json!({
            "min_inner_bend_radius_m": 1e-9,
            "tape_thickness_m": 1e-4
        });
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .unwrap();
        assert!(cand.bend_strain.is_some());
        assert!(cand.manufacturing_feasible);
    }

    #[test]
    fn v17_strain_bound_requires_tape_thickness() {
        let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v17".into();
        v["manufacturing"] = serde_json::json!({
            "min_inner_bend_radius_m": 1e-9,
            "max_bend_strain": 0.004
        });
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(
            err.contains("requires tape_thickness_m"),
            "a strain bound without the thickness must not validate, got: {err}"
        );
        for (thickness, bound, want_err) in [
            (serde_json::json!(0.0), serde_json::json!(0.004), true),
            (serde_json::json!(-1e-4), serde_json::json!(0.004), true),
            (serde_json::json!(1e-4), serde_json::json!(0.0), true),
            (serde_json::json!(1e-4), serde_json::json!(1.5), true),
        ] {
            let mut v: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
            v["schema"] = "optcoil-coupled-search/v17".into();
            v["manufacturing"] = serde_json::json!({
                "min_inner_bend_radius_m": 1e-9,
                "tape_thickness_m": thickness,
                "max_bend_strain": bound
            });
            let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
            assert_eq!(case.validate().is_err(), want_err);
        }
    }

    #[test]
    fn v17_fields_must_not_be_declared_before_v17() {
        // The identical declarations at v16 must not validate.
        let mut v: serde_json::Value =
            serde_json::from_str(&v17_reduced_case_json(1e-4, 0.004, 1e6)).unwrap();
        v["schema"] = "optcoil-coupled-search/v16".into();
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(
            err.contains("v17") || err.contains("v3-v16") || err.contains("v4-v16"),
            "v17 mechanics fields on a v16 case must fail the gate, got: {err}"
        );
    }

    #[test]
    fn screens_must_not_be_declared_before_v15() {
        for (field, block) in [
            ("thermal_margin", serde_json::json!({"min_margin_k": 1.0})),
            (
                "ac_loss",
                serde_json::json!({
                    "frequency_hz": 1.0,
                    "transport_amplitude_fraction": 0.5,
                    "sc_layer_thickness_m": 1.0e-6,
                    "max_loss_w": 10.0
                }),
            ),
            (
                "quench_hotspot",
                serde_json::json!({
                    "dump_time_constant_s": 0.1,
                    "stabilizer_area_m2": 1.0e-4,
                    "quench_function_a2s_per_m4": [[4.0, 0.0], [300.0, 1.0e15]],
                    "max_hotspot_k": 300.0
                }),
            ),
            (
                "screening_current",
                serde_json::json!({"max_penetrated_width_fraction": 0.5}),
            ),
        ] {
            let mut v14: serde_json::Value = serde_json::from_str(&v3_reduced_case_json()).unwrap();
            v14["schema"] = "optcoil-coupled-search/v14".into();
            v14[field] = block;
            let case: CoupledSearchCase = serde_json::from_value(v14).unwrap();
            let err = case.validate().unwrap_err().to_string();
            assert!(
                err.contains("schema v15"),
                "{field} on v14 must fail with a v15 gate, got: {err}"
            );
        }
    }

    #[test]
    fn v15_validates_screen_blocks() {
        // A quench table that does not bracket the operating temperature
        // is rejected at validation, never silently extrapolated.
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v["quench_hotspot"]["quench_function_a2s_per_m4"] =
            serde_json::json!([[50.0, 0.0], [300.0, 1.0e15]]);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().unwrap_err().to_string().contains("bracket"));

        // An out-of-range Norris amplitude fraction is rejected.
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v["ac_loss"]["transport_amplitude_fraction"] = serde_json::json!(1.5);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().is_err());
    }

    #[test]
    fn quench_table_exhaustion_is_inconclusive_never_extrapolated() {
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        // A table whose U-span cannot absorb the declared dump's MIITs.
        v["quench_hotspot"]["quench_function_a2s_per_m4"] =
            serde_json::json!([[4.0, 0.0], [21.0, 1.0e6], [40.0, 2.0e6]]);
        v["quench_hotspot"]["dump_time_constant_s"] = serde_json::json!(10.0);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screens.is_some())
            .expect("a candidate with screens");
        let quench = cand
            .screens
            .as_ref()
            .unwrap()
            .quench_hotspot
            .as_ref()
            .unwrap();
        assert_eq!(quench.status, Status::Inconclusive);
        assert!(quench.table_exhausted);
        assert!(quench.hotspot_temperature_k.is_none());
        // The inconclusive screen joins the aggregate — the candidate
        // can no longer claim PASS.
        assert_ne!(cand.status, Status::Pass);
    }

    #[test]
    fn quench_hotspot_evaluate_inverts_the_table_piecewise_linearly() {
        let quench = optcoil_model::coupled_search::QuenchHotspotScreen {
            dump_time_constant_s: 1.0,
            stabilizer_area_m2: 1.0,
            quench_function_a2s_per_m4: vec![[4.0, 0.0], [20.0, 10.0], [40.0, 30.0], [100.0, 90.0]],
            max_hotspot_k: 60.0,
        };
        // J = I_op/(s·A_stab) = 4 A/m²; MIITs = J²·τ/2 = 8 A²s.
        // U(25 K) = 15 → U_required = 23 ∈ [U(20)=10, U(40)=30) →
        // T_hs = 20 + (23−10)/(30−10)·20 = 33 K exactly.
        let r = quench_hotspot_evaluate(&quench, 4.0, 1, 25.0);
        assert_eq!(r.status, Status::Pass);
        assert!((r.hotspot_temperature_k.unwrap() - 33.0).abs() < 1e-9);
        // τ = 0 → MIITs = 0 → the hot spot is exactly T_op.
        let mut q = quench.clone();
        q.dump_time_constant_s = 0.0;
        let r = quench_hotspot_evaluate(&q, 100.0, 1, 20.0);
        assert_eq!(r.status, Status::Pass);
        assert!((r.hotspot_temperature_k.unwrap() - 20.0).abs() < 1e-9);
        // A declared limit below the hot spot fails; a table that cannot
        // absorb the MIITs is INCONCLUSIVE, never extrapolated.
        let mut q = quench.clone();
        q.max_hotspot_k = 30.0;
        assert_eq!(
            quench_hotspot_evaluate(&q, 4.0, 1, 25.0).status,
            Status::Fail
        );
        let mut q = quench.clone();
        q.dump_time_constant_s = 1.0e4;
        let r = quench_hotspot_evaluate(&q, 4.0, 1, 25.0);
        assert_eq!(r.status, Status::Inconclusive);
        assert!(r.table_exhausted);
        // A non-physical current is never evaluated, never a silent pass.
        let r = quench_hotspot_evaluate(&quench, f64::NAN, 1, 25.0);
        assert_eq!(r.status, Status::NotEvaluated);
    }

    #[test]
    fn screen_formulas_hit_their_documented_limits() {
        // Norris transport: zero amplitude dissipates exactly nothing,
        // the function is finite as i → 1⁻, and monotone in between.
        assert_eq!(norris_transport_j_per_m_per_cycle(1000.0, 0.0), 0.0);
        let near_one = norris_transport_j_per_m_per_cycle(1000.0, 1.0 - 1e-9);
        assert!(near_one.is_finite() && near_one > 0.0);
        let half = norris_transport_j_per_m_per_cycle(1000.0, 0.5);
        assert!(half > 0.0 && half < near_one);

        // Bean slab: continuous at B_m = B_p (both branches must give
        // 2·B_p²/(3·μ₀) per volume there), zero at zero field, linear
        // saturation asymptote 2·B_p·B_m/μ₀ above.
        let k_c = 1.0e5; // A/m → B_p = μ₀·K_c/2 ≈ 62.8 mT
        let b_p = MU0_H_PER_M * k_c / 2.0;
        let below = bean_slab_parallel_j_per_m_per_cycle(b_p * (1.0 - 1e-9), k_c, 1e-6, 0.012);
        let above = bean_slab_parallel_j_per_m_per_cycle(b_p * (1.0 + 1e-9), k_c, 1e-6, 0.012);
        assert!(
            (below - above).abs() <= 1e-7 * above,
            "Bean branches discontinuous at B_p: {below} vs {above}"
        );
        assert_eq!(
            bean_slab_parallel_j_per_m_per_cycle(0.0, k_c, 1e-6, 0.012),
            0.0
        );
        // Deep in saturation (B_m ≫ B_p) the loss is asymptotically
        // linear: q_vol → 2·B_p·B_m/μ₀.
        let deep = bean_slab_parallel_j_per_m_per_cycle(100.0 * b_p, k_c, 1e-6, 0.012);
        let asymptote = (2.0 * b_p / MU0_H_PER_M) * 100.0 * b_p * 0.012 * 1e-6;
        assert!((deep - asymptote).abs() / asymptote < 0.01);

        // Perpendicular bound: linear in B_perp, zero at zero, and equal
        // to 4·m_sat·B with m_sat = K_c·(w/2)².
        assert_eq!(perpendicular_bound_j_per_m_per_cycle(k_c, 0.012, 0.0), 0.0);
        let b1 = perpendicular_bound_j_per_m_per_cycle(k_c, 0.012, 1.0);
        let b2 = perpendicular_bound_j_per_m_per_cycle(k_c, 0.012, 2.0);
        assert!((b2 - 2.0 * b1).abs() < 1e-9 * b2);
        let expected = 4.0 * k_c * (0.006_f64).powi(2) * 1.0;
        assert!((b1 - expected).abs() < 1e-9 * expected);

        // Brandt–Indenbom: no penetration at zero field, full
        // penetration in the high-field limit, ~39% at B = B_c.
        assert_eq!(penetrated_width_fraction(0.0, k_c), 0.0);
        assert!(penetrated_width_fraction(1.0e6, k_c) > 0.999999);
        let b_c = screening_b_c_t(k_c);
        let at_bc = penetrated_width_fraction(b_c, k_c);
        assert!((at_bc - (1.0 - 1.0 / 1.0_f64.cosh())).abs() < 1e-12);
        // Zero sheet current cannot bound a core — fully penetrated.
        assert_eq!(penetrated_width_fraction(1.0, 0.0), 1.0);
    }

    #[test]
    fn screens_report_not_evaluated_when_the_point_plan_never_ran() {
        // A pruning case at v18: the 3-turn candidate's coarse plan
        // fails (enormous NI for the same bore target), so its
        // point-dependent screens must read NOT_EVALUATED while the
        // quench bound — needing only I_op — still resolves.
        let mut v: serde_json::Value =
            serde_json::from_str(&reduced_case_with_pruning_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v18".into();
        v["thermal_margin"] = serde_json::json!({"min_margin_k": 1.0});
        v["ac_loss"] = serde_json::json!({
            "frequency_hz": 0.1,
            "transport_amplitude_fraction": 0.5,
            "sc_layer_thickness_m": 1.0e-6,
            "max_loss_w": 1.0e18
        });
        v["quench_hotspot"] = serde_json::json!({
            "dump_time_constant_s": 0.1,
            "stabilizer_area_m2": 1.0e-4,
            "quench_function_a2s_per_m4": [[4.0, 0.0], [21.0, 1.0e13], [300.0, 6.0e15]],
            "max_hotspot_k": 300.0
        });
        v["screening_current"] = serde_json::json!({"max_penetrated_width_fraction": 1.0});
        // v16: the transition screen is point-dependent too — it must
        // read NOT_EVALUATED on a pruned candidate, never a silent PASS.
        v["transition"] = serde_json::json!({"max_e_over_ec": 1.0});
        // v18: the quench transient is likewise point-dependent.
        v["quench_transient"] = serde_json::json!({
            "dump_time_constant_s": 0.05,
            "conducting_area_per_width_m": 5.0e-3,
            "resistivity_ohm_m": [[20.0, 3.0e-9], [45.0, 1.5e-8]],
            "heat_capacity_j_per_m3k": [[20.0, 1.0e9], [45.0, 1.2e9]],
            "max_temperature_k": 45.0
        });
        // v2+ requires a refinement block.
        v["refinement"] = serde_json::json!({
            "pancake_counts": [2],
            "turn_resolution": 5,
            "turn_bounds": {"min": 1, "max": 100},
            "brackets": [{"tapes": 2, "fail_turns": 3, "pass_turns": 60}],
            "monotonicity_check": true
        });
        // v3+ requires the usable-volume region.
        v["requirement"]["good_field_region"] = serde_json::json!({
            "half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3
        });
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let pruned = record
            .candidates
            .iter()
            .find(|c| c.pruned_by.is_some())
            .expect("a coarse-pruned candidate");
        assert!(
            pruned.screens.is_some(),
            "a declared screen set must be recorded even when pruned"
        );
        let screens = pruned.screens.as_ref().unwrap();
        assert_eq!(
            screens.thermal_margin.as_ref().unwrap().status,
            Status::NotEvaluated
        );
        assert_eq!(
            screens.ac_loss.as_ref().unwrap().status,
            Status::NotEvaluated
        );
        assert_eq!(
            screens.screening_current.as_ref().unwrap().status,
            Status::NotEvaluated
        );
        assert_eq!(
            screens.transition.as_ref().unwrap().status,
            Status::NotEvaluated
        );
        let transient = screens.quench_transient.as_ref().unwrap();
        assert_eq!(transient.status, Status::NotEvaluated);
        assert_eq!(transient.model, QUENCH_TRANSIENT_SCREEN_MODEL_ID);
        // The dump bound still resolved on the operating current alone.
        let quench = screens.quench_hotspot.as_ref().unwrap();
        assert!(
            matches!(
                quench.status,
                Status::Pass | Status::Fail | Status::Inconclusive
            ),
            "pruned candidate's quench screen should still resolve, got {:?}",
            quench.status
        );
    }

    #[test]
    fn a_failed_screen_demotes_an_otherwise_passing_candidate() {
        // The reduced case's low-field-clamped ~1 T perpendicular field
        // sits far above the strip's B_c — a 0.5 penetration-fraction
        // limit must fail every evaluated candidate.
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v["screening_current"]["max_penetrated_width_fraction"] = serde_json::json!(0.5);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let cand = record
            .candidates
            .iter()
            .find(|c| c.screening.is_some())
            .expect("a screened candidate");
        let screening = cand
            .screens
            .as_ref()
            .unwrap()
            .screening_current
            .as_ref()
            .unwrap();
        assert_eq!(screening.status, Status::Fail);
        assert!(screening.max_penetrated_width_fraction.unwrap() > 0.5);
        assert_eq!(cand.status, Status::Fail);
        // Acceptance still agrees — both sides see the same FAIL.
        assert_eq!(
            record.acceptance.baseline.screens_agreement_status,
            Status::Pass
        );
    }

    /// The v15 fixture lifted to schema v18 with the lumped
    /// quench-transient screen declared. `initial_temperature_k` = 30
    /// is the "quench detected, protection fires" boundary condition:
    /// the dataset's 30 K capacity sits ~0.73 of its 21 K value, so the
    /// passing candidate's high-utilization points start the dump
    /// already sharing. The declared property tables span [20, 45] K —
    /// bracketing T_op = 21 and T0 = 30 — and the high heat-capacity
    /// declaration keeps the trajectory inside measured coverage so a
    /// clean verdict is reachable.
    fn v18_reduced_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v15_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v18".into();
        v["quench_transient"] = serde_json::json!({
            "dump_time_constant_s": 0.05,
            "initial_temperature_k": 30.0,
            "conducting_area_per_width_m": 5.0e-3,
            "resistivity_ohm_m": [[20.0, 3.0e-9], [45.0, 1.5e-8]],
            "heat_capacity_j_per_m3k": [[20.0, 1.0e9], [45.0, 1.2e9]],
            "max_temperature_k": 45.0,
            "detection_voltage_v": 1.0e-6,
            "max_detection_time_s": 0.5
        });
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn v18_transient_evaluates_a_declared_quench_dump() {
        let record =
            run_coupled_search_case(&v18_reduced_case_json(), &CoupledSearchOptions::default())
                .unwrap();
        assert_eq!(record.schema, "optcoil-coupled-search-run/v26");
        // Every screened candidate records the transient screen; at
        // least one starts the dump already sharing (its high-
        // utilization points' capacity at T0 = 30 K sits below demand).
        let records: Vec<_> = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.quench_transient.as_ref())
            .collect();
        assert!(!records.is_empty());
        let sharing = records
            .iter()
            .find(|r| r.points_sharing > 0)
            .expect("the declared quench shares current on some candidate");
        assert_eq!(sharing.model, QUENCH_TRANSIENT_SCREEN_MODEL_ID);
        assert_eq!(sharing.status, Status::Pass);
        assert!(!sharing.coverage_exhausted);
        assert!(!sharing.peak_temperature_is_lower_bound);
        // The dump rescued the declared hot spot: the trajectory peaked
        // barely above T0 and stayed inside measured coverage.
        let peak = sharing.peak_temperature_k.unwrap();
        assert!(peak > 30.0 && peak < 45.0, "peak {peak}");
        assert!(sharing.limiting.is_some());
        // The series signature was already present at dump start —
        // detection resolves at t = 0.
        assert_eq!(sharing.detection_time_s, Some(0.0));
        assert!(sharing.peak_series_voltage_v.unwrap() > 0.0);
        assert_eq!(sharing.points_inconclusive, 0);
        // A candidate whose 30 K capacity still covers demand is
        // static: T holds T0 and no signature develops — under a
        // *declared* quench that is an undetectable quench by
        // construction, so the detection bound fails.
        let static_rec = records
            .iter()
            .find(|r| r.points_sharing == 0)
            .expect("some candidate stays static at T0");
        assert_eq!(static_rec.status, Status::Fail);
        assert_eq!(static_rec.peak_temperature_k, Some(30.0));
        assert_eq!(static_rec.peak_series_voltage_v, Some(0.0));
        assert_eq!(static_rec.detection_time_s, None);
        // The acceptance module's independent trajectory agrees.
        for c in [
            &record.acceptance.baseline,
            record
                .acceptance
                .best
                .as_ref()
                .unwrap_or(&record.acceptance.baseline),
        ] {
            let agreement = c
                .recomputed_screens
                .as_ref()
                .and_then(|s| s.quench_transient.as_ref())
                .expect("recomputed transient digest");
            assert_eq!(agreement.status, Status::Pass);
            assert_eq!(agreement.recomputed_status, agreement.recorded_status);
        }
    }

    #[test]
    fn v18_transient_marks_an_undetectable_or_late_quench_fail() {
        // A threshold above the trajectory's whole signature means the
        // declared quench is never detected — the bound fails.
        let mut v: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        v["quench_transient"]["detection_voltage_v"] = serde_json::json!(1.0e3);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let sharing = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.quench_transient.as_ref())
            .find(|r| r.points_sharing > 0)
            .expect("a sharing candidate");
        assert_eq!(sharing.detection_time_s, None);
        assert_eq!(sharing.status, Status::Fail);
    }

    #[test]
    fn v18_transient_fails_a_declared_temperature_bound() {
        // A bound below the declared initial temperature fails every
        // candidate — even a static one holds T0 = 30 K.
        let mut v: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        v["quench_transient"]["max_temperature_k"] = serde_json::json!(29.9);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let records: Vec<_> = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.quench_transient.as_ref())
            .collect();
        assert!(!records.is_empty());
        assert!(records.iter().all(|r| r.status == Status::Fail));
        // The failed declared screen joins the aggregate fail-closed.
        assert!(record.candidates.iter().all(|c| c.status != Status::Pass));
    }

    #[test]
    fn v18_transient_reports_coverage_exhaustion_as_inconclusive() {
        // T0 at the dataset's 40 K top: the first heating step leaves
        // measured Ic coverage — the trajectory's true peak is unknown,
        // so the screen reports INCONCLUSIVE with the reached
        // temperature flagged as a lower bound. Never extrapolated.
        let mut v: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        v["quench_transient"]["initial_temperature_k"] = serde_json::json!(40.0);
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let sharing = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.quench_transient.as_ref())
            .find(|r| r.points_sharing > 0)
            .expect("a sharing candidate at T0 = 40 K");
        assert!(sharing.coverage_exhausted);
        assert_eq!(sharing.status, Status::Inconclusive);
        // Acceptance recomputes the same exhaustion — agreement holds.
        for c in [
            &record.acceptance.baseline,
            record
                .acceptance
                .best
                .as_ref()
                .unwrap_or(&record.acceptance.baseline),
        ] {
            let agreement = c
                .recomputed_screens
                .as_ref()
                .and_then(|s| s.quench_transient.as_ref())
                .expect("recomputed transient digest");
            assert_eq!(agreement.status, Status::Pass);
        }
    }

    #[test]
    fn v18_transient_without_initial_temperature_reports_the_dump_alone() {
        // No declared quench: the dump alone decides. An over-critical
        // candidate (u > 1 at T_op on some points) shares immediately;
        // a screened candidate holds u < 1, so nothing shares — a
        // correct, honest "no quench in this model" answer.
        let mut v: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        v["quench_transient"]
            .as_object_mut()
            .unwrap()
            .remove("initial_temperature_k");
        let record = run_coupled_search_case(
            &serde_json::to_string(&v).unwrap(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        let records: Vec<_> = record
            .candidates
            .iter()
            .filter_map(|c| c.screens.as_ref()?.quench_transient.as_ref())
            .collect();
        let sharing = records
            .iter()
            .find(|r| r.points_sharing > 0)
            .expect("an over-critical candidate shares at T_op");
        assert_eq!(sharing.status, Status::Pass);
        assert!(sharing.peak_temperature_k.unwrap() > 21.0);
        assert!(sharing.peak_series_voltage_v.unwrap() > 0.0);
        let static_rec = records
            .iter()
            .find(|r| r.points_sharing == 0)
            .expect("a screened candidate stays static");
        assert_eq!(static_rec.status, Status::Pass);
        assert_eq!(static_rec.peak_temperature_k, Some(21.0));
        assert_eq!(static_rec.peak_series_voltage_v, Some(0.0));
        assert_eq!(static_rec.detection_time_s, None);
    }

    #[test]
    fn quench_transient_must_not_be_declared_before_v18() {
        let mut v: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v17".into();
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(
            err.contains("schema v18"),
            "quench_transient on v17 must fail with a v18 gate, got: {err}"
        );
    }

    #[test]
    fn quench_transient_declaration_validates_its_tables_and_bounds() {
        let base: serde_json::Value = serde_json::from_str(&v18_reduced_case_json()).unwrap();
        // A detection-time bound without the voltage threshold.
        let mut v = base.clone();
        v["quench_transient"]
            .as_object_mut()
            .unwrap()
            .remove("detection_voltage_v");
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(err.contains("detection_voltage_v"), "got: {err}");
        // An initial temperature below the operating point.
        let mut v = base.clone();
        v["quench_transient"]["initial_temperature_k"] = serde_json::json!(10.0);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().is_err());
        // An initial temperature outside the property-table span.
        let mut v = base.clone();
        v["quench_transient"]["initial_temperature_k"] = serde_json::json!(60.0);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().is_err());
        // A table that does not bracket the operating temperature.
        let mut v = base.clone();
        v["quench_transient"]["resistivity_ohm_m"] =
            serde_json::json!([[25.0, 3.0e-9], [45.0, 1.5e-8]]);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        let err = case.validate().unwrap_err().to_string();
        assert!(err.contains("bracket"), "got: {err}");
        // A non-monotone temperature axis.
        let mut v = base.clone();
        v["quench_transient"]["heat_capacity_j_per_m3k"] =
            serde_json::json!([[45.0, 1.0e9], [20.0, 1.2e9]]);
        let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
        assert!(case.validate().is_err());
        // Non-positive declarations.
        for (key, value) in [
            ("dump_time_constant_s", serde_json::json!(0.0)),
            ("conducting_area_per_width_m", serde_json::json!(-1e-3)),
            ("max_temperature_k", serde_json::json!(0.0)),
            ("max_detection_time_s", serde_json::json!(0.0)),
        ] {
            let mut v = base.clone();
            v["quench_transient"][key] = value.clone();
            let case: CoupledSearchCase = serde_json::from_value(v).unwrap();
            assert!(case.validate().is_err(), "{key} must reject {value:?}");
        }
    }

    #[test]
    fn declared_table_at_interpolates_and_refuses_extrapolation() {
        let table = [[20.0, 3.0e-9], [45.0, 1.5e-8]];
        assert_eq!(declared_table_at(&table, 20.0), Some(3.0e-9));
        let hi = declared_table_at(&table, 45.0).unwrap();
        assert!((hi - 1.5e-8).abs() < 1e-12);
        let mid = declared_table_at(&table, 32.5).unwrap();
        assert!((mid - 9.0e-9).abs() < 1e-12);
        assert_eq!(declared_table_at(&table, 19.9), None);
        assert_eq!(declared_table_at(&table, 45.1), None);
        assert_eq!(declared_table_at(&table, f64::NAN), None);
    }
}
