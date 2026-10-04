//! Deterministic, bounded exact enumeration for small allocation cases.
//! Desktop and CLI share this API. No external solver is invoked here.

/// Time types: `std::time` panics on wasm32 — `web-time` supplies the
/// same API against `performance.now()`/`Date` there and forwards to
/// `std` on native.
#[cfg(target_arch = "wasm32")]
pub(crate) mod time {
    pub(crate) use web_time::{Instant, SystemTime, UNIX_EPOCH};
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod time {
    pub(crate) use std::time::{Instant, SystemTime, UNIX_EPOCH};
}

pub mod acceptance;
pub mod bakeoff;
pub mod bom;
pub mod coupled;
pub mod coupled_refine;
pub mod coupled_search;
pub mod field;
pub mod fieldmap;
pub mod gradereport;
pub mod kernel_crosscheck;
pub mod material;
pub mod preflight;
pub mod reel;
pub mod report;
pub mod reprice;
pub mod review;
pub mod robustness;
pub mod search_acceptance;
pub mod sensitivity;
pub mod sizing;
pub mod study;
pub mod verify;

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::time::{Instant, SystemTime, UNIX_EPOCH};

use optcoil_model::{
    Allocation, Assessment, Candidate, Case, ModelError, Status, allocation_changed,
};
use optcoil_physics::{SCREENING_MODEL_ID, tape_capacity_a};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const SEARCH_ID: &str = "bounded-exhaustive-allocation/v1";
pub const RUN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Field(#[from] optcoil_physics::racetrack::FieldError),
    #[error("{0}")]
    Invalid(String),
    /// The caller's cancellation flag was set before the run completed;
    /// no record is produced — a partial candidate list is not evidence.
    #[error("run cancelled before completion")]
    Cancelled,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOptions {
    pub max_evaluations: u64,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_evaluations: 100_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    Exhausted,
    EvaluationLimit,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    pub optcoil_version: String,
    pub case_sha256: String,
    /// Includes parsed case, options and implementation identities; not a cache.
    pub input_sha256: String,
    pub screening_model_id: String,
    pub checker_id: String,
    pub search_id: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    pub case: Case,
    pub options: SearchOptions,
    pub search_space_size: u64,
    pub evaluated_candidates: u64,
    pub screening_pass_candidates: u64,
    pub inconclusive_candidates: u64,
    pub termination: Termination,
    pub proven_optimal_in_screening_model: bool,
    pub baseline: Assessment,
    pub best: Assessment,
    pub savings_usd: f64,
    pub savings_percent: f64,
    pub limitations: Vec<String>,
}

impl RunRecord {
    /// Exclusive creation protects earlier results. Serialize before touching disk.
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub(crate) fn write_json_new(
    value: &impl Serialize,
    path: impl AsRef<Path>,
) -> Result<(), RunError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let path = path.as_ref();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}

pub fn run(case: &Case, options: &SearchOptions) -> Result<RunRecord, RunError> {
    run_cancellable(case, options, &AtomicBool::new(false))
}

pub fn run_cancellable(
    case: &Case,
    options: &SearchOptions,
    cancel: &AtomicBool,
) -> Result<RunRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;
    case.validate()?;
    if options.max_evaluations == 0 {
        return Err(RunError::Invalid("max_evaluations must be positive".into()));
    }
    let baseline = acceptance::assess(case, &case.baseline)?;
    if baseline.screening_status != Status::Pass {
        return Err(RunError::Invalid(format!(
            "baseline screening must PASS; got {:?}",
            baseline.screening_status
        )));
    }
    let mut search_space_size = 1_u64;
    let mut choices = Vec::new();
    for module in &case.modules {
        let count = module.allowed_grade_ids.len() as u64 * u64::from(module.max_tapes);
        search_space_size = search_space_size.checked_mul(count).ok_or_else(|| {
            RunError::Invalid("search space exceeds u64; use a future structured optimizer".into())
        })?;
        choices.push(
            module
                .allowed_grade_ids
                .iter()
                .flat_map(|id| {
                    (1..=module.max_tapes).map(move |tapes| Allocation {
                        module_id: module.id.clone(),
                        grade_id: id.clone(),
                        tapes,
                    })
                })
                .collect::<Vec<_>>(),
        );
    }
    let mut best_candidate = case.baseline.clone();
    let mut best_score = objective(case, &best_candidate);
    let mut evaluated_candidates = 0;
    let mut screening_pass_candidates = 0;
    let mut inconclusive_candidates = 0;
    let termination = loop {
        if cancel.load(Ordering::Relaxed) {
            break Termination::Cancelled;
        }
        if evaluated_candidates == search_space_size {
            break Termination::Exhausted;
        }
        if evaluated_candidates == options.max_evaluations {
            break Termination::EvaluationLimit;
        }
        // Mixed-radix indexing avoids recursive depth and makes ordering reproducible.
        let mut index = evaluated_candidates;
        let allocations = choices
            .iter()
            .map(|module_choices| {
                let choice = (index % module_choices.len() as u64) as usize;
                index /= module_choices.len() as u64;
                module_choices[choice].clone()
            })
            .collect();
        let candidate = Candidate { allocations };
        evaluated_candidates += 1;
        match screen(case, &candidate) {
            Status::Pass => {
                screening_pass_candidates += 1;
                let score = objective(case, &candidate);
                if score < best_score {
                    best_score = score;
                    best_candidate = candidate;
                }
            }
            Status::Inconclusive => inconclusive_candidates += 1,
            Status::Fail | Status::NotEvaluated => {}
        }
    };
    let best = acceptance::assess(case, &best_candidate)?;
    if best.screening_status != Status::Pass
        || (best_score - best.cost.total_usd).abs() > 1e-8 * best.cost.total_usd.max(1.0)
    {
        return Err(RunError::Invalid(
            "candidate failed the separate cost/constraint acceptance path".into(),
        ));
    }
    let savings_usd = baseline.cost.total_usd - best.cost.total_usd;
    let case_sha256 = hash(&serde_json::to_vec(case)?);
    let input_sha256 = hash(&serde_json::to_vec(&(
        case,
        options,
        env!("CARGO_PKG_VERSION"),
        SCREENING_MODEL_ID,
        acceptance::CHECKER_ID,
        SEARCH_ID,
    ))?);
    Ok(RunRecord {
        schema_version: RUN_SCHEMA_VERSION,
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        case_sha256,
        input_sha256,
        screening_model_id: SCREENING_MODEL_ID.into(),
        checker_id: acceptance::CHECKER_ID.into(),
        search_id: SEARCH_ID.into(),
        started_unix_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        case: case.clone(),
        options: options.clone(),
        search_space_size,
        evaluated_candidates,
        screening_pass_candidates,
        inconclusive_candidates,
        termination,
        proven_optimal_in_screening_model: termination == Termination::Exhausted && inconclusive_candidates == 0,
        savings_percent: savings_usd / baseline.cost.total_usd * 100.0,
        savings_usd,
        baseline,
        best,
        limitations: vec![
            "Synthetic allocation benchmark; savings are not evidence of real magnet cost reduction.".into(),
            "Fixed lengths and prescribed fields; ideal equal current sharing between tapes.".into(),
            "Cost checker is separate; capacity physics is shared, not independently validated.".into(),
            "Coupled magnetics, mechanics, thermal behavior, quench and actual manufacturing are NOT_EVALUATED.".into(),
            "Optimality applies only to enumerated grade/count choices in this screening model.".into(),
        ],
    })
}

fn screen(case: &Case, candidate: &Candidate) -> Status {
    for (i, interface) in case.interfaces.iter().enumerate() {
        if !interface.allow_allocation_change
            && allocation_changed(&candidate.allocations[i], &candidate.allocations[i + 1])
        {
            return Status::Fail;
        }
    }
    let mut unknown = false;
    for (module, allocation) in case.modules.iter().zip(&candidate.allocations) {
        let Some(grade) = case.grade(&allocation.grade_id) else {
            return Status::Fail;
        };
        match tape_capacity_a(grade, &module.operating_point) {
            Some(ic)
                if ic * f64::from(allocation.tapes) * case.utilization_limit
                    < case.circuit_current_a =>
            {
                return Status::Fail;
            }
            None => unknown = true,
            Some(_) => {}
        }
    }
    if unknown {
        Status::Inconclusive
    } else {
        Status::Pass
    }
}

/// Search's compact objective. Acceptance independently builds a quantity ledger.
fn objective(case: &Case, candidate: &Candidate) -> f64 {
    let conductor: f64 = case
        .modules
        .iter()
        .zip(&candidate.allocations)
        .map(|(module, allocation)| {
            let price = case
                .grade(&allocation.grade_id)
                .map_or(f64::INFINITY, |grade| grade.price_usd_per_m);
            f64::from(module.turns)
                * module.mean_turn_length_m
                * f64::from(allocation.tapes)
                * price
                * (1.0 + case.scrap_fraction)
        })
        .sum();
    let joints: f64 = case
        .interfaces
        .iter()
        .enumerate()
        .map(|(i, interface)| {
            interface.fixed_cost_usd
                + if allocation_changed(&candidate.allocations[i], &candidate.allocations[i + 1]) {
                    interface.change_cost_usd
                } else {
                    0.0
                }
        })
        .sum();
    conductor + joints + case.assembly_cost_per_module_usd * case.modules.len() as f64
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fixture_matches_hand_calculation() {
        let record = run(&Case::demo().unwrap(), &SearchOptions::default()).unwrap();
        assert_eq!(record.baseline.cost.total_usd, 932.0);
        assert_eq!(record.best.cost.total_usd, 824.0);
        assert_eq!(record.savings_usd, 108.0);
        assert_eq!(
            record
                .best
                .candidate
                .allocations
                .iter()
                .map(|a| (a.grade_id.as_str(), a.tapes))
                .collect::<Vec<_>>(),
            [("standard", 4), ("standard", 5), ("premium", 3)]
        );
        assert_eq!(record.evaluated_candidates, 4096);
        assert!(record.proven_optimal_in_screening_model);
        assert_eq!(record.best.engineering_status, Status::NotEvaluated);
        assert_eq!(record.best.cost.joints_usd, 74.0);
        assert_eq!(record.best.cost.scrap_usd, 60.0);
    }

    #[test]
    fn forbidden_changes_and_expensive_joints_change_the_optimum() {
        let mut case = Case::demo().unwrap();
        for interface in &mut case.interfaces {
            interface.allow_allocation_change = false;
        }
        let record = run(&case, &SearchOptions::default()).unwrap();
        assert_eq!(record.best.cost.total_usd, 932.0);
        let mut altered = case.baseline.clone();
        altered.allocations[0].tapes = 4;
        assert_eq!(
            acceptance::assess(&case, &altered)
                .unwrap()
                .screening_status,
            Status::Fail
        );
        for interface in &mut case.interfaces {
            interface.allow_allocation_change = true;
            interface.change_cost_usd = 10_000.0;
        }
        assert_eq!(
            run(&case, &SearchOptions::default())
                .unwrap()
                .best
                .cost
                .total_usd,
            932.0
        );
    }

    #[test]
    fn limits_and_cancellation_never_claim_optimality() {
        let case = Case::demo().unwrap();
        let partial = run(&case, &SearchOptions { max_evaluations: 1 }).unwrap();
        assert_eq!(partial.termination, Termination::EvaluationLimit);
        assert_eq!(partial.best.cost.total_usd, partial.baseline.cost.total_usd);
        assert!(!partial.proven_optimal_in_screening_model);
        let cancelled =
            run_cancellable(&case, &SearchOptions::default(), &AtomicBool::new(true)).unwrap();
        assert_eq!(cancelled.termination, Termination::Cancelled);
        assert_eq!(cancelled.evaluated_candidates, 0);
        assert!(!cancelled.proven_optimal_in_screening_model);
    }

    #[test]
    fn unsupported_materials_remain_unknown_and_invalid_baseline_is_rejected() {
        let mut case = Case::demo().unwrap();
        case.grades[0].domain.max_field_t = 1.0;
        let record = run(&case, &SearchOptions::default()).unwrap();
        assert!(record.inconclusive_candidates > 0);
        assert!(!record.proven_optimal_in_screening_model);
        let mut candidate = case.baseline.clone();
        candidate.allocations[0].grade_id = "standard".into();
        let assessment = acceptance::assess(&case, &candidate).unwrap();
        assert_eq!(assessment.screening_status, Status::Inconclusive);
        assert_eq!(assessment.modules[0].allowed_current_a, None);
        case.baseline = candidate;
        assert!(run(&case, &SearchOptions::default()).is_err());
    }

    #[test]
    fn identity_is_reproducible_and_tracks_inputs_and_settings() {
        let mut case = Case::demo().unwrap();
        let options = SearchOptions::default();
        let first = run(&case, &options).unwrap();
        let second = run(&case, &options).unwrap();
        assert_eq!(first.input_sha256, second.input_sha256);
        assert_eq!(first.best.candidate, second.best.candidate);
        case.grades[0].price_usd_per_m += 0.01;
        assert_ne!(
            first.input_sha256,
            run(&case, &options).unwrap().input_sha256
        );
        assert_ne!(
            first.input_sha256,
            run(
                &Case::demo().unwrap(),
                &SearchOptions { max_evaluations: 2 }
            )
            .unwrap()
            .input_sha256
        );
        let restored: RunRecord =
            serde_json::from_str(&serde_json::to_string(&first).unwrap()).unwrap();
        assert_eq!(first.best.cost.total_usd, restored.best.cost.total_usd);
    }
}
