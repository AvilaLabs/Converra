//! Contract §8 D8: independent acceptance for the OC-007 coupled cost
//! search. Separate module, separate authority (contract A1): every number
//! here is recomputed from a selected candidate's geometry alone, never
//! copied from the search's own bookkeeping, and the coupled screening is
//! re-run through the real `run_coupled_case` entry point — not the search's
//! own coarse-plan shortcut — for the selected optimum and the baseline.
//!
//! No independent reference exists per candidate (only the frozen OC-004
//! geometry has one), so the rerun's own `independent_comparison` check is
//! always INCONCLUSIVE here by construction; its `field_refinement` check
//! must PASS instead, and this module says so plainly rather than treating
//! the missing comparison as a silent pass.

use std::{collections::BTreeMap, sync::Mutex};

use optcoil_model::{
    Check, Status,
    coupled::{Station, TapeNormal},
    coupled_search::{
        BASE_TAPE_SPEC_ID, CandidateDims, CoupledSearchCase, build_coupled_case,
        expand_relative_turn_indices, refined_plan_stations, refined_plan_turn_indices,
    },
    magnetics::{CurrentModel, Racetrack},
    material::MaterialDataset,
};
use optcoil_physics::{
    critical_current::IcInterpolator,
    racetrack::{MU0_H_PER_M, RacetrackEvaluator},
    tape_frame,
};
use serde::{Deserialize, Serialize};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{
    RunError,
    coupled::{
        CandidateResult, CoupledOptions, LimitingPoint, SpecRuntime, StationGroup,
        aggregate_status, run_coupled_case_with_datasets_ticked_cancellable, spec_runtime,
    },
    coupled_search::{
        AcLossScreenRecord, QuenchHotspotScreenRecord, QuenchTransientScreenRecord,
        ScreeningCurrentScreenRecord, SearchCandidateResult, SearchCostLedger, SearchProgress,
        SpecPiecePlan, ThermalMarginScreenRecord, TransitionScreenRecord,
    },
};

fn status_not_evaluated() -> Status {
    Status::NotEvaluated
}

/// The dataset refs a generated coupled case may be supplied with:
/// exactly the resolved datasets its declared material bindings name.
/// The search-level map is a superset — a candidate whose assignment
/// never references a spec emits no region for it, and the coupled
/// runner rightly rejects a supplied dataset no binding declares.
fn declared_dataset_refs<'a>(
    case: &optcoil_model::coupled::CoupledCase,
    datasets: &'a BTreeMap<String, MaterialDataset>,
) -> Vec<&'a MaterialDataset> {
    let declared: std::collections::BTreeSet<&str> = case
        .material_bindings()
        .iter()
        .map(|(_, m)| m.dataset_id.as_str())
        .collect();
    datasets
        .values()
        .filter(|d| declared.contains(d.metadata.id.as_str()))
        .collect()
}

/// Bumped to v3 for schema v3's geometric gates: the acceptance module now
/// independently recomputes the region/pack overlap verdict and the
/// `manufacturing` inner-bend-radius feasibility, both folded into
/// `agreement_status` fail-closed.
/// Bumped to v4 for schema v7's hoop-stress bound (OC-018): the
/// independently recomputed `mechanical_feasible` now ANDs the declared
/// hoop bound onto the Lorentz-load check, matching the search's
/// combined semantics.
/// Bumped to v17 for case schema v24: the piece-quantized procurement
/// ledger — the piece-plan walk (runs, offering argmin, splice and
/// remnant accounting) is recomputed on this module's own arithmetic
/// below, written separately from `coupled_search::piece_plan_ledger`.
/// Bumped to v5 for search schema v10: the independent cost
/// recomputation prices each turn under its region's assigned spec, and
/// the screening reruns carry the candidate's `tape_spec_ids` into the
/// generated v6 case — per-binding datasets resolve per turn there.
/// Bumped to v6 for search schema v12: the independent mechanical
/// recomputation now accumulates the rerun's per-turn normal loads into
/// a transverse interface pressure (its own trapezoid accumulation, not
/// `coupled_search::peak_transverse_pressure`) and ANDs a declared
/// `max_transverse_pressure_pa` into `recomputed_feasible`, matching the
/// search's combined semantics.
/// Bumped to v7 for run-schema v12's pressure-coverage check: the
/// refined §9.4 plan now also carries its own accumulated transverse
/// pressure, and a declared `max_transverse_pressure_pa` makes the
/// declared plan's pressure resolution load-bearing — a shortfall the
/// declared gate cannot absorb fails agreement like any other coverage
/// failure.
/// Bumped to v8 for run-schema v15's adjacent screens: every declared
/// screen is recomputed on this module's own rerun — independently
/// written accumulation and root-find structure — and compared
/// verdict-for-verdict against the search's recorded screens. A
/// disagreement fails `screens_agreement_status`, which ANDs into the
/// candidate's `agreement_status` fail-closed.
/// Bumped to v9 for run-schema v16's `transition` screen: the measured
/// E–J law `u^n` is recomputed per point on the rerun's own stored
/// capacities and mirror-pair re-queries, with a separately organized
/// voltage accumulation and a worst-depth fold. Bumped to v10 for
/// run-schema v17's winding-mechanics pair: the outer-fiber bend strain
/// is recomputed from this module's own inner-radius arithmetic into
/// `recomputed_manufacturing_feasible`, and the undivided
/// membrane-tension resultant from its own load-column accumulation
/// into `recomputed_feasible`.
/// Bumped to v11 for run-schema v18's `quench_transient` screen: the
/// lumped driven-dump trajectory is re-integrated on this module's own
/// seeds — per-point sequential organization under the model's declared
/// stepping — and compared status-for-status plus peak temperature,
/// detection time, peak series voltage, and the coverage flags.
/// Bumped to v12 for run-schema v19's `fixed_geometry.path3d`: the helix
/// pack's installed-length ledger is recomputed on this module's own
/// per-segment offset-length arithmetic — `|sweep|·hypot(R+δ, c)` — so
/// the check cannot inherit a mistake from `CoilPath3D::length_at_offset_m`.
/// Bumped to v14 for run-schema v21's racetrack-dimension axes: every
/// recomputation — the Racetrack evaluator, the pack-overlap screen, the
/// bend/mechanics bounds, the per-turn loop-length walk and the cost
/// ledger — resolves the candidate's recorded dims rather than the
/// case's fixed declaration, so a record naming one geometry but priced
/// or screened at another fails by construction. Bumped to v15 for
/// run-schema v22's region-uniformity figure: the lattice maximum and
/// the (max − min)/B_center deviation are independently recomputed on
/// this module's own extremes and center value, and a recorded
/// deviation that disagrees fails region agreement — a record claiming
/// a tighter uniformity than it evaluated fails by construction.
/// Bumped to v16 for run-schema v23's midplane harmonic expansion: the
/// reference circle is independently re-evaluated and the DFT
/// recomputed, with b0 and every normal/skew coefficient compared under
/// the NI-scaled gate — a record claiming a multipole spectrum it did
/// not evaluate, or missing a declared spectrum, fails by construction.
/// Bumped to v18 for the acceptance fallback walk: when the cheapest
/// screening-PASS candidate fails the §9.4 refined-plan gate, the
/// remaining PASS candidates are re-verified in ascending cost order
/// (bounded — see `ACCEPTANCE_FALLBACK_LIMIT`) and the first to hold
/// both the refined gate and the agreement gates becomes the reported
/// `best`. Which candidate `best` names therefore differs by
/// construction on cases where the walk promotes a survivor.
/// Bumped to v19 for baseline-equals-best reuse: the baseline's independent
/// recomputation now also supplies the selected-best acceptance result when
/// both indices are identical, with its progress leg collapsed to zero.
pub const COUPLED_SEARCH_ACCEPTANCE_CHECKER_ID: &str =
    "coupled-search-acceptance-independent-cost-and-field-recomputation/v19";

/// Bound on the fallback walk: the number of *additional* PASS
/// candidates re-verified after the cheapest fails §9.4. Bounded
/// because each re-verification costs a refined-plan rerun — and a
/// sampling plan that under-resolves several candidates in a row is
/// systematically inadequate, so more attempts chase the same gap
/// rather than converging on a survivor.
const ACCEPTANCE_FALLBACK_LIMIT: usize = 4;

/// Contract §9.4: the refined-plan screening summary for one candidate,
/// combined across however many station groups the thread pool evaluated.
/// A dedicated (rather than reused `coupled::CandidateResult`) type: only
/// the fields the shortfall gate and the record actually need are
/// meaningful once results from several independent `run_coupled_case`
/// calls (one per station group) are combined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingRefinementResult {
    pub status: Status,
    pub min_allowed_screening_a: Option<f64>,
    pub limiting: Option<LimitingPoint>,
    pub max_utilization: Option<f64>,
    /// This module's own transverse-pressure estimate accumulated on the
    /// refined plan's denser turn sampling — the coverage reference the
    /// coarse plan's `recomputed_transverse_pressure_pa` is checked
    /// against. `None` when the refined plan did not run or sampled no
    /// field. Defaulted so records written before run-schema v12 still
    /// parse.
    #[serde(default)]
    pub transverse_pressure_pa: Option<f64>,
    pub stations_evaluated: usize,
    pub points_evaluated: u64,
}

/// Run-schema v15: one declared adjacent screen's recomputation digest.
/// `status` is the *agreement* verdict — PASS iff this module's own
/// recomputed screen status and headline value match the search's
/// recorded ones; FAIL on any mismatch or on a declared screen the
/// record cannot substantiate; NOT_EVALUATED only on undeclared
/// screens, which emit no entry at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecomputedScreenAgreement {
    /// The screen's status as recomputed here on the module's own rerun.
    pub recomputed_status: Status,
    /// The status the search recorded for the same screen.
    pub recorded_status: Status,
    /// The recomputed headline quantity: min margin (K) for
    /// `thermal_margin`, total loss (W) for `ac_loss`, hot-spot
    /// temperature (K) for `quench_hotspot`, the maximum penetrated
    /// width fraction for `screening_current`, and the maximum `E/Ec`
    /// transition depth for `transition`. `None` when the screen
    /// resolved no value.
    #[serde(default)]
    pub recomputed_value: Option<f64>,
    /// The recorded headline quantity, same convention.
    #[serde(default)]
    pub recorded_value: Option<f64>,
    /// PASS iff `recomputed_status == recorded_status` and both headline
    /// values agree within the module's agreement tolerance (or are both
    /// absent).
    pub status: Status,
}

/// Run-schema v15: per-screen recomputation digests — only the screens
/// the case declares carry an entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecomputedScreens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_margin: Option<RecomputedScreenAgreement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ac_loss: Option<RecomputedScreenAgreement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quench_hotspot: Option<RecomputedScreenAgreement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screening_current: Option<RecomputedScreenAgreement>,
    /// Run-schema v16: the measured E–J transition screen's
    /// recomputation digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<RecomputedScreenAgreement>,
    /// Run-schema v18: the lumped quench-transient screen's
    /// recomputation digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quench_transient: Option<RecomputedScreenAgreement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecomputedCandidate {
    pub label: String,
    pub index: usize,
    pub turns_along_normal: u32,
    pub tapes_along_width: u32,
    pub recomputed_cost: SearchCostLedger,
    pub search_cost_usd: f64,
    pub cost_relative_error: f64,
    /// PASS iff `cost_relative_error <= 1e-8` (contract D8).
    pub cost_agreement_status: Status,
    pub recomputed_unit_bore_bz_t_per_ampere_turn: f64,
    pub search_unit_bore_bz_t_per_ampere_turn: f64,
    pub field_absolute_error_t: f64,
    /// PASS iff the field difference stays within the case's own refinement
    /// gate (`field_scale_t * max_refinement_change_fraction`).
    pub field_agreement_status: Status,
    /// Schema v3 only: the usable-volume lattice minimum independently
    /// re-evaluated at the final quadrature order.
    pub recomputed_region_min_unit_bz_t_per_ampere_turn: Option<f64>,
    /// Schema v22: the region's (max − min) / B_center uniformity figure,
    /// independently recomputed on this module's own lattice extremes and
    /// center value.
    pub recomputed_relative_deviation: Option<f64>,
    pub region_field_absolute_error_t: Option<f64>,
    /// PASS/FAIL under the same NI-scaled gate as `field_agreement_status`;
    /// FAIL when the region is declared but unrecorded or vice versa, or
    /// when the independently recomputed pack-overlap verdict disagrees
    /// with the recorded one; NOT_EVALUATED when no region is declared.
    pub region_agreement_status: Status,
    /// Schema v3 `manufacturing` gate, recomputed from the candidate's own
    /// geometry: PASS iff the recomputed feasibility verdict matches the
    /// search's recorded one; NOT_EVALUATED when no manufacturing block is
    /// declared; FAIL on a mismatch (or a missing record).
    pub manufacturing_agreement_status: Status,
    /// Schema v4 `mechanical` screen, recomputed from this module's own
    /// rerun's peak sampled field times the operating current: PASS iff
    /// the recomputed feasibility verdict matches the search's recorded
    /// one; NOT_EVALUATED when no mechanical block is declared; FAIL on a
    /// mismatch (or an unevaluable load on the search side). Defaults to
    /// NOT_EVALUATED so v3 records (no mechanical block) still parse.
    #[serde(default = "status_not_evaluated")]
    pub mechanical_agreement_status: Status,
    /// Search schema v12: this module's own transverse-pressure estimate,
    /// recomputed from the rerun's per-(station, tape-column, turn)
    /// normal loads with a separately written trapezoid accumulation.
    /// Recorded alongside the verdict-level `mechanical_agreement_status`
    /// so the ledger carries the recomputed screen value itself; `None`
    /// when the rerun sampled no field. Defaulted so records written
    /// before run-schema v11 still parse.
    #[serde(default)]
    pub recomputed_transverse_pressure_pa: Option<f64>,
    /// Search schema v17: this module's own outer-fiber bend-strain
    /// recomputation — the declared tape thickness over twice the inner
    /// bend radius. `None` when the case declares no
    /// `tape_thickness_m` or the radius did not resolve. Defaulted so
    /// records written before run-schema v17 still parse.
    #[serde(default)]
    pub recomputed_bend_strain: Option<f64>,
    /// Search schema v17: this module's own membrane-tension resultant
    /// (N/m) — `recomputed_transverse_pressure_pa`'s undivided numerator
    /// at the same accumulation. `None` when the rerun sampled no field.
    /// Defaulted so records written before run-schema v17 still parse.
    #[serde(default)]
    pub recomputed_membrane_tension_n_per_m: Option<f64>,
    pub rerun_screening_status: Status,
    pub rerun_refinement_status: Status,
    /// Always INCONCLUSIVE: no independent reference exists per candidate.
    pub rerun_numerical_status: Status,
    pub screening_status_agreement: bool,
    /// The coarse (declared 4-station/~15-turn) plan's own
    /// `min_allowed_screening_a`, from this module's own independent rerun
    /// above -- never the search's own bookkeeping (contract §9.4).
    pub coarse_min_allowed_screening_a: Option<f64>,
    pub coarse_limiting: Option<LimitingPoint>,
    /// Contract §9.4: the finer 10-station re-evaluation. Only computed
    /// when this candidate's own original `status` was PASS (NotEvaluated
    /// otherwise: nothing to re-verify for a candidate that never claimed a
    /// PASS in the first place).
    pub refined: SamplingRefinementResult,
    /// `(coarse_min_allowed_screening_a - refined.min_allowed_screening_a) /
    /// coarse_min_allowed_screening_a`, clamped to >= 0 (a refined margin
    /// that is *larger* than the coarse one is not a shortfall). `None` when
    /// either minimum is unavailable.
    pub sampling_shortfall_fraction: Option<f64>,
    /// PASS iff the original status was PASS, the refined plan also reports
    /// PASS, and the shortfall stays within
    /// `refined_plan.max_sampling_shortfall_fraction`; FAIL if the original
    /// was PASS but either of those fails; NotEvaluated when the original
    /// was never PASS (contract §9.4).
    pub sampling_refinement_status: Status,
    /// `(refined.transverse_pressure_pa - coarse pressure) / coarse`,
    /// clamped to >= 0 — the pressure-accumulation counterpart of
    /// `sampling_shortfall_fraction`: a refined estimate *larger* than the
    /// coarse one means the declared plan under-resolved the load profile
    /// the v12 bound screens. `None` when either estimate is unavailable
    /// or the coarse estimate is zero. Defaulted so records written before
    /// run-schema v12 still parse.
    #[serde(default)]
    pub pressure_shortfall_fraction: Option<f64>,
    /// Evaluated only when `mechanical.max_transverse_pressure_pa` is
    /// declared (the resolution is only load-bearing then): PASS iff the
    /// shortfall stays within the same declared
    /// `max_sampling_shortfall_fraction` gate; FAIL when the shortfall
    /// exceeds it or cannot be computed for a PASS candidate under a
    /// declared bound; NOT_EVALUATED otherwise. Defaulted so records
    /// written before run-schema v12 still parse.
    #[serde(default = "status_not_evaluated")]
    pub pressure_coverage_status: Status,
    /// Run-schema v15: this module's own recomputation of every declared
    /// adjacent screen against its own rerun — verdict-level agreement
    /// plus the recomputed headline values. `None` when the case
    /// declares no screens (or the pack geometry was unrealizable).
    /// Defaulted so records written before run-schema v15 still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recomputed_screens: Option<RecomputedScreens>,
    /// PASS iff every declared screen's recomputation agrees with the
    /// record; NOT_EVALUATED when no screen is declared. Defaulted so
    /// records written before run-schema v15 still parse.
    #[serde(default = "status_not_evaluated")]
    pub screens_agreement_status: Status,
    pub agreement_status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoupledSearchAcceptance {
    pub checker_id: String,
    pub best: Option<RecomputedCandidate>,
    pub baseline: RecomputedCandidate,
    /// Candidates re-verified by the fallback walk after the cheapest
    /// PASS candidate failed the §9.4 refined-plan gate — in examination
    /// order (ascending cost). When a later candidate was promoted to
    /// `best`, every entry here is a cheaper candidate that could not be
    /// substantiated; when the walk exhausted without a survivor, `best`
    /// still names the cheapest PASS candidate and the entries are the
    /// other candidates that were tried. Empty when the walk never ran —
    /// i.e. when the first optimum held the gate, or none existed.
    /// Defaulted so records written before checker v18 still parse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_attempts: Vec<RecomputedCandidate>,
    /// PASS iff the baseline and the reported `best` individually have
    /// `agreement_status` PASS and every walk attempt's recomputation
    /// agreed with the record. Attempts contribute recompute agreement
    /// only — a candidate lands in `fallback_attempts` precisely because
    /// its §9.4 or pressure verdict failed, and that verdict is narrated
    /// by the `acceptance_fallback` check rather than re-counted here.
    pub agreement_status: Status,
    pub checks: Vec<Check>,
    pub limitations: Vec<String>,
}

/// Recompute and re-verify the search's selected optimum and its baseline
/// (contract D8). Takes the search case and its already-computed candidates
/// rather than re-running the search: acceptance audits what the search
/// reported, it does not repeat the search itself. Embedded directly into
/// `CoupledSearchRunRecord.acceptance` by `coupled_search::run_coupled_search_case`
/// (contract §5: the run record itself carries "acceptance results").
/// `datasets` is keyed by `metadata.id` and covers every binding the
/// case declares — the search's resolved map, so a graded candidate's
/// rerun screens each turn under the same supplied/embedded datasets the
/// search itself used.
///
/// Checker v18: when the named `best_index` fails the §9.4 refined-plan
/// gate, the remaining screening-PASS candidates are examined in
/// ascending cost order (bounded by `ACCEPTANCE_FALLBACK_LIMIT`) and the
/// first to hold both gates is promoted into `best`; the cheaper
/// unresolved candidates are recorded in `fallback_attempts`.
pub fn assess(
    search: &CoupledSearchCase,
    candidates: &[SearchCandidateResult],
    baseline_index: usize,
    best_index: Option<usize>,
    datasets: &BTreeMap<String, MaterialDataset>,
) -> Result<CoupledSearchAcceptance, RunError> {
    assess_progress(
        search,
        candidates,
        baseline_index,
        best_index,
        datasets,
        None,
    )
}

/// Progress-reporting form of [`assess`]: the runner pre-plans two
/// legs after the candidate legs (indices `candidates.len()` and
/// `candidates.len() + 1`, pessimistically sized); each recomputation
/// revises its leg's plan down to the discovered scope — PASS candidates
/// include the §9.4 refined plan — then ticks it once per field
/// evaluation. Display-only; the tick is never read back into a verdict.
pub fn assess_progress(
    search: &CoupledSearchCase,
    candidates: &[SearchCandidateResult],
    baseline_index: usize,
    best_index: Option<usize>,
    datasets: &BTreeMap<String, MaterialDataset>,
    progress: Option<&SearchProgress>,
) -> Result<CoupledSearchAcceptance, RunError> {
    assess_progress_cancellable(
        search,
        candidates,
        baseline_index,
        best_index,
        datasets,
        progress,
        &AtomicBool::new(false),
    )
}

/// Cancellation-aware form of [`assess_progress`]. The flag is passed into
/// every independent field rerun and refined-plan worker. Cancellation
/// returns `RunError::Cancelled`, so no partial acceptance is emitted.
pub(crate) fn assess_progress_cancellable(
    search: &CoupledSearchCase,
    candidates: &[SearchCandidateResult],
    baseline_index: usize,
    best_index: Option<usize>,
    datasets: &BTreeMap<String, MaterialDataset>,
    progress: Option<&SearchProgress>,
    cancel: &AtomicBool,
) -> Result<CoupledSearchAcceptance, RunError> {
    check_cancelled(cancel)?;
    // Fetch the pre-planned leg, revising its estimate to the actual
    // scope; if the runner did not pre-plan one (standalone callers),
    // append it instead.
    let leg = |p: &SearchProgress, index: usize, planned: u64| {
        p.set_leg_planned(index, planned);
        p.leg(index).unwrap_or_else(|| p.add_leg(planned))
    };
    let baseline_original = &candidates[baseline_index];
    let baseline_leg = progress.map(|p| {
        p.set_phase("acceptance: recomputing baseline".to_owned());
        leg(
            p,
            candidates.len(),
            acceptance_planned_evals(
                search,
                baseline_original.geometry.turns_along_normal,
                baseline_original.geometry.tapes_along_width,
                baseline_original.status == Status::Pass,
            ),
        )
    });
    let baseline = recompute_one_cancellable(
        "baseline",
        baseline_index,
        search,
        baseline_original,
        datasets,
        baseline_leg.as_deref(),
        cancel,
    )?;

    check_cancelled(cancel)?;
    let best = match best_index {
        Some(i) if i == baseline_index => {
            // The baseline has already been independently recomputed above.
            // When it is also the selected optimum, copying that recomputed
            // assessment preserves the independent acceptance check while
            // avoiding a second identical field/material rerun.
            if let Some(p) = progress {
                p.set_leg_planned(candidates.len() + 1, 0);
            }
            let mut same_candidate = baseline.clone();
            same_candidate.label = "best".into();
            Some(same_candidate)
        }
        Some(i) => {
            let best_leg = progress.map(|p| {
                p.set_phase("acceptance: recomputing optimum".to_owned());
                leg(
                    p,
                    candidates.len() + 1,
                    acceptance_planned_evals(
                        search,
                        candidates[i].geometry.turns_along_normal,
                        candidates[i].geometry.tapes_along_width,
                        candidates[i].status == Status::Pass,
                    ),
                )
            });
            Some(recompute_one_cancellable(
                "best",
                i,
                search,
                &candidates[i],
                datasets,
                best_leg.as_deref(),
                cancel,
            )?)
        }
        None => {
            // No optimum — collapse the pre-planned leg so it reads done.
            if let Some(p) = progress {
                p.set_leg_planned(candidates.len() + 1, 0);
            }
            None
        }
    };

    // Fallback walk (checker v18): when the cheapest screening-PASS
    // candidate fails the §9.4 refined-plan gate, examine the remaining
    // PASS candidates in ascending cost order and promote the first to
    // hold both the refined gate and the agreement gates. The run then
    // reports a usable optimum and the cheaper unresolved candidates sit
    // in `fallback_attempts` — the honest distinction between "no cheaper
    // design was found" and "a cheaper design was found but the declared
    // sampling could not substantiate it".
    let mut best = best;
    let mut fallback_attempts: Vec<RecomputedCandidate> = Vec::new();
    let mut walk_ran = false;
    let mut promoted = false;
    if best
        .as_ref()
        .is_some_and(|b| b.sampling_refinement_status != Status::Pass)
    {
        walk_ran = true;
        // The first optimum is itself the cheapest unresolved candidate;
        // it joins the attempts in examination order.
        if let Some(mut first) = best.take() {
            first.label = "fallback".into();
            fallback_attempts.push(first);
        }
        // Alternatives only: never the tried optimum, and never the
        // baseline — it is the case's reference design, not a candidate
        // the search priced for purchase (callers that pass a synthetic
        // [baseline, optimum] list then find no alternatives at all,
        // which is the correct outcome there).
        let mut order: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(j, c)| {
                Some(*j) != best_index && *j != baseline_index && c.status == Status::Pass
            })
            .map(|(j, _)| j)
            .collect();
        order.sort_by(|&a, &b| {
            candidates[a]
                .cost
                .total_usd
                .total_cmp(&candidates[b].cost.total_usd)
                .then(
                    candidates[a]
                        .geometry
                        .total_turns
                        .cmp(&candidates[b].geometry.total_turns),
                )
                .then(a.cmp(&b))
        });
        for &j in order.iter().take(ACCEPTANCE_FALLBACK_LIMIT) {
            check_cancelled(cancel)?;
            let leg = progress.map(|p| {
                p.set_phase("acceptance: examining cheaper candidates".to_owned());
                p.add_leg(acceptance_planned_evals(
                    search,
                    candidates[j].geometry.turns_along_normal,
                    candidates[j].geometry.tapes_along_width,
                    true,
                ))
            });
            let mut attempt = recompute_one_cancellable(
                "fallback",
                j,
                search,
                &candidates[j],
                datasets,
                leg.as_deref(),
                cancel,
            )?;
            if attempt.sampling_refinement_status == Status::Pass
                && attempt.agreement_status == Status::Pass
            {
                attempt.label = "best".into();
                best = Some(attempt);
                promoted = true;
                break;
            }
            fallback_attempts.push(attempt);
        }
        if best.is_none() {
            // Nothing survived: restore the first optimum's recompute so
            // `best` still names the cheapest PASS candidate, carrying
            // its blocking verdict — the pre-walk semantics exactly.
            let mut first = fallback_attempts.remove(0);
            first.label = "best".into();
            best = Some(first);
        }
    }

    // The gate covers every candidate this module recomputed — baseline,
    // the reported best, and each walk attempt. Attempts contribute their
    // *recompute* agreement only: an attempt is an attempt because its
    // §9.4 or pressure verdict already failed, and that verdict is what
    // the fallback check narrates — folding it into `agreement_status`
    // again would fail every record the walk touched, including a clean
    // promotion. What stays audited is whether the recorded numbers
    // recompute correctly.
    let attempts_agree = fallback_attempts.iter().all(|a| {
        a.cost_agreement_status == Status::Pass
            && a.field_agreement_status == Status::Pass
            && a.region_agreement_status != Status::Fail
            && a.manufacturing_agreement_status != Status::Fail
            && a.mechanical_agreement_status != Status::Fail
            && a.rerun_refinement_status == Status::Pass
            && a.screening_status_agreement
            && a.screens_agreement_status != Status::Fail
    });
    let agreement_status = match &best {
        Some(b)
            if b.agreement_status == Status::Pass
                && baseline.agreement_status == Status::Pass
                && attempts_agree =>
        {
            Status::Pass
        }
        Some(_) => Status::Fail,
        None if attempts_agree => baseline.agreement_status,
        None => Status::Fail,
    };

    let mut checks = vec![Check {
        id: "baseline_acceptance".into(),
        status: baseline.agreement_status,
        detail: format!(
            "cost {:?} (relative error {:.3e}), field {:?} (absolute error {:.3e} T), screening rerun {:?} (search recorded {:?}), refinement {:?}",
            baseline.cost_agreement_status,
            baseline.cost_relative_error,
            baseline.field_agreement_status,
            baseline.field_absolute_error_t,
            baseline.rerun_screening_status,
            baseline_original.screening.as_ref().map(|s| s.status),
            baseline.rerun_refinement_status,
        ),
    }];
    checks.push(match &best {
        Some(b) => Check {
            id: "best_acceptance".into(),
            status: b.agreement_status,
            detail: format!(
                "cost {:?} (relative error {:.3e}), field {:?} (absolute error {:.3e} T), screening rerun {:?}, refinement {:?}",
                b.cost_agreement_status,
                b.cost_relative_error,
                b.field_agreement_status,
                b.field_absolute_error_t,
                b.rerun_screening_status,
                b.rerun_refinement_status,
            ),
        },
        None => Check {
            id: "best_acceptance".into(),
            status: Status::NotEvaluated,
            detail: "search_status is not PASS (no candidate has requirement, refinement and screening all PASS); acceptance covers only the baseline.".into(),
        },
    });
    checks.push(Check {
        id: "sampling_refinement".into(),
        status: match &best {
            Some(b) => aggregate_status([baseline.sampling_refinement_status, b.sampling_refinement_status]),
            None => baseline.sampling_refinement_status,
        },
        detail: format!(
            "contract §9.4 refined 10-station plan (max_sampling_shortfall_fraction {:.4}): baseline {}; best {}",
            search.refined_plan.max_sampling_shortfall_fraction,
            format_sampling_refinement(&baseline),
            match &best {
                Some(b) => format_sampling_refinement(b),
                None => "not evaluated (no PASS candidate)".into(),
            },
        ),
    });
    checks.push(Check {
        id: "pressure_coverage".into(),
        status: match &best {
            Some(b) => aggregate_status([baseline.pressure_coverage_status, b.pressure_coverage_status]),
            None => baseline.pressure_coverage_status,
        },
        detail: format!(
            "refined-plan transverse-pressure coverage (gate = max_sampling_shortfall_fraction {:.4}; only evaluated when a v12 pressure bound is declared): baseline {}; best {}",
            search.refined_plan.max_sampling_shortfall_fraction,
            format_pressure_coverage(&baseline),
            match &best {
                Some(b) => format_pressure_coverage(b),
                None => "not evaluated (no PASS candidate)".into(),
            },
        ),
    });
    checks.push(Check {
        id: "adjacent_screens".into(),
        status: match &best {
            Some(b) => aggregate_status([
                baseline.screens_agreement_status,
                b.screens_agreement_status,
            ]),
            None => baseline.screens_agreement_status,
        },
        detail: format!(
            "run-schema v15+ adjacent-screen recomputation (verdict-level agreement on this module's own rerun): baseline {}; best {}",
            format_screens_agreement(&baseline),
            match &best {
                Some(b) => format_screens_agreement(b),
                None => "not evaluated (no PASS candidate)".into(),
            },
        ),
    });
    if walk_ran {
        let (status, detail) = if promoted {
            let b = best.as_ref().expect("promoted implies best");
            (
                Status::Pass,
                format!(
                    "§9.4 fallback: the cheapest PASS candidate (index {}) under-resolved on the refined plan; {} further candidate(s) examined in ascending cost order before candidate {} ({}x{}, ${:.0}) held both gates — it is the reported optimum, and the cheaper unresolved candidates are in fallback_attempts.",
                    fallback_attempts
                        .first()
                        .map(|a| a.index)
                        .unwrap_or(usize::MAX),
                    fallback_attempts.len() - 1,
                    b.index,
                    b.turns_along_normal,
                    b.tapes_along_width,
                    b.recomputed_cost.total_usd,
                ),
            )
        } else if fallback_attempts.is_empty() {
            (
                Status::Inconclusive,
                "§9.4 fallback: the cheapest PASS candidate under-resolved on the refined plan; no other screening-PASS candidate exists to examine.".into(),
            )
        } else {
            (
                Status::Inconclusive,
                format!(
                    "§9.4 fallback: the cheapest PASS candidate under-resolved on the refined plan; {} further candidate(s) examined in ascending cost order (limit {}) — none held both gates, so the cheapest remains the reported optimum and the run stays inconclusive.",
                    fallback_attempts.len(),
                    ACCEPTANCE_FALLBACK_LIMIT,
                ),
            )
        };
        checks.push(Check {
            id: "acceptance_fallback".into(),
            status,
            detail,
        });
    }

    Ok(CoupledSearchAcceptance {
        checker_id: COUPLED_SEARCH_ACCEPTANCE_CHECKER_ID.into(),
        best,
        baseline,
        fallback_attempts,
        agreement_status,
        checks,
        limitations: vec![
            "No independent reference exists per candidate; each rerun's own independent_comparison check is INCONCLUSIVE by construction. Its field_refinement check must PASS instead, and this record states that plainly rather than treating a missing comparison as a pass.".into(),
            "Cost agreement is independently recomputed arithmetic from the candidate's geometry alone, not independent physical evidence; it catches search/acceptance divergence, not model error.".into(),
            "The refined 10-station plan (contract §9.4) is still a finite, declared sampling plan, not a continuous scan; it reduces but does not eliminate the chance the true weakest location lies elsewhere. A shortfall beyond the declared gate downgrades search_status to INCONCLUSIVE rather than being reconciled by adjusting the plan after the fact.".into(),
            "The transverse-pressure coverage check reuses the declared max_sampling_shortfall_fraction gate: when a pressure bound is declared, the refined plan's denser turn sampling must reproduce the declared plan's pressure estimate within that fraction. A pressure profile is an accumulated integral, so coverage failure means the declared plan under-resolved the load distribution — it says nothing about the bound's own adequacy, which remains a declared assumption.".into(),
            "Adjacent-screen agreement recomputes each declared screen's verdict on this module's own rerun — it verifies that the search applied the declared screen semantics and carried the inputs through, never that a screen's declared assumptions are physically adequate. The screens are first-order estimates and bounds: a PASS is not a quench-protection, cryogenic, or screening-stress certification. Under a declared field map the recomputation propagates the customer-declared map exactly as the field check does — it verifies propagation, not the map's correctness.".into(),
        ],
    })
}

/// One candidate's contract §9.4 summary line for the `sampling_refinement`
/// check's human-readable detail.
fn format_sampling_refinement(c: &RecomputedCandidate) -> String {
    if c.sampling_refinement_status == Status::NotEvaluated {
        return format!(
            "{}x{} not evaluated (original status was not PASS)",
            c.turns_along_normal, c.tapes_along_width
        );
    }
    format!(
        "{}x{} {:?} (coarse min {:?} A at {:?}, refined min {:?} A at {:?}, shortfall {:?})",
        c.turns_along_normal,
        c.tapes_along_width,
        c.sampling_refinement_status,
        c.coarse_min_allowed_screening_a,
        c.coarse_limiting,
        c.refined.min_allowed_screening_a,
        c.refined.limiting,
        c.sampling_shortfall_fraction,
    )
}

/// One candidate's pressure-coverage summary line for the
/// `pressure_coverage` check's human-readable detail.
fn format_pressure_coverage(c: &RecomputedCandidate) -> String {
    if c.pressure_coverage_status == Status::NotEvaluated {
        return format!(
            "{}x{} not evaluated ({}; coarse {:?} Pa, refined {:?} Pa)",
            c.turns_along_normal,
            c.tapes_along_width,
            if c.sampling_refinement_status == Status::NotEvaluated
                && c.refined.transverse_pressure_pa.is_none()
            {
                "no declared bound or no PASS original"
            } else {
                "no declared bound"
            },
            c.recomputed_transverse_pressure_pa,
            c.refined.transverse_pressure_pa,
        );
    }
    format!(
        "{}x{} {:?} (coarse {:?} Pa, refined {:?} Pa, shortfall {:?})",
        c.turns_along_normal,
        c.tapes_along_width,
        c.pressure_coverage_status,
        c.recomputed_transverse_pressure_pa,
        c.refined.transverse_pressure_pa,
        c.pressure_shortfall_fraction,
    )
}

/// One candidate's adjacent-screen summary for the `adjacent_screens`
/// check's human-readable detail: each declared screen's agreement
/// verdict with its recorded→recomputed status pair.
fn format_screens_agreement(c: &RecomputedCandidate) -> String {
    let Some(screens) = &c.recomputed_screens else {
        return format!(
            "{}x{} not evaluated (no declared screens or unrealizable pack)",
            c.turns_along_normal, c.tapes_along_width
        );
    };
    let entry = |name: &str, e: &Option<RecomputedScreenAgreement>| {
        e.as_ref().map(|e| {
            format!(
                "{name} {:?} (recorded {:?} -> recomputed {:?})",
                e.status, e.recorded_status, e.recomputed_status
            )
        })
    };
    [
        entry("thermal_margin", &screens.thermal_margin),
        entry("ac_loss", &screens.ac_loss),
        entry("quench_hotspot", &screens.quench_hotspot),
        entry("screening_current", &screens.screening_current),
        entry("transition", &screens.transition),
        entry("quench_transient", &screens.quench_transient),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ")
}

/// Estimated field evaluations for one `recompute_one` leg: the
/// fresh-evaluator probes (bore + good-field lattice + harmonics
/// circle, final order only), the original-plan rerun, and — when the
/// candidate claimed PASS — the §9.4 refined plan. Mirrors the search
/// side's `planned_field_evals`; written separately so the estimate can
/// drift independently rather than sharing a bug.
pub(crate) fn acceptance_planned_evals(
    search: &CoupledSearchCase,
    turns: u32,
    tapes: u32,
    candidate_passed: bool,
) -> u64 {
    let orders: u64 = if search.field_map.is_some() { 1 } else { 2 };
    let mut evals = 0_u64;
    if search.field_map.is_none() {
        evals += 1; // bore probe, final order
        if let Some(region) = &search.requirement.good_field_region {
            evals += region.lattice_points(search.requirement.bore_probe_m).len() as u64;
            if let Some(harmonics) = &region.harmonics {
                evals += u64::from(harmonics.theta_samples);
            }
        }
    }
    let turn_indices =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns).len() as u64;
    evals += search.sampling.stations.len() as u64 * turn_indices * u64::from(tapes) * 5 * orders;
    if candidate_passed {
        let refined_turns = refined_plan_turn_indices(
            turns,
            &expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns),
        )
        .len() as u64;
        evals += refined_plan_stations(search).len() as u64
            * refined_turns
            * u64::from(tapes)
            * 5
            * orders;
    }
    evals
}

fn tick(t: Option<&AtomicU64>) {
    if let Some(t) = t {
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

#[cfg(test)]
fn recompute_one(
    label: &str,
    index: usize,
    search: &CoupledSearchCase,
    original: &SearchCandidateResult,
    datasets: &BTreeMap<String, MaterialDataset>,
    // Progress tick: incremented once per field evaluation.
    progress_tick: Option<&AtomicU64>,
) -> Result<RecomputedCandidate, RunError> {
    recompute_one_cancellable(
        label,
        index,
        search,
        original,
        datasets,
        progress_tick,
        &AtomicBool::new(false),
    )
}

#[allow(clippy::too_many_arguments)]
fn recompute_one_cancellable(
    label: &str,
    index: usize,
    search: &CoupledSearchCase,
    original: &SearchCandidateResult,
    datasets: &BTreeMap<String, MaterialDataset>,
    progress_tick: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> Result<RecomputedCandidate, RunError> {
    check_cancelled(cancel)?;
    let turns = original.geometry.turns_along_normal;
    let tapes = original.geometry.tapes_along_width;
    // The candidate's recorded assignment (schema v10) — `None` on
    // ungraded cases and records predating run-schema v10.
    let tape_spec_ids = original.geometry.tape_spec_ids.as_deref();
    // Schema v21: the candidate's axis-resolved racetrack dims ride the
    // record — shadow them into the geometry every recomputation below
    // reads, so searched and fixed dims take one path.
    let dims = original.geometry.dims();
    let fg = search.fixed_geometry.resolved_for(dims);

    let recomputed_cost = recompute_cost(
        search,
        &fg,
        turns,
        tapes,
        original.geometry.strands_parallel,
        tape_spec_ids,
    );
    let search_cost_usd = original.cost.total_usd;
    let cost_relative_error =
        ((recomputed_cost.total_usd - search_cost_usd) / search_cost_usd.max(1.0)).abs();
    // Schema v20: the declared opex figure is part of what a tampered
    // record could lie about — compare it on the same 1e-8 gate. Both
    // sides are Option; mismatch on presence is disagreement by itself.
    let opex_relative_error = match (recomputed_cost.opex_usd, original.cost.opex_usd) {
        (Some(recomputed), Some(original)) => {
            ((recomputed - original) / original.abs().max(1.0)).abs()
        }
        (None, None) => 0.0,
        _ => f64::INFINITY,
    };
    let cost_agreement_status = if cost_relative_error <= 1e-8 && opex_relative_error <= 1e-8 {
        Status::Pass
    } else {
        Status::Fail
    };

    // Schema v13: under a declared field map the extents are the fixed
    // declarations, not count x pitch/width.
    let (radial_width_m, axial_height_m) = fg.candidate_extents_m(turns, tapes);
    let final_order = search.numerics.quadrature_orders[1];
    // Under a declared map there is no engine field to re-evaluate — the
    // bore field re-derives from the map producer's declared anchor
    // (magnetostatic linearity). The agreement check then verifies the
    // search carried the declared value through correctly, not that the
    // field itself is right — the record's limitations say so.
    let fresh_evaluator = if search.field_map.is_none() {
        Some(match &fg.path {
            Some(path) => RacetrackEvaluator::from_path(
                path,
                radial_width_m,
                axial_height_m,
                1.0,
                final_order,
            )?,
            None => {
                let geometry = Racetrack {
                    straight_half_length_m: fg.straight_half_length_m.ok_or_else(|| {
                        RunError::Invalid(
                            "fixed_geometry declares neither path nor racetrack dimensions"
                                .to_owned(),
                        )
                    })?,
                    bend_radius_m: fg.bend_radius_m.ok_or_else(|| {
                        RunError::Invalid(
                            "fixed_geometry declares neither path nor racetrack dimensions"
                                .to_owned(),
                        )
                    })?,
                    radial_width_m,
                    axial_height_m,
                    ampere_turns_a: 1.0,
                    current_model: CurrentModel::UniformWindingPack,
                };
                RacetrackEvaluator::new(&geometry, final_order)?
            }
        })
    } else {
        None
    };
    let recomputed_unit_bore_bz = match &search.field_map {
        Some(fm) => fm.bore_field_at_reference_t / fm.map.reference_ampere_turns_a(),
        None => {
            check_cancelled(cancel)?;
            let value = fresh_evaluator
                .as_ref()
                .expect("engine-field runs build the acceptance evaluator")
                .evaluate(search.requirement.bore_probe_m)?;
            tick(progress_tick);
            value.field_t[2]
        }
    };
    // Contract §9.2: compare fields at the candidate's own ampere-turns
    // (unit field x NI, in tesla), never a per-ampere-turn difference
    // against a tesla tolerance directly. Both sides share the same NI
    // (the candidate's own declared ampere_turns_a), so this is equivalent
    // to comparing the two candidates' actual bore fields in tesla.
    let field_absolute_error_t =
        (recomputed_unit_bore_bz - original.unit_bore_bz_t_per_ampere_turn).abs()
            * original.ampere_turns_a;
    let refinement_gate_t =
        search.numerics.field_scale_t * search.numerics.max_refinement_change_fraction;
    let field_agreement_status = if field_absolute_error_t <= refinement_gate_t {
        Status::Pass
    } else {
        Status::Fail
    };

    // Schema v3: the usable-volume minimum is independently re-evaluated on
    // the same lattice and compared under the same NI-scaled gate. Absent
    // region -> both sides are None and the check does not apply.
    let recomputed_region_extremes = match &search.requirement.good_field_region {
        None => None,
        Some(region) => Some(
            region
                .lattice_points(search.requirement.bore_probe_m)
                .iter()
                .map(|&point| {
                    check_cancelled(cancel)?;
                    fresh_evaluator
                        .as_ref()
                        .expect("good_field_region is forbidden under a declared field_map")
                        .evaluate(point)
                        .map_err(RunError::from)
                        .map(|value| {
                            tick(progress_tick);
                            value.field_t[2]
                        })
                })
                .collect::<Result<Vec<f64>, _>>()?
                .into_iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), bz| {
                    (lo.min(bz), hi.max(bz))
                }),
        ),
    };
    let recomputed_region_min = recomputed_region_extremes.map(|(lo, _)| lo);
    // Schema v22: the region's uniformity figure is independently
    // recomputed on this module's own center value — the deviation is
    // only as trustworthy as the extremes and the normalizer together.
    let recomputed_relative_deviation = recomputed_region_extremes.map(|(lo, hi)| {
        if recomputed_unit_bore_bz > 0.0 {
            (hi - lo) / recomputed_unit_bore_bz
        } else {
            f64::INFINITY
        }
    });
    // Schema v23: the midplane reference circle is independently
    // re-evaluated at the final order and the DFT recomputed — the
    // spectrum is only as trustworthy as this module's own samples.
    let recomputed_harmonics = match search
        .requirement
        .good_field_region
        .as_ref()
        .and_then(|region| region.harmonics.as_ref())
    {
        None => None,
        Some(harmonics) => {
            let n_theta = harmonics.theta_samples as usize;
            let probe = search.requirement.bore_probe_m;
            let mut circle = Vec::with_capacity(n_theta);
            for k in 0..n_theta {
                check_cancelled(cancel)?;
                let theta = 2.0 * std::f64::consts::PI * k as f64 / n_theta as f64;
                circle.push(
                    fresh_evaluator
                        .as_ref()
                        .expect("good_field_region is forbidden under a declared field_map")
                        .evaluate([
                            probe[0] + harmonics.reference_radius_m * theta.cos(),
                            probe[1] + harmonics.reference_radius_m * theta.sin(),
                            probe[2],
                        ])?
                        .field_t[2],
                );
                tick(progress_tick);
            }
            let b0 = circle.iter().sum::<f64>() / n_theta as f64;
            let coefficients = |unit: fn(f64) -> f64| -> Vec<f64> {
                (1..=harmonics.max_order)
                    .map(|n| {
                        let sum: f64 = circle
                            .iter()
                            .enumerate()
                            .map(|(k, &bz)| {
                                bz * unit(
                                    n as f64 * 2.0 * std::f64::consts::PI * k as f64
                                        / n_theta as f64,
                                )
                            })
                            .sum();
                        if b0 > 0.0 {
                            (2.0 * sum / n_theta as f64) / b0
                        } else {
                            f64::INFINITY
                        }
                    })
                    .collect()
            };
            Some((b0, coefficients(f64::cos), coefficients(f64::sin)))
        }
    };
    let region_field_absolute_error_t = match (recomputed_region_min, &original.good_field) {
        (Some(recomputed), Some(recorded)) => Some(
            (recomputed - recorded.min_unit_bz_t_per_ampere_turn).abs() * original.ampere_turns_a,
        ),
        _ => None,
    };
    // Fail closed on inconsistency: a declared region with no recorded
    // evaluation, or a recorded one the case never declared, is a record
    // that cannot substantiate its own requirement claim. The overlap
    // verdict is pure geometry and independently recomputed from the
    // candidate's own pack dimensions -- a mismatch means the record does
    // not describe the case it claims to evaluate.
    let recomputed_pack_overlap =
        search
            .requirement
            .good_field_region
            .as_ref()
            .map(|region| match &fg.path {
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
            });
    let overlap_mismatch = matches!(
        (recomputed_pack_overlap, &original.good_field),
        (Some(recomputed), Some(recorded)) if recomputed != recorded.pack_overlap
    );
    // Schema v22: the recorded deviation is compared under the same
    // NI-scaled absolute gate — deviations are unit-field-normalized,
    // so the error de-normalizes through the recorded center value.
    let deviation_absolute_error_t = match (recomputed_relative_deviation, &original.good_field) {
        (Some(recomputed), Some(recorded)) => recorded.relative_deviation.map(|recorded_dev| {
            (recomputed - recorded_dev).abs()
                * original.unit_bore_bz_t_per_ampere_turn
                * original.ampere_turns_a
        }),
        _ => None,
    };
    // Schema v23: the recorded spectrum is compared coefficient-wise,
    // de-normalized through the recorded b0 into a tesla error — a
    // record claiming a different multipole content than it evaluated
    // fails, and a declared expansion with no recorded spectrum fails
    // closed.
    let harmonics_declared = search
        .requirement
        .good_field_region
        .as_ref()
        .is_some_and(|region| region.harmonics.is_some());
    let harmonics_recorded = original.good_field.as_ref().is_some_and(|field| {
        field.harmonic_normal_units.is_some() && field.harmonic_skew_units.is_some()
    });
    let harmonic_absolute_error_t = match (recomputed_harmonics, &original.good_field) {
        (Some((b0_rec, normal_rec, skew_rec)), Some(recorded)) => {
            let b0_error = recorded
                .harmonic_b0_t_per_ampere_turn
                .map(|recorded_b0| (b0_rec - recorded_b0).abs() * original.ampere_turns_a);
            let coefficient_error = |recorded_units: Option<&Vec<f64>>,
                                     recomputed: &Vec<f64>,
                                     recorded_b0: Option<f64>| {
                match (recorded_units, recorded_b0) {
                    (Some(recorded), Some(rb0)) if recorded.len() == recomputed.len() => Some(
                        recorded
                            .iter()
                            .zip(recomputed)
                            .map(|(&r, &c)| (c - r).abs() * rb0.abs() * original.ampere_turns_a)
                            .fold(0.0_f64, f64::max),
                    ),
                    // A declared expansion with a missing or
                    // length-mismatched spectrum cannot substantiate
                    // its claim — unbounded error forces the gate.
                    _ => Some(f64::INFINITY),
                }
            };
            let normal_error = coefficient_error(
                recorded.harmonic_normal_units.as_ref(),
                &normal_rec,
                recorded.harmonic_b0_t_per_ampere_turn,
            );
            let skew_error = coefficient_error(
                recorded.harmonic_skew_units.as_ref(),
                &skew_rec,
                recorded.harmonic_b0_t_per_ampere_turn,
            );
            [b0_error, normal_error, skew_error]
                .into_iter()
                .flatten()
                .fold(0.0_f64, f64::max)
                .into()
        }
        _ if harmonics_declared => Some(f64::INFINITY),
        _ => None,
    };
    let region_agreement_status = match (
        region_field_absolute_error_t,
        search.requirement.good_field_region.is_some(),
        original.good_field.is_some(),
    ) {
        _ if overlap_mismatch => Status::Fail,
        _ if harmonics_declared && !harmonics_recorded => Status::Fail,
        (Some(error), _, _) if error > refinement_gate_t => Status::Fail,
        _ if deviation_absolute_error_t.is_some_and(|e| e > refinement_gate_t) => Status::Fail,
        _ if harmonic_absolute_error_t.is_some_and(|e| e > refinement_gate_t) => Status::Fail,
        (Some(_), _, _) => Status::Pass,
        (None, false, false) => Status::NotEvaluated,
        (None, _, _) => Status::Fail,
    };

    // Schema v3 manufacturing gate, recomputed from the same geometry
    // arithmetic the search used (min local curvature radius on a path).
    let inner_bend_radius_m = fg
        .min_inner_radius_m(radial_width_m)
        .unwrap_or(f64::NEG_INFINITY);
    // Schema v17: the tape's declared thickness over twice the inner
    // bend radius — the elastic-beam outer-fiber strain, recomputed from
    // this module's own radius and fail-closed against a declared bound
    // when it cannot resolve.
    let recomputed_bend_strain = search
        .manufacturing
        .as_ref()
        .and_then(|m| m.tape_thickness_m)
        .and_then(|t| (inner_bend_radius_m > 0.0).then_some(t / (2.0 * inner_bend_radius_m)));
    let recomputed_manufacturing_feasible = search.manufacturing.as_ref().map(|m| {
        inner_bend_radius_m >= m.min_inner_bend_radius_m
            && match m.max_bend_strain {
                Some(limit) => recomputed_bend_strain.is_some_and(|s| s <= limit),
                None => true,
            }
    });
    let manufacturing_agreement_status = match recomputed_manufacturing_feasible {
        None => Status::NotEvaluated,
        Some(feasible) if feasible == original.manufacturing_feasible => Status::Pass,
        Some(_) => Status::Fail,
    };

    // A candidate whose pack geometry is unrealizable (degenerate bend
    // radius, or — schema v10 — a grading region that collapses empty on
    // its turn count) claimed no field evaluation or screening verdict.
    // Acceptance reproduces exactly that: cost agreement still computed
    // (pure arithmetic), every field/screening agreement Fail or
    // NotEvaluated, overall agreement Fail — an unrealizable declared
    // baseline must surface as a failed acceptance, never abort the run.
    if !original.pack_geometry_valid {
        return Ok(RecomputedCandidate {
            label: label.into(),
            index,
            turns_along_normal: turns,
            tapes_along_width: tapes,
            recomputed_cost,
            search_cost_usd,
            cost_relative_error,
            cost_agreement_status,
            recomputed_unit_bore_bz_t_per_ampere_turn: f64::NAN,
            search_unit_bore_bz_t_per_ampere_turn: original.unit_bore_bz_t_per_ampere_turn,
            field_absolute_error_t: f64::NAN,
            field_agreement_status: Status::Fail,
            recomputed_region_min_unit_bz_t_per_ampere_turn: None,
            recomputed_relative_deviation: None,
            region_field_absolute_error_t: None,
            region_agreement_status: Status::Fail,
            manufacturing_agreement_status,
            mechanical_agreement_status: Status::NotEvaluated,
            recomputed_transverse_pressure_pa: None,
            recomputed_bend_strain,
            recomputed_membrane_tension_n_per_m: None,
            rerun_screening_status: Status::NotEvaluated,
            rerun_refinement_status: Status::NotEvaluated,
            rerun_numerical_status: Status::NotEvaluated,
            screening_status_agreement: false,
            coarse_min_allowed_screening_a: None,
            coarse_limiting: None,
            refined: SamplingRefinementResult {
                status: Status::NotEvaluated,
                min_allowed_screening_a: None,
                limiting: None,
                max_utilization: None,
                transverse_pressure_pa: None,
                stations_evaluated: 0,
                points_evaluated: 0,
            },
            sampling_shortfall_fraction: None,
            sampling_refinement_status: Status::NotEvaluated,
            pressure_shortfall_fraction: None,
            pressure_coverage_status: Status::NotEvaluated,
            recomputed_screens: None,
            screens_agreement_status: Status::NotEvaluated,
            agreement_status: Status::Fail,
        });
    }

    let full_stations = search.sampling.stations.clone();
    let full_turn_indices =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns);
    let full_tape_indices: Vec<u32> = (1..=tapes).collect();
    let coupled_case = build_coupled_case(
        search,
        turns,
        tapes,
        original.geometry.strands_parallel,
        original.operating_current_a,
        full_stations,
        full_turn_indices,
        full_tape_indices,
        tape_spec_ids,
        dims,
    )?;
    let case_json = serde_json::to_string(&coupled_case)?;
    let dataset_refs = declared_dataset_refs(&coupled_case, datasets);
    let rerun = run_coupled_case_with_datasets_ticked_cancellable(
        &case_json,
        None,
        &CoupledOptions::default(),
        &dataset_refs,
        progress_tick,
        cancel,
    )?;
    let rerun_candidate = &rerun.candidates[0];
    let rerun_screening_status = rerun_candidate.status;
    let rerun_refinement_status = rerun
        .checks
        .iter()
        .find(|c| c.id == "field_refinement")
        .map(|c| c.status)
        .unwrap_or(Status::Inconclusive);
    let rerun_numerical_status = rerun.numerical_status;

    // Search schema v12: this module's own transverse-pressure estimate,
    // accumulated from the rerun's per-turn normal loads. Deliberately
    // written separately from `coupled_search::peak_transverse_pressure`
    // (different collection shape and f64 cell boundaries rather than
    // integer midpoints) so a mistake in one is unlikely to be replicated
    // in the other — the same convention `recompute_cost` follows. Each
    // sampled turn's signed normal load is `I_op·⟨B·ŵ⟩` with physical B
    // the unit field times NI; the interface after a turn transmits the
    // larger of the two face-anchored cumulative loads, and the pressure
    // divides by the tape-width interface extent.
    let (recomputed_transverse_pressure_pa, recomputed_membrane_tension_n_per_m) =
        sampled_transverse_pressure(
            &rerun.stations,
            original.operating_current_a,
            original.ampere_turns_a,
            f64::from(original.geometry.turns_along_normal),
            coupled_case.winding.tape_width_m,
        );

    // Schema v4 mechanical screen, recomputed independently: the rerun's
    // own peak sampled field times the candidate's operating current,
    // compared against the declared limit and against the search's own
    // verdict.
    let mechanical_agreement_status = {
        let rerun_peak_t = rerun
            .stations
            .iter()
            .flat_map(|s| &s.tapes)
            .flat_map(|t| &t.per_candidate[0].points)
            .map(|p| p.magnitude_t)
            .fold(0.0_f64, f64::max);
        let recomputed_feasible = search.mechanical.as_ref().map(|m| {
            let load = (original.operating_current_a.is_finite() && rerun_peak_t.is_finite())
                .then_some(original.operating_current_a * rerun_peak_t);
            let lorentz_ok = load.is_some_and(|l| l <= m.max_lorentz_load_n_per_m);
            // OC-018 (schema v7): same hoop bound, written out again here —
            // per-strand load × the outermost turn radius ÷ the declared
            // tension section. A declared limit with no computable stress
            // is infeasible, matching the search's own semantics.
            let hoop_ok = match m.max_hoop_stress_pa {
                Some(limit) => m
                    .tension_section_area_m2
                    .and_then(|area| {
                        load.map(|l| {
                            let r_outer = search
                                .fixed_geometry
                                .max_outer_radius_m(fg.candidate_extents_m(turns, tapes).0)
                                .unwrap_or(f64::INFINITY);
                            (l / f64::from(original.geometry.strands_parallel.max(1))) * r_outer
                                / area
                        })
                    })
                    .is_some_and(|s| s <= limit),
                None => true,
            };
            // Schema v12: same transverse-pressure bound — a declared
            // limit with no computable pressure is infeasible, matching
            // the search's fail-closed semantics.
            let pressure_ok = match m.max_transverse_pressure_pa {
                Some(limit) => recomputed_transverse_pressure_pa.is_some_and(|p| p <= limit),
                None => true,
            };
            // Schema v17: same membrane-tension bound on this module's
            // own undivided interface-load resultant.
            let tension_ok = match m.max_membrane_tension_n_per_m {
                Some(limit) => recomputed_membrane_tension_n_per_m.is_some_and(|t| t <= limit),
                None => true,
            };
            lorentz_ok && hoop_ok && pressure_ok && tension_ok
        });
        match (recomputed_feasible, original.mechanical_feasible) {
            (None, _) => Status::NotEvaluated,
            (Some(r), Some(o)) if r == o => Status::Pass,
            (Some(_), _) => Status::Fail,
        }
    };

    // Run-schema v15: the declared adjacent screens, recomputed on this
    // module's own rerun and compared verdict-for-verdict with the
    // search's recorded screens. Under a declared field map the rerun's
    // per-point data propagates the customer-declared map — agreement
    // verifies that propagation, not the map's physical correctness.
    let (recomputed_screens, screens_agreement_status) =
        recompute_screens(search, &coupled_case, &rerun.stations, original, datasets)?;

    // Contract §9.4: the "coarse" minimum/limiting point for the shortfall
    // comparison is this module's own independent rerun above (the
    // declared 4-station/~15-turn plan), never the search's own bookkeeping.
    let coarse_min_allowed_screening_a = rerun_candidate.min_allowed_screening_a;
    let coarse_limiting = rerun_candidate.limiting.clone();

    let screening_status_agreement = original
        .screening
        .as_ref()
        .is_some_and(|s| s.status == rerun_screening_status);

    // Contract §9.4: the coarse sampling plan (4 stations, ~15 of n turns)
    // cannot by itself establish the true weakest location. Only meaningful
    // for a candidate that actually claimed PASS in the first place --
    // skip the (expensive) refined re-evaluation entirely (NotEvaluated)
    // for one that did not.
    let refined = if original.status == Status::Pass {
        run_refined_plan(
            search,
            dims,
            turns,
            tapes,
            original.geometry.strands_parallel,
            original.operating_current_a,
            datasets,
            tape_spec_ids,
            progress_tick,
            cancel,
        )?
    } else {
        SamplingRefinementResult {
            status: Status::NotEvaluated,
            min_allowed_screening_a: None,
            limiting: None,
            max_utilization: None,
            transverse_pressure_pa: None,
            stations_evaluated: 0,
            points_evaluated: 0,
        }
    };
    let (sampling_shortfall_fraction, sampling_refinement_status) = sampling_refinement_verdict(
        original.status,
        coarse_min_allowed_screening_a,
        &refined,
        search.refined_plan.max_sampling_shortfall_fraction,
    );
    let (pressure_shortfall_fraction, pressure_coverage_status) = pressure_coverage_verdict(
        original.status,
        search
            .mechanical
            .as_ref()
            .is_some_and(|m| m.max_transverse_pressure_pa.is_some()),
        recomputed_transverse_pressure_pa,
        refined.transverse_pressure_pa,
        search.refined_plan.max_sampling_shortfall_fraction,
    );

    let agreement_status = if cost_agreement_status == Status::Pass
        && field_agreement_status == Status::Pass
        && region_agreement_status != Status::Fail
        && manufacturing_agreement_status != Status::Fail
        && mechanical_agreement_status != Status::Fail
        && rerun_refinement_status == Status::Pass
        && screening_status_agreement
        && sampling_refinement_status != Status::Fail
        && pressure_coverage_status != Status::Fail
        && screens_agreement_status != Status::Fail
    {
        Status::Pass
    } else {
        Status::Fail
    };

    Ok(RecomputedCandidate {
        label: label.into(),
        index,
        turns_along_normal: turns,
        tapes_along_width: tapes,
        recomputed_cost,
        search_cost_usd,
        cost_relative_error,
        cost_agreement_status,
        recomputed_unit_bore_bz_t_per_ampere_turn: recomputed_unit_bore_bz,
        search_unit_bore_bz_t_per_ampere_turn: original.unit_bore_bz_t_per_ampere_turn,
        field_absolute_error_t,
        field_agreement_status,
        recomputed_region_min_unit_bz_t_per_ampere_turn: recomputed_region_min,
        recomputed_relative_deviation,
        region_field_absolute_error_t,
        region_agreement_status,
        manufacturing_agreement_status,
        mechanical_agreement_status,
        recomputed_transverse_pressure_pa,
        recomputed_bend_strain,
        recomputed_membrane_tension_n_per_m,
        rerun_screening_status,
        rerun_refinement_status,
        rerun_numerical_status,
        screening_status_agreement,
        coarse_min_allowed_screening_a,
        coarse_limiting,
        refined,
        sampling_shortfall_fraction,
        sampling_refinement_status,
        pressure_shortfall_fraction,
        pressure_coverage_status,
        recomputed_screens,
        screens_agreement_status,
        agreement_status,
    })
}

/// Per-(station, tape-column) signed normal loads by sampled turn index —
/// this module's own collection shape, deliberately different from
/// `coupled_search`'s column map so an accumulation mistake in one is
/// unlikely to be replicated in the other.
type LoadColumns = BTreeMap<(String, u32), BTreeMap<u32, f64>>;

/// `I_op·⟨B·ŵ⟩` per unit length at each sampled turn, physical B the
/// unit field times NI.
fn signed_normal_load_columns(
    stations: &[StationGroup],
    operating_current_a: f64,
    ampere_turns_a: f64,
) -> LoadColumns {
    let mut columns: LoadColumns = BTreeMap::new();
    for group in stations {
        for tape in &group.tapes {
            let (acc, n_w) = tape.points.iter().fold((0.0_f64, 0_u32), |(a, c), p| {
                (
                    a + (0..3)
                        .map(|k| p.unit_field_t_per_ampere_turn[1][k] * tape.frame.w[k])
                        .sum::<f64>(),
                    c + 1,
                )
            });
            if n_w > 0 {
                columns
                    .entry((tape.station.clone(), tape.tape_index))
                    .or_default()
                    .insert(
                        tape.turn_index,
                        operating_current_a * ampere_turns_a * acc / f64::from(n_w),
                    );
            }
        }
    }
    columns
}

/// This module's own trapezoid accumulation of a column's sampled turn
/// loads over `[1, N]` (f64 midpoint cell boundaries — the search side
/// uses integer midpoints): each interface transmits the larger of the
/// two face-anchored cumulative loads; the peak across interfaces and
/// columns, divided by the tape-width extent, is the pressure. Returns
/// `(pressure_pa, peak_load_n_per_m)` — the undivided peak load is the
/// schema-v17 membrane-tension resultant.
fn accumulate_interface_pressure(
    columns: &LoadColumns,
    turns_f: f64,
    tape_width_m: f64,
) -> (f64, f64) {
    let mut peak_load = 0.0_f64;
    for column in columns.values() {
        let entries: Vec<(f64, f64)> = column.iter().map(|(&t, &f)| (f64::from(t), f)).collect();
        if entries.is_empty() {
            continue;
        }
        let last = entries.len() - 1;
        let cell = |i: usize| -> (f64, f64) {
            let lo = if i == 0 {
                0.0
            } else {
                (entries[i - 1].0 + entries[i].0) * 0.5
            };
            let hi = if i == last {
                turns_f
            } else {
                (entries[i].0 + entries[i + 1].0) * 0.5
            };
            (lo, hi)
        };
        let total: f64 = entries
            .iter()
            .enumerate()
            .map(|(i, &(_, f))| {
                let (lo, hi) = cell(i);
                (hi - lo) * f
            })
            .sum();
        let mut running = 0.0_f64;
        for (i, &(_, f)) in entries.iter().enumerate() {
            let (lo, hi) = cell(i);
            running += (hi - lo) * f;
            peak_load = peak_load.max(running.abs().max((total - running).abs()));
        }
    }
    (peak_load / tape_width_m, peak_load)
}

/// `None` when the plan sampled no field at all — callers fold a missing
/// estimate into the verdicts the same fail-closed way the search does.
/// Returns `(pressure_pa, membrane_tension_n_per_m)` — the pressure's own
/// undivided numerator is the schema-v17 membrane-tension resultant.
fn sampled_transverse_pressure(
    stations: &[StationGroup],
    operating_current_a: f64,
    ampere_turns_a: f64,
    turns_f: f64,
    tape_width_m: f64,
) -> (Option<f64>, Option<f64>) {
    let columns = signed_normal_load_columns(stations, operating_current_a, ampere_turns_a);
    if columns.is_empty() {
        (None, None)
    } else {
        let (pressure, load) = accumulate_interface_pressure(&columns, turns_f, tape_width_m);
        (Some(pressure), Some(load))
    }
}

/// The pressure-resolution counterpart of `sampling_refinement_verdict`:
/// the declared plan's pressure estimate is checked against the refined
/// plan's, gated on the same declared `max_sampling_shortfall_fraction`.
/// `(1)` this candidate never claimed PASS -> NotEvaluated, no shortfall
/// computed; `(2)` no pressure bound declared -> NotEvaluated (the
/// resolution is not load-bearing), though the fraction is still recorded
/// when computable; `(3)` bound declared but either estimate unavailable
/// (or a zero coarse estimate) -> FAIL, fail-closed; `(4)` otherwise PASS
/// iff `(refined - coarse) / coarse`, clamped >= 0, stays within the gate.
fn pressure_coverage_verdict(
    original_status: Status,
    bound_declared: bool,
    coarse_pressure_pa: Option<f64>,
    refined_pressure_pa: Option<f64>,
    max_shortfall_fraction: f64,
) -> (Option<f64>, Status) {
    if original_status != Status::Pass {
        return (None, Status::NotEvaluated);
    }
    let shortfall = match (coarse_pressure_pa, refined_pressure_pa) {
        (Some(coarse), Some(refined)) if coarse > 0.0 => {
            Some(((refined - coarse) / coarse).max(0.0))
        }
        _ => None,
    };
    if !bound_declared {
        return (shortfall, Status::NotEvaluated);
    }
    match shortfall {
        Some(f) if f <= max_shortfall_fraction => (shortfall, Status::Pass),
        _ => (shortfall, Status::Fail),
    }
}

/// Contract §9.4's shortfall/verdict arithmetic, pulled out as a pure
/// function so it can be unit-tested directly against synthetic inputs,
/// independent of any real physics or material coverage: `(1)` this
/// candidate never claimed PASS -> NotEvaluated, no shortfall computed;
/// `(2)` either minimum is unavailable -> no shortfall fraction, and the
/// verdict is FAIL (fail-closed: an incomputable shortfall is never treated
/// as "within gate"); `(3)` otherwise the shortfall fraction is `(coarse -
/// refined) / coarse`, clamped to >= 0, and the verdict is PASS iff the
/// refined plan itself reports PASS *and* the shortfall stays within the
/// declared gate.
fn sampling_refinement_verdict(
    original_status: Status,
    coarse_min_allowed_screening_a: Option<f64>,
    refined: &SamplingRefinementResult,
    max_sampling_shortfall_fraction: f64,
) -> (Option<f64>, Status) {
    if original_status != Status::Pass {
        return (None, Status::NotEvaluated);
    }
    let shortfall_fraction = match (
        coarse_min_allowed_screening_a,
        refined.min_allowed_screening_a,
    ) {
        (Some(coarse_min), Some(refined_min)) if coarse_min > 0.0 => {
            Some(((coarse_min - refined_min) / coarse_min).max(0.0))
        }
        _ => None,
    };
    let shortfall_ok = shortfall_fraction.is_some_and(|f| f <= max_sampling_shortfall_fraction);
    let status = if refined.status == Status::Pass && shortfall_ok {
        Status::Pass
    } else {
        Status::Fail
    };
    (shortfall_fraction, status)
}

/// Contract §9.4: re-evaluate one candidate's screening on the refined
/// 10-station plan (the search's own 4 stations plus
/// `refined_plan.additional_stations`'s 6), using every 5th turn (plus 1,
/// 2, 3, n-2, n-1, n) and all tapes, with the thread pool partitioned
/// across stations -- `run_coupled_case` itself is not internally
/// threaded, so this splits the refined plan's stations into
/// `execution.max_threads` groups, each run through the real,
/// unmodified `run_coupled_case` entry point independently, then
/// combines (never spawns more than the case's own declared
/// `execution.max_threads`).
#[allow(clippy::too_many_arguments)]
fn run_refined_plan(
    search: &CoupledSearchCase,
    dims: CandidateDims,
    turns: u32,
    tapes: u32,
    strands: u32,
    current_a: f64,
    datasets: &BTreeMap<String, MaterialDataset>,
    tape_spec_ids: Option<&[String]>,
    // Progress tick: shared across the station-group threads.
    progress_tick: Option<&AtomicU64>,
    cancel: &AtomicBool,
) -> Result<SamplingRefinementResult, RunError> {
    check_cancelled(cancel)?;
    // Schema v21: axis-resolved dims shadow the fixed declarations —
    // the refined plan evaluates the same searched geometry.
    let fg = search.fixed_geometry.resolved_for(dims);
    let stations = refined_plan_stations(search);
    let coarse_turn_indices =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, turns);
    let turn_indices = refined_plan_turn_indices(turns, &coarse_turn_indices);
    let tape_indices: Vec<u32> = (1..=tapes).collect();
    let points_evaluated = (stations.len() * turn_indices.len() * tape_indices.len() * 5) as u64;

    let thread_count = (search.execution.max_threads as usize)
        .max(1)
        .min(stations.len().max(1));
    let mut groups: Vec<Vec<Station>> = vec![Vec::new(); thread_count];
    for (i, station) in stations.into_iter().enumerate() {
        groups[i % thread_count].push(station);
    }
    let groups: Vec<Vec<Station>> = groups.into_iter().filter(|g| !g.is_empty()).collect();

    /// One station-group thread's contribution: its `CandidateResult`
    /// plus the sampled load columns its stations produced.
    type RefinedPart = (CandidateResult, LoadColumns);
    let outcomes: Mutex<Vec<Result<RefinedPart, RunError>>> =
        Mutex::new(Vec::with_capacity(groups.len()));
    std::thread::scope(|scope| {
        for group in &groups {
            scope.spawn(|| {
                let outcome = (|| -> Result<RefinedPart, RunError> {
                    let case = build_coupled_case(
                        search,
                        turns,
                        tapes,
                        strands,
                        current_a,
                        group.clone(),
                        turn_indices.clone(),
                        tape_indices.clone(),
                        tape_spec_ids,
                        dims,
                    )?;
                    let case_json = serde_json::to_string(&case)?;
                    let dataset_refs = declared_dataset_refs(&case, datasets);
                    let record = run_coupled_case_with_datasets_ticked_cancellable(
                        &case_json,
                        None,
                        &CoupledOptions::default(),
                        &dataset_refs,
                        progress_tick,
                        cancel,
                    )?;
                    // This module's own per-turn normal loads on the
                    // refined plan's denser sampling — the pressure
                    // coverage reference. `current_a` is the candidate's
                    // operating current; the NI factor comes from the
                    // rerun's own record.
                    let loads = signed_normal_load_columns(
                        &record.stations,
                        current_a,
                        record.candidates[0].ampere_turns_a,
                    );
                    Ok((record.candidates[0].clone(), loads))
                })();
                outcomes
                    .lock()
                    .expect("refined-plan results mutex poisoned")
                    .push(outcome);
            });
        }
    });
    check_cancelled(cancel)?;
    let parts: Vec<RefinedPart> = outcomes
        .into_inner()
        .expect("refined-plan results mutex poisoned")
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    let status = aggregate_status(parts.iter().map(|(p, _)| p.status));
    let mut min_allowed_screening_a: Option<f64> = None;
    let mut limiting: Option<LimitingPoint> = None;
    let mut max_utilization: Option<f64> = None;
    let mut pressure_columns: LoadColumns = BTreeMap::new();
    for (part, loads) in &parts {
        if let Some(allowed) = part.min_allowed_screening_a
            && min_allowed_screening_a.is_none_or(|current| allowed < current)
        {
            min_allowed_screening_a = Some(allowed);
            limiting = part.limiting.clone();
        }
        if let Some(utilization) = part.max_utilization {
            max_utilization =
                Some(max_utilization.map_or(utilization, |current: f64| current.max(utilization)));
        }
        for (key, column) in loads {
            pressure_columns
                .entry(key.clone())
                .or_default()
                .extend(column);
        }
    }
    // Station groups partition the refined stations, so their column maps
    // are disjoint-keyed by construction; the merged map is the refined
    // plan's full sampled load profile.
    let transverse_pressure_pa = if pressure_columns.is_empty() {
        None
    } else {
        Some(accumulate_interface_pressure(&pressure_columns, f64::from(turns), fg.tape_width_m).0)
    };

    Ok(SamplingRefinementResult {
        status,
        min_allowed_screening_a,
        limiting,
        max_utilization,
        transverse_pressure_pa,
        stations_evaluated: groups.iter().map(|g| g.len()).sum(),
        points_evaluated,
    })
}

// ---------------------------------------------------------------------
// Run-schema v15: the four first-order adjacent screens, recomputed here
// on this module's own rerun — every implementation below is written
// separately from `coupled_search`'s screen evaluation (different
// iteration shape, accumulation order, and root-find structure) so a
// mistake in one is unlikely to be replicated in the other. The physics
// primitives the case semantics name — the tape-frame decomposition, the
// mirror-pair interpolator query, μ₀ — are the shared data layer, the
// same role `RacetrackEvaluator` already plays for the field check.
// ---------------------------------------------------------------------

/// `U(T)` on the declared quench table by `partition_point` — an
/// inversion walk written independently of the search's window scan.
fn acceptance_u_at(table: &[[f64; 2]], temperature_k: f64) -> f64 {
    let i = table.partition_point(|row| row[0] < temperature_k);
    if i == 0 {
        return table[0][1];
    }
    if i >= table.len() {
        return table[table.len() - 1][1];
    }
    let (t0, u0) = (table[i - 1][0], table[i - 1][1]);
    let (t1, u1) = (table[i][0], table[i][1]);
    u0 + (u1 - u0) * (temperature_k - t0) / (t1 - t0)
}

/// The adiabatic hot-spot bound, recomputed from `I_op` alone: `MIITs =
/// J_strand²·τ/2` resolved against the declared `U(T)` table. `None`'s
/// status is INCONCLUSIVE on table exhaustion, NOT_EVALUATED on a
/// non-physical current.
fn acceptance_quench(
    quench: &optcoil_model::coupled_search::QuenchHotspotScreen,
    operating_current_a: f64,
    strands_parallel: u32,
    operating_temperature_k: f64,
) -> (Status, Option<f64>) {
    if !operating_current_a.is_finite() || operating_current_a <= 0.0 {
        return (Status::NotEvaluated, None);
    }
    let j = (operating_current_a / f64::from(strands_parallel.max(1))) / quench.stabilizer_area_m2;
    let miit = j * j * quench.dump_time_constant_s / 2.0;
    let table = &quench.quench_function_a2s_per_m4;
    let u_required = acceptance_u_at(table, operating_temperature_k) + miit;
    if u_required > table[table.len() - 1][1] {
        return (Status::Inconclusive, None);
    }
    // Invert: first row index whose U reaches the requirement, then
    // linear back into that row's segment.
    let i = table.partition_point(|row| row[1] < u_required);
    let t_hs = if i == 0 {
        table[0][0]
    } else if i >= table.len() {
        table[table.len() - 1][0]
    } else {
        let (t0, u0) = (table[i - 1][0], table[i - 1][1]);
        let (t1, u1) = (table[i][0], table[i][1]);
        if u1 > u0 {
            t0 + (t1 - t0) * (u_required - u0) / (u1 - u0)
        } else {
            t0
        }
    };
    (
        if t_hs <= quench.max_hotspot_k {
            Status::Pass
        } else {
            Status::Fail
        },
        Some(t_hs),
    )
}

/// One position's loop length, written out from the case's own offsets
/// (the same arithmetic `recompute_cost` prices) rather than calling the
/// search ledger's or `CoilPath`'s helpers.
fn acceptance_loop_length_m(
    search: &CoupledSearchCase,
    fg: &optcoil_model::coupled_search::FixedGeometry,
    turns: u32,
    tapes: u32,
    turn_index: u32,
    tape_index: u32,
) -> f64 {
    let offset_m = if search.field_map.is_some() {
        let extent_m = search
            .fixed_geometry
            .pack_radial_width_m
            .expect("v13 validation requires fixed extents");
        match fg.tape_normal {
            TapeNormal::Radial => {
                -extent_m / 2.0 + (f64::from(turn_index) - 0.5) * extent_m / f64::from(turns)
            }
            TapeNormal::Axial => {
                -extent_m / 2.0 + (f64::from(tape_index) - 0.5) * extent_m / f64::from(tapes)
            }
        }
    } else {
        let span_m = f64::from(turns) * fg.radial_pitch_m;
        -span_m / 2.0 + (f64::from(turn_index) - 0.5) * fg.radial_pitch_m
    };
    match (&fg.path, &fg.path3d) {
        (Some(path), _) => path
            .segments
            .iter()
            .map(|segment| match *segment {
                optcoil_model::path::PathSegment::Line { length_m } => length_m,
                optcoil_model::path::PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => (radius_m + sweep_deg.signum() * offset_m) * sweep_deg.to_radians().abs(),
            })
            .sum(),
        // Schema v19: the cylinder-radial offset turn at `R + δ` is a
        // helix of unchanged pitch — `|turns|·2π·sqrt((R+δ)² + c²)`
        // metres per segment with `c = rise/(2π)`, written out rather
        // than calling `CoilPath3D::length_at_offset_m` so the check
        // cannot inherit a mistake from the shared helper.
        (None, Some(path3d)) => path3d
            .segments
            .iter()
            .map(|segment| match *segment {
                optcoil_model::path3d::PathSegment3D::Helix {
                    radius_m,
                    turns,
                    rise_per_turn_m,
                    ..
                } => {
                    let sweep_rad = turns.abs() * 2.0 * std::f64::consts::PI;
                    let rise_per_rad_m = rise_per_turn_m / (2.0 * std::f64::consts::PI);
                    sweep_rad * (radius_m + offset_m).hypot(rise_per_rad_m)
                }
            })
            .sum(),
        (None, None) => {
            4.0 * search
                .fixed_geometry
                .straight_half_length_m
                .unwrap_or(f64::NAN)
                + 2.0 * std::f64::consts::PI * (fg.bend_radius_m.unwrap_or(f64::NAN) + offset_m)
        }
    }
}

/// One point's `T_cs − T_op`, recomputed with a different root-find
/// organization than the search's: a 33-point resolvability scan
/// isolates the dataset's coverage edge (refined by bisection), then a
/// second bisection lands the crossing. `(margin, true)` is the same
/// lower-bound convention the search reports when capacity still
/// exceeds demand at the resolvable ceiling.
#[allow(clippy::too_many_arguments)]
fn acceptance_temperature_margin_k(
    interpolator: &IcInterpolator,
    low_field_clamp_t: f64,
    query_field_t: f64,
    angle_folded_deg: f64,
    mirror_angle_deg: f64,
    k_demand_a_per_m: f64,
    t_op_k: f64,
    t_top_k: f64,
) -> Result<Option<(f64, bool)>, RunError> {
    let capacity = |t: f64| -> Result<Option<f64>, RunError> {
        Ok(tape_frame::query_mirror_pair_with_clamp(
            interpolator,
            t,
            query_field_t,
            angle_folded_deg,
            mirror_angle_deg,
            low_field_clamp_t,
        )?
        .k_used_a_per_m)
    };
    let Some(k_op) = capacity(t_op_k)? else {
        return Ok(None);
    };
    if k_op <= k_demand_a_per_m {
        return Ok(Some((0.0, false)));
    }
    if t_top_k <= t_op_k {
        return Ok(None);
    }
    // Resolvable ceiling: scan 32 cells; on the first unresolved cell
    // bisect the coverage edge. Every preceding grid point resolved, so
    // the scan's `t_prev` is always a resolved anchor.
    let mut t_ceiling = t_top_k;
    let mut t_prev = t_op_k;
    for i in 1..=32_u32 {
        let t = t_op_k + (t_top_k - t_op_k) * f64::from(i) / 32.0;
        if capacity(t)?.is_none() {
            let (mut lo, mut hi) = (t_prev, t);
            for _ in 0..32 {
                let mid = 0.5 * (lo + hi);
                if capacity(mid)?.is_some() {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            t_ceiling = lo;
            break;
        }
        t_prev = t;
    }
    let Some(k_ceiling) = capacity(t_ceiling)? else {
        return Ok(None);
    };
    if k_ceiling > k_demand_a_per_m {
        return Ok(Some((t_ceiling - t_op_k, true)));
    }
    // The crossing lies in (t_op_k, t_ceiling]: capacity is monotone
    // nonincreasing in T on every shipped dataset (a screen assumption).
    let (mut lo, mut hi) = (t_op_k, t_ceiling);
    for _ in 0..36 {
        let mid = 0.5 * (lo + hi);
        match capacity(mid)? {
            Some(k) if k > k_demand_a_per_m => lo = mid,
            _ => hi = mid,
        }
    }
    Ok(Some((hi - t_op_k, false)))
}

/// The agreement verdict between one recomputed screen outcome and the
/// search's recorded one: statuses must match exactly; headline values
/// must agree within a loose tolerance (the two root-finds/accumulations
/// are structurally different, so bit-equality is not the gate —
/// divergence beyond ~1e-6 relative is), and both may be absent
/// together. `recorded` is `None` when the candidate never claimed the
/// screen — a declared screen the record cannot substantiate fails.
fn screen_agreement(
    recomputed_status: Status,
    recomputed_value: Option<f64>,
    recorded_status: Option<Status>,
    recorded_value: Option<f64>,
    extra_mismatch: bool,
) -> RecomputedScreenAgreement {
    let recorded_status = recorded_status.unwrap_or(Status::NotEvaluated);
    let values_close = match (recomputed_value, recorded_value) {
        (Some(a), Some(b)) => (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0),
        (None, None) => true,
        _ => false,
    };
    let status = if recomputed_status == recorded_status && values_close && !extra_mismatch {
        Status::Pass
    } else {
        Status::Fail
    };
    RecomputedScreenAgreement {
        recomputed_status,
        recorded_status,
        recomputed_value,
        recorded_value,
        status,
    }
}

/// One sharing point's inputs to the quench-transient recomputation —
/// collected during the station loop, integrated afterward. The voltage
/// weight `w_q·dℓ/n_stations` uses this module's own loop-length
/// arithmetic.
struct TransientSeed<'a> {
    runtime: &'a SpecRuntime,
    query_field_t: f64,
    angle_folded_deg: f64,
    mirror_angle_deg: f64,
    voltage_weight: f64,
}

/// The acceptance side's lumped adiabatic driven-dump recomputation —
/// the same declared scheme (`optcoil_coupled_search`'s
/// `QUENCH_TRANSIENT_TAUS`/`STEPS` fixed-step Euler is part of the model
/// identity) organized per-point: each sharing point's trajectory is
/// integrated to completion, adding its per-step resistive field into a
/// shared series-voltage series. Per-point evolution is the same
/// sequence of operations the search performs, so trajectories agree to
/// machine precision; the voltage fold differs in association only.
/// Returns (peak T with a left-coverage flag, detection time, peak
/// series voltage, whether any trajectory exhausted coverage).
fn acceptance_quench_transient(
    declared: &optcoil_model::coupled_search::QuenchTransientScreen,
    t0_k: f64,
    k_demand_a_per_m: f64,
    seeds: &[TransientSeed<'_>],
) -> Result<(f64, bool, Option<f64>, f64, bool), RunError> {
    use crate::coupled_search::{QUENCH_TRANSIENT_STEPS, QUENCH_TRANSIENT_TAUS};
    let tau = declared.dump_time_constant_s;
    let dt = QUENCH_TRANSIENT_TAUS * tau / f64::from(QUENCH_TRANSIENT_STEPS);
    let a_pw = declared.conducting_area_per_width_m;
    let n = QUENCH_TRANSIENT_STEPS as usize;
    let mut voltage_series = vec![0.0_f64; n + 1];
    let mut peak_temperature_k = f64::NEG_INFINITY;
    let mut peak_left_coverage = false;
    let mut coverage_exhausted = false;
    for seed in seeds {
        let mut temperature_k = t0_k;
        let mut left_coverage = false;
        for (i, slot) in voltage_series.iter_mut().enumerate() {
            let t_i = f64::from(i as u32) * dt;
            let clamped = tape_frame::query_mirror_pair_with_clamp(
                &seed.runtime.interpolator,
                temperature_k,
                seed.query_field_t,
                seed.angle_folded_deg,
                seed.mirror_angle_deg,
                seed.runtime.material.low_field_clamp_t,
            )?;
            let Some(k_used) = clamped.k_used_a_per_m else {
                left_coverage = true;
                break;
            };
            let k_m = (k_demand_a_per_m * (-t_i / tau).exp() - k_used).max(0.0);
            if k_m == 0.0 {
                break;
            }
            let (Some(rho), Some(c_v)) = (
                acceptance_declared_table_at(&declared.resistivity_ohm_m, temperature_k),
                acceptance_declared_table_at(&declared.heat_capacity_j_per_m3k, temperature_k),
            ) else {
                left_coverage = true;
                break;
            };
            let j_m = k_m / a_pw;
            *slot += j_m * rho * seed.voltage_weight;
            if i < n {
                temperature_k += j_m * j_m * rho / c_v * dt;
            }
        }
        coverage_exhausted |= left_coverage;
        if temperature_k > peak_temperature_k {
            peak_temperature_k = temperature_k;
            peak_left_coverage = left_coverage;
        }
    }
    // Detection: the first step whose series voltage crosses the
    // declared threshold — same linear in-step estimate as the search.
    let mut detection_time_s: Option<f64> = None;
    let mut peak_series_voltage_v = 0.0_f64;
    if let Some(threshold) = declared.detection_voltage_v {
        for (i, &v) in voltage_series.iter().enumerate() {
            peak_series_voltage_v = peak_series_voltage_v.max(v);
            if detection_time_s.is_none() && v > threshold {
                detection_time_s = Some(if i == 0 {
                    0.0
                } else {
                    let prev = voltage_series[i - 1];
                    let frac = if v > prev {
                        ((threshold - prev) / (v - prev)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    f64::from(i as u32) * dt - dt + frac * dt
                });
            }
        }
    } else {
        peak_series_voltage_v = voltage_series.iter().copied().fold(0.0, f64::max);
    }
    Ok((
        peak_temperature_k,
        peak_left_coverage,
        detection_time_s,
        peak_series_voltage_v,
        coverage_exhausted,
    ))
}

/// The acceptance module's own piecewise-linear declared-table lookup —
/// no extrapolation; `None` outside the declared span or on nonfinite
/// input. Same convention as `declared_table_at` on the search side,
/// written separately.
fn acceptance_declared_table_at(table: &[[f64; 2]], t: f64) -> Option<f64> {
    if !t.is_finite() || t < table[0][0] || t > table[table.len() - 1][0] {
        return None;
    }
    table
        .windows(2)
        .find(|w| t <= w[1][0])
        .map(|w| w[0][1] + (w[1][1] - w[0][1]) * (t - w[0][0]) / (w[1][0] - w[0][0]))
        .or(Some(table[table.len() - 1][1]))
}

/// Recompute every declared screen on this module's own rerun and
/// compare each against the search's recorded result. Called with the
/// generated `coupled_case` and the rerun's station records.
fn recompute_screens(
    search: &CoupledSearchCase,
    coupled_case: &optcoil_model::coupled::CoupledCase,
    rerun_stations: &[StationGroup],
    original: &SearchCandidateResult,
    datasets: &BTreeMap<String, MaterialDataset>,
) -> Result<(Option<RecomputedScreens>, Status), RunError> {
    let any_declared = search.thermal_margin.is_some()
        || search.ac_loss.is_some()
        || search.quench_hotspot.is_some()
        || search.screening_current.is_some()
        || search.transition.is_some()
        || search.quench_transient.is_some();
    if !any_declared {
        return Ok((None, Status::NotEvaluated));
    }
    let recorded = original.screens.as_ref();
    let turns = original.geometry.turns_along_normal;
    let tapes = original.geometry.tapes_along_width;
    let strands = f64::from(original.geometry.strands_parallel.max(1));
    // Schema v21: axis-resolved dims shadow the fixed declarations.
    let fg = search.fixed_geometry.resolved_for(original.geometry.dims());
    let tape_width_m = coupled_case.winding.tape_width_m;
    let n_stations = rerun_stations.len().max(1) as f64;
    let t_op_k = search.operating.temperature_k;
    let k_demand_a_per_m = original.operating_current_a / (tape_width_m * strands);

    // Per-binding runtimes and per-dataset temperature ceilings, built
    // when a screen needs interpolator access: thermal-margin's T_cs
    // root-find or the transition screen's measured-exponent re-query.
    let needs_runtime = search.thermal_margin.is_some()
        || search.transition.is_some()
        || search.quench_transient.is_some();
    let critical_state_strip =
        search.limits.self_field_correction.as_deref() == Some("critical_state_strip");
    let mut spec_runtimes: BTreeMap<String, SpecRuntime> = BTreeMap::new();
    let mut t_tops: BTreeMap<String, f64> = BTreeMap::new();
    let base_runtime = if needs_runtime {
        for (id, spec) in search.tape_specs.iter().flat_map(|s| s.iter()) {
            spec_runtimes.insert(
                id.clone(),
                spec_runtime(
                    &spec.material,
                    &datasets[&spec.material.dataset_id],
                    t_op_k,
                    critical_state_strip,
                )?,
            );
        }
        for d in datasets.values() {
            t_tops.insert(
                d.metadata.id.clone(),
                d.points
                    .iter()
                    .map(|p| p.nominal_temperature_k)
                    .fold(f64::NEG_INFINITY, f64::max),
            );
        }
        Some(spec_runtime(
            &search.material,
            &datasets[&search.material.dataset_id],
            t_op_k,
            critical_state_strip,
        )?)
    } else {
        None
    };
    let runtime_for_turn = |turn_index: u32| -> &SpecRuntime {
        match coupled_case.winding.spec_for_turn(turn_index) {
            Some(id) => spec_runtimes.get(id).unwrap_or_else(|| {
                base_runtime
                    .as_ref()
                    .expect("declared screens build runtimes")
            }),
            None => base_runtime
                .as_ref()
                .expect("declared screens build runtimes"),
        }
    };

    // Independently organized accumulators — the screening screen folds a
    // maximum; the loss screen accumulates per-position cells keyed by
    // (turn, tape); the thermal screen keeps (value, lower_bound, counts).
    let mut screen_max: Option<(f64, f64)> = None; // (fraction, ratio)
    let mut screen_evaluated = 0_u64;
    let mut screen_inconclusive = 0_u64;
    let mut loss_cells: BTreeMap<(u32, u32), [f64; 3]> = BTreeMap::new();
    let mut loss_evaluated = 0_u64;
    let mut loss_inconclusive = 0_u64;
    let mut margin_best: Option<(f64, bool)> = None;
    let mut margin_evaluated = 0_u64;
    let mut margin_inconclusive = 0_u64;
    // The transition screen's own accumulators: worst (depth, u, n) by
    // running max and a per-point voltage sum — resolved inside the
    // point loop rather than folded over per-position cells.
    let mut trans_max: Option<(f64, f64, f64)> = None;
    let mut trans_voltage_v = 0.0_f64;
    let mut trans_evaluated = 0_u64;
    let mut trans_inconclusive = 0_u64;
    // The quench-transient screen collects its sharing points during
    // the loop, then integrates each point's full trajectory afterward
    // — per-point sequential organization, deliberately different from
    // the search side's step-fused pass over all live trajectories.
    let mut transient_seeds: Vec<TransientSeed<'_>> = Vec::new();
    let mut transient_evaluated = 0_u64;
    let mut transient_inconclusive = 0_u64;
    let mut transient_static_peak_k = f64::NEG_INFINITY;

    let i_ac = search
        .ac_loss
        .as_ref()
        .map(|a| a.transport_amplitude_fraction)
        .unwrap_or(0.0);
    let g_i = if search.ac_loss.is_some() {
        (1.0 - i_ac) * (1.0 - i_ac).ln() + (1.0 + i_ac) * (1.0 + i_ac).ln() - i_ac * i_ac
    } else {
        0.0
    };
    let d_sc = search
        .ac_loss
        .as_ref()
        .map(|a| a.sc_layer_thickness_m)
        .unwrap_or(1.0);

    for group in rerun_stations {
        for tape in &group.tapes {
            let frame = optcoil_model::coupled::TapeFrame {
                t: tape.frame.t,
                n: tape.frame.n,
                w: tape.frame.w,
            };
            for p in &tape.per_candidate[0].points {
                // The screening-current flag.
                if search.screening_current.is_some() {
                    match p.k_used_a_per_m {
                        Some(k_c) => {
                            screen_evaluated += 1;
                            let b_n = tape_frame::decompose(p.field_t, &frame).b_n.abs();
                            let b_c = MU0_H_PER_M * k_c / std::f64::consts::PI;
                            let ratio = if b_c > 0.0 { b_n / b_c } else { f64::INFINITY };
                            let fraction = if ratio.is_finite() {
                                1.0 - 1.0 / ratio.cosh()
                            } else {
                                1.0
                            };
                            if screen_max.is_none_or(|(f, _)| fraction > f) {
                                screen_max = Some((fraction, ratio));
                            }
                        }
                        None => screen_inconclusive += 1,
                    }
                }
                // The hysteretic loss estimate-plus-bound.
                if search.ac_loss.is_some() {
                    match p.k_used_a_per_m {
                        Some(k_c) => {
                            loss_evaluated += 1;
                            let c = tape_frame::decompose(p.field_t, &frame);
                            let ic_a = k_c * tape_width_m;
                            let q_transport =
                                MU0_H_PER_M * ic_a * ic_a / std::f64::consts::PI * g_i;
                            let b_par = c.b_w.abs();
                            let b_p = MU0_H_PER_M * (k_c / d_sc) * (d_sc / 2.0);
                            let q_vol = if b_par <= b_p {
                                2.0 * b_par.powi(3) / (3.0 * MU0_H_PER_M * b_p)
                            } else {
                                (2.0 * b_p / MU0_H_PER_M) * (b_par - 2.0 * b_p / 3.0)
                            };
                            let q_parallel = q_vol * tape_width_m * d_sc;
                            let q_perp = k_c * tape_width_m * tape_width_m * c.b_n.abs();
                            let w = tape_frame::LOBATTO_WEIGHTS[p.width_index as usize];
                            let cell = loss_cells
                                .entry((tape.turn_index, tape.tape_index))
                                .or_default();
                            cell[0] += w * q_transport;
                            cell[1] += w * q_parallel;
                            cell[2] += w * q_perp;
                        }
                        None => loss_inconclusive += 1,
                    }
                }
                // The current-sharing temperature margin.
                if search.thermal_margin.is_some() {
                    match p.k_used_a_per_m {
                        Some(_) => {
                            let runtime = runtime_for_turn(tape.turn_index);
                            let t_top = t_tops
                                .get(&runtime.material.dataset_id)
                                .copied()
                                .unwrap_or(f64::NEG_INFINITY);
                            match acceptance_temperature_margin_k(
                                &runtime.interpolator,
                                runtime.material.low_field_clamp_t,
                                p.query_field_t,
                                p.angle_folded_deg,
                                p.mirror_angle_deg,
                                k_demand_a_per_m,
                                t_op_k,
                                t_top,
                            )? {
                                Some((margin, lower_bound)) => {
                                    margin_evaluated += 1;
                                    let better = match margin_best {
                                        None => true,
                                        Some((m, b)) => {
                                            margin < m || (margin == m && b && !lower_bound)
                                        }
                                    };
                                    if better {
                                        margin_best = Some((margin, lower_bound));
                                    }
                                }
                                None => margin_inconclusive += 1,
                            }
                        }
                        None => margin_inconclusive += 1,
                    }
                }
                // The measured E–J transition depth — re-queried on this
                // module's own runtime at the point's recorded query
                // coordinates; the per-strand voltage accumulates E·dℓ
                // directly per point (strands parallel, no multiplier).
                if search.transition.is_some() {
                    match p.k_used_a_per_m {
                        Some(k_used) => {
                            let runtime = runtime_for_turn(tape.turn_index);
                            let clamped = tape_frame::query_mirror_pair_with_clamp(
                                &runtime.interpolator,
                                t_op_k,
                                p.query_field_t,
                                p.angle_folded_deg,
                                p.mirror_angle_deg,
                                runtime.material.low_field_clamp_t,
                            )?;
                            match clamped.n_value {
                                Some(n) => {
                                    trans_evaluated += 1;
                                    let u = k_demand_a_per_m / k_used;
                                    let e_ratio = if u.is_finite() && u >= 0.0 {
                                        u.powf(n)
                                    } else {
                                        f64::INFINITY
                                    };
                                    trans_voltage_v +=
                                        search.operating.electric_field_criterion_v_per_m
                                            * e_ratio
                                            * tape_frame::LOBATTO_WEIGHTS[p.width_index as usize]
                                            * acceptance_loop_length_m(
                                                search,
                                                &fg,
                                                turns,
                                                tapes,
                                                tape.turn_index,
                                                tape.tape_index,
                                            )
                                            / n_stations;
                                    if trans_max.is_none_or(|(e, _, _)| e_ratio > e) {
                                        trans_max = Some((e_ratio, u, n));
                                    }
                                }
                                None => trans_inconclusive += 1,
                            }
                        }
                        None => trans_inconclusive += 1,
                    }
                }
                // The lumped quench transient: classify each point now
                // on its lumped initial temperature T₀ (the declared
                // "quench detected" start, or T_op) — capacity at T₀ is
                // re-queried on this module's own runtime, not taken
                // from the recorded T_op value. Sharing points' seeds
                // collect; the trajectories integrate after the loop.
                if let Some(transient) = &search.quench_transient {
                    let t0 = transient.initial_temperature_k.unwrap_or(t_op_k);
                    let runtime = runtime_for_turn(tape.turn_index);
                    let clamped = tape_frame::query_mirror_pair_with_clamp(
                        &runtime.interpolator,
                        t0,
                        p.query_field_t,
                        p.angle_folded_deg,
                        p.mirror_angle_deg,
                        runtime.material.low_field_clamp_t,
                    )?;
                    match clamped.k_used_a_per_m {
                        Some(k_used) => {
                            transient_evaluated += 1;
                            if k_demand_a_per_m > k_used {
                                transient_seeds.push(TransientSeed {
                                    runtime,
                                    query_field_t: p.query_field_t,
                                    angle_folded_deg: p.angle_folded_deg,
                                    mirror_angle_deg: p.mirror_angle_deg,
                                    voltage_weight: tape_frame::LOBATTO_WEIGHTS
                                        [p.width_index as usize]
                                        * acceptance_loop_length_m(
                                            search,
                                            &fg,
                                            turns,
                                            tapes,
                                            tape.turn_index,
                                            tape.tape_index,
                                        )
                                        / n_stations,
                                });
                            } else {
                                transient_static_peak_k = transient_static_peak_k.max(t0);
                            }
                        }
                        None => transient_inconclusive += 1,
                    }
                }
            }
        }
    }

    let thermal_margin = search.thermal_margin.as_ref().map(|declared| {
        let recomputed_status = if margin_inconclusive > 0 || margin_evaluated == 0 {
            Status::Inconclusive
        } else if margin_best.is_some_and(|(m, _)| m >= declared.min_margin_k) {
            Status::Pass
        } else {
            Status::Fail
        };
        let rec: Option<&ThermalMarginScreenRecord> =
            recorded.and_then(|s| s.thermal_margin.as_ref());
        let lb_mismatch = rec.is_some_and(|r| {
            r.min_margin_is_lower_bound != margin_best.map(|(_, b)| b).unwrap_or(false)
        });
        screen_agreement(
            recomputed_status,
            margin_best.map(|(m, _)| m),
            rec.map(|r| r.status),
            rec.and_then(|r| r.min_margin_k),
            lb_mismatch,
        )
    });

    let ac_loss = search.ac_loss.as_ref().map(|declared| {
        let totals = loss_cells
            .iter()
            .fold([0.0_f64; 3], |acc, ((turn, tape), e)| {
                let len = acceptance_loop_length_m(search, &fg, turns, tapes, *turn, *tape);
                let metres = len / n_stations * strands;
                [
                    acc[0] + e[0] * metres,
                    acc[1] + e[1] * metres,
                    acc[2] + e[2] * metres,
                ]
            });
        let recomputed_total = (loss_evaluated > 0)
            .then_some((totals[0] + totals[1] + totals[2]) * declared.frequency_hz);
        let recomputed_status = if loss_inconclusive > 0 || recomputed_total.is_none() {
            Status::Inconclusive
        } else if recomputed_total.is_some_and(|t| t <= declared.max_loss_w) {
            Status::Pass
        } else {
            Status::Fail
        };
        let rec: Option<&AcLossScreenRecord> = recorded.and_then(|s| s.ac_loss.as_ref());
        screen_agreement(
            recomputed_status,
            recomputed_total,
            rec.map(|r| r.status),
            rec.and_then(|r| r.total_loss_w),
            false,
        )
    });

    let quench_hotspot = search.quench_hotspot.as_ref().map(|declared| {
        let (recomputed_status, recomputed_t) = acceptance_quench(
            declared,
            original.operating_current_a,
            original.geometry.strands_parallel,
            t_op_k,
        );
        let rec: Option<&QuenchHotspotScreenRecord> =
            recorded.and_then(|s| s.quench_hotspot.as_ref());
        let exhaustion_mismatch = rec.is_some_and(|r| {
            r.table_exhausted
                != (recomputed_status == Status::Inconclusive && recomputed_t.is_none())
        });
        screen_agreement(
            recomputed_status,
            recomputed_t,
            rec.map(|r| r.status),
            rec.and_then(|r| r.hotspot_temperature_k),
            exhaustion_mismatch,
        )
    });

    let screening_current = search.screening_current.as_ref().map(|declared| {
        let recomputed_status = if screen_inconclusive > 0 || screen_evaluated == 0 {
            Status::Inconclusive
        } else if screen_max.is_some_and(|(f, _)| f <= declared.max_penetrated_width_fraction) {
            Status::Pass
        } else {
            Status::Fail
        };
        let rec: Option<&ScreeningCurrentScreenRecord> =
            recorded.and_then(|s| s.screening_current.as_ref());
        screen_agreement(
            recomputed_status,
            screen_max.map(|(f, _)| f),
            rec.map(|r| r.status),
            rec.and_then(|r| r.max_penetrated_width_fraction),
            false,
        )
    });

    let transition = search.transition.as_ref().map(|declared| {
        let recomputed_status = if trans_inconclusive > 0 || trans_evaluated == 0 {
            Status::Inconclusive
        } else if trans_max.is_some_and(|(e, _, _)| e <= declared.max_e_over_ec)
            && declared.max_voltage_v.is_none_or(|v| trans_voltage_v <= v)
        {
            Status::Pass
        } else {
            Status::Fail
        };
        let rec: Option<&TransitionScreenRecord> = recorded.and_then(|s| s.transition.as_ref());
        // The headline comparison is the worst `E/Ec`; the per-strand
        // terminal-voltage estimate is additionally compared as an extra
        // mismatch beyond the loose agreement tolerance.
        let voltage_mismatch = match (
            (trans_evaluated > 0).then_some(trans_voltage_v),
            rec.and_then(|r| r.terminal_voltage_v),
        ) {
            (Some(a), Some(b)) => (a - b).abs() > 1e-6 * a.abs().max(b.abs()).max(1.0),
            (None, None) => false,
            _ => true,
        };
        screen_agreement(
            recomputed_status,
            trans_max.map(|(e, _, _)| e),
            rec.map(|r| r.status),
            rec.and_then(|r| r.max_e_over_ec),
            voltage_mismatch,
        )
    });

    let quench_transient = search
        .quench_transient
        .as_ref()
        .map(|declared| {
            // The recorded record — needed for both the headline
            // comparison and the detection-time/coverage-flag
            // mismatches.
            let rec: Option<&QuenchTransientScreenRecord> =
                recorded.and_then(|s| s.quench_transient.as_ref());
            let t0_k = declared.initial_temperature_k.unwrap_or(t_op_k);
            let (
                recomputed_peak_k,
                recomputed_peak_left_coverage,
                recomputed_detection_s,
                recomputed_peak_voltage_v,
                recomputed_exhausted,
            ) = acceptance_quench_transient(declared, t0_k, k_demand_a_per_m, &transient_seeds)?;
            // The static points' contribution to the peak — every
            // evaluated point holds at least T₀, so the peak is
            // `max(sharing peaks, T₀)` whenever any point evaluated.
            let recomputed_peak_k = if transient_evaluated > 0 {
                Some(transient_static_peak_k.max(t0_k).max(recomputed_peak_k))
            } else {
                None
            };
            // Status algebra mirroring the declared screen's semantics,
            // re-derived here: temperature bound, detection bound, and the
            // coverage-exhaustion/unresolved rules.
            let mut saw_fail = false;
            let mut saw_unresolved = false;
            if let Some(limit) = declared.max_temperature_k {
                match recomputed_peak_k {
                    Some(p) if p > limit => saw_fail = true,
                    Some(_) => {
                        if recomputed_exhausted {
                            saw_unresolved = true;
                        }
                    }
                    None => saw_unresolved = true,
                }
            }
            if let Some(bound) = declared.max_detection_time_s {
                match recomputed_detection_s {
                    Some(t) if t <= bound => {}
                    Some(_) => saw_fail = true,
                    None => {
                        if recomputed_peak_voltage_v > 0.0 {
                            if recomputed_exhausted {
                                saw_unresolved = true;
                            } else {
                                saw_fail = true;
                            }
                        } else if declared.initial_temperature_k.is_some() {
                            // A declared quench with no signature is
                            // undetectable by construction.
                            saw_fail = true;
                        }
                    }
                }
            }
            if declared.max_temperature_k.is_none()
                && declared.max_detection_time_s.is_none()
                && recomputed_exhausted
            {
                saw_unresolved = true;
            }
            let recomputed_status = if transient_inconclusive > 0 || transient_evaluated == 0 {
                Status::Inconclusive
            } else if saw_fail {
                Status::Fail
            } else if saw_unresolved {
                Status::Inconclusive
            } else {
                Status::Pass
            };
            // Beyond the headline value: detection time, peak series
            // voltage, and the coverage flags must agree too — a tampered
            // trajectory record fails on any of them.
            let detection_mismatch =
                match (recomputed_detection_s, rec.and_then(|r| r.detection_time_s)) {
                    (Some(a), Some(b)) => (a - b).abs() > 1e-6 * a.abs().max(b.abs()).max(1e-9),
                    (None, None) => false,
                    _ => true,
                };
            let voltage_mismatch = match (
                (transient_evaluated > 0).then_some(recomputed_peak_voltage_v),
                rec.and_then(|r| r.peak_series_voltage_v),
            ) {
                (Some(a), Some(b)) => (a - b).abs() > 1e-6 * a.abs().max(b.abs()).max(1e-12),
                (None, None) => false,
                _ => true,
            };
            let flag_mismatch = rec.is_some_and(|r| {
                r.coverage_exhausted != recomputed_exhausted
                    || r.peak_temperature_is_lower_bound != recomputed_peak_left_coverage
            });
            Ok::<_, RunError>(screen_agreement(
                recomputed_status,
                recomputed_peak_k,
                rec.map(|r| r.status),
                rec.and_then(|r| r.peak_temperature_k),
                detection_mismatch || voltage_mismatch || flag_mismatch,
            ))
        })
        .transpose()?;

    let screens = RecomputedScreens {
        thermal_margin,
        ac_loss,
        quench_hotspot,
        screening_current,
        transition,
        quench_transient,
    };
    let status = {
        let mut entries = [
            screens.thermal_margin.as_ref(),
            screens.ac_loss.as_ref(),
            screens.quench_hotspot.as_ref(),
            screens.screening_current.as_ref(),
            screens.transition.as_ref(),
            screens.quench_transient.as_ref(),
        ]
        .into_iter()
        .flatten();
        if entries.any(|e| e.status == Status::Fail) {
            Status::Fail
        } else {
            Status::Pass
        }
    };
    Ok((Some(screens), status))
}

/// Independent recomputation of the D6 cost formula, written separately
/// from `coupled_search::compute_cost_ledger` (different variable names and
/// loop shape) so a mistake in one is unlikely to be replicated in the
/// other — the same relationship `acceptance.rs::assess` has to
/// `lib.rs::objective` for OC-001.
fn recompute_cost(
    search: &CoupledSearchCase,
    fg: &optcoil_model::coupled_search::FixedGeometry,
    turns: u32,
    tapes: u32,
    strands: u32,
    tape_spec_ids: Option<&[String]>,
) -> SearchCostLedger {
    let pitch_m = fg.radial_pitch_m;
    let half_width_m = f64::from(turns) * pitch_m / 2.0;
    // Schema v13: under a declared field map the pack footprint is the
    // fixed declaration — and under an axial tape normal the turn index
    // advances axially, so turn row k's conductors are the `tapes` tapes
    // spread across the *declared* radial extent, each on its own hoop.
    // Under a radial normal the row's conductors share the turn's hoop
    // at its offset across the declared extent. Written out
    // per-conductor here rather than via the search ledger's symmetric
    // `tapes x L(0)` shortcut. Legacy cases keep the historical
    // `turns x radial_pitch` span — bit-identical ledgers for v1-v12.
    let row_offsets_m: Vec<Vec<f64>> = if search.field_map.is_some() {
        let radial_extent_m = search
            .fixed_geometry
            .pack_radial_width_m
            .expect("v13 validation requires fixed extents");
        match fg.tape_normal {
            TapeNormal::Radial => (1..=turns)
                .map(|k| {
                    vec![
                        -radial_extent_m / 2.0
                            + (f64::from(k) - 0.5) * radial_extent_m / f64::from(turns),
                    ]
                })
                .collect(),
            TapeNormal::Axial => (1..=turns)
                .map(|_| {
                    (1..=tapes)
                        .map(|i| {
                            -radial_extent_m / 2.0
                                + (f64::from(i) - 0.5) * radial_extent_m / f64::from(tapes)
                        })
                        .collect()
                })
                .collect(),
        }
    } else {
        (1..=turns)
            .map(|k| vec![-half_width_m + (f64::from(k) - 0.5) * pitch_m])
            .collect()
    };
    // Schema v10: a turn's price is its covering region's assigned spec.
    // Resolved here directly from `grading.regions`' fraction bounds —
    // never via `spec_id_for_turn`/`spec_price_usd_per_m`, so the
    // recomputation can't inherit a mistake in the search's own helpers.
    let price_for_turn = |turn_number: u32| -> f64 {
        let mut price = search.cost.price_usd_per_m;
        if let (Some(grading), Some(assignment)) = (&search.grading, tape_spec_ids) {
            for (region, spec_id) in grading.regions.iter().zip(assignment.iter()) {
                let first = (region.turn_range[0] * f64::from(turns)).floor() as u32 + 1;
                let last = (region.turn_range[1] * f64::from(turns)).floor() as u32;
                if last >= first
                    && (first..=last).contains(&turn_number)
                    && spec_id != BASE_TAPE_SPEC_ID
                    && let Some(specs) = &search.tape_specs
                    && let Some(spec) = specs.get(spec_id)
                {
                    price = spec.price_usd_per_m;
                }
            }
        }
        price
    };
    // Schema v24: the spec id covering each turn — resolved here
    // directly from `grading.regions`' fraction bounds, never via
    // `spec_id_for_turn`, so the recomputation can't inherit a mistake
    // in the search's own helpers.
    let spec_id_for_turn = |turn_number: u32| -> String {
        let mut id = BASE_TAPE_SPEC_ID.to_string();
        if let (Some(grading), Some(assignment)) = (&search.grading, tape_spec_ids) {
            for (region, spec_id) in grading.regions.iter().zip(assignment.iter()) {
                let first = (region.turn_range[0] * f64::from(turns)).floor() as u32 + 1;
                let last = (region.turn_range[1] * f64::from(turns)).floor() as u32;
                if last >= first && (first..=last).contains(&turn_number) {
                    id = spec_id.clone();
                }
            }
        }
        id
    };
    let mut total_perimeter_m = 0.0_f64;
    let mut conductor_usd_per_layer = 0.0_f64;
    // Schema v24: per-row conductor-unit lengths ride alongside for the
    // piece-policy walk below; empty vec otherwise.
    let mut row_lengths_m: Vec<f64> = Vec::new();
    match (&fg.path, &fg.path3d) {
        // Schema v9 path geometry: the turn at signed offset `o` from the
        // centerline measures `length + 2*pi*o` on straights… write it out
        // segment by segment — a line keeps its length, an arc of radius
        // R sweeping θ measures (R + sign(θ)·o)·|θ|. Independently
        // re-derived rather than calling CoilPath::length_at_offset_m so
        // the check cannot inherit a mistake from it.
        (Some(path), _) => {
            for (row, offsets_m) in row_offsets_m.iter().enumerate() {
                let mut row_length_m = 0.0;
                for &offset_m in offsets_m {
                    for segment in &path.segments {
                        row_length_m += match *segment {
                            optcoil_model::path::PathSegment::Line { length_m } => length_m,
                            optcoil_model::path::PathSegment::Arc {
                                radius_m,
                                sweep_deg,
                            } => {
                                (radius_m + sweep_deg.signum() * offset_m)
                                    * sweep_deg.to_radians().abs()
                            }
                        };
                    }
                }
                total_perimeter_m += row_length_m;
                row_lengths_m.push(row_length_m);
                conductor_usd_per_layer += row_length_m * price_for_turn(row as u32 + 1);
            }
        }
        // Schema v19 helix geometry: the turn at cylinder-radial offset
        // `o` is a helix of radius `R + o` and unchanged rise — written
        // out segment by segment as `|turns|·2π·hypot(R+o, rise/2π)`
        // rather than calling `CoilPath3D::length_at_offset_m`, so the
        // check cannot inherit a mistake from it.
        (None, Some(path3d)) => {
            for (row, offsets_m) in row_offsets_m.iter().enumerate() {
                let mut row_length_m = 0.0;
                for &offset_m in offsets_m {
                    for segment in &path3d.segments {
                        row_length_m += match *segment {
                            optcoil_model::path3d::PathSegment3D::Helix {
                                radius_m,
                                turns,
                                rise_per_turn_m,
                                ..
                            } => {
                                turns.abs()
                                    * 2.0
                                    * std::f64::consts::PI
                                    * (radius_m + offset_m)
                                        .hypot(rise_per_turn_m / (2.0 * std::f64::consts::PI))
                            }
                        };
                    }
                }
                total_perimeter_m += row_length_m;
                row_lengths_m.push(row_length_m);
                conductor_usd_per_layer += row_length_m * price_for_turn(row as u32 + 1);
            }
        }
        (None, None) => {
            let bend_radius_m = fg.bend_radius_m.unwrap_or(f64::NAN);
            for (row, offsets_m) in row_offsets_m.iter().enumerate() {
                let mut row_length_m = 0.0;
                for &offset_m in offsets_m {
                    let radius_m = bend_radius_m + offset_m;
                    row_length_m += 4.0 * fg.straight_half_length_m.unwrap_or(f64::NAN)
                        + 2.0 * std::f64::consts::PI * radius_m;
                }
                total_perimeter_m += row_length_m;
                row_lengths_m.push(row_length_m);
                conductor_usd_per_layer += row_length_m * price_for_turn(row as u32 + 1);
            }
        }
    }
    let conductors_per_row = if search.field_map.is_some() && fg.tape_normal == TapeNormal::Axial {
        // The tape count was enumerated in each row's offsets above.
        1.0
    } else {
        f64::from(tapes)
    };
    let conductor_layers = conductors_per_row * f64::from(strands);
    let installed_length_m = total_perimeter_m * conductor_layers;
    let assembly_usd = search.cost.assembly_cost_per_pancake_usd * f64::from(tapes);
    // Schema v24: the piece-quantized ledger, recomputed on this module's
    // own arithmetic — the stream build, run lengths, offering argmin
    // and splice accounting below deliberately differ in shape from
    // `coupled_search::piece_plan_ledger`.
    let mut piece_cols: Option<(u32, u32, u32, u32, f64, Vec<SpecPiecePlan>)> = None;
    let (conductor_usd, scrap_usd, purchased_length_m, joints_usd) =
        if let Some(policy) = &search.cost.piece_policy {
            use optcoil_model::coupled_search::{PieceBoundary, PieceUnit};
            // One module's ordered (spec, unit-length) walk.
            let module_walk: Vec<(String, f64)> = row_lengths_m
                .iter()
                .enumerate()
                .map(|(i, &l)| (spec_id_for_turn(i as u32 + 1), l))
                .collect();
            // Spec runs of that walk, in order.
            let mut module_runs: Vec<(String, f64)> = Vec::new();
            for (spec, len) in &module_walk {
                match module_runs.last_mut() {
                    Some((s, total)) if s == spec => *total += len,
                    _ => module_runs.push((spec.clone(), *len)),
                }
            }
            let transitions = module_runs.len().saturating_sub(1) as u32;
            // The winding's piece streams: per_module → every module
            // re-walks the run list and resets pieces at its boundary;
            // continuous → one stream whose layer runs cover all `tapes`
            // axial positions.
            let mut stream: Vec<(String, f64)> = Vec::new();
            let (module_joints, spec_transitions) = match policy.boundary {
                PieceBoundary::PerModule => {
                    for _ in 0..tapes {
                        stream.extend(module_runs.iter().cloned());
                    }
                    (tapes.saturating_sub(1), tapes * transitions)
                }
                PieceBoundary::Continuous => {
                    for (spec, len) in &module_runs {
                        stream.push((spec.clone(), len * f64::from(tapes)));
                    }
                    (0, transitions)
                }
            };
            // Offerings resolved per spec — own catalogue, else base
            // catalogue, else the policy's scalar at the spec's own rate.
            let offerings_of = |spec: &str| -> Vec<(f64, f64)> {
                if let Some(specs) = &search.tape_specs
                    && let Some(s) = specs.get(spec)
                    && let Some(o) = &s.piece_offerings
                {
                    return o.iter().map(|o| (o.length_m, o.price_usd_per_m)).collect();
                }
                if let Some(o) = &search.cost.piece_offerings {
                    return o.iter().map(|o| (o.length_m, o.price_usd_per_m)).collect();
                }
                let p = policy
                    .piece_length_m
                    .expect("v24 validation requires a base piece source");
                let price = if spec == BASE_TAPE_SPEC_ID {
                    search.cost.price_usd_per_m
                } else {
                    search
                        .tape_specs
                        .as_ref()
                        .and_then(|s| s.get(spec))
                        .map(|s| s.price_usd_per_m)
                        .expect("turn spec resolves on a validated case")
                };
                vec![(p, price)]
            };
            let mult = match policy.piece_unit {
                PieceUnit::ConductorUnit => 1u32,
                PieceUnit::PerStrand => strands,
            };
            // Group stream runs by spec, preserving first-appearance order.
            let mut spec_order: Vec<String> = Vec::new();
            let mut by_spec: Vec<(String, Vec<f64>)> = Vec::new();
            for (spec, len) in &stream {
                match by_spec.iter_mut().find(|(s, _)| s == spec) {
                    Some((_, lens)) => lens.push(*len),
                    None => {
                        spec_order.push(spec.clone());
                        by_spec.push((spec.clone(), vec![*len]));
                    }
                }
            }
            let mut plans: Vec<SpecPiecePlan> = Vec::new();
            for (spec, lens) in &by_spec {
                let mut best: Option<(f64, f64, f64, u32, u32, f64, f64)> = None;
                for &(len_m, price) in &offerings_of(spec) {
                    let mut n_pieces = 0u32;
                    let mut n_splices = 0u32;
                    let mut remnant_u = 0.0;
                    let mut purchased_u = 0.0;
                    for &run in lens {
                        let needed = run * (1.0 + search.cost.scrap_fraction);
                        let n = (needed / len_m).ceil().max(1.0) as u32;
                        n_pieces += n;
                        n_splices += n - 1;
                        purchased_u += f64::from(n) * len_m;
                        remnant_u += f64::from(n) * len_m - needed;
                    }
                    let cost = purchased_u * price * f64::from(strands)
                        + f64::from(n_splices * mult) * policy.splice_cost_usd;
                    if best.map(|b| cost < b.0).unwrap_or(true) {
                        best = Some((
                            cost,
                            len_m,
                            price,
                            n_pieces,
                            n_splices,
                            remnant_u,
                            purchased_u,
                        ));
                    }
                }
                let (_, len_m, price, n_pieces, n_splices, remnant_u, purchased_u) =
                    best.expect("v24 catalogues are nonempty");
                plans.push(SpecPiecePlan {
                    spec_id: spec.clone(),
                    piece_length_m: len_m,
                    price_usd_per_m: price,
                    pieces: n_pieces * mult,
                    piece_splices: n_splices * mult,
                    remnant_length_m: remnant_u * f64::from(strands),
                    purchased_length_m: purchased_u * f64::from(strands),
                });
            }
            // Installed tape-metres per spec for the conductor spend.
            let installed_of = |spec: &str| -> f64 {
                let mut unit_m = 0.0;
                for (s, l) in &module_walk {
                    if s == spec {
                        unit_m += l;
                    }
                }
                unit_m * f64::from(tapes) * f64::from(strands)
            };
            let mut conductor = 0.0_f64;
            let mut scrap = 0.0_f64;
            let mut purchased = 0.0_f64;
            let mut remnant = 0.0_f64;
            let mut piece_splices = 0u32;
            let mut pieces = 0u32;
            for plan in &plans {
                let installed = installed_of(&plan.spec_id);
                conductor += installed * plan.price_usd_per_m;
                scrap += (plan.purchased_length_m - installed) * plan.price_usd_per_m;
                purchased += plan.purchased_length_m;
                remnant += plan.remnant_length_m;
                piece_splices += plan.piece_splices;
                pieces += plan.pieces;
            }
            let joints = search.cost.joint_cost_usd * f64::from(module_joints)
                + policy.splice_cost_usd * f64::from(piece_splices + spec_transitions * mult);
            piece_cols = Some((
                module_joints,
                piece_splices,
                spec_transitions * mult,
                pieces,
                remnant,
                plans,
            ));
            (conductor, scrap, purchased, joints)
        } else {
            let scrap_length_m = installed_length_m * search.cost.scrap_fraction;
            let purchased_length_m = installed_length_m + scrap_length_m;
            // Ungraded cases keep the legacy ordering (bit-identical to the
            // search ledger's own ungraded branch); graded cases multiply the
            // per-turn price-weighted length, scrap at each turn's own price.
            let (conductor_usd, scrap_usd) = if search.grading.is_some() {
                let conductor_usd = conductor_usd_per_layer * conductor_layers;
                (conductor_usd, conductor_usd * search.cost.scrap_fraction)
            } else {
                (
                    installed_length_m * search.cost.price_usd_per_m,
                    scrap_length_m * search.cost.price_usd_per_m,
                )
            };
            let joints_usd = search.cost.joint_cost_usd * f64::from(tapes.saturating_sub(1));
            (conductor_usd, scrap_usd, purchased_length_m, joints_usd)
        };
    let total_usd = conductor_usd + scrap_usd + assembly_usd + joints_usd;
    // Schema v20: the declared opex terms re-derived here on this
    // module's own arithmetic — the search side must not be the only
    // place the Carnot-scaled input power is computed.
    let opex_usd = search.opex.as_ref().map(|opex| {
        let cold_w: f64 = opex.heat_loads_w.iter().map(|h| h.power_w).sum();
        let input_w = cold_w * (opex.sink_temperature_k - search.operating.temperature_k)
            / (search.operating.temperature_k * opex.cop_fraction_of_carnot);
        input_w * opex.operating_hours_per_year / 1000.0
            * opex.electricity_usd_per_kwh
            * f64::from(opex.operating_years)
    });
    let (module_joints, piece_splices, spec_splices, pieces_bought, remnant_length_m, piece_plan) =
        piece_cols
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coupled_search::{CoupledSearchOptions, run_coupled_search_case};

    fn reduced_case_json() -> &'static str {
        r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "acceptance-test-case",
  "provenance": "search_acceptance.rs unit test fixture; not a frozen benchmark.",
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
  "pruning": null,
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
    }

    #[test]
    fn acceptance_agrees_with_a_clean_search_run() {
        // This reduced fixture's tiny/synthetic geometry means the refined
        // §9.4 plan's much denser sampling (versus the search's own single
        // declared station) can genuinely land a query at a near-normal
        // incidence angle (transverse field component crossing zero) the
        // measured dataset does not cover -- an authentic, expected
        // coverage limitation (see `sampling_refinement_verdict`'s own
        // dedicated tests for the pass/fail arithmetic in isolation from
        // real physics), not a bug: the assertions below check what a
        // "clean" run actually agrees on (cost, field, the coarse
        // screening-status rerun) without presupposing the refined plan
        // itself is always fully determined.
        let run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Pass);
        assert_eq!(acceptance.baseline.field_agreement_status, Status::Pass);
        assert_eq!(
            acceptance.baseline.rerun_numerical_status,
            Status::Inconclusive
        );
        assert!(acceptance.baseline.screening_status_agreement);
        assert_eq!(acceptance.baseline.rerun_refinement_status, Status::Pass);
        // The refined-plan gate is fully exercised (never skipped) for a
        // baseline whose own original status was PASS.
        assert_ne!(
            acceptance.baseline.sampling_refinement_status,
            Status::NotEvaluated
        );
        if let Some(best) = &acceptance.best {
            assert_eq!(best.cost_agreement_status, Status::Pass);
            assert_eq!(best.field_agreement_status, Status::Pass);
            assert!(best.screening_status_agreement);
            assert_eq!(best.rerun_refinement_status, Status::Pass);
        }
        // agreement_status is exactly the AND of its own recorded sub-checks
        // (including the §9.4 gate) -- verified structurally rather than
        // assumed to be Pass, since the refined-plan outcome above is
        // data-dependent.
        let expected_baseline_agreement = if acceptance.baseline.cost_agreement_status
            == Status::Pass
            && acceptance.baseline.field_agreement_status == Status::Pass
            && acceptance.baseline.rerun_refinement_status == Status::Pass
            && acceptance.baseline.screening_status_agreement
            && acceptance.baseline.sampling_refinement_status != Status::Fail
        {
            Status::Pass
        } else {
            Status::Fail
        };
        assert_eq!(
            acceptance.baseline.agreement_status,
            expected_baseline_agreement
        );
    }

    #[test]
    fn identical_baseline_and_best_reuse_one_independent_acceptance_rerun() {
        let run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let progress = SearchProgress::new();
        for _ in 0..run.candidates.len() + 2 {
            progress.add_leg(1);
        }
        let acceptance = assess_progress_cancellable(
            &run.case,
            &run.candidates,
            run.baseline_index,
            Some(run.baseline_index),
            &datasets,
            Some(&progress),
            &AtomicBool::new(false),
        )
        .unwrap();
        let best = acceptance
            .best
            .as_ref()
            .expect("baseline is also selected best");
        assert_eq!(best.label, "best");
        assert_eq!(best.index, acceptance.baseline.index);
        assert_eq!(
            best.cost_agreement_status,
            acceptance.baseline.cost_agreement_status
        );
        assert_eq!(
            best.field_agreement_status,
            acceptance.baseline.field_agreement_status
        );
        let baseline_leg = progress.leg(run.candidates.len()).unwrap();
        let best_leg = progress.leg(run.candidates.len() + 1).unwrap();
        assert!(
            baseline_leg.load(Ordering::Relaxed) > 0,
            "baseline acceptance rerun should perform field work"
        );
        assert_eq!(
            best_leg.load(Ordering::Relaxed),
            0,
            "identical optimum must not repeat its field work"
        );
        assert_eq!(
            progress.legs.lock().unwrap()[run.candidates.len() + 1].planned,
            0
        );
    }

    #[test]
    fn acceptance_detects_a_planted_cost_disagreement() {
        let mut run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        run.candidates[run.baseline_index].cost.total_usd += 1.0;
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Fail);
        assert!(acceptance.baseline.cost_relative_error > 1e-8);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
        assert_eq!(acceptance.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_opex_disagreement() {
        // Schema v20: a tampered opex_usd must fail the cost-agreement
        // gate even though total_usd (capex) is untouched.
        let mut run = run_coupled_search_case(
            &crate::coupled_search::tests::v20_opex_case_json(),
            &CoupledSearchOptions::default(),
        )
        .unwrap();
        run.candidates[run.baseline_index].cost.opex_usd = run.candidates[run.baseline_index]
            .cost
            .opex_usd
            .map(|o| o + 1.0);
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
        assert_eq!(acceptance.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_field_disagreement() {
        let mut run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        run.candidates[run.baseline_index].unit_bore_bz_t_per_ampere_turn += 10.0;
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.field_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }

    /// The shared `reduced_case_json` fixture declares a deliberately loose
    /// `max_refinement_change_fraction` (0.5 T) so its other tests stay
    /// robust to normal floating-point noise. A genuine 0.4 T error (this
    /// test's planted discrepancy) would pass even a *correct* comparison
    /// against that gate, which would defeat the point of the test, so this
    /// tightens the gate to something comparable to the real D2 case
    /// (0.0025 T) without changing anything else about the cheap fixture.
    fn reduced_case_json_tight_refinement_gate() -> String {
        reduced_case_json().replace(
            r#""max_refinement_change_fraction": 0.5"#,
            r#""max_refinement_change_fraction": 0.0025"#,
        )
    }

    #[test]
    fn acceptance_scales_the_field_discrepancy_by_ampere_turns_not_a_bare_tesla_tolerance() {
        // Contract §9.2: a per-ampere-turn discrepancy of 1e-6 T/A-turn at
        // NI = 4e5 A-turn is a genuine 0.4 T error and must FAIL -- comparing
        // the raw 1e-6 T/A-turn difference directly against the (tesla)
        // refinement gate would wrongly PASS.
        let case_json = reduced_case_json_tight_refinement_gate();
        let run = run_coupled_search_case(&case_json, &CoupledSearchOptions::default()).unwrap();
        let case = &run.case;
        let mut candidate = run.candidates[run.baseline_index].clone();

        // The value the fresh evaluator inside recompute_one will
        // independently find, so the planted discrepancy below is exactly
        // 1e-6 T/A-turn relative to it.
        let radial_width_m =
            f64::from(candidate.geometry.turns_along_normal) * case.fixed_geometry.radial_pitch_m;
        let axial_height_m =
            f64::from(candidate.geometry.tapes_along_width) * case.fixed_geometry.tape_width_m;
        let geometry = Racetrack {
            straight_half_length_m: case.fixed_geometry.straight_half_length_m.unwrap(),
            bend_radius_m: case.fixed_geometry.bend_radius_m.unwrap(),
            radial_width_m,
            axial_height_m,
            ampere_turns_a: 1.0,
            current_model: CurrentModel::UniformWindingPack,
        };
        let evaluator =
            RacetrackEvaluator::new(&geometry, case.numerics.quadrature_orders[1]).unwrap();
        let true_unit_bore_bz = evaluator
            .evaluate(case.requirement.bore_probe_m)
            .unwrap()
            .field_t[2];

        candidate.unit_bore_bz_t_per_ampere_turn = true_unit_bore_bz - 1e-6;
        candidate.ampere_turns_a = 4.0e5;

        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let recomputed = recompute_one(
            "baseline",
            run.baseline_index,
            case,
            &candidate,
            &datasets,
            None,
        )
        .unwrap();
        assert!(
            (recomputed.field_absolute_error_t - 0.4).abs() < 1e-9,
            "expected a genuine 0.4 T error (1e-6 T/A-turn x 4e5 A-turn), got {}",
            recomputed.field_absolute_error_t
        );
        assert_eq!(recomputed.field_agreement_status, Status::Fail);
        assert_eq!(recomputed.agreement_status, Status::Fail);
    }

    fn refined(status: Status, min_allowed_screening_a: Option<f64>) -> SamplingRefinementResult {
        SamplingRefinementResult {
            status,
            min_allowed_screening_a,
            limiting: None,
            max_utilization: None,
            transverse_pressure_pa: None,
            stations_evaluated: 10,
            points_evaluated: 1000,
        }
    }

    #[test]
    fn sampling_refinement_verdict_skips_a_candidate_that_never_claimed_pass() {
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Fail,
            Some(1000.0),
            &refined(Status::Pass, Some(1000.0)),
            0.02,
        );
        assert_eq!(shortfall, None);
        assert_eq!(status, Status::NotEvaluated);
    }

    #[test]
    fn sampling_refinement_verdict_passes_within_the_declared_gate() {
        // 1.5% shortfall, under the 2% gate, refined plan itself PASS.
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            Some(1000.0),
            &refined(Status::Pass, Some(985.0)),
            0.02,
        );
        assert!((shortfall.unwrap() - 0.015).abs() < 1e-12);
        assert_eq!(status, Status::Pass);
    }

    #[test]
    fn sampling_refinement_verdict_fails_beyond_the_declared_gate() {
        // 5% shortfall, over the 2% gate.
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            Some(1000.0),
            &refined(Status::Pass, Some(950.0)),
            0.02,
        );
        assert!((shortfall.unwrap() - 0.05).abs() < 1e-12);
        assert_eq!(status, Status::Fail);
    }

    #[test]
    fn sampling_refinement_verdict_fails_when_the_refined_plan_loses_pass_even_with_zero_shortfall()
    {
        // Contract §9.4: "the candidate keeps its PASS status on the
        // refined plan" is a separate gate from the shortfall fraction --
        // a refined margin that is not smaller (shortfall 0) still fails
        // if the refined plan itself is not PASS (e.g. INCONCLUSIVE from
        // an unsupported point elsewhere in the finer sampling).
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            Some(1000.0),
            &refined(Status::Inconclusive, Some(1000.0)),
            0.02,
        );
        assert_eq!(shortfall, Some(0.0));
        assert_eq!(status, Status::Fail);
    }

    #[test]
    fn sampling_refinement_verdict_fails_closed_when_a_minimum_is_unavailable() {
        // Neither an all-Unsupported refined plan (no determined point, so
        // min_allowed_screening_a is None) nor a missing coarse minimum can
        // be silently treated as "shortfall within gate".
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            Some(1000.0),
            &refined(Status::Inconclusive, None),
            0.02,
        );
        assert_eq!(shortfall, None);
        assert_eq!(status, Status::Fail);

        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            None,
            &refined(Status::Pass, Some(950.0)),
            0.02,
        );
        assert_eq!(shortfall, None);
        assert_eq!(status, Status::Fail);
    }

    #[test]
    fn sampling_refinement_verdict_treats_a_larger_refined_margin_as_zero_not_negative_shortfall() {
        // A refined minimum *larger* than the coarse one (a better margin,
        // not a shortfall) must clamp to 0.0, not go negative.
        let (shortfall, status) = sampling_refinement_verdict(
            Status::Pass,
            Some(1000.0),
            &refined(Status::Pass, Some(1050.0)),
            0.02,
        );
        assert_eq!(shortfall, Some(0.0));
        assert_eq!(status, Status::Pass);
    }

    #[test]
    fn pressure_coverage_skips_a_candidate_that_never_claimed_pass() {
        let (shortfall, status) =
            pressure_coverage_verdict(Status::Fail, true, Some(1.0e7), Some(1.2e7), 0.02);
        assert_eq!(shortfall, None);
        assert_eq!(status, Status::NotEvaluated);
    }

    #[test]
    fn pressure_coverage_records_but_does_not_gate_without_a_declared_bound() {
        // A PASS candidate's shortfall is still recorded for the ledger
        // when computable; with no declared bound the resolution is not
        // load-bearing, so the status stays NOT_EVALUATED either way.
        let (shortfall, status) =
            pressure_coverage_verdict(Status::Pass, false, Some(1.0e7), Some(1.2e7), 0.02);
        assert!((shortfall.unwrap() - 0.2).abs() < 1e-12);
        assert_eq!(status, Status::NotEvaluated);
    }

    #[test]
    fn pressure_coverage_passes_within_the_declared_gate() {
        // Refined estimate 1.5% above coarse — under the 2% gate.
        let (shortfall, status) =
            pressure_coverage_verdict(Status::Pass, true, Some(1.0e7), Some(1.015e7), 0.02);
        assert!((shortfall.unwrap() - 0.015).abs() < 1e-12);
        assert_eq!(status, Status::Pass);
    }

    #[test]
    fn pressure_coverage_fails_beyond_the_declared_gate() {
        // Refined estimate 5% above coarse — the declared plan
        // under-resolved the load accumulation the bound screens.
        let (shortfall, status) =
            pressure_coverage_verdict(Status::Pass, true, Some(1.0e7), Some(1.05e7), 0.02);
        assert!((shortfall.unwrap() - 0.05).abs() < 1e-12);
        assert_eq!(status, Status::Fail);
    }

    #[test]
    fn pressure_coverage_clamps_a_lower_refined_estimate_to_zero() {
        // A refined estimate *below* the coarse one is not a coverage
        // failure — the coarse plan over-estimated, which is safe-side.
        let (shortfall, status) =
            pressure_coverage_verdict(Status::Pass, true, Some(1.0e7), Some(0.9e7), 0.02);
        assert_eq!(shortfall, Some(0.0));
        assert_eq!(status, Status::Pass);
    }

    #[test]
    fn pressure_coverage_fails_closed_when_an_estimate_is_unavailable() {
        // A declared bound whose resolution cannot be checked — a missing
        // coarse or refined estimate, or a zero coarse estimate the
        // fraction cannot normalize — must never pass silently.
        for (coarse, refined) in [
            (None, Some(1.0e7)),
            (Some(1.0e7), None),
            (Some(0.0), Some(1.0e7)),
        ] {
            let (shortfall, status) =
                pressure_coverage_verdict(Status::Pass, true, coarse, refined, 0.02);
            assert_eq!(shortfall, None);
            assert_eq!(status, Status::Fail);
        }
    }

    /// This module's fixture lifted to schema v15 with all four screens
    /// declared: v2+ needs the refinement block, v3+ the usable-volume
    /// region.
    fn v15_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(reduced_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v15".into();
        v["requirement"]["good_field_region"] = serde_json::json!({
            "half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3
        });
        v["refinement"] = serde_json::json!({
            "pancake_counts": [2],
            "turn_resolution": 5,
            "turn_bounds": {"min": 1, "max": 100},
            "brackets": [{"tapes": 2, "fail_turns": 3, "pass_turns": 60}],
            "monotonicity_check": true
        });
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
        serde_json::to_string(&v).unwrap()
    }

    /// The v15 fixture lifted to schema v16 with the measured E–J
    /// transition screen declared at the criterion bound.
    fn v16_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v15_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v16".into();
        v["transition"] = serde_json::json!({"max_e_over_ec": 1.0});
        serde_json::to_string(&v).unwrap()
    }

    /// v17: the winding-mechanics pair — the bend-strain fields on
    /// `manufacturing` and the membrane-tension bound on `mechanical`.
    fn v17_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v16_case_json()).unwrap();
        v["schema"] = "optcoil-coupled-search/v17".into();
        v["manufacturing"] = serde_json::json!({
            "min_inner_bend_radius_m": 1e-9,
            "tape_thickness_m": 1.0e-4,
            "max_bend_strain": 0.05
        });
        v["mechanical"] = serde_json::json!({
            "max_lorentz_load_n_per_m": 1.0e18,
            "max_membrane_tension_n_per_m": 1.0e18
        });
        serde_json::to_string(&v).unwrap()
    }

    /// v18: the lumped quench-transient screen — same declared
    /// parameters the search-side fixture uses, so the recomputation
    /// exercises real sharing trajectories at T0 = 30 K.
    fn v18_case_json() -> String {
        let mut v: serde_json::Value = serde_json::from_str(&v17_case_json()).unwrap();
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

    /// v19: the non-planar helix fixture — a 4-turn CCT/CORC-class
    /// layer under a declared Cartesian field map. The acceptance
    /// ledger must recompute its installed length from per-segment
    /// helix arithmetic, not the production helper.
    fn v19_case_json() -> String {
        crate::coupled_search::tests::helix_map_case_json("[4]")
    }

    fn datasets_for() -> BTreeMap<String, MaterialDataset> {
        let dataset = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        BTreeMap::from([(dataset.metadata.id.clone(), dataset)])
    }

    #[test]
    fn acceptance_recomputes_declared_screens_on_its_own_rerun() {
        let run =
            run_coupled_search_case(&v15_case_json(), &CoupledSearchOptions::default()).unwrap();
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        for c in [
            &acceptance.baseline,
            acceptance.best.as_ref().unwrap_or(&acceptance.baseline),
        ] {
            assert_eq!(c.screens_agreement_status, Status::Pass);
            let screens = c.recomputed_screens.as_ref().unwrap();
            // Every declared screen's recomputed status equals the
            // recorded one — the digests carry both.
            for e in [
                screens.thermal_margin.as_ref(),
                screens.ac_loss.as_ref(),
                screens.quench_hotspot.as_ref(),
                screens.screening_current.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(e.status, Status::Pass);
                assert_eq!(e.recomputed_status, e.recorded_status);
            }
        }
        assert!(
            acceptance
                .checks
                .iter()
                .any(|c| c.id == "adjacent_screens" && c.status == Status::Pass)
        );
    }

    #[test]
    fn acceptance_recomputes_the_transition_screen_on_its_own_rerun() {
        let run =
            run_coupled_search_case(&v16_case_json(), &CoupledSearchOptions::default()).unwrap();
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        for c in [
            &acceptance.baseline,
            acceptance.best.as_ref().unwrap_or(&acceptance.baseline),
        ] {
            let entry = c
                .recomputed_screens
                .as_ref()
                .and_then(|s| s.transition.as_ref())
                .expect("recomputed transition digest");
            assert_eq!(entry.status, Status::Pass);
            assert_eq!(entry.recomputed_status, entry.recorded_status);
        }
    }

    #[test]
    fn acceptance_detects_a_planted_transition_value_disagreement() {
        // A record that keeps the right verdict but misreports the
        // transition depth must still fail agreement.
        let mut run =
            run_coupled_search_case(&v16_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        let transition = baseline
            .screens
            .as_mut()
            .unwrap()
            .transition
            .as_mut()
            .unwrap();
        // Fabricate a fixed headline depth — a relative plant can be
        // absorbed when the recorded depth underflows to 0.0 or
        // saturates huge, so the planted value must not derive from it.
        transition.max_e_over_ec = Some(0.5);
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        let entry = acceptance
            .baseline
            .recomputed_screens
            .as_ref()
            .unwrap()
            .transition
            .as_ref()
            .unwrap();
        assert_eq!(entry.status, Status::Fail);
        assert_eq!(acceptance.baseline.screens_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
        assert_eq!(acceptance.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_recomputes_the_v17_mechanics_bounds_on_its_own_rerun() {
        // The recomputed bend strain and membrane-tension resultant are
        // recorded alongside the verdict-level agreements.
        let run =
            run_coupled_search_case(&v17_case_json(), &CoupledSearchOptions::default()).unwrap();
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        for c in [
            &acceptance.baseline,
            acceptance.best.as_ref().unwrap_or(&acceptance.baseline),
        ] {
            assert_eq!(c.manufacturing_agreement_status, Status::Pass);
            assert_eq!(c.mechanical_agreement_status, Status::Pass);
            assert!(c.recomputed_bend_strain.is_some());
            assert!(c.recomputed_membrane_tension_n_per_m.is_some());
        }
        // The recomputed strain tracks the recorded one on the baseline.
        let baseline = &run.candidates[run.baseline_index];
        let recorded = baseline.bend_strain.unwrap();
        let recomputed = acceptance.baseline.recomputed_bend_strain.unwrap();
        assert!(
            (recomputed - recorded).abs() <= 1e-12 * recorded.max(1.0),
            "recomputed bend strain {recomputed} vs recorded {recorded}"
        );
    }

    #[test]
    fn acceptance_detects_a_planted_manufacturing_verdict_disagreement() {
        // Flipping the recorded manufacturing verdict must flip the
        // agreement — the bend-strain bound is really recomputed.
        let mut run =
            run_coupled_search_case(&v17_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        baseline.manufacturing_feasible = !baseline.manufacturing_feasible;
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(
            acceptance.baseline.manufacturing_agreement_status,
            Status::Fail
        );
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_mechanical_verdict_disagreement() {
        // Same for the mechanical screen's membrane-tension bound: a
        // flipped recorded verdict cannot survive the recomputation.
        let mut run =
            run_coupled_search_case(&v17_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        baseline.mechanical_feasible = baseline.mechanical_feasible.map(|f| !f);
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(
            acceptance.baseline.mechanical_agreement_status,
            Status::Fail
        );
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_screen_disagreement() {
        let mut run =
            run_coupled_search_case(&v15_case_json(), &CoupledSearchOptions::default()).unwrap();
        // Flip the recorded screening-current verdict on the baseline —
        // the independent recomputation must not reproduce the lie.
        let baseline = &mut run.candidates[run.baseline_index];
        let screening = baseline
            .screens
            .as_mut()
            .unwrap()
            .screening_current
            .as_mut()
            .unwrap();
        screening.status = match screening.status {
            Status::Pass => Status::Fail,
            _ => Status::Pass,
        };
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        let entry = acceptance
            .baseline
            .recomputed_screens
            .as_ref()
            .unwrap()
            .screening_current
            .as_ref()
            .unwrap();
        assert_eq!(entry.status, Status::Fail);
        assert_eq!(acceptance.baseline.screens_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
        assert_eq!(acceptance.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_screen_value_disagreement() {
        // A record that keeps the right verdict but misreports the
        // magnitude — e.g. a halved loss total — must still fail: the
        // agreement check compares headline values, not just statuses.
        let mut run =
            run_coupled_search_case(&v15_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        let ac = baseline.screens.as_mut().unwrap().ac_loss.as_mut().unwrap();
        ac.total_loss_w = ac.total_loss_w.map(|w| w * 0.5);
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(
            acceptance
                .baseline
                .recomputed_screens
                .as_ref()
                .unwrap()
                .ac_loss
                .as_ref()
                .unwrap()
                .status,
            Status::Fail
        );
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_recomputes_the_quench_transient_on_its_own_rerun() {
        // The lumped driven-dump trajectory is re-integrated on this
        // module's own seeds and agrees status-for-status plus every
        // recorded headline.
        let run =
            run_coupled_search_case(&v18_case_json(), &CoupledSearchOptions::default()).unwrap();
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        for c in [
            &acceptance.baseline,
            acceptance.best.as_ref().unwrap_or(&acceptance.baseline),
        ] {
            let entry = c
                .recomputed_screens
                .as_ref()
                .and_then(|s| s.quench_transient.as_ref())
                .expect("recomputed quench-transient digest");
            assert_eq!(entry.status, Status::Pass);
            assert_eq!(entry.recomputed_status, entry.recorded_status);
        }
    }

    #[test]
    fn acceptance_detects_a_planted_transient_peak_disagreement() {
        // A record that keeps the right verdict but misreports the peak
        // temperature must still fail agreement — the trajectory is
        // really re-integrated.
        let mut run =
            run_coupled_search_case(&v18_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        let transient = baseline
            .screens
            .as_mut()
            .unwrap()
            .quench_transient
            .as_mut()
            .unwrap();
        transient.peak_temperature_k = Some(44.0);
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        let entry = acceptance
            .baseline
            .recomputed_screens
            .as_ref()
            .unwrap()
            .quench_transient
            .as_ref()
            .unwrap();
        assert_eq!(entry.status, Status::Fail);
        assert_eq!(acceptance.baseline.screens_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_detects_a_planted_transient_flag_disagreement() {
        // The coverage flags are compared too — a record that hides an
        // exhausted trajectory must fail even when the headline values
        // are left alone.
        let mut run =
            run_coupled_search_case(&v18_case_json(), &CoupledSearchOptions::default()).unwrap();
        let baseline = &mut run.candidates[run.baseline_index];
        let transient = baseline
            .screens
            .as_mut()
            .unwrap()
            .quench_transient
            .as_mut()
            .unwrap();
        transient.coverage_exhausted = !transient.coverage_exhausted;
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        let entry = acceptance
            .baseline
            .recomputed_screens
            .as_ref()
            .unwrap()
            .quench_transient
            .as_ref()
            .unwrap();
        assert_eq!(entry.status, Status::Fail);
        assert_eq!(acceptance.baseline.screens_agreement_status, Status::Fail);
    }

    #[test]
    fn acceptance_recomputes_the_helix_ledger_on_its_own_rerun() {
        // The v19 helix case: installed length must recompute from the
        // acceptance module's own per-segment helix arithmetic, and the
        // recorded cost must agree exactly.
        let run =
            run_coupled_search_case(&v19_case_json(), &CoupledSearchOptions::default()).unwrap();
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Pass);
        assert_eq!(acceptance.baseline.agreement_status, Status::Pass);
    }

    #[test]
    fn fallback_walk_promotes_the_cheapest_surviving_alternative() {
        // The walk's trigger is `sampling_refinement_status != Pass` —
        // exercised here by naming a non-PASS candidate as the optimum
        // (its refined plan is NotEvaluated, the same branch a §9.4
        // shortfall takes). With one genuine PASS alternative priced
        // above it, the walk must promote that candidate and record the
        // unresolved cheaper one.
        let run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        let blocked = run.candidates.iter().position(|c| c.status != Status::Pass);
        let survivor = run
            .candidates
            .iter()
            .position(|c| c.status == Status::Pass && c.index != run.baseline_index);
        let (Some(blocked), Some(survivor)) = (blocked, survivor) else {
            // Fixture drift — no failed candidate to name or no
            // non-baseline PASS to promote; skip rather than weaken.
            return;
        };
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            Some(blocked),
            &datasets,
        )
        .unwrap();

        let best = acceptance.best.expect("the walk promotes the survivor");
        assert_eq!(best.index, survivor);
        assert_eq!(best.label, "best");
        // The demoted first pick is recorded as the cheaper unresolved
        // candidate, relabeled and carrying its NotEvaluated verdict.
        assert_eq!(acceptance.fallback_attempts.len(), 1);
        assert_eq!(acceptance.fallback_attempts[0].index, blocked);
        assert_eq!(
            acceptance.fallback_attempts[0].sampling_refinement_status,
            Status::NotEvaluated
        );
        let check = acceptance
            .checks
            .iter()
            .find(|c| c.id == "acceptance_fallback")
            .expect("the walk reports itself");
        assert_eq!(check.status, Status::Pass, "{}", check.detail);
        assert!(check.detail.contains("fallback"), "{}", check.detail);
        // A clean promotion keeps run-level agreement PASS: the attempt's
        // known verdict does not double-count into the agreement gate.
        assert_eq!(acceptance.agreement_status, Status::Pass);
    }

    #[test]
    fn fallback_walk_reports_no_alternatives_when_only_the_baseline_remains() {
        // Same trigger, but the only other PASS candidate is the
        // baseline — not a purchasable alternative — so the walk must
        // report that nothing else exists rather than promote it.
        let run =
            run_coupled_search_case(reduced_case_json(), &CoupledSearchOptions::default()).unwrap();
        let blocked = run
            .candidates
            .iter()
            .position(|c| c.status != Status::Pass && c.index != run.baseline_index);
        let only_baseline_passes = run
            .candidates
            .iter()
            .all(|c| c.status != Status::Pass || c.index == run.baseline_index);
        let (Some(blocked), true) = (blocked, only_baseline_passes) else {
            return; // fixture drift — this branch needs exactly that shape
        };
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            Some(blocked),
            &datasets,
        )
        .unwrap();

        // Nothing promotable: `best` still names the blocked first pick,
        // and the check says why the walk stopped.
        assert_eq!(acceptance.best.as_ref().map(|b| b.index), Some(blocked));
        assert!(acceptance.fallback_attempts.is_empty());
        let check = acceptance
            .checks
            .iter()
            .find(|c| c.id == "acceptance_fallback")
            .expect("the walk reports itself");
        assert_eq!(check.status, Status::Inconclusive, "{}", check.detail);
        assert!(
            check.detail.contains("no other screening-PASS"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn acceptance_detects_a_planted_helix_cost_disagreement() {
        // A record that under-reports installed length must fail the
        // recomputation — the helix ledger is not trusted.
        let mut run =
            run_coupled_search_case(&v19_case_json(), &CoupledSearchOptions::default()).unwrap();
        run.candidates[run.baseline_index].cost.total_usd -= 1.0;
        let datasets = datasets_for();
        let acceptance = assess(
            &run.case,
            &run.candidates,
            run.baseline_index,
            run.best_index,
            &datasets,
        )
        .unwrap();
        assert_eq!(acceptance.baseline.cost_agreement_status, Status::Fail);
        assert_eq!(acceptance.baseline.agreement_status, Status::Fail);
    }
}
