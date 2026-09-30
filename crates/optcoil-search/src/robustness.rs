//! Deterministic what-if reruns across named supplier, conductor and
//! operating scenarios. Results are scenario records, not probabilities or
//! confidence estimates.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use optcoil_model::{
    Status,
    coupled_search::CoupledSearchCase,
    material::{
        MATERIAL_DATASET_BUNDLE_SCHEMA, MATERIAL_SCHEMA_V2, MaterialBundle, MaterialDataClass,
        MaterialDataset,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    RunError,
    coupled_search::{self, CoupledSearchOptions, CoupledSearchRunRecord, SearchProgress},
    preflight::preflight_coupled_search_with_options,
    study::{StudyError, StudyWorkspace},
};

pub const ROBUSTNESS_SPEC_SCHEMA: &str = "optcoil-robustness-spec/v1";
pub const ROBUSTNESS_RECORD_SCHEMA: &str = "optcoil-robustness-study/v1";
pub const MAX_ROBUSTNESS_VARIANTS: usize = 8;
pub const MAX_ROBUSTNESS_SCENARIOS: usize = 16;
pub const MAX_ROBUSTNESS_RUNS: usize = 64;
pub const MAX_ROBUSTNESS_RESULT_BYTES: usize = 48 * 1024 * 1024;
/// Aggregate preflight proxy cap across all scenario reruns.
pub const MAX_ROBUSTNESS_KERNEL_WORK: u128 = 750_000_000;
pub const MAX_ROBUSTNESS_CANDIDATES: u128 = 4_096;
const ROBUSTNESS_HEADER_RESERVE_BYTES: usize = 16 * 1024;
const ROBUSTNESS_ROW_METADATA_RESERVE_BYTES: usize = 512;

#[derive(Debug, Error)]
pub enum RobustnessError {
    #[error("invalid robustness study: {0}")]
    Invalid(String),
    #[error(transparent)]
    Study(#[from] StudyError),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Explicit analyst-authored scenarios. `nominal` is mandatory and must
/// have no perturbations; other rows carry no implied probability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobustnessSpec {
    pub schema: String,
    pub scenarios: Vec<RobustnessScenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobustnessScenario {
    pub id: String,
    pub name: String,
    /// Multiplier applied to all conductor rates and piece-catalogue rates
    /// whose material binding resolves to this dataset identity.
    #[serde(default)]
    pub price_multipliers: BTreeMap<String, f64>,
    /// Multiplier applied to Ic in each bound dataset with this identity.
    #[serde(default)]
    pub ic_multipliers: BTreeMap<String, f64>,
    /// Uniform additive change to the declared operating temperature.
    pub temperature_offset_k: f64,
}

impl RobustnessScenario {
    pub fn nominal() -> Self {
        Self {
            id: "nominal".into(),
            name: "Nominal conditions".into(),
            price_multipliers: BTreeMap::new(),
            ic_multipliers: BTreeMap::new(),
            temperature_offset_k: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessPreflight {
    pub schema: String,
    pub variant_ids: Vec<String>,
    pub scenario_ids: Vec<String>,
    pub run_count: usize,
    pub candidate_runs: u128,
    pub kernel_work_proxy: u128,
    pub estimated_output_bytes: u128,
    pub preflights: Vec<RobustnessRunPreflight>,
    pub ready_to_run: bool,
    pub engineering_acceptance_claim: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessRunPreflight {
    pub variant_id: String,
    pub scenario_id: String,
    pub candidate_count: usize,
    pub kernel_work_proxy: u128,
    pub ready_to_run: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessStudyRecord {
    pub schema: String,
    pub spec: RobustnessSpec,
    pub variant_ids: Vec<String>,
    pub source_fingerprints: Vec<RobustnessSourceFingerprint>,
    pub engine_fingerprint: String,
    pub input_fingerprint: String,
    pub rows: Vec<RobustnessRunRecord>,
    pub scenario_summaries: Vec<RobustnessScenarioSummary>,
    pub winner_switches: Vec<RobustnessWinnerSwitch>,
    /// True only when every requested variant×scenario row has a selected
    /// PASS candidate with PASS cost/screen agreement and search status.
    pub all_scenarios_have_supported_winner: bool,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessSourceFingerprint {
    pub variant_id: String,
    pub case_sha256: String,
    pub bundle_sha256: Vec<String>,
    pub resolved_bundle_sha256: Vec<String>,
    pub options_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessRunRecord {
    pub variant_id: String,
    pub variant_name: String,
    pub scenario_id: String,
    pub scenario_name: String,
    /// Exact source inputs are retained so the run can be repeated offline.
    pub source_case_json: String,
    pub source_dataset_bundles: Vec<String>,
    pub source_options: CoupledSearchOptions,
    /// Complete resolved source bindings, including embedded datasets,
    /// serialized as offline-reopenable bundles.
    pub resolved_source_dataset_bundles: Vec<String>,
    pub transformed_case_json: Option<String>,
    pub derived_dataset_bundles: Vec<String>,
    pub run_record_json: Option<String>,
    pub status: RobustnessRunStatus,
    pub error: Option<String>,
    pub winner: Option<RobustnessWinner>,
    /// Whether the nominal selected geometry remains PASS in this scenario's
    /// primary candidate screens. This is not an acceptance-level refined
    /// recheck and does not mean the geometry remains the preferred winner.
    pub nominal_winner_geometry_survives: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RobustnessRunStatus {
    Completed,
    /// Bounded grid completed and all choices conclusively failed declared
    /// gates; this variant has no eligible design in the declared search.
    NoWinner,
    /// The run completed but cannot establish whether a winner exists.
    Unresolved,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessWinner {
    pub candidate_index: usize,
    pub turns_along_normal: u32,
    pub tapes_along_width: u32,
    pub strands_parallel: u32,
    pub bend_radius_m: Option<f64>,
    pub straight_half_length_m: Option<f64>,
    pub tape_spec_ids: Option<Vec<String>>,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessScenarioSummary {
    pub scenario_id: String,
    pub winner_variant_id: Option<String>,
    pub winner_cost_usd: Option<f64>,
    pub resolved_cost_min_usd: Option<f64>,
    pub resolved_cost_max_usd: Option<f64>,
    pub possible_regret_usd: Option<f64>,
    pub all_variants_resolved: bool,
    pub missing_variant_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobustnessWinnerSwitch {
    pub from_scenario_id: String,
    pub to_scenario_id: String,
    pub from_variant_id: String,
    pub to_variant_id: String,
}

pub fn robustness_preflight(
    workspace: &StudyWorkspace,
    variant_ids: &[String],
    spec: &RobustnessSpec,
) -> Result<RobustnessPreflight, RobustnessError> {
    let selected = validate_request(workspace, variant_ids, spec)?;
    let run_count = variant_ids.len().saturating_mul(spec.scenarios.len());
    let mut source_payload_lower_bound = count_serialized_size(spec, MAX_ROBUSTNESS_RESULT_BYTES)
        .ok_or_else(|| RobustnessError::Invalid("robustness spec exceeds payload cap".into()))?
        .saturating_add(ROBUSTNESS_HEADER_RESERVE_BYTES)
        .saturating_add(run_count.saturating_mul(ROBUSTNESS_ROW_METADATA_RESERVE_BYTES));
    for variant in &selected {
        let source_case = CoupledSearchCase::from_json(&variant.case_json)
            .map_err(|e| RobustnessError::Invalid(format!("case parse: {e}")))?;
        let resolved_bundles = complete_source_bundles(variant, &source_case)?;
        let per_row = count_serialized_size(
            &(
                &variant.case_json,
                &variant.dataset_bundles,
                &resolved_bundles,
            ),
            MAX_ROBUSTNESS_RESULT_BYTES,
        )
        .ok_or_else(|| {
            RobustnessError::Invalid("source inputs exceed robustness result cap".into())
        })?;
        source_payload_lower_bound =
            source_payload_lower_bound.saturating_add(per_row.saturating_mul(spec.scenarios.len()));
        if source_payload_lower_bound > MAX_ROBUSTNESS_RESULT_BYTES {
            return Err(RobustnessError::Invalid(format!(
                "repeated robustness source payload requires at least {source_payload_lower_bound} bytes, exceeding the {} MiB result cap",
                MAX_ROBUSTNESS_RESULT_BYTES / (1024 * 1024)
            )));
        }
    }
    let mut preflights = Vec::new();
    let mut candidate_runs = 0_u128;
    let mut kernel_work_proxy = 0_u128;
    let mut estimated_output_bytes = source_payload_lower_bound as u128;
    for variant in &selected {
        let source_case: CoupledSearchCase = serde_json::from_str(&variant.case_json)?;
        for scenario in &spec.scenarios {
            let transformed = transform_inputs(variant, &source_case, scenario);
            match transformed {
                Ok((case_json, datasets, derived_bundles)) => {
                    let parsed: CoupledSearchCase = serde_json::from_str(&case_json)?;
                    let pf = preflight_coupled_search_with_options(
                        &parsed,
                        Some(&datasets),
                        &variant.options,
                    );
                    let ready = pf.ready_to_run;
                    let errors: Vec<String> = pf.errors.iter().map(|e| e.message.clone()).collect();
                    candidate_runs =
                        candidate_runs.saturating_add(pf.workload.candidate_count as u128);
                    kernel_work_proxy = kernel_work_proxy
                        .saturating_add(pf.workload.primary_kernel_work_proxy)
                        .saturating_add(pf.workload.refined_kernel_work_proxy);
                    let per_row_artifact_bytes = case_json.len() as u128
                        + derived_bundles
                            .iter()
                            .map(|raw| raw.len() as u128)
                            .sum::<u128>()
                        + ROBUSTNESS_ROW_METADATA_RESERVE_BYTES as u128;
                    estimated_output_bytes = estimated_output_bytes
                        .saturating_add(per_row_artifact_bytes)
                        .saturating_add(pf.workload.candidate_count as u128 * 8_192);
                    if estimated_output_bytes > MAX_ROBUSTNESS_RESULT_BYTES as u128 {
                        return Err(RobustnessError::Invalid(format!(
                            "estimated retained output {estimated_output_bytes} bytes exceeds the {} MiB robustness result cap",
                            MAX_ROBUSTNESS_RESULT_BYTES / (1024 * 1024)
                        )));
                    }
                    preflights.push(RobustnessRunPreflight {
                        variant_id: variant.id.clone(),
                        scenario_id: scenario.id.clone(),
                        candidate_count: pf.workload.candidate_count,
                        kernel_work_proxy: pf
                            .workload
                            .primary_kernel_work_proxy
                            .saturating_add(pf.workload.refined_kernel_work_proxy),
                        ready_to_run: ready,
                        errors,
                    });
                }
                Err(error) => preflights.push(RobustnessRunPreflight {
                    variant_id: variant.id.clone(),
                    scenario_id: scenario.id.clone(),
                    candidate_count: 0,
                    kernel_work_proxy: 0,
                    ready_to_run: false,
                    errors: vec![error.to_string()],
                }),
            }
        }
    }
    if candidate_runs > MAX_ROBUSTNESS_CANDIDATES {
        return Err(RobustnessError::Invalid(format!(
            "aggregate candidate count {candidate_runs} exceeds the {MAX_ROBUSTNESS_CANDIDATES} robustness cap"
        )));
    }
    if kernel_work_proxy > MAX_ROBUSTNESS_KERNEL_WORK {
        return Err(RobustnessError::Invalid(format!(
            "aggregate kernel work proxy {kernel_work_proxy} exceeds the {MAX_ROBUSTNESS_KERNEL_WORK} robustness cap"
        )));
    }
    if estimated_output_bytes > MAX_ROBUSTNESS_RESULT_BYTES as u128 {
        return Err(RobustnessError::Invalid(format!(
            "estimated retained output {estimated_output_bytes} bytes exceeds the {} MiB robustness result cap",
            MAX_ROBUSTNESS_RESULT_BYTES / (1024 * 1024)
        )));
    }
    Ok(RobustnessPreflight {
        schema: "optcoil-robustness-preflight/v1".into(),
        variant_ids: variant_ids.to_vec(),
        scenario_ids: spec.scenarios.iter().map(|s| s.id.clone()).collect(),
        run_count,
        candidate_runs,
        kernel_work_proxy,
        estimated_output_bytes,
        ready_to_run: preflights.iter().all(|p| p.ready_to_run),
        preflights,
        engineering_acceptance_claim: false,
    })
}

pub fn run_study_robustness(
    workspace: &StudyWorkspace,
    variant_ids: &[String],
    spec: &RobustnessSpec,
    cancel: &AtomicBool,
    progress: Option<&SearchProgress>,
) -> Result<RobustnessStudyRecord, RobustnessError> {
    let selected = validate_request(workspace, variant_ids, spec)?;
    let preflight = robustness_preflight(workspace, variant_ids, spec)?;
    let sources = selected
        .iter()
        .map(|variant| source_fingerprint(variant))
        .collect::<Result<Vec<_>, _>>()?;
    let mut serialized_payload_used =
        count_serialized_size(&(&spec, variant_ids, &sources), MAX_ROBUSTNESS_RESULT_BYTES)
            .ok_or_else(|| {
                RobustnessError::Invalid("robustness result header exceeds payload cap".into())
            })?
            .saturating_add(ROBUSTNESS_HEADER_RESERVE_BYTES)
            .saturating_add(preflight.run_count * ROBUSTNESS_ROW_METADATA_RESERVE_BYTES);
    let mut rows = Vec::with_capacity(preflight.run_count);
    for scenario in &spec.scenarios {
        for variant in &selected {
            if cancel.load(Ordering::Relaxed) {
                return Err(RunError::Cancelled.into());
            }
            let source_case: CoupledSearchCase = serde_json::from_str(&variant.case_json)?;
            let mut row = RobustnessRunRecord {
                variant_id: variant.id.clone(),
                variant_name: variant.name.clone(),
                scenario_id: scenario.id.clone(),
                scenario_name: scenario.name.clone(),
                source_case_json: variant.case_json.clone(),
                source_dataset_bundles: variant.dataset_bundles.clone(),
                source_options: variant.options,
                resolved_source_dataset_bundles: Vec::new(),
                transformed_case_json: None,
                derived_dataset_bundles: Vec::new(),
                run_record_json: None,
                status: RobustnessRunStatus::Failed,
                error: None,
                winner: None,
                nominal_winner_geometry_survives: None,
            };
            row.resolved_source_dataset_bundles = complete_source_bundles(variant, &source_case)
                .unwrap_or_else(|_| variant.dataset_bundles.clone());
            match transform_inputs(variant, &source_case, scenario) {
                Err(error) => row.error = Some(error.to_string()),
                Ok((case_json, datasets, derived_bundles)) => {
                    row.transformed_case_json = Some(case_json.clone());
                    row.derived_dataset_bundles = derived_bundles;
                    if !preflight_row_ready(&preflight, &variant.id, &scenario.id) {
                        row.error = preflight_row_error(&preflight, &variant.id, &scenario.id);
                    } else {
                        match coupled_search::run_coupled_search_case_with_datasets_progress(
                            &case_json,
                            &variant.options,
                            &datasets,
                            cancel,
                            progress,
                        ) {
                            Ok(run) => {
                                let winner = select_winner(&run);
                                row.status = match run_resolution(&run, winner.is_some()) {
                                    RobustnessRunStatus::Completed => {
                                        RobustnessRunStatus::Completed
                                    }
                                    RobustnessRunStatus::NoWinner => RobustnessRunStatus::NoWinner,
                                    _ => {
                                        row.error = Some(format!(
                                            "completed run did not resolve a supported winner (search status {:?}, agreement {:?})",
                                            run.search_status, run.acceptance.agreement_status
                                        ));
                                        RobustnessRunStatus::Unresolved
                                    }
                                };
                                row.winner = winner;
                                row.run_record_json = Some(serde_json::to_string(&run)?);
                                row.nominal_winner_geometry_survives = Some(false);
                            }
                            Err(RunError::Cancelled) => return Err(RunError::Cancelled.into()),
                            Err(error) => row.error = Some(error.to_string()),
                        }
                    }
                }
            }
            let remaining = MAX_ROBUSTNESS_RESULT_BYTES.saturating_sub(serialized_payload_used);
            let row_bytes = count_serialized_size(&row, remaining).ok_or_else(|| {
                RobustnessError::Invalid(format!(
                    "adding row '{}/{}' would exceed the {} MiB robustness result cap",
                    row.variant_id,
                    row.scenario_id,
                    MAX_ROBUSTNESS_RESULT_BYTES / (1024 * 1024)
                ))
            })?;
            serialized_payload_used = serialized_payload_used
                .saturating_add(row_bytes)
                .saturating_add(1);
            rows.push(row);
        }
    }
    mark_nominal_geometry_survival(&mut rows, &spec.scenarios);
    let scenario_summaries = summarize_scenarios(&rows, &spec.scenarios);
    let winner_switches = find_winner_switches(&scenario_summaries);
    let all_scenarios_have_supported_winner = rows.iter().all(|row| {
        matches!(
            row.status,
            RobustnessRunStatus::Completed | RobustnessRunStatus::NoWinner
        )
    }) && scenario_summaries
        .iter()
        .all(|summary| summary.winner_variant_id.is_some());
    let engine_fingerprint = engine_fingerprint();
    let input_fingerprint = hash(&serde_json::to_vec(&(spec, &sources, &engine_fingerprint))?);
    let record = RobustnessStudyRecord {
        schema: ROBUSTNESS_RECORD_SCHEMA.into(),
        spec: spec.clone(),
        variant_ids: variant_ids.to_vec(),
        source_fingerprints: sources,
        engine_fingerprint,
        input_fingerprint,
        rows,
        scenario_summaries,
        winner_switches,
        all_scenarios_have_supported_winner,
        limitations: vec![
            "Scenarios are explicit analyst-authored what-ifs; no probabilities or confidence intervals are inferred.".into(),
            "A winner is a screening and cost result under the retained case assumptions, not engineering qualification.".into(),
            "nominal_winner_geometry_survives reports candidate-level primary-screen survival only; it is not an acceptance-level refined recheck or a preference claim.".into(),
            "Missing, failed or unresolved scenario runs prevent an all-scenarios-supported-winner conclusion.".into(),
        ],
    };
    if count_serialized_size(&record, MAX_ROBUSTNESS_RESULT_BYTES).is_none() {
        return Err(RobustnessError::Invalid(format!(
            "robustness result exceeds {} MiB payload limit",
            MAX_ROBUSTNESS_RESULT_BYTES / (1024 * 1024)
        )));
    }
    Ok(record)
}

/// Validate an imported or retained artifact without treating it as a live
/// engine cache. Every embedded run and source bundle is checked against the
/// retained row inputs and the record-level fingerprints.
pub fn validate_robustness_record(record: &RobustnessStudyRecord) -> Result<(), RobustnessError> {
    if count_serialized_size(record, MAX_ROBUSTNESS_RESULT_BYTES).is_none() {
        return Err(RobustnessError::Invalid(
            "robustness record exceeds payload limit".into(),
        ));
    }
    if record.schema != ROBUSTNESS_RECORD_SCHEMA {
        return Err(RobustnessError::Invalid(format!(
            "unsupported robustness record schema '{}'",
            record.schema
        )));
    }
    validate_spec(&record.spec, record.variant_ids.len())?;
    if record.variant_ids.is_empty()
        || record.variant_ids.len() > MAX_ROBUSTNESS_VARIANTS
        || record.variant_ids.iter().collect::<BTreeSet<_>>().len() != record.variant_ids.len()
        || record.variant_ids.len() * record.spec.scenarios.len() != record.rows.len()
        || record.rows.len() > MAX_ROBUSTNESS_RUNS
    {
        return Err(RobustnessError::Invalid(
            "record variant/scenario dimensions are invalid".into(),
        ));
    }
    if record.engine_fingerprint.len() != 64
        || !record
            .engine_fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(RobustnessError::Invalid(
            "malformed engine fingerprint".into(),
        ));
    }
    let mut row_keys = BTreeSet::new();
    let mut source_fingerprints = BTreeMap::new();
    for row in &record.rows {
        if !record.variant_ids.contains(&row.variant_id)
            || !record
                .spec
                .scenarios
                .iter()
                .any(|s| s.id == row.scenario_id)
            || !row_keys.insert((row.variant_id.as_str(), row.scenario_id.as_str()))
        {
            return Err(RobustnessError::Invalid(
                "record row references an unknown or duplicate variant/scenario".into(),
            ));
        }
        let source_case = CoupledSearchCase::from_json(&row.source_case_json)
            .map_err(|e| RobustnessError::Invalid(format!("source case: {e}")))?;
        let expected_resolved = complete_source_bundles(
            &crate::study::StudyVariant {
                id: row.variant_id.clone(),
                name: row.variant_name.clone(),
                case_json: row.source_case_json.clone(),
                dataset_bundles: row.source_dataset_bundles.clone(),
                options: row.source_options,
                results: Vec::new(),
            },
            &source_case,
        )?;
        if expected_resolved != row.resolved_source_dataset_bundles {
            return Err(RobustnessError::Invalid(
                "retained resolved source bundles do not match original source bytes and embedded identities".into(),
            ));
        }
        let mut source_datasets = datasets_from_bundle_list(&row.resolved_source_dataset_bundles)?;
        add_embedded_bindings(&source_case, &mut source_datasets);
        let source_complete = source_case.material_bindings().iter().all(|(_, binding)| {
            source_datasets
                .get(&binding.dataset_id)
                .is_some_and(|dataset| dataset.metadata.csv_sha256 == binding.csv_sha256)
        });
        if source_complete {
            source_case
                .validate_against_dataset_map(&source_datasets)
                .map_err(|e| RobustnessError::Invalid(format!("retained source bindings: {e}")))?;
        } else if row.status != RobustnessRunStatus::Failed {
            return Err(RobustnessError::Invalid(
                "completed row is missing a source dataset needed for offline replay".into(),
            ));
        }
        for original in &row.source_dataset_bundles {
            MaterialBundle::from_json(original)
                .map_err(|e| RobustnessError::Invalid(format!("source bundle: {e}")))?;
        }
        let source = RobustnessSourceFingerprint {
            variant_id: row.variant_id.clone(),
            case_sha256: hash(row.source_case_json.as_bytes()),
            bundle_sha256: row
                .source_dataset_bundles
                .iter()
                .map(|raw| hash(raw.as_bytes()))
                .collect(),
            resolved_bundle_sha256: row
                .resolved_source_dataset_bundles
                .iter()
                .map(|raw| hash(raw.as_bytes()))
                .collect(),
            options_sha256: hash(&serde_json::to_vec(&row.source_options)?),
        };
        if let Some(previous) = source_fingerprints.insert(row.variant_id.clone(), source.clone())
            && serde_json::to_value(previous)? != serde_json::to_value(source)?
        {
            return Err(RobustnessError::Invalid(format!(
                "source inputs differ between rows for variant '{}'",
                row.variant_id
            )));
        }
        if let Some(case_json) = &row.transformed_case_json {
            let transformed = CoupledSearchCase::from_json(case_json)
                .map_err(|e| RobustnessError::Invalid(format!("transformed case: {e}")))?;
            let mut datasets = datasets_from_bundle_list(&row.resolved_source_dataset_bundles)?;
            datasets.extend(datasets_from_bundle_list(&row.derived_dataset_bundles)?);
            add_embedded_bindings(&transformed, &mut datasets);
            transformed
                .validate_against_dataset_map(&datasets)
                .map_err(|e| RobustnessError::Invalid(format!("transformed bindings: {e}")))?;
            let scenario = record
                .spec
                .scenarios
                .iter()
                .find(|scenario| scenario.id == row.scenario_id)
                .expect("row scenario was validated above");
            let source_variant = crate::study::StudyVariant {
                id: row.variant_id.clone(),
                name: row.variant_name.clone(),
                case_json: row.source_case_json.clone(),
                dataset_bundles: row.source_dataset_bundles.clone(),
                options: row.source_options,
                results: Vec::new(),
            };
            match transform_inputs(&source_variant, &source_case, scenario) {
                Ok((expected_case, _, expected_bundles)) => {
                    if expected_case != *case_json
                        || expected_bundles != row.derived_dataset_bundles
                    {
                        return Err(RobustnessError::Invalid(
                            "retained transformed inputs do not match the declared scenario".into(),
                        ));
                    }
                }
                Err(error) if row.status == RobustnessRunStatus::Failed => {
                    if row.error.as_deref() != Some(error.to_string().as_str()) {
                        return Err(RobustnessError::Invalid(
                            "failed row reason does not match the reproducible scenario error"
                                .into(),
                        ));
                    }
                }
                Err(error) => {
                    return Err(RobustnessError::Invalid(format!(
                        "retained scenario inputs cannot be reconstructed: {error}"
                    )));
                }
            }
            if let Some(run_json) = &row.run_record_json {
                let run: CoupledSearchRunRecord = serde_json::from_str(run_json)?;
                if serde_json::to_value(&run.case)? != serde_json::to_value(&transformed)? {
                    return Err(RobustnessError::Invalid(
                        "run case differs from retained transformed case".into(),
                    ));
                }
                let checks = crate::verify::verify_record_checks(
                    run_json,
                    Some(case_json),
                    None,
                    None,
                    &active_bundle_refs(row, &transformed)?,
                )?;
                if checks
                    .iter()
                    .any(|check| check.outcome == crate::verify::Outcome::Fail)
                {
                    return Err(RobustnessError::Invalid(
                        "retained run record failed reopen verification".into(),
                    ));
                }
                let expected_run_input = hash(&serde_json::to_vec(&(
                    &run.case_sha256,
                    &run.implementation_sha256,
                    &row.source_options,
                    coupled_search::COUPLED_SEARCH_MODEL_ID,
                    coupled_search::COUPLED_SEARCH_CHECKER_ID,
                    env!("CARGO_PKG_VERSION"),
                ))?);
                if run.input_sha256 != expected_run_input {
                    return Err(RobustnessError::Invalid(
                        "run input fingerprint does not match retained execution options".into(),
                    ));
                }
                let expected_winner = select_winner(&run);
                let expected_status = run_resolution(&run, expected_winner.is_some());
                if serde_json::to_value(&expected_winner)? != serde_json::to_value(&row.winner)?
                    || expected_status != row.status
                {
                    return Err(RobustnessError::Invalid(
                        "row winner or resolution status differs from its run record".into(),
                    ));
                }
                match row.status {
                    RobustnessRunStatus::Completed if row.winner.is_none() => {
                        return Err(RobustnessError::Invalid(
                            "completed row has no supported winner".into(),
                        ));
                    }
                    RobustnessRunStatus::NoWinner if row.winner.is_some() => {
                        return Err(RobustnessError::Invalid(
                            "no-winner row unexpectedly contains a winner".into(),
                        ));
                    }
                    RobustnessRunStatus::Failed => {
                        return Err(RobustnessError::Invalid(
                            "failed row must not retain a completed run record".into(),
                        ));
                    }
                    _ => {}
                }
            } else if row.status != RobustnessRunStatus::Failed {
                return Err(RobustnessError::Invalid(
                    "row status requires a completed run record".into(),
                ));
            } else if row.error.is_none() {
                return Err(RobustnessError::Invalid(
                    "failed or unresolved row must include its reason".into(),
                ));
            }
        } else if row.status != RobustnessRunStatus::Failed
            || row.run_record_json.is_some()
            || row.error.is_none()
        {
            return Err(RobustnessError::Invalid(
                "row without transformed inputs must be an explicit failed row".into(),
            ));
        }
    }
    if source_fingerprints.len() != record.variant_ids.len() {
        return Err(RobustnessError::Invalid(
            "record is missing one or more variant source snapshots".into(),
        ));
    }
    let bound_dataset_ids: BTreeSet<String> = record
        .rows
        .iter()
        .filter_map(|row| CoupledSearchCase::from_json(&row.source_case_json).ok())
        .flat_map(|case| {
            case.material_bindings()
                .into_iter()
                .map(|(_, binding)| binding.dataset_id.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    for scenario in &record.spec.scenarios {
        for dataset_id in scenario
            .price_multipliers
            .keys()
            .chain(scenario.ic_multipliers.keys())
        {
            if !bound_dataset_ids.contains(dataset_id) {
                return Err(RobustnessError::Invalid(format!(
                    "scenario '{}' references unbound dataset '{dataset_id}'",
                    scenario.id
                )));
            }
        }
    }
    let computed_sources: Vec<_> = record
        .variant_ids
        .iter()
        .map(|id| source_fingerprints[id].clone())
        .collect();
    if serde_json::to_value(&computed_sources)?
        != serde_json::to_value(&record.source_fingerprints)?
    {
        return Err(RobustnessError::Invalid(
            "record source fingerprints do not match retained inputs".into(),
        ));
    }
    let source_variants: Vec<_> = record
        .variant_ids
        .iter()
        .map(|id| {
            let row = record
                .rows
                .iter()
                .find(|row| &row.variant_id == id)
                .expect("every declared variant has at least one scenario row");
            crate::study::StudyVariant {
                id: row.variant_id.clone(),
                name: row.variant_name.clone(),
                case_json: row.source_case_json.clone(),
                dataset_bundles: row.source_dataset_bundles.clone(),
                options: row.source_options,
                results: Vec::new(),
            }
        })
        .collect();
    let source_refs: Vec<_> = source_variants.iter().collect();
    ensure_comparable(&source_refs)?;
    let expected_input = hash(&serde_json::to_vec(&(
        &record.spec,
        &record.source_fingerprints,
        &record.engine_fingerprint,
    ))?);
    if expected_input != record.input_fingerprint {
        return Err(RobustnessError::Invalid(
            "record input fingerprint does not match retained inputs".into(),
        ));
    }
    if record.all_scenarios_have_supported_winner
        != (record.rows.iter().all(|row| {
            matches!(
                row.status,
                RobustnessRunStatus::Completed | RobustnessRunStatus::NoWinner
            )
        }) && record
            .scenario_summaries
            .iter()
            .all(|summary| summary.winner_variant_id.is_some()))
    {
        return Err(RobustnessError::Invalid(
            "record aggregate supported-winner flag is inconsistent".into(),
        ));
    }
    let expected_summaries = summarize_scenarios(&record.rows, &record.spec.scenarios);
    let expected_switches = find_winner_switches(&expected_summaries);
    let mut expected_rows = record.rows.clone();
    mark_nominal_geometry_survival(&mut expected_rows, &record.spec.scenarios);
    if serde_json::to_value(&expected_summaries)?
        != serde_json::to_value(&record.scenario_summaries)?
        || serde_json::to_value(&expected_switches)?
            != serde_json::to_value(&record.winner_switches)?
        || expected_rows
            .iter()
            .zip(&record.rows)
            .any(|(a, b)| a.nominal_winner_geometry_survives != b.nominal_winner_geometry_survives)
    {
        return Err(RobustnessError::Invalid(
            "record summaries, winner switches or nominal-geometry survival are inconsistent"
                .into(),
        ));
    }
    Ok(())
}

/// Returns whether a retained result still points at the exact current
/// variant source bytes and the current engine implementation identity.
pub fn robustness_record_is_current(
    workspace: &StudyWorkspace,
    record: &RobustnessStudyRecord,
) -> Result<bool, RobustnessError> {
    validate_robustness_record(record)?;
    robustness_record_binding_is_current(workspace, record)
}

/// Fast, nonvalidating source-binding check for a record already validated
/// during import or attachment. It checks the engine fingerprint and exact
/// retained variant source bytes/options, without reopening run records.
pub fn robustness_record_binding_is_current(
    workspace: &StudyWorkspace,
    record: &RobustnessStudyRecord,
) -> Result<bool, RobustnessError> {
    if record.engine_fingerprint != engine_fingerprint() {
        return Ok(false);
    }
    for id in &record.variant_ids {
        let Ok(current) = workspace.variant(id) else {
            return Ok(false);
        };
        let Some(row) = record.rows.iter().find(|row| row.variant_id == *id) else {
            return Ok(false);
        };
        if current.case_json != row.source_case_json
            || current.dataset_bundles != row.source_dataset_bundles
            || serde_json::to_value(current.options)? != serde_json::to_value(row.source_options)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn datasets_from_bundle_list(
    bundles: &[String],
) -> Result<BTreeMap<String, MaterialDataset>, RobustnessError> {
    let mut datasets = BTreeMap::new();
    for raw in bundles {
        let bundle = MaterialBundle::from_json(raw)
            .map_err(|e| RobustnessError::Invalid(format!("dataset bundle: {e}")))?;
        let id = bundle.dataset.metadata.id.clone();
        if datasets.insert(id.clone(), bundle.dataset).is_some() {
            return Err(RobustnessError::Invalid(format!(
                "duplicate dataset bundle id '{id}'"
            )));
        }
    }
    Ok(datasets)
}

fn active_bundle_refs<'a>(
    row: &'a RobustnessRunRecord,
    case: &CoupledSearchCase,
) -> Result<Vec<&'a str>, RobustnessError> {
    let bindings: BTreeSet<(String, String)> = case
        .material_bindings()
        .into_iter()
        .map(|(_, binding)| (binding.dataset_id.clone(), binding.csv_sha256.clone()))
        .collect();
    let mut refs = Vec::new();
    let mut found = BTreeSet::new();
    for raw in row
        .resolved_source_dataset_bundles
        .iter()
        .chain(&row.derived_dataset_bundles)
    {
        let bundle = MaterialBundle::from_json(raw)
            .map_err(|e| RobustnessError::Invalid(format!("run dataset bundle: {e}")))?;
        let identity = (
            bundle.dataset.metadata.id,
            bundle.dataset.metadata.csv_sha256,
        );
        if bindings.contains(&identity) {
            found.insert(identity);
            refs.push(raw.as_str());
        }
    }
    if found != bindings {
        return Err(RobustnessError::Invalid(
            "run's retained dataset bundles do not cover every transformed material binding".into(),
        ));
    }
    Ok(refs)
}

fn add_embedded_bindings(
    case: &CoupledSearchCase,
    datasets: &mut BTreeMap<String, MaterialDataset>,
) {
    for (_, binding) in case.material_bindings() {
        if !datasets.contains_key(&binding.dataset_id)
            && let Ok(dataset) = MaterialDataset::embedded_by_id(&binding.dataset_id)
        {
            datasets.insert(binding.dataset_id.clone(), dataset);
        }
    }
}

fn validate_request<'a>(
    workspace: &'a StudyWorkspace,
    variant_ids: &[String],
    spec: &RobustnessSpec,
) -> Result<Vec<&'a crate::study::StudyVariant>, RobustnessError> {
    workspace.validate()?;
    validate_spec(spec, variant_ids.len())?;
    if variant_ids.is_empty() || variant_ids.len() > MAX_ROBUSTNESS_VARIANTS {
        return Err(RobustnessError::Invalid(format!(
            "select 1..={MAX_ROBUSTNESS_VARIANTS} variants"
        )));
    }
    let mut selected = Vec::new();
    let mut unique = BTreeSet::new();
    for id in variant_ids {
        if !unique.insert(id) {
            return Err(RobustnessError::Invalid(format!(
                "duplicate variant id '{id}'"
            )));
        }
        selected.push(workspace.variant(id)?);
    }
    ensure_comparable(&selected)?;
    for scenario in &spec.scenarios {
        let mut known = BTreeSet::new();
        for variant in &selected {
            let case: CoupledSearchCase = serde_json::from_str(&variant.case_json)?;
            known.extend(
                case.material_bindings()
                    .into_iter()
                    .map(|(_, b)| b.dataset_id.clone()),
            );
        }
        for id in scenario
            .price_multipliers
            .keys()
            .chain(scenario.ic_multipliers.keys())
        {
            if !known.contains(id) {
                return Err(RobustnessError::Invalid(format!(
                    "scenario '{}' references dataset id '{id}' not bound by selected variants",
                    scenario.id
                )));
            }
        }
    }
    Ok(selected)
}

fn validate_spec(spec: &RobustnessSpec, variant_count: usize) -> Result<(), RobustnessError> {
    if spec.schema != ROBUSTNESS_SPEC_SCHEMA {
        return Err(RobustnessError::Invalid(format!(
            "unsupported robustness spec schema '{}'",
            spec.schema
        )));
    }
    if spec.scenarios.is_empty() || spec.scenarios.len() > MAX_ROBUSTNESS_SCENARIOS {
        return Err(RobustnessError::Invalid(format!(
            "declare 1..={MAX_ROBUSTNESS_SCENARIOS} scenarios"
        )));
    }
    if variant_count == 0
        || variant_count > MAX_ROBUSTNESS_VARIANTS
        || variant_count.saturating_mul(spec.scenarios.len()) > MAX_ROBUSTNESS_RUNS
    {
        return Err(RobustnessError::Invalid(format!(
            "select 1..={MAX_ROBUSTNESS_VARIANTS} variants and stay within {MAX_ROBUSTNESS_RUNS} total runs"
        )));
    }
    let mut scenario_ids = BTreeSet::new();
    for scenario in &spec.scenarios {
        if scenario.id.trim().is_empty()
            || scenario.id.len() > 80
            || !scenario_ids.insert(scenario.id.clone())
            || scenario.name.trim().is_empty()
            || scenario.name.len() > 200
            || !scenario.temperature_offset_k.is_finite()
            || scenario.temperature_offset_k.abs() > 400.0
        {
            return Err(RobustnessError::Invalid(
                "scenario ids/names must be nonempty, unique and bounded; temperature offset must be finite and within ±400 K".into(),
            ));
        }
        for (dataset_id, factor) in scenario
            .price_multipliers
            .iter()
            .chain(&scenario.ic_multipliers)
        {
            if dataset_id.trim().is_empty()
                || !factor.is_finite()
                || *factor <= 0.0
                || *factor > 1e6
            {
                return Err(RobustnessError::Invalid(format!(
                    "scenario '{}' has an invalid multiplier for dataset '{dataset_id}'",
                    scenario.id
                )));
            }
        }
    }
    if !spec.scenarios.iter().any(|scenario| {
        scenario.id == "nominal"
            && scenario.price_multipliers.values().all(|v| *v == 1.0)
            && scenario.ic_multipliers.values().all(|v| *v == 1.0)
            && scenario.temperature_offset_k == 0.0
    }) {
        return Err(RobustnessError::Invalid(
            "an explicit nominal scenario with unit multipliers and zero temperature offset is required".into(),
        ));
    }
    Ok(())
}

fn ensure_comparable(variants: &[&crate::study::StudyVariant]) -> Result<(), RobustnessError> {
    let first = CoupledSearchCase::from_json(&variants[0].case_json)
        .map_err(|e| RobustnessError::Invalid(format!("case parse: {e}")))?;
    let contract = comparable_contract(&first)?;
    for variant in &variants[1..] {
        let case = CoupledSearchCase::from_json(&variant.case_json)
            .map_err(|e| RobustnessError::Invalid(format!("case parse: {e}")))?;
        if comparable_contract(&case)? != contract {
            return Err(RobustnessError::Invalid(format!(
                "variant '{}' has different requirements, operating point, limits, numerics, sampling or refinement; objective comparison is invalid",
                variant.id
            )));
        }
    }
    Ok(())
}

fn comparable_contract(case: &CoupledSearchCase) -> Result<Value, serde_json::Error> {
    Ok(serde_json::json!({
        "requirement": case.requirement,
        "operating": case.operating,
        "limits": case.limits,
        "numerics": case.numerics,
        "sampling": case.sampling,
        "refined_plan": case.refined_plan,
        "manufacturing": case.manufacturing,
        "opex": case.opex,
    }))
}

type TransformedInputs = (String, BTreeMap<String, MaterialDataset>, Vec<String>);

fn transform_inputs(
    variant: &crate::study::StudyVariant,
    source_case: &CoupledSearchCase,
    scenario: &RobustnessScenario,
) -> Result<TransformedInputs, RobustnessError> {
    let mut case = source_case.clone();
    case.operating.temperature_k += scenario.temperature_offset_k;
    apply_price_multipliers(&mut case, scenario)?;
    let bindings: BTreeMap<String, String> = case
        .material_bindings()
        .into_iter()
        .map(|(_, binding)| (binding.dataset_id.clone(), binding.csv_sha256.clone()))
        .collect();
    let mut datasets = load_datasets(variant, source_case)?;
    for (dataset_id, expected_csv_hash) in &bindings {
        let source = datasets.get(dataset_id).ok_or_else(|| {
            RobustnessError::Invalid(format!("dataset '{dataset_id}' is unavailable"))
        })?;
        if &source.metadata.csv_sha256 != expected_csv_hash {
            return Err(RobustnessError::Invalid(format!(
                "source dataset '{dataset_id}' does not match the case-pinned CSV hash"
            )));
        }
    }
    // Re-key every affected binding consistently. When multiplier=1 the
    // identity and bytes are preserved exactly.
    let mut derived_bundles = Vec::new();
    let mut new_ids = BTreeMap::new();
    for (dataset_id, multiplier) in &scenario.ic_multipliers {
        if *multiplier == 1.0 {
            continue;
        }
        let source = datasets.get(dataset_id).ok_or_else(|| {
            RobustnessError::Invalid(format!("dataset '{dataset_id}' is unavailable"))
        })?;
        let mut derived = source
            .scaled_ic(*multiplier)
            .map_err(|e| RobustnessError::Invalid(e.to_string()))?;
        derived.metadata.schema = MATERIAL_SCHEMA_V2.into();
        derived.metadata.data_class = MaterialDataClass::SyntheticSensitivity;
        derived.metadata.normalization = format!(
            "synthetic robustness sensitivity transform: source dataset '{dataset_id}', Ic multiplier {multiplier}; source measurements are not relabeled as transformed measurements"
        );
        derived.metadata.limitations.push(format!(
            "Synthetic robustness what-if derived by multiplying Ic and bridge Ic in source dataset '{dataset_id}' by {multiplier}; these values are not measurements."
        ));
        let new_id = derived.metadata.id.clone();
        let json = dataset_bundle_json(&derived)?;
        derived_bundles.push(json);
        datasets.remove(dataset_id);
        datasets.insert(new_id.clone(), derived);
        new_ids.insert(dataset_id.clone(), new_id);
    }
    if let Some(new_id) = new_ids.get(&case.material.dataset_id) {
        case.material.dataset_id = new_id.clone();
        case.material.csv_sha256 = datasets[new_id].metadata.csv_sha256.clone();
    }
    if let Some(specs) = &mut case.tape_specs {
        for spec in specs.values_mut() {
            if let Some(new_id) = new_ids.get(&spec.material.dataset_id) {
                spec.material.dataset_id = new_id.clone();
                spec.material.csv_sha256 = datasets[new_id].metadata.csv_sha256.clone();
            }
        }
    }
    case.validate()
        .map_err(|e| RobustnessError::Invalid(e.to_string()))?;
    case.validate_against_dataset_map(&datasets)
        .map_err(|e| RobustnessError::Invalid(e.to_string()))?;
    let case_json = serde_json::to_string(&case)?;
    Ok((case_json, datasets, derived_bundles))
}

fn apply_price_multipliers(
    case: &mut CoupledSearchCase,
    scenario: &RobustnessScenario,
) -> Result<(), RobustnessError> {
    let mut base = BTreeMap::new();
    base.insert("base".to_owned(), case.material.dataset_id.clone());
    if let Some(specs) = &case.tape_specs {
        for (id, spec) in specs {
            base.insert(id.clone(), spec.material.dataset_id.clone());
        }
    }
    if let Some(multiplier) = scenario.price_multipliers.get(&case.material.dataset_id) {
        case.cost.price_usd_per_m = scale_price(case.cost.price_usd_per_m, *multiplier)?;
        if let Some(offerings) = &mut case.cost.piece_offerings {
            for offering in offerings {
                offering.price_usd_per_m = scale_price(offering.price_usd_per_m, *multiplier)?;
            }
        }
    }
    if let Some(specs) = &mut case.tape_specs {
        for (id, spec) in specs {
            if let Some(multiplier) = scenario.price_multipliers.get(&spec.material.dataset_id) {
                spec.price_usd_per_m = scale_price(spec.price_usd_per_m, *multiplier)?;
                if let Some(offerings) = &mut spec.piece_offerings {
                    for offering in offerings {
                        offering.price_usd_per_m =
                            scale_price(offering.price_usd_per_m, *multiplier)?;
                    }
                }
            }
            let _ = id;
        }
    }
    // Verify the binding map enumerated every declared dataset-specific
    // price surface; assembly, splice, joint and cooling terms are separate.
    let declared: BTreeSet<_> = case
        .material_bindings()
        .into_iter()
        .map(|(_, b)| b.dataset_id.clone())
        .collect();
    if base.values().any(|id| !declared.contains(id)) {
        return Err(RobustnessError::Invalid(
            "internal material price binding mismatch".into(),
        ));
    }
    Ok(())
}

fn scale_price(price: f64, multiplier: f64) -> Result<f64, RobustnessError> {
    let scaled = price * multiplier;
    if !scaled.is_finite() || scaled <= 0.0 || scaled > 1e12 {
        return Err(RobustnessError::Invalid(
            "price multiplier produces a price outside the supported finite positive range".into(),
        ));
    }
    Ok(scaled)
}

fn load_datasets(
    variant: &crate::study::StudyVariant,
    case: &CoupledSearchCase,
) -> Result<BTreeMap<String, MaterialDataset>, RobustnessError> {
    let mut out = BTreeMap::new();
    for bundle_json in &variant.dataset_bundles {
        let bundle = MaterialBundle::from_json(bundle_json)
            .map_err(|e| RobustnessError::Invalid(format!("dataset bundle: {e}")))?;
        let id = bundle.dataset.metadata.id.clone();
        if out.insert(id.clone(), bundle.dataset).is_some() {
            return Err(RobustnessError::Invalid(format!(
                "duplicate dataset bundle id '{id}'"
            )));
        }
    }
    for (_, binding) in case.material_bindings() {
        if !out.contains_key(&binding.dataset_id)
            && let Ok(dataset) = MaterialDataset::embedded_by_id(&binding.dataset_id)
        {
            out.insert(binding.dataset_id.clone(), dataset);
        }
    }
    Ok(out)
}

fn complete_source_bundles(
    variant: &crate::study::StudyVariant,
    case: &CoupledSearchCase,
) -> Result<Vec<String>, RobustnessError> {
    let mut bundles = variant.dataset_bundles.clone();
    let mut present = BTreeSet::new();
    for raw in &variant.dataset_bundles {
        let bundle = MaterialBundle::from_json(raw)
            .map_err(|e| RobustnessError::Invalid(format!("dataset bundle: {e}")))?;
        present.insert(bundle.dataset.metadata.id);
    }
    for (_, binding) in case.material_bindings() {
        if !present.contains(&binding.dataset_id)
            && let Ok(bundle) = MaterialDataset::embedded_bundle_json(&binding.dataset_id)
        {
            bundles.push(bundle);
            present.insert(binding.dataset_id.clone());
        }
    }
    Ok(bundles)
}

fn dataset_bundle_json(dataset: &MaterialDataset) -> Result<String, RobustnessError> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    for point in &dataset.points {
        writer
            .serialize(point)
            .map_err(|e| RobustnessError::Invalid(e.to_string()))?;
    }
    let csv_data = String::from_utf8(
        writer
            .into_inner()
            .map_err(|e| RobustnessError::Invalid(e.to_string()))?,
    )
    .map_err(|e| RobustnessError::Invalid(e.to_string()))?;
    let bundle = serde_json::json!({
        "schema": MATERIAL_DATASET_BUNDLE_SCHEMA,
        "metadata": dataset.metadata,
        "csv_data": csv_data,
    });
    Ok(serde_json::to_string_pretty(&bundle)?)
}

fn select_winner(run: &CoupledSearchRunRecord) -> Option<RobustnessWinner> {
    if run.search_status != Status::Pass || run.acceptance.agreement_status != Status::Pass {
        return None;
    }
    let accepted = run.acceptance.best.as_ref()?;
    if accepted.cost_agreement_status != Status::Pass
        || accepted.screens_agreement_status != Status::Pass
    {
        return None;
    }
    let candidate = run.candidates.get(accepted.index)?;
    if candidate.status != Status::Pass || accepted.index != run.best_index? {
        return None;
    }
    Some(RobustnessWinner {
        candidate_index: accepted.index,
        turns_along_normal: candidate.geometry.turns_along_normal,
        tapes_along_width: candidate.geometry.tapes_along_width,
        strands_parallel: candidate.geometry.strands_parallel,
        bend_radius_m: candidate.geometry.bend_radius_m,
        straight_half_length_m: candidate.geometry.straight_half_length_m,
        tape_spec_ids: candidate.geometry.tape_spec_ids.clone(),
        cost_usd: candidate.cost.total_usd,
    })
}

fn run_resolution(run: &CoupledSearchRunRecord, has_supported_winner: bool) -> RobustnessRunStatus {
    if has_supported_winner {
        return RobustnessRunStatus::Completed;
    }
    let conclusively_empty = run.search_status == Status::Fail
        && run.acceptance.agreement_status == Status::Pass
        && !run.candidates.is_empty()
        && run
            .candidates
            .iter()
            .all(|candidate| candidate.status == Status::Fail);
    if conclusively_empty {
        RobustnessRunStatus::NoWinner
    } else {
        RobustnessRunStatus::Unresolved
    }
}

fn mark_nominal_geometry_survival(
    rows: &mut [RobustnessRunRecord],
    scenarios: &[RobustnessScenario],
) {
    let _ = scenarios;
    for i in 0..rows.len() {
        if rows[i].scenario_id == "nominal" {
            rows[i].nominal_winner_geometry_survives = rows[i].winner.is_some().then_some(true);
            continue;
        }
        let nominal = rows
            .iter()
            .find(|r| r.variant_id == rows[i].variant_id && r.scenario_id == "nominal")
            .and_then(|r| r.winner.clone());
        let Some(nominal) = nominal else {
            rows[i].nominal_winner_geometry_survives = None;
            continue;
        };
        rows[i].nominal_winner_geometry_survives = rows[i]
            .run_record_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<CoupledSearchRunRecord>(json).ok())
            .and_then(|run| {
                let matching_statuses = run
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.geometry.turns_along_normal == nominal.turns_along_normal
                            && candidate.geometry.tapes_along_width == nominal.tapes_along_width
                            && candidate.geometry.strands_parallel == nominal.strands_parallel
                            && candidate.geometry.bend_radius_m == nominal.bend_radius_m
                            && candidate.geometry.straight_half_length_m
                                == nominal.straight_half_length_m
                            && candidate.geometry.tape_spec_ids == nominal.tape_spec_ids
                    })
                    .map(|candidate| candidate.status);
                geometry_eligibility(matching_statuses)
            });
    }
}

fn geometry_eligibility(statuses: impl Iterator<Item = Status>) -> Option<bool> {
    let statuses: Vec<_> = statuses.collect();
    if statuses.contains(&Status::Pass) {
        Some(true)
    } else if statuses.is_empty()
        || statuses
            .iter()
            .any(|status| matches!(status, Status::Inconclusive | Status::NotEvaluated))
    {
        None
    } else {
        Some(false)
    }
}

fn summarize_scenarios(
    rows: &[RobustnessRunRecord],
    scenarios: &[RobustnessScenario],
) -> Vec<RobustnessScenarioSummary> {
    scenarios
        .iter()
        .map(|scenario| {
            let scenario_rows: Vec<_> = rows
                .iter()
                .filter(|r| r.scenario_id == scenario.id)
                .collect();
            let mut winners: Vec<_> = scenario_rows
                .iter()
                .filter_map(|r| {
                    r.winner
                        .as_ref()
                        .map(|w| (r.variant_id.clone(), w.cost_usd))
                })
                .collect();
            winners.sort_by(|a, b| a.1.total_cmp(&b.1));
            let missing_variant_ids: Vec<_> = scenario_rows
                .iter()
                .filter(|r| {
                    matches!(
                        r.status,
                        RobustnessRunStatus::Failed | RobustnessRunStatus::Unresolved
                    )
                })
                .map(|r| r.variant_id.clone())
                .collect();
            let all_variants_resolved = missing_variant_ids.is_empty();
            let winner = all_variants_resolved.then(|| winners.first()).flatten();
            RobustnessScenarioSummary {
                scenario_id: scenario.id.clone(),
                winner_variant_id: winner.map(|(id, _)| id.clone()),
                winner_cost_usd: winner.map(|(_, cost)| *cost),
                resolved_cost_min_usd: all_variants_resolved
                    .then(|| winners.first().map(|(_, c)| *c))
                    .flatten(),
                resolved_cost_max_usd: all_variants_resolved
                    .then(|| winners.last().map(|(_, c)| *c))
                    .flatten(),
                possible_regret_usd: all_variants_resolved
                    .then(|| {
                        winners
                            .last()
                            .zip(winners.first())
                            .map(|(last, first)| last.1 - first.1)
                    })
                    .flatten(),
                all_variants_resolved,
                missing_variant_ids,
            }
        })
        .collect()
}

fn find_winner_switches(summaries: &[RobustnessScenarioSummary]) -> Vec<RobustnessWinnerSwitch> {
    let nominal = summaries
        .iter()
        .find(|s| s.scenario_id == "nominal")
        .and_then(|s| s.winner_variant_id.clone());
    let Some(from) = nominal else {
        return Vec::new();
    };
    summaries
        .iter()
        .filter_map(|summary| {
            let to = summary.winner_variant_id.as_ref()?;
            (to != &from).then(|| RobustnessWinnerSwitch {
                from_scenario_id: "nominal".into(),
                to_scenario_id: summary.scenario_id.clone(),
                from_variant_id: from.clone(),
                to_variant_id: to.clone(),
            })
        })
        .collect()
}

fn preflight_row_ready(pf: &RobustnessPreflight, variant_id: &str, scenario_id: &str) -> bool {
    pf.preflights
        .iter()
        .find(|r| r.variant_id == variant_id && r.scenario_id == scenario_id)
        .is_some_and(|r| r.ready_to_run)
}

fn preflight_row_error(
    pf: &RobustnessPreflight,
    variant_id: &str,
    scenario_id: &str,
) -> Option<String> {
    pf.preflights
        .iter()
        .find(|r| r.variant_id == variant_id && r.scenario_id == scenario_id)
        .map(|r| r.errors.join("; "))
}

fn source_fingerprint(
    variant: &crate::study::StudyVariant,
) -> Result<RobustnessSourceFingerprint, RobustnessError> {
    let case = CoupledSearchCase::from_json(&variant.case_json)
        .map_err(|e| RobustnessError::Invalid(format!("case parse: {e}")))?;
    let resolved = complete_source_bundles(variant, &case)?;
    Ok(RobustnessSourceFingerprint {
        variant_id: variant.id.clone(),
        case_sha256: hash(variant.case_json.as_bytes()),
        bundle_sha256: variant
            .dataset_bundles
            .iter()
            .map(|s| hash(s.as_bytes()))
            .collect(),
        resolved_bundle_sha256: resolved.iter().map(|raw| hash(raw.as_bytes())).collect(),
        options_sha256: hash(&serde_json::to_vec(&variant.options)?),
    })
}

fn engine_fingerprint() -> String {
    let model_lib = include_str!("../../optcoil-model/src/lib.rs");
    let material = include_str!("../../optcoil-model/src/material.rs");
    let model_search = include_str!("../../optcoil-model/src/coupled_search.rs");
    let physics = include_str!("../../optcoil-physics/src/racetrack.rs");
    let critical_current = include_str!("../../optcoil-physics/src/critical_current.rs");
    let tape_frame = include_str!("../../optcoil-physics/src/tape_frame.rs");
    let model_magnetics = include_str!("../../optcoil-model/src/magnetics.rs");
    let model_coupled = include_str!("../../optcoil-model/src/coupled.rs");
    let search_lib = include_str!("lib.rs");
    let search_coupled = include_str!("coupled.rs");
    let implementation = include_str!("coupled_search.rs");
    let acceptance = include_str!("search_acceptance.rs");
    let robustness = include_str!("robustness.rs");
    let lockfile = include_str!("../../../Cargo.lock");
    let toolchain = include_str!("../../../rust-toolchain.toml");
    hash(
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            env!("CARGO_PKG_VERSION"),
            coupled_search::COUPLED_SEARCH_MODEL_ID,
            coupled_search::COUPLED_SEARCH_CHECKER_ID,
            model_lib,
            model_magnetics,
            material,
            model_coupled,
            model_search,
            physics,
            critical_current,
            tape_frame,
            search_lib,
            search_coupled,
            implementation,
            format_args!("{acceptance}:{robustness}:{lockfile}:{toolchain}")
        )
        .as_bytes(),
    )
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct BoundedCounter {
    bytes: usize,
    limit: usize,
}

impl Write for BoundedCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "serialized payload exceeded byte cap",
            ));
        }
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn count_serialized_size(value: &impl Serialize, limit: usize) -> Option<usize> {
    let mut counter = BoundedCounter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    use crate::study::StudyWorkspace;

    fn nominal_spec() -> RobustnessSpec {
        RobustnessSpec {
            schema: ROBUSTNESS_SPEC_SCHEMA.into(),
            scenarios: vec![RobustnessScenario::nominal()],
        }
    }

    fn workspace(case: &str) -> (StudyWorkspace, String) {
        let mut workspace = StudyWorkspace::new("robustness tests");
        let id = workspace
            .add_variant(
                "candidate",
                case.to_owned(),
                Vec::new(),
                CoupledSearchOptions::default(),
            )
            .unwrap();
        (workspace, id)
    }

    #[test]
    fn graded_and_piece_catalogue_prices_scale_by_bound_dataset() {
        let mut graded: CoupledSearchCase =
            serde_json::from_str(optcoil_model::coupled_search::OC020_JSON).unwrap();
        let base_dataset_id = graded.material.dataset_id.clone();
        let lowfield_id = graded.tape_specs.as_ref().unwrap()["hts-lowfield"]
            .material
            .dataset_id
            .clone();
        let original_base = graded.cost.price_usd_per_m;
        let original_spec = graded.tape_specs.as_ref().unwrap()["hts-lowfield"].price_usd_per_m;
        let scenario = RobustnessScenario {
            id: "supplier_change".into(),
            name: "supplier rates".into(),
            price_multipliers: BTreeMap::from([(base_dataset_id, 1.2), (lowfield_id, 0.5)]),
            ic_multipliers: BTreeMap::new(),
            temperature_offset_k: 0.0,
        };
        apply_price_multipliers(&mut graded, &scenario).unwrap();
        assert_eq!(graded.cost.price_usd_per_m, original_base * 1.2);
        assert_eq!(
            graded.tape_specs.as_ref().unwrap()["hts-lowfield"].price_usd_per_m,
            original_spec * 0.5
        );

        let mut pieces: CoupledSearchCase = serde_json::from_str(include_str!(
            "../../../benchmarks/coupled/oc-027-tfmc-pieces.json"
        ))
        .unwrap();
        let dataset_id = pieces.material.dataset_id.clone();
        let original_rate = pieces.cost.price_usd_per_m;
        let original_offerings = pieces.cost.piece_offerings.clone().unwrap();
        let piece_scenario = RobustnessScenario {
            id: "quoted".into(),
            name: "changed offering rates".into(),
            price_multipliers: BTreeMap::from([(dataset_id, 1.5)]),
            ic_multipliers: BTreeMap::new(),
            temperature_offset_k: 0.0,
        };
        apply_price_multipliers(&mut pieces, &piece_scenario).unwrap();
        assert_eq!(pieces.cost.price_usd_per_m, original_rate * 1.5);
        for (actual, original) in pieces
            .cost
            .piece_offerings
            .as_ref()
            .unwrap()
            .iter()
            .zip(original_offerings)
        {
            assert_eq!(actual.length_m, original.length_m);
            assert_eq!(actual.price_usd_per_m, original.price_usd_per_m * 1.5);
        }
        assert_eq!(pieces.cost.piece_policy.unwrap().splice_cost_usd, 500.0);
    }

    #[test]
    fn ic_scenario_is_synthetic_hash_bound_and_keeps_nominal_identity() {
        let case_json = optcoil_model::coupled_search::OC019_JSON;
        let source_case = CoupledSearchCase::from_json(case_json).unwrap();
        let variant = crate::study::StudyVariant {
            id: "variant-0001".into(),
            name: "source".into(),
            case_json: case_json.into(),
            dataset_bundles: Vec::new(),
            options: CoupledSearchOptions::default(),
            results: Vec::new(),
        };
        let nominal = RobustnessScenario::nominal();
        let (_, nominal_data, nominal_bundles) =
            transform_inputs(&variant, &source_case, &nominal).unwrap();
        assert!(nominal_bundles.is_empty());
        assert!(nominal_data.contains_key(&source_case.material.dataset_id));
        assert_eq!(source_case.material.dataset_id, "robinson-superpower-ap-v3");
        let source_bundles = complete_source_bundles(&variant, &source_case).unwrap();
        assert_eq!(source_bundles.len(), 1);
        let retained_source = MaterialBundle::from_json(&source_bundles[0]).unwrap();
        assert_eq!(
            retained_source.dataset.metadata.id,
            source_case.material.dataset_id
        );
        assert_eq!(
            retained_source.dataset.metadata.csv_sha256,
            source_case.material.csv_sha256
        );

        let transformed = RobustnessScenario {
            id: "lower_ic".into(),
            name: "20 percent lower Ic".into(),
            price_multipliers: BTreeMap::from([(source_case.material.dataset_id.clone(), 1.25)]),
            ic_multipliers: BTreeMap::from([(source_case.material.dataset_id.clone(), 0.8)]),
            temperature_offset_k: 0.0,
        };
        let (case_json, datasets, bundles) =
            transform_inputs(&variant, &source_case, &transformed).unwrap();
        let derived_case = CoupledSearchCase::from_json(&case_json).unwrap();
        let binding = &derived_case.material;
        let derived = &datasets[&binding.dataset_id];
        assert_ne!(binding.dataset_id, source_case.material.dataset_id);
        assert_eq!(binding.dataset_id, derived.metadata.id);
        assert_eq!(binding.csv_sha256, derived.metadata.csv_sha256);
        assert_eq!(derived_case.cost.price_usd_per_m, 37.5);
        assert!(matches!(
            derived.metadata.data_class,
            MaterialDataClass::SyntheticSensitivity
        ));
        assert!(
            derived
                .metadata
                .normalization
                .contains("sensitivity transform")
        );
        assert_eq!(bundles.len(), 1);
        let bundle: MaterialBundle = MaterialBundle::from_json(&bundles[0]).unwrap();
        assert!(bundle.attestation.is_none());
        assert!(matches!(
            bundle.dataset.metadata.data_class,
            MaterialDataClass::SyntheticSensitivity
        ));
        let source = MaterialDataset::embedded_by_id(&source_case.material.dataset_id).unwrap();
        assert_eq!(source.metadata.id, source_case.material.dataset_id);
        assert!(source.metadata.csv_sha256 != derived.metadata.csv_sha256);
    }

    #[test]
    fn scenario_winner_switch_and_missing_winner_are_explicit() {
        let scenarios = vec![
            RobustnessScenario::nominal(),
            RobustnessScenario {
                id: "price_shift".into(),
                name: "changed supplier rates".into(),
                price_multipliers: BTreeMap::new(),
                ic_multipliers: BTreeMap::new(),
                temperature_offset_k: 0.0,
            },
            RobustnessScenario {
                id: "unresolved".into(),
                name: "unsupported operating point".into(),
                price_multipliers: BTreeMap::new(),
                ic_multipliers: BTreeMap::new(),
                temperature_offset_k: 0.0,
            },
            RobustnessScenario {
                id: "bounded_fail".into(),
                name: "one alternative fails every bounded candidate".into(),
                price_multipliers: BTreeMap::new(),
                ic_multipliers: BTreeMap::new(),
                temperature_offset_k: 0.0,
            },
        ];
        let rows = vec![
            result_row("a", "nominal", Some(120.0)),
            result_row("b", "nominal", Some(100.0)),
            result_row("a", "price_shift", Some(300.0)),
            result_row("b", "price_shift", Some(200.0)),
            {
                let mut row = result_row("a", "unresolved", None);
                row.status = RobustnessRunStatus::Unresolved;
                row
            },
            {
                let mut row = result_row("b", "unresolved", None);
                row.status = RobustnessRunStatus::Unresolved;
                row
            },
            result_row("a", "bounded_fail", Some(80.0)),
            result_row("b", "bounded_fail", None),
        ];
        let summary = summarize_scenarios(&rows, &scenarios);
        let switches = find_winner_switches(&summary);
        assert_eq!(summary[0].winner_variant_id.as_deref(), Some("b"));
        assert_eq!(summary[1].winner_variant_id.as_deref(), Some("b"));
        assert!(summary[2].winner_variant_id.is_none());
        assert!(!summary[2].all_variants_resolved);
        assert_eq!(summary[2].possible_regret_usd, None);
        assert_eq!(summary[3].winner_variant_id.as_deref(), Some("a"));
        assert!(summary[3].all_variants_resolved);
        assert!(summary[3].missing_variant_ids.is_empty());
        assert_eq!(switches.len(), 1);
        assert_eq!(switches[0].to_scenario_id, "bounded_fail");

        let mut switched = rows;
        switched[2].winner.as_mut().unwrap().cost_usd = 80.0;
        let switched_summary = summarize_scenarios(&switched, &scenarios);
        let switches = find_winner_switches(&switched_summary);
        assert_eq!(switches.len(), 2);
        assert!(
            switches
                .iter()
                .all(|switch| switch.from_variant_id == "b" && switch.to_variant_id == "a")
        );
    }

    #[test]
    fn geometry_survival_is_tri_state_for_missing_or_unresolved_candidate() {
        assert_eq!(
            geometry_eligibility([Status::Fail].into_iter()),
            Some(false)
        );
        assert_eq!(
            geometry_eligibility([Status::Inconclusive, Status::Fail].into_iter()),
            None
        );
        assert_eq!(geometry_eligibility(std::iter::empty()), None);
        assert_eq!(geometry_eligibility([Status::Pass].into_iter()), Some(true));
    }

    #[test]
    fn rejects_invalid_scenario_and_incomparable_requirements() {
        let (mut workspace, first) = workspace(optcoil_model::coupled_search::OC019_JSON);
        let second = workspace.duplicate_variant(&first, "second").unwrap();
        let ids = vec![first, second.clone()];
        let mut invalid = nominal_spec();
        invalid.scenarios[0].temperature_offset_k = f64::NAN;
        assert!(validate_request(&workspace, &ids, &invalid).is_err());

        let changed =
            serde_json::from_str::<Value>(optcoil_model::coupled_search::OC019_JSON).unwrap();
        let mut changed = changed;
        changed["requirement"]["b_target_t"] = Value::from(0.051);
        workspace
            .revise_variant(
                &second,
                serde_json::to_string(&changed).unwrap(),
                Vec::new(),
                CoupledSearchOptions::default(),
            )
            .unwrap();
        assert!(validate_request(&workspace, &ids, &nominal_spec()).is_err());
    }

    #[test]
    fn cancellation_returns_no_partial_robustness_record() {
        let (workspace, id) = workspace(optcoil_model::coupled_search::OC019_JSON);
        let cancelled = AtomicBool::new(true);
        let result = run_study_robustness(&workspace, &[id], &nominal_spec(), &cancelled, None);
        assert!(matches!(
            result,
            Err(RobustnessError::Run(RunError::Cancelled))
        ));
    }

    #[test]
    fn repeated_source_bytes_are_capped_before_scenario_preflight_work() {
        let mut spec = nominal_spec();
        for index in 1..MAX_ROBUSTNESS_SCENARIOS {
            spec.scenarios.push(RobustnessScenario {
                id: format!("case-{index}"),
                name: format!("what-if {index}"),
                price_multipliers: BTreeMap::new(),
                ic_multipliers: BTreeMap::new(),
                temperature_offset_k: 0.0,
            });
        }
        let (small, small_id) = workspace(optcoil_model::coupled_search::OC019_JSON);
        let small_pf = robustness_preflight(&small, &[small_id], &spec).unwrap();
        assert_eq!(small_pf.run_count, MAX_ROBUSTNESS_SCENARIOS);

        let padded_case = format!(
            "{}{}",
            optcoil_model::coupled_search::OC019_JSON,
            " ".repeat(3_200_000)
        );
        let (large, large_id) = workspace(&padded_case);
        let error = robustness_preflight(&large, &[large_id], &spec).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("repeated robustness source payload")
        );
    }

    #[test]
    fn retained_record_reopens_and_rejects_run_winner_and_summary_tampering() {
        let mut source: Value =
            serde_json::from_str(optcoil_model::coupled_search::OC019_JSON).unwrap();
        source["choices"]["turns_along_normal"] = serde_json::json!([200]);
        source["choices"]["tapes_along_width"] = serde_json::json!([3]);
        let source_json = serde_json::to_string(&source).unwrap();
        let (workspace, id) = workspace(&source_json);
        let record = run_study_robustness(
            &workspace,
            std::slice::from_ref(&id),
            &nominal_spec(),
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        validate_robustness_record(&record).unwrap();
        assert!(robustness_record_binding_is_current(&workspace, &record).unwrap());

        let mut forged_winner = record.clone();
        if let Some(winner) = forged_winner.rows[0].winner.as_mut() {
            winner.cost_usd += 1.0;
        } else {
            forged_winner.rows[0].winner = Some(RobustnessWinner {
                candidate_index: 0,
                turns_along_normal: 200,
                tapes_along_width: 3,
                strands_parallel: 1,
                bend_radius_m: None,
                straight_half_length_m: None,
                tape_spec_ids: None,
                cost_usd: 1.0,
            });
        }
        assert!(validate_robustness_record(&forged_winner).is_err());

        let mut forged_run = record.clone();
        let mut run: Value =
            serde_json::from_str(forged_run.rows[0].run_record_json.as_ref().unwrap()).unwrap();
        run["input_sha256"] = Value::String("0".repeat(64));
        forged_run.rows[0].run_record_json = Some(serde_json::to_string(&run).unwrap());
        assert!(validate_robustness_record(&forged_run).is_err());

        let mut forged_summary = record.clone();
        forged_summary.scenario_summaries[0].winner_cost_usd = Some(0.0);
        assert!(validate_robustness_record(&forged_summary).is_err());

        let mut changed_workspace = workspace.clone();
        let mut revised: Value = serde_json::from_str(&source_json).unwrap();
        revised["cost"]["price_usd_per_m"] = Value::from(31.0);
        changed_workspace
            .revise_variant(
                &id,
                serde_json::to_string(&revised).unwrap(),
                Vec::new(),
                CoupledSearchOptions::default(),
            )
            .unwrap();
        assert!(!robustness_record_binding_is_current(&changed_workspace, &record).unwrap());
    }

    fn result_row(variant_id: &str, scenario_id: &str, cost: Option<f64>) -> RobustnessRunRecord {
        RobustnessRunRecord {
            variant_id: variant_id.into(),
            variant_name: variant_id.into(),
            scenario_id: scenario_id.into(),
            scenario_name: scenario_id.into(),
            source_case_json: String::new(),
            source_dataset_bundles: Vec::new(),
            source_options: CoupledSearchOptions::default(),
            resolved_source_dataset_bundles: Vec::new(),
            transformed_case_json: None,
            derived_dataset_bundles: Vec::new(),
            run_record_json: None,
            status: if cost.is_some() {
                RobustnessRunStatus::Completed
            } else {
                RobustnessRunStatus::NoWinner
            },
            error: None,
            winner: cost.map(|cost_usd| RobustnessWinner {
                candidate_index: 0,
                turns_along_normal: 12,
                tapes_along_width: 2,
                strands_parallel: 1,
                bend_radius_m: Some(0.2),
                straight_half_length_m: Some(0.3),
                tape_spec_ids: None,
                cost_usd,
            }),
            nominal_winner_geometry_survives: None,
        }
    }
}
