//! Grading comparison report (`optcoil-grading-report/v1`): a derived view
//! over a completed coupled-search run record that quantifies what grading
//! bought — "premium tape only where the field demands it" as a modeled
//! dollar figure, not an assertion.
//!
//! A graded case's search already explores uniform assignments (any region
//! can pick any spec id, including `"base"` everywhere), so the record
//! contains both the mixed optimum and every uniform alternative. This
//! report reads that record — no recompute — and reports, per spec id, the
//! cheapest PASS candidate whose assignment is uniformly that spec
//! ("buy that vendor's tape everywhere"), next to the search's own optimum.
//! `grading_delta_usd` is best-uniform minus optimum cost: the modeled
//! value of mixing specs across regions. It is zero when the optimum is
//! itself uniform — grading offered nothing this case needed — which is
//! data, not a report failure.
//!
//! The report binds `source_record_sha256`: it is a view over committed
//! evidence, not new evidence. All figures are modeled under the case's
//! declared prices.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use optcoil_model::{Status, coupled_search::BASE_TAPE_SPEC_ID};

use crate::{RunError, coupled_search::CoupledSearchRunRecord};

pub const GRADING_REPORT_SCHEMA: &str = "optcoil-grading-report/v1";

/// The cheapest PASS candidate whose per-region assignment is uniformly
/// one spec id — "that vendor's tape everywhere".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniformSpecRow {
    pub spec_id: String,
    /// The dataset the spec's binding actually resolved to (record's
    /// `dataset_id` for `base`, `spec_datasets[id]` otherwise).
    pub dataset_id: String,
    pub best_candidate_index: usize,
    pub total_usd: f64,
    /// Versus the record's own baseline cost (negative means the uniform
    /// design already beats the declared baseline).
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    /// The optimum's operating margin at this uniform design.
    pub max_utilization: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradingReport {
    pub schema: String,
    pub optcoil_version: String,
    /// SHA-256 of the source record file bytes — the comparison's evidence
    /// basis.
    pub source_record_sha256: String,
    /// The run record's own `case_sha256`.
    pub case_sha256: String,
    /// True iff the run was a graded case (per-region assignments present).
    pub graded: bool,
    /// The search optimum's resolved assignment (per-region spec ids).
    pub optimum_assignment: Option<Vec<String>>,
    pub optimum_total_usd: Option<f64>,
    /// True iff the optimum's assignment is uniform — grading expanded the
    /// grid but the winner needed no mix. `grading_delta` is then 0 by
    /// construction (the best uniform *is* the optimum).
    pub optimum_is_uniform: Option<bool>,
    /// Per spec id: the cheapest PASS candidate using that spec on every
    /// region. Specs with no passing uniform row are absent.
    pub uniform_rows: Vec<UniformSpecRow>,
    /// The cheapest uniform row — "one vendor everywhere".
    pub best_uniform_spec_id: Option<String>,
    /// `best_uniform_cost − optimum_cost` — the modeled value grading
    /// bought. `None` when no PASS optimum or no uniform row exists.
    pub grading_delta_usd: Option<f64>,
    pub grading_delta_percent: Option<f64>,
    pub limitations: Vec<String>,
}

impl GradingReport {
    pub fn write_new(&self, path: impl AsRef<std::path::Path>) -> Result<(), RunError> {
        crate::write_json_new(self, path)
    }
}

/// Build the grading comparison from a serialized coupled-search run
/// record. Errors when the record carries no per-region assignments (an
/// ungraded run — there is nothing to compare).
pub fn grade_report_from_record(record_json: &str) -> Result<GradingReport, RunError> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("run record does not parse: {e}")))?;

    let graded = record
        .candidates
        .iter()
        .any(|c| c.geometry.tape_spec_ids.is_some());
    if !graded {
        return Err(RunError::Invalid(
            "record has no graded candidates — nothing to compare".into(),
        ));
    }

    let baseline_total_usd = record.candidates[record.baseline_index].cost.total_usd;
    let dataset_of = |spec_id: &str| -> String {
        if spec_id == BASE_TAPE_SPEC_ID {
            record.dataset_id.clone()
        } else {
            record
                .spec_datasets
                .get(spec_id)
                .map(|d| d.id.clone())
                .unwrap_or_else(|| spec_id.to_string())
        }
    };

    // Distinct spec ids actually assignable (union over assignments).
    let mut spec_ids: Vec<String> = record
        .candidates
        .iter()
        .filter_map(|c| c.geometry.tape_spec_ids.as_ref())
        .flatten()
        .cloned()
        .collect();
    spec_ids.sort();
    spec_ids.dedup();

    let mut uniform_rows = Vec::new();
    for spec_id in &spec_ids {
        // Cheapest PASS candidate with a uniform assignment of this spec.
        let best = record
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.status == Status::Pass)
            .filter(|(_, c)| {
                c.geometry
                    .tape_spec_ids
                    .as_ref()
                    .is_some_and(|ids| !ids.is_empty() && ids.iter().all(|s| s == spec_id))
            })
            .min_by(|(_, a), (_, b)| {
                a.cost
                    .total_usd
                    .total_cmp(&b.cost.total_usd)
                    .then(a.index.cmp(&b.index))
            });
        if let Some((i, c)) = best {
            let saving = baseline_total_usd - c.cost.total_usd;
            uniform_rows.push(UniformSpecRow {
                spec_id: spec_id.clone(),
                dataset_id: dataset_of(spec_id),
                best_candidate_index: i,
                total_usd: c.cost.total_usd,
                savings_usd: Some(saving),
                savings_percent: Some(saving / baseline_total_usd * 100.0),
                max_utilization: c.screening.as_ref().and_then(|s| s.max_utilization),
            });
        }
    }

    let best = record.best_index.map(|i| &record.candidates[i]);
    let optimum_assignment = best.and_then(|c| c.geometry.tape_spec_ids.clone());
    let optimum_total_usd = best.map(|c| c.cost.total_usd);
    let optimum_is_uniform = optimum_assignment
        .as_ref()
        .map(|ids| !ids.is_empty() && ids.iter().all(|s| s == &ids[0]));

    let best_uniform_spec_id = uniform_rows
        .iter()
        .min_by(|a, b| a.total_usd.total_cmp(&b.total_usd))
        .map(|u| (u.spec_id.clone(), u.total_usd));
    let (grading_delta_usd, grading_delta_percent) =
        match (&best_uniform_spec_id, optimum_total_usd) {
            (Some((_, u_total)), Some(o)) => {
                let d = u_total - o;
                (Some(d), Some(d / u_total * 100.0))
            }
            _ => (None, None),
        };

    Ok(GradingReport {
        schema: GRADING_REPORT_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        source_record_sha256: format!("{:x}", Sha256::digest(record_json.as_bytes())),
        case_sha256: record.case_sha256.clone(),
        graded,
        optimum_assignment,
        optimum_total_usd,
        optimum_is_uniform,
        uniform_rows,
        best_uniform_spec_id: best_uniform_spec_id.map(|(id, _)| id),
        grading_delta_usd,
        grading_delta_percent,
        limitations: vec![
            "Derived view over a committed coupled-search run record — no recompute; every figure traces to a candidate the record's own acceptance recomputation checked. `grading_delta_usd` is best-uniform minus optimum cost under the case's declared (often synthetic) prices — a modeled value, never a supplier quote.".into(),
            "A uniform row means 'one spec on every region', not 'one physical tape product': a spec is a declared material binding + price. `base` is the case-level material binding.".into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coupled_search::{
        CoupledSearchOptions, run_coupled_search_case, tests::reduced_case_json,
        tests::reduced_graded_case_json,
    };

    fn run_record(case_json: &str) -> String {
        let record =
            run_coupled_search_case(case_json, &CoupledSearchOptions { threads: None }).unwrap();
        serde_json::to_string(&record).unwrap()
    }

    #[test]
    fn report_rejects_an_ungraded_record() {
        let record = run_record(&reduced_case_json("[3, 200]", 30.0));
        assert!(grade_report_from_record(&record).is_err());
    }

    #[test]
    fn report_compares_uniform_and_graded_assignments() {
        // 2 turns choices × 2 specs × 2 regions = 8 candidates. cheap spec
        // at $10/m vs base $30/m — same physics binding, cheaper tape.
        let record = run_record(&reduced_graded_case_json("[4, 200]", 10.0, 0.5));
        let report = grade_report_from_record(&record).unwrap();

        assert!(report.graded);
        // Both spec ids surfaced as uniform rows — physics is identical
        // across specs, so a uniform assignment of either passes wherever
        // the geometry does.
        let ids: Vec<&str> = report
            .uniform_rows
            .iter()
            .map(|r| r.spec_id.as_str())
            .collect();
        assert!(ids.contains(&"base"), "rows: {ids:?}");
        assert!(ids.contains(&"cheap"), "rows: {ids:?}");
        // Uniform-cheap is the cheapest uniform row — cheap everywhere
        // beats base everywhere under this price gap.
        assert_eq!(report.best_uniform_spec_id.as_deref(), Some("cheap"));
        // Delta is nonnegative: the optimum is never worse than the best
        // uniform (uniform assignments are inside the graded grid).
        let delta = report.grading_delta_usd.unwrap();
        assert!(delta >= 0.0);
        // Hash-bound to the record bytes actually read.
        assert_eq!(
            report.source_record_sha256,
            format!("{:x}", Sha256::digest(record.as_bytes()))
        );
    }
}
