//! Python bindings for Converra's headless engine.
//!
//! The boundary is deliberately JSON: case, spec and record documents
//! keep a single schema definition (in `optcoil-model`), and every
//! function here is a thin adapter over `optcoil-search` — the same
//! code paths the CLI and workbench run. Nothing is reimplemented in
//! Python, so a Python run produces byte-identical records to the CLI.
//!
//! Long-running operations release the GIL; callers can use the module
//! from Python threads. Cancellation is not exposed yet — runs bound
//! themselves via `search_limits`/`max_evaluations` in the case.

use optcoil_model::material::MaterialDataset;
use optcoil_search::bakeoff::run_bakeoff;
use optcoil_search::coupled_search::{
    CoupledSearchOptions, CoupledSearchRunRecord, run_coupled_search_case,
};
use optcoil_search::reel::{
    OperatingPoint, rate_inventory_json, validate_inventory_json, validate_passport_json,
};
use optcoil_search::report::render_search_report_html;
use optcoil_search::sensitivity::run_sensitivity_sweep;
use optcoil_search::synthetic::synthesize_inventory_json;
use optcoil_search::verify::{verify_dataset_bundle, verify_record_checks};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::Serialize;
use std::sync::atomic::AtomicBool;

fn err(e: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn options(threads: Option<u32>) -> CoupledSearchOptions {
    CoupledSearchOptions { threads }
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    outcome: &'static str,
    detail: String,
}

fn outcome_name(o: optcoil_search::verify::Outcome) -> &'static str {
    use optcoil_search::verify::Outcome::*;
    match o {
        Pass => "PASS",
        Fail => "FAIL",
        NotChecked => "NOT_CHECKED",
    }
}

/// Run a coupled-search case. `case_json` is the same v24+ document the
/// CLI accepts; returns the run record JSON. `threads` defaults to all
/// cores, capped by the case's `search_limits.max_threads`.
#[pyfunction]
#[pyo3(signature = (case_json, threads=None))]
fn run_search(py: Python<'_>, case_json: &str, threads: Option<u32>) -> PyResult<String> {
    let record = py
        .detach(|| run_coupled_search_case(case_json, &options(threads)))
        .map_err(err)?;
    serde_json::to_string(&record).map_err(err)
}

/// Verify a run record: artifact bindings, record structure and the
/// independent cost-ledger recompute. Returns a JSON array of
/// `{name, outcome, detail}` checks — outcome is PASS / FAIL /
/// NOT_CHECKED. Physics is verified by `run_search` re-evaluation, not
/// by this check.
#[pyfunction]
#[pyo3(signature = (record_json, case_json=None, dataset_jsons=None))]
fn verify_record(
    record_json: &str,
    case_json: Option<&str>,
    dataset_jsons: Option<Vec<String>>,
) -> PyResult<String> {
    let datasets: Vec<String> = dataset_jsons.unwrap_or_default();
    let refs: Vec<&str> = datasets.iter().map(String::as_str).collect();
    let checks = verify_record_checks(record_json, case_json, None, None, &refs).map_err(err)?;
    let checks: Vec<Check> = checks
        .iter()
        .map(|c| Check {
            name: c.name,
            outcome: outcome_name(c.outcome),
            detail: c.detail.clone(),
        })
        .collect();
    serde_json::to_string(&checks).map_err(err)
}

/// Run a sensitivity sweep spec over a case. Returns the sweep record
/// JSON.
#[pyfunction]
#[pyo3(signature = (case_json, spec_json, threads=None))]
fn run_sensitivity(
    py: Python<'_>,
    case_json: &str,
    spec_json: &str,
    threads: Option<u32>,
) -> PyResult<String> {
    let record = py
        .detach(|| {
            run_sensitivity_sweep(
                case_json,
                spec_json,
                &options(threads),
                None,
                &AtomicBool::new(false),
            )
        })
        .map_err(err)?;
    serde_json::to_string(&record).map_err(err)
}

/// Run a bakeoff spec: the case re-evaluated against every dataset
/// bundle under `bundle_dir`. Returns the bakeoff record JSON.
#[pyfunction]
#[pyo3(signature = (case_json, spec_json, bundle_dir, threads=None))]
fn run_dataset_bakeoff(
    py: Python<'_>,
    case_json: &str,
    spec_json: &str,
    bundle_dir: &str,
    threads: Option<u32>,
) -> PyResult<String> {
    let record = py
        .detach(|| {
            run_bakeoff(
                case_json,
                spec_json,
                &options(threads),
                std::path::Path::new(bundle_dir),
                &AtomicBool::new(false),
            )
        })
        .map_err(err)?;
    serde_json::to_string(&record).map_err(err)
}

/// Render the standalone HTML report for a run record.
#[pyfunction]
fn render_report(record_json: &str) -> PyResult<String> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json).map_err(err)?;
    render_search_report_html(&record).map_err(err)
}

/// The embedded material datasets: id, material, data class and
/// provenance for each. `data_class` distinguishes measured data from
/// declared model fits.
#[pyfunction]
fn list_datasets() -> PyResult<String> {
    #[derive(Serialize)]
    struct Info {
        id: String,
        material: String,
        data_class: String,
        point_count: usize,
        license: String,
        source_doi: String,
    }
    let mut out = Vec::new();
    for id in MaterialDataset::EMBEDDED_IDS {
        let ds = MaterialDataset::embedded_by_id(id).map_err(err)?;
        out.push(Info {
            id: ds.metadata.id.clone(),
            material: ds.metadata.material.clone(),
            data_class: serde_json::to_value(ds.metadata.data_class)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            point_count: ds.metadata.point_count,
            license: ds.metadata.license.clone(),
            source_doi: ds.metadata.source_doi.clone(),
        });
    }
    serde_json::to_string(&out).map_err(err)
}

/// Verify a dataset bundle's schema, content binding and ed25519
/// attestation. `registry_json` binds issuer keys; `pubkey` (an
/// `ed25519:<hex>` string) verifies a signature without a registry.
/// Returns a JSON array of `{name, verdict, detail}` checks.
#[pyfunction]
#[pyo3(signature = (bundle_json, registry_json=None, pubkey=None))]
fn verify_dataset(
    bundle_json: &str,
    registry_json: Option<&str>,
    pubkey: Option<&str>,
) -> PyResult<String> {
    #[derive(Serialize)]
    struct DCheck {
        name: &'static str,
        verdict: &'static str,
        detail: String,
    }
    let checks = verify_dataset_bundle(bundle_json, registry_json, pubkey).map_err(err)?;
    let checks: Vec<DCheck> = checks
        .iter()
        .map(|c| DCheck {
            name: c.name,
            verdict: c.verdict,
            detail: c.detail.clone(),
        })
        .collect();
    serde_json::to_string(&checks).map_err(err)
}

/// Validate a reel passport (`optcoil-reel-passport/v1`) given as JSON
/// text. Returns a JSON summary including `passport_sha256`, the SHA-256
/// of the text's exact bytes.
#[pyfunction]
fn validate_reel_passport(passport_json: &str) -> PyResult<String> {
    serde_json::to_string(&validate_passport_json(passport_json).map_err(err)?).map_err(err)
}

/// Validate a reel inventory (`optcoil-reel-inventory/v1`) given as JSON
/// text. Returns a JSON summary with totals, length by product and
/// evidence-class counts.
#[pyfunction]
fn validate_reel_inventory(inventory_json: &str) -> PyResult<String> {
    serde_json::to_string(&validate_inventory_json(inventory_json).map_err(err)?).map_err(err)
}

/// Rate every reel of an inventory at an operating point. `dataset_jsons`
/// are dataset bundle documents; each product map's id and CSV SHA-256
/// must match the passport, otherwise the reel is `map_unavailable`.
/// Embedded datasets are used for ids that are not supplied. Returns the
/// `optcoil-reel-rating/v1` record JSON.
#[pyfunction]
#[pyo3(signature = (inventory_json, temperature_k, field_t, angle_deg, criterion_v_per_m=1e-4, dataset_jsons=None))]
fn rate_reel_inventory(
    inventory_json: &str,
    temperature_k: f64,
    field_t: f64,
    angle_deg: f64,
    criterion_v_per_m: f64,
    dataset_jsons: Option<Vec<String>>,
) -> PyResult<String> {
    let bundles: Vec<String> = dataset_jsons.unwrap_or_default();
    let refs: Vec<&str> = bundles.iter().map(String::as_str).collect();
    let record = rate_inventory_json(
        inventory_json,
        OperatingPoint {
            temperature_k,
            field_t,
            angle_from_normal_deg: angle_deg,
            electric_field_criterion_v_per_m: criterion_v_per_m,
        },
        &refs,
    )
    .map_err(err)?;
    serde_json::to_string(&record).map_err(err)
}

/// Generate a synthetic reel inventory from a
/// `optcoil-synthetic-inventory-spec/v1` document. `dataset_jsons` supply
/// product maps that are not embedded; their id and CSV SHA-256 must match
/// the spec. Returns `(inventory_json, truth_json)`; the truth file binds
/// the SHA-256 of the exact inventory text. Same seed, same bytes.
#[pyfunction]
#[pyo3(signature = (spec_json, dataset_jsons=None))]
fn synthesize_reel_inventory(
    spec_json: &str,
    dataset_jsons: Option<Vec<String>>,
) -> PyResult<(String, String)> {
    let bundles: Vec<String> = dataset_jsons.unwrap_or_default();
    let refs: Vec<&str> = bundles.iter().map(String::as_str).collect();
    synthesize_inventory_json(spec_json, &refs).map_err(err)
}

/// The Converra engine — same code as the `optcoil` CLI.
#[pymodule]
fn converra(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_search, m)?)?;
    m.add_function(wrap_pyfunction!(verify_record, m)?)?;
    m.add_function(wrap_pyfunction!(run_sensitivity, m)?)?;
    m.add_function(wrap_pyfunction!(run_dataset_bakeoff, m)?)?;
    m.add_function(wrap_pyfunction!(render_report, m)?)?;
    m.add_function(wrap_pyfunction!(list_datasets, m)?)?;
    m.add_function(wrap_pyfunction!(verify_dataset, m)?)?;
    m.add_function(wrap_pyfunction!(validate_reel_passport, m)?)?;
    m.add_function(wrap_pyfunction!(validate_reel_inventory, m)?)?;
    m.add_function(wrap_pyfunction!(rate_reel_inventory, m)?)?;
    m.add_function(wrap_pyfunction!(synthesize_reel_inventory, m)?)?;
    Ok(())
}
