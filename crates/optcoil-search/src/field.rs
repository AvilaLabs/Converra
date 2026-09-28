//! Headless, reproducible field calculation and independent-reference comparison.

use std::{
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use optcoil_model::{
    Check, Status,
    magnetics::{FieldCase, OC002_JSON},
};
use optcoil_physics::racetrack::{FIELD_MODEL_ID, MU0_H_PER_M, RacetrackEvaluator, norm, sub};
use serde::{Deserialize, Serialize};

use crate::{RunError, hash, write_json_new};

pub const FIELD_CHECKER_ID: &str = "fixed-probe-vector-error-and-refinement/v1";
pub const OC002_REFERENCE_JSON: &str =
    include_str!("../../../benchmarks/synthetic/oc-002.reference.json");
const REFERENCE_SOURCE: &str = include_str!("../../../tools/reference_oc002.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldOptions {
    /// Three strictly increasing orders; the last two define refinement change.
    pub orders: [u32; 3],
}

impl Default for FieldOptions {
    fn default() -> Self {
        Self {
            orders: [6, 10, 14],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldReference {
    pub schema: String,
    /// SHA-256 of the exact input JSON bytes; even formatting edits invalidate it.
    pub case_sha256: String,
    pub method: String,
    pub source_sha256: String,
    pub source_path: String,
    pub python_version: String,
    pub numpy_version: String,
    pub platform: String,
    pub orders: [u32; 3],
    pub component_subdivisions: u32,
    pub mu0_h_per_m: f64,
    pub probes: Vec<ReferenceProbe>,
    pub analytic_audits: Vec<AnalyticAudit>,
    pub elapsed_seconds: f64,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceProbe {
    pub id: String,
    pub position_m: [f64; 3],
    pub fields_t: [[f64; 3]; 3],
    pub refinement_changes_t: [f64; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticAudit {
    pub z_m: f64,
    pub expected_bz_t: f64,
    pub computed_field_t: [f64; 3],
    pub vector_error_t: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldLevel {
    pub order: u32,
    pub field_t: [f64; 3],
    pub kernel_evaluations: u64,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldProbeResult {
    pub id: String,
    pub position_m: [f64; 3],
    pub levels: Vec<FieldLevel>,
    pub refinement_changes_t: [f64; 2],
    pub reference_field_t: Option<[f64; 3]>,
    pub reference_error_t: Option<f64>,
    pub reference_error_fraction: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldRunRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub checker_id: String,
    pub case_sha256: String,
    pub reference_sha256: Option<String>,
    /// Relevant model, kernel and runner sources plus lockfile, bundled at build.
    /// An identity for reproducibility, not an attestation or proof of accuracy.
    pub implementation_sha256: String,
    pub input_sha256: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    pub case: FieldCase,
    pub options: FieldOptions,
    pub source_cells: usize,
    pub kernel_evaluations: u64,
    pub results: Vec<FieldProbeResult>,
    pub reference: Option<FieldReference>,
    pub max_reference_error_t: Option<f64>,
    pub max_reference_error_fraction: Option<f64>,
    pub max_refinement_change_t: f64,
    pub max_refinement_change_fraction: f64,
    pub max_reference_refinement_t: Option<f64>,
    pub numerical_status: Status,
    pub engineering_status: Status,
    pub checks: Vec<Check>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub os: String,
    pub architecture: String,
    pub cpu_model: Option<String>,
    pub available_parallelism: Option<usize>,
    pub debug_assertions: bool,
    pub execution_threads: u32,
}

impl FieldRunRecord {
    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_json_new(self, path)
    }
}

pub fn run_oc002(options: &FieldOptions) -> Result<FieldRunRecord, RunError> {
    run_field_case(OC002_JSON, Some(OC002_REFERENCE_JSON), options)
}

/// Missing references and unresolved refinement produce INCONCLUSIVE. A field
/// benchmark never changes engineering acceptance, which is NOT_EVALUATED.
pub fn run_field_case(
    case_json: &str,
    reference_json: Option<&str>,
    options: &FieldOptions,
) -> Result<FieldRunRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;
    let case = FieldCase::from_json(case_json)?;
    if options.orders.iter().any(|n| !(2..=24).contains(n))
        || options.orders.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(RunError::Invalid(
            "field evaluation requires three increasing quadrature orders in 2..=24".into(),
        ));
    }
    let case_sha256 = hash(case_json.as_bytes());
    let reference: Option<FieldReference> = reference_json.map(serde_json::from_str).transpose()?;
    if let Some(reference) = &reference {
        validate_reference(reference, &case, &case_sha256)?;
    }
    let solvers = options
        .orders
        .iter()
        .map(|&n| RacetrackEvaluator::new(&case.geometry, n))
        .collect::<Result<Vec<_>, _>>()?;
    let mut results = Vec::with_capacity(case.probes.len());
    for (index, probe) in case.probes.iter().enumerate() {
        let mut levels = Vec::new();
        for (&order, solver) in options.orders.iter().zip(&solvers) {
            let clock = Instant::now();
            let value = solver.evaluate(probe.position_m)?;
            levels.push(FieldLevel {
                order,
                field_t: value.field_t,
                kernel_evaluations: value.kernel_evaluations,
                elapsed_ms: clock.elapsed().as_secs_f64() * 1000.0,
            });
        }
        let refinement_changes_t = [
            norm(sub(levels[1].field_t, levels[0].field_t)),
            norm(sub(levels[2].field_t, levels[1].field_t)),
        ];
        let reference_field_t = reference.as_ref().map(|r| r.probes[index].fields_t[2]);
        let reference_error_t =
            reference_field_t.map(|target| norm(sub(levels[2].field_t, target)));
        results.push(FieldProbeResult {
            id: probe.id.clone(),
            position_m: probe.position_m,
            levels,
            refinement_changes_t,
            reference_field_t,
            reference_error_t,
            reference_error_fraction: reference_error_t.map(|e| e / case.acceptance.field_scale_t),
        });
    }
    let scale = case.acceptance.field_scale_t;
    let max_refinement_change_t = results
        .iter()
        .map(|p| p.refinement_changes_t[1])
        .fold(0.0, f64::max);
    let max_reference_error_t = reference.as_ref().map(|_| {
        results
            .iter()
            .filter_map(|p| p.reference_error_t)
            .fold(0.0, f64::max)
    });
    let max_reference_refinement_t = reference.as_ref().map(|r| {
        r.probes
            .iter()
            .map(|p| norm(sub(p.fields_t[2], p.fields_t[1])))
            .fold(0.0, f64::max)
    });
    if [
        Some(max_refinement_change_t),
        max_reference_error_t,
        max_reference_refinement_t,
    ]
    .into_iter()
    .flatten()
    .any(|value| !(value / scale).is_finite())
    {
        return Err(RunError::Invalid(
            "field scale produces nonfinite normalized errors".into(),
        ));
    }
    let (numerical_status, checks) = check_metrics(
        &case,
        max_refinement_change_t,
        max_reference_error_t,
        max_reference_refinement_t,
    );
    let reference_sha256 = reference_json.map(|json| hash(json.as_bytes()));
    let implementation_sha256 = hash(&serde_json::to_vec(&(
        include_str!("../../optcoil-model/src/lib.rs"),
        include_str!("../../optcoil-model/src/magnetics.rs"),
        include_str!("../../optcoil-physics/src/racetrack.rs"),
        include_str!("lib.rs"),
        include_str!("field.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?);
    let input_sha256 = hash(&serde_json::to_vec(&(
        &case_sha256,
        &reference_sha256,
        &implementation_sha256,
        options,
        FIELD_MODEL_ID,
        FIELD_CHECKER_ID,
        env!("CARGO_PKG_VERSION"),
    ))?);
    let kernel_evaluations = results
        .iter()
        .flat_map(|p| &p.levels)
        .map(|l| l.kernel_evaluations)
        .sum();
    Ok(FieldRunRecord {
        schema: "optcoil-field-run/v1".into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: FIELD_MODEL_ID.into(), checker_id: FIELD_CHECKER_ID.into(),
        case_sha256, reference_sha256, implementation_sha256, input_sha256,
        started_unix_ms, elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        runtime: runtime_info(), source_cells: solvers[0].cell_count(), kernel_evaluations,
        case, options: options.clone(), results, reference,
        max_reference_error_t, max_reference_error_fraction: max_reference_error_t.map(|e| e / scale),
        max_refinement_change_t, max_refinement_change_fraction: max_refinement_change_t / scale,
        max_reference_refinement_t, numerical_status, engineering_status: Status::NotEvaluated, checks,
        limitations: vec![
            "Uniform prescribed tangential winding-pack current, canonical XY racetrack, conventional vacuum mu0 = 4*pi*1e-7 H/m.".into(),
            "Probe fields are point values, not tape-volume averages, a guaranteed peak field, or HTS operating margins.".into(),
            "Quadrature refinement is an observed change, not a certified error bound. Accuracy claims cover only the specified probes and comparison model.".into(),
            "Reference is an independently implemented analytical-z/2D quadrature calculation, not experimental or commercial FEM validation.".into(),
            "HTS current redistribution, material response, mechanics, thermal behavior, quench and manufacturing/cost optimization are NOT_EVALUATED here.".into(),
            "This benchmark still uses prescribed field inputs; it is not yet coupled into allocation search or the desktop workflow.".into(),
        ],
    })
}

fn validate_reference(
    reference: &FieldReference,
    case: &FieldCase,
    case_sha256: &str,
) -> Result<(), RunError> {
    let invalid = |s: &str| RunError::Invalid(format!("field reference: {s}"));
    if reference.schema != "optcoil-field-reference/v1"
        || reference.method != "analytical-z-integral-planar-duffy-gauss/v1"
        || reference.orders != [24, 48, 72]
        || reference.component_subdivisions != 16
        || (reference.mu0_h_per_m - MU0_H_PER_M).abs() > 1e-20
    {
        return Err(invalid(
            "unsupported schema, method, settings or permeability",
        ));
    }
    if reference.case_sha256 != case_sha256 {
        return Err(invalid(
            "case SHA-256 does not match the exact input JSON; regenerate the reference",
        ));
    }
    if reference.source_sha256 != hash(REFERENCE_SOURCE.as_bytes()) {
        return Err(invalid(
            "source SHA-256 does not match the bundled independent reference implementation",
        ));
    }
    if reference.probes.len() != case.probes.len() {
        return Err(invalid("probe count mismatch"));
    }
    for (actual, expected) in reference.probes.iter().zip(&case.probes) {
        if actual.id != expected.id || actual.position_m != expected.position_m {
            return Err(invalid("probe identity, ordering or coordinates mismatch"));
        }
        if actual.fields_t.iter().flatten().any(|x| !x.is_finite()) {
            return Err(invalid("nonfinite field value"));
        }
        for i in 0..2 {
            let change = norm(sub(actual.fields_t[i + 1], actual.fields_t[i]));
            if !actual.refinement_changes_t[i].is_finite()
                || actual.refinement_changes_t[i] < 0.0
                || (change - actual.refinement_changes_t[i]).abs()
                    > 1e-12 * case.acceptance.field_scale_t
            {
                return Err(invalid(
                    "refinement summary disagrees with stored field vectors",
                ));
            }
        }
    }
    Ok(())
}

fn check_metrics(
    case: &FieldCase,
    change_t: f64,
    error_t: Option<f64>,
    reference_change_t: Option<f64>,
) -> (Status, Vec<Check>) {
    let a = &case.acceptance;
    let convergence = if change_t / a.field_scale_t <= a.max_refinement_change_fraction {
        Status::Pass
    } else {
        Status::Inconclusive
    };
    let reference_convergence = match reference_change_t {
        Some(value) if value / a.field_scale_t <= a.max_reference_refinement_fraction => {
            Status::Pass
        }
        Some(_) => Status::Inconclusive,
        None => Status::NotEvaluated,
    };
    let comparison = match error_t {
        None => Status::NotEvaluated,
        Some(_) if convergence != Status::Pass || reference_convergence != Status::Pass => {
            Status::Inconclusive
        }
        Some(error) if error / a.field_scale_t <= a.max_reference_error_fraction => Status::Pass,
        Some(_) => Status::Fail,
    };
    let numerical = match comparison {
        Status::Pass | Status::Fail => comparison,
        Status::Inconclusive | Status::NotEvaluated => Status::Inconclusive,
    };
    (
        numerical,
        vec![
            Check {
                id: "quadrature_refinement".into(),
                status: convergence,
                detail: format!(
                    "Maximum last-two-order vector change {change_t:.9e} T; limit {:.9e} T",
                    a.field_scale_t * a.max_refinement_change_fraction
                ),
            },
            Check {
                id: "reference_refinement".into(),
                status: reference_convergence,
                detail: format!(
                    "Recomputed reference last-two-order change {reference_change_t:?} T; limit {:.9e} T",
                    a.field_scale_t * a.max_reference_refinement_fraction
                ),
            },
            Check {
                id: "independent_field_comparison".into(),
                status: comparison,
                detail: format!(
                    "Maximum vector discrepancy {error_t:?} T; limit {:.9e} T; comparison requires both refinement checks to pass",
                    a.field_scale_t * a.max_reference_error_fraction
                ),
            },
        ],
    )
}

pub(crate) fn runtime_info() -> RuntimeInfo {
    let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(_, value)| value.trim().to_owned())
            })
        });
    RuntimeInfo {
        os: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
        cpu_model,
        available_parallelism: std::thread::available_parallelism().ok().map(usize::from),
        debug_assertions: cfg!(debug_assertions),
        execution_threads: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_and_stale_reference_evidence_is_rejected_before_solving() {
        let case = FieldCase::from_json(OC002_JSON).unwrap();
        let reference: FieldReference = serde_json::from_str(OC002_REFERENCE_JSON).unwrap();
        let identity = hash(OC002_JSON.as_bytes());
        validate_reference(&reference, &case, &identity).unwrap();
        assert!(validate_reference(&reference, &case, "wrong-case").is_err());
        let mut bad = reference.clone();
        bad.probes[0].position_m[0] = 1.0;
        assert!(validate_reference(&bad, &case, &identity).is_err());
        let mut bad = reference.clone();
        bad.probes.pop();
        assert!(validate_reference(&bad, &case, &identity).is_err());
        let mut bad = reference.clone();
        bad.source_sha256 = "stale-source".into();
        assert!(validate_reference(&bad, &case, &identity).is_err());
        let mut bad = reference;
        bad.probes[0].refinement_changes_t[1] = 0.01;
        assert!(validate_reference(&bad, &case, &identity).is_err());
    }

    #[test]
    fn unresolved_checks_cannot_produce_a_numerical_pass() {
        let case = FieldCase::from_json(OC002_JSON).unwrap();
        assert_eq!(
            check_metrics(&case, 1e-8, Some(1e-8), Some(1e-8)).0,
            Status::Pass
        );
        assert_eq!(
            check_metrics(&case, 1e-8, Some(0.01), Some(1e-8)).0,
            Status::Fail
        );
        assert_eq!(
            check_metrics(&case, 0.01, Some(1e-8), Some(1e-8)).0,
            Status::Inconclusive
        );
        assert_eq!(
            check_metrics(&case, 1e-8, Some(1e-8), Some(0.01)).0,
            Status::Inconclusive
        );
        assert_eq!(
            check_metrics(&case, 1e-8, None, None).0,
            Status::Inconclusive
        );
    }

    #[test]
    fn custom_geometry_runs_without_borrowing_the_frozen_reference() {
        let mut case = FieldCase::from_json(OC002_JSON).unwrap();
        case.probes.truncate(1);
        let options = FieldOptions { orders: [2, 3, 4] };
        let first = run_field_case(&serde_json::to_string(&case).unwrap(), None, &options).unwrap();
        assert_eq!(first.numerical_status, Status::Inconclusive);
        assert_eq!(first.engineering_status, Status::NotEvaluated);
        assert!(first.max_reference_error_t.is_none());
        case.geometry.bend_radius_m *= 1.25;
        let second =
            run_field_case(&serde_json::to_string(&case).unwrap(), None, &options).unwrap();
        assert_ne!(first.input_sha256, second.input_sha256);
        assert!(
            norm(sub(
                first.results[0].levels[2].field_t,
                second.results[0].levels[2].field_t
            )) > 0.01
        );
        case.acceptance.field_scale_t = 1e-320;
        assert!(run_field_case(&serde_json::to_string(&case).unwrap(), None, &options).is_err());
        assert!(run_field_case(OC002_JSON, None, &FieldOptions { orders: [4, 4, 6] }).is_err());
    }
}
