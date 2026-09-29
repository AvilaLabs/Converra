//! Reprice a coupled-search run record at a different conductor price —
//! exact arithmetic, not a rerun.
//!
//! The cost model is closed-form: `conductor_usd = installed_length_m ×
//! price`, `scrap_usd = scrap length × price`, and assembly/joint costs are
//! price-independent. Every per-candidate ledger is already in the record,
//! so a new price produces exact new totals with zero physics work — and
//! zero claim that physics reran. The note binds the source record's SHA,
//! restates its verdicts unchanged, and re-selects the optimum under the
//! new totals (the argmin *can* move when fixed costs are significant).
//!
//! This is a derived view: `derived_from` names the evidence, and the note
//! states plainly that the source run's physics — not a new evaluation —
//! underwrites every status shown.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use optcoil_model::Status;

use crate::{RunError, coupled_search::CoupledSearchRunRecord};

pub const REPRICE_NOTE_SCHEMA: &str = "optcoil-reprice-note/v2";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepricedCandidate {
    pub index: usize,
    pub geometry: crate::coupled_search::CandidateGeometry,
    pub status: Status,
    pub installed_length_m: f64,
    pub purchased_length_m: f64,
    pub conductor_usd: f64,
    pub scrap_usd: f64,
    pub assembly_usd: f64,
    pub joints_usd: f64,
    pub total_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepriceNote {
    pub schema: String,
    pub optcoil_version: String,
    /// SHA-256 of the source run-record file bytes — the evidence this
    /// note derives from.
    pub source_record_sha256: String,
    pub source_case_sha256: String,
    pub source_price_usd_per_m: f64,
    pub price_usd_per_m: f64,
    /// Where the new price came from — e.g. a published band, a supplier
    /// quote. Bound into the note like case provenance.
    pub price_provenance: String,
    pub candidates: Vec<RepricedCandidate>,
    /// Index into `candidates` — repriced baseline (the record's baseline).
    pub baseline_index: usize,
    /// Repriced optimum: the cheapest candidate among those the source
    /// record screened `PASS`. `None` when the source found no optimum.
    pub best_index: Option<usize>,
    pub baseline_total_usd: f64,
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    /// Source run verdict. It describes the source choice only.
    #[serde(alias = "search_status")]
    pub source_search_status: Status,
    /// Source run acceptance agreement. It does not certify the repriced
    /// winner, which may be a different candidate.
    #[serde(alias = "acceptance_agreement_status")]
    pub source_acceptance_agreement_status: Status,
    /// Acceptance of the repriced selection was not recomputed.
    pub selection_acceptance_status: Status,
    pub limitations: Vec<String>,
}

impl RepriceNote {
    pub fn write_new(&self, path: impl AsRef<std::path::Path>) -> Result<(), RunError> {
        crate::write_json_new(self, path)
    }
}

/// Reprice every candidate in `record_json` at `price_usd_per_m`.
/// `price_provenance` must say where the price came from — a note without
/// it would be an unsourced dollar figure.
pub fn reprice_record(
    record_json: &str,
    record_sha256: &str,
    price_usd_per_m: f64,
    price_provenance: &str,
) -> Result<RepriceNote, RunError> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("coupled-search record parse: {e}")))?;
    if record.candidates.is_empty() {
        return Err(RunError::Invalid(
            "reprice: source record has no candidates".into(),
        ));
    }
    if record.baseline_index >= record.candidates.len() {
        return Err(RunError::Invalid(format!(
            "reprice: baseline_index {} is outside {} candidates",
            record.baseline_index,
            record.candidates.len()
        )));
    }
    let source_checks = crate::verify::verify_record_checks(record_json, None, None, None, &[])?;
    let failed_checks: Vec<_> = source_checks
        .iter()
        .filter(|check| check.outcome == crate::verify::Outcome::Fail)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect();
    if !failed_checks.is_empty() {
        return Err(RunError::Invalid(format!(
            "reprice: source record failed integrity checks: {}",
            failed_checks.join("; ")
        )));
    }
    if !(price_usd_per_m.is_finite() && price_usd_per_m > 0.0) {
        return Err(RunError::Invalid(
            "reprice: --price-usd-per-m must be a positive finite number".into(),
        ));
    }
    if price_provenance.trim().is_empty() {
        return Err(RunError::Invalid(
            "reprice: --price-provenance is required — say where this price comes from".into(),
        ));
    }
    if record.case.cost.piece_policy.is_some()
        || record
            .candidates
            .iter()
            .any(|c| c.cost.piece_plan.is_some())
    {
        return Err(RunError::Invalid(
            "reprice: this record uses schema v24 piece-catalogue pricing — \
             a single $/m cannot reprice per-spec offerings; re-run the case \
             with updated piece_offerings instead"
                .into(),
        ));
    }
    // A single scalar can only describe a case whose candidates all use
    // the same base conductor price. Grading assigns prices per spec, so
    // scaling installed metres by the base price silently corrupts its
    // ledger even when the selected candidate happens to be uniform.
    if record.case.grading.is_some()
        || record
            .case
            .tape_specs
            .as_ref()
            .is_some_and(|specs| !specs.is_empty())
        || record
            .candidates
            .iter()
            .any(|c| c.geometry.tape_spec_ids.is_some())
    {
        return Err(RunError::Invalid(
            "reprice: scalar $/m repricing supports uniform cases only; graded/spec-priced records require per-spec repricing"
                .into(),
        ));
    }
    let scrap_fraction = record.case.cost.scrap_fraction;
    let candidates: Vec<RepricedCandidate> = record
        .candidates
        .iter()
        .map(|c| {
            let installed = c.cost.installed_length_m;
            let conductor_usd = installed * price_usd_per_m;
            // Same model as compute_cost_ledger: scrap is a fraction of
            // installed length, assembly/joints are price-independent.
            let scrap_usd = installed * scrap_fraction * price_usd_per_m;
            RepricedCandidate {
                index: c.index,
                geometry: c.geometry.clone(),
                status: c.status,
                installed_length_m: installed,
                purchased_length_m: installed * (1.0 + scrap_fraction),
                conductor_usd,
                scrap_usd,
                assembly_usd: c.cost.assembly_usd,
                joints_usd: c.cost.joints_usd,
                total_usd: conductor_usd + scrap_usd + c.cost.assembly_usd + c.cost.joints_usd,
            }
        })
        .collect();
    // Same comparator as the search's own best_index selection: cost,
    // then fewer total turns, then index — a reprice must name the same
    // optimum a rerun would on a cost tie.
    let best_index = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status == Status::Pass)
        .min_by(|(_, a), (_, b)| {
            a.total_usd
                .total_cmp(&b.total_usd)
                .then(a.geometry.total_turns.cmp(&b.geometry.total_turns))
                .then(a.index.cmp(&b.index))
        })
        .map(|(i, _)| i);
    let baseline_total = candidates[record.baseline_index].total_usd;
    let savings_usd = best_index.map(|i| baseline_total - candidates[i].total_usd);
    Ok(RepriceNote {
        schema: REPRICE_NOTE_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        source_record_sha256: record_sha256.into(),
        source_case_sha256: record.case_sha256.clone(),
        source_price_usd_per_m: record.case.cost.price_usd_per_m,
        price_usd_per_m,
        price_provenance: price_provenance.trim().into(),
        baseline_index: record.baseline_index,
        best_index,
        baseline_total_usd: baseline_total,
        savings_usd,
        savings_percent: savings_usd.map(|s| 100.0 * s / baseline_total),
        source_search_status: record.search_status,
        source_acceptance_agreement_status: record.acceptance.agreement_status,
        selection_acceptance_status: Status::NotEvaluated,
        candidates,
        limitations: vec![
            "Derived arithmetic on the source record — no physics reran; verdicts, margins and acceptance statuses are the source run's, unchanged by construction.".into(),
            "Source search and acceptance statuses describe the source run only. Acceptance of the repriced selection is NOT_EVALUATED because the selected candidate can change.".into(),
            "The optimum is re-selected by argmin over repriced totals among source-screened PASS candidates; under a large enough price change fixed costs can move it.".into(),
            "Assembly and joint costs are price-independent in the cost model and carry over unchanged.".into(),
        ],
    })
}

/// Hash the record file bytes — the note binds exactly what was read.
pub fn record_sha256(record_json: &str) -> String {
    format!("{:x}", Sha256::digest(record_json.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repricing_scales_conductor_terms_and_preserves_fixed_costs() {
        // Minimal record-shaped JSON is brittle; exercise the arithmetic on
        // a real reduced-case run instead.
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::reduced_case_json("[3]", 30.0),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let json = serde_json::to_string(&record).unwrap();
        let note = reprice_record(&json, &record_sha256(&json), 60.0, "test doubling").unwrap();
        for (src, repriced) in record.candidates.iter().zip(note.candidates.iter()) {
            assert_eq!(src.status, repriced.status);
            assert_eq!(
                src.geometry.turns_along_normal,
                repriced.geometry.turns_along_normal
            );
            assert!((repriced.conductor_usd - src.cost.conductor_usd * 2.0).abs() < 1e-6);
            assert!((repriced.scrap_usd - src.cost.scrap_usd * 2.0).abs() < 1e-6);
            assert_eq!(repriced.assembly_usd, src.cost.assembly_usd);
            assert_eq!(repriced.joints_usd, src.cost.joints_usd);
        }
        assert_eq!(note.source_price_usd_per_m, 30.0);
        assert_eq!(note.source_search_status, record.search_status);
        assert_eq!(note.selection_acceptance_status, Status::NotEvaluated);
        assert!(note.savings_usd.is_some() == record.savings_usd.is_some());
    }

    #[test]
    fn piece_priced_records_are_refused() {
        // A scalar $/m cannot reprice per-spec piece catalogues — the
        // honest answer is a refusal, not a hybrid total.
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::piece_case_json("[3]", "per_module", 50.0, 5.0),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let json = serde_json::to_string(&record).unwrap();
        let err = reprice_record(&json, &record_sha256(&json), 60.0, "test").unwrap_err();
        assert!(
            matches!(err, RunError::Invalid(ref m) if m.contains("piece-catalogue")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn graded_records_keep_their_actual_spec_priced_ledger_and_refuse_scalar_reprice() {
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::reduced_graded_case_json("[4, 200]", 10.0, 0.5),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let candidate = &record.candidates[record.best_index.unwrap()];
        assert_eq!(
            candidate.cost.total_usd,
            candidate.cost.conductor_usd
                + candidate.cost.scrap_usd
                + candidate.cost.assembly_usd
                + candidate.cost.joints_usd
        );
        let json = serde_json::to_string(&record).unwrap();
        let bom = crate::bom::bom_from_record(&json).unwrap();
        let grading = crate::gradereport::grade_report_from_record(&json).unwrap();
        assert!(bom.totals_agree);
        assert_eq!(bom.record_total_usd, candidate.cost.total_usd);
        assert_eq!(bom.total_usd, candidate.cost.total_usd);
        assert_eq!(grading.optimum_total_usd, Some(candidate.cost.total_usd));
        let err = reprice_record(&json, &record_sha256(&json), 60.0, "test quote").unwrap_err();
        assert!(matches!(err, RunError::Invalid(ref m) if m.contains("graded/spec-priced")));
    }

    #[test]
    fn missing_provenance_is_rejected() {
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::reduced_case_json("[3]", 30.0),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let json = serde_json::to_string(&record).unwrap();
        assert!(reprice_record(&json, "x", 60.0, "  ").is_err());
        assert!(reprice_record(&json, "x", -5.0, "test").is_err());
    }

    #[test]
    fn invalid_source_indices_and_ledger_are_rejected_without_indexing() {
        let record = crate::coupled_search::run_coupled_search_case(
            &crate::coupled_search::tests::reduced_case_json("[3]", 30.0),
            &crate::coupled_search::CoupledSearchOptions::default(),
        )
        .unwrap();
        let mut value = serde_json::to_value(&record).unwrap();
        value["baseline_index"] = serde_json::json!(usize::MAX);
        let invalid_index = serde_json::to_string(&value).unwrap();
        assert!(reprice_record(&invalid_index, "x", 60.0, "test").is_err());

        value = serde_json::to_value(&record).unwrap();
        // Finite corruption exercises the independent ledger recomputation.
        value["candidates"][0]["cost"]["total_usd"] = serde_json::json!(123.0);
        let invalid_ledger = serde_json::to_string(&value).unwrap();
        let err = reprice_record(&invalid_ledger, "x", 60.0, "test").unwrap_err();
        assert!(err.to_string().contains("failed integrity checks"));
    }
}
