//! Headless decision summaries, case diffs, and portable review packages.
//!
//! Review artifacts preserve the raw bytes that established case and
//! record identity. A package is evidence organization, not a new physics
//! run or acceptance decision.

use std::collections::{BTreeMap, BTreeSet};

use optcoil_model::{Status, material::MaterialBundle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{RunError, bom, coupled_search::CoupledSearchRunRecord, report, verify};

pub const REVIEW_PACKAGE_SCHEMA: &str = "optcoil-review-package/v1";
pub const REVIEW_SUMMARY_SCHEMA: &str = "optcoil-decision-summary/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostComponents {
    pub conductor_usd: f64,
    pub scrap_usd: f64,
    pub assembly_usd: f64,
    pub joints_usd: f64,
    pub total_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionSummary {
    pub schema: String,
    pub source_record_sha256: String,
    pub source_case_sha256: String,
    pub selected_candidate_index: Option<usize>,
    pub selected_geometry: Option<crate::coupled_search::CandidateGeometry>,
    pub selected_status: Status,
    pub baseline_candidate_index: usize,
    pub baseline_total_usd: f64,
    pub selected_total_usd: Option<f64>,
    pub savings_usd: Option<f64>,
    pub current_utilization: Option<f64>,
    pub utilization_limit: f64,
    pub limiting_location: Option<String>,
    pub unresolved_gates: Vec<String>,
    pub cost_components: Option<CostComponents>,
    pub price_basis: Vec<String>,
    pub limitations: Vec<String>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputChange {
    pub pointer: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPackageManifest {
    pub schema: String,
    pub package_schema: String,
    pub case: PackageFile,
    pub run_record: PackageFile,
    pub identities: PackageIdentities,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageIdentities {
    pub coupled_search_model_id: String,
    pub coupled_search_checker_id: String,
    pub dataset_id: String,
    pub dataset_csv_sha256: String,
}

/// Build a compact interpretation of the record's own selection. The
/// record's original `best_index` remains the selection authority.
pub fn decision_summary(record_json: &str) -> Result<DecisionSummary, RunError> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("review: run record parse: {e}")))?;
    let baseline = record
        .candidates
        .get(record.baseline_index)
        .ok_or_else(|| {
            RunError::Invalid(format!(
                "review: baseline_index {} is outside {} candidates",
                record.baseline_index,
                record.candidates.len()
            ))
        })?;
    let selected = record
        .best_index
        .map(|i| {
            record.candidates.get(i).ok_or_else(|| {
                RunError::Invalid(format!(
                    "review: best_index {i} is outside {} candidates",
                    record.candidates.len()
                ))
            })
        })
        .transpose()?;
    let mut unresolved_gates = Vec::new();
    if record.search_status != Status::Pass {
        unresolved_gates.push(format!("search status is {:?}", record.search_status));
    }
    if record.acceptance.agreement_status != Status::Pass {
        unresolved_gates.push(format!(
            "source acceptance agreement is {:?}",
            record.acceptance.agreement_status
        ));
    }
    if let Some(candidate) = selected {
        for (name, status) in [
            ("candidate", candidate.status),
            ("requirement", candidate.requirement_status),
            ("field refinement", candidate.refinement_status),
            ("numerical", candidate.numerical_status),
        ] {
            if status != Status::Pass {
                unresolved_gates.push(format!("selected {name} status is {status:?}"));
            }
        }
        if candidate.mechanical_feasible == Some(false) {
            unresolved_gates.push("selected candidate exceeds a declared mechanical bound".into());
        }
        if record.case.mechanical.is_some() && candidate.mechanical_feasible.is_none() {
            unresolved_gates.push("mechanical feasibility was not evaluated".into());
        }
        match &candidate.screening {
            Some(screening) => {
                if screening.status != Status::Pass {
                    unresolved_gates.push(format!(
                        "current-capacity screening is {:?}",
                        screening.status
                    ));
                }
                let counts = &screening.point_counts;
                if counts.unsupported > 0 {
                    unresolved_gates.push(format!(
                        "selected candidate has {} sampled point(s) outside material data coverage",
                        counts.unsupported
                    ));
                }
                if counts.lower_bound > 0 {
                    unresolved_gates.push(format!(
                        "selected candidate has {} lower-bound point(s) without determined capacity",
                        counts.lower_bound
                    ));
                }
                if counts.along_current_excluded > 0 {
                    unresolved_gates.push(format!(
                        "selected candidate has {} excluded along-current point(s)",
                        counts.along_current_excluded
                    ));
                }
                if screening.max_self_field_ratio > record.case.limits.max_self_field_ratio {
                    unresolved_gates.push(format!(
                        "selected candidate self-field ratio {:.4} exceeds declared limit {:.4}",
                        screening.max_self_field_ratio, record.case.limits.max_self_field_ratio
                    ));
                }
                if screening.max_along_current_fraction
                    > record.case.limits.max_along_current_field_fraction
                {
                    unresolved_gates.push(format!(
                        "selected candidate along-current field fraction {:.4} exceeds declared limit {:.4}",
                        screening.max_along_current_fraction,
                        record.case.limits.max_along_current_field_fraction
                    ));
                }
                if screening
                    .max_utilization
                    .is_some_and(|u| u > record.case.limits.utilization_limit)
                {
                    unresolved_gates.push(format!(
                        "selected candidate utilization {:.4} exceeds declared limit {:.4}",
                        screening.max_utilization.unwrap_or_default(),
                        record.case.limits.utilization_limit
                    ));
                }
            }
            None => unresolved_gates.push("current-capacity screening was not evaluated".into()),
        }
        if let Some(screens) = &candidate.screens {
            for (name, status) in [
                (
                    "thermal margin",
                    screens.thermal_margin.as_ref().map(|s| s.status),
                ),
                ("AC loss", screens.ac_loss.as_ref().map(|s| s.status)),
                (
                    "quench hotspot",
                    screens.quench_hotspot.as_ref().map(|s| s.status),
                ),
                (
                    "screening current",
                    screens.screening_current.as_ref().map(|s| s.status),
                ),
                ("transition", screens.transition.as_ref().map(|s| s.status)),
                (
                    "quench transient",
                    screens.quench_transient.as_ref().map(|s| s.status),
                ),
            ] {
                if let Some(status) = status
                    && status != Status::Pass
                {
                    unresolved_gates.push(format!("declared {name} screen is {status:?}"));
                }
            }
        }
        append_declared_screen_gaps(&record, candidate, &mut unresolved_gates);
    } else {
        unresolved_gates.push(
            "the source record has no selected PASS candidate; this search result does not by itself prove physical infeasibility".into(),
        );
        for candidate in record
            .candidates
            .iter()
            .filter(|c| c.status != Status::Pass)
        {
            unresolved_gates.push(format!(
                "candidate {} status is {:?}",
                candidate.index, candidate.status
            ));
            if candidate.requirement_status != Status::Pass {
                unresolved_gates.push(format!(
                    "candidate {} requirement status is {:?}",
                    candidate.index, candidate.requirement_status
                ));
            }
            if candidate.refinement_status != Status::Pass {
                unresolved_gates.push(format!(
                    "candidate {} field-refinement status is {:?}",
                    candidate.index, candidate.refinement_status
                ));
            }
            if candidate.numerical_status != Status::Pass {
                unresolved_gates.push(format!(
                    "candidate {} numerical comparison status is {:?}",
                    candidate.index, candidate.numerical_status
                ));
            }
            if !candidate.pack_geometry_valid {
                unresolved_gates.push(format!(
                    "candidate {} has degenerate pack geometry",
                    candidate.index
                ));
            }
            if !candidate.manufacturing_feasible {
                unresolved_gates.push(format!(
                    "candidate {} violates declared bend/manufacturing limits",
                    candidate.index
                ));
            }
            if candidate.mechanical_feasible == Some(false) {
                unresolved_gates.push(format!(
                    "candidate {} exceeds a declared mechanical bound",
                    candidate.index
                ));
            }
            if let Some(screen) = &candidate.screening {
                let counts = &screen.point_counts;
                if counts.unsupported > 0 {
                    unresolved_gates.push(format!(
                        "candidate {} has {} sampled point(s) outside material data coverage",
                        candidate.index, counts.unsupported
                    ));
                }
                if counts.lower_bound > 0 {
                    unresolved_gates.push(format!(
                        "candidate {} has {} lower-bound point(s) without determined capacity",
                        candidate.index, counts.lower_bound
                    ));
                }
                if counts.along_current_excluded > 0 {
                    unresolved_gates.push(format!(
                        "candidate {} has {} excluded along-current point(s)",
                        candidate.index, counts.along_current_excluded
                    ));
                }
                if screen.max_self_field_ratio > record.case.limits.max_self_field_ratio {
                    unresolved_gates.push(format!(
                        "candidate {} self-field ratio {:.4} exceeds declared limit {:.4}",
                        candidate.index,
                        screen.max_self_field_ratio,
                        record.case.limits.max_self_field_ratio
                    ));
                }
                if screen.max_along_current_fraction
                    > record.case.limits.max_along_current_field_fraction
                {
                    unresolved_gates.push(format!(
                        "candidate {} along-current field fraction {:.4} exceeds declared limit {:.4}",
                        candidate.index,
                        screen.max_along_current_fraction,
                        record.case.limits.max_along_current_field_fraction
                    ));
                }
                if screen
                    .max_utilization
                    .is_some_and(|u| u > record.case.limits.utilization_limit)
                {
                    unresolved_gates.push(format!(
                        "candidate {} utilization {:.4} exceeds declared limit {:.4}",
                        candidate.index,
                        screen.max_utilization.unwrap_or_default(),
                        record.case.limits.utilization_limit
                    ));
                }
            } else {
                unresolved_gates.push(format!(
                    "candidate {} current-capacity screening was not evaluated",
                    candidate.index
                ));
            }
            if let Some(screens) = &candidate.screens {
                for (name, status) in [
                    (
                        "thermal margin",
                        screens.thermal_margin.as_ref().map(|s| s.status),
                    ),
                    ("AC loss", screens.ac_loss.as_ref().map(|s| s.status)),
                    (
                        "quench hotspot",
                        screens.quench_hotspot.as_ref().map(|s| s.status),
                    ),
                    (
                        "screening current",
                        screens.screening_current.as_ref().map(|s| s.status),
                    ),
                    ("transition", screens.transition.as_ref().map(|s| s.status)),
                    (
                        "quench transient",
                        screens.quench_transient.as_ref().map(|s| s.status),
                    ),
                ] {
                    if let Some(status) = status
                        && status != Status::Pass
                    {
                        unresolved_gates.push(format!(
                            "candidate {} declared {name} screen is {status:?}",
                            candidate.index
                        ));
                    }
                }
            }
            append_declared_screen_gaps(&record, candidate, &mut unresolved_gates);
        }
    }
    unresolved_gates.sort();
    unresolved_gates.dedup();

    let utilization = selected.and_then(|c| c.screening.as_ref());
    let selected_cost = selected.map(|c| &c.cost);
    let selected_total = selected_cost.map(|c| c.total_usd);
    let savings = selected_total.map(|cost| baseline.cost.total_usd - cost);
    let mut next_actions = Vec::new();
    if !unresolved_gates.is_empty() {
        if unresolved_gates.iter().any(|g| g.contains("Inconclusive")) {
            next_actions.push("Resolve the named data-coverage or numerical refinement uncertainty, then rerun the declared acceptance checks.".into());
        }
        if selected.is_some() && unresolved_gates.iter().any(|g| g.contains("Fail")) {
            next_actions.push("Revise the candidate or requirements that failed, then run a new search and independent acceptance check.".into());
        }
        if selected.is_none() {
            next_actions.push("Resolve the listed inconclusive coverage, self-field, or refinement blockers, or expand the candidate set; the aggregate search FAIL verdict does not establish a physical impossibility.".into());
        }
        if unresolved_gates.iter().any(|g| g.contains("not evaluated")) {
            next_actions.push("Run the missing declared engineering screens and acceptance checks before treating this selection as acceptable.".into());
        }
        if next_actions.is_empty() {
            next_actions
                .push("Resolve every listed unresolved gate before engineering acceptance.".into());
        }
    }
    if record.search_status == Status::Pass && record.acceptance.agreement_status == Status::Pass {
        next_actions
            .push("Review the recorded operating margins and manufacturing assumptions.".into());
    }
    next_actions.push("Confirm material prices, procurement lengths, joints, and assembly costs with the supplier and manufacturer.".into());
    next_actions.push("Complete structural, thermal, quench-protection, and manufacturing qualification outside this screening model.".into());

    Ok(DecisionSummary {
        schema: REVIEW_SUMMARY_SCHEMA.into(),
        source_record_sha256: sha256_hex(record_json.as_bytes()),
        source_case_sha256: record.case_sha256.clone(),
        selected_candidate_index: record.best_index,
        selected_geometry: selected.map(|c| c.geometry.clone()),
        selected_status: selected.map_or(Status::NotEvaluated, |c| c.status),
        baseline_candidate_index: record.baseline_index,
        baseline_total_usd: baseline.cost.total_usd,
        selected_total_usd: selected_total,
        savings_usd: savings,
        current_utilization: utilization.and_then(|s| s.max_utilization),
        utilization_limit: record.case.limits.utilization_limit,
        limiting_location: utilization
            .and_then(|s| s.limiting.as_ref())
            .map(|l| l.station.clone()),
        unresolved_gates,
        cost_components: selected_cost.map(|c| CostComponents {
            conductor_usd: c.conductor_usd,
            scrap_usd: c.scrap_usd,
            assembly_usd: c.assembly_usd,
            joints_usd: c.joints_usd,
            total_usd: c.total_usd,
        }),
        price_basis: price_basis(&record),
        limitations: record.limitations.clone(),
        next_actions,
    })
}

/// Recursively return the changed JSON values as escaped JSON-pointer paths.
pub fn input_changes(case_a_json: &str, case_b_json: &str) -> Result<Vec<InputChange>, RunError> {
    let a: Value = serde_json::from_str(case_a_json)
        .map_err(|e| RunError::Invalid(format!("review diff: first case JSON: {e}")))?;
    let b: Value = serde_json::from_str(case_b_json)
        .map_err(|e| RunError::Invalid(format!("review diff: second case JSON: {e}")))?;
    let mut changes = Vec::new();
    diff_value("", Some(&a), Some(&b), &mut changes);
    Ok(changes)
}

fn diff_value(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    out: &mut Vec<InputChange>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let keys: BTreeSet<&str> = a.keys().chain(b.keys()).map(String::as_str).collect();
            for key in keys {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                diff_value(&format!("{path}/{escaped}"), a.get(key), b.get(key), out);
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for i in 0..a.len().max(b.len()) {
                diff_value(&format!("{path}/{i}"), a.get(i), b.get(i), out);
            }
        }
        _ => out.push(InputChange {
            pointer: if path.is_empty() {
                "/".into()
            } else {
                path.into()
            },
            before: before.cloned(),
            after: after.cloned(),
        }),
    }
}

fn append_declared_screen_gaps(
    record: &CoupledSearchRunRecord,
    candidate: &crate::coupled_search::SearchCandidateResult,
    gates: &mut Vec<String>,
) {
    let Some(results) = &candidate.screens else {
        for (name, declared) in [
            ("thermal margin", record.case.thermal_margin.is_some()),
            ("AC loss", record.case.ac_loss.is_some()),
            ("quench hotspot", record.case.quench_hotspot.is_some()),
            ("screening current", record.case.screening_current.is_some()),
            ("transition", record.case.transition.is_some()),
            ("quench transient", record.case.quench_transient.is_some()),
        ] {
            if declared {
                gates.push(format!("declared {name} screen has no candidate result"));
            }
        }
        return;
    };
    for (name, declared, evaluated) in [
        (
            "thermal margin",
            record.case.thermal_margin.is_some(),
            results.thermal_margin.is_some(),
        ),
        (
            "AC loss",
            record.case.ac_loss.is_some(),
            results.ac_loss.is_some(),
        ),
        (
            "quench hotspot",
            record.case.quench_hotspot.is_some(),
            results.quench_hotspot.is_some(),
        ),
        (
            "screening current",
            record.case.screening_current.is_some(),
            results.screening_current.is_some(),
        ),
        (
            "transition",
            record.case.transition.is_some(),
            results.transition.is_some(),
        ),
        (
            "quench transient",
            record.case.quench_transient.is_some(),
            results.quench_transient.is_some(),
        ),
    ] {
        if declared && !evaluated {
            gates.push(format!("declared {name} screen result is missing"));
        }
    }
}

fn price_basis(record: &CoupledSearchRunRecord) -> Vec<String> {
    let label = |source: Option<optcoil_model::coupled_search::PriceSource>| {
        source
            .map(|s| format!("{:?}", s).to_lowercase())
            .unwrap_or_else(|| "unspecified".into())
    };
    let mut basis = vec![format!(
        "Base conductor: ${:.6}/m; price source {}.",
        record.case.cost.price_usd_per_m,
        label(record.case.cost.price_source)
    )];
    basis.push(format!(
        "Declared case provenance and price assumptions: {}",
        record.case.provenance
    ));
    if let Some(specs) = &record.case.tape_specs {
        for (id, spec) in specs {
            basis.push(format!(
                "Spec {id}: ${:.6}/m; price source {}.",
                spec.price_usd_per_m,
                label(spec.price_source)
            ));
        }
    }
    if record.case.cost.piece_policy.is_some() {
        basis.push("Piece procurement costs use the candidate ledger's chosen per-spec piece offerings and splice schedule.".into());
    }
    basis
}

/// Build a directory tree as `(relative path, UTF-8 contents)` artifacts.
/// The exact raw case bytes are required because `case_sha256` identifies
/// those bytes; typed reserialization cannot recreate that identity.
pub fn review_package(
    record_json: &str,
    case_json: Option<&str>,
    provided_bundles: &[MaterialBundle],
) -> Result<Vec<(String, String)>, RunError> {
    let texts = provided_bundles
        .iter()
        .map(MaterialBundle::to_json)
        .collect::<Result<Vec<_>, _>>()?;
    let refs = texts.iter().map(String::as_str).collect::<Vec<_>>();
    review_package_with_bundle_json(record_json, case_json, &refs)
}

/// Build a review package retaining each supplied bundle's exact JSON bytes.
/// Registry entries can pin the hash of the whole bundle file, so preserving
/// only its parsed CSV and signature is insufficient for portable verification.
/// Clients reading bundle files should use this entry point rather than the
/// typed convenience API, whose inputs have already lost their source bytes.
pub fn review_package_with_bundle_json(
    record_json: &str,
    case_json: Option<&str>,
    provided_bundle_json: &[&str],
) -> Result<Vec<(String, String)>, RunError> {
    let mut provided_bundles = BTreeMap::new();
    for &text in provided_bundle_json {
        let bundle = MaterialBundle::from_json(text)?;
        let id = bundle.dataset.metadata.id.clone();
        if provided_bundles
            .insert(id.clone(), (bundle, text))
            .is_some()
        {
            return Err(RunError::Invalid(format!(
                "review package: duplicate supplied dataset id '{id}'"
            )));
        }
    }
    let case_json = case_json.ok_or_else(|| {
        RunError::Invalid(
            "review package: original case bytes unavailable; provide the exact case source file used for this run"
                .into(),
        )
    })?;
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("review package: run record parse: {e}")))?;
    if let Some(specs) = &record.case.tape_specs {
        let missing: Vec<&str> = specs
            .keys()
            .map(String::as_str)
            .filter(|id| !record.spec_datasets.contains_key(*id))
            .collect();
        if !missing.is_empty() {
            return Err(RunError::Invalid(format!(
                "review package: record lacks resolved dataset identities for named tape specs {missing:?}; this older graded record cannot produce a complete dataset package. Rerun the original case with its resolved datasets to produce a current record."
            )));
        }
    }
    let case_hash = sha256_hex(case_json.as_bytes());
    if case_hash
        != record
            .case_sha256
            .strip_prefix("sha256:")
            .unwrap_or(&record.case_sha256)
    {
        return Err(RunError::Invalid(format!(
            "review package: supplied case bytes hash to {case_hash}, but the record binds {}; provide the original case file bytes",
            record.case_sha256
        )));
    }
    // Check record structure and exact case binding before packaging it.
    ensure_checks_pass(verify::verify_record_checks(
        record_json,
        Some(case_json),
        None,
        None,
        &[],
    )?)?;
    let _baseline = record
        .candidates
        .get(record.baseline_index)
        .ok_or_else(|| {
            RunError::Invalid("review package: baseline index is outside candidate list".into())
        })?;
    if record
        .best_index
        .is_some_and(|i| i >= record.candidates.len())
    {
        return Err(RunError::Invalid(
            "review package: best index is outside candidate list".into(),
        ));
    }

    let mut referenced = vec![(record.dataset_id.clone(), record.dataset_csv_sha256.clone())];
    referenced.extend(
        record
            .spec_datasets
            .values()
            .map(|d| (d.id.clone(), d.csv_sha256.clone())),
    );
    referenced.sort();
    referenced.dedup();
    for id in provided_bundles.keys() {
        if !referenced.iter().any(|(declared, _)| declared == id) {
            return Err(RunError::Invalid(format!(
                "review package: supplied dataset '{id}' is not referenced by the record"
            )));
        }
    }
    let mut datasets = Vec::new();
    let mut dataset_files = Vec::new();
    for (ordinal, (id, expected_sha)) in referenced.iter().enumerate() {
        let text = if let Some((bundle, text)) = provided_bundles.get(id) {
            if strip_sha(&bundle.dataset.metadata.csv_sha256) != strip_sha(expected_sha) {
                return Err(RunError::Invalid(format!(
                    "review package: supplied dataset '{id}' CSV hash does not match the record's {expected_sha}"
                )));
            }
            (*text).to_owned()
        } else if let Some(text) = embedded_bundle_json(id)? {
            text
        } else {
            return Err(RunError::Invalid(format!(
                "review package: dataset '{id}' ({expected_sha}) is external and its original material bundle was not provided"
            )));
        };
        datasets.push(text.clone());
        dataset_files.push((format!("datasets/dataset-{:03}.json", ordinal + 1), text));
    }
    let dataset_refs: Vec<&str> = datasets.iter().map(String::as_str).collect();
    // Reuse the independent verifier. A package fails closed if any bound
    // identity or candidate ledger fails its existing checks.
    ensure_checks_pass(verify::verify_record_checks(
        record_json,
        Some(case_json),
        None,
        None,
        &dataset_refs,
    )?)?;

    let summary = decision_summary(record_json)?;
    let summary_json = serde_json::to_string_pretty(&summary)?;
    let mut artifacts = vec![
        ("case.json".into(), case_json.into()),
        ("run-record.json".into(), record_json.into()),
        ("decision-summary.json".into(), summary_json),
        (
            "report.html".into(),
            report::render_search_report_html(&record)?,
        ),
    ];
    if record.best_index.is_some() {
        let bom_record = bom::bom_from_record(record_json)?;
        if !bom_record.totals_agree {
            return Err(RunError::Invalid(
                "review package: BOM totals do not agree with the source ledger".into(),
            ));
        }
        artifacts.push((
            "bom.json".into(),
            serde_json::to_string_pretty(&bom_record)?,
        ));
        artifacts.push(("rfq.md".into(), bom::rfq_markdown_from_record(record_json)?));
    } else {
        artifacts.push((
            "procurement-unavailable.txt".into(),
            "No PASS optimum was recorded, so the package does not contain a BOM or RFQ. See decision-summary.json for the selection and unresolved gates.\n".into(),
        ));
    }
    artifacts.extend(dataset_files.clone());
    let mut command =
        String::from("optcoil verify run-record.json case.json --manifest manifest.json");
    for (path, _) in &dataset_files {
        command.push_str(" --dataset ");
        command.push_str(path);
    }
    let rerun_bundles = dataset_files
        .iter()
        .map(|(path, _)| format!(" --dataset-bundle {path}"))
        .collect::<String>();
    let readme = format!(
        "Converra review package. The manifest binds the exact case and run-record bytes; its files map hashes every package artifact except manifest.json itself.\n\nVerify from this directory:\n  optcoil verify-package .\n\nOr run the underlying record verifier with all packaged datasets:\n  {command}\n\nRerun the search from this directory with every packaged material binding:\n  optcoil coupled-search case.json --output rerun.json{rerun_bundles}\n\nEach `--dataset-bundle` is resolved by its declared dataset id and must match the exact id and CSV SHA-256 required by the case. Supplying the package's exact bundle for an embedded dataset is also safe; a mismatched or undeclared bundle is rejected. The command includes every dependency, so it also works for graded cases with multiple external spec datasets.\n\nVerification checks artifact bindings and ledger arithmetic. It does not validate physics or establish engineering acceptance. Dataset signatures, when present, require separate `optcoil dataset verify` checks against a trusted registry/key.\n"
    );
    let mut files = BTreeMap::new();
    for (path, contents) in &artifacts {
        safe_relative_path(path)?;
        files.insert(path.clone(), sha256_hex(contents.as_bytes()));
    }
    files.insert("README.txt".into(), sha256_hex(readme.as_bytes()));
    let manifest = ReviewPackageManifest {
        schema: "optcoil-evidence-manifest/v1".into(),
        package_schema: REVIEW_PACKAGE_SCHEMA.into(),
        case: PackageFile {
            path: "case.json".into(),
            sha256: case_hash,
        },
        run_record: PackageFile {
            path: "run-record.json".into(),
            sha256: sha256_hex(record_json.as_bytes()),
        },
        identities: PackageIdentities {
            coupled_search_model_id: record.coupled_search_model_id.clone(),
            coupled_search_checker_id: record.coupled_search_checker_id.clone(),
            dataset_id: record.dataset_id.clone(),
            dataset_csv_sha256: record.dataset_csv_sha256.clone(),
        },
        files,
    };
    artifacts.push((
        "manifest.json".into(),
        serde_json::to_string_pretty(&manifest)?,
    ));
    artifacts.push(("README.txt".into(), readme));
    Ok(artifacts)
}

/// Verify every package file hash, then run the existing record verifier
/// against the exact packaged case, record and dataset bundle bytes.
pub fn verify_review_package(artifacts: &[(String, String)]) -> Result<(), RunError> {
    let mut map = BTreeMap::new();
    for (path, contents) in artifacts {
        safe_relative_path(path)?;
        if map.insert(path.as_str(), contents.as_str()).is_some() {
            return Err(RunError::Invalid(format!(
                "review package verify: duplicate artifact path '{path}'"
            )));
        }
    }
    let manifest_text = map
        .get("manifest.json")
        .ok_or_else(|| RunError::Invalid("review package verify: manifest.json missing".into()))?;
    let manifest: ReviewPackageManifest = serde_json::from_str(manifest_text)
        .map_err(|e| RunError::Invalid(format!("review package verify: manifest parse: {e}")))?;
    if manifest.package_schema != REVIEW_PACKAGE_SCHEMA
        || !manifest.schema.starts_with("optcoil-evidence-manifest/")
    {
        return Err(RunError::Invalid(format!(
            "review package verify: unsupported manifest/package schema {}/{}",
            manifest.schema, manifest.package_schema
        )));
    }
    if artifacts.len() != manifest.files.len() + 1 {
        return Err(RunError::Invalid(
            "review package verify: manifest does not enumerate every artifact exactly once".into(),
        ));
    }
    safe_relative_path(&manifest.case.path)?;
    safe_relative_path(&manifest.run_record.path)?;
    for (path, expected) in &manifest.files {
        safe_relative_path(path)?;
        let contents = map
            .get(path.as_str())
            .ok_or_else(|| RunError::Invalid(format!("review package verify: missing {path}")))?;
        let actual = sha256_hex(contents.as_bytes());
        if &actual != expected {
            return Err(RunError::Invalid(format!(
                "review package verify: SHA-256 mismatch for {path}"
            )));
        }
    }
    let case = map
        .get(manifest.case.path.as_str())
        .ok_or_else(|| RunError::Invalid("review package verify: case artifact missing".into()))?;
    let record = map.get(manifest.run_record.path.as_str()).ok_or_else(|| {
        RunError::Invalid("review package verify: record artifact missing".into())
    })?;
    let dataset_jsons: Vec<&str> = manifest
        .files
        .keys()
        .filter(|p| p.starts_with("datasets/") && p.ends_with(".json"))
        .filter_map(|p| map.get(p.as_str()).copied())
        .collect();
    ensure_checks_pass(verify::verify_record_checks(
        record,
        Some(case),
        Some(manifest_text),
        None,
        &dataset_jsons,
    )?)?;
    Ok(())
}

/// Pack a verified review package into an uncompressed POSIX ustar archive.
/// This keeps nested dataset paths intact for browser downloads without a
/// filesystem API or an added compression dependency.
pub fn review_package_tar(artifacts: &[(String, String)]) -> Result<Vec<u8>, RunError> {
    verify_review_package(artifacts)?;
    let mut tar = Vec::new();
    for (path, contents) in artifacts {
        safe_relative_path(path)?;
        if !path.is_ascii() || path.len() > 100 {
            return Err(RunError::Invalid(format!(
                "review package tar: path must be ASCII and at most 100 bytes: '{path}'"
            )));
        }
        let bytes = contents.as_bytes();
        let mut header = [0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        tar_octal(&mut header[100..108], 0o644)?;
        tar_octal(&mut header[108..116], 0)?;
        tar_octal(&mut header[116..124], 0)?;
        tar_octal(&mut header[124..136], bytes.len() as u64)?;
        tar_octal(&mut header[136..148], 0)?;
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u64 = header.iter().map(|b| u64::from(*b)).sum();
        let checksum_text = format!("{checksum:06o}\0 ");
        if checksum_text.len() != 8 {
            return Err(RunError::Invalid(
                "review package tar: header checksum overflow".into(),
            ));
        }
        header[148..156].copy_from_slice(checksum_text.as_bytes());
        tar.extend_from_slice(&header);
        tar.extend_from_slice(bytes);
        let padding = (512 - bytes.len() % 512) % 512;
        tar.resize(tar.len() + padding, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    Ok(tar)
}

fn tar_octal(field: &mut [u8], value: u64) -> Result<(), RunError> {
    let text = format!("{:0width$o}\0", value, width = field.len() - 1);
    if text.len() > field.len() {
        return Err(RunError::Invalid(
            "review package tar: numeric field overflow".into(),
        ));
    }
    field.fill(0);
    field[..text.len()].copy_from_slice(text.as_bytes());
    Ok(())
}

fn ensure_checks_pass(checks: Vec<verify::CheckLine>) -> Result<(), RunError> {
    let failures: Vec<String> = checks
        .iter()
        .filter(|check| check.outcome == verify::Outcome::Fail)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RunError::Invalid(format!(
            "review package: evidence verification failed: {}",
            failures.join("; ")
        )))
    }
}

fn safe_relative_path(path: &str) -> Result<(), RunError> {
    let p = std::path::Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(RunError::Invalid(format!(
            "review package: unsafe relative path '{path}'"
        )));
    }
    Ok(())
}

fn strip_sha(s: &str) -> &str {
    s.strip_prefix("sha256:").unwrap_or(s)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn embedded_bundle_json(id: &str) -> Result<Option<String>, RunError> {
    use optcoil_model::material as m;
    // Keep the issued bundle bytes intact, including signatures. Registry
    // entries bind the exact file hash, not a reserialized equivalent.
    let issued = match id {
        m::SUPERPOWER_ID => Some(include_str!(
            "../../../data/materials/robinson-superpower-ap-v3/bundle.json"
        )),
        m::SUPERPOWER_LOWFIELD_ID => Some(include_str!(
            "../../../data/materials/robinson-superpower-ap-v3-lowfield/bundle.json"
        )),
        m::SUPERPOWER_MODELEXT_ID => Some(include_str!(
            "../../../data/materials/robinson-superpower-ap-v3-modelext/bundle.json"
        )),
        m::SHANGHAI_HFLT_ID => Some(include_str!(
            "../../../data/materials/robinson-shanghai-hflt-v3/bundle.json"
        )),
        m::THEVA_AP_ID => Some(include_str!(
            "../../../data/materials/robinson-theva-ap-v2/bundle.json"
        )),
        m::FFJ_YBCO_ID => Some(include_str!(
            "../../../data/materials/robinson-ffj-ybco-v1/bundle.json"
        )),
        _ => None,
    };
    if let Some(text) = issued {
        let bundle = MaterialBundle::from_json(text)?;
        if bundle.dataset.metadata.id != id {
            return Err(RunError::Invalid(format!(
                "embedded bundle metadata id does not match '{id}'"
            )));
        }
        return Ok(Some(text.to_owned()));
    }
    let csv: Option<&[u8]> = match id {
        m::BABOUCHE_SP_ID => Some(m::BABOUCHE_SP_CSV),
        m::BABOUCHE_SST_ID => Some(m::BABOUCHE_SST_CSV),
        m::BABOUCHE_SP_V2_ID => Some(m::BABOUCHE_SP_V2_CSV),
        m::BABOUCHE_SST_V2_ID => Some(m::BABOUCHE_SST_V2_CSV),
        _ => None,
    };
    let Some(csv) = csv else { return Ok(None) };
    let dataset = optcoil_model::material::MaterialDataset::embedded_by_id(id)?;
    let csv = String::from_utf8(csv.to_vec())
        .map_err(|e| RunError::Invalid(format!("embedded dataset '{id}' CSV is not UTF-8: {e}")))?;
    Ok(Some(
        MaterialBundle::from_parts(dataset, csv, None)?.to_json()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::atomic::AtomicBool};

    fn external_reference_bundle(source_id: &str, external_id: &str) -> MaterialBundle {
        use optcoil_model::material as m;
        let csv = match source_id {
            m::SUPERPOWER_LOWFIELD_ID => m::SUPERPOWER_LOWFIELD_CSV,
            m::SUPERPOWER_MODELEXT_ID => m::SUPERPOWER_MODELEXT_CSV,
            _ => panic!("test fixture must use an attributed embedded reference dataset"),
        };
        let mut dataset = m::MaterialDataset::embedded_by_id(source_id).unwrap();
        // These are software fixtures built from attributed reference CSVs;
        // the suffixed identities model two separately supplied customer
        // bundles without claiming that the fixture ids are published tapes.
        dataset.metadata.id = external_id.into();
        MaterialBundle::from_parts(dataset, String::from_utf8(csv.to_vec()).unwrap(), None).unwrap()
    }

    #[test]
    fn json_diff_uses_escaped_pointers_and_reports_array_changes() {
        let changes = input_changes(
            r#"{"a/b":{"~x":1},"list":[1,2]}"#,
            r#"{"a/b":{"~x":2},"list":[1]}"#,
        )
        .unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].pointer, "/a~1b/~0x");
        assert_eq!(changes[1].pointer, "/list/1");
    }

    #[test]
    fn package_refuses_unavailable_or_reserialized_case_bytes() {
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::reduced_case_json("[3]", 30.0),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            review_package(&json, None, &[])
                .unwrap_err()
                .to_string()
                .contains("original case bytes unavailable")
        );
        assert!(
            review_package(&json, Some("{}"), &[])
                .unwrap_err()
                .to_string()
                .contains("original case file bytes")
        );
    }

    #[test]
    fn package_refuses_legacy_graded_record_without_resolved_spec_identities() {
        let case = crate::coupled_search::tests::reduced_graded_case_json("[4, 200]", 10.0, 0.5);
        let record = crate::coupled_search::run_coupled_search_case(
            &case,
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let mut legacy: Value = serde_json::to_value(record).unwrap();
        legacy["schema"] = Value::String("optcoil-coupled-search-run/v12".into());
        legacy["spec_datasets"] = Value::Object(serde_json::Map::new());
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        let error = review_package(&legacy_json, Some(&case), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("lacks resolved dataset identities"));
        assert!(error.contains("Rerun the original case"));
    }

    #[test]
    fn package_carries_and_verifies_two_external_graded_dataset_dependencies() {
        let first = external_reference_bundle(
            optcoil_model::material::SUPERPOWER_LOWFIELD_ID,
            "review-fixture-external-lowfield",
        );
        let second = external_reference_bundle(
            optcoil_model::material::SUPERPOWER_MODELEXT_ID,
            "review-fixture-external-modelext",
        );
        let mut case: Value = serde_json::from_str(
            &crate::coupled_search::tests::reduced_graded_case_json("[4]", 10.0, 0.5),
        )
        .unwrap();
        for (spec_id, bundle) in [("cheap", &first), ("other", &second)] {
            if case["tape_specs"].get(spec_id).is_none() {
                case["tape_specs"][spec_id] = case["tape_specs"]["cheap"].clone();
            }
            case["tape_specs"][spec_id]["material"]["dataset_id"] =
                Value::String(bundle.dataset.metadata.id.clone());
            case["tape_specs"][spec_id]["material"]["csv_sha256"] =
                Value::String(bundle.dataset.metadata.csv_sha256.clone());
        }
        case["grading"]["regions"][0]["tape_spec_choices"] =
            serde_json::json!(["base", "cheap", "other"]);
        case["grading"]["regions"][1]["tape_spec_choices"] =
            serde_json::json!(["base", "cheap", "other"]);
        let case_json = serde_json::to_string(&case).unwrap();
        let datasets = BTreeMap::from([
            (first.dataset.metadata.id.clone(), first.dataset.clone()),
            (second.dataset.metadata.id.clone(), second.dataset.clone()),
        ]);
        let record = crate::coupled_search::run_coupled_search_case_with_datasets(
            &case_json,
            &crate::coupled_search::CoupledSearchOptions::default(),
            &datasets,
            &AtomicBool::new(false),
        )
        .unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        // Source formatting participates in registry bundle identity. Use
        // deliberately noncanonical texts to catch a parse/serialize cycle.
        let first_text = format!("\n{}\n\n", first.to_json().unwrap());
        let second_text = serde_json::to_string(
            &serde_json::from_str::<Value>(&second.to_json().unwrap()).unwrap(),
        )
        .unwrap();
        let package = review_package_with_bundle_json(
            &record_json,
            Some(&case_json),
            &[&first_text, &second_text],
        )
        .unwrap();
        verify_review_package(&package).unwrap();
        for (id, expected) in [
            (first.dataset.metadata.id.as_str(), first_text.as_str()),
            (second.dataset.metadata.id.as_str(), second_text.as_str()),
            (
                optcoil_model::material::SUPERPOWER_ID,
                include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json"),
            ),
        ] {
            let text = package
                .iter()
                .filter(|(path, _)| path.starts_with("datasets/"))
                .find(|(_, text)| {
                    MaterialBundle::from_json(text).unwrap().dataset.metadata.id == id
                })
                .unwrap()
                .1
                .as_str();
            assert_eq!(text.as_bytes(), expected.as_bytes(), "bundle {id}");
            if id == optcoil_model::material::SUPERPOWER_ID {
                let checks = verify::verify_dataset_bundle(
                    text,
                    Some(include_str!("../../../data/registry/datasets.json")),
                    None,
                )
                .unwrap();
                assert!(checks.iter().all(|check| check.verdict == "PASS"));
            }
        }
        assert!(
            review_package_with_bundle_json(
                &record_json,
                Some(&case_json),
                &[&first_text, &first_text, &second_text],
            )
            .unwrap_err()
            .to_string()
            .contains("duplicate supplied dataset id")
        );
        let readme = package
            .iter()
            .find(|(path, _)| path == "README.txt")
            .unwrap()
            .1
            .clone();
        assert!(readme.contains("--dataset-bundle datasets/dataset-002.json"));
        assert!(readme.contains("every packaged material binding"));
        assert_eq!(
            package
                .iter()
                .filter(|(path, _)| path.starts_with("datasets/") && path.ends_with(".json"))
                .count(),
            3 // base material plus both referenced external specs
        );

        let missing_one = review_package(
            &record_json,
            Some(&case_json),
            &[external_reference_bundle(
                optcoil_model::material::SUPERPOWER_LOWFIELD_ID,
                "review-fixture-external-lowfield",
            )],
        );
        assert!(missing_one.unwrap_err().to_string().contains("external"));
        let wrong_pair = [
            external_reference_bundle(
                optcoil_model::material::SUPERPOWER_LOWFIELD_ID,
                "review-fixture-external-modelext",
            ),
            external_reference_bundle(
                optcoil_model::material::SUPERPOWER_MODELEXT_ID,
                "review-fixture-external-lowfield",
            ),
        ];
        assert!(
            review_package(&record_json, Some(&case_json), &wrong_pair)
                .unwrap_err()
                .to_string()
                .contains("external")
        );
    }

    #[test]
    fn package_verifies_hashes_and_rejects_duplicate_and_tampered_artifacts() {
        let case = crate::coupled_search::tests::reduced_case_json("[3]", 30.0);
        let record = crate::coupled_search::run_coupled_search_case(
            &case,
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        let package = review_package(&record_json, Some(&case), &[]).unwrap();
        verify_review_package(&package).unwrap();
        let archive = review_package_tar(&package).unwrap();
        assert_eq!(&archive[..9], b"case.json");
        assert_eq!(&archive[257..263], b"ustar\0");
        assert!(archive.ends_with(&[0; 1024]));

        let mut duplicate = package.clone();
        duplicate.push(duplicate[0].clone());
        assert!(
            verify_review_package(&duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate artifact path")
        );

        let mut tampered = package;
        let record_position = tampered
            .iter()
            .position(|(path, _)| path == "run-record.json")
            .unwrap();
        let mut changed_record: Value = serde_json::from_str(&tampered[record_position].1).unwrap();
        let candidate_index = 0;
        let old_total = changed_record["candidates"][candidate_index]["cost"]["total_usd"]
            .as_f64()
            .unwrap();
        changed_record["candidates"][candidate_index]["cost"]["total_usd"] =
            Value::from(old_total + 1.0);
        tampered[record_position].1 = serde_json::to_string(&changed_record).unwrap();
        let manifest_position = tampered
            .iter()
            .position(|(path, _)| path == "manifest.json")
            .unwrap();
        let mut manifest: Value = serde_json::from_str(&tampered[manifest_position].1).unwrap();
        let record_hash = sha256_hex(tampered[record_position].1.as_bytes());
        manifest["run_record"]["sha256"] = Value::String(record_hash.clone());
        manifest["files"]["run-record.json"] = Value::String(record_hash);
        tampered[manifest_position].1 = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(
            verify_review_package(&tampered)
                .unwrap_err()
                .to_string()
                .contains("evidence verification failed")
        );
    }
}
