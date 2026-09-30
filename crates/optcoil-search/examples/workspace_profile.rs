//! Measure the complete study operation, including result verification, at
//! unchanged fidelity. Generated output is software evidence, not user research.
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

use optcoil_search::{
    coupled_search::{CoupledSearchOptions, CoupledSearchRunRecord},
    study::{StudyEngineSession, StudyWorkspace, exact_input_key},
};
use serde_json::{Value, json};

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "usage: workspace_profile <case.json> <new-output-directory> [bundle.json ...]".into(),
        );
    }
    let case_json = fs::read_to_string(&args[0])?;
    let output = Path::new(&args[1]);
    fs::create_dir(output)?;
    let bundles: Vec<String> = args[2..]
        .iter()
        .map(fs::read_to_string)
        .collect::<Result<_, _>>()?;
    let options = CoupledSearchOptions { threads: Some(1) };
    let mut workspace = StudyWorkspace::new("Fixed-input iteration measurement");
    let id = workspace.add_variant(
        "Unchanged study",
        case_json.clone(),
        bundles.clone(),
        options,
    )?;
    let original_key = exact_input_key(workspace.variant(&id)?)?;
    let mut engine = StudyEngineSession::default();
    assert!(engine.cached_result(workspace.variant(&id)?)?.is_none());
    let start = Instant::now();
    let first_id = engine.run_variant(&mut workspace, &id)?;
    let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
    let record_json = workspace
        .variant(&id)?
        .results
        .iter()
        .find(|r| r.id == first_id)
        .ok_or("first result missing")?
        .record_json
        .clone();
    let record: CoupledSearchRunRecord = serde_json::from_str(&record_json)?;
    let mut warm_ms = Vec::new();
    for _ in 0..5 {
        assert!(engine.cached_result(workspace.variant(&id)?)?.is_some());
        let start = Instant::now();
        let reused_id = engine.run_variant(&mut workspace, &id)?;
        warm_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        let reused = workspace
            .variant(&id)?
            .results
            .iter()
            .find(|r| r.id == reused_id)
            .ok_or("reused result missing")?;
        assert_eq!(reused.record_json, record_json, "reuse changed evidence");
    }

    // This checks invalidation without treating perturbed inputs as computed
    // alternatives. Each modification is visible and reset to exact source bytes.
    let mut invalidations = Vec::new();
    for (name, pointer, multiplier) in [
        ("price", "/cost/price_usd_per_m", 1.1),
        ("field requirement", "/requirement/b_target_t", 1.01),
        ("temperature", "/operating/temperature_k", 1.001),
    ] {
        let mut value: Value = serde_json::from_str(&case_json)?;
        let original = value
            .pointer(pointer)
            .and_then(Value::as_f64)
            .ok_or("measurement input missing")?;
        *value
            .pointer_mut(pointer)
            .ok_or("measurement pointer missing")? = json!(original * multiplier);
        workspace.revise_variant(
            &id,
            serde_json::to_string(&value)?,
            bundles.clone(),
            options,
        )?;
        assert_ne!(exact_input_key(workspace.variant(&id)?)?, original_key);
        assert!(engine.cached_result(workspace.variant(&id)?)?.is_none());
        invalidations.push(name);
        workspace.revise_variant(&id, case_json.clone(), bundles.clone(), options)?;
        assert!(engine.cached_result(workspace.variant(&id)?)?.is_some());
    }
    let implicit = CoupledSearchOptions { threads: None };
    workspace.revise_variant(&id, case_json.clone(), bundles.clone(), implicit)?;
    assert!(engine.cached_result(workspace.variant(&id)?)?.is_none());
    invalidations.push("execution options");
    workspace.revise_variant(&id, case_json.clone(), bundles.clone(), options)?;

    if !bundles.is_empty() {
        let mut changed = bundles.clone();
        // JSON whitespace changes exact bundle bytes even when measurements are
        // equal. Signed raw-file identities must never be normalized away.
        changed[0].push('\n');
        workspace.revise_variant(&id, case_json.clone(), changed, options)?;
        assert!(engine.cached_result(workspace.variant(&id)?)?.is_none());
        invalidations.push("raw conductor bundle bytes");
        workspace.revise_variant(&id, case_json.clone(), bundles.clone(), options)?;
    }

    let persisted = workspace.to_json()?;
    let reopened = StudyWorkspace::from_json(&persisted)?;
    let new_session = StudyEngineSession::default();
    assert!(new_session.cached_result(reopened.variant(&id)?)?.is_none());
    let mut ordered = warm_ms.clone();
    ordered.sort_by(f64::total_cmp);
    let summary = json!({
        "schema": "converra-study-iteration-measurement/v1",
        "scope": "one cold operation and five exact-input live-session reuses; no physics or screening fidelity changed",
        "source_case": args[0].to_string_lossy(),
        "case_sha256": record.case_sha256,
        "implementation_sha256": record.implementation_sha256,
        "input_sha256": record.input_sha256,
        "options": options,
        "cold_operation_ms": cold_ms,
        "warm_operation_ms": warm_ms,
        "median_warm_operation_ms": ordered[ordered.len()/2],
        "source_run_elapsed_ms": record.elapsed_ms,
        "search_status": record.search_status,
        "acceptance_agreement_status": record.acceptance.agreement_status,
        "source_search_kernel_evaluations": record.kernel_evaluations,
        "record_bytes_identical_on_reuse": true,
        "invalidations_checked": invalidations,
        "reopened_records_populate_cache": false,
        "customer_validation": false
    });
    write_new(&output.join("run-record.json"), record_json.as_bytes())?;
    write_new(&output.join("workspace.json"), persisted.as_bytes())?;
    write_new(
        &output.join("measurement.json"),
        serde_json::to_string_pretty(&summary)?.as_bytes(),
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
