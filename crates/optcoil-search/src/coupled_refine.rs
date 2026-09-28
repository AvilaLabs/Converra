//! Headless OC-008 runner: per-pancake-count bracketing bisection refining
//! OC-007's discrete-grid cost optimum (contract §2, Stage B). Under this
//! model, at fixed pancake count p, utilization rises and cost falls as the
//! turn count n falls, so the cheapest passing design at p is the smallest
//! passing n; this module finds n*(p) by integer bisection to a declared
//! resolution, for each p in a declared set, then selects the cheapest
//! across p using the same acceptance module OC-007 already uses.
//!
//! Every candidate evaluation here goes through
//! `crate::coupled_search::evaluate_one_candidate` — the same requirement ->
//! NI -> I_op -> coarse-pruning -> full-plan pipeline OC-007's own exhaustive
//! search already validates — so OC-007's own types, records and screening
//! decisions are reused unchanged, never reimplemented.

use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::time::{Instant, SystemTime, UNIX_EPOCH};

use optcoil_model::{
    Check, Status,
    coupled_search::{Bracket, CandidateDims, CoupledSearchCase, TurnBounds},
    material::MaterialDataset,
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    coupled::{MaterialRuntimes, resolve_datasets, spec_runtime},
    coupled_search::{SearchCandidateResult, evaluate_one_candidate},
    field::{RuntimeInfo, runtime_info},
    hash,
    search_acceptance::{self, CoupledSearchAcceptance},
    write_json_new,
};

/// Bumped to v2 for search schema v11: a graded case bisects each
/// (tapes, assignment) slice over the shared per-tapes bracket, and the
/// global optimum is selected across both axes — the candidate tuple is
/// now (turns, tapes, strands, assignment).
pub const COUPLED_REFINE_MODEL_ID: &str =
    "bracketing-bisection-refinement-of-coupled-cost-optimum/v2";
pub const COUPLED_REFINE_CHECKER_ID: &str =
    "coupled-refine-per-pancake-count-bisection-and-global-selection/v2";

/// Runtime override: may only lower the case's own declared
/// `execution.max_threads` (mirrors `CoupledSearchOptions`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledRefineOptions {
    pub threads: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BracketStatus {
    Valid,
    Invalid,
}

/// One bisection slice's own result: every candidate evaluated (bracket
/// ends and bisection midpoints, in evaluation order), the resulting n*
/// (contract §2 B3(3)) and the monotonicity check (B3(4)). On ungraded
/// cases a slice is just a pancake count; on graded (v11) cases the
/// slices are the (tapes, assignment) product, each bisecting turns over
/// that tapes' declared bracket under one fixed assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PancakeRefinementResult {
    pub tapes_along_width: u32,
    /// Schema v11: the per-region spec assignment this slice bisected
    /// under, in `grading.regions` order. `None` on ungraded cases.
    #[serde(default)]
    pub tape_spec_ids: Option<Vec<String>>,
    pub bracket: Bracket,
    /// INVALID iff the declared bracket's own expectation (`fail_turns`
    /// FAILs or is INCONCLUSIVE; `pass_turns` PASSes) does not hold; no
    /// bisection is attempted for this pancake count when INVALID.
    pub bracket_status: BracketStatus,
    pub evaluated: Vec<SearchCandidateResult>,
    /// n*(p): the smallest evaluated n that PASSes. `None` only when the
    /// bracket itself is INVALID (a valid bracket's own `pass_turns` end
    /// always PASSes, so a valid bracket always has a `best_turns`).
    pub best_turns: Option<u32>,
    /// Contract §2 B3(4): true iff `refinement.monotonicity_check` is false
    /// (skipped entirely) or the check ran and found no violation among
    /// this pancake count's own evaluated candidates.
    pub monotonicity_ok: bool,
    /// FAIL iff the bracket is INVALID; INCONCLUSIVE iff the bracket is
    /// valid but the monotonicity check found a violation (the bisection
    /// assumption failed); PASS iff the bracket is valid and monotone.
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalOptimum {
    pub tapes_along_width: u32,
    pub turns_along_normal: u32,
    /// The winning slice's assignment on graded cases (`None` ungraded);
    /// identical to `candidate.geometry.tape_spec_ids`, surfaced here so
    /// the record's headline carries it without an indirection.
    #[serde(default)]
    pub tape_spec_ids: Option<Vec<String>>,
    pub candidate: SearchCandidateResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoupledRefineRunRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub coupled_refine_model_id: String,
    pub coupled_refine_checker_id: String,
    pub case_sha256: String,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    pub case: CoupledSearchCase,
    pub dataset_id: String,
    pub dataset_csv_sha256: String,
    /// One entry per `refinement.pancake_counts`, in declared order.
    pub pancakes: Vec<PancakeRefinementResult>,
    /// Contract §2 B3: the cheapest n*(p) with a valid, monotone bracket;
    /// ties by fewer total turns, then by fewer pancakes.
    pub global_optimum: Option<GlobalOptimum>,
    /// Contract §2 B2/B4: the declared 120x4 baseline (OC-007's own
    /// optimum), evaluated through the identical pipeline as every other
    /// candidate here, independent of the bisection.
    pub baseline: SearchCandidateResult,
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    pub kernel_evaluations: u64,
    pub points_evaluated: u64,
    /// Contract §2 B4: `global_optimum`'s margin to `case.limits.utilization_limit`
    /// (`utilization_limit - max_utilization`); by construction this is
    /// expected to sit near zero (the design rides the declared limit, not
    /// a demonstrated engineering margin).
    pub optimum_utilization_margin: Option<f64>,
    /// FAIL if no pancake count has a valid, monotone bracket. Otherwise
    /// PASS, unless the acceptance module's refined-plan re-evaluation of
    /// the baseline (contract §2 B4, exactly OC-007's own rule) or the
    /// global optimum loses PASS or exceeds the declared sampling shortfall
    /// gate, in which case this is INCONCLUSIVE.
    pub search_status: Status,
    /// Contract §2 B4: exactly OC-007's own acceptance module
    /// (`search_acceptance.rs`), reused unchanged, on the global optimum and
    /// the declared 120x4 baseline.
    pub acceptance: CoupledSearchAcceptance,
    pub checks: Vec<Check>,
    pub limitations: Vec<String>,
}

impl CoupledRefineRunRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_oc008(options: &CoupledRefineOptions) -> Result<CoupledRefineRunRecord, RunError> {
    run_coupled_refine_case(optcoil_model::coupled_search::OC008_JSON, options)
}

pub fn run_coupled_refine_case(
    case_json: &str,
    options: &CoupledRefineOptions,
) -> Result<CoupledRefineRunRecord, RunError> {
    run_coupled_refine_case_with_dataset(case_json, options, None)
}

/// Dataset-supplying form of [`run_coupled_refine_case`], same contract
/// as `run_coupled_search_case_with_dataset`: the supplied dataset must
/// match the case's declared `dataset_id`/`csv_sha256`.
pub fn run_coupled_refine_case_with_dataset(
    case_json: &str,
    options: &CoupledRefineOptions,
    dataset: Option<MaterialDataset>,
) -> Result<CoupledRefineRunRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;

    let search = CoupledSearchCase::from_json(case_json)?;
    let case_sha256 = hash(case_json.as_bytes());
    let refinement = search.refinement.clone().ok_or_else(|| {
        RunError::Invalid(
            "coupled-refine requires a schema v2 case declaring a refinement block".into(),
        )
    })?;
    // Schema v21: the declared turn brackets assume turn-monotonicity at
    // a fixed geometry — a case that enumerates racetrack dims has no
    // single geometry for a bracket to mean. Reject, never bisect under
    // an unstated geometry.
    if search.choices.bend_radius_m.is_some() || search.choices.straight_half_length_m.is_some() {
        return Err(RunError::Invalid(
            "coupled-refine requires fixed racetrack geometry — cases declaring \
             choices.bend_radius_m or choices.straight_half_length_m axes are \
             unsupported (a declared bracket assumes turn-monotonicity at one \
             geometry)"
                .into(),
        ));
    }
    // Schema v11: a graded case declares one material binding per tape
    // spec plus the base `material`; every binding resolves its declared
    // dataset — supplied datasets matching a declared id win, the rest
    // fall to the embedded registry, and a stray supply is rejected
    // outright (same rule as the search runner).
    let supplied: Vec<MaterialDataset> = dataset.into_iter().collect();
    let bindings = search.material_bindings();
    for d in &supplied {
        if !bindings.iter().any(|(_, m)| m.dataset_id == d.metadata.id) {
            return Err(RunError::Invalid(format!(
                "supplied dataset '{}' is not declared by any material binding",
                d.metadata.id
            )));
        }
    }
    let datasets = resolve_datasets(bindings.iter().map(|&(_, m)| m), &supplied)?;
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

    // Contract §2 B3: parallel execution across bisection slices, never
    // more than `execution_threads` candidate evaluations at once, each
    // candidate evaluation itself single-threaded. Each thread bisects one
    // slice at a time to completion (each bisection is inherently
    // sequential -- a midpoint depends on the previous one), so this
    // bounds the number of concurrent single-threaded evaluations exactly
    // as declared. Schema v11: the slices are the (pancake_counts x
    // assignments) product — each assignment bisects turns over its
    // tapes' declared bracket under that fixed spec assignment; `[[]]` on
    // ungraded cases keeps the per-pancake-count behavior identical.
    let assignments = search.assignments();
    let mut work: Vec<(u32, &[String])> =
        Vec::with_capacity(refinement.pancake_counts.len() * assignments.len());
    for &tapes in &refinement.pancake_counts {
        for assignment in &assignments {
            work.push((tapes, assignment));
        }
    }
    let n_p = work.len();
    let counter = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, Result<PancakeRefinementResult, RunError>)>> =
        Mutex::new(Vec::with_capacity(n_p));

    // Same shape as the candidate-screening pool in coupled_search:
    // wasm32 has no OS threads — the shared work loop runs inline.
    let refine_work = || loop {
        let idx = counter.fetch_add(1, Ordering::Relaxed);
        if idx >= n_p {
            break;
        }
        let (tapes, assignment) = work[idx];
        let bracket = *refinement
            .brackets
            .iter()
            .find(|b| b.tapes == tapes)
            .expect("CoupledSearchCase::validate enforces a bijection between pancake_counts and brackets");
        let outcome = bisect_one_pancake(
            &search,
            tapes,
            assignment,
            &bracket,
            refinement.turn_resolution,
            &refinement.turn_bounds,
            refinement.monotonicity_check,
            &runtimes,
        );
        results
            .lock()
            .expect("refine results mutex poisoned")
            .push((idx, outcome));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|scope| {
        for _ in 0..execution_threads.min(n_p as u32).max(1) {
            // &refine_work: one Fn closure shared across the workers.
            #[allow(clippy::needless_borrows_for_generic_args)]
            scope.spawn(&refine_work);
        }
    });
    #[cfg(target_arch = "wasm32")]
    refine_work();

    let mut ordered = results.into_inner().expect("refine results mutex poisoned");
    ordered.sort_by_key(|(idx, _)| *idx);
    let mut pancakes = Vec::with_capacity(n_p);
    for (_, outcome) in ordered {
        pancakes.push(outcome?);
    }

    // Contract §2 B2/B4: the declared 120x4 baseline (OC-007's own
    // optimum), evaluated through the identical pipeline, independent of
    // the bisection above.
    let baseline = evaluate_one_candidate(
        &search,
        0,
        search.baseline.turns_along_normal,
        search.baseline.tapes_along_width,
        search.baseline_strands(),
        search.baseline.tape_spec_ids.as_deref().unwrap_or(&[]),
        CandidateDims::default(),
        &runtimes,
        None,
    )?;

    // Contract §2 B3: global optimum = the cheapest n*(p) with a valid,
    // monotone bracket; ties by fewer total turns, then (for full
    // determinism, not itself declared) by fewer pancakes.
    let global_optimum = pancakes
        .iter()
        .filter(|p| p.status == Status::Pass)
        .filter_map(|p| {
            let turns = p.best_turns?;
            let candidate = p
                .evaluated
                .iter()
                .find(|c| c.geometry.turns_along_normal == turns && c.status == Status::Pass)?;
            Some((p.tapes_along_width, turns, candidate.clone()))
        })
        .min_by(|a, b| {
            a.2.cost
                .total_usd
                .total_cmp(&b.2.cost.total_usd)
                .then(a.2.geometry.total_turns.cmp(&b.2.geometry.total_turns))
                .then(a.0.cmp(&b.0))
                .then(a.2.geometry.tape_spec_ids.cmp(&b.2.geometry.tape_spec_ids))
        })
        .map(
            |(tapes_along_width, turns_along_normal, candidate)| GlobalOptimum {
                tapes_along_width,
                turns_along_normal,
                tape_spec_ids: candidate.geometry.tape_spec_ids.clone(),
                candidate,
            },
        );

    let (savings_usd, savings_percent) = match &global_optimum {
        Some(best) => {
            let saving = baseline.cost.total_usd - best.candidate.cost.total_usd;
            (Some(saving), Some(saving / baseline.cost.total_usd * 100.0))
        }
        None => (None, None),
    };
    let optimum_utilization_margin = global_optimum.as_ref().and_then(|best| {
        best.candidate
            .screening
            .as_ref()
            .and_then(|s| s.max_utilization)
            .map(|u| search.limits.utilization_limit - u)
    });

    let kernel_evaluations: u64 = pancakes
        .iter()
        .flat_map(|p| &p.evaluated)
        .map(|c| c.coarse_kernel_evaluations + c.full_kernel_evaluations)
        .sum::<u64>()
        + baseline.coarse_kernel_evaluations
        + baseline.full_kernel_evaluations;
    let points_evaluated: u64 = pancakes
        .iter()
        .flat_map(|p| &p.evaluated)
        .map(|c| c.coarse_points_evaluated + c.full_points_evaluated)
        .sum::<u64>()
        + baseline.coarse_points_evaluated
        + baseline.full_points_evaluated;

    // Contract §2 B4: exactly OC-007's own acceptance module, reused
    // unchanged, on the global optimum and the declared 120x4 baseline. It
    // takes a slice indexed by baseline_index/best_index, so a short slice
    // (baseline, plus the optimum when one exists) stands in for OC-007's
    // own candidate list.
    let mut acceptance_candidates = vec![baseline.clone()];
    let best_index = global_optimum.as_ref().map(|best| {
        acceptance_candidates.push(best.candidate.clone());
        1
    });
    let acceptance = search_acceptance::assess(
        &search,
        &acceptance_candidates,
        0,
        best_index,
        &runtimes.datasets,
    )?;

    // Contract §2 B4: FAIL if no pancake count has a valid, monotone
    // bracket; otherwise PASS unless the acceptance module's refined-plan
    // re-evaluation of the baseline or the optimum loses PASS or exceeds
    // the declared shortfall gate, in which case INCONCLUSIVE -- mirrors
    // `coupled_search::run_coupled_search_case`'s own rule exactly.
    let search_status = if global_optimum.is_none() {
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

    let pass_p = pancakes.iter().filter(|p| p.status == Status::Pass).count();
    let invalid_p = pancakes
        .iter()
        .filter(|p| p.bracket_status == BracketStatus::Invalid)
        .count();
    let inconclusive_p = pancakes
        .iter()
        .filter(|p| p.status == Status::Inconclusive)
        .count();
    let checks = vec![
        Check {
            id: "bracketing_completion".into(),
            status: Status::Pass,
            detail: format!(
                "{} of {} pancake counts bisected ({pass_p} PASS, {inconclusive_p} INCONCLUSIVE (monotonicity violation), {invalid_p} INVALID bracket)",
                pancakes.len(),
                n_p,
            ),
        },
        Check {
            id: "baseline_screening".into(),
            status: baseline.status,
            detail: format!(
                "Baseline {}x{} candidate status {:?}",
                search.baseline.turns_along_normal,
                search.baseline.tapes_along_width,
                baseline.status
            ),
        },
    ];

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
        include_str!("coupled_refine.rs"),
        include_str!("search_acceptance.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?);
    let input_sha256 = hash(&serde_json::to_vec(&(
        &case_sha256,
        &implementation_sha256,
        options,
        COUPLED_REFINE_MODEL_ID,
        COUPLED_REFINE_CHECKER_ID,
        env!("CARGO_PKG_VERSION"),
    ))?);

    Ok(CoupledRefineRunRecord {
        // v2: `pancakes` entries carry `tape_spec_ids` and slices are the
        // (tapes, assignment) product on graded cases. v3: the embedded
        // `SearchCandidateResult`s carry `transverse_pressure_pa` and
        // `transverse_pressure_location` (search schema v12's screen).
        schema: "optcoil-coupled-refine-run/v3".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        coupled_refine_model_id: COUPLED_REFINE_MODEL_ID.into(),
        coupled_refine_checker_id: COUPLED_REFINE_CHECKER_ID.into(),
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
        pancakes,
        global_optimum,
        baseline,
        savings_usd,
        savings_percent,
        kernel_evaluations,
        points_evaluated,
        optimum_utilization_margin,
        search_status,
        acceptance,
        checks,
        limitations: vec![
            "Modeled, synthetic prices, screening model: cost is an invented placeholder model, not supplier data or a production cost estimate.".into(),
            "The design sits at the declared limits by construction: utilization_limit (0.8) and interpolation_overprediction_budget (0.10) are the only margins the global optimum retains; this is not a demonstrated engineering margin.".into(),
            "No independent reference exists per candidate; only the frozen reference geometry has one. Every candidate's numerical_status is INCONCLUSIVE by construction; refinement_status is the separate PASS/FAIL check this record relies on instead.".into(),
            "A bracket declared INVALID for a pancake count means that count's own starting assumption did not hold; no bisection was attempted for it, and it is excluded from the global optimum -- never silently patched or re-declared after the fact.".into(),
            "On graded (v11) cases each declared assignment bisects independently over its tapes' shared bracket: an assignment that cannot meet the bracket's own expectation (fail_turns not passing, pass_turns passing) is INVALID for that slice only, and the global optimum is selected across all valid (tapes, assignment) slices.".into(),
            "width_transfer basis is none (no Jc nonuniformity across the tape width or batch variation modeled); one measured specimen; conductor-filled pack with no inter-pancake spacer gaps.".into(),
            "Mechanical, thermal, quench and manufacturing acceptance remain NOT_EVALUATED.".into(),
        ],
    })
}

/// Contract §2 B3: per-pancake-count bracketing bisection. `(1)` evaluate
/// both bracket ends through the OC-007 candidate pipeline; `(2)` the
/// bracket is valid only if `fail_turns` FAILs (or is INCONCLUSIVE) and
/// `pass_turns` PASSes -- otherwise INVALID, nothing further evaluated;
/// `(3)` integer-midpoint bisect until `pass - fail <= turn_resolution`;
/// n*(p) is the smallest evaluated n that PASSes; `(4)` a monotonicity
/// violation across every evaluated n for this p (utilization nonincreasing
/// in n) makes the result INCONCLUSIVE.
#[allow(clippy::too_many_arguments)]
fn bisect_one_pancake(
    search: &CoupledSearchCase,
    tapes: u32,
    tape_spec_ids: &[String],
    bracket: &Bracket,
    turn_resolution: u32,
    turn_bounds: &TurnBounds,
    monotonicity_check: bool,
    runtimes: &MaterialRuntimes,
) -> Result<PancakeRefinementResult, RunError> {
    let _ = turn_bounds; // already enforced by CoupledSearchCase::validate on the declared bracket
    let mut evaluated: Vec<SearchCandidateResult> = Vec::new();
    let mut next_index = 0_usize;
    let evaluate_n = |turns: u32,
                      evaluated: &mut Vec<SearchCandidateResult>,
                      next_index: &mut usize|
     -> Result<SearchCandidateResult, RunError> {
        // Schema v4: refinement bisects turns at fixed (tapes, strands);
        // the strand count is held at the case's declared baseline value.
        // Schema v11: and at a fixed per-region spec assignment — each
        // graded slice bisects under one assignment only.
        let result = evaluate_one_candidate(
            search,
            *next_index,
            turns,
            tapes,
            search.baseline_strands(),
            tape_spec_ids,
            CandidateDims::default(),
            runtimes,
            None,
        )?;
        *next_index += 1;
        evaluated.push(result.clone());
        Ok(result)
    };
    let slice_spec_ids = search.grading.is_some().then(|| tape_spec_ids.to_vec());

    let fail_end = evaluate_n(bracket.fail_turns, &mut evaluated, &mut next_index)?;
    let pass_end = evaluate_n(bracket.pass_turns, &mut evaluated, &mut next_index)?;
    let bracket_valid = fail_end.status != Status::Pass && pass_end.status == Status::Pass;

    if !bracket_valid {
        return Ok(PancakeRefinementResult {
            tapes_along_width: tapes,
            tape_spec_ids: slice_spec_ids,
            bracket: *bracket,
            bracket_status: BracketStatus::Invalid,
            evaluated,
            best_turns: None,
            monotonicity_ok: true, // vacuous: the check never ran
            status: pancake_status(BracketStatus::Invalid, true, None),
        });
    }

    let mut error: Option<RunError> = None;
    integer_bisect(
        bracket.fail_turns,
        bracket.pass_turns,
        turn_resolution,
        |mid| match evaluate_n(mid, &mut evaluated, &mut next_index) {
            Ok(result) => result.status == Status::Pass,
            Err(e) => {
                error.get_or_insert(e);
                false
            }
        },
    );
    if let Some(e) = error {
        return Err(e);
    }

    // Contract §2 B3(3): "n*(p) = the smallest evaluated n that PASSes",
    // computed directly from the evaluated list rather than trusted from
    // the bisection loop's own internal bookkeeping.
    let best_turns = evaluated
        .iter()
        .filter(|c| c.status == Status::Pass)
        .map(|c| c.geometry.turns_along_normal)
        .min();

    let monotonicity_ok_p = if !monotonicity_check {
        true
    } else {
        let mut points: Vec<(u32, f64)> = evaluated
            .iter()
            .filter_map(|c| {
                c.screening
                    .as_ref()
                    .and_then(|s| s.max_utilization)
                    .map(|u| (c.geometry.turns_along_normal, u))
            })
            .collect();
        points.sort_by_key(|&(n, _)| n);
        utilization_is_monotone_nonincreasing(&points)
    };

    let status = pancake_status(BracketStatus::Valid, monotonicity_ok_p, best_turns);

    Ok(PancakeRefinementResult {
        tapes_along_width: tapes,
        tape_spec_ids: slice_spec_ids,
        bracket: *bracket,
        bracket_status: BracketStatus::Valid,
        evaluated,
        best_turns,
        monotonicity_ok: monotonicity_ok_p,
        status,
    })
}

/// Pure integer-bisection core (contract §2 B3(3)), independent of any real
/// screening evaluation: given a bracket already known to satisfy `fail`
/// FAILs / `pass` PASSes, and a black-box `probe` reporting whether turn
/// count n PASSes, bisect the interval to the declared resolution. Every
/// midpoint probed is reported to `probe` in order; `probe`'s own side
/// effects (real evaluation, or -- in the unit test below -- a synthetic
/// monotone function) are the caller's responsibility, keeping this
/// function itself directly testable against a known step function.
fn integer_bisect(
    fail_turns: u32,
    pass_turns: u32,
    turn_resolution: u32,
    mut probe: impl FnMut(u32) -> bool,
) {
    let mut fail_n = fail_turns;
    let mut pass_n = pass_turns;
    while pass_n - fail_n > turn_resolution {
        let mid = fail_n + (pass_n - fail_n) / 2;
        if mid == fail_n || mid == pass_n {
            break; // defensive: integer bisection cannot make further progress
        }
        if probe(mid) {
            pass_n = mid;
        } else {
            fail_n = mid;
        }
    }
}

/// Contract §2 B3(4): utilization must be nonincreasing in n across every
/// evaluated candidate for a pancake count. `points` need not be sorted by
/// the caller in general, but this function assumes ascending n (the real
/// caller sorts first) and is a pure function purely so it can be unit
/// tested directly against a planted non-monotone sequence, independent of
/// any real physics. A small absolute tolerance (1e-9) absorbs
/// floating-point roundoff, not a physically meaningful reversal.
fn utilization_is_monotone_nonincreasing(points: &[(u32, f64)]) -> bool {
    points.windows(2).all(|w| w[1].1 <= w[0].1 + 1e-9)
}

/// Contract §2 B3(2)/(4): the pancake-count-level verdict, pulled out as a
/// pure function so the exact contract wording -- "FAIL iff INVALID, ...
/// INCONCLUSIVE iff ... the monotonicity check found a violation" -- can be
/// unit tested directly against planted inputs, independent of any real
/// bisection or physics. `best_turns` being `None` while the bracket is
/// `Valid` is unreachable in practice (a valid bracket's own `pass_turns`
/// end already PASSed), but is still handled fail-closed rather than
/// assumed.
fn pancake_status(
    bracket_status: BracketStatus,
    monotonicity_ok: bool,
    best_turns: Option<u32>,
) -> Status {
    if bracket_status == BracketStatus::Invalid {
        Status::Fail
    } else if !monotonicity_ok {
        Status::Inconclusive
    } else if best_turns.is_some() {
        Status::Pass
    } else {
        Status::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Pure-function tests (contract §2 B5): independent of real physics.
    // -----------------------------------------------------------------

    #[test]
    fn bisection_reaches_the_known_n_star_within_the_resolution_on_a_synthetic_step_function() {
        // A synthetic "utilization" step function: PASS (true) iff n >= 137,
        // FAIL otherwise -- a made-up threshold with no physical meaning,
        // chosen only to exercise the pure bisection arithmetic.
        const THRESHOLD: u32 = 137;
        let mut probed: Vec<u32> = Vec::new();
        let mut last_pass = 400_u32; // the declared pass_turns end, always a PASS
        integer_bisect(40, 400, 2, |mid| {
            probed.push(mid);
            let passes = mid >= THRESHOLD;
            if passes {
                last_pass = last_pass.min(mid);
            }
            passes
        });
        assert!(
            !probed.is_empty(),
            "bisection must actually probe midpoints"
        );
        assert!(
            last_pass >= THRESHOLD && last_pass - THRESHOLD < 2,
            "expected n* within the declared resolution of the true threshold {THRESHOLD}, got {last_pass}"
        );
    }

    #[test]
    fn bisection_never_probes_outside_the_declared_bracket() {
        let mut probed: Vec<u32> = Vec::new();
        integer_bisect(40, 120, 2, |mid| {
            probed.push(mid);
            mid >= 90
        });
        assert!(probed.iter().all(|&n| (40..=120).contains(&n)));
    }

    #[test]
    fn a_planted_non_monotone_utilization_sequence_is_detected() {
        // Genuinely monotone (nonincreasing) in n: no violation.
        assert!(utilization_is_monotone_nonincreasing(&[
            (40, 0.95),
            (80, 0.60),
            (120, 0.40)
        ]));
        // Planted violation: utilization rises from n=80 to n=120, which
        // contradicts the model's own monotonicity assumption (contract B1)
        // and must be caught, not silently accepted.
        assert!(!utilization_is_monotone_nonincreasing(&[
            (40, 0.95),
            (80, 0.60),
            (120, 0.70)
        ]));
        // Equal utilization (a plateau) is not itself a violation.
        assert!(utilization_is_monotone_nonincreasing(&[
            (40, 0.60),
            (80, 0.60),
            (120, 0.60)
        ]));
        // A single point is vacuously monotone.
        assert!(utilization_is_monotone_nonincreasing(&[(80, 0.60)]));
        assert!(utilization_is_monotone_nonincreasing(&[]));
    }

    #[test]
    fn a_planted_non_monotone_sequence_yields_inconclusive_pancake_status() {
        // Contract §2 B3(4): "a violation is recorded and makes that p's
        // result INCONCLUSIVE (the bisection assumption failed)" -- checked
        // directly at the level of `pancake_status`'s own verdict, planting
        // the same non-monotone sequence the pure detector test above
        // already flags as a violation.
        let violation =
            !utilization_is_monotone_nonincreasing(&[(40, 0.95), (80, 0.60), (120, 0.70)]);
        assert!(violation);
        assert_eq!(
            pancake_status(BracketStatus::Valid, !violation, Some(80)),
            Status::Inconclusive
        );
        // A monotone sequence with the same bracket/best_turns shape PASSes
        // instead, isolating monotonicity as the deciding factor.
        let no_violation =
            utilization_is_monotone_nonincreasing(&[(40, 0.95), (80, 0.60), (120, 0.40)]);
        assert_eq!(
            pancake_status(BracketStatus::Valid, no_violation, Some(80)),
            Status::Pass
        );
        // An INVALID bracket is FAIL regardless of monotonicity or best_turns.
        assert_eq!(
            pancake_status(BracketStatus::Invalid, true, Some(80)),
            Status::Fail
        );
    }

    // -----------------------------------------------------------------
    // Real-physics-backed tests, on a tiny reduced case (never the frozen
    // OC-008 case): schema v1 fixture reused directly by `bisect_one_pancake`
    // (which takes `bracket`/`turn_resolution`/etc. as separate parameters,
    // independent of `search.refinement`), and a schema v2 fixture for the
    // top-level `run_coupled_refine_case` entry point.
    // -----------------------------------------------------------------

    /// Same shape as `coupled_search.rs`'s own `reduced_case_json` fixture
    /// (deliberately duplicated here for this module's own independence):
    /// b_target_t 0.05 T, turns 3 (enormous NI, a genuine FAIL far over the
    /// utilization limit) or 60 (small NI, a genuine PASS), tapes 2.
    fn v1_reduced_case_json() -> &'static str {
        r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "coupled-refine-test-case",
  "provenance": "coupled_refine.rs unit test fixture; not a frozen benchmark.",
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

    /// The same reduced geometry as `v1_reduced_case_json`, extended to
    /// schema v2 with a `refinement` block over pancake counts {2, 3}, each
    /// bracketed by the already-known FAIL-at-3/PASS-at-60 turns from the v1
    /// fixture above (more tapes at the same turns only ever raises total
    /// turns and lowers I_op further, so the same bracket ends hold for
    /// tapes 3 too).
    fn v2_reduced_case_json() -> &'static str {
        r#"{
  "schema": "optcoil-coupled-search/v2",
  "id": "coupled-refine-v2-test-case",
  "provenance": "coupled_refine.rs unit test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [3, 60], "tapes_along_width": [2, 3]},
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
  "refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 5,
    "turn_bounds": {"min": 1, "max": 100},
    "brackets": [
      {"tapes": 2, "fail_turns": 3, "pass_turns": 60},
      {"tapes": 3, "fail_turns": 3, "pass_turns": 60}
    ],
    "monotonicity_check": true
  },
  "execution": {"max_threads": 3}
}
"#
    }

    fn load_v1_fixture_and_runtimes() -> (CoupledSearchCase, MaterialRuntimes) {
        let search = CoupledSearchCase::from_json(v1_reduced_case_json()).unwrap();
        let datasets = resolve_datasets(std::iter::once(&search.material), &[]).unwrap();
        let runtimes = MaterialRuntimes {
            base: spec_runtime(
                &search.material,
                &datasets[&search.material.dataset_id],
                search.operating.temperature_k,
                search.limits.self_field_correction.as_deref() == Some("critical_state_strip"),
            )
            .unwrap(),
            specs: std::collections::BTreeMap::new(),
            datasets,
        };
        (search, runtimes)
    }

    #[test]
    fn an_invalid_bracket_is_reported_invalid_and_evaluates_nothing_further() {
        let (search, runtimes) = load_v1_fixture_and_runtimes();
        // turns=60 already PASSes (the fixture's own baseline), so a bracket
        // that declares it as the *fail* end violates the bracket's own
        // expectation.
        let bracket = Bracket {
            tapes: 2,
            fail_turns: 60,
            pass_turns: 90,
        };
        let result = bisect_one_pancake(
            &search,
            2,
            &[],
            &bracket,
            5,
            &TurnBounds { min: 1, max: 100 },
            true,
            &runtimes,
        )
        .unwrap();
        assert_eq!(result.bracket_status, BracketStatus::Invalid);
        assert_eq!(result.status, Status::Fail);
        assert_eq!(result.best_turns, None);
        assert_eq!(
            result.evaluated.len(),
            2,
            "only the two bracket ends may be evaluated once the bracket is invalid"
        );
    }

    #[test]
    fn a_valid_bracket_bisects_to_a_passing_n_star_within_the_resolution() {
        let (search, runtimes) = load_v1_fixture_and_runtimes();
        let bracket = Bracket {
            tapes: 2,
            fail_turns: 3,
            pass_turns: 60,
        };
        let result = bisect_one_pancake(
            &search,
            2,
            &[],
            &bracket,
            5,
            &TurnBounds { min: 1, max: 100 },
            true,
            &runtimes,
        )
        .unwrap();
        assert_eq!(result.bracket_status, BracketStatus::Valid);
        assert_eq!(result.status, Status::Pass);
        let best = result.best_turns.expect("a valid bracket always finds n*");
        assert!(
            result
                .evaluated
                .iter()
                .any(|c| c.geometry.turns_along_normal == best && c.status == Status::Pass)
        );
        // n* is the smallest evaluated n that PASSes -- no smaller evaluated
        // n may also PASS.
        assert!(
            result
                .evaluated
                .iter()
                .all(|c| c.geometry.turns_along_normal >= best || c.status != Status::Pass)
        );
    }

    #[test]
    fn single_thread_and_multi_thread_results_are_bit_identical() {
        let single = run_coupled_refine_case(
            v2_reduced_case_json(),
            &CoupledRefineOptions { threads: Some(1) },
        )
        .unwrap();
        let multi = run_coupled_refine_case(
            v2_reduced_case_json(),
            &CoupledRefineOptions { threads: Some(3) },
        )
        .unwrap();
        assert_eq!(single.pancakes.len(), multi.pancakes.len());
        for (a, b) in single.pancakes.iter().zip(&multi.pancakes) {
            assert_eq!(a.tapes_along_width, b.tapes_along_width);
            assert_eq!(a.bracket_status, b.bracket_status);
            assert_eq!(a.status, b.status);
            assert_eq!(a.best_turns, b.best_turns);
            assert_eq!(a.evaluated.len(), b.evaluated.len());
            for (ca, cb) in a.evaluated.iter().zip(&b.evaluated) {
                assert_eq!(
                    ca.geometry.turns_along_normal,
                    cb.geometry.turns_along_normal
                );
                assert_eq!(ca.status, cb.status);
                assert_eq!(ca.cost.total_usd.to_bits(), cb.cost.total_usd.to_bits());
                assert_eq!(ca.ampere_turns_a.to_bits(), cb.ampere_turns_a.to_bits());
            }
        }
        assert_eq!(
            single
                .global_optimum
                .as_ref()
                .map(|g| (g.tapes_along_width, g.turns_along_normal)),
            multi
                .global_optimum
                .as_ref()
                .map(|g| (g.tapes_along_width, g.turns_along_normal))
        );
        assert_eq!(single.savings_usd, multi.savings_usd);
        assert_eq!(
            single.baseline.cost.total_usd,
            multi.baseline.cost.total_usd
        );
    }

    #[test]
    fn threads_option_may_only_lower_the_case_declared_maximum() {
        assert!(
            run_coupled_refine_case(
                v2_reduced_case_json(),
                &CoupledRefineOptions { threads: Some(99) }
            )
            .is_err()
        );
        assert!(
            run_coupled_refine_case(
                v2_reduced_case_json(),
                &CoupledRefineOptions { threads: Some(0) }
            )
            .is_err()
        );
        assert!(
            run_coupled_refine_case(
                v2_reduced_case_json(),
                &CoupledRefineOptions { threads: Some(2) }
            )
            .is_ok()
        );
    }

    #[test]
    fn a_v1_case_requesting_coupled_refine_is_rejected_rather_than_silently_skipping_refinement() {
        assert!(
            run_coupled_refine_case(v1_reduced_case_json(), &CoupledRefineOptions::default())
                .is_err()
        );
    }

    #[test]
    fn v1_search_cases_still_parse_and_oc007s_own_pipeline_is_unaffected_by_this_module() {
        // Contract §2 B5: schema v1 (OC-007) must be unaffected by adding
        // the v2 refinement block and this module's own bracketing runner.
        // coupled_search.rs's own test suite already exercises this fixture
        // shape directly; asserted again here for this module's own
        // traceability.
        let record = crate::coupled_search::run_coupled_search_case(
            v1_reduced_case_json(),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        assert_eq!(
            record.case.schema,
            optcoil_model::coupled_search::COUPLED_SEARCH_CASE_SCHEMA_V1
        );
        assert!(record.case.refinement.is_none());
        assert_eq!(record.candidates.len(), 2);
    }

    /// A v11 graded refinement fixture on the reduced skeleton: two
    /// half-width grading regions, the named `cheap` spec bound to a
    /// caller-supplied dataset id (so a `scaled_ic` synthetic weak tape
    /// can be supplied at run time). Calibrated to the same physics point
    /// as `coupled_search.rs`'s weak-spec capability test — 1.5 T bore
    /// target, 0.5 mm radial pitch — where a 0.725x-Ic spec fails on the
    /// high-field inner half but passes on the outer half, so the declared
    /// [200, 300] bracket is valid exactly for the assignments that keep
    /// base tape inside.
    fn v11_graded_case_json(spec_dataset_id: &str, spec_sha256: &str) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v11",
  "id": "coupled-refine-v11-graded-test-case",
  "provenance": "coupled_refine.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 1.5, "tolerance_fraction": 1e-6, "good_field_region": {{"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3}}}},
  "fixed_geometry": {{
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0005,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  }},
  "choices": {{"turns_along_normal": [300], "tapes_along_width": [2]}},
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
        "dataset_id": "{spec_dataset_id}",
        "csv_sha256": "{spec_sha256}",
        "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
        "angle_mapping": "period_180_field_reversal",
        "mirror_policy": "minimum_of_mirror_pair",
        "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
        "field_magnitude_policy": "total_magnitude_with_transverse_angle",
        "low_field_policy": "monotone_field_lower_bound",
        "low_field_clamp_t": 1.001,
        "monotonicity_tolerance": 0.001
      }},
      "price_usd_per_m": 10.0
    }}
  }},
  "grading": {{
    "regions": [
      {{"turn_range": [0.0, 0.5], "tape_spec_choices": ["base", "cheap"]}},
      {{"turn_range": [0.5, 1.0], "tape_spec_choices": ["base", "cheap"]}}
    ]
  }},
  "sampling": {{
    "stations": [{{"id": "s0", "kind": "straight", "x_m": 0.0}}],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "fraction", "value": 0.125}},
      {{"kind": "fraction", "value": 0.5}},
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
    "turns_along_normal": 300,
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
  "refinement": {{
    "pancake_counts": [2],
    "turn_resolution": 5,
    "turn_bounds": {{"min": 1, "max": 400}},
    "brackets": [
      {{"tapes": 2, "fail_turns": 200, "pass_turns": 300}}
    ],
    "monotonicity_check": true
  }},
  "execution": {{"max_threads": 3}}
}}
"#
        )
    }

    /// Graded refinement end to end: the (tapes x assignment) product is
    /// bisected slice by slice, each slice records its own assignment, a
    /// bracket that cannot hold under the weak spec on the inner half is
    /// INVALID for exactly those slices, and the global optimum selects
    /// the feasible graded mix — verified through independent acceptance.
    #[test]
    fn graded_refinement_bisects_per_assignment_and_selects_across_slices() {
        let base_ds = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let weak = base_ds.scaled_ic(0.725).unwrap();
        let json = v11_graded_case_json(&weak.metadata.id, &weak.metadata.csv_sha256);
        let record = run_coupled_refine_case_with_dataset(
            &json,
            &CoupledRefineOptions::default(),
            Some(weak),
        )
        .unwrap();

        // 1 pancake count x 4 assignments = 4 slices, in declaration order.
        assert_eq!(record.pancakes.len(), 4);
        let expected: [[&str; 2]; 4] = [
            ["base", "base"],
            ["base", "cheap"],
            ["cheap", "base"],
            ["cheap", "cheap"],
        ];
        for (slice, ids) in record.pancakes.iter().zip(expected) {
            assert_eq!(
                slice.tape_spec_ids.as_deref(),
                Some(ids.map(str::to_string).as_slice())
            );
        }

        // At 300 turns the weak spec breaches the 0.8 limit on the inner
        // (high-field) half, so the pass end of the declared bracket fails
        // under exactly the two assignments that put it there.
        assert_eq!(record.pancakes[0].bracket_status, BracketStatus::Valid);
        assert_eq!(record.pancakes[1].bracket_status, BracketStatus::Valid);
        assert_eq!(record.pancakes[2].bracket_status, BracketStatus::Invalid);
        assert_eq!(record.pancakes[3].bracket_status, BracketStatus::Invalid);

        // Every evaluated candidate carries its slice's assignment, and a
        // valid slice's n* is its smallest evaluated passing turn count.
        for slice in &record.pancakes {
            for candidate in &slice.evaluated {
                assert_eq!(candidate.geometry.tape_spec_ids, slice.tape_spec_ids);
            }
        }

        // The graded mix wins the global selection: outer-half cheap tape
        // is feasible and strictly cheaper than all-base.
        let best = record.global_optimum.as_ref().unwrap();
        assert_eq!(
            best.tape_spec_ids.as_deref(),
            Some(["base", "cheap"].map(str::to_string).as_slice())
        );
        assert!(best.candidate.cost.total_usd < record.baseline.cost.total_usd);
        assert_eq!(best.candidate.geometry.tape_spec_ids, best.tape_spec_ids);

        // The baseline honored its declared all-base assignment, and the
        // independent acceptance path agrees on the graded ledgers.
        assert_eq!(
            record.baseline.geometry.tape_spec_ids.as_deref(),
            Some(["base", "base"].map(str::to_string).as_slice())
        );
        assert_eq!(
            record.acceptance.baseline.cost_agreement_status,
            Status::Pass
        );
    }

    /// The embedded registry alone cannot satisfy a graded case's named
    /// spec when that spec binds a synthetic dataset id; a stray supply is
    /// rejected, and the declared supply resolves alongside the embedded
    /// base binding.
    #[test]
    fn graded_refinement_resolves_each_binding_to_its_own_dataset() {
        let base_ds = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let weak = base_ds.scaled_ic(0.725).unwrap();
        let json = v11_graded_case_json(&weak.metadata.id, &weak.metadata.csv_sha256);

        // A dataset no binding declares is a stray supply: rejected.
        let stray = base_ds.scaled_ic(0.5).unwrap();
        assert!(
            run_coupled_refine_case_with_dataset(
                &json,
                &CoupledRefineOptions::default(),
                Some(stray),
            )
            .is_err()
        );

        // The declared weak dataset resolves the `cheap` binding while the
        // base binding falls to the embedded registry.
        let record = run_coupled_refine_case_with_dataset(
            &json,
            &CoupledRefineOptions::default(),
            Some(weak),
        )
        .unwrap();
        assert_eq!(record.dataset_id, "robinson-superpower-ap-v3");
        assert_eq!(record.case.schema, "optcoil-coupled-search/v11");
    }

    #[test]
    fn embedded_oc008_case_is_loadable_for_the_cli_but_is_never_run_by_this_test_suite() {
        // Parsing/validating the frozen case is not "running the search" --
        // this only exercises schema validity, at negligible cost, and
        // deliberately never calls run_oc008()/run_coupled_refine_case on
        // it (the frozen OC-008 search is run detached, under the memory
        // guard, by the main session).
        let case = CoupledSearchCase::from_json(optcoil_model::coupled_search::OC008_JSON).unwrap();
        assert_eq!(
            case.schema,
            optcoil_model::coupled_search::COUPLED_SEARCH_CASE_SCHEMA_V2
        );
        assert!(case.refinement.is_some());
    }
}
