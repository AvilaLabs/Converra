//! Versioned, bounded application recovery snapshots.
//!
//! JSON documents are retained as strings so recovery does not normalize case,
//! run-record, or workspace evidence while saving a draft of the UI state.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

pub const RECOVERY_SCHEMA_VERSION: u32 = 1;
pub const MAX_RECOVERY_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_AUTHOR_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMPORTER_BYTES: usize = 48 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySnapshot {
    pub schema_version: u32,
    /// Unix epoch milliseconds, supplied by the recovery coordinator.
    pub saved_at_unix_ms: u64,
    /// Exact validated study-workspace JSON from `StudyWorkspace::to_json`.
    pub workspace_json: String,
    pub active_source: Option<ActiveSource>,
    /// Serialized `CaseDraft`; its case text may intentionally be invalid.
    pub author_json: Option<String>,
    /// Serialized importer editor state; malformed values remain editable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importer_json: Option<String>,
    /// Recovery of result panels, unrun study configuration, and external
    /// material needed to rerun a case. Missing on early v1 snapshots.
    #[serde(default)]
    pub artifacts: Option<RecoveryArtifacts>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryArtifacts {
    pub sweep_record_json: Option<String>,
    pub comparison_record_json: Option<String>,
    pub robustness_scenarios_json: Option<String>,
    pub robustness_variant_ids: Vec<String>,
    pub dataset_bundle_jsons: Vec<String>,
    /// A prior result retained as history when it does not belong to the
    /// active case's exact input bytes.
    pub historical_search_record_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveSource {
    pub kind: ActiveSourceKind,
    /// Exact source document opened in the active case/record view.
    pub source_json: String,
    /// Exact current run record shown alongside a case, when present.
    pub current_record_json: Option<String>,
    pub page: String,
    pub edit_variant_id: Option<String>,
    /// A display-only price scenario; it never modifies accepted run data.
    pub display_price_usd_per_m: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveSourceKind {
    CoupledCase,
    Allocation,
    SavedRunRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    TooLarge,
    UnsupportedVersion(u32),
    InvalidJson(String),
    InvalidWorkspace(String),
    InvalidSource(String),
    InvalidDraft(String),
    #[cfg(not(target_arch = "wasm32"))]
    Io(String),
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "recovery snapshot exceeds its size limit"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported recovery snapshot version {version}")
            }
            Self::InvalidJson(error) => write!(f, "invalid recovery JSON: {error}"),
            Self::InvalidWorkspace(error) => write!(f, "invalid recovery workspace: {error}"),
            Self::InvalidSource(error) => write!(f, "invalid recovery source: {error}"),
            Self::InvalidDraft(error) => write!(f, "invalid recovery draft: {error}"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Io(error) => write!(f, "recovery storage error: {error}"),
        }
    }
}

impl std::error::Error for RecoveryError {}

impl RecoverySnapshot {
    #[cfg(test)]
    pub fn new(
        saved_at_unix_ms: u64,
        workspace_json: String,
        active_source: Option<ActiveSource>,
        author_json: Option<String>,
    ) -> Result<Self, RecoveryError> {
        let snapshot = Self {
            schema_version: RECOVERY_SCHEMA_VERSION,
            saved_at_unix_ms,
            workspace_json,
            active_source,
            author_json,
            importer_json: None,
            artifacts: None,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub fn new_with_artifacts(
        saved_at_unix_ms: u64,
        workspace_json: String,
        active_source: Option<ActiveSource>,
        author_json: Option<String>,
        artifacts: Option<RecoveryArtifacts>,
        importer_json: Option<String>,
    ) -> Result<Self, RecoveryError> {
        let snapshot = Self {
            schema_version: RECOVERY_SCHEMA_VERSION,
            saved_at_unix_ms,
            workspace_json,
            active_source,
            author_json,
            importer_json,
            artifacts,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub fn to_json(&self) -> Result<String, RecoveryError> {
        self.validate()?;
        let json =
            serde_json::to_string(self).map_err(|e| RecoveryError::InvalidJson(e.to_string()))?;
        if json.len() > MAX_RECOVERY_BYTES {
            return Err(RecoveryError::TooLarge);
        }
        Ok(json)
    }

    pub fn from_json(json: &str) -> Result<Self, RecoveryError> {
        if json.len() > MAX_RECOVERY_BYTES {
            return Err(RecoveryError::TooLarge);
        }
        let snapshot: Self =
            serde_json::from_str(json).map_err(|e| RecoveryError::InvalidJson(e.to_string()))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), RecoveryError> {
        if self.schema_version != RECOVERY_SCHEMA_VERSION {
            return Err(RecoveryError::UnsupportedVersion(self.schema_version));
        }
        if self.workspace_json.len() > MAX_WORKSPACE_BYTES {
            return Err(RecoveryError::TooLarge);
        }
        let payload_bytes = self.workspace_json.len()
            + self.active_source.as_ref().map_or(0, |source| {
                source.source_json.len()
                    + source.current_record_json.as_ref().map_or(0, String::len)
            })
            + self.author_json.as_ref().map_or(0, String::len)
            + self.importer_json.as_ref().map_or(0, String::len)
            + self
                .artifacts
                .as_ref()
                .map_or(0, RecoveryArtifacts::payload_bytes);
        if payload_bytes > MAX_RECOVERY_BYTES {
            return Err(RecoveryError::TooLarge);
        }
        optcoil_search::study::StudyWorkspace::from_json(&self.workspace_json)
            .map_err(|e| RecoveryError::InvalidWorkspace(e.to_string()))?;
        if let Some(source) = &self.active_source {
            if source.source_json.len() > MAX_SOURCE_BYTES
                || source
                    .current_record_json
                    .as_ref()
                    .is_some_and(|s| s.len() > MAX_SOURCE_BYTES)
                || source.page.len() > 128
                || source
                    .edit_variant_id
                    .as_ref()
                    .is_some_and(|s| s.len() > 256)
                || source
                    .display_price_usd_per_m
                    .is_some_and(|p| !p.is_finite() || p <= 0.0)
            {
                return Err(RecoveryError::TooLarge);
            }
            validate_active_source(source)?;
        }
        if let Some(draft) = &self.author_json {
            if draft.len() > MAX_AUTHOR_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            // Deserialization checks the draft DTO shape only. Invalid edits
            // to its case JSON are retained and remain editable.
            crate::author::CaseDraft::from_recovery_json(draft)
                .map_err(RecoveryError::InvalidDraft)?;
        }
        if let Some(importer_json) = &self.importer_json {
            if importer_json.len() > MAX_IMPORTER_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            crate::importer::ImportDraft::from_recovery_json(importer_json)
                .map_err(RecoveryError::InvalidDraft)?;
        }
        if let Some(artifacts) = &self.artifacts {
            artifacts.validate()?;
        }
        Ok(())
    }

    /// Whether a case editor snapshot still points at the exact selected
    /// workspace variant source. A stale source can still be recovered as
    /// historical evidence, but must not be displayed as the current case.
    pub fn editor_source_matches_workspace(&self) -> Result<bool, RecoveryError> {
        let Some(source) = &self.active_source else {
            return Ok(true);
        };
        let Some(variant_id) = source.edit_variant_id.as_deref() else {
            return Ok(true);
        };
        if source.kind != ActiveSourceKind::CoupledCase {
            return Ok(false);
        }
        let workspace = optcoil_search::study::StudyWorkspace::from_json(&self.workspace_json)
            .map_err(|e| RecoveryError::InvalidWorkspace(e.to_string()))?;
        Ok(workspace
            .variants
            .iter()
            .find(|variant| variant.id == variant_id)
            .is_some_and(|variant| variant.case_json == source.source_json))
    }

    pub fn sweep_matches_active_source(&self) -> Result<Option<bool>, RecoveryError> {
        let Some(sweep_json) = self
            .artifacts
            .as_ref()
            .and_then(|artifacts| artifacts.sweep_record_json.as_deref())
        else {
            return Ok(None);
        };
        let Some(source) = &self.active_source else {
            return Ok(None);
        };
        if source.kind != ActiveSourceKind::CoupledCase {
            return Ok(Some(false));
        }
        let sweep: optcoil_search::sensitivity::SensitivitySweepRecord =
            serde_json::from_str(sweep_json)
                .map_err(|e| RecoveryError::InvalidSource(format!("sensitivity record: {e}")))?;
        Ok(Some(
            sweep.case_sha256 == sha256_hex(source.source_json.as_bytes()),
        ))
    }
}

impl RecoveryArtifacts {
    fn payload_bytes(&self) -> usize {
        self.sweep_record_json.as_ref().map_or(0, String::len)
            + self.comparison_record_json.as_ref().map_or(0, String::len)
            + self
                .robustness_scenarios_json
                .as_ref()
                .map_or(0, String::len)
            + self
                .robustness_variant_ids
                .iter()
                .map(String::len)
                .sum::<usize>()
            + self
                .dataset_bundle_jsons
                .iter()
                .map(String::len)
                .sum::<usize>()
            + self
                .historical_search_record_json
                .as_ref()
                .map_or(0, String::len)
    }

    fn validate(&self) -> Result<(), RecoveryError> {
        if self.robustness_variant_ids.len() > 64
            || self
                .robustness_variant_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 256)
            || self.dataset_bundle_jsons.len() > 64
            || self.payload_bytes() > MAX_RECOVERY_BYTES
        {
            return Err(RecoveryError::TooLarge);
        }
        if let Some(json) = &self.sweep_record_json {
            if json.len() > MAX_SOURCE_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            let record: optcoil_search::sensitivity::SensitivitySweepRecord =
                serde_json::from_str(json).map_err(|e| {
                    RecoveryError::InvalidSource(format!("sensitivity record: {e}"))
                })?;
            if record.schema != optcoil_search::sensitivity::SENSITIVITY_RECORD_SCHEMA {
                return Err(RecoveryError::InvalidSource(
                    "unsupported sensitivity record schema".into(),
                ));
            }
            for (index, point) in record.points.iter().enumerate() {
                if point.case_json.is_empty() {
                    if point.error.is_none() {
                        return Err(RecoveryError::InvalidSource(format!(
                            "sensitivity point {index} has no case source or error"
                        )));
                    }
                } else if point.case_sha256.is_empty() {
                    let legitimate_pre_run_failure = point.error.is_some()
                        && point.search_status == optcoil_model::Status::NotEvaluated
                        && point.agreement_status == optcoil_model::Status::NotEvaluated
                        && point.optimum.is_none()
                        && point.optimum_total_usd.is_none()
                        && point.baseline_total_usd.is_none()
                        && point.savings_usd.is_none()
                        && point.savings_percent.is_none();
                    if !legitimate_pre_run_failure {
                        return Err(RecoveryError::InvalidSource(format!(
                            "sensitivity point {index} has no case hash and is not an unresolved pre-run failure"
                        )));
                    }
                } else if point.case_sha256 != sha256_hex(point.case_json.as_bytes()) {
                    return Err(RecoveryError::InvalidSource(format!(
                        "sensitivity point {index} case hash does not match its exact source"
                    )));
                }
            }
        }
        if let Some(json) = &self.comparison_record_json {
            if json.len() > MAX_SOURCE_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            validate_saved_record(json)?;
        }
        if let Some(json) = &self.robustness_scenarios_json {
            if json.len() > MAX_AUTHOR_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            // Keep this typed as editor data so restore can install it into
            // the coordinator. Do not apply execution-time semantic checks:
            // incomplete IDs and invalid multipliers remain editable.
            let scenarios: Vec<optcoil_search::robustness::RobustnessScenario> =
                serde_json::from_str(json).map_err(|e| {
                    RecoveryError::InvalidDraft(format!("robustness scenarios: {e}"))
                })?;
            if scenarios.len() > optcoil_search::robustness::MAX_ROBUSTNESS_SCENARIOS {
                return Err(RecoveryError::TooLarge);
            }
        }
        for json in &self.dataset_bundle_jsons {
            if json.len() > MAX_SOURCE_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            optcoil_model::material::MaterialBundle::from_json(json)
                .map_err(|e| RecoveryError::InvalidSource(format!("dataset bundle: {e}")))?;
        }
        if let Some(json) = &self.historical_search_record_json {
            if json.len() > MAX_SOURCE_BYTES {
                return Err(RecoveryError::TooLarge);
            }
            validate_saved_record(json)?;
        }
        Ok(())
    }
}

fn validate_active_source(source: &ActiveSource) -> Result<(), RecoveryError> {
    use optcoil_model::coupled_search::CoupledSearchCase;
    match source.kind {
        ActiveSourceKind::CoupledCase => {
            let case = CoupledSearchCase::from_json(&source.source_json)
                .map_err(|e| RecoveryError::InvalidSource(format!("coupled case: {e}")))?;
            if let Some(record_json) = &source.current_record_json {
                validate_coupled_record(record_json, Some(&source.source_json), Some(&case))?;
            }
        }
        ActiveSourceKind::Allocation => {
            let case = optcoil_model::Case::from_json(&source.source_json)
                .map_err(|e| RecoveryError::InvalidSource(format!("allocation case: {e}")))?;
            if let Some(record_json) = &source.current_record_json {
                let record: optcoil_search::RunRecord = serde_json::from_str(record_json)
                    .map_err(|e| RecoveryError::InvalidSource(format!("allocation record: {e}")))?;
                record
                    .case
                    .validate()
                    .map_err(|e| RecoveryError::InvalidSource(format!("record case: {e}")))?;
                validate_allocation_record(&record, Some(&source.source_json))?;
                if serde_json::to_value(&record.case).ok() != serde_json::to_value(&case).ok() {
                    return Err(RecoveryError::InvalidSource(
                        "allocation run record is not bound to the exact active case".into(),
                    ));
                }
            }
        }
        ActiveSourceKind::SavedRunRecord => {
            if source.current_record_json.is_some() {
                return Err(RecoveryError::InvalidSource(
                    "record-only source cannot contain a second current record".into(),
                ));
            }
            validate_saved_record(&source.source_json)?;
        }
    }
    Ok(())
}

fn validate_coupled_record(
    record_json: &str,
    case_json: Option<&str>,
    case: Option<&optcoil_model::coupled_search::CoupledSearchCase>,
) -> Result<(), RecoveryError> {
    let record: optcoil_search::coupled_search::CoupledSearchRunRecord =
        serde_json::from_str(record_json)
            .map_err(|e| RecoveryError::InvalidSource(format!("coupled run record: {e}")))?;
    record
        .case
        .validate()
        .map_err(|e| RecoveryError::InvalidSource(format!("record case: {e}")))?;
    if let (Some(case_json), Some(case)) = (case_json, case)
        && (record.case_sha256 != sha256_hex(case_json.as_bytes())
            || serde_json::to_value(&record.case).ok() != serde_json::to_value(case).ok())
    {
        return Err(RecoveryError::InvalidSource(
            "coupled run record is stale or contradicts the exact active case".into(),
        ));
    }
    check_record_verification(record_json, case_json)
}

fn validate_saved_record(record_json: &str) -> Result<(), RecoveryError> {
    if let Ok(record) =
        serde_json::from_str::<optcoil_search::coupled_search::CoupledSearchRunRecord>(record_json)
    {
        record
            .case
            .validate()
            .map_err(|e| RecoveryError::InvalidSource(format!("record case: {e}")))?;
        return check_record_verification(record_json, None);
    }
    let record: optcoil_search::RunRecord = serde_json::from_str(record_json)
        .map_err(|e| RecoveryError::InvalidSource(format!("saved run record: {e}")))?;
    record
        .case
        .validate()
        .map_err(|e| RecoveryError::InvalidSource(format!("record case: {e}")))?;
    validate_allocation_record(&record, None)?;
    Ok(())
}

fn validate_allocation_record(
    record: &optcoil_search::RunRecord,
    case_json: Option<&str>,
) -> Result<(), RecoveryError> {
    let identity_ok = record.schema_version == optcoil_search::RUN_SCHEMA_VERSION
        && !record.optcoil_version.is_empty()
        && !record.input_sha256.is_empty()
        && !record.screening_model_id.is_empty()
        && !record.checker_id.is_empty()
        && record.search_id == optcoil_search::SEARCH_ID;
    let case_hash_ok = case_json.map_or_else(
        || {
            record.case_sha256.len() == 64
                && record
                    .case_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        },
        |json| record.case_sha256 == sha256_hex(json.as_bytes()),
    );
    let counts_sum_ok = record
        .screening_pass_candidates
        .checked_add(record.inconclusive_candidates)
        .is_some_and(|sum| sum <= record.evaluated_candidates);
    let counts_ok = record.evaluated_candidates <= record.search_space_size
        && record.screening_pass_candidates <= record.evaluated_candidates
        && record.inconclusive_candidates <= record.evaluated_candidates
        && counts_sum_ok;
    let assessments_ok = record.baseline.screening_status == optcoil_model::Status::Pass
        && record.best.screening_status == optcoil_model::Status::Pass
        && record.baseline.candidate.allocations == record.case.baseline.allocations;
    if !identity_ok || !case_hash_ok || !counts_ok || !assessments_ok {
        return Err(RecoveryError::InvalidSource(
            "allocation run record identities or assessment summary are inconsistent".into(),
        ));
    }
    Ok(())
}

fn check_record_verification(
    record_json: &str,
    case_json: Option<&str>,
) -> Result<(), RecoveryError> {
    let checks =
        optcoil_search::verify::verify_record_checks(record_json, case_json, None, None, &[])
            .map_err(|e| RecoveryError::InvalidSource(format!("run record verification: {e}")))?;
    if let Some(failed) = checks
        .iter()
        .find(|check| check.outcome == optcoil_search::verify::Outcome::Fail)
    {
        return Err(RecoveryError::InvalidSource(format!(
            "run record {} failed: {}",
            failed.name, failed.detail
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read and validate the bounded native recovery file.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_snapshot(path: &Path) -> Result<RecoverySnapshot, RecoveryError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| RecoveryError::Io(e.to_string()))?;
    let mut bytes = Vec::new();
    file.take((MAX_RECOVERY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| RecoveryError::Io(e.to_string()))?;
    if bytes.len() > MAX_RECOVERY_BYTES {
        return Err(RecoveryError::TooLarge);
    }
    let json =
        std::str::from_utf8(&bytes).map_err(|e| RecoveryError::InvalidJson(e.to_string()))?;
    RecoverySnapshot::from_json(json)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn discard_snapshot(path: &Path) -> Result<(), RecoveryError> {
    let _lock = recovery_file_lock(path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RecoveryError::Io(error.to_string())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn discard_snapshot_checked(
    path: &Path,
    expected_saved_at: Option<u64>,
) -> Result<(), RecoveryError> {
    let _lock = recovery_file_lock(path)?;
    check_expected_snapshot(path, Some(expected_saved_at))?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RecoveryError::Io(error.to_string())),
    }
}

/// Write via a sibling temporary file and rename. Any serialization or write
/// failure leaves the previous recovery file intact.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
pub fn write_snapshot_atomic(
    path: &Path,
    snapshot: &RecoverySnapshot,
) -> Result<(), RecoveryError> {
    write_snapshot_inner(path, snapshot, None)
}

/// Compare-and-swap a recovery write against the version loaded by this
/// window. `None` expects no stored draft; `Some(timestamp)` expects exactly
/// that saved version. This prevents one window overwriting another's work.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_snapshot_checked(
    path: &Path,
    snapshot: &RecoverySnapshot,
    expected_saved_at: Option<u64>,
) -> Result<(), RecoveryError> {
    write_snapshot_inner(path, snapshot, Some(expected_saved_at))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_snapshot_inner(
    path: &Path,
    snapshot: &RecoverySnapshot,
    expected_saved_at: Option<Option<u64>>,
) -> Result<(), RecoveryError> {
    use std::io::Write;
    let json = snapshot.to_json()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| RecoveryError::Io("recovery path has no file name".into()))?;
    let _lock = recovery_file_lock(path)?;
    if let Some(expected) = expected_saved_at {
        check_expected_snapshot(path, Some(expected))?;
    } else if path.exists() {
        let existing = read_snapshot(path)?;
        if existing.saved_at_unix_ms >= snapshot.saved_at_unix_ms {
            return Err(RecoveryError::Io(
                "Another Converra window saved newer work; export your current study before replacing that draft.".into(),
            ));
        }
    }
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (temporary, mut file) = (0..128)
        .find_map(|_| {
            let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{}.{}.{}.{}.tmp",
                name.to_string_lossy(),
                std::process::id(),
                snapshot.saved_at_unix_ms,
                sequence
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .ok_or_else(|| RecoveryError::Io("unable to allocate a unique temporary file".into()))?
        .map_err(|error| RecoveryError::Io(error.to_string()))?;
    let write_result = (|| {
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temporary);
        return Err(RecoveryError::Io(error.to_string()));
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn recovery_file_lock(path: &Path) -> Result<std::fs::File, RecoveryError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| RecoveryError::Io("recovery path has no file name".into()))?;
    let lock_path = parent.join(format!(".{}.lock", name.to_string_lossy()));
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| RecoveryError::Io(error.to_string()))?;
    lock.lock()
        .map_err(|error| RecoveryError::Io(error.to_string()))?;
    Ok(lock)
}

#[cfg(not(target_arch = "wasm32"))]
fn check_expected_snapshot(
    path: &Path,
    expected_saved_at: Option<Option<u64>>,
) -> Result<(), RecoveryError> {
    let Some(expected) = expected_saved_at else {
        return Ok(());
    };
    let actual = if path.exists() {
        Some(read_snapshot(path)?.saved_at_unix_ms)
    } else {
        None
    };
    if actual != expected {
        return Err(RecoveryError::Io(
            "Another Converra window saved newer work; export your current study before replacing that draft.".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_search::study::StudyWorkspace;

    fn snapshot() -> RecoverySnapshot {
        RecoverySnapshot::new(
            1_800_000_000_000,
            StudyWorkspace::new("recovery test").to_json().unwrap(),
            Some(ActiveSource {
                kind: ActiveSourceKind::CoupledCase,
                source_json: optcoil_model::coupled_search::OC008_JSON.into(),
                current_record_json: None,
                page: "search".into(),
                edit_variant_id: None,
                display_price_usd_per_m: Some(75.0),
            }),
            None,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_preserves_source_strings_and_display_state() {
        let original = snapshot();
        let encoded = original.to_json().unwrap();
        let decoded = RecoverySnapshot::from_json(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn importer_draft_round_trips_invalid_editor_values_but_requires_json_object() {
        let mut importer_json: serde_json::Value = serde_json::from_str(
            &crate::importer::ImportDraft::default()
                .recovery_json()
                .unwrap(),
        )
        .unwrap();
        importer_json["uncertainty"] = serde_json::json!("uncertainty still being entered");
        importer_json["n_constant"] = serde_json::json!("not a parsed number yet");
        let mut original = snapshot();
        original.importer_json = Some(importer_json.to_string());
        let encoded = original.to_json().unwrap();
        let decoded = RecoverySnapshot::from_json(&encoded).unwrap();
        assert_eq!(decoded.importer_json, original.importer_json);
        let restored = crate::importer::ImportDraft::from_recovery_json(
            decoded.importer_json.as_deref().unwrap(),
        )
        .unwrap();
        let restored_json: serde_json::Value =
            serde_json::from_str(&restored.recovery_json().unwrap()).unwrap();
        assert_eq!(
            restored_json["uncertainty"],
            "uncertainty still being entered"
        );
        assert_eq!(restored_json["n_constant"], "not a parsed number yet");

        let mut invalid_shape = snapshot();
        invalid_shape.importer_json = Some("[]".into());
        assert!(matches!(
            invalid_shape.validate(),
            Err(RecoveryError::InvalidDraft(_))
        ));
        invalid_shape.importer_json = Some("{ unfinished".into());
        assert!(matches!(
            invalid_shape.validate(),
            Err(RecoveryError::InvalidDraft(_))
        ));
        invalid_shape.importer_json = Some(r#"{"file_base64":""}"#.into());
        assert!(matches!(
            invalid_shape.validate(),
            Err(RecoveryError::InvalidDraft(_))
        ));
    }

    #[test]
    fn sensitivity_point_hash_must_match_its_preserved_case_json() {
        use std::sync::atomic::AtomicBool;

        let dataset = optcoil_model::material::MaterialDataset::from_csv(
            optcoil_model::material::SUPERPOWER_LOWFIELD_METADATA,
            optcoil_model::material::SUPERPOWER_LOWFIELD_CSV,
        )
        .unwrap();
        let datasets = std::collections::BTreeMap::from([(dataset.metadata.id.clone(), dataset)]);
        let spec = r#"{"schema":"optcoil-sensitivity/v1","id":"recovery-check","provenance":"Recovery identity check","axes":[{"kind":"temperature_k","values":[4.0]}]}"#;
        let record = optcoil_search::sensitivity::run_sensitivity_sweep_with_datasets(
            optcoil_model::coupled_search::OC008_JSON,
            spec,
            &optcoil_search::coupled_search::CoupledSearchOptions { threads: Some(1) },
            &datasets,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(record.points[0].error.is_some());
        assert!(!record.points[0].case_json.is_empty());

        let mut original = snapshot();
        original.artifacts = Some(RecoveryArtifacts {
            sweep_record_json: Some(serde_json::to_string(&record).unwrap()),
            ..RecoveryArtifacts::default()
        });
        assert!(original.validate().is_ok());

        let mut tampered = record.clone();
        tampered.points[0].case_json.push(' ');
        original.artifacts.as_mut().unwrap().sweep_record_json =
            Some(serde_json::to_string(&tampered).unwrap());
        assert!(matches!(
            original.validate(),
            Err(RecoveryError::InvalidSource(_))
        ));

        // The runner also emits pre-run mutation failures with these exact
        // unavailable-evidence fields and no point hash.
        let mut pre_run_failure = record;
        pre_run_failure.points[0].case_sha256.clear();
        pre_run_failure.points[0].search_status = optcoil_model::Status::NotEvaluated;
        pre_run_failure.points[0].agreement_status = optcoil_model::Status::NotEvaluated;
        pre_run_failure.points[0].optimum = None;
        pre_run_failure.points[0].optimum_total_usd = None;
        pre_run_failure.points[0].baseline_total_usd = None;
        pre_run_failure.points[0].savings_usd = None;
        pre_run_failure.points[0].savings_percent = None;
        original.artifacts.as_mut().unwrap().sweep_record_json =
            Some(serde_json::to_string(&pre_run_failure).unwrap());
        assert!(original.validate().is_ok());
    }

    #[test]
    fn rejects_corrupt_oversized_and_unknown_version_snapshots() {
        assert!(matches!(
            RecoverySnapshot::from_json("{"),
            Err(RecoveryError::InvalidJson(_))
        ));
        assert_eq!(
            RecoverySnapshot::from_json(&" ".repeat(MAX_RECOVERY_BYTES + 1)),
            Err(RecoveryError::TooLarge)
        );
        let mut value: serde_json::Value =
            serde_json::from_str(&snapshot().to_json().unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(99);
        assert_eq!(
            RecoverySnapshot::from_json(&value.to_string()),
            Err(RecoveryError::UnsupportedVersion(99))
        );
        let mut legacy: serde_json::Value =
            serde_json::from_str(&snapshot().to_json().unwrap()).unwrap();
        legacy.as_object_mut().unwrap().remove("artifacts");
        assert!(RecoverySnapshot::from_json(&legacy.to_string()).is_ok());
    }

    #[test]
    fn artifacts_round_trip_and_reject_malformed_typed_configuration() {
        let scenarios = r#"[{"id":"","name":"unfinished","price_multipliers":{"dataset-x":-1.0},"ic_multipliers":{},"temperature_offset_k":0.0}]"#;
        let artifacts = RecoveryArtifacts {
            robustness_scenarios_json: Some(scenarios.into()),
            robustness_variant_ids: vec!["variant-0001".into()],
            ..RecoveryArtifacts::default()
        };
        let mut stored = snapshot();
        stored.artifacts = Some(artifacts);
        let restored = RecoverySnapshot::from_json(&stored.to_json().unwrap()).unwrap();
        assert_eq!(
            restored
                .artifacts
                .unwrap()
                .robustness_scenarios_json
                .as_deref(),
            Some(scenarios)
        );

        let mut empty_configuration = snapshot();
        empty_configuration.artifacts = Some(RecoveryArtifacts {
            robustness_scenarios_json: Some("[]".into()),
            ..RecoveryArtifacts::default()
        });
        assert!(RecoverySnapshot::from_json(&empty_configuration.to_json().unwrap()).is_ok());

        let mut malformed = snapshot();
        malformed.artifacts = Some(RecoveryArtifacts {
            robustness_scenarios_json: Some("[{ invalid scenario]".into()),
            ..RecoveryArtifacts::default()
        });
        assert!(matches!(
            malformed.validate(),
            Err(RecoveryError::InvalidDraft(_))
        ));
    }

    #[test]
    fn reports_stale_editor_identity_without_discarding_the_snapshot() {
        let mut workspace = StudyWorkspace::new("recovery test");
        let mut snapshot = snapshot();
        snapshot.workspace_json = workspace.to_json().unwrap();
        let active = snapshot.active_source.as_mut().unwrap();
        active.edit_variant_id = Some("variant-1".into());
        active.source_json = optcoil_model::coupled_search::OC008_JSON.into();
        assert!(!snapshot.editor_source_matches_workspace().unwrap());
        workspace.name = "renamed".into();
        snapshot.workspace_json = workspace.to_json().unwrap();
        assert!(RecoverySnapshot::from_json(&snapshot.to_json().unwrap()).is_ok());
    }

    #[test]
    fn rejects_invalid_physical_cases_and_mismatched_current_record_identity() {
        let mut invalid = snapshot();
        invalid.active_source.as_mut().unwrap().source_json =
            r#"{"schema":"optcoil-coupled-search/v24","requirement":{"b_target_t":-1}}"#.into();
        assert!(matches!(
            invalid.validate(),
            Err(RecoveryError::InvalidSource(_))
        ));

        let case = optcoil_model::Case::demo().unwrap();
        let exact_case_json = serde_json::to_string(&case).unwrap();
        let record =
            optcoil_search::run(&case, &optcoil_search::SearchOptions { max_evaluations: 1 })
                .unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        let active = ActiveSource {
            kind: ActiveSourceKind::Allocation,
            source_json: exact_case_json.clone(),
            current_record_json: Some(record_json),
            page: "search".into(),
            edit_variant_id: None,
            display_price_usd_per_m: None,
        };
        let mut snapshot = RecoverySnapshot::new(
            1,
            StudyWorkspace::new("record binding").to_json().unwrap(),
            Some(active.clone()),
            None,
        )
        .unwrap();
        snapshot.active_source.as_mut().unwrap().source_json = format!(" {exact_case_json}");
        assert!(matches!(
            snapshot.validate(),
            Err(RecoveryError::InvalidSource(_))
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_atomic_storage_round_trips_and_replaces_previous_file() {
        let dir = std::env::temp_dir().join(format!("converra-recovery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recovery.json");
        let first = snapshot();
        write_snapshot_atomic(&path, &first).unwrap();
        assert_eq!(read_snapshot(&path).unwrap(), first);
        let mut older = first.clone();
        older.saved_at_unix_ms -= 1;
        assert!(write_snapshot_atomic(&path, &older).is_err());
        assert_eq!(read_snapshot(&path).unwrap(), first);
        let mut second = snapshot();
        second.saved_at_unix_ms += 1;
        write_snapshot_atomic(&path, &second).unwrap();
        assert_eq!(read_snapshot(&path).unwrap(), second);
        let mut invalid = second.clone();
        invalid.schema_version += 1;
        assert!(write_snapshot_atomic(&path, &invalid).is_err());
        assert_eq!(read_snapshot(&path).unwrap(), second);
        let mut other_window = second.clone();
        other_window.saved_at_unix_ms += 1;
        assert!(write_snapshot_checked(&path, &other_window, None).is_err());
        assert!(
            write_snapshot_checked(&path, &other_window, Some(first.saved_at_unix_ms)).is_err()
        );
        assert_eq!(read_snapshot(&path).unwrap(), second);
        write_snapshot_checked(&path, &other_window, Some(second.saved_at_unix_ms)).unwrap();
        assert_eq!(read_snapshot(&path).unwrap(), other_window);
        assert!(discard_snapshot_checked(&path, Some(second.saved_at_unix_ms)).is_err());
        discard_snapshot_checked(&path, Some(other_window.saved_at_unix_ms)).unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
