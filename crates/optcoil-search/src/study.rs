//! Portable, headless workspace for named coupled-search engineering studies.
//!
//! A workspace is an organization and review format. Its records remain
//! evidence only after the normal runner and verifier have accepted them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;

use optcoil_model::{
    Status,
    coupled_search::CoupledSearchCase,
    material::{MaterialBundle, MaterialDataset},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    RunError,
    coupled_search::{self, CoupledSearchOptions, CoupledSearchRunRecord, SearchProgress},
    preflight::{StudyPreflight, preflight_coupled_search_with_options},
    review::{self, DecisionSummary, InputChange},
};

pub const STUDY_WORKSPACE_SCHEMA: &str = "optcoil-study-workspace/v1";
pub const MAX_STUDY_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_STUDY_VARIANTS: usize = 64;
pub const MAX_STUDY_RESULTS_PER_VARIANT: usize = 128;
pub const MAX_STUDY_CASE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_STUDY_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STUDY_RESULT_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_STUDY_LIVE_CACHE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_STUDY_LIVE_CACHE_ENTRIES: usize = 64;

#[derive(Debug, Error)]
pub enum StudyError {
    #[error("invalid study workspace: {0}")]
    Invalid(String),
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyWorkspace {
    pub schema: String,
    pub name: String,
    pub selected_variant_id: Option<String>,
    pub variants: Vec<StudyVariant>,
    #[serde(default)]
    next_variant_number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyVariant {
    pub id: String,
    pub name: String,
    /// Exact UTF-8 source bytes used to establish case identity.
    pub case_json: String,
    /// Exact UTF-8 bundle bytes; registry identities may pin the whole file.
    pub dataset_bundles: Vec<String>,
    pub options: CoupledSearchOptions,
    pub results: Vec<StudyResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyResult {
    pub id: String,
    pub attached_unix_ms: u64,
    /// Exact completed run-record JSON bytes.
    pub record_json: String,
    pub case_sha256: String,
    pub input_sha256: String,
    pub exact_input_key: String,
    /// Source bindings retained so the record remains independently reopenable
    /// after its parent variant is revised.
    pub source_case_json: String,
    pub source_dataset_bundles: Vec<String>,
    pub source_options: CoupledSearchOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyIssue {
    pub code: String,
    pub status: Status,
    pub message: String,
    pub input_pointer: Option<String>,
    pub observed_limit: Option<String>,
    pub data_gap: Option<String>,
    pub candidate_index: Option<usize>,
    pub limiting_location: Option<String>,
    pub observed_value: Option<f64>,
    pub declared_limit: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyDiagnosis {
    pub schema: String,
    pub issues: Vec<StudyIssue>,
    pub follow_up_axes: Vec<FollowUpAxis>,
    pub engineering_acceptance_claim: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpAxis {
    TapesAlongWidth,
    StrandsParallel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowUpExperiment {
    pub schema: String,
    pub source_variant_id: String,
    pub axis: FollowUpAxis,
    pub proposed_case_json: String,
    pub changed_inputs: Vec<InputChange>,
    /// Readiness and workload for this exact proposed input; no search runs
    /// until a caller explicitly accepts the case and starts it.
    pub preflight: StudyPreflight,
    pub rationale: String,
    pub calculated: bool,
    pub requirements_changed: bool,
    pub numerical_gates_changed: bool,
    pub limits_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudySummary {
    pub schema: String,
    pub variant_id: String,
    pub variant_name: String,
    pub case_sha256: String,
    pub dataset_identities: Vec<DatasetIdentitySummary>,
    pub result_count: usize,
    pub current_binding_result_ids: Vec<String>,
    #[serde(default)]
    pub latest_search_status: Option<Status>,
    pub latest_decision: Option<DecisionSummary>,
    pub diagnosis: StudyDiagnosis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetIdentitySummary {
    pub dataset_id: String,
    pub csv_sha256: String,
    pub bundle_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyDiff {
    pub schema: String,
    pub left_variant_id: String,
    pub right_variant_id: String,
    pub changes: Vec<InputChange>,
    pub result_comparison: Option<StudyResultComparison>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyResultComparison {
    pub left_status: Status,
    pub right_status: Status,
    pub left_selected_status: Status,
    pub right_selected_status: Status,
    pub left_selected_candidate_index: Option<usize>,
    pub right_selected_candidate_index: Option<usize>,
    pub left_cost_usd: Option<f64>,
    pub right_cost_usd: Option<f64>,
    pub cost_change_usd: Option<f64>,
    pub left_utilization: Option<f64>,
    pub right_utilization: Option<f64>,
    pub utilization_change: Option<f64>,
    /// Headroom to the selected case's utilization limit (`limit - usage`).
    pub left_utilization_headroom: Option<f64>,
    pub right_utilization_headroom: Option<f64>,
    pub utilization_headroom_change: Option<f64>,
    pub left_limiting_location: Option<String>,
    pub right_limiting_location: Option<String>,
}

/// Serialized review artifacts from the existing portable review builder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPackage {
    pub artifacts: Vec<(String, String)>,
}

impl StudyWorkspace {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: STUDY_WORKSPACE_SCHEMA.into(),
            name: name.into(),
            selected_variant_id: None,
            variants: Vec::new(),
            next_variant_number: 1,
        }
    }

    pub fn from_json(json: &str) -> Result<Self, StudyError> {
        if json.len() > MAX_STUDY_WORKSPACE_BYTES {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        let workspace: Self = serde_json::from_str(json)?;
        workspace.validate()?;
        Ok(workspace)
    }

    pub fn to_json(&self) -> Result<String, StudyError> {
        self.validate()?;
        let json = serde_json::to_string_pretty(self)?;
        if json.len() > MAX_STUDY_WORKSPACE_BYTES {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        Ok(json)
    }

    pub fn validate(&self) -> Result<(), StudyError> {
        if self.schema != STUDY_WORKSPACE_SCHEMA {
            return Err(StudyError::Invalid(format!(
                "unsupported schema '{}'",
                self.schema
            )));
        }
        if self.name.trim().is_empty() || self.name.len() > 200 {
            return Err(StudyError::Invalid(
                "workspace name must contain 1–200 bytes".into(),
            ));
        }
        if self.next_variant_number == 0 {
            return Err(StudyError::Invalid(
                "next_variant_number must be positive".into(),
            ));
        }
        if self.variants.len() > MAX_STUDY_VARIANTS {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 variant limit".into(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut payload_bytes = 0_usize;
        for variant in &self.variants {
            if !ids.insert(variant.id.as_str()) {
                return Err(StudyError::Invalid(format!(
                    "duplicate variant id '{}'",
                    variant.id
                )));
            }
            validate_variant(variant)?;
            payload_bytes = payload_bytes.saturating_add(variant_payload_bytes(variant));
        }
        if payload_bytes > MAX_STUDY_WORKSPACE_BYTES {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        if self
            .selected_variant_id
            .as_ref()
            .is_some_and(|id| !ids.contains(id.as_str()))
        {
            return Err(StudyError::Invalid(
                "selected_variant_id does not name a variant".into(),
            ));
        }
        Ok(())
    }

    pub fn add_variant(
        &mut self,
        name: impl Into<String>,
        case_json: String,
        dataset_bundles: Vec<String>,
        options: CoupledSearchOptions,
    ) -> Result<String, StudyError> {
        if self.variants.len() >= MAX_STUDY_VARIANTS {
            return Err(StudyError::Invalid(
                "workspace already has 64 variants".into(),
            ));
        }
        let number = self.next_variant_number;
        let following_number = number
            .checked_add(1)
            .ok_or_else(|| StudyError::Invalid("variant id sequence exhausted".into()))?;
        let id = format!("variant-{number:04}");
        let variant = StudyVariant {
            id: id.clone(),
            name: name.into(),
            case_json,
            dataset_bundles,
            options,
            results: Vec::new(),
        };
        validate_variant(&variant)?;
        if self
            .variants
            .iter()
            .map(variant_payload_bytes)
            .sum::<usize>()
            .saturating_add(variant_payload_bytes(&variant))
            > MAX_STUDY_WORKSPACE_BYTES
        {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        self.variants.push(variant);
        self.next_variant_number = following_number;
        self.selected_variant_id = Some(id.clone());
        Ok(id)
    }

    pub fn duplicate_variant(
        &mut self,
        source_id: &str,
        name: impl Into<String>,
    ) -> Result<String, StudyError> {
        let source = self.variant(source_id)?.clone();
        let id = self.add_variant(
            name,
            source.case_json,
            source.dataset_bundles,
            source.options,
        )?;
        Ok(id)
    }

    pub fn revise_variant(
        &mut self,
        id: &str,
        case_json: String,
        dataset_bundles: Vec<String>,
        options: CoupledSearchOptions,
    ) -> Result<(), StudyError> {
        let old = self.variant(id)?.clone();
        let updated = StudyVariant {
            id: id.into(),
            name: old.name.clone(),
            case_json,
            dataset_bundles,
            options,
            results: old.results.clone(),
        };
        validate_variant(&updated)?;
        let projected = self
            .variants
            .iter()
            .map(variant_payload_bytes)
            .sum::<usize>()
            .saturating_sub(variant_payload_bytes(&old))
            .saturating_add(variant_payload_bytes(&updated));
        if projected > MAX_STUDY_WORKSPACE_BYTES {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        *self.variant_mut(id)? = updated;
        Ok(())
    }

    pub fn rename_variant(&mut self, id: &str, name: impl Into<String>) -> Result<(), StudyError> {
        let name = name.into();
        if name.trim().is_empty() || name.len() > 200 {
            return Err(StudyError::Invalid(
                "variant name must contain 1–200 bytes".into(),
            ));
        }
        self.variant_mut(id)?.name = name;
        Ok(())
    }

    pub fn select_variant(&mut self, id: &str) -> Result<(), StudyError> {
        self.variant(id)?;
        self.selected_variant_id = Some(id.into());
        Ok(())
    }

    pub fn variant(&self, id: &str) -> Result<&StudyVariant, StudyError> {
        self.variants
            .iter()
            .find(|v| v.id == id)
            .ok_or_else(|| StudyError::Invalid(format!("unknown variant '{id}'")))
    }
    pub fn variant_mut(&mut self, id: &str) -> Result<&mut StudyVariant, StudyError> {
        self.variants
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or_else(|| StudyError::Invalid(format!("unknown variant '{id}'")))
    }

    pub fn preflight_variant(&self, id: &str) -> Result<StudyPreflight, StudyError> {
        let variant = self.variant(id)?;
        let case: CoupledSearchCase = serde_json::from_str(&variant.case_json)
            .map_err(|e| StudyError::Invalid(format!("case parse: {e}")))?;
        let datasets = datasets_for_case(&variant.case_json, &variant.dataset_bundles)?;
        Ok(preflight_coupled_search_with_options(
            &case,
            Some(&datasets),
            &variant.options,
        ))
    }

    pub fn diagnose_variant(&self, id: &str) -> Result<StudyDiagnosis, StudyError> {
        let preflight = self.preflight_variant(id)?;
        let variant = self.variant(id)?;
        let mut issues = preflight
            .items
            .iter()
            .map(|item| {
                let status = if matches!(item.level, crate::preflight::PreflightLevel::Error) {
                    Status::Inconclusive
                } else {
                    Status::NotEvaluated
                };
                let lower = item.message.to_lowercase();
                StudyIssue {
                    code: item.code.clone(),
                    status,
                    message: item.message.clone(),
                    input_pointer: issue_pointer(&item.code),
                    observed_limit: observed_limit(&item.message),
                    data_gap: (lower.contains("dataset") || lower.contains("coverage"))
                        .then_some(item.message.clone()),
                    candidate_index: None,
                    limiting_location: None,
                    observed_value: None,
                    declared_limit: None,
                    unit: None,
                }
            })
            .collect::<Vec<_>>();
        if let Some(result) = self.current_results(variant)?.last()
            && let Ok(record) = serde_json::from_str::<CoupledSearchRunRecord>(&result.record_json)
        {
            issues.extend(record_issues(&record));
        }
        let follow_up_axes = [FollowUpAxis::TapesAlongWidth, FollowUpAxis::StrandsParallel]
            .into_iter()
            .filter(|axis| self.propose_followup(id, *axis).is_ok())
            .collect();
        Ok(StudyDiagnosis {
            schema: "optcoil-study-diagnosis/v1".into(),
            issues,
            follow_up_axes,
            engineering_acceptance_claim: false,
        })
    }

    pub fn attach_result(&mut self, id: &str, record_json: String) -> Result<String, StudyError> {
        if record_json.len() > MAX_STUDY_RESULT_BYTES {
            return Err(StudyError::Invalid(
                "run record exceeds 24 MiB limit".into(),
            ));
        }
        let variant = self.variant(id)?;
        let record: CoupledSearchRunRecord = serde_json::from_str(&record_json)
            .map_err(|e| StudyError::Invalid(format!("record parse: {e}")))?;
        let case_hash = sha256(variant.case_json.as_bytes());
        if strip_sha(&record.case_sha256) != case_hash {
            return Err(StudyError::Invalid(
                "result case_sha256 does not match exact variant case bytes".into(),
            ));
        }
        let parsed_case: CoupledSearchCase = serde_json::from_str(&variant.case_json)
            .map_err(|e| StudyError::Invalid(format!("case parse: {e}")))?;
        if serde_json::to_value(&record.case)? != serde_json::to_value(&parsed_case)?
            || record.case_sha256 != case_hash
        {
            return Err(StudyError::Invalid(
                "record case or byte identity differs from variant".into(),
            ));
        }
        if record.coupled_search_model_id != crate::coupled_search::COUPLED_SEARCH_MODEL_ID
            || record.coupled_search_checker_id != crate::coupled_search::COUPLED_SEARCH_CHECKER_ID
        {
            return Err(StudyError::Invalid(
                "result model/checker identity is stale".into(),
            ));
        }
        let dataset_jsons: Vec<&str> = variant.dataset_bundles.iter().map(String::as_str).collect();
        let checks = crate::verify::verify_record_checks(
            &record_json,
            Some(&variant.case_json),
            None,
            None,
            &dataset_jsons,
        )?;
        if checks
            .iter()
            .any(|c| c.outcome == crate::verify::Outcome::Fail)
        {
            return Err(StudyError::Invalid(
                "record verifier rejected one or more bindings or ledgers".into(),
            ));
        }
        let datasets = datasets_for_case(&variant.case_json, &variant.dataset_bundles)?;
        parsed_case
            .validate_against_dataset_map(&datasets)
            .map_err(|e| StudyError::Invalid(format!("dataset binding validation: {e}")))?;
        let expected_threads = variant
            .options
            .threads
            .unwrap_or(parsed_case.execution.max_threads);
        if record.runtime.execution_threads != expected_threads {
            return Err(StudyError::Invalid(
                "result execution thread option differs from variant".into(),
            ));
        }
        if record.input_sha256 != expected_run_input_sha(&record, &variant.options)? {
            return Err(StudyError::Invalid(
                "result input_sha256 does not match the exact variant options and recorded implementation identities".into(),
            ));
        }
        if self.variant(id)?.results.len() >= MAX_STUDY_RESULTS_PER_VARIANT {
            return Err(StudyError::Invalid(
                "variant already has 128 results".into(),
            ));
        }
        let key = exact_input_key(variant)?;
        let result_id = format!("result-{}", &sha256(record_json.as_bytes())[..16]);
        let result = StudyResult {
            id: result_id.clone(),
            attached_unix_ms: now_ms(),
            record_json,
            case_sha256: case_hash,
            input_sha256: record.input_sha256.clone(),
            exact_input_key: key,
            source_case_json: variant.case_json.clone(),
            source_dataset_bundles: variant.dataset_bundles.clone(),
            source_options: variant.options,
        };
        let current_size = self
            .variants
            .iter()
            .map(variant_payload_bytes)
            .sum::<usize>();
        let old_variant_size = variant_payload_bytes(variant);
        let projected = current_size
            .saturating_sub(old_variant_size)
            .saturating_add(old_variant_size)
            .saturating_add(result.record_json.len())
            .saturating_add(result.source_case_json.len())
            .saturating_add(
                result
                    .source_dataset_bundles
                    .iter()
                    .map(String::len)
                    .sum::<usize>(),
            );
        if projected > MAX_STUDY_WORKSPACE_BYTES {
            return Err(StudyError::Invalid(
                "workspace exceeds 64 MiB payload limit".into(),
            ));
        }
        let v = self.variant_mut(id)?;
        if v.results.iter().any(|r| r.id == result_id) {
            return Err(StudyError::Invalid(
                "identical result is already attached".into(),
            ));
        }
        v.results.push(result);
        Ok(result_id)
    }

    pub fn compact_summary(&self, id: &str) -> Result<StudySummary, StudyError> {
        let variant = self.variant(id)?;
        let identities = bundle_summaries(&variant.dataset_bundles)?;
        let current = self.current_results(variant)?;
        let latest_decision = current
            .last()
            .map(|r| review::decision_summary(&r.record_json))
            .transpose()?;
        let latest_search_status = current
            .last()
            .map(|r| {
                serde_json::from_str::<CoupledSearchRunRecord>(&r.record_json)
                    .map(|record| record.search_status)
            })
            .transpose()?;
        Ok(StudySummary {
            schema: "optcoil-study-summary/v1".into(),
            variant_id: id.into(),
            variant_name: variant.name.clone(),
            case_sha256: sha256(variant.case_json.as_bytes()),
            dataset_identities: identities,
            result_count: variant.results.len(),
            current_binding_result_ids: current.iter().map(|r| r.id.clone()).collect(),
            latest_search_status,
            latest_decision,
            diagnosis: self.diagnose_variant(id)?,
        })
    }

    pub fn case_diff(&self, left_id: &str, right_id: &str) -> Result<StudyDiff, StudyError> {
        let left = self.variant(left_id)?;
        let right = self.variant(right_id)?;
        let a: Value = serde_json::from_str(&left.case_json)?;
        let b: Value = serde_json::from_str(&right.case_json)?;
        let mut changes = Vec::new();
        diff_values("", Some(&a), Some(&b), &mut changes);
        let l: BTreeMap<String, String> = bundle_summaries(&left.dataset_bundles)?
            .into_iter()
            .map(|x| {
                (
                    x.dataset_id,
                    format!("{}; bundle {}", x.csv_sha256, x.bundle_sha256),
                )
            })
            .collect();
        let r: BTreeMap<String, String> = bundle_summaries(&right.dataset_bundles)?
            .into_iter()
            .map(|x| {
                (
                    x.dataset_id,
                    format!("{}; bundle {}", x.csv_sha256, x.bundle_sha256),
                )
            })
            .collect();
        for id in l.keys().chain(r.keys()).cloned().collect::<BTreeSet<_>>() {
            if l.get(&id) != r.get(&id) {
                changes.push(InputChange {
                    pointer: format!("/dataset_bundles/{}", escape_pointer(&id)),
                    before: l.get(&id).cloned().map(Value::String),
                    after: r.get(&id).cloned().map(Value::String),
                });
            }
        }
        if left.options.threads != right.options.threads {
            changes.push(InputChange {
                pointer: "/options/threads".into(),
                before: Some(serde_json::to_value(left.options.threads)?),
                after: Some(serde_json::to_value(right.options.threads)?),
            });
        }
        Ok(StudyDiff {
            schema: "optcoil-study-diff/v1".into(),
            left_variant_id: left_id.into(),
            right_variant_id: right_id.into(),
            changes,
            result_comparison: compare_latest_results(
                self.current_results(left)?,
                self.current_results(right)?,
            )?,
        })
    }

    pub fn propose_followup(
        &self,
        id: &str,
        axis: FollowUpAxis,
    ) -> Result<FollowUpExperiment, StudyError> {
        let variant = self.variant(id)?;
        let original: Value = serde_json::from_str(&variant.case_json)?;
        let mut proposed = original.clone();
        let pointer = match axis {
            FollowUpAxis::TapesAlongWidth => "/choices/tapes_along_width",
            FollowUpAxis::StrandsParallel => "/choices/strands_parallel",
        };
        let parsed_original = CoupledSearchCase::from_json(&variant.case_json)
            .map_err(|e| StudyError::Invalid(format!("case validation: {e}")))?;
        if axis == FollowUpAxis::StrandsParallel && !supports_strands_axis(&parsed_original.schema)
        {
            return Err(StudyError::Invalid(
                "this case schema does not support strands_parallel choices".into(),
            ));
        }
        if axis == FollowUpAxis::StrandsParallel && proposed.pointer(pointer).is_none() {
            proposed["choices"]["strands_parallel"] = serde_json::json!([1]);
        }
        let array = proposed
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| {
                StudyError::Invalid(format!("case does not declare follow-up axis {pointer}"))
            })?;
        if array.len() >= 64 {
            return Err(StudyError::Invalid(format!(
                "{pointer} already has 64 choices, the unchanged engine numerical limit"
            )));
        }
        let max = array
            .iter()
            .filter_map(Value::as_u64)
            .max()
            .ok_or_else(|| {
                StudyError::Invalid(format!("{pointer} has no positive integer values"))
            })?;
        let ceiling = match axis {
            FollowUpAxis::TapesAlongWidth => 1_000,
            FollowUpAxis::StrandsParallel => 4_096,
        };
        if max >= ceiling {
            return Err(StudyError::Invalid(format!(
                "{pointer} already reaches engine numerical limit {ceiling}; no extension proposed"
            )));
        }
        array.push(Value::from(max + 1));
        array.sort_by_key(|v| v.as_u64().unwrap_or(u64::MAX));
        array.dedup();
        let proposed_case_json = serde_json::to_string_pretty(&proposed)?;
        let proposed_case = CoupledSearchCase::from_json(&proposed_case_json).map_err(|e| {
            StudyError::Invalid(format!(
                "proposed follow-up exceeds existing case gates: {e}"
            ))
        })?;
        let datasets = datasets_for_case(&proposed_case_json, &variant.dataset_bundles)?;
        let preflight = preflight_coupled_search_with_options(
            &proposed_case,
            Some(&datasets),
            &variant.options,
        );
        let mut changes = Vec::new();
        diff_values("", Some(&original), Some(&proposed), &mut changes);
        Ok(FollowUpExperiment {
            schema: "optcoil-follow-up-experiment/v1".into(),
            source_variant_id: id.into(),
            axis,
            proposed_case_json,
            changed_inputs: changes,
            preflight,
            rationale: format!(
                "Add the next allowed {} choice to examine a wider declared search space; this explicit proposal has not been calculated and does not imply feasibility.",
                match axis {
                    FollowUpAxis::TapesAlongWidth => "tape-count",
                    FollowUpAxis::StrandsParallel => "parallel-strand",
                }
            ),
            calculated: false,
            requirements_changed: false,
            numerical_gates_changed: false,
            limits_changed: false,
        })
    }

    pub fn review_package_variant(
        &self,
        id: &str,
        result_id: &str,
    ) -> Result<ReviewPackage, StudyError> {
        let variant = self.variant(id)?;
        let result = variant
            .results
            .iter()
            .find(|r| r.id == result_id)
            .ok_or_else(|| StudyError::Invalid(format!("unknown result '{result_id}'")))?;
        let bundles: Vec<&str> = result
            .source_dataset_bundles
            .iter()
            .map(String::as_str)
            .collect();
        let artifacts = review::review_package_with_bundle_json(
            &result.record_json,
            Some(&result.source_case_json),
            &bundles,
        )?;
        Ok(ReviewPackage { artifacts })
    }

    fn current_results<'a>(
        &self,
        variant: &'a StudyVariant,
    ) -> Result<Vec<&'a StudyResult>, StudyError> {
        let key = exact_input_key(variant)?;
        Ok(variant
            .results
            .iter()
            .filter(|r| {
                r.exact_input_key == key && r.case_sha256 == sha256(variant.case_json.as_bytes())
            })
            .collect())
    }
}

/// Hashes every executable input and implementation identity. It is suitable
/// only as a lookup key in a live [`StudyEngineSession`], never as proof.
pub fn exact_input_key(variant: &StudyVariant) -> Result<String, StudyError> {
    let mut hasher = Sha256::new();
    hasher.update(b"optcoil-study-live-input/v1\0");
    hasher.update((variant.case_json.len() as u64).to_le_bytes());
    hasher.update(variant.case_json.as_bytes());
    let options = serde_json::to_vec(&variant.options)?;
    hasher.update((options.len() as u64).to_le_bytes());
    hasher.update(options);
    hasher.update(crate::coupled_search::COUPLED_SEARCH_MODEL_ID.as_bytes());
    hasher.update(crate::coupled_search::COUPLED_SEARCH_CHECKER_ID.as_bytes());
    for bundle in &variant.dataset_bundles {
        hasher.update((bundle.len() as u64).to_le_bytes());
        hasher.update(bundle.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Non-serialized, per-running-engine cache. Results enter only after this
/// session computed them from the exact case, options, bundle and model bytes.
#[derive(Default)]
pub struct StudyEngineSession {
    completed: BTreeMap<String, String>,
}

impl StudyEngineSession {
    pub fn cached_result<'a>(
        &'a self,
        variant: &StudyVariant,
    ) -> Result<Option<&'a str>, StudyError> {
        Ok(self
            .completed
            .get(&exact_input_key(variant)?)
            .map(String::as_str))
    }
    pub fn run_variant(
        &mut self,
        workspace: &mut StudyWorkspace,
        id: &str,
    ) -> Result<String, StudyError> {
        self.run_variant_with(workspace, id, &AtomicBool::new(false), None)
    }
    pub fn run_variant_with(
        &mut self,
        workspace: &mut StudyWorkspace,
        id: &str,
        cancel: &AtomicBool,
        progress: Option<&SearchProgress>,
    ) -> Result<String, StudyError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(RunError::Cancelled.into());
        }
        workspace.validate()?;
        let variant = workspace.variant(id)?.clone();
        let key = exact_input_key(&variant)?;
        let (record_json, was_cached) = if let Some(cached) = self.completed.get(&key) {
            if let Some(existing) = workspace
                .current_results(&variant)?
                .into_iter()
                .find(|result| result.record_json == *cached)
            {
                return Ok(existing.id.clone());
            }
            (cached.clone(), true)
        } else {
            let datasets = datasets_for_case(&variant.case_json, &variant.dataset_bundles)?;
            variant_case_preflight(&variant, &datasets)?;
            let record = coupled_search::run_coupled_search_case_with_datasets_progress(
                &variant.case_json,
                &variant.options,
                &datasets,
                cancel,
                progress,
            )?;
            let json = serde_json::to_string(&record)?;
            (json, false)
        };
        let result_id = workspace.attach_result(id, record_json.clone())?;
        if !was_cached {
            self.cache_insert(key, record_json);
        }
        Ok(result_id)
    }

    fn cache_insert(&mut self, key: String, record_json: String) {
        if record_json.len() > MAX_STUDY_RESULT_BYTES {
            return;
        }
        while self.completed.len() >= MAX_STUDY_LIVE_CACHE_ENTRIES
            || self
                .completed
                .values()
                .map(String::len)
                .sum::<usize>()
                .saturating_add(record_json.len())
                > MAX_STUDY_LIVE_CACHE_BYTES
        {
            let Some(first_key) = self.completed.keys().next().cloned() else {
                break;
            };
            self.completed.remove(&first_key);
        }
        self.completed.insert(key, record_json);
    }
}

fn validate_variant(v: &StudyVariant) -> Result<(), StudyError> {
    if v.id.trim().is_empty() || v.id.len() > 80 || v.name.trim().is_empty() || v.name.len() > 200 {
        return Err(StudyError::Invalid(
            "variant id/name is empty or too long".into(),
        ));
    }
    if v.case_json.len() > MAX_STUDY_CASE_BYTES {
        return Err(StudyError::Invalid("case exceeds 4 MiB limit".into()));
    }
    CoupledSearchCase::from_json(&v.case_json)
        .map_err(|e| StudyError::Invalid(format!("case validation: {e}")))?;
    if v.dataset_bundles.len() > 32 {
        return Err(StudyError::Invalid(
            "variant exceeds 32 dataset bundle limit".into(),
        ));
    }
    if v.dataset_bundles
        .iter()
        .any(|b| b.len() > MAX_STUDY_BUNDLE_BYTES)
    {
        return Err(StudyError::Invalid(
            "dataset bundle exceeds 8 MiB limit".into(),
        ));
    }
    let _ = resolve_bundles(&v.dataset_bundles)?;
    if v.results.len() > MAX_STUDY_RESULTS_PER_VARIANT {
        return Err(StudyError::Invalid(
            "variant exceeds 128 result limit".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut payload_bytes = v.case_json.len();
    for bundle in &v.dataset_bundles {
        payload_bytes = payload_bytes.saturating_add(bundle.len());
    }
    for result in &v.results {
        if !seen.insert(result.id.as_str()) || result.record_json.len() > MAX_STUDY_RESULT_BYTES {
            return Err(StudyError::Invalid(
                "duplicate result id or result exceeds 24 MiB limit".into(),
            ));
        }
        let hash = sha256(result.record_json.as_bytes());
        if result.id != format!("result-{}", &hash[..16]) {
            return Err(StudyError::Invalid(
                "result id does not match exact record bytes".into(),
            ));
        }
        let record: CoupledSearchRunRecord = serde_json::from_str(&result.record_json)
            .map_err(|e| StudyError::Invalid(format!("stored result parse: {e}")))?;
        if strip_sha(&record.case_sha256) != result.case_sha256
            || record.input_sha256 != result.input_sha256
            || result.case_sha256 != sha256(result.source_case_json.as_bytes())
        {
            return Err(StudyError::Invalid(
                "stored result metadata differs from record or source case".into(),
            ));
        }
        if result.source_case_json.len() > MAX_STUDY_CASE_BYTES
            || result.source_dataset_bundles.len() > 32
            || result
                .source_dataset_bundles
                .iter()
                .any(|b| b.len() > MAX_STUDY_BUNDLE_BYTES)
        {
            return Err(StudyError::Invalid(
                "stored result source inputs exceed payload limits".into(),
            ));
        }
        payload_bytes = payload_bytes
            .saturating_add(result.record_json.len())
            .saturating_add(result.source_case_json.len());
        for bundle in &result.source_dataset_bundles {
            payload_bytes = payload_bytes.saturating_add(bundle.len());
        }
        let source_variant = StudyVariant {
            id: v.id.clone(),
            name: v.name.clone(),
            case_json: result.source_case_json.clone(),
            dataset_bundles: result.source_dataset_bundles.clone(),
            options: result.source_options,
            results: Vec::new(),
        };
        let source_case: CoupledSearchCase = serde_json::from_str(&source_variant.case_json)
            .map_err(|e| StudyError::Invalid(format!("stored result source case parse: {e}")))?;
        let source_datasets =
            datasets_for_case(&source_variant.case_json, &source_variant.dataset_bundles)?;
        source_case
            .validate_against_dataset_map(&source_datasets)
            .map_err(|e| {
                StudyError::Invalid(format!("stored result dataset bindings are invalid: {e}"))
            })?;
        if serde_json::to_value(&record.case)? != serde_json::to_value(&source_case)?
            || exact_input_key(&source_variant)? != result.exact_input_key
        {
            return Err(StudyError::Invalid(
                "stored result is stale or tampered relative to its retained source inputs".into(),
            ));
        }
        if record.input_sha256 != expected_run_input_sha(&record, &source_variant.options)? {
            return Err(StudyError::Invalid(
                "stored result input identity does not match retained source options".into(),
            ));
        }
        if record.coupled_search_model_id != crate::coupled_search::COUPLED_SEARCH_MODEL_ID
            || record.coupled_search_checker_id != crate::coupled_search::COUPLED_SEARCH_CHECKER_ID
        {
            return Err(StudyError::Invalid(
                "stored result model/checker identity is stale".into(),
            ));
        }
        let bundle_refs: Vec<&str> = source_variant
            .dataset_bundles
            .iter()
            .map(String::as_str)
            .collect();
        let checks = crate::verify::verify_record_checks(
            &result.record_json,
            Some(&source_variant.case_json),
            None,
            None,
            &bundle_refs,
        )?;
        if checks
            .iter()
            .any(|c| c.outcome == crate::verify::Outcome::Fail)
        {
            return Err(StudyError::Invalid(
                "stored result failed reopen verification".into(),
            ));
        }
    }
    if payload_bytes > MAX_STUDY_WORKSPACE_BYTES {
        return Err(StudyError::Invalid(
            "variant payload exceeds 64 MiB workspace limit".into(),
        ));
    }
    Ok(())
}

fn resolve_bundles(raw: &[String]) -> Result<BTreeMap<String, MaterialDataset>, StudyError> {
    let mut map = BTreeMap::new();
    for text in raw {
        let bundle = MaterialBundle::from_json(text)
            .map_err(|e| StudyError::Invalid(format!("dataset bundle: {e}")))?;
        let id = bundle.dataset.metadata.id.clone();
        if map.insert(id.clone(), bundle.dataset).is_some() {
            return Err(StudyError::Invalid(format!(
                "duplicate dataset bundle identity '{id}'"
            )));
        }
    }
    Ok(map)
}

fn datasets_for_case(
    case_json: &str,
    raw: &[String],
) -> Result<BTreeMap<String, MaterialDataset>, StudyError> {
    let case: CoupledSearchCase = serde_json::from_str(case_json)
        .map_err(|e| StudyError::Invalid(format!("case parse: {e}")))?;
    let mut datasets = resolve_bundles(raw)?;
    for (_, binding) in case.material_bindings() {
        if !datasets.contains_key(&binding.dataset_id)
            && let Ok(dataset) = MaterialDataset::embedded_by_id(&binding.dataset_id)
        {
            datasets.insert(binding.dataset_id.clone(), dataset);
        }
    }
    Ok(datasets)
}

fn bundle_summaries(raw: &[String]) -> Result<Vec<DatasetIdentitySummary>, StudyError> {
    raw.iter()
        .map(|text| {
            let b = MaterialBundle::from_json(text)
                .map_err(|e| StudyError::Invalid(format!("dataset bundle: {e}")))?;
            Ok(DatasetIdentitySummary {
                dataset_id: b.dataset.metadata.id,
                csv_sha256: b.dataset.metadata.csv_sha256,
                bundle_sha256: sha256(text.as_bytes()),
            })
        })
        .collect()
}

fn variant_case_preflight(
    v: &StudyVariant,
    datasets: &BTreeMap<String, MaterialDataset>,
) -> Result<(), StudyError> {
    let case: CoupledSearchCase = serde_json::from_str(&v.case_json)
        .map_err(|e| StudyError::Invalid(format!("case parse: {e}")))?;
    let p = preflight_coupled_search_with_options(&case, Some(datasets), &v.options);
    if !p.ready_to_run {
        return Err(StudyError::Invalid(format!(
            "preflight has {} blocking input issue(s)",
            p.errors.len()
        )));
    }
    Ok(())
}

fn issue_pointer(code: &str) -> Option<String> {
    match code {
        "dataset_missing" | "dataset_identity_mismatch" | "dataset_not_declared" => {
            Some("/material".into())
        }
        "threads_exceed_case_limit" | "invalid_threads" => Some("/execution/max_threads".into()),
        "invalid_case" => Some("/".into()),
        _ => None,
    }
}

fn supports_strands_axis(schema: &str) -> bool {
    schema
        .rsplit_once("/v")
        .and_then(|(_, version)| version.parse::<u32>().ok())
        .is_some_and(|version| version >= 4)
}

fn record_issues(record: &CoupledSearchRunRecord) -> Vec<StudyIssue> {
    let mut out = Vec::new();
    if record.search_status != Status::Pass {
        out.push(StudyIssue {
            code: "search_selection_unresolved".into(),
            status: record.search_status,
            message: format!(
                "Recorded search status is {:?}. {}",
                record.search_status,
                if record.best_index.is_none() {
                    "No eligible PASS candidate was selected; this does not establish physical impossibility."
                } else {
                    "Inspect the selected candidate's recorded checks."
                }
            ),
            input_pointer: Some("/choices".into()),
            observed_limit: None,
            data_gap: None,
            candidate_index: record.best_index,
            limiting_location: None,
            observed_value: None,
            declared_limit: None,
            unit: None,
        });
    }
    for candidate in &record.candidates {
        if candidate.status != Status::Pass {
            out.push(metric_gate_issue(
                "candidate_status",
                candidate.status,
                candidate.index,
                format!(
                    "candidate {} overall status is {:?}",
                    candidate.index, candidate.status
                ),
                None,
                None,
                None,
                None,
                None,
                None,
            ));
        }
        let screening_location = |point: Option<&crate::coupled::LimitingPoint>| {
            point.map(|p| {
                format!(
                    "station={}, turn={}, tape={}, width={}",
                    p.station, p.turn_index, p.tape_index, p.width_index
                )
            })
        };
        if candidate.requirement_status != Status::Pass {
            out.push(StudyIssue {
                code: "requirement_gate_unresolved".into(),
                status: candidate.requirement_status,
                message: format!(
                    "candidate {} requirement gate is {:?}",
                    candidate.index, candidate.requirement_status
                ),
                input_pointer: Some("/requirement".into()),
                observed_limit: None,
                data_gap: None,
                candidate_index: Some(candidate.index),
                limiting_location: None,
                observed_value: None,
                declared_limit: None,
                unit: None,
            });
        }
        if candidate.refinement_status != Status::Pass {
            let limit = record.case.numerics.field_scale_t
                * record.case.numerics.max_refinement_change_fraction;
            let (code, observed, location) = if candidate.bore_refinement_change_t.is_finite()
                && candidate.bore_refinement_change_t > limit
            {
                (
                    "bore_refinement_gate_unresolved",
                    Some(candidate.bore_refinement_change_t),
                    None,
                )
            } else if candidate.good_field.as_ref().is_some_and(|field| {
                field.refinement_change_t.is_finite() && field.refinement_change_t > limit
            }) {
                let field = candidate.good_field.as_ref().expect("checked above");
                (
                    "good_field_refinement_gate_unresolved",
                    Some(field.refinement_change_t),
                    None,
                )
            } else if candidate.screening.as_ref().is_some_and(|screening| {
                screening.max_refinement_change_t.is_finite()
                    && screening.max_refinement_change_t > limit
            }) {
                (
                    "screening_refinement_gate_unresolved",
                    candidate
                        .screening
                        .as_ref()
                        .map(|screening| screening.max_refinement_change_t),
                    None,
                )
            } else {
                ("field_refinement_gate_unresolved", None, None)
            };
            let measured_excess = observed.is_some();
            out.push(StudyIssue { code: code.into(), status: candidate.refinement_status,
                message: if measured_excess { format!("candidate {} evaluated a field refinement change above the declared {limit} T gate", candidate.index) } else { format!("candidate {} refinement status is {:?}; the record does not contain a finite refinement value identifying the cause", candidate.index, candidate.refinement_status) },
                input_pointer: measured_excess.then(|| "/numerics/max_refinement_change_fraction".into()), observed_limit: measured_excess.then(|| format!("{limit} T")), data_gap: None,
                candidate_index: Some(candidate.index), limiting_location: location, observed_value: observed, declared_limit: measured_excess.then_some(limit), unit: measured_excess.then(|| "T".into()) });
        }
        if candidate.numerical_status != Status::Pass {
            out.push(StudyIssue {
                code: "numerical_reference_unresolved".into(),
                status: candidate.numerical_status,
                message: format!(
                    "candidate {} numerical comparison status is {:?}",
                    candidate.index, candidate.numerical_status
                ),
                input_pointer: None,
                observed_limit: None,
                data_gap: Some(
                    "No independent per-candidate reference establishes numerical accuracy.".into(),
                ),
                candidate_index: Some(candidate.index),
                limiting_location: None,
                observed_value: None,
                declared_limit: None,
                unit: None,
            });
        }
        if let Some(screening) = &candidate.screening {
            if let Some(value) = screening.max_utilization {
                let bound = record.case.limits.utilization_limit;
                if value > bound {
                    out.push(StudyIssue {
                        code: "utilization_limit_exceeded".into(),
                        status: Status::Fail,
                        message: format!(
                            "candidate {} utilization {value} exceeds declared limit {bound}",
                            candidate.index
                        ),
                        input_pointer: Some("/limits/utilization_limit".into()),
                        observed_limit: Some(bound.to_string()),
                        data_gap: None,
                        candidate_index: Some(candidate.index),
                        limiting_location: screening_location(screening.limiting.as_ref()),
                        observed_value: Some(value),
                        declared_limit: Some(bound),
                        unit: Some("ratio".into()),
                    });
                }
            }
            match record.case.limits.self_field_correction.as_deref() {
                None => {
                    if let Some(value) = screening.limiting_self_field_ratio {
                        let bound = record.case.limits.max_self_field_ratio;
                        if value > bound {
                            out.push(StudyIssue { code: "self_field_applicability_limit".into(), status: Status::Inconclusive,
                            message: format!("candidate {} limiting uncorrected self-field ratio {value} exceeds applicability limit {bound}", candidate.index),
                            input_pointer: Some("/limits/max_self_field_ratio".into()), observed_limit: Some(bound.to_string()), data_gap: Some("The declared query omits a self-field correction in this dominance range.".into()),
                            candidate_index: Some(candidate.index), limiting_location: screening_location(screening.limiting.as_ref()), observed_value: Some(value), declared_limit: Some(bound), unit: Some("ratio".into()) });
                        }
                    }
                }
                Some("uniform_transport")
                    if screening
                        .max_transport_self_field_ratio
                        .is_some_and(|value| value > 1.0) =>
                {
                    let value = screening.max_transport_self_field_ratio.unwrap_or_default();
                    out.push(StudyIssue { code: "uniform_transport_self_field_bound_exceeded".into(), status: Status::Inconclusive,
                        message: format!("candidate {} transport self-field ratio {value} exceeds the uniform-transport bound of 1", candidate.index),
                        input_pointer: Some("/limits/self_field_correction".into()), observed_limit: Some("1 ratio".into()), data_gap: Some("The declared uniform-transport correction is outside its dominance validity bound.".into()),
                        candidate_index: Some(candidate.index), limiting_location: None, observed_value: Some(value), declared_limit: Some(1.0), unit: Some("ratio".into()) });
                }
                // critical_state_strip is declared as a bound at any ratio; it has no ratio gate.
                _ => {}
            }
            let along = screening.max_along_current_fraction;
            let along_limit = record.case.limits.max_along_current_field_fraction;
            let bounded_model = record.case.limits.along_current_model.is_some();
            let beyond_bound = along > optcoil_model::coupled::ALONG_CURRENT_BOUND_CEILING;
            let counts = &screening.point_counts;
            if counts.along_current_excluded > 0
                && along > along_limit
                && (!bounded_model || beyond_bound)
            {
                let limit = if bounded_model {
                    optcoil_model::coupled::ALONG_CURRENT_BOUND_CEILING
                } else {
                    along_limit
                };
                out.push(StudyIssue { code: "along_current_query_excluded".into(), status: Status::Inconclusive,
                    message: format!("candidate {} maximum along-current fraction {along} exceeds supported query limit {limit}", candidate.index),
                    input_pointer: Some(if bounded_model { "/limits/along_current_model" } else { "/limits/max_along_current_field_fraction" }.into()), observed_limit: Some(limit.to_string()),
                    data_gap: Some("The declared material angle query cannot resolve this along-current field fraction.".into()),
                    candidate_index: Some(candidate.index), limiting_location: None, observed_value: Some(along), declared_limit: Some(limit), unit: Some("fraction".into()) });
            }
            if counts.unsupported > 0 || counts.along_current_excluded > 0 {
                out.push(StudyIssue {
                    code: "material_coverage_gap".into(), status: Status::Inconclusive,
                    message: format!("candidate {} has {} unsupported and {} along-current-excluded sampled point(s); {} lower-bound point(s) remain conservative evidence", candidate.index, counts.unsupported, counts.along_current_excluded, counts.lower_bound),
                    input_pointer: Some("/material".into()), observed_limit: None,
                    data_gap: Some("Material coverage or query policy left sampled points undetermined.".into()),
                    candidate_index: Some(candidate.index), limiting_location: None, observed_value: None, declared_limit: None, unit: None,
                });
            }
        }
        append_declared_gate_issues(record, candidate, &mut out, &screening_location);
        if let Some(mechanical) = &record.case.mechanical {
            let values = [
                (
                    "lorentz_load_exceeded",
                    candidate.lorentz_load_n_per_m,
                    Some(mechanical.max_lorentz_load_n_per_m),
                    "/mechanical/max_lorentz_load_n_per_m",
                    "N/m",
                ),
                (
                    "hoop_stress_exceeded",
                    candidate.hoop_stress_pa,
                    mechanical.max_hoop_stress_pa,
                    "/mechanical/max_hoop_stress_pa",
                    "Pa",
                ),
                (
                    "transverse_pressure_exceeded",
                    candidate.transverse_pressure_pa,
                    mechanical.max_transverse_pressure_pa,
                    "/mechanical/max_transverse_pressure_pa",
                    "Pa",
                ),
                (
                    "membrane_tension_exceeded",
                    candidate.membrane_tension_n_per_m,
                    mechanical.max_membrane_tension_n_per_m,
                    "/mechanical/max_membrane_tension_n_per_m",
                    "N/m",
                ),
            ];
            for (code, observed, limit, pointer, unit) in values {
                if let (Some(value), Some(bound)) = (observed, limit)
                    && value > bound
                {
                    let location = match code {
                        "transverse_pressure_exceeded" => {
                            candidate.transverse_pressure_location.as_ref()
                        }
                        _ => None,
                    }
                    .map(|p| {
                        format!(
                            "station={}, turn={}, tape={}, width={}",
                            p.station, p.turn_index, p.tape_index, p.width_index
                        )
                    });
                    out.push(StudyIssue {
                        code: code.into(),
                        status: Status::Fail,
                        message: format!(
                            "candidate {} observed {value} {unit}, declared limit {bound} {unit}",
                            candidate.index
                        ),
                        input_pointer: Some(pointer.into()),
                        observed_limit: Some(bound.to_string()),
                        data_gap: None,
                        candidate_index: Some(candidate.index),
                        limiting_location: location,
                        observed_value: Some(value),
                        declared_limit: Some(bound),
                        unit: Some(unit.into()),
                    });
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)] // Keep the diagnostic field mapping explicit at call sites.
fn metric_gate_issue(
    code: &str,
    status: Status,
    candidate_index: usize,
    message: String,
    input_pointer: Option<&str>,
    observed: Option<f64>,
    limit: Option<f64>,
    unit: Option<&str>,
    location: Option<String>,
    data_gap: Option<String>,
) -> StudyIssue {
    StudyIssue {
        code: code.into(),
        status,
        message,
        input_pointer: input_pointer.map(str::to_owned),
        observed_limit: limit.map(|value| match unit {
            Some(unit) => format!("{value} {unit}"),
            None => value.to_string(),
        }),
        data_gap,
        candidate_index: Some(candidate_index),
        limiting_location: location,
        observed_value: observed,
        declared_limit: limit,
        unit: unit.map(str::to_owned),
    }
}

fn append_declared_gate_issues(
    record: &CoupledSearchRunRecord,
    candidate: &crate::coupled_search::SearchCandidateResult,
    out: &mut Vec<StudyIssue>,
    screening_location: &impl Fn(Option<&crate::coupled::LimitingPoint>) -> Option<String>,
) {
    if !candidate.pack_geometry_valid {
        out.push(metric_gate_issue(
            "pack_geometry_invalid", Status::Fail, candidate.index,
            format!("candidate {} winding pack geometry is degenerate; the pack occupies the bore and cannot be evaluated", candidate.index),
            Some("/fixed_geometry"), None, None, None, None,
            Some("The recorded geometry is not valid for field evaluation.".into()),
        ));
    }
    if !candidate.manufacturing_feasible {
        let manufacturing = record.case.manufacturing.as_ref();
        let mut emitted = false;
        if let (Some(radius), Some(declared)) = (
            candidate.inner_bend_radius_m,
            manufacturing.map(|m| m.min_inner_bend_radius_m),
        ) && radius < declared
        {
            out.push(metric_gate_issue(
                "minimum_inner_bend_radius_exceeded", Status::Fail, candidate.index,
                format!("candidate {} inner bend radius {radius} m is below declared minimum {declared} m", candidate.index),
                Some("/manufacturing/min_inner_bend_radius_m"), Some(radius), Some(declared), Some("m"), None, None,
            ));
            emitted = true;
        }
        if let (Some(strain), Some(declared)) = (
            candidate.bend_strain,
            manufacturing.and_then(|m| m.max_bend_strain),
        ) && strain > declared
        {
            out.push(metric_gate_issue(
                "maximum_bend_strain_exceeded",
                Status::Fail,
                candidate.index,
                format!(
                    "candidate {} bend strain {strain} exceeds declared maximum {declared}",
                    candidate.index
                ),
                Some("/manufacturing/max_bend_strain"),
                Some(strain),
                Some(declared),
                Some("fraction"),
                None,
                None,
            ));
            emitted = true;
        }
        if !emitted {
            out.push(metric_gate_issue(
                "manufacturing_gate_unresolved", Status::Fail, candidate.index,
                format!("candidate {} failed the declared manufacturing feasibility gate", candidate.index),
                Some("/manufacturing"), None, None, None, None,
                Some("The record does not contain a finite measurement identifying the manufacturing failure.".into()),
            ));
        }
    }

    if let Some(field) = &candidate.good_field {
        if field.pack_overlap {
            out.push(metric_gate_issue(
                "good_field_pack_overlap",
                Status::Fail,
                candidate.index,
                format!(
                    "candidate {} declared good-field region overlaps the winding pack",
                    candidate.index
                ),
                Some("/requirement/good_field_region"),
                None,
                None,
                None,
                None,
                None,
            ));
        }
        if let (Some(value), Some(limit)) = (
            field.relative_deviation,
            record
                .case
                .requirement
                .good_field_region
                .as_ref()
                .and_then(|r| r.max_relative_deviation),
        ) && value > limit
        {
            out.push(metric_gate_issue(
                    "good_field_uniformity_exceeded", Status::Fail, candidate.index,
                    format!("candidate {} good-field relative deviation {value} exceeds declared maximum {limit}", candidate.index),
                    Some("/requirement/good_field_region/max_relative_deviation"), Some(value), Some(limit), Some("fraction"), None, None,
                ));
        }
        if let Some(harmonics) = record
            .case
            .requirement
            .good_field_region
            .as_ref()
            .and_then(|r| r.harmonics.as_ref())
        {
            for (axis, units, limit) in [
                (
                    "normal",
                    field.harmonic_normal_units.as_ref(),
                    harmonics.max_normal_unit_fraction,
                ),
                (
                    "skew",
                    field.harmonic_skew_units.as_ref(),
                    harmonics.max_skew_unit_fraction,
                ),
            ] {
                if let (Some(units), Some(limit)) = (units, limit) {
                    let observed = units.iter().map(|v| v.abs()).fold(0.0_f64, f64::max);
                    if observed > limit {
                        let pointer = if axis == "normal" {
                            "/requirement/good_field_region/harmonics/max_normal_unit_fraction"
                        } else {
                            "/requirement/good_field_region/harmonics/max_skew_unit_fraction"
                        };
                        out.push(metric_gate_issue(
                            if axis == "normal" { "good_field_normal_harmonic_exceeded" } else { "good_field_skew_harmonic_exceeded" },
                            Status::Fail, candidate.index,
                            format!("candidate {} maximum absolute {axis} harmonic {observed} exceeds declared maximum {limit}", candidate.index),
                            Some(pointer), Some(observed), Some(limit), Some("fraction"), None, None,
                        ));
                    }
                }
            }
        }
    }

    let Some(screens) = &candidate.screens else {
        return;
    };
    if let (Some(declared), Some(result)) = (&record.case.thermal_margin, &screens.thermal_margin)
        && result.status != Status::Pass
    {
        let observed = result.min_margin_k;
        out.push(metric_gate_issue(
            "thermal_margin_screen_unresolved",
            result.status,
            candidate.index,
            format!(
                "candidate {} thermal-margin screen is {:?}",
                candidate.index, result.status
            ),
            Some("/thermal_margin/min_margin_k"),
            observed,
            Some(declared.min_margin_k),
            Some("K"),
            screening_location(result.limiting.as_ref()),
            screen_gap(
                result.status,
                result.points_inconclusive,
                result.points_evaluated,
                result.min_margin_is_lower_bound,
            ),
        ));
    }
    if let (Some(declared), Some(result)) = (&record.case.ac_loss, &screens.ac_loss)
        && result.status != Status::Pass
    {
        out.push(metric_gate_issue(
            "ac_loss_screen_unresolved",
            result.status,
            candidate.index,
            format!(
                "candidate {} AC-loss screen is {:?}",
                candidate.index, result.status
            ),
            Some("/ac_loss/max_loss_w"),
            result.total_loss_w,
            Some(declared.max_loss_w),
            Some("W"),
            None,
            screen_gap(
                result.status,
                result.points_inconclusive,
                result.points_evaluated,
                false,
            ),
        ));
    }
    if let (Some(declared), Some(result)) = (&record.case.quench_hotspot, &screens.quench_hotspot)
        && result.status != Status::Pass
    {
        out.push(metric_gate_issue(
            "quench_hotspot_screen_unresolved",
            result.status,
            candidate.index,
            format!(
                "candidate {} quench-hotspot screen is {:?}",
                candidate.index, result.status
            ),
            Some("/quench_hotspot/max_hotspot_k"),
            result.hotspot_temperature_k,
            Some(declared.max_hotspot_k),
            Some("K"),
            None,
            (result.status == Status::Inconclusive).then(|| {
                if result.table_exhausted {
                    "The required quench integral exceeds the declared table coverage.".into()
                } else {
                    "The recorded hot-spot temperature is unavailable.".into()
                }
            }),
        ));
    }
    if let (Some(declared), Some(result)) =
        (&record.case.screening_current, &screens.screening_current)
        && result.status != Status::Pass
    {
        out.push(metric_gate_issue(
            "screening_current_screen_unresolved",
            result.status,
            candidate.index,
            format!(
                "candidate {} screening-current flag is {:?}",
                candidate.index, result.status
            ),
            Some("/screening_current/max_penetrated_width_fraction"),
            result.max_penetrated_width_fraction,
            Some(declared.max_penetrated_width_fraction),
            Some("fraction"),
            screening_location(result.limiting.as_ref()),
            screen_gap(
                result.status,
                result.points_inconclusive,
                result.points_evaluated,
                false,
            ),
        ));
    }
    if let (Some(declared), Some(result)) = (&record.case.transition, &screens.transition)
        && result.status != Status::Pass
    {
        let depth_exceeded = result
            .max_e_over_ec
            .is_some_and(|v| v > declared.max_e_over_ec);
        let voltage_exceeded = declared
            .max_voltage_v
            .zip(result.terminal_voltage_v)
            .is_some_and(|(limit, v)| v > limit);
        if depth_exceeded || (result.status == Status::Fail && !voltage_exceeded) {
            out.push(metric_gate_issue(
                "transition_depth_screen_unresolved",
                result.status,
                candidate.index,
                format!(
                    "candidate {} transition-depth screen is {:?}",
                    candidate.index, result.status
                ),
                Some("/transition/max_e_over_ec"),
                result.max_e_over_ec,
                Some(declared.max_e_over_ec),
                Some("ratio"),
                screening_location(result.limiting.as_ref()),
                screen_gap(
                    result.status,
                    result.points_inconclusive,
                    result.points_evaluated,
                    false,
                ),
            ));
        }
        if voltage_exceeded {
            out.push(metric_gate_issue(
                "transition_voltage_screen_unresolved",
                result.status,
                candidate.index,
                format!(
                    "candidate {} transition voltage {:?} exceeds declared maximum {:?}",
                    candidate.index, result.terminal_voltage_v, declared.max_voltage_v
                ),
                Some("/transition/max_voltage_v"),
                result.terminal_voltage_v,
                declared.max_voltage_v,
                Some("V"),
                screening_location(result.limiting.as_ref()),
                None,
            ));
        }
        if result.status == Status::Fail && !depth_exceeded && !voltage_exceeded {
            out.push(metric_gate_issue(
                    "transition_screen_unresolved", result.status, candidate.index,
                    format!("candidate {} transition screen failed but no recorded metric identifies the bound violation", candidate.index),
                    Some("/transition"), None, None, None, None,
                    Some("Screen result and declared threshold do not identify a finite failing metric.".into()),
                ));
        }
        if result.status == Status::Inconclusive {
            out.push(metric_gate_issue(
                "transition_screen_unresolved",
                result.status,
                candidate.index,
                format!(
                    "candidate {} transition screen is inconclusive",
                    candidate.index
                ),
                Some("/transition"),
                None,
                None,
                None,
                None,
                screen_gap(
                    result.status,
                    result.points_inconclusive,
                    result.points_evaluated,
                    false,
                ),
            ));
        }
    }
    if let (Some(declared), Some(result)) =
        (&record.case.quench_transient, &screens.quench_transient)
        && result.status != Status::Pass
    {
        let temperature_exceeded = declared
            .max_temperature_k
            .zip(result.peak_temperature_k)
            .is_some_and(|(limit, value)| value > limit);
        let detection_exceeded = declared
            .max_detection_time_s
            .is_some_and(|limit| result.detection_time_s.is_none_or(|value| value > limit));
        if temperature_exceeded {
            out.push(metric_gate_issue(
                "quench_transient_temperature_exceeded",
                result.status,
                candidate.index,
                format!(
                    "candidate {} transient peak temperature exceeds the declared maximum",
                    candidate.index
                ),
                Some("/quench_transient/max_temperature_k"),
                result.peak_temperature_k,
                declared.max_temperature_k,
                Some("K"),
                screening_location(result.limiting.as_ref()),
                None,
            ));
        }
        if detection_exceeded {
            out.push(metric_gate_issue(
                "quench_transient_detection_unresolved",
                result.status,
                candidate.index,
                format!(
                    "candidate {} transient voltage detection did not meet the declared time bound",
                    candidate.index
                ),
                Some("/quench_transient/max_detection_time_s"),
                result.detection_time_s,
                declared.max_detection_time_s,
                Some("s"),
                None,
                result
                    .detection_time_s
                    .is_none()
                    .then(|| "No threshold crossing time was recorded.".into()),
            ));
        }
        if !temperature_exceeded && !detection_exceeded {
            out.push(metric_gate_issue(
                "quench_transient_screen_unresolved",
                result.status,
                candidate.index,
                format!(
                    "candidate {} quench-transient screen is {:?}",
                    candidate.index, result.status
                ),
                Some("/quench_transient"),
                None,
                None,
                None,
                screening_location(result.limiting.as_ref()),
                screen_gap(
                    result.status,
                    result.points_inconclusive,
                    result.points_evaluated,
                    result.coverage_exhausted,
                ),
            ));
        }
    }
}

fn screen_gap(
    status: Status,
    inconclusive: u64,
    evaluated: u64,
    coverage_limited: bool,
) -> Option<String> {
    match status {
        Status::NotEvaluated => Some("The declared screen did not run for this candidate.".into()),
        Status::Inconclusive if coverage_limited => Some("The screen reports a coverage-limited bound; the unresolved region is not extrapolated.".into()),
        Status::Inconclusive if inconclusive > 0 => Some(format!("{inconclusive} of {evaluated} points were inconclusive.")),
        Status::Inconclusive => Some("The recorded screen could not resolve its declared gate.".into()),
        _ => None,
    }
}

fn compare_latest_results(
    left: Vec<&StudyResult>,
    right: Vec<&StudyResult>,
) -> Result<Option<StudyResultComparison>, StudyError> {
    let (Some(l), Some(r)) = (left.last(), right.last()) else {
        return Ok(None);
    };
    let a = review::decision_summary(&l.record_json)?;
    let b = review::decision_summary(&r.record_json)?;
    let left_record: CoupledSearchRunRecord = serde_json::from_str(&l.record_json)?;
    let right_record: CoupledSearchRunRecord = serde_json::from_str(&r.record_json)?;
    let left_headroom = a
        .current_utilization
        .map(|usage| a.utilization_limit - usage);
    let right_headroom = b
        .current_utilization
        .map(|usage| b.utilization_limit - usage);
    Ok(Some(StudyResultComparison {
        left_status: left_record.search_status,
        right_status: right_record.search_status,
        left_selected_status: a.selected_status,
        right_selected_status: b.selected_status,
        left_selected_candidate_index: a.selected_candidate_index,
        right_selected_candidate_index: b.selected_candidate_index,
        left_cost_usd: a.selected_total_usd,
        right_cost_usd: b.selected_total_usd,
        cost_change_usd: a
            .selected_total_usd
            .zip(b.selected_total_usd)
            .map(|(x, y)| y - x),
        left_utilization: a.current_utilization,
        right_utilization: b.current_utilization,
        utilization_change: a
            .current_utilization
            .zip(b.current_utilization)
            .map(|(x, y)| y - x),
        left_utilization_headroom: left_headroom,
        right_utilization_headroom: right_headroom,
        utilization_headroom_change: left_headroom.zip(right_headroom).map(|(x, y)| y - x),
        left_limiting_location: a.limiting_location,
        right_limiting_location: b.limiting_location,
    }))
}
fn observed_limit(message: &str) -> Option<String> {
    let lower = message.to_lowercase();
    if lower.contains("maximum") || lower.contains("limit") || lower.contains("exceeds") {
        Some(message.into())
    } else {
        None
    }
}
fn diff_values(
    pointer: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    out: &mut Vec<InputChange>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let p = format!("{pointer}/{}", escape_pointer(key));
                diff_values(&p, a.get(key), b.get(key), out);
            }
        }
        _ => out.push(InputChange {
            pointer: if pointer.is_empty() {
                "/".into()
            } else {
                pointer.into()
            },
            before: before.cloned(),
            after: after.cloned(),
        }),
    }
}
fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn strip_sha(hash: &str) -> &str {
    hash.strip_prefix("sha256:").unwrap_or(hash)
}
fn expected_run_input_sha(
    record: &CoupledSearchRunRecord,
    options: &CoupledSearchOptions,
) -> Result<String, StudyError> {
    let bytes = serde_json::to_vec(&(
        &record.case_sha256,
        &record.implementation_sha256,
        options,
        crate::coupled_search::COUPLED_SEARCH_MODEL_ID,
        crate::coupled_search::COUPLED_SEARCH_CHECKER_ID,
        env!("CARGO_PKG_VERSION"),
    ))?;
    Ok(sha256(&bytes))
}
fn now_ms() -> u64 {
    crate::time::SystemTime::now()
        .duration_since(crate::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn variant_payload_bytes(v: &StudyVariant) -> usize {
    v.case_json.len()
        + v.dataset_bundles.iter().map(String::len).sum::<usize>()
        + v.results
            .iter()
            .map(|r| {
                r.record_json.len()
                    + r.source_case_json.len()
                    + r.source_dataset_bundles
                        .iter()
                        .map(String::len)
                        .sum::<usize>()
            })
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRADED_CASE: &str = include_str!("../../../benchmarks/coupled/oc-020.json");
    const MULTI_DATASET_CASE: &str = include_str!("../../../benchmarks/coupled/oc-022.json");
    const SMALL_CASE: &str = include_str!("../../../benchmarks/coupled/oc-019.json");
    const EXTERNAL_BUNDLE: &str =
        include_str!("../../../data/materials/robinson-shanghai-hflt-v3/bundle.json");
    const BASE_BUNDLE: &str =
        include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json");

    #[test]
    fn workspace_preserves_exact_inputs_and_explicit_followup_changes() {
        let mut workspace = StudyWorkspace::new("materials trade study");
        let raw_case = format!("\n{SMALL_CASE}\n");
        let a = workspace
            .add_variant(
                "graded baseline",
                raw_case.clone(),
                vec![],
                CoupledSearchOptions::default(),
            )
            .unwrap();
        let b = workspace.duplicate_variant(&a, "copy").unwrap();
        let before = exact_input_key(workspace.variant(&a).unwrap()).unwrap();
        let before_value: Value = serde_json::from_str(&raw_case).unwrap();
        let proposal = workspace
            .propose_followup(&a, FollowUpAxis::TapesAlongWidth)
            .unwrap();
        assert!(!proposal.calculated);
        assert!(
            !proposal.requirements_changed
                && !proposal.numerical_gates_changed
                && !proposal.limits_changed
        );
        assert!(
            proposal
                .changed_inputs
                .iter()
                .any(|change| change.pointer == "/choices/tapes_along_width")
        );
        let after_value: Value = serde_json::from_str(&proposal.proposed_case_json).unwrap();
        for pointer in ["/requirement", "/limits", "/numerics", "/refined_plan"] {
            assert_eq!(before_value.pointer(pointer), after_value.pointer(pointer));
            assert!(
                !proposal
                    .changed_inputs
                    .iter()
                    .any(|change| change.pointer.starts_with(pointer))
            );
        }
        let strands = workspace
            .propose_followup(&a, FollowUpAxis::StrandsParallel)
            .unwrap();
        let strands_case: Value = serde_json::from_str(&strands.proposed_case_json).unwrap();
        assert_eq!(
            strands_case.pointer("/choices/strands_parallel"),
            Some(&serde_json::json!([1, 2]))
        );
        assert!(strands.preflight.workload.candidate_count > 0);
        assert!(!strands.calculated);
        let revised = proposal.proposed_case_json;
        workspace
            .revise_variant(&b, revised, vec![], CoupledSearchOptions::default())
            .unwrap();
        assert_ne!(
            before,
            exact_input_key(workspace.variant(&b).unwrap()).unwrap()
        );
        let diff = workspace.case_diff(&a, &b).unwrap();
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].pointer, "/choices/tapes_along_width");
        let encoded = workspace.to_json().unwrap();
        let reopened = StudyWorkspace::from_json(&encoded).unwrap();
        assert_eq!(reopened.variant(&a).unwrap().case_json, raw_case);
    }

    #[test]
    fn live_input_key_changes_with_price_options_and_exact_bundle_bytes() {
        let base = StudyVariant {
            id: "a".into(),
            name: "base".into(),
            case_json: SMALL_CASE.into(),
            dataset_bundles: vec![],
            options: CoupledSearchOptions::default(),
            results: vec![],
        };
        let initial = exact_input_key(&base).unwrap();
        let mut price = base.clone();
        let mut case: Value = serde_json::from_str(SMALL_CASE).unwrap();
        case["cost"]["price_usd_per_m"] =
            Value::from(case["cost"]["price_usd_per_m"].as_f64().unwrap() + 0.01);
        price.case_json = serde_json::to_string(&case).unwrap();
        assert_ne!(initial, exact_input_key(&price).unwrap());
        let mut options = base.clone();
        options.options.threads = Some(1);
        assert_ne!(initial, exact_input_key(&options).unwrap());
        let mut bundle_bytes = base;
        bundle_bytes
            .dataset_bundles
            .push(format!("{EXTERNAL_BUNDLE}\n"));
        assert_ne!(initial, exact_input_key(&bundle_bytes).unwrap());
    }

    fn screening_fixture() -> crate::coupled::CandidateResult {
        crate::coupled::CandidateResult {
            current_a: 1.0,
            ampere_turns_a: 1.0,
            status: Status::Pass,
            limiting: Some(crate::coupled::LimitingPoint {
                station: "top".into(),
                tape_index: 2,
                turn_index: 3,
                width_index: 4,
            }),
            min_allowed_screening_a: Some(2.0),
            max_utilization: Some(0.9),
            point_counts: crate::coupled::PointCounts {
                along_current_excluded: 1,
                ..Default::default()
            },
            max_self_field_ratio: 4.0,
            limiting_self_field_ratio: Some(1.2),
            max_transport_self_field_ratio: Some(0.7),
            max_along_current_fraction: 0.4,
            max_refinement_change_t: 0.0,
        }
    }

    #[test]
    fn diagnoses_follow_recorded_gate_semantics_and_locations() {
        let mut case_value: Value = serde_json::from_str(SMALL_CASE).unwrap();
        let baseline = case_value["baseline"].clone();
        case_value["choices"]["turns_along_normal"] =
            serde_json::json!([baseline["turns_along_normal"]]);
        case_value["choices"]["tapes_along_width"] =
            serde_json::json!([baseline["tapes_along_width"]]);
        let case_json = serde_json::to_string(&case_value).unwrap();
        let mut record = coupled_search::run_coupled_search_case(
            &case_json,
            &CoupledSearchOptions { threads: Some(1) },
        )
        .unwrap();
        let mut case = CoupledSearchCase::from_json(&case_json).unwrap();
        case.limits.utilization_limit = 0.8;
        case.limits.max_self_field_ratio = 0.5;
        case.limits.max_along_current_field_fraction = 0.3;
        case.limits.self_field_correction = None;
        case.limits.along_current_model = None;
        record.case = case.clone();
        record.candidates.truncate(1);
        record.candidates[0].requirement_status = Status::Pass;
        record.candidates[0].refinement_status = Status::Pass;
        record.candidates[0].screening = Some(screening_fixture());
        let issues = record_issues(&record);
        let utilization = issues
            .iter()
            .find(|issue| issue.code == "utilization_limit_exceeded")
            .unwrap();
        assert_eq!(utilization.status, Status::Fail);
        assert_eq!(utilization.observed_value, Some(0.9));
        assert_eq!(
            utilization.limiting_location.as_deref(),
            Some("station=top, turn=3, tape=2, width=4")
        );
        let self_field = issues
            .iter()
            .find(|issue| issue.code == "self_field_applicability_limit")
            .unwrap();
        assert_eq!(self_field.status, Status::Inconclusive);
        assert_eq!(self_field.observed_value, Some(1.2));
        let along = issues
            .iter()
            .find(|issue| issue.code == "along_current_query_excluded")
            .unwrap();
        assert_eq!(along.status, Status::Inconclusive);
        assert_eq!(along.limiting_location, None);

        record.case.limits.along_current_model = Some("transverse_bound".into());
        assert!(
            !record_issues(&record)
                .iter()
                .any(|issue| issue.code == "along_current_query_excluded")
        );
        record.case.limits.self_field_correction = Some("critical_state_strip".into());
        assert!(
            !record_issues(&record)
                .iter()
                .any(|issue| issue.code == "self_field_applicability_limit")
        );
        record.case.limits.self_field_correction = Some("uniform_transport".into());
        record.candidates[0]
            .screening
            .as_mut()
            .unwrap()
            .max_transport_self_field_ratio = Some(1.1);
        let uniform = record_issues(&record);
        let issue = uniform
            .iter()
            .find(|issue| issue.code == "uniform_transport_self_field_bound_exceeded")
            .unwrap();
        assert_eq!(issue.status, Status::Inconclusive);
        assert_eq!(issue.limiting_location, None);

        // A conservative lower bound can be accepted by the coupled
        // coverage gate. Do not infer an excluded query from the maximum
        // angle ratio alone when no point was actually excluded.
        let screening = record.candidates[0].screening.as_mut().unwrap();
        screening.point_counts = crate::coupled::PointCounts {
            lower_bound: 3,
            ..Default::default()
        };
        screening.max_along_current_fraction = 0.4;
        record.case.limits.self_field_correction = None;
        record.case.limits.along_current_model = None;
        let lower_bound_issues = record_issues(&record);
        assert!(
            !lower_bound_issues
                .iter()
                .any(|issue| issue.code == "material_coverage_gap"
                    || issue.code == "along_current_query_excluded")
        );

        // The same reported ratio is diagnostic when an actual point was
        // excluded; include the source count in the regression fixture.
        record.candidates[0]
            .screening
            .as_mut()
            .unwrap()
            .point_counts
            .along_current_excluded = 1;
        let excluded_issues = record_issues(&record);
        let coverage = excluded_issues
            .iter()
            .find(|issue| issue.code == "material_coverage_gap")
            .unwrap();
        assert_eq!(coverage.status, Status::Inconclusive);
        assert!(
            excluded_issues
                .iter()
                .any(|issue| issue.code == "along_current_query_excluded")
        );
    }

    #[test]
    fn diagnoses_optional_screen_limits_with_source_units_and_locations() {
        let mut record = coupled_search::run_coupled_search_case(
            SMALL_CASE,
            &CoupledSearchOptions { threads: Some(1) },
        )
        .unwrap();
        let candidate = &mut record.candidates[0];
        candidate.screens = Some(coupled_search::CandidateScreens {
            thermal_margin: Some(coupled_search::ThermalMarginScreenRecord {
                model: "fixture".into(),
                status: Status::Inconclusive,
                min_margin_k: Some(1.5),
                min_margin_is_lower_bound: false,
                limiting: Some(crate::coupled::LimitingPoint {
                    station: "outer".into(),
                    tape_index: 1,
                    turn_index: 2,
                    width_index: 3,
                }),
                points_evaluated: 1,
                points_inconclusive: 1,
            }),
            ac_loss: Some(coupled_search::AcLossScreenRecord {
                model: "fixture".into(),
                status: Status::Fail,
                transport_loss_w: Some(1.0),
                parallel_slab_loss_w: Some(2.0),
                perpendicular_bound_w: Some(3.0),
                total_loss_w: Some(6.0),
                peak_loss_j_per_m_per_cycle: Some(0.1),
                limiting: None,
                points_evaluated: 1,
                points_inconclusive: 0,
            }),
            quench_hotspot: Some(coupled_search::QuenchHotspotScreenRecord {
                model: "fixture".into(),
                status: Status::Inconclusive,
                strand_current_density_a_per_m2: Some(10.0),
                miit_a2s: Some(20.0),
                hotspot_temperature_k: None,
                table_exhausted: true,
            }),
            screening_current: None,
            transition: None,
            quench_transient: None,
        });
        record.case.thermal_margin =
            Some(optcoil_model::coupled_search::ThermalMarginScreen { min_margin_k: 2.0 });
        record.case.ac_loss = Some(optcoil_model::coupled_search::AcLossScreen {
            frequency_hz: 1.0,
            transport_amplitude_fraction: 0.1,
            sc_layer_thickness_m: 1e-6,
            max_loss_w: 5.0,
        });
        record.case.quench_hotspot = Some(optcoil_model::coupled_search::QuenchHotspotScreen {
            dump_time_constant_s: 1.0,
            stabilizer_area_m2: 1e-6,
            quench_function_a2s_per_m4: vec![[1.0, 0.0], [2.0, 1.0]],
            max_hotspot_k: 100.0,
        });

        let issues = record_issues(&record);
        let thermal = issues
            .iter()
            .find(|issue| issue.code == "thermal_margin_screen_unresolved")
            .unwrap();
        assert_eq!(thermal.status, Status::Inconclusive);
        assert_eq!(thermal.observed_value, Some(1.5));
        assert_eq!(thermal.declared_limit, Some(2.0));
        assert_eq!(thermal.unit.as_deref(), Some("K"));
        assert_eq!(
            thermal.limiting_location.as_deref(),
            Some("station=outer, turn=2, tape=1, width=3")
        );

        let loss = issues
            .iter()
            .find(|issue| issue.code == "ac_loss_screen_unresolved")
            .unwrap();
        assert_eq!(loss.status, Status::Fail);
        assert_eq!(loss.observed_value, Some(6.0));
        assert_eq!(loss.declared_limit, Some(5.0));
        assert_eq!(loss.unit.as_deref(), Some("W"));
        assert_eq!(loss.limiting_location, None);

        let hotspot = issues
            .iter()
            .find(|issue| issue.code == "quench_hotspot_screen_unresolved")
            .unwrap();
        assert_eq!(hotspot.status, Status::Inconclusive);
        assert_eq!(hotspot.observed_value, None);
        assert!(
            hotspot
                .data_gap
                .as_deref()
                .unwrap()
                .contains("table coverage")
        );
    }

    #[test]
    fn graded_and_external_multi_dataset_bindings_preflight_without_substitution() {
        let mut workspace = StudyWorkspace::new("dataset binding checks");
        let graded = workspace
            .add_variant(
                "graded",
                GRADED_CASE.into(),
                vec![],
                CoupledSearchOptions::default(),
            )
            .unwrap();
        assert!(workspace.preflight_variant(&graded).unwrap().ready_to_run);

        let external = workspace
            .add_variant(
                "external",
                MULTI_DATASET_CASE.into(),
                vec![EXTERNAL_BUNDLE.into()],
                CoupledSearchOptions::default(),
            )
            .unwrap();
        let preflight = workspace.preflight_variant(&external).unwrap();
        assert!(preflight.ready_to_run, "{:?}", preflight.errors);
        let summary = workspace.compact_summary(&external).unwrap();
        assert!(
            summary
                .dataset_identities
                .iter()
                .any(|identity| identity.dataset_id == "robinson-shanghai-hflt-v3")
        );
        let reopened = StudyWorkspace::from_json(&workspace.to_json().unwrap()).unwrap();
        assert_eq!(
            reopened.variant(&external).unwrap().dataset_bundles[0],
            EXTERNAL_BUNDLE
        );
        assert!(
            workspace
                .revise_variant(
                    &external,
                    "{}".into(),
                    vec![],
                    CoupledSearchOptions::default()
                )
                .is_err()
        );
        assert_eq!(
            workspace.variant(&external).unwrap().case_json,
            MULTI_DATASET_CASE
        );

        let altered_bundle = format!("{EXTERNAL_BUNDLE}\n");
        let original_key = exact_input_key(workspace.variant(&external).unwrap()).unwrap();
        workspace
            .revise_variant(
                &external,
                MULTI_DATASET_CASE.into(),
                vec![altered_bundle],
                CoupledSearchOptions::default(),
            )
            .unwrap();
        assert_ne!(
            original_key,
            exact_input_key(workspace.variant(&external).unwrap()).unwrap()
        );
    }

    #[test]
    fn bounded_mixed_binding_record_keeps_all_historical_bundle_bytes() {
        let mut case: Value = serde_json::from_str(MULTI_DATASET_CASE).unwrap();
        // A deliberately narrow software fixture: retain the graded regional
        // bindings, target and physical limits while reducing sampling cost.
        case["choices"]["turns_along_normal"] = serde_json::json!([2]);
        case["choices"]["tapes_along_width"] = serde_json::json!([2]);
        case["baseline"]["turns_along_normal"] = serde_json::json!(2);
        case["baseline"]["tapes_along_width"] = serde_json::json!(2);
        case["grading"]["regions"][0]["tape_spec_choices"] = serde_json::json!(["base"]);
        case["grading"]["regions"][1]["tape_spec_choices"] = serde_json::json!(["hts-shanghai"]);
        case["baseline"]["tape_spec_ids"] = serde_json::json!(["base", "hts-shanghai"]);
        case["sampling"]["stations"] = serde_json::json!([
            { "id": "s15", "kind": "straight", "x_m": 0.15 },
            { "id": "a45", "kind": "arc", "azimuth_deg": 45.0 }
        ]);
        case["sampling"]["relative_turn_indices"] = serde_json::json!([
            { "kind": "from_start", "offset": 1 },
            { "kind": "from_end", "offset": 0 }
        ]);
        case["sampling"]["width_points"] = serde_json::json!(5);
        case["numerics"]["quadrature_orders"] = serde_json::json!([6, 8]);
        case["refined_plan"]["additional_stations"] = serde_json::json!([
            { "id": "arc_15", "kind": "arc", "azimuth_deg": 15.0 },
            { "id": "arc_30", "kind": "arc", "azimuth_deg": 30.0 },
            { "id": "arc_60", "kind": "arc", "azimuth_deg": 60.0 },
            { "id": "arc_75", "kind": "arc", "azimuth_deg": 75.0 },
            { "id": "straight_015", "kind": "straight", "x_m": 0.15 },
            { "id": "straight_025", "kind": "straight", "x_m": 0.25 }
        ]);
        case["requirement"]["good_field_region"]["points_per_axis"] = Value::from(2);
        let source_case = serde_json::to_string(&case).unwrap();
        let bundles = vec![BASE_BUNDLE.to_owned(), EXTERNAL_BUNDLE.to_owned()];
        let mut workspace = StudyWorkspace::new("bounded mixed-binding evidence");
        let id = workspace
            .add_variant(
                "mixed base and Shanghai",
                source_case.clone(),
                bundles.clone(),
                CoupledSearchOptions { threads: Some(1) },
            )
            .unwrap();
        assert!(workspace.preflight_variant(&id).unwrap().ready_to_run);
        let mut session = StudyEngineSession::default();
        let result_id = session.run_variant(&mut workspace, &id).unwrap();
        let package = workspace.review_package_variant(&id, &result_id).unwrap();
        assert!(
            package
                .artifacts
                .iter()
                .any(|(path, bytes)| path.starts_with("datasets/") && bytes == BASE_BUNDLE)
        );
        assert!(
            package
                .artifacts
                .iter()
                .any(|(path, bytes)| path.starts_with("datasets/") && bytes == EXTERNAL_BUNDLE)
        );

        case["cost"]["price_usd_per_m"] =
            Value::from(case["cost"]["price_usd_per_m"].as_f64().unwrap() + 0.01);
        workspace
            .revise_variant(
                &id,
                serde_json::to_string(&case).unwrap(),
                bundles,
                CoupledSearchOptions { threads: Some(1) },
            )
            .unwrap();
        assert!(
            workspace
                .compact_summary(&id)
                .unwrap()
                .current_binding_result_ids
                .is_empty()
        );
        assert!(workspace.review_package_variant(&id, &result_id).is_ok());
        assert!(StudyWorkspace::from_json(&workspace.to_json().unwrap()).is_ok());
    }

    #[test]
    fn live_session_run_records_reopen_and_old_results_remain_historical() {
        let mut case: Value = serde_json::from_str(include_str!(
            "../../../benchmarks/coupled/oc-031-helix-layer.json"
        ))
        .unwrap();
        let baseline = case.pointer("/baseline").unwrap().clone();
        case["choices"]["turns_along_normal"] = serde_json::json!([baseline["turns_along_normal"]]);
        case["choices"]["tapes_along_width"] = serde_json::json!([baseline["tapes_along_width"]]);
        if !case["choices"]["strands_parallel"].is_null() {
            case["choices"]["strands_parallel"] = serde_json::json!([baseline["strands_parallel"]]);
        }
        let case_json = serde_json::to_string(&case).unwrap();
        let bundles = vec![BASE_BUNDLE.to_owned()];
        let mut workspace = StudyWorkspace::new("small declared field-map study");
        let id = workspace
            .add_variant(
                "one geometry",
                case_json.clone(),
                bundles.clone(),
                CoupledSearchOptions { threads: Some(1) },
            )
            .unwrap();
        let mut session = StudyEngineSession::default();
        let result_id = session.run_variant(&mut workspace, &id).unwrap();
        let package = workspace.review_package_variant(&id, &result_id).unwrap();
        assert!(
            package
                .artifacts
                .iter()
                .any(|(path, text)| path == "datasets/dataset-001.json" && text == BASE_BUNDLE)
        );
        let decision = workspace
            .compact_summary(&id)
            .unwrap()
            .latest_decision
            .unwrap();
        assert_eq!(decision.selected_candidate_index, None);
        assert_eq!(decision.selected_status, Status::NotEvaluated);
        assert!(
            session
                .cached_result(workspace.variant(&id).unwrap())
                .unwrap()
                .is_some()
        );
        let sibling = workspace.duplicate_variant(&id, "imported copy").unwrap();
        let other_session = StudyEngineSession::default();
        assert!(
            other_session
                .cached_result(workspace.variant(&sibling).unwrap())
                .unwrap()
                .is_none()
        );
        let live_record = session
            .cached_result(workspace.variant(&id).unwrap())
            .unwrap()
            .unwrap()
            .to_owned();
        workspace.attach_result(&sibling, live_record).unwrap();
        assert!(
            other_session
                .cached_result(workspace.variant(&sibling).unwrap())
                .unwrap()
                .is_none()
        );
        let comparison = workspace
            .case_diff(&id, &sibling)
            .unwrap()
            .result_comparison
            .unwrap();
        assert_eq!(comparison.left_status, Status::Fail);
        assert_eq!(comparison.right_status, Status::Fail);
        assert_eq!(comparison.left_cost_usd, None);
        assert_eq!(comparison.right_cost_usd, None);
        assert!(
            workspace
                .revise_variant(
                    &id,
                    "{}".into(),
                    bundles.clone(),
                    CoupledSearchOptions { threads: Some(1) }
                )
                .is_err()
        );
        assert_eq!(workspace.variant(&id).unwrap().case_json, case_json);
        assert_eq!(workspace.variant(&id).unwrap().results.len(), 1);
        assert!(matches!(
            session.run_variant_with(&mut workspace, &id, &AtomicBool::new(true), None),
            Err(StudyError::Run(RunError::Cancelled))
        ));
        assert_eq!(workspace.variant(&id).unwrap().results.len(), 1);
        let reopened = StudyWorkspace::from_json(&workspace.to_json().unwrap()).unwrap();
        assert_eq!(reopened.variant(&id).unwrap().results[0].id, result_id);
        let mut tampered: Value = serde_json::from_str(&workspace.to_json().unwrap()).unwrap();
        tampered["variants"][0]["results"][0]["record_json"] = Value::String("{}".into());
        assert!(StudyWorkspace::from_json(&tampered.to_string()).is_err());

        let old_record = workspace.variant(&id).unwrap().results[0]
            .record_json
            .clone();
        let edited = case_json.replace("OC-031", "OC-031 revised label");
        workspace
            .revise_variant(
                &id,
                edited,
                bundles.clone(),
                CoupledSearchOptions { threads: Some(1) },
            )
            .unwrap();
        assert!(workspace.attach_result(&id, old_record).is_err());
        assert!(
            session
                .cached_result(workspace.variant(&id).unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(workspace.variant(&id).unwrap().results.len(), 1);
        let old_package = workspace.review_package_variant(&id, &result_id).unwrap();
        assert!(
            old_package
                .artifacts
                .iter()
                .any(|(path, text)| path == "datasets/dataset-001.json" && text == BASE_BUNDLE)
        );
        assert!(
            workspace
                .compact_summary(&id)
                .unwrap()
                .current_binding_result_ids
                .is_empty()
        );
        assert!(StudyWorkspace::from_json(&workspace.to_json().unwrap()).is_ok());
    }
}
