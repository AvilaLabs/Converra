//! Headless measured-data import, bounded queries and withheld-plane validation.

use std::{collections::HashSet, path::Path};

use crate::time::Instant;

use optcoil_model::{
    Status,
    material::{
        MaterialAcceptance, MaterialBenchmark, MaterialDataset, MaterialFold, MaterialQuery,
        OC003_JSON, OC005_JSON, SUPERPOWER_CSV, SUPERPOWER_METADATA, ValidationAxis,
        fold_holdout_lists,
    },
};
use optcoil_physics::critical_current::{
    IcEstimate, IcInterpolationMethod, IcInterpolator, IcMeshSummary, LOG_IC_MODEL_ID,
};
use serde::{Deserialize, Serialize};

use crate::{RunError, hash, write_json_new};

pub const MATERIAL_CHECKER_ID: &str = "withheld-nominal-planes-measured-coordinate-validation/v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialValidationPoint {
    pub source_row: u32,
    pub coordinates: [f64; 3],
    pub nominal_coordinates: [f64; 3],
    pub measured_ic_a_per_m: f64,
    pub prediction: Option<IcEstimate>,
    pub relative_error: Option<f64>,
    pub signed_relative_error: Option<f64>,
    /// Number of this point's nominal coordinates that lie on a withheld
    /// plane: always 1 for a single-axis fold; 1..=3 for a multi-axis fold.
    pub off_grid_axes: u8,
}

/// Whole-fold and per-stratum error metrics share this shape; a stratum
/// groups scored points of a multi-axis fold by `off_grid_axes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialStratumResult {
    pub off_grid_axes: u8,
    pub points: usize,
    pub covered_points: usize,
    pub coverage_fraction: f64,
    pub p95_relative_error: Option<f64>,
    pub max_relative_error: Option<f64>,
    pub max_positive_relative_error: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialFoldResult {
    pub fold: MaterialFold,
    pub training_source_rows: Vec<u32>,
    /// Removed from training but outside the predeclared scoring region.
    pub unscored_boundary_source_rows: Vec<u32>,
    pub mesh: IcMeshSummary,
    pub points: Vec<MaterialValidationPoint>,
    pub covered_points: usize,
    pub coverage_fraction: f64,
    pub p95_relative_error: Option<f64>,
    pub max_relative_error: Option<f64>,
    pub max_positive_relative_error: Option<f64>,
    /// Empty for single-axis folds; one entry per off-grid-axis count
    /// (1..=3) for a multi-axis fold.
    pub strata: Vec<MaterialStratumResult>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialRunRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub checker_id: String,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub elapsed_ms: f64,
    pub benchmark: MaterialBenchmark,
    pub dataset: MaterialDataset,
    pub full_mesh: IcMeshSummary,
    pub max_node_relative_error: f64,
    pub folds: Vec<MaterialFoldResult>,
    pub interpolation_validation_status: Status,
    pub engineering_status: Status,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialQueryRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub dataset: MaterialDataset,
    pub query: MaterialQuery,
    pub mesh: IcMeshSummary,
    pub estimate: Option<IcEstimate>,
    pub domain_status: Status,
    pub criterion_status: Status,
    pub engineering_status: Status,
    pub detail: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialSuiteRecord {
    pub schema: String,
    pub input_sha256: String,
    pub linear_baseline: MaterialRunRecord,
    pub logarithmic_development: MaterialRunRecord,
    pub reserved_challenge: MaterialRunRecord,
    pub qualified_interpolation_model_id: Option<String>,
    pub interpolation_validation_status: Status,
    pub engineering_status: Status,
}

impl MaterialSuiteRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_material_suite() -> Result<MaterialSuiteRecord, RunError> {
    let linear_baseline = run_oc003()?;
    let logarithmic_development = run_oc003_log()?;
    let reserved_challenge = run_oc003_challenge()?;
    let statuses = [
        logarithmic_development.interpolation_validation_status,
        reserved_challenge.interpolation_validation_status,
    ];
    let interpolation_validation_status = if statuses.contains(&Status::Fail) {
        Status::Fail
    } else if statuses.iter().all(|s| *s == Status::Pass) {
        Status::Pass
    } else {
        Status::Inconclusive
    };
    let input_sha256 = hash(&serde_json::to_vec(&(
        &linear_baseline.input_sha256,
        &logarithmic_development.input_sha256,
        &reserved_challenge.input_sha256,
    ))?);
    Ok(MaterialSuiteRecord {
        schema: "optcoil-material-validation-suite/v2".into(),
        input_sha256,
        linear_baseline,
        logarithmic_development,
        reserved_challenge,
        qualified_interpolation_model_id: (interpolation_validation_status == Status::Pass)
            .then(|| LOG_IC_MODEL_ID.into()),
        interpolation_validation_status,
        engineering_status: Status::NotEvaluated,
    })
}

impl MaterialRunRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}
impl MaterialQueryRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_oc003() -> Result<MaterialRunRecord, RunError> {
    run_material_benchmark(
        &MaterialDataset::embedded()?,
        &MaterialBenchmark::from_json(OC003_JSON)?,
    )
}

pub fn run_oc003_log() -> Result<MaterialRunRecord, RunError> {
    run_material_benchmark(
        &MaterialDataset::embedded()?,
        &MaterialBenchmark::from_json(include_str!(
            "../../../benchmarks/measured/oc-003-log.json"
        ))?,
    )
}

pub fn run_oc003_challenge() -> Result<MaterialRunRecord, RunError> {
    let data = MaterialDataset::from_csv(
        include_str!(
            "../../../data/materials/robinson-superpower-ap-v3/challenge-20-50k/material.json"
        ),
        include_bytes!(
            "../../../data/materials/robinson-superpower-ap-v3/challenge-20-50k/measurements.csv"
        ),
    )?;
    run_material_benchmark(
        &data,
        &MaterialBenchmark::from_json(include_str!(
            "../../../benchmarks/measured/oc-003-challenge.json"
        ))?,
    )
}

/// OC-005: the frozen simultaneous multi-axis withheld-plane fold, run
/// against the embedded dataset. Kept separate from `run_material_suite`
/// (OC-003 only); OC-005 is exercised through `material-validate` and tests.
pub fn run_oc005() -> Result<MaterialRunRecord, RunError> {
    run_material_benchmark(
        &MaterialDataset::embedded()?,
        &MaterialBenchmark::from_json(OC005_JSON)?,
    )
}

pub fn run_material_benchmark(
    dataset: &MaterialDataset,
    benchmark: &MaterialBenchmark,
) -> Result<MaterialRunRecord, RunError> {
    let clock = Instant::now();
    dataset.validate()?;
    benchmark.validate()?;
    if benchmark.dataset_id != dataset.metadata.id {
        return Err(RunError::Invalid(
            "material benchmark dataset or method identity mismatch".into(),
        ));
    }
    let method = IcInterpolationMethod::from_id(&benchmark.method)
        .ok_or_else(|| RunError::Invalid("unsupported material interpolation method".into()))?;
    let model =
        IcInterpolator::with_method(&dataset.points, dataset.metadata.max_cell_spans, method)?;
    let mut max_node_relative_error = 0.0_f64;
    for point in &dataset.points {
        let estimate = model
            .evaluate(point.position())?
            .ok_or_else(|| RunError::Invalid("interpolator lost a source measurement".into()))?;
        max_node_relative_error =
            max_node_relative_error.max((estimate.ic_a_per_m / point.ic_a_per_m - 1.0).abs());
    }
    let nominal_low: [f64; 3] = std::array::from_fn(|k| {
        dataset
            .points
            .iter()
            .map(|p| p.nominal_position()[k])
            .fold(f64::INFINITY, f64::min)
    });
    let nominal_high: [f64; 3] = std::array::from_fn(|k| {
        dataset
            .points
            .iter()
            .map(|p| p.nominal_position()[k])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let mut folds = Vec::new();
    for fold in &benchmark.folds {
        let is_multi = matches!(fold.axis, ValidationAxis::Multi);
        let holdout_lists = fold_holdout_lists(fold);
        for (axis, list) in holdout_lists.iter().enumerate() {
            if list.iter().any(|&v| {
                v <= nominal_low[axis]
                    || v >= nominal_high[axis]
                    || !dataset
                        .points
                        .iter()
                        .any(|p| p.nominal_position()[axis] == v)
            }) {
                return Err(RunError::Invalid(
                    "held-out planes must be existing, strictly interior nominal levels".into(),
                ));
            }
        }
        // `axis_hit[k]` is true iff this point's k-th nominal coordinate lies
        // on a plane withheld by this fold. A single-axis fold can only ever
        // set its own axis; a multi-axis fold can set any subset. A point is
        // held iff any axis is hit, and off_grid_axes is just the hit count —
        // always 1 for a single-axis fold, 1..=3 for a multi-axis fold.
        let mut held: Vec<(_, [bool; 3])> = Vec::new();
        let mut training = Vec::new();
        for p in &dataset.points {
            let nominal = p.nominal_position();
            let axis_hit = std::array::from_fn(|k| holdout_lists[k].contains(&nominal[k]));
            if axis_hit.iter().any(|&hit| hit) {
                held.push((p.clone(), axis_hit));
            } else {
                training.push(p.clone());
            }
        }
        let training_source_rows: Vec<_> = training.iter().map(|p| p.source_row).collect();
        let training_ids: HashSet<_> = training_source_rows.iter().copied().collect();
        let interpolator =
            IcInterpolator::with_method(&training, dataset.metadata.max_cell_spans, method)?;
        let mut unscored_boundary_source_rows = Vec::new();
        let mut points = Vec::new();
        for (point, axis_hit) in held {
            let nominal = point.nominal_position();
            // Generalizes the single-axis boundary policy: a held point is an
            // unscored boundary omission iff some axis that this fold did NOT
            // withhold for this point sits on that axis's nominal boundary.
            if (0..3).any(|k| {
                !axis_hit[k] && (nominal[k] <= nominal_low[k] || nominal[k] >= nominal_high[k])
            }) {
                unscored_boundary_source_rows.push(point.source_row);
                continue;
            }
            let off_grid_axes = axis_hit.iter().filter(|&&hit| hit).count() as u8;
            let prediction = interpolator.evaluate(point.position())?;
            if prediction.as_ref().is_some_and(|p| {
                p.support
                    .iter()
                    .any(|s| !training_ids.contains(&s.source_row))
            }) {
                return Err(RunError::Invalid(
                    "held-out measurement leaked into interpolation support".into(),
                ));
            }
            let signed_relative_error = prediction
                .as_ref()
                .map(|p| (p.ic_a_per_m - point.ic_a_per_m) / point.ic_a_per_m);
            if signed_relative_error.is_some_and(|v| !v.is_finite()) {
                return Err(RunError::Invalid(
                    "nonfinite interpolation validation error".into(),
                ));
            }
            points.push(MaterialValidationPoint {
                source_row: point.source_row,
                coordinates: point.position(),
                nominal_coordinates: nominal,
                measured_ic_a_per_m: point.ic_a_per_m,
                prediction,
                relative_error: signed_relative_error.map(f64::abs),
                signed_relative_error,
                off_grid_axes,
            });
        }
        if points.is_empty() {
            return Err(RunError::Invalid(
                "no eligible withheld measurements under the scoring policy".into(),
            ));
        }
        let (
            covered_points,
            coverage_fraction,
            p95_relative_error,
            max_relative_error,
            max_positive_relative_error,
        ) = fold_metrics(&points);
        let mut status = assess_fold(
            coverage_fraction,
            p95_relative_error,
            max_relative_error,
            max_positive_relative_error,
            &benchmark.acceptance,
        );
        let strata = if is_multi {
            (1..=3u8)
                .map(|off_grid_axes| {
                    let subset: Vec<&MaterialValidationPoint> = points
                        .iter()
                        .filter(|p| p.off_grid_axes == off_grid_axes)
                        .collect();
                    stratum_result(off_grid_axes, &subset)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if is_multi && let Some(gate) = &benchmark.stratum_acceptance {
            let three_axis = strata.iter().find(|s| s.off_grid_axes == 3);
            let stratum_status = match three_axis {
                None => Status::Inconclusive,
                Some(s) if s.points == 0 => Status::Inconclusive,
                Some(s) => match s.max_positive_relative_error {
                    None => Status::Inconclusive,
                    Some(v) if v > gate.three_axis_max_positive_relative_error => Status::Fail,
                    Some(_) => Status::Pass,
                },
            };
            status = combine_status(status, stratum_status);
        }
        folds.push(MaterialFoldResult {
            fold: fold.clone(),
            training_source_rows,
            unscored_boundary_source_rows,
            mesh: interpolator.summary().clone(),
            points,
            covered_points,
            coverage_fraction,
            p95_relative_error,
            max_relative_error,
            max_positive_relative_error,
            strata,
            status,
        });
    }
    let interpolation_validation_status = if max_node_relative_error
        > benchmark.acceptance.max_node_relative_error
        || folds.iter().any(|f| f.status == Status::Fail)
    {
        Status::Fail
    } else if folds.iter().any(|f| f.status != Status::Pass) {
        Status::Inconclusive
    } else {
        Status::Pass
    };
    let implementation_sha256 = implementation_hash()?;
    let input_sha256 = hash(&serde_json::to_vec(&(
        dataset,
        benchmark,
        &implementation_sha256,
        method.id(),
        MATERIAL_CHECKER_ID,
    ))?);
    Ok(MaterialRunRecord {
        schema: "optcoil-material-validation/v3".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: method.id().into(),
        checker_id: MATERIAL_CHECKER_ID.into(),
        implementation_sha256,
        input_sha256,
        elapsed_ms: clock.elapsed().as_secs_f64() * 1000.0,
        benchmark: benchmark.clone(),
        dataset: dataset.clone(),
        full_mesh: model.summary().clone(),
        max_node_relative_error,
        folds,
        interpolation_validation_status,
        engineering_status: Status::NotEvaluated,
        limitations: limitations(),
    })
}

pub fn query_material(
    metadata_json: &str,
    csv_bytes: &[u8],
    query: MaterialQuery,
) -> Result<MaterialQueryRecord, RunError> {
    let dataset = MaterialDataset::from_csv(metadata_json, csv_bytes)?;
    query_material_dataset(dataset, query)
}

fn query_material_dataset(
    dataset: MaterialDataset,
    query: MaterialQuery,
) -> Result<MaterialQueryRecord, RunError> {
    query.validate()?;
    let model = IcInterpolator::with_method(
        &dataset.points,
        dataset.metadata.max_cell_spans,
        IcInterpolationMethod::LogFieldLogCurrent,
    )?;
    let criterion_matches = (query.electric_field_criterion_v_per_m
        / dataset.metadata.electric_field_criterion_v_per_m
        - 1.0)
        .abs()
        <= 1e-12;
    let estimate = if criterion_matches {
        model.evaluate(query.position())?
    } else {
        None
    };
    let domain_status = if estimate.is_some() {
        Status::Pass
    } else {
        Status::Inconclusive
    };
    let detail = if !criterion_matches {
        "Electric-field criterion differs from the source; automatic criterion conversion is not implemented."
    } else if estimate.is_none() {
        "Query lies outside supported measured cells, or in a coverage hole; no angle folding or extrapolation was applied."
    } else {
        "Nominal critical current per width under the source's applied-field, maximum-Lorentz measurement conditions. This is not an engineering current allowance."
    }.into();
    let implementation_sha256 = implementation_hash()?;
    let input_sha256 = hash(&serde_json::to_vec(&(
        &dataset,
        &query,
        &implementation_sha256,
        LOG_IC_MODEL_ID,
    ))?);
    Ok(MaterialQueryRecord {
        schema: "optcoil-material-query/v2".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: LOG_IC_MODEL_ID.into(),
        implementation_sha256,
        input_sha256,
        dataset,
        query,
        mesh: model.summary().clone(),
        estimate,
        domain_status,
        criterion_status: if criterion_matches {
            Status::Pass
        } else {
            Status::Inconclusive
        },
        engineering_status: Status::NotEvaluated,
        detail,
        limitations: limitations(),
    })
}

pub fn query_embedded_material(query: MaterialQuery) -> Result<MaterialQueryRecord, RunError> {
    query_material(SUPERPOWER_METADATA, SUPERPOWER_CSV, query)
}

/// As `query_embedded_material`, but against whichever embedded dataset
/// `id` names (see `MaterialDataset::embedded_by_id`); an unknown id is
/// rejected explicitly rather than silently falling back to the original.
pub fn query_embedded_material_by_id(
    id: &str,
    query: MaterialQuery,
) -> Result<MaterialQueryRecord, RunError> {
    query_material_dataset(MaterialDataset::embedded_by_id(id)?, query)
}

fn percentile95(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        None
    } else {
        Some(sorted[(0.95 * sorted.len() as f64).ceil() as usize - 1])
    }
}

/// Shared coverage/error summary for a whole fold or one of its strata:
/// `total` is the scored-point denominator (coverage_fraction is 0.0, not
/// NaN, when it is zero).
fn metrics_over<'a>(
    points: impl IntoIterator<Item = &'a MaterialValidationPoint> + Clone,
    total: usize,
) -> (usize, f64, Option<f64>, Option<f64>, Option<f64>) {
    let mut errors: Vec<f64> = points
        .clone()
        .into_iter()
        .filter_map(|p| p.relative_error)
        .collect();
    errors.sort_by(f64::total_cmp);
    let covered_points = errors.len();
    let coverage_fraction = if total == 0 {
        0.0
    } else {
        covered_points as f64 / total as f64
    };
    let p95_relative_error = percentile95(&errors);
    let max_relative_error = errors.last().copied();
    let max_positive_relative_error = (!errors.is_empty()).then(|| {
        points
            .into_iter()
            .filter_map(|p| p.signed_relative_error)
            .fold(0.0, f64::max)
    });
    (
        covered_points,
        coverage_fraction,
        p95_relative_error,
        max_relative_error,
        max_positive_relative_error,
    )
}

fn fold_metrics(
    points: &[MaterialValidationPoint],
) -> (usize, f64, Option<f64>, Option<f64>, Option<f64>) {
    metrics_over(points.iter(), points.len())
}

fn stratum_result(off_grid_axes: u8, points: &[&MaterialValidationPoint]) -> MaterialStratumResult {
    let (
        covered_points,
        coverage_fraction,
        p95_relative_error,
        max_relative_error,
        max_positive_relative_error,
    ) = metrics_over(points.iter().copied(), points.len());
    MaterialStratumResult {
        off_grid_axes,
        points: points.len(),
        covered_points,
        coverage_fraction,
        p95_relative_error,
        max_relative_error,
        max_positive_relative_error,
    }
}

/// FAIL beats INCONCLUSIVE beats PASS, matching contract A10's shared
/// fail-closed lattice: combines a fold's whole-fold status with its
/// three-axis stratum gate status.
fn combine_status(a: Status, b: Status) -> Status {
    if a == Status::Fail || b == Status::Fail {
        Status::Fail
    } else if a != Status::Pass || b != Status::Pass {
        Status::Inconclusive
    } else {
        Status::Pass
    }
}

fn assess_fold(
    coverage: f64,
    p95: Option<f64>,
    maximum: Option<f64>,
    positive: Option<f64>,
    limits: &MaterialAcceptance,
) -> Status {
    let (Some(p95), Some(maximum), Some(positive)) = (p95, maximum, positive) else {
        return Status::Inconclusive;
    };
    if coverage < limits.minimum_coverage_fraction {
        Status::Inconclusive
    } else if p95 > limits.max_p95_relative_error
        || maximum > limits.max_relative_error
        || positive > limits.max_positive_relative_error
    {
        Status::Fail
    } else {
        Status::Pass
    }
}

fn implementation_hash() -> Result<String, RunError> {
    Ok(hash(&serde_json::to_vec(&(
        include_str!("../../optcoil-model/src/material.rs"),
        include_str!("../../optcoil-physics/src/critical_current.rs"),
        include_str!("material.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?))
}

fn limitations() -> Vec<String> {
    vec![
        "One research specimen: a 1 mm patterned bridge, not a manufacturer batch guarantee or full-width tape qualification.".into(),
        "Measured applied-field response includes the specimen's self-field; no intrinsic local Jc(B) identification or coupling to the winding-pack field evaluator is established.".into(),
        "Oriented angle from the tape normal in maximum Lorentz-force geometry; no reflection symmetry, arbitrary azimuth or unsupported extrapolation.".into(),
        "Withheld-plane validation tests interpolation on this specimen. Observed errors are not a statistical confidence interval or guaranteed lower current bound.".into(),
        "Measurement uncertainty and strain state are not quantified by this dataset; engineering current margins, thermal, mechanical, quench and manufacturing acceptance remain NOT_EVALUATED.".into(),
    ]
}

#[cfg(test)]
mod tests {
    use optcoil_model::material::{
        AngleConvention, CellSpanLimits, HoldoutByAxis, MATERIAL_SCHEMA, MaterialDataClass,
        MaterialFieldBasis, MaterialMetadata, MaterialPoint, MaterialSelection, StratumAcceptance,
    };

    use super::*;

    /// Round to a fixed number of decimal places for a printed-precision
    /// comparison against docs/OC003.md, which reports percentages to 3
    /// decimal places (e.g. "5.404%").
    fn round_pct(fraction: f64, places: i32) -> f64 {
        let scale = 10f64.powi(places);
        (fraction * 100.0 * scale).round() / scale
    }

    #[test]
    fn v1_benchmarks_still_parse_and_reproduce_oc003_docs_table_precision() {
        // docs/OC003.md's table, reproduced to its own printed precision
        // (3 decimal places on each percentage), for every existing v1
        // benchmark file. This pins "unchanged to the printed precision"
        // independently of the status-only assertions in the other tests.
        let linear = run_oc003().unwrap();
        let expected_linear = [
            (5.404, 19.778, 10.474),
            (8.057, 15.868, 15.868),
            (10.037, 11.300, 11.300),
        ];
        for (fold, expected) in linear.folds.iter().zip(expected_linear) {
            assert_eq!(
                (
                    round_pct(fold.p95_relative_error.unwrap(), 3),
                    round_pct(fold.max_relative_error.unwrap(), 3),
                    round_pct(fold.max_positive_relative_error.unwrap(), 3),
                ),
                expected,
                "fold {}",
                fold.fold.id
            );
            assert!(fold.strata.is_empty());
            assert!(fold.points.iter().all(|p| p.off_grid_axes == 1));
        }

        let log = run_oc003_log().unwrap();
        let expected_log = [
            (3.237, 19.967, 5.776),
            (3.468, 9.393, 3.959),
            (3.114, 4.911, 4.911),
        ];
        for (fold, expected) in log.folds.iter().zip(expected_log) {
            assert_eq!(
                (
                    round_pct(fold.p95_relative_error.unwrap(), 3),
                    round_pct(fold.max_relative_error.unwrap(), 3),
                    round_pct(fold.max_positive_relative_error.unwrap(), 3),
                ),
                expected,
                "fold {}",
                fold.fold.id
            );
        }

        let challenge = run_oc003_challenge().unwrap();
        let fold = &challenge.folds[0];
        assert_eq!(
            (
                round_pct(fold.p95_relative_error.unwrap(), 3),
                round_pct(fold.max_relative_error.unwrap(), 3),
                round_pct(fold.max_positive_relative_error.unwrap(), 3),
            ),
            (3.172, 5.667, 5.667)
        );
    }

    #[test]
    fn frozen_whole_plane_holdouts_have_no_training_leakage() {
        let record = run_oc003().unwrap();
        for result in &record.folds {
            eprintln!(
                "{}: {} points, coverage {}, p95 {:?}, max {:?}, positive {:?}, status {:?}",
                result.fold.id,
                result.points.len(),
                result.coverage_fraction,
                result.p95_relative_error,
                result.max_relative_error,
                result.max_positive_relative_error,
                result.status
            );
            let training: HashSet<_> = result.training_source_rows.iter().copied().collect();
            for point in &result.points {
                assert!(!training.contains(&point.source_row));
                if let Some(prediction) = &point.prediction {
                    assert!(
                        prediction
                            .support
                            .iter()
                            .all(|p| training.contains(&p.source_row))
                    );
                }
            }
        }
        assert_eq!(
            record
                .folds
                .iter()
                .map(|f| f.points.len())
                .collect::<Vec<_>>(),
            [315, 410, 369]
        );
        assert_eq!(record.engineering_status, Status::NotEvaluated);
        // The frozen baseline actually misses the 10% field-plane P95 gate.
        // Preserve that failure rather than weakening the acceptance limit.
        assert_eq!(record.interpolation_validation_status, Status::Fail);
        assert!(record.folds[2].p95_relative_error.unwrap() > 0.1);
    }

    #[test]
    fn logarithmic_development_validation() {
        let record = run_oc003_log().unwrap();
        for f in &record.folds {
            eprintln!(
                "{}: coverage {}, p95 {:?}, max {:?}, positive {:?}, status {:?}",
                f.fold.id,
                f.coverage_fraction,
                f.p95_relative_error,
                f.max_relative_error,
                f.max_positive_relative_error,
                f.status
            );
        }
        assert_eq!(record.interpolation_validation_status, Status::Pass);
    }

    #[test]
    fn reserved_temperature_challenge_excludes_all_45k_training_rows() {
        // The method and limits were frozen before this challenge was evaluated.
        let record = run_oc003_challenge().unwrap();
        let fold = &record.folds[0];
        eprintln!(
            "reserved 45 K: coverage {}, p95 {:?}, max {:?}, positive {:?}, status {:?}",
            fold.coverage_fraction,
            fold.p95_relative_error,
            fold.max_relative_error,
            fold.max_positive_relative_error,
            fold.status
        );
        let training: HashSet<_> = fold.training_source_rows.iter().copied().collect();
        assert_eq!(fold.points.len(), 205);
        for p in &record.dataset.points {
            if p.nominal_temperature_k == 45.0 {
                assert!(!training.contains(&p.source_row));
            }
        }
        assert_eq!(record.interpolation_validation_status, Status::Pass);
        assert_eq!(record.engineering_status, Status::NotEvaluated);
    }

    #[test]
    fn queries_preserve_missing_coverage_and_criterion_mismatch() {
        let query = MaterialQuery {
            temperature_k: 30.0,
            applied_field_t: 5.0,
            angle_from_normal_deg: 90.0,
            electric_field_criterion_v_per_m: 1e-4,
        };
        let record = query_embedded_material(query).unwrap();
        assert_eq!(record.domain_status, Status::Pass);
        assert!(record.estimate.unwrap().ic_a_per_m > 0.0);
        assert_eq!(record.engineering_status, Status::NotEvaluated);
        for query in [
            MaterialQuery {
                applied_field_t: 12.0,
                ..query
            },
            MaterialQuery {
                angle_from_normal_deg: 270.0,
                ..query
            },
            MaterialQuery {
                electric_field_criterion_v_per_m: 1e-5,
                ..query
            },
        ] {
            let record = query_embedded_material(query).unwrap();
            assert_eq!(record.domain_status, Status::Inconclusive);
            assert!(record.estimate.is_none());
        }
    }

    #[test]
    fn query_embedded_material_by_id_selects_the_named_dataset_and_rejects_unknown_ids() {
        let original_query = MaterialQuery {
            temperature_k: 30.0,
            applied_field_t: 5.0,
            angle_from_normal_deg: 90.0,
            electric_field_criterion_v_per_m: 1e-4,
        };
        let record =
            query_embedded_material_by_id("robinson-superpower-ap-v3", original_query).unwrap();
        assert_eq!(record.dataset.metadata.id, "robinson-superpower-ap-v3");
        assert_eq!(record.domain_status, Status::Pass);

        // A point outside the original 1-8 T span but inside the low-field
        // extension is INCONCLUSIVE against the original dataset...
        let low_field_query = MaterialQuery {
            applied_field_t: 0.1,
            ..original_query
        };
        let against_original =
            query_embedded_material_by_id("robinson-superpower-ap-v3", low_field_query).unwrap();
        assert_eq!(against_original.domain_status, Status::Inconclusive);

        // ...but selecting the low-field dataset by id covers it instead.
        let against_lowfield =
            query_embedded_material_by_id("robinson-superpower-ap-v3-lowfield", low_field_query)
                .unwrap();
        assert_eq!(
            against_lowfield.dataset.metadata.id,
            "robinson-superpower-ap-v3-lowfield"
        );
        assert_eq!(against_lowfield.domain_status, Status::Pass);
        assert!(against_lowfield.estimate.unwrap().ic_a_per_m > 0.0);

        assert!(query_embedded_material_by_id("not-a-real-dataset", original_query).is_err());
    }

    #[test]
    fn failed_error_gates_and_missing_predictions_never_pass() {
        let limits = MaterialBenchmark::from_json(OC003_JSON).unwrap().acceptance;
        assert_eq!(
            assess_fold(1.0, Some(0.05), Some(0.25), Some(0.25), &limits),
            Status::Fail
        );
        assert_eq!(
            assess_fold(0.9, Some(0.01), Some(0.02), Some(0.02), &limits),
            Status::Inconclusive
        );
        assert_eq!(
            assess_fold(0.0, None, None, None, &limits),
            Status::Inconclusive
        );
        assert_eq!(
            percentile95(&(1..=20).map(f64::from).collect::<Vec<_>>()),
            Some(19.0)
        );
    }

    #[test]
    fn oc005_frozen_multi_axis_fold_matches_hand_derived_counts() {
        // Counts derived independently by hand from the CSV under the
        // declared rule (see docs/OC005 fold description / task record);
        // this pins the runner to that derivation rather than to whatever
        // the implementation happens to produce.
        let record = run_oc005().unwrap();
        assert_eq!(record.folds.len(), 1);
        let fold = &record.folds[0];
        assert_eq!(fold.training_source_rows.len(), 264);
        assert_eq!(fold.points.len(), 575);
        assert_eq!(fold.unscored_boundary_source_rows.len(), 666);
        assert_eq!(fold.coverage_fraction, 1.0);
        assert_eq!(fold.covered_points, 575);
        let strata: std::collections::BTreeMap<u8, (usize, usize)> = fold
            .strata
            .iter()
            .map(|s| (s.off_grid_axes, (s.points, s.covered_points)))
            .collect();
        assert_eq!(
            strata,
            std::collections::BTreeMap::from([(1, (182, 182)), (2, (267, 267)), (3, (126, 126))])
        );
        assert_eq!(
            fold.strata.iter().map(|s| s.points).sum::<usize>(),
            fold.points.len()
        );
        assert_eq!(
            fold.training_source_rows.len()
                + fold.points.len()
                + fold.unscored_boundary_source_rows.len(),
            record.dataset.points.len()
        );
    }

    #[test]
    fn oc005_multi_axis_predictions_never_leak_withheld_rows_into_support() {
        let record = run_oc005().unwrap();
        let fold = &record.folds[0];
        let training: HashSet<_> = fold.training_source_rows.iter().copied().collect();
        for point in &fold.points {
            assert!(!training.contains(&point.source_row));
            if let Some(prediction) = &point.prediction {
                assert!(
                    prediction
                        .support
                        .iter()
                        .all(|s| training.contains(&s.source_row))
                );
            }
        }
        assert_eq!(record.engineering_status, Status::NotEvaluated);
    }

    #[test]
    fn multi_axis_fold_rejected_under_v1_schema() {
        let mut benchmark = MaterialBenchmark::from_json(OC005_JSON).unwrap();
        benchmark.schema = "optcoil-material-benchmark/v1".into();
        assert!(benchmark.validate().is_err());
    }

    #[test]
    fn multi_axis_fold_rejects_holdout_values_alongside_holdout_by_axis() {
        let mut benchmark = MaterialBenchmark::from_json(OC005_JSON).unwrap();
        benchmark.folds[0].holdout_values = vec![1.0];
        assert!(benchmark.validate().is_err());
    }

    #[test]
    fn stratum_acceptance_rejected_without_any_multi_axis_fold() {
        let mut benchmark = MaterialBenchmark::from_json(include_str!(
            "../../../benchmarks/measured/oc-003-log.json"
        ))
        .unwrap();
        benchmark.stratum_acceptance = Some(StratumAcceptance {
            three_axis_max_positive_relative_error: 0.05,
        });
        assert!(benchmark.validate().is_err());
    }

    #[test]
    fn stratum_acceptance_gate_rejected_above_max_positive_relative_error() {
        let mut benchmark = MaterialBenchmark::from_json(OC005_JSON).unwrap();
        benchmark.stratum_acceptance = Some(StratumAcceptance {
            three_axis_max_positive_relative_error: benchmark
                .acceptance
                .max_positive_relative_error
                + 0.01,
        });
        assert!(benchmark.validate().is_err());
    }

    /// A tiny 3x3x3 synthetic grid (temperature, field, angle) whose
    /// `ln(Ic/w)` is an exact affine function of (T, ln B, angle), so the
    /// declared log-field-log-current method reproduces it exactly at every
    /// point except one deliberately corrupted node. Coordinates equal
    /// nominal values exactly, so every point lands precisely on its own
    /// nominal grid node.
    fn synthetic_multi_axis_dataset() -> MaterialDataset {
        let temperatures = [10.0, 20.0, 30.0];
        let fields = [1.0, 2.0, 4.0];
        let angles = [0.0, 60.0, 120.0];
        let hash64 = "0".repeat(64);
        let metadata = MaterialMetadata {
            schema: MATERIAL_SCHEMA.into(),
            id: "synthetic-multi-axis-fixture".into(),
            data_class: MaterialDataClass::Measured,
            material: "synthetic".into(),
            sample_id: "synthetic-sample".into(),
            source_doi: "synthetic:fixture".into(),
            source_url: "https://example.invalid/synthetic".into(),
            authors: vec!["OptCoil test fixture".into()],
            license: "not applicable".into(),
            license_url: "https://example.invalid/license".into(),
            measurement_dates: "not applicable".into(),
            source_xlsx_sha256: hash64.clone(),
            csv_sha256: hash64.clone(),
            preparation_source_sha256: hash64.clone(),
            source_description_sha256: hash64,
            electric_field_criterion_v_per_m: 1e-4,
            voltage_tap_spacing_m: 0.005,
            measured_bridge_width_m: 0.001,
            original_tape_width_m: 0.012,
            field_basis: MaterialFieldBasis::AppliedFieldIncludingSampleSelfFieldResponse,
            angle_convention: AngleConvention::OrientedFromTapeNormalInMaximumLorentzGeometry,
            coordinate_policy: "synthetic: measured coordinates equal nominal coordinates".into(),
            normalization: "not applicable".into(),
            measurement_uncertainty_fraction: None,
            strain_state: "not applicable (synthetic fixture)".into(),
            selection: MaterialSelection {
                nominal_temperature_k: temperatures.to_vec(),
                nominal_field_t: fields.to_vec(),
                nominal_angle_range_deg: [0.0, 120.0],
            },
            max_cell_spans: CellSpanLimits {
                temperature_k: 20.0,
                field_ratio: 4.0,
                angle_deg: 120.0,
            },
            point_count: temperatures.len() * fields.len() * angles.len(),
            limitations: vec![
                "Synthetic fixture for stratum-gate unit tests; not measured data.".into(),
            ],
            tabular_import: None,
        };
        let mut points = Vec::new();
        let mut source_row = 1u32;
        for &t in &temperatures {
            for &b in &fields {
                for &a in &angles {
                    let ic_a_per_m = (0.01 * t + 0.5 * b.ln() + 0.001 * a + 5.0).exp();
                    points.push(MaterialPoint {
                        source_row,
                        nominal_temperature_k: t,
                        nominal_field_t: b,
                        nominal_angle_deg: a,
                        temperature_k: t,
                        applied_field_t: b,
                        angle_from_normal_deg: a,
                        ic_a_per_m,
                        bridge_ic_a: ic_a_per_m * 0.001,
                        n_value: 20.0,
                    });
                    source_row += 1;
                }
            }
        }
        let dataset = MaterialDataset { metadata, points };
        dataset.validate().unwrap();
        dataset
    }

    fn synthetic_multi_axis_benchmark(
        holdout_by_axis: HoldoutByAxis,
        dataset_id: &str,
    ) -> MaterialBenchmark {
        let benchmark = MaterialBenchmark {
            schema: "optcoil-material-benchmark/v2".into(),
            id: "synthetic-multi-axis-gate-test".into(),
            description: "Synthetic fixture exercising the three-axis stratum gate.".into(),
            dataset_id: dataset_id.into(),
            method: LOG_IC_MODEL_ID.into(),
            folds: vec![MaterialFold {
                id: "multi_axis_synthetic".into(),
                axis: ValidationAxis::Multi,
                holdout_values: Vec::new(),
                holdout_by_axis: Some(holdout_by_axis),
            }],
            holdout_policy: "synthetic fixture".into(),
            // Deliberately loose whole-fold gates: these tests isolate the
            // stricter, separate stratum_acceptance gate.
            acceptance: MaterialAcceptance {
                minimum_coverage_fraction: 1.0,
                max_p95_relative_error: 1.0,
                max_relative_error: 1.0,
                max_positive_relative_error: 1.0,
                max_node_relative_error: 1.0,
            },
            stratum_acceptance: Some(StratumAcceptance {
                three_axis_max_positive_relative_error: 0.05,
            }),
        };
        benchmark.validate().unwrap();
        benchmark
    }

    #[test]
    fn synthetic_three_axis_stratum_gate_fails_on_planted_overprediction() {
        let mut dataset = synthetic_multi_axis_dataset();
        let target = dataset
            .points
            .iter_mut()
            .find(|p| {
                p.nominal_temperature_k == 20.0
                    && p.nominal_field_t == 2.0
                    && p.nominal_angle_deg == 60.0
            })
            .unwrap();
        // Plant a violation: halve the triple-held point's own measured
        // value. Training never sees this point, so the log-linear fit
        // still predicts the untouched model value there, which now reads
        // as a ~100% overprediction against the corrupted "measurement".
        target.ic_a_per_m /= 2.0;
        dataset.validate().unwrap();
        let benchmark = synthetic_multi_axis_benchmark(
            HoldoutByAxis {
                temperature: vec![20.0],
                field: vec![2.0],
                angle: vec![60.0],
            },
            &dataset.metadata.id,
        );
        let record = run_material_benchmark(&dataset, &benchmark).unwrap();
        let fold = &record.folds[0];
        let three_axis = fold.strata.iter().find(|s| s.off_grid_axes == 3).unwrap();
        assert_eq!(three_axis.points, 1);
        assert!(three_axis.max_positive_relative_error.unwrap() > 0.05);
        assert_eq!(fold.status, Status::Fail);
        assert_eq!(record.interpolation_validation_status, Status::Fail);
    }

    #[test]
    fn synthetic_three_axis_stratum_gate_is_inconclusive_when_stratum_is_empty() {
        let dataset = synthetic_multi_axis_dataset();
        // Angle is never withheld, so no point can ever have all three
        // coordinates on a withheld plane: the three-axis stratum is
        // structurally empty regardless of the (exact, unmodified) errors
        // on the one- and two-axis strata.
        let benchmark = synthetic_multi_axis_benchmark(
            HoldoutByAxis {
                temperature: vec![20.0],
                field: vec![2.0],
                angle: Vec::new(),
            },
            &dataset.metadata.id,
        );
        let record = run_material_benchmark(&dataset, &benchmark).unwrap();
        let fold = &record.folds[0];
        let three_axis = fold.strata.iter().find(|s| s.off_grid_axes == 3).unwrap();
        assert_eq!(three_axis.points, 0);
        assert_eq!(fold.status, Status::Inconclusive);
        assert_eq!(record.interpolation_validation_status, Status::Inconclusive);
    }

    /// Regression test for a cross-fold contamination bug: `stratum_acceptance`
    /// must only ever gate the multi-axis fold(s) it is declared against, never
    /// an unrelated single-axis fold sharing the same benchmark file. Before
    /// the fix, the stratum-gate block ran for every fold regardless of
    /// `is_multi`; a single-axis fold always has an empty `strata` list, so
    /// `three_axis` was always `None` and `combine_status` unconditionally
    /// demoted the single-axis fold's status to `Inconclusive` whenever
    /// `stratum_acceptance` was `Some(_)` anywhere in the file - independent of
    /// what the multi-axis fold's own stratum verdict actually was.
    #[test]
    fn single_axis_fold_status_is_unaffected_by_an_unrelated_multi_axis_fold_and_stratum_gate() {
        let dataset = synthetic_multi_axis_dataset();
        let loose = MaterialAcceptance {
            minimum_coverage_fraction: 1.0,
            max_p95_relative_error: 1.0,
            max_relative_error: 1.0,
            max_positive_relative_error: 1.0,
            max_node_relative_error: 1.0,
        };
        let single_axis_fold = MaterialFold {
            id: "temperature_only".into(),
            axis: ValidationAxis::Temperature,
            holdout_values: vec![20.0],
            holdout_by_axis: None,
        };

        // Baseline: the single-axis fold run completely alone, no
        // `stratum_acceptance` in play at all.
        let solo_benchmark = MaterialBenchmark {
            schema: "optcoil-material-benchmark/v2".into(),
            id: "solo-single-axis".into(),
            description: "single-axis fold run alone".into(),
            dataset_id: dataset.metadata.id.clone(),
            method: LOG_IC_MODEL_ID.into(),
            folds: vec![single_axis_fold.clone()],
            holdout_policy: "synthetic fixture".into(),
            acceptance: loose.clone(),
            stratum_acceptance: None,
        };
        solo_benchmark.validate().unwrap();
        let solo_record = run_material_benchmark(&dataset, &solo_benchmark).unwrap();
        let solo_fold = &solo_record.folds[0];
        assert_eq!(solo_fold.status, Status::Pass);

        // Case 1: paired with an unrelated multi-axis fold whose own
        // three-axis stratum comfortably PASSES its stratum gate.
        let passing_multi_fold = MaterialFold {
            id: "multi_axis_synthetic".into(),
            axis: ValidationAxis::Multi,
            holdout_values: Vec::new(),
            holdout_by_axis: Some(HoldoutByAxis {
                temperature: vec![20.0],
                field: vec![2.0],
                angle: vec![60.0],
            }),
        };
        // Case 2: paired with an unrelated multi-axis fold whose three-axis
        // stratum is structurally EMPTY (angle never withheld), so that
        // fold's own stratum status is INCONCLUSIVE, not PASS.
        let inconclusive_multi_fold = MaterialFold {
            id: "multi_axis_synthetic_no_angle_holdout".into(),
            axis: ValidationAxis::Multi,
            holdout_values: Vec::new(),
            holdout_by_axis: Some(HoldoutByAxis {
                temperature: vec![20.0],
                field: vec![2.0],
                angle: Vec::new(),
            }),
        };

        for (label, multi_fold, expected_multi_status) in [
            (
                "multi-axis fold PASSes its stratum gate",
                passing_multi_fold,
                Status::Pass,
            ),
            (
                "multi-axis fold is INCONCLUSIVE (empty three-axis stratum)",
                inconclusive_multi_fold,
                Status::Inconclusive,
            ),
        ] {
            let combined_benchmark = MaterialBenchmark {
                schema: "optcoil-material-benchmark/v2".into(),
                id: "combined-single-and-multi".into(),
                description:
                    "single-axis fold sharing a benchmark file with an unrelated multi-axis fold"
                        .into(),
                dataset_id: dataset.metadata.id.clone(),
                method: LOG_IC_MODEL_ID.into(),
                folds: vec![single_axis_fold.clone(), multi_fold],
                holdout_policy: "synthetic fixture".into(),
                acceptance: loose.clone(),
                stratum_acceptance: Some(StratumAcceptance {
                    three_axis_max_positive_relative_error: 0.05,
                }),
            };
            combined_benchmark.validate().unwrap();
            let record = run_material_benchmark(&dataset, &combined_benchmark).unwrap();
            let combined_single = &record.folds[0];
            let combined_multi = &record.folds[1];
            assert_eq!(
                combined_multi.status, expected_multi_status,
                "{label}: multi-axis fold's own status"
            );
            assert_eq!(
                combined_single.status,
                Status::Pass,
                "{label}: single-axis fold's status must stay Pass, unaffected by the \
                 unrelated multi-axis fold's stratum verdict"
            );
            assert_eq!(
                (
                    combined_single.p95_relative_error,
                    combined_single.max_relative_error,
                    combined_single.max_positive_relative_error,
                ),
                (
                    solo_fold.p95_relative_error,
                    solo_fold.max_relative_error,
                    solo_fold.max_positive_relative_error,
                ),
                "{label}: single-axis fold's own error metrics must be identical to its solo run"
            );
            assert!(combined_single.strata.is_empty());
        }
    }
}
