//! Sensitivity sweeps over a coupled-search case (`optcoil-sensitivity/v1`,
//! `/v2` and `/v3` specs): a full-factorial grid of declared perturbations,
//! each point an ordinary `run_coupled_search_case` execution of the
//! mutated case. v2 adds the `requirement_b_target_t` axis — the
//! certified-ceiling question run as a declared grid of full searches; v3
//! adds the `utilization_limit` axis — the capacity-margin frontier,
//! "what does headroom cost?" run the same way.
//!
//! A sweep answers "does the optimum survive a −10% tape lot / a +4 K
//! operating point / the upper price band?" — declared engineering
//! questions, not uncertainty quantification. Every point's
//! `case_sha256` and `dataset_csv_sha256` bind exactly what ran — and
//! `case_json` stores the mutated case verbatim — so a scaled dataset can
//! never be mistaken for the base measurements.

use std::sync::atomic::AtomicBool;

use crate::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use optcoil_model::{
    Status,
    coupled_search::CoupledSearchCase,
    material::MaterialDataset,
    sensitivity::{SensitivityAxis, SensitivitySpec},
};

use crate::{
    RunError,
    coupled_search::{CoupledSearchOptions, run_coupled_search_case_with_dataset},
    field::{RuntimeInfo, runtime_info},
};

pub const SENSITIVITY_RECORD_SCHEMA: &str = "optcoil-sensitivity-record/v2";

/// One axis assignment in a sweep point: `axis` is the spec's kind id
/// (`ic_scale`, `temperature_k`, `price_usd_per_m`,
/// `requirement_b_target_t`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityValue {
    pub axis: String,
    pub value: f64,
}

/// One full-factorial point: the mutated case's hash, the dataset identity
/// it actually ran against, and the run's headline outcome. `error` is set
/// (and the verdict fields stay `None`) when the mutated case itself is
/// invalid — e.g. a temperature outside the dataset's span — rather than
/// discarding the whole sweep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivityPoint {
    pub values: Vec<SensitivityValue>,
    pub case_sha256: String,
    /// The mutated case JSON verbatim — every point is independently
    /// re-runnable (`case_sha256` is this string's hash).
    pub case_json: String,
    pub dataset_id: String,
    pub dataset_csv_sha256: String,
    /// The selected optimum's geometry, e.g. `{"turns_along_normal": 200,
    /// "tapes_along_width": 8, "strands_parallel": 1}`.
    pub optimum: Option<crate::coupled_search::CandidateGeometry>,
    pub optimum_total_usd: Option<f64>,
    pub baseline_total_usd: Option<f64>,
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    pub search_status: Status,
    pub agreement_status: Status,
    pub error: Option<String>,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitivitySweepRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub spec: SensitivitySpec,
    pub spec_sha256: String,
    /// SHA-256 of the *unperturbed* case file bytes — the sweep's own
    /// identity input. Each point additionally carries the sha of the
    /// mutated case that actually ran.
    pub case_sha256: String,
    /// The dataset the base case declared, before any `ic_scale`
    /// perturbation; points report their own (possibly scaled) identity.
    pub base_dataset_id: String,
    pub base_dataset_csv_sha256: String,
    pub coupled_search_model_id: String,
    pub coupled_search_checker_id: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    pub points: Vec<SensitivityPoint>,
    pub limitations: Vec<String>,
}

impl SensitivitySweepRecord {
    pub fn write_new(&self, path: impl AsRef<std::path::Path>) -> Result<(), RunError> {
        crate::write_json_new(self, path)
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Run every full-factorial point of `spec_json` over `case_json`.
/// `dataset` follows the same rule as
/// [`run_coupled_search_case_with_dataset`]: when `Some`, it replaces the
/// embedded lookup and must match the case's declared identity.
pub fn run_sensitivity_sweep(
    case_json: &str,
    spec_json: &str,
    options: &CoupledSearchOptions,
    dataset: Option<MaterialDataset>,
    cancel: &AtomicBool,
) -> Result<SensitivitySweepRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;

    let spec = SensitivitySpec::from_json(spec_json)?;
    let base_case = CoupledSearchCase::from_json(case_json)?;
    // Same threads contract as the inner runner — checked once up-front
    // so an invalid --threads fails the sweep instead of filling every
    // point with the same error.
    let execution_threads = match options.threads {
        Some(0) => {
            return Err(RunError::Invalid("--threads must be at least 1".into()));
        }
        Some(t) if t > base_case.execution.max_threads => {
            return Err(RunError::Invalid(format!(
                "--threads may only lower the case's own execution.max_threads ({}); got {t}",
                base_case.execution.max_threads
            )));
        }
        Some(t) => t,
        None => base_case.execution.max_threads,
    };
    let base_dataset = match dataset {
        Some(dataset) => dataset,
        None => MaterialDataset::embedded_by_id(&base_case.material.dataset_id)?,
    };
    base_case.validate_against_dataset(&base_dataset)?;

    // Full-factorial index space over the spec's axes, in spec order.
    let combos: Vec<Vec<(usize, f64)>> = {
        let mut combos = vec![Vec::new()];
        for (axis_i, axis) in spec.axes.iter().enumerate() {
            let mut next = Vec::with_capacity(combos.len() * axis.values().len());
            for combo in &combos {
                for &value in axis.values() {
                    let mut c = combo.clone();
                    c.push((axis_i, value));
                    next.push(c);
                }
            }
            combos = next;
        }
        combos
    };

    let mut points = Vec::with_capacity(combos.len());
    for combo in combos {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(RunError::Cancelled);
        }
        let point_started = Instant::now();
        let mut case = base_case.clone();
        let mut ds = base_dataset.clone();
        let mut mutation_failed = None;
        for &(axis_i, value) in &combo {
            match &spec.axes[axis_i] {
                SensitivityAxis::IcScale { .. } => match ds.scaled_ic(value) {
                    Ok(scaled) => {
                        case.material.dataset_id = scaled.metadata.id.clone();
                        case.material.csv_sha256 = scaled.metadata.csv_sha256.clone();
                        ds = scaled;
                    }
                    Err(e) => mutation_failed = Some(e.to_string()),
                },
                SensitivityAxis::TemperatureK { .. } => {
                    case.operating.temperature_k = value;
                }
                SensitivityAxis::PriceUsdPerM { .. } => {
                    case.cost.price_usd_per_m = value;
                }
                SensitivityAxis::RequirementBTargetT { .. } => {
                    case.requirement.b_target_t = value;
                }
                SensitivityAxis::UtilizationLimit { .. } => {
                    case.limits.utilization_limit = value;
                }
            }
        }
        let values: Vec<SensitivityValue> = combo
            .iter()
            .map(|&(axis_i, value)| SensitivityValue {
                axis: spec.axes[axis_i].kind_id().into(),
                value,
            })
            .collect();

        // Serialize once — every outcome below stores it verbatim.
        let mutated_json =
            serde_json::to_string(&case).map_err(|e| RunError::Invalid(e.to_string()))?;
        let point = match mutation_failed {
            Some(error) => SensitivityPoint {
                values,
                case_sha256: String::new(),
                case_json: mutated_json.clone(),
                dataset_id: ds.metadata.id.clone(),
                dataset_csv_sha256: ds.metadata.csv_sha256.clone(),
                optimum: None,
                optimum_total_usd: None,
                baseline_total_usd: None,
                savings_usd: None,
                savings_percent: None,
                search_status: Status::NotEvaluated,
                agreement_status: Status::NotEvaluated,
                error: Some(error),
                elapsed_ms: point_started.elapsed().as_secs_f64() * 1000.0,
            },
            None => {
                match run_coupled_search_case_with_dataset(
                    &mutated_json,
                    options,
                    Some(ds.clone()),
                    cancel,
                ) {
                    Ok(record) => {
                        let best = record.best_index.map(|i| &record.candidates[i]);
                        SensitivityPoint {
                            values,
                            case_sha256: record.case_sha256.clone(),
                            case_json: mutated_json.clone(),
                            dataset_id: record.dataset_id.clone(),
                            dataset_csv_sha256: record.dataset_csv_sha256.clone(),
                            optimum: best.map(|c| c.geometry.clone()),
                            optimum_total_usd: best.map(|c| c.cost.total_usd),
                            baseline_total_usd: Some(
                                record.candidates[record.baseline_index].cost.total_usd,
                            ),
                            savings_usd: record.savings_usd,
                            savings_percent: record.savings_percent,
                            search_status: record.search_status,
                            agreement_status: record.acceptance.agreement_status,
                            error: None,
                            elapsed_ms: record.elapsed_ms,
                        }
                    }
                    // Cancellation propagates — a partial sweep must not
                    // present itself as a complete record.
                    Err(RunError::Cancelled) => return Err(RunError::Cancelled),
                    Err(e) => SensitivityPoint {
                        values,
                        case_sha256: hash(mutated_json.as_bytes()),
                        case_json: mutated_json.clone(),
                        dataset_id: ds.metadata.id.clone(),
                        dataset_csv_sha256: ds.metadata.csv_sha256.clone(),
                        optimum: None,
                        optimum_total_usd: None,
                        baseline_total_usd: None,
                        savings_usd: None,
                        savings_percent: None,
                        search_status: Status::NotEvaluated,
                        agreement_status: Status::NotEvaluated,
                        error: Some(e.to_string()),
                        elapsed_ms: point_started.elapsed().as_secs_f64() * 1000.0,
                    },
                }
            }
        };
        points.push(point);
    }

    Ok(SensitivitySweepRecord {
        schema: SENSITIVITY_RECORD_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        spec_sha256: hash(spec_json.as_bytes()),
        case_sha256: hash(case_json.as_bytes()),
        base_dataset_id: base_dataset.metadata.id.clone(),
        base_dataset_csv_sha256: base_dataset.metadata.csv_sha256.clone(),
        coupled_search_model_id: crate::coupled_search::COUPLED_SEARCH_MODEL_ID.into(),
        coupled_search_checker_id:
            crate::search_acceptance::COUPLED_SEARCH_ACCEPTANCE_CHECKER_ID.into(),
        started_unix_ms,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        runtime: {
            let mut r: RuntimeInfo = runtime_info();
            r.execution_threads = execution_threads;
            r
        },
        spec,
        points,
        limitations: vec![
            "Each point reruns the same screening model under one declared perturbation; a sweep is a set of named what-ifs, not an uncertainty distribution.".into(),
            "ic_scale points run on a re-hashed scaled copy of the dataset — the scaled CSV is synthetic, bound by the point's dataset_csv_sha256 and mutated case_sha256.".into(),
            "Points whose mutated case is invalid (e.g. a temperature outside the dataset span) are recorded with an error and NotEvaluated verdicts rather than discarding the sweep.".into(),
            "requirement_b_target_t points each ask whether the declared winding space can verifiably meet that target field; the response need not be monotone (dataset floors can leave lower targets INCONCLUSIVE), so a certified ceiling is the largest declared value whose search_status is PASS — declared-grid resolution, not a bisected or continuous bound.".into(),
            "utilization_limit points each ask what the declared capacity margin costs: a tighter gate can remove the cheapest passing geometry outright, so the cost-vs-margin frontier can step rather than slope. The axis perturbs the declared screening bound only — it does not change what the tape measured.".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn reduced_case_json() -> String {
        // Mirrors coupled_search's reduced fixture: tiny choices, trivial
        // requirement, one-station sampling — cheap to run repeatedly.
        r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "reduced-sweep-test-case",
  "provenance": "sensitivity.rs unit test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [3], "tapes_along_width": [2]},
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
  "baseline": {"turns_along_normal": 3, "tapes_along_width": 2},
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
        .into()
    }

    #[test]
    fn sweep_runs_full_factorial_and_binds_each_point() {
        let spec = r#"{"schema": "optcoil-sensitivity/v1", "id": "t",
            "provenance": "test", "axes": [
            {"kind": "price_usd_per_m", "values": [30.0, 60.0]},
            {"kind": "ic_scale", "values": [1.0, 0.9]}
        ]}"#;
        let record = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.points.len(), 4);
        assert_eq!(
            record.base_dataset_csv_sha256,
            "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354"
        );
        // ic_scale=0.9 points carry a derived dataset identity; the
        // ic_scale=1.0... wait — scaling at exactly 1.0 still produces a
        // derived id because the perturbation is applied unconditionally.
        // That keeps every point's binding uniform.
        for point in &record.points {
            assert!(!point.case_sha256.is_empty());
            assert!(point.error.is_none(), "point error: {:?}", point.error);
            assert_ne!(point.search_status, Status::NotEvaluated);
            let ic = point
                .values
                .iter()
                .find(|v| v.axis == "ic_scale")
                .unwrap()
                .value;
            if ic == 1.0 {
                assert_eq!(point.dataset_id, "robinson-superpower-ap-v3@icx1");
            } else {
                assert!(point.dataset_id.ends_with("@icx0.9"));
            }
        }
        // The 60 $/m points must cost more than the 30 $/m points at the
        // same optimum — the axis actually did something.
        let at = |price: f64, ic: f64| {
            record
                .points
                .iter()
                .find(|p| {
                    p.values
                        .iter()
                        .any(|v| v.axis == "price_usd_per_m" && v.value == price)
                        && p.values
                            .iter()
                            .any(|v| v.axis == "ic_scale" && v.value == ic)
                })
                .unwrap()
        };
        assert!(
            at(60.0, 1.0).baseline_total_usd.unwrap() > at(30.0, 1.0).baseline_total_usd.unwrap()
        );
    }

    #[test]
    fn sweep_records_invalid_points_and_propagates_cancellation() {
        // A temperature outside the dataset's nominal span fails at run
        // time — the point is recorded with an error, not fatal.
        let spec = r#"{"schema": "optcoil-sensitivity/v1", "id": "t",
            "provenance": "test", "axes": [
            {"kind": "temperature_k", "values": [21.0, 4.0]}
        ]}"#;
        let record = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.points.len(), 2);
        let bad = record
            .points
            .iter()
            .find(|p| p.values[0].value == 4.0)
            .unwrap();
        assert!(bad.error.is_some());
        assert_eq!(bad.search_status, Status::NotEvaluated);

        let cancelled = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(true),
        );
        assert!(matches!(cancelled, Err(RunError::Cancelled)));
    }

    #[test]
    fn requirement_b_target_axis_mutates_the_case_each_point_ran() {
        let spec = r#"{"schema": "optcoil-sensitivity/v2", "id": "t",
            "provenance": "test", "axes": [
            {"kind": "requirement_b_target_t", "values": [0.05, 0.02]}
        ]}"#;
        let record = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.points.len(), 2);
        for point in &record.points {
            assert!(point.error.is_none(), "point error: {:?}", point.error);
            assert_eq!(point.values[0].axis, "requirement_b_target_t");
            let case: serde_json::Value = serde_json::from_str(&point.case_json).unwrap();
            assert_eq!(
                case["requirement"]["b_target_t"].as_f64().unwrap(),
                point.values[0].value
            );
            // Each point is a real search with its own acceptance.
            assert_ne!(point.search_status, Status::NotEvaluated);
            assert_ne!(point.agreement_status, Status::NotEvaluated);
        }
    }

    #[test]
    fn utilization_limit_axis_mutates_the_case_each_point_ran() {
        let spec = r#"{"schema": "optcoil-sensitivity/v3", "id": "t",
            "provenance": "test", "axes": [
            {"kind": "utilization_limit", "values": [0.8, 0.5]}
        ]}"#;
        let record = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.points.len(), 2);
        for point in &record.points {
            assert!(point.error.is_none(), "point error: {:?}", point.error);
            assert_eq!(point.values[0].axis, "utilization_limit");
            let case: serde_json::Value = serde_json::from_str(&point.case_json).unwrap();
            assert_eq!(
                case["limits"]["utilization_limit"].as_f64().unwrap(),
                point.values[0].value
            );
            assert_ne!(point.search_status, Status::NotEvaluated);
            assert_ne!(point.agreement_status, Status::NotEvaluated);
        }
    }

    /// The specimen-spread reading: `ic_scale` × `requirement_b_target_t`
    /// in one sweep produces, per declared scale, the certified ceiling —
    /// the largest target whose `search_status` is PASS. The record must
    /// carry enough to read that band per scale row: both axis values,
    /// the mutated case hash and the derived dataset identity.
    #[test]
    fn ic_scale_and_b_target_compose_into_a_per_scale_ceiling_band() {
        let spec = r#"{"schema": "optcoil-sensitivity/v2", "id": "t",
            "provenance": "test", "axes": [
            {"kind": "ic_scale", "values": [1.0, 0.8]},
            {"kind": "requirement_b_target_t", "values": [0.05, 0.02]}
        ]}"#;
        let record = run_sensitivity_sweep(
            &reduced_case_json(),
            spec,
            &CoupledSearchOptions::default(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(record.points.len(), 4);
        for point in &record.points {
            assert!(point.error.is_none(), "point error: {:?}", point.error);
            assert_eq!(point.values.len(), 2);
            // The two mutations are both in the recorded case — the scaled
            // dataset identity and the mutated target.
            let case: serde_json::Value = serde_json::from_str(&point.case_json).unwrap();
            let scale = point
                .values
                .iter()
                .find(|v| v.axis == "ic_scale")
                .unwrap()
                .value;
            let target = point
                .values
                .iter()
                .find(|v| v.axis == "requirement_b_target_t")
                .unwrap()
                .value;
            assert_eq!(case["requirement"]["b_target_t"].as_f64().unwrap(), target);
            assert_eq!(
                point.dataset_id,
                format!("robinson-superpower-ap-v3@icx{scale}")
            );
        }
        // The band read is a consumer computation over the record: per
        // scale, the largest declared target that still passes.
        for scale in [1.0, 0.8] {
            let ceiling = record
                .points
                .iter()
                .filter(|p| {
                    p.values
                        .iter()
                        .any(|v| v.axis == "ic_scale" && v.value == scale)
                })
                .filter(|p| p.search_status == Status::Pass)
                .map(|p| {
                    p.values
                        .iter()
                        .find(|v| v.axis == "requirement_b_target_t")
                        .unwrap()
                        .value
                })
                .fold(None, |acc: Option<f64>, t| {
                    Some(acc.map_or(t, |a| a.max(t)))
                });
            assert!(ceiling.is_some(), "scale {scale} should certify a ceiling");
        }
    }
}
