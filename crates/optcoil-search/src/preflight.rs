//! Shared, headless readiness summary for coupled search studies.
//!
//! Preflight reports declared inputs and obvious dependency mismatches. It
//! does not evaluate field coverage, run screening, or establish engineering
//! acceptance; the coupled runner remains authoritative for those checks.

use std::collections::BTreeMap;

use optcoil_model::{coupled_search::CoupledSearchCase, material::MaterialDataset};
use serde::{Deserialize, Serialize};

use crate::coupled_search::CoupledSearchOptions;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PreflightOptions {
    /// Runtime thread override. The engine permits this only to lower the
    /// case-declared maximum; preflight applies that same rule.
    pub threads: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightLevel {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightItem {
    pub level: PreflightLevel,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetPreflight {
    pub binding_id: String,
    pub dataset_id: String,
    pub expected_csv_sha256: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_csv_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature_range_k: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_criterion_v_per_m: Option<f64>,
    pub temperature_compatible: Option<bool>,
    pub criterion_compatible: Option<bool>,
    pub identity_compatible: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadEstimate {
    pub candidate_count: usize,
    /// Candidate search points before refinement. This is an upper estimate
    /// from declared stations, sampled turns/tapes, and width quadrature.
    pub primary_point_upper_estimate: u128,
    /// Sum of q³ across the two declared quadrature orders times the primary
    /// point estimate. This is a relative kernel-work proxy, not elapsed time.
    pub primary_kernel_work_proxy: u128,
    pub refined_point_upper_estimate: u128,
    pub refined_kernel_work_proxy: u128,
    pub estimate_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyPreflight {
    pub schema: String,
    pub case_id: String,
    pub geometry_support: String,
    pub field_source: String,
    pub datasets: Vec<DatasetPreflight>,
    pub temperature_k: f64,
    pub electric_field_criterion_v_per_m: f64,
    pub width_transfer: String,
    pub price_basis: Vec<String>,
    pub declared_screens: Vec<String>,
    pub missing_screens: Vec<String>,
    pub quadrature_orders: [u32; 2],
    pub refinement_change_limit_fraction: f64,
    pub declared_threads: u32,
    pub selected_threads: u32,
    pub workload: WorkloadEstimate,
    pub errors: Vec<PreflightItem>,
    pub warnings: Vec<PreflightItem>,
    pub items: Vec<PreflightItem>,
    pub ready_to_run: bool,
    /// Always false: readiness is input/dependency checking, not a result.
    pub engineering_acceptance_claim: bool,
}

/// Build a study preflight for an already parsed coupled search case.
///
/// Supplied datasets are keyed by their actual metadata id. Embedded
/// datasets resolve automatically; an unresolved external id remains an
/// actionable missing dependency. Dataset identity and domain decisions
/// use `CoupledSearchCase::validate_against_dataset_map`, the same checks
/// used by the runner. This function never queries field coverage.
pub fn preflight_coupled_search(
    case: &CoupledSearchCase,
    resolved_datasets: Option<&BTreeMap<String, MaterialDataset>>,
    options: &PreflightOptions,
) -> StudyPreflight {
    let mut items = Vec::new();
    if let Err(error) = case.validate() {
        items.push(error_item(
            "invalid_case",
            format!("Case validation failed: {error}"),
            "Correct the case using the coupled search schema and rerun preflight.".into(),
        ));
    }

    let declared_threads = case.execution.max_threads;
    let selected_threads = match options.threads {
        None => declared_threads,
        Some(0) => {
            items.push(error_item(
                "invalid_threads",
                "Requested thread count is zero.".into(),
                "Set threads to 1 or more, up to the case's declared maximum.".into(),
            ));
            declared_threads
        }
        Some(n) if n > declared_threads => {
            items.push(error_item(
                "threads_exceed_case_limit",
                format!("Requested {n} threads exceeds the case maximum of {declared_threads}."),
                format!("Use at most {declared_threads} threads or update the case's execution.max_threads."),
            ));
            declared_threads
        }
        Some(n) => n,
    };

    let mut datasets = Vec::new();
    let mut dataset_map = BTreeMap::new();
    if let Some(supplied) = resolved_datasets {
        for (id, dataset) in supplied {
            if id != &dataset.metadata.id {
                items.push(error_item(
                    "dataset_map_key_mismatch",
                    format!(
                        "Resolved dataset map key '{id}' does not match dataset identity '{}'.",
                        dataset.metadata.id
                    ),
                    "Key each resolved dataset by its metadata.id.".into(),
                ));
            }
            dataset_map.insert(dataset.metadata.id.clone(), dataset.clone());
        }
    }

    for (binding_id, settings) in case.material_bindings() {
        let dataset = dataset_map
            .get(&settings.dataset_id)
            .cloned()
            .or_else(|| MaterialDataset::embedded_by_id(&settings.dataset_id).ok());
        if let Some(ds) = &dataset {
            let expected = settings.csv_sha256 == ds.metadata.csv_sha256;
            let identity = settings.dataset_id == ds.metadata.id && expected;
            let criterion = (case.operating.electric_field_criterion_v_per_m
                - ds.metadata.electric_field_criterion_v_per_m)
                .abs()
                <= 1e-12 * ds.metadata.electric_field_criterion_v_per_m.max(1.0);
            let ts = &ds.metadata.selection.nominal_temperature_k;
            let t_min = ts.iter().copied().fold(f64::INFINITY, f64::min);
            let t_max = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let temperature =
                case.operating.temperature_k > t_min && case.operating.temperature_k < t_max;
            if !identity {
                items.push(error_item(
                    "dataset_identity_mismatch",
                    format!("Binding '{binding_id}' expects dataset '{}' with CSV SHA-256 {}, but resolved identity is '{}' / {}.", settings.dataset_id, settings.csv_sha256, ds.metadata.id, ds.metadata.csv_sha256),
                    "Supply the dataset bytes pinned by the case, or correct the declared dataset_id and csv_sha256 after verifying provenance.".into(),
                ));
            }
            if !criterion {
                items.push(error_item(
                    "criterion_mismatch",
                    format!("Binding '{binding_id}' uses criterion {} V/m; dataset '{}' declares {} V/m.", case.operating.electric_field_criterion_v_per_m, ds.metadata.id, ds.metadata.electric_field_criterion_v_per_m),
                    "Use the dataset's declared criterion or select data characterized at the case criterion; automatic criterion conversion is unsupported.".into(),
                ));
            }
            if !temperature {
                items.push(error_item(
                    "temperature_outside_dataset_span",
                    format!("Binding '{binding_id}' temperature {} K is not strictly inside dataset '{}' nominal span [{t_min}, {t_max}] K.", case.operating.temperature_k, ds.metadata.id),
                    "Select a dataset whose nominal temperature span strictly contains the operating temperature. Do not extrapolate.".into(),
                ));
            }
            datasets.push(DatasetPreflight {
                binding_id: binding_id.into(),
                dataset_id: settings.dataset_id.clone(),
                expected_csv_sha256: settings.csv_sha256.clone(),
                available: true,
                actual_csv_sha256: Some(ds.metadata.csv_sha256.clone()),
                data_class: Some(
                    serde_json::to_value(ds.metadata.data_class)
                        .unwrap_or_default()
                        .as_str()
                        .unwrap_or("unknown")
                        .to_owned(),
                ),
                temperature_range_k: Some([t_min, t_max]),
                dataset_criterion_v_per_m: Some(ds.metadata.electric_field_criterion_v_per_m),
                temperature_compatible: Some(temperature),
                criterion_compatible: Some(criterion),
                identity_compatible: Some(identity),
            });
        } else {
            items.push(error_item(
                "dataset_missing",
                format!("Binding '{binding_id}' requires dataset '{}' (CSV SHA-256 {}).", settings.dataset_id, settings.csv_sha256),
                "Provide the matching dataset in the resolved dataset map. The runner will not substitute another dataset.".into(),
            ));
            datasets.push(DatasetPreflight {
                binding_id: binding_id.into(),
                dataset_id: settings.dataset_id.clone(),
                expected_csv_sha256: settings.csv_sha256.clone(),
                available: false,
                actual_csv_sha256: None,
                data_class: None,
                temperature_range_k: None,
                dataset_criterion_v_per_m: None,
                temperature_compatible: None,
                criterion_compatible: None,
                identity_compatible: None,
            });
        }
    }

    if let Some(map) = resolved_datasets {
        for id in map.keys() {
            if !case
                .material_bindings()
                .iter()
                .any(|(_, m)| m.dataset_id == *id)
            {
                items.push(error_item(
                    "dataset_not_declared",
                    format!("Resolved dataset '{id}' is not referenced by this case."),
                    "Remove the unrelated dataset or declare a material binding that uses it."
                        .into(),
                ));
            }
        }
    }
    if case.validate().is_ok()
        && datasets.iter().all(|d| d.available)
        && let Err(error) =
            case.validate_against_dataset_map(&dataset_map_with_embedded(case, &dataset_map))
    {
        items.push(error_item(
                "dataset_validation_failed",
                format!("The engine's material dependency validation failed: {error}"),
                "Resolve every tape-spec binding to the dataset identity, method, criterion, and temperature domain declared by the case.".into(),
            ));
    }

    let geometry_support = if case.fixed_geometry.path3d.is_some() {
        "nonplanar helical path; supported with a declared Cartesian field map".to_owned()
    } else if case.fixed_geometry.path.is_some() {
        "general planar path; supported by the built-in field evaluator".to_owned()
    } else {
        "racetrack; supported by the built-in field evaluator".to_owned()
    };
    let field_source = if case.field_map.is_some() {
        "case-declared external field map; map provenance and calibration are author-declared"
            .to_owned()
    } else {
        "built-in numerical field evaluator".to_owned()
    };

    let mut price_basis = vec![format!(
        "base conductor: ${}/m, provenance {:?}",
        case.cost.price_usd_per_m, case.cost.price_source
    )];
    price_basis.push(format!(
        "declared case provenance and price assumptions: {}",
        case.provenance
    ));
    price_basis.push(format!(
        "cost ledger: scrap fraction {}, assembly ${}/pancake, interface joint ${}; piece policy {}",
        case.cost.scrap_fraction,
        case.cost.assembly_cost_per_pancake_usd,
        case.cost.joint_cost_usd,
        if case.cost.piece_policy.is_some() { "declared" } else { "none" }
    ));
    if let Some(specs) = &case.tape_specs {
        price_basis.extend(specs.iter().map(|(id, spec)| {
            format!(
                "tape spec '{id}': ${}/m, provenance {:?}",
                spec.price_usd_per_m, spec.price_source
            )
        }));
    }
    let declared_screens = declared_screens(case);
    let missing_screens = missing_screens(case);
    for missing in &missing_screens {
        items.push(PreflightItem {
            level: PreflightLevel::Warning,
            code: "screen_not_evaluated".into(),
            message: format!("The study does not evaluate {missing}."),
            correction: None,
        });
    }
    let workload = workload(case);
    let errors: Vec<PreflightItem> = items
        .iter()
        .filter(|i| matches!(i.level, PreflightLevel::Error))
        .cloned()
        .collect();
    let warnings: Vec<_> = items
        .iter()
        .filter(|i| matches!(i.level, PreflightLevel::Warning))
        .cloned()
        .collect();
    StudyPreflight {
        schema: "optcoil-study-preflight/v1".into(),
        case_id: case.id.clone(),
        geometry_support,
        field_source,
        datasets,
        temperature_k: case.operating.temperature_k,
        electric_field_criterion_v_per_m: case.operating.electric_field_criterion_v_per_m,
        width_transfer: format!(
            "Bridge Ic per metre is scaled to the declared {:.3} mm tape width under uniform width and current-sharing assumptions. Full-width and batch qualification remain unperformed.",
            case.fixed_geometry.tape_width_m * 1000.0
        ),
        price_basis,
        declared_screens,
        missing_screens,
        quadrature_orders: case.numerics.quadrature_orders,
        refinement_change_limit_fraction: case.numerics.max_refinement_change_fraction,
        declared_threads,
        selected_threads,
        workload,
        ready_to_run: errors.is_empty(),
        engineering_acceptance_claim: false,
        errors,
        warnings,
        items,
    }
}

/// Convenience accepting the runner's thread options without requiring
/// clients to translate the small shared option type.
pub fn preflight_coupled_search_with_options(
    case: &CoupledSearchCase,
    resolved_datasets: Option<&BTreeMap<String, MaterialDataset>>,
    options: &CoupledSearchOptions,
) -> StudyPreflight {
    preflight_coupled_search(
        case,
        resolved_datasets,
        &PreflightOptions {
            threads: options.threads,
        },
    )
}

fn dataset_map_with_embedded(
    case: &CoupledSearchCase,
    resolved: &BTreeMap<String, MaterialDataset>,
) -> BTreeMap<String, MaterialDataset> {
    let mut all = resolved.clone();
    for (_, material) in case.material_bindings() {
        if !all.contains_key(&material.dataset_id)
            && let Ok(dataset) = MaterialDataset::embedded_by_id(&material.dataset_id)
        {
            all.insert(material.dataset_id.clone(), dataset);
        }
    }
    all
}

fn declared_screens(case: &CoupledSearchCase) -> Vec<String> {
    let mut screens = vec![
        "field requirement".into(),
        "material current capacity".into(),
        "utilization".into(),
        "bend radius".into(),
        "interpolation and numerical refinement".into(),
    ];
    if case.requirement.good_field_region.is_some() {
        screens.push("good-field region".into());
    }
    if case
        .manufacturing
        .as_ref()
        .is_some_and(|m| m.max_bend_strain.is_some())
    {
        screens.push("outer-fiber bend strain".into());
    }
    if let Some(m) = &case.mechanical {
        screens.push("Lorentz load".into());
        if m.max_hoop_stress_pa.is_some() {
            screens.push("hoop stress".into());
        }
        if m.max_transverse_pressure_pa.is_some() {
            screens.push("transverse pressure".into());
        }
        if m.max_membrane_tension_n_per_m.is_some() {
            screens.push("membrane tension".into());
        }
    }
    if case.limits.self_field_correction.is_some() {
        screens.push("self-field correction".into());
    }
    if case.limits.along_current_model.is_some() {
        screens.push("along-current model".into());
    }
    for (enabled, name) in [
        (case.thermal_margin.is_some(), "thermal margin"),
        (case.ac_loss.is_some(), "AC loss"),
        (case.quench_hotspot.is_some(), "quench hotspot"),
        (case.screening_current.is_some(), "screening current"),
        (case.transition.is_some(), "E–J transition"),
        (case.quench_transient.is_some(), "quench transient"),
    ] {
        if enabled {
            screens.push(name.into());
        }
    }
    screens
}

fn missing_screens(case: &CoupledSearchCase) -> Vec<String> {
    let mut missing = vec![
        "width-wise Jc transfer and batch variation".into(),
        "independent candidate-specific field reference".into(),
    ];
    for (enabled, name) in [
        (case.thermal_margin.is_none(), "thermal margin"),
        (case.ac_loss.is_none(), "AC loss"),
        (case.quench_hotspot.is_none(), "quench hotspot"),
        (case.screening_current.is_none(), "screening current"),
        (case.transition.is_none(), "E–J transition"),
        (case.quench_transient.is_none(), "quench transient"),
    ] {
        if enabled {
            missing.push(name.into());
        }
    }
    missing
}

fn workload(case: &CoupledSearchCase) -> WorkloadEstimate {
    let points = case
        .choices
        .turns_along_normal
        .iter()
        .map(|&turns| {
            let turn_count = optcoil_model::coupled_search::expand_relative_turn_indices(
                &case.sampling.relative_turn_indices,
                turns,
            )
            .len() as u128;
            (case.sampling.stations.len() as u128)
                .saturating_mul(turn_count)
                .saturating_mul(
                    case.choices
                        .tapes_along_width
                        .iter()
                        .copied()
                        .max()
                        .unwrap_or(0) as u128,
                )
                .saturating_mul(case.sampling.width_points as u128)
        })
        .max()
        .unwrap_or(0);
    let primary = points.saturating_mul(case.candidate_count() as u128);
    let refined_turns = case
        .choices
        .turns_along_normal
        .iter()
        .map(|&n| {
            optcoil_model::coupled_search::refined_plan_turn_indices(
                n,
                &optcoil_model::coupled_search::expand_relative_turn_indices(
                    &case.sampling.relative_turn_indices,
                    n,
                ),
            )
            .len() as u128
        })
        .max()
        .unwrap_or(0);
    let refined_stations =
        (case.sampling.stations.len() + case.refined_plan.additional_stations.len()) as u128;
    let refined = refined_stations
        .saturating_mul(refined_turns)
        .saturating_mul(
            case.choices
                .tapes_along_width
                .iter()
                .copied()
                .max()
                .unwrap_or(0) as u128,
        )
        .saturating_mul(case.sampling.width_points as u128)
        .saturating_mul(2); // baseline and one selected optimum acceptance recheck
    let q_work = case
        .numerics
        .quadrature_orders
        .into_iter()
        .map(|q| u128::from(q).saturating_pow(3))
        .sum::<u128>();
    WorkloadEstimate {
        candidate_count: case.candidate_count(),
        primary_point_upper_estimate: primary,
        primary_kernel_work_proxy: primary.saturating_mul(q_work),
        refined_point_upper_estimate: refined,
        refined_kernel_work_proxy: refined.saturating_mul(u128::from(case.numerics.quadrature_orders[1]).saturating_pow(3)),
        estimate_label: "Conservative work proxy from the declared grid and sampling plan; early exits and extra checks change actual work. It is not a wall-clock prediction or guarantee.".into(),
    }
}

fn error_item(code: &str, message: String, correction: String) -> PreflightItem {
    PreflightItem {
        level: PreflightLevel::Error,
        code: code.into(),
        message,
        correction: Some(correction),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::coupled_search::CoupledSearchCase;

    fn datasets(case: &CoupledSearchCase) -> BTreeMap<String, MaterialDataset> {
        case.material_bindings()
            .into_iter()
            .filter_map(|(_, m)| {
                MaterialDataset::embedded_by_id(&m.dataset_id)
                    .ok()
                    .map(|d| (m.dataset_id.clone(), d))
            })
            .collect()
    }

    #[test]
    fn embedded_measured_reference_case_is_ready_without_an_acceptance_claim() {
        let case = CoupledSearchCase::embedded().unwrap();
        let result = preflight_coupled_search(&case, None, &PreflightOptions::default());
        assert!(result.ready_to_run, "{:?}", result.errors);
        assert_eq!(result.datasets[0].data_class.as_deref(), Some("measured"));
        assert!(!result.engineering_acceptance_claim);
        assert!(
            result
                .width_transfer
                .contains("Full-width and batch qualification remain unperformed")
        );
        assert!(result.workload.candidate_count > 0);
        assert!(
            result.workload.primary_kernel_work_proxy
                > result.workload.primary_point_upper_estimate
        );
    }

    #[test]
    fn missing_external_dataset_has_actionable_error_and_identity_is_not_substituted() {
        let mut case = CoupledSearchCase::embedded().unwrap();
        case.material.dataset_id = "customer-lot-1".into();
        case.material.csv_sha256 = "a".repeat(64);
        let result = preflight_coupled_search(&case, None, &PreflightOptions::default());
        assert!(!result.ready_to_run);
        assert!(!result.datasets[0].available);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.code == "dataset_missing" && e.correction.is_some())
        );
    }

    #[test]
    fn temperature_criterion_and_dataset_identity_mismatches_are_reported() {
        let original = CoupledSearchCase::embedded().unwrap();
        let ds = datasets(&original);
        let mut bad_t = original.clone();
        bad_t.operating.temperature_k = 20.0;
        let r = preflight_coupled_search(&bad_t, Some(&ds), &PreflightOptions::default());
        assert!(
            r.errors
                .iter()
                .any(|e| e.code == "temperature_outside_dataset_span")
        );
        let mut bad_c = original.clone();
        bad_c.operating.electric_field_criterion_v_per_m *= 2.0;
        let r = preflight_coupled_search(&bad_c, Some(&ds), &PreflightOptions::default());
        assert!(r.errors.iter().any(|e| e.code == "criterion_mismatch"));
        let mut bad_id = original;
        bad_id.material.csv_sha256 = "b".repeat(64);
        let ds = datasets(&bad_id);
        let r = preflight_coupled_search(&bad_id, Some(&ds), &PreflightOptions::default());
        assert!(
            r.errors
                .iter()
                .any(|e| e.code == "dataset_identity_mismatch")
        );
    }

    #[test]
    fn graded_case_reports_every_spec_dependency() {
        let case = CoupledSearchCase::embedded_oc020().unwrap();
        let missing = preflight_coupled_search(&case, None, &PreflightOptions::default());
        assert!(missing.ready_to_run, "embedded data should resolve");
        assert_eq!(missing.datasets.len(), 2);
        assert!(missing.datasets.iter().any(|d| d.binding_id == "base"));
        assert!(missing.datasets.iter().any(|d| d.binding_id != "base"));
        let empty = BTreeMap::new();
        let supplied = preflight_coupled_search(&case, Some(&empty), &PreflightOptions::default());
        assert!(supplied.ready_to_run);
    }

    #[test]
    fn workload_covers_grid_and_thread_override_rules() {
        let case = CoupledSearchCase::embedded().unwrap();
        let base = preflight_coupled_search(&case, None, &PreflightOptions::default());
        assert_eq!(base.workload.candidate_count, case.candidate_count());
        assert!(base.workload.refined_point_upper_estimate > 0);
        assert!(base.workload.estimate_label.contains("not a wall-clock"));
        let low = preflight_coupled_search(&case, None, &PreflightOptions { threads: Some(1) });
        assert_eq!(low.selected_threads, 1);
        let too_many = preflight_coupled_search(
            &case,
            None,
            &PreflightOptions {
                threads: Some(case.execution.max_threads + 1),
            },
        );
        assert!(!too_many.ready_to_run);
        assert!(
            too_many
                .errors
                .iter()
                .any(|e| e.code == "threads_exceed_case_limit")
        );
    }
}
