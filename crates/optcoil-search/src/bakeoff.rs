//! Vendor bake-off: one coupled-search case run once per declared entry,
//! ranked into a comparison record.
//!
//! v1 (`optcoil-bakeoff/v1`) answers the materials question — "which
//! measured dataset gives the best savings for this requirement" — by
//! mutating only `material.dataset_id` / `material.csv_sha256`; every
//! declared policy stays identical so rows compare like-for-like.
//!
//! v2 (`optcoil-bakeoff/v2`) answers the procurement question — "which
//! *product* gives the cheapest accepted design" — by mutating the
//! product's real attributes per row: tape width (a winding-topology
//! change, not a label), optional stack thickness, declared price with
//! its provenance class, optional piece catalogue, and optional
//! justified policy overrides. Each row is a different physical case
//! answering the same requirement; comparability comes through the
//! declared baseline topology, so the ranking orders PASS rows by
//! accepted total cost, not savings. Graded cases are refused — a
//! product price cannot honestly reprice per-spec selections.
//!
//! Every row runs an ordinary `run_coupled_search_case_with_dataset`, so
//! its acceptance recomputation stands on its own. An entry whose dataset
//! cannot resolve or whose mutated case fails validation records `error`
//! and `NOT_EVALUATED` rather than sinking the comparison — that failure
//! is itself data ("this entry cannot answer this case").
//!
//! The ranking is mechanical: PASS rows first, then INCONCLUSIVE, then
//! FAIL, then error rows — never a vendor endorsement. A FAIL row means
//! no passing design under that entry, not that the tape is bad.

use std::{
    sync::atomic::AtomicBool,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use optcoil_model::{
    Status,
    bakeoff::{BAKEOFF_SPEC_SCHEMA_V2, BakeoffSpec, MaterialPolicyOverride},
    coupled_search::{CoupledSearchCase, PieceOffering, PriceSource},
    material::MaterialDataset,
};

use crate::{
    RunError,
    coupled_search::{
        CandidateGeometry, CoupledSearchOptions, run_coupled_search_case_with_dataset,
    },
    field::{RuntimeInfo, runtime_info},
};

pub const BAKEOFF_RECORD_SCHEMA: &str = "optcoil-bakeoff-record/v2";

/// One spec entry normalized for the runner: a v1 dataset carries only a
/// binding; a v2 product additionally carries the geometry/price/policy
/// attributes that mutate the case.
struct DeclaredEntry {
    dataset_id: String,
    bundle: Option<String>,
    /// v2 only — the product's display identity; `None` on v1 rows.
    product_id: Option<String>,
    tape_width_m: Option<f64>,
    tape_thickness_m: Option<f64>,
    price_usd_per_m: Option<f64>,
    price_source: Option<PriceSource>,
    piece_offerings: Option<Vec<PieceOffering>>,
    material_policy: Option<MaterialPolicyOverride>,
}

/// One bake-off row: the dataset that ran, the mutated case that pinned it,
/// and the run's headline outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BakeoffEntry {
    /// v2: the product's declared identity. `None` on v1 rows and records
    /// predating record-schema v2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<String>,
    pub dataset_id: String,
    /// The resolved dataset's real CSV hash — bound even when the spec
    /// entry named a bundle path.
    pub dataset_csv_sha256: String,
    /// SHA-256 of the mutated case JSON actually run.
    pub case_sha256: String,
    /// The mutated case verbatim — every row is independently re-runnable.
    pub case_json: String,
    pub optimum: Option<CandidateGeometry>,
    pub optimum_total_usd: Option<f64>,
    pub baseline_total_usd: Option<f64>,
    pub savings_usd: Option<f64>,
    pub savings_percent: Option<f64>,
    /// The selected optimum's `max_utilization` (worst-point demand /
    /// capacity fraction) — the operating margin the price buys.
    pub optimum_max_utilization: Option<f64>,
    pub search_status: Status,
    pub agreement_status: Status,
    /// Set when the dataset could not resolve or the mutated case/run
    /// failed — verdict fields stay `None` and the row ranks last.
    pub error: Option<String>,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BakeoffRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub spec: BakeoffSpec,
    pub spec_sha256: String,
    /// SHA-256 of the *unmutated* case file bytes — the comparison's own
    /// identity input. Each entry additionally carries the sha of the
    /// mutated case that ran.
    pub case_sha256: String,
    /// The dataset the base case declared — the comparison's own baseline
    /// row context (it may or may not appear in `spec.datasets`).
    pub base_dataset_id: String,
    pub coupled_search_model_id: String,
    pub coupled_search_checker_id: String,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    pub runtime: RuntimeInfo,
    /// Entries in spec order.
    pub entries: Vec<BakeoffEntry>,
    /// `dataset_id`s in rank order: PASS rows by `savings_usd` descending,
    /// then INCONCLUSIVE, then FAIL; error rows are unranked and omitted.
    pub ranking: Vec<String>,
    pub ranking_basis: String,
    pub limitations: Vec<String>,
}

impl BakeoffRecord {
    pub fn write_new(&self, path: impl AsRef<std::path::Path>) -> Result<(), RunError> {
        crate::write_json_new(self, path)
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Run every declared dataset of `spec_json` against `case_json`.
/// `bundle_root` resolves a spec entry's relative `bundle` path (the spec
/// file's directory — the CLI passes it; tests pass the working directory).
pub fn run_bakeoff(
    case_json: &str,
    spec_json: &str,
    options: &CoupledSearchOptions,
    bundle_root: &std::path::Path,
    cancel: &AtomicBool,
) -> Result<BakeoffRecord, RunError> {
    let started = Instant::now();
    let started_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| RunError::Invalid(e.to_string()))?
        .as_millis() as u64;

    let spec = BakeoffSpec::from_json(spec_json)?;
    let base_case = CoupledSearchCase::from_json(case_json)?;
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
    let options = CoupledSearchOptions {
        threads: Some(execution_threads),
    };

    // v2 is a procurement comparison: every product reprices the base
    // conductor. A graded case's specs carry their own per-spec prices,
    // so a single product price would silently reprice only the
    // ungraded share — misleading. Reject up front rather than emit a
    // row of error entries.
    let is_v2 = spec.schema == BAKEOFF_SPEC_SCHEMA_V2;
    if is_v2 {
        let graded = base_case.grading.is_some()
            || base_case.tape_specs.as_ref().is_some_and(|s| !s.is_empty());
        if graded {
            return Err(RunError::Invalid(
                "optcoil-bakeoff/v2 products reprice the base conductor only; graded cases need per-product per-spec pricing, which the v2 spec does not carry".into(),
            ));
        }
        // Every v2 product declares `price_source` — a v24-gated field —
        // so the mutated case is only legal when the base case already
        // runs at v24. Requiring the author to bump the case keeps the
        // schema change a declared input, not a silent runner edit.
        if base_case.schema != optcoil_model::coupled_search::COUPLED_SEARCH_CASE_SCHEMA_V24 {
            return Err(RunError::Invalid(format!(
                "optcoil-bakeoff/v2 requires the case at {} (product prices carry v24 provenance fields); the case declares `{}`",
                optcoil_model::coupled_search::COUPLED_SEARCH_CASE_SCHEMA_V24,
                base_case.schema
            )));
        }
    }

    let declared_entries: Vec<DeclaredEntry> = if is_v2 {
        spec.products
            .iter()
            .map(|p| DeclaredEntry {
                dataset_id: p.dataset_id.clone(),
                bundle: p.bundle.clone(),
                product_id: Some(p.product_id.clone()),
                tape_width_m: p.tape_width_m,
                tape_thickness_m: p.tape_thickness_m,
                price_usd_per_m: Some(p.price_usd_per_m),
                price_source: Some(p.price_source),
                piece_offerings: p.piece_offerings.clone(),
                material_policy: p.material_policy.clone(),
            })
            .collect()
    } else {
        spec.datasets
            .iter()
            .map(|d| DeclaredEntry {
                dataset_id: d.dataset_id.clone(),
                bundle: d.bundle.clone(),
                product_id: None,
                tape_width_m: None,
                tape_thickness_m: None,
                price_usd_per_m: None,
                price_source: None,
                piece_offerings: None,
                material_policy: None,
            })
            .collect()
    };

    let mut entries = Vec::with_capacity(declared_entries.len());
    for declared in &declared_entries {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(RunError::Cancelled);
        }
        let entry_started = Instant::now();
        // Resolve the dataset first — a resolution failure still records a
        // row (dataset_id known, csv unknown), marked by its error.
        let resolved = match &declared.bundle {
            Some(bundle) => {
                let path = bundle_root.join(bundle);
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("{e}"))
                    .and_then(|json| {
                        MaterialDataset::from_bundle_json(&json).map_err(|e| e.to_string())
                    })
            }
            None => {
                MaterialDataset::embedded_by_id(&declared.dataset_id).map_err(|e| e.to_string())
            }
        };
        let make_error =
            |error: String, case_json_mut: String, ds_id: String, ds_hash: String| BakeoffEntry {
                product_id: declared.product_id.clone(),
                dataset_id: ds_id,
                dataset_csv_sha256: ds_hash,
                case_sha256: hash(case_json_mut.as_bytes()),
                case_json: case_json_mut,
                optimum: None,
                optimum_total_usd: None,
                baseline_total_usd: None,
                savings_usd: None,
                savings_percent: None,
                optimum_max_utilization: None,
                search_status: Status::NotEvaluated,
                agreement_status: Status::NotEvaluated,
                error: Some(error),
                elapsed_ms: entry_started.elapsed().as_secs_f64() * 1000.0,
            };

        let ds = match resolved {
            Ok(ds) => ds,
            Err(e) => {
                let mut stub = base_case.clone();
                stub.material.dataset_id = declared.dataset_id.clone();
                stub.material.csv_sha256.clear();
                let stub_json =
                    serde_json::to_string(&stub).map_err(|e| RunError::Invalid(e.to_string()))?;
                entries.push(make_error(
                    format!("dataset resolution failed: {e}"),
                    stub_json,
                    declared.dataset_id.clone(),
                    String::new(),
                ));
                continue;
            }
        };
        if ds.metadata.id != declared.dataset_id {
            let mut stub = base_case.clone();
            stub.material.dataset_id = declared.dataset_id.clone();
            stub.material.csv_sha256 = ds.metadata.csv_sha256.clone();
            let stub_json =
                serde_json::to_string(&stub).map_err(|e| RunError::Invalid(e.to_string()))?;
            entries.push(make_error(
                format!(
                    "declared dataset_id `{}` does not match resolved `{}`",
                    declared.dataset_id, ds.metadata.id
                ),
                stub_json,
                declared.dataset_id.clone(),
                ds.metadata.csv_sha256.clone(),
            ));
            continue;
        }

        let mut case = base_case.clone();
        case.material.dataset_id = ds.metadata.id.clone();
        case.material.csv_sha256 = ds.metadata.csv_sha256.clone();
        // v2 product attributes: the product's real tape width (a
        // winding-topology change, not a label), stack thickness for the
        // bend bound, its declared price with provenance, an optional
        // piece catalogue, and any justified policy overrides.
        if let Some(p) = &declared.material_policy {
            p.apply(&mut case.material);
        }
        if let Some(w) = declared.tape_width_m {
            case.fixed_geometry.tape_width_m = w;
        }
        if let Some(t) = declared.tape_thickness_m {
            match &mut case.manufacturing {
                Some(m) => m.tape_thickness_m = Some(t),
                None => {
                    let mut stub = case.clone();
                    stub.material.csv_sha256 = ds.metadata.csv_sha256.clone();
                    entries.push(make_error(
                        "product declares tape_thickness_m but the case declares no manufacturing block to carry it".into(),
                        serde_json::to_string(&stub)
                            .map_err(|e| RunError::Invalid(e.to_string()))?,
                        declared.dataset_id.clone(),
                        ds.metadata.csv_sha256.clone(),
                    ));
                    continue;
                }
            }
        }
        if let Some(p) = declared.price_usd_per_m {
            case.cost.price_usd_per_m = p;
        }
        if let Some(s) = declared.price_source {
            case.cost.price_source = Some(s);
        }
        if let Some(o) = &declared.piece_offerings {
            case.cost.piece_offerings = Some(o.clone());
        }
        let mutated_json =
            serde_json::to_string(&case).map_err(|e| RunError::Invalid(e.to_string()))?;
        let case_sha256 = hash(mutated_json.as_bytes());

        // The mutation can produce an invalid combination — a piece
        // catalogue without a declared policy, a width that breaks
        // field-map extent consistency, a thickness without a declared
        // bend bound. Re-validate before running so the row records the
        // real reason rather than a solver error.
        if let Err(e) = CoupledSearchCase::from_json(&mutated_json) {
            entries.push(make_error(
                format!("mutated case failed validation: {e}"),
                mutated_json,
                ds.metadata.id.clone(),
                ds.metadata.csv_sha256.clone(),
            ));
            continue;
        }

        let result =
            run_coupled_search_case_with_dataset(&mutated_json, &options, Some(ds.clone()), cancel);
        match result {
            Ok(record) => {
                let best = record.best_index.map(|i| &record.candidates[i]);
                entries.push(BakeoffEntry {
                    product_id: declared.product_id.clone(),
                    dataset_id: ds.metadata.id.clone(),
                    dataset_csv_sha256: ds.metadata.csv_sha256.clone(),
                    case_sha256,
                    case_json: mutated_json,
                    optimum: best.map(|c| c.geometry.clone()),
                    optimum_total_usd: best.map(|c| c.cost.total_usd),
                    baseline_total_usd: Some(
                        record.candidates[record.baseline_index].cost.total_usd,
                    ),
                    savings_usd: record.savings_usd,
                    savings_percent: record.savings_percent,
                    optimum_max_utilization: best
                        .and_then(|c| c.screening.as_ref())
                        .and_then(|s| s.max_utilization),
                    search_status: record.search_status,
                    agreement_status: record.acceptance.agreement_status,
                    error: None,
                    elapsed_ms: entry_started.elapsed().as_secs_f64() * 1000.0,
                });
            }
            Err(e) => {
                entries.push(make_error(
                    e.to_string(),
                    mutated_json,
                    ds.metadata.id.clone(),
                    ds.metadata.csv_sha256.clone(),
                ));
            }
        }
    }

    // Mechanical ranking: v1 ranks PASS rows by modeled savings
    // descending (like-for-like datasets under one declared case); v2 is
    // a procurement comparison and ranks PASS rows by the accepted
    // optimum's total modeled cost ascending — the purchasable figure.
    // Then INCONCLUSIVE, then FAIL; error rows are unranked. The ranking
    // names products on v2 rows, datasets on v1.
    let rank_class = |e: &BakeoffEntry| -> u8 {
        if e.error.is_some() {
            3
        } else {
            match e.search_status {
                Status::Pass => 0,
                Status::Inconclusive => 1,
                Status::Fail => 2,
                Status::NotEvaluated => 3,
            }
        }
    };
    let mut ranked: Vec<&BakeoffEntry> = entries.iter().filter(|e| e.error.is_none()).collect();
    ranked.sort_by(|a, b| {
        rank_class(a).cmp(&rank_class(b)).then_with(|| {
            if is_v2 {
                a.optimum_total_usd
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&b.optimum_total_usd.unwrap_or(f64::INFINITY))
            } else {
                b.savings_usd
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.savings_usd.unwrap_or(f64::NEG_INFINITY))
            }
        })
    });
    let label = |e: &BakeoffEntry| e.product_id.clone().unwrap_or_else(|| e.dataset_id.clone());
    let ranking: Vec<String> = ranked.iter().map(|e| label(e)).collect();

    let mut limitations = vec![
        "Vendor bake-off: each row is an ordinary coupled-search record whose acceptance recomputation stands alone; this record adds only the comparison — rankings are mechanical, never a vendor endorsement. A row's error means that entry cannot answer this case under its declared attributes — it is not a tape defect.".into(),
    ];
    if is_v2 {
        limitations.push(
            "v2 procurement comparison: each row mutates the product's real tape width/thickness, declared price and any declared policy overrides — rows are different physical cases answering the same requirement, comparable through the declared baseline topology, not identical windings. Prices are case-author declarations labeled by price_source; `quoted` means a declared quote, not a live supplier feed. A product's dataset may not support its declared width — bridge-to-full-width applicability remains a declared assumption per the dataset's own provenance.".into(),
        );
    } else {
        limitations.push(
            "Like-for-like basis: only material.dataset_id / csv_sha256 mutate per row; every other declared policy (low-field clamp, field basis, angle mapping, width transfer, screens) stays identical. Cost figures are modeled under the case's declared prices — invented placeholders carry no supplier basis.".into(),
        );
    }

    Ok(BakeoffRecord {
        schema: BAKEOFF_RECORD_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        spec,
        spec_sha256: hash(spec_json.as_bytes()),
        case_sha256: hash(case_json.as_bytes()),
        base_dataset_id: base_case.material.dataset_id.clone(),
        coupled_search_model_id: crate::coupled_search::COUPLED_SEARCH_MODEL_ID.into(),
        coupled_search_checker_id: crate::coupled_search::COUPLED_SEARCH_CHECKER_ID.into(),
        started_unix_ms,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        runtime: runtime_info(),
        entries,
        ranking,
        ranking_basis: if is_v2 {
            "PASS rows by accepted optimum_total_usd ascending, then INCONCLUSIVE, then FAIL; error rows unranked".into()
        } else {
            "PASS rows by modeled savings_usd descending, then INCONCLUSIVE, then FAIL; error rows unranked".into()
        },
        limitations,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use optcoil_model::Status;

    use super::*;
    use crate::coupled_search::tests::reduced_case_json;

    fn spec_json(entries: &str) -> String {
        format!(
            r#"{{"schema": "optcoil-bakeoff/v1",
            "provenance": "bakeoff.rs unit test fixture; not a frozen benchmark.",
            "datasets": [{entries}]}}"#
        )
    }

    fn run(case: &str, spec: &str) -> BakeoffRecord {
        run_bakeoff(
            case,
            spec,
            &CoupledSearchOptions { threads: None },
            std::path::Path::new("."),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    #[test]
    fn bakeoff_runs_each_dataset_and_binds_identities() {
        let case = reduced_case_json("[3, 200]", 30.0);
        let spec = spec_json(
            r#"{"dataset_id": "robinson-superpower-ap-v3"},
               {"dataset_id": "robinson-shanghai-hflt-v3"}"#,
        );
        let record = run(&case, &spec);

        assert_eq!(record.entries.len(), 2);
        assert_eq!(record.entries[0].dataset_id, "robinson-superpower-ap-v3");
        assert_eq!(record.entries[1].dataset_id, "robinson-shanghai-hflt-v3");
        for entry in &record.entries {
            assert!(entry.error.is_none());
            assert!(!entry.case_sha256.is_empty());
            assert!(!entry.dataset_csv_sha256.is_empty());
            // The mutated case parses and pins exactly this dataset.
            let parsed = CoupledSearchCase::from_json(&entry.case_json).unwrap();
            assert_eq!(parsed.material.dataset_id, entry.dataset_id);
            assert_eq!(parsed.material.csv_sha256, entry.dataset_csv_sha256);
        }
        // Identities differ per dataset — no row can alias another.
        assert_ne!(record.entries[0].case_sha256, record.entries[1].case_sha256);
        assert_ne!(
            record.entries[0].dataset_csv_sha256,
            record.entries[1].dataset_csv_sha256
        );
        // Ranking covers every non-error entry exactly once.
        assert_eq!(record.ranking.len(), 2);
    }

    #[test]
    fn bakeoff_records_unresolvable_dataset_as_error_row() {
        let case = reduced_case_json("[3, 200]", 30.0);
        let spec = spec_json(
            r#"{"dataset_id": "robinson-superpower-ap-v3"},
               {"dataset_id": "no-such-dataset"}"#,
        );
        let record = run(&case, &spec);

        assert_eq!(record.entries.len(), 2);
        let bad = &record.entries[1];
        assert!(bad.error.is_some());
        assert_eq!(bad.search_status, Status::NotEvaluated);
        // The failed row is unranked; the good row still ran.
        assert_eq!(record.ranking, vec!["robinson-superpower-ap-v3"]);
    }

    fn product_spec_json(entries: &str) -> String {
        format!(
            r#"{{"schema": "optcoil-bakeoff/v2",
            "provenance": "bakeoff.rs unit test fixture; not a frozen benchmark.",
            "products": [{entries}]}}"#
        )
    }

    /// The reduced fixture at schema v24 — v2 specs require it (product
    /// prices carry the v24 `price_source` provenance field). The v1
    /// fixture lacks the v2+ blocks, so `good_field_region` and
    /// `refinement` are added to satisfy the newer gates.
    fn reduced_v24_case_json() -> String {
        let mut v: serde_json::Value =
            serde_json::from_str(&reduced_case_json("[3, 200]", 30.0)).unwrap();
        v["schema"] = serde_json::json!("optcoil-coupled-search/v24");
        v["requirement"]["good_field_region"] = serde_json::json!({
            "half_extents_m": [0.05, 0.05, 0.05],
            "points_per_axis": 3
        });
        v["refinement"] = serde_json::json!({
            "pancake_counts": [2],
            "turn_resolution": 10,
            "turn_bounds": {"min": 1, "max": 100},
            "brackets": [{"tapes": 2, "fail_turns": 1, "pass_turns": 3}],
            "monotonicity_check": false
        });
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn bakeoff_v2_mutates_product_attributes_and_ranks_by_total_cost() {
        let case = reduced_v24_case_json();
        let spec = product_spec_json(
            r#"{"product_id": "wide-12mm", "dataset_id": "robinson-superpower-ap-v3",
                "tape_width_m": 0.012, "price_usd_per_m": 40.0, "price_source": "quoted"},
               {"product_id": "narrow-4mm", "dataset_id": "robinson-superpower-ap-v3",
                "tape_width_m": 0.004, "price_usd_per_m": 15.0, "price_source": "estimated",
                "material_policy": {"low_field_clamp_t": 1.01}}"#,
        );
        let record = run(&case, &spec);

        assert_eq!(record.entries.len(), 2);
        let wide = &record.entries[0];
        let narrow = &record.entries[1];
        assert_eq!(wide.product_id.as_deref(), Some("wide-12mm"));
        assert_eq!(narrow.product_id.as_deref(), Some("narrow-4mm"));

        // The mutated case carries the product's real attributes —
        // width is a winding-topology change, price carries provenance.
        let wcase = CoupledSearchCase::from_json(&wide.case_json).unwrap();
        assert_eq!(wcase.fixed_geometry.tape_width_m, 0.012);
        assert_eq!(wcase.cost.price_usd_per_m, 40.0);
        assert_eq!(
            wcase.cost.price_source,
            Some(optcoil_model::coupled_search::PriceSource::Quoted)
        );
        let ncase = CoupledSearchCase::from_json(&narrow.case_json).unwrap();
        assert_eq!(ncase.fixed_geometry.tape_width_m, 0.004);
        assert_eq!(ncase.cost.price_usd_per_m, 15.0);
        assert_eq!(
            ncase.cost.price_source,
            Some(optcoil_model::coupled_search::PriceSource::Estimated)
        );
        // The declared policy override applied to that row only.
        assert_eq!(ncase.material.low_field_clamp_t, 1.01);
        assert_eq!(wcase.material.low_field_clamp_t, 1.001);

        // v2 ranks PASS rows by accepted total cost, ascending — pin the
        // mechanism by reproducing the expected order from the entries
        // (the reduced fixture's physics decides which rows pass).
        assert_eq!(
            record.ranking_basis,
            "PASS rows by accepted optimum_total_usd ascending, then INCONCLUSIVE, then FAIL; error rows unranked"
        );
        let class = |e: &BakeoffEntry| -> u8 {
            if e.error.is_some() {
                3
            } else {
                match e.search_status {
                    Status::Pass => 0,
                    Status::Inconclusive => 1,
                    Status::Fail => 2,
                    Status::NotEvaluated => 3,
                }
            }
        };
        let mut expected: Vec<&BakeoffEntry> = record
            .entries
            .iter()
            .filter(|e| e.error.is_none())
            .collect();
        expected.sort_by(|a, b| {
            class(a).cmp(&class(b)).then_with(|| {
                a.optimum_total_usd
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&b.optimum_total_usd.unwrap_or(f64::INFINITY))
            })
        });
        assert_eq!(
            record.ranking,
            expected
                .iter()
                .map(|e| e.product_id.clone().unwrap())
                .collect::<Vec<_>>(),
            "ranking is product ids ordered PASS-by-total-cost, then INCONCLUSIVE, then FAIL"
        );
    }

    #[test]
    fn bakeoff_v2_thickness_without_manufacturing_block_errors() {
        let case = reduced_v24_case_json();
        let spec = product_spec_json(
            r#"{"product_id": "thick-tape", "dataset_id": "robinson-superpower-ap-v3",
                "tape_thickness_m": 0.0001, "price_usd_per_m": 40.0,
                "price_source": "published"}"#,
        );
        let record = run(&case, &spec);
        let entry = &record.entries[0];
        assert!(entry.error.is_some());
        assert!(entry.error.as_ref().unwrap().contains("manufacturing"));
        assert_eq!(entry.search_status, Status::NotEvaluated);
    }

    #[test]
    fn bakeoff_v2_invalid_mutation_records_error_row() {
        // A piece catalogue without a declared piece_policy fails the
        // mutated case's own v24 validation — the row records why.
        let case = reduced_v24_case_json();
        let spec = product_spec_json(
            r#"{"product_id": "cut-pieces", "dataset_id": "robinson-superpower-ap-v3",
                "price_usd_per_m": 40.0, "price_source": "quoted",
                "piece_offerings": [{"length_m": 300.0, "price_usd_per_m": 42.0}]}"#,
        );
        let record = run(&case, &spec);
        let entry = &record.entries[0];
        assert!(
            entry
                .error
                .as_ref()
                .is_some_and(|e| e.contains("validation")),
            "{:?}",
            entry.error
        );
    }

    #[test]
    fn bakeoff_v2_rejects_graded_case() {
        let mut v: serde_json::Value = serde_json::from_str(&reduced_v24_case_json()).unwrap();
        let material = v["material"].clone();
        v["tape_specs"] = serde_json::json!({
            "ext": {"material": material, "price_usd_per_m": 60.0}
        });
        v["grading"] = serde_json::json!({
            "regions": [{"turn_range": [0.5, 1.0], "tape_spec_choices": ["base", "ext"]}]
        });
        v["baseline"]["tape_spec_ids"] = serde_json::json!(["base"]);
        let spec = product_spec_json(
            r#"{"product_id": "p", "dataset_id": "robinson-superpower-ap-v3",
                "price_usd_per_m": 40.0, "price_source": "quoted"}"#,
        );
        let err = run_bakeoff(
            &serde_json::to_string(&v).unwrap(),
            &spec,
            &CoupledSearchOptions { threads: None },
            std::path::Path::new("."),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.to_string().contains("graded"), "{err}");
    }
}
