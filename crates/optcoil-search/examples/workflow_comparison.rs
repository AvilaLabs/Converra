//! Exercise the shared named-study workflow on frozen case and bundle bytes.
//! The companion Python harness drives the explicit-file CLI on those same
//! bytes. This is software workflow evidence, not human-time research.
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

use optcoil_search::{
    coupled_search::CoupledSearchOptions,
    study::{StudyEngineSession, StudyWorkspace},
};
use serde_json::json;

fn read(path: &std::ffi::OsStr) -> Result<String, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?)
}

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
    if args.len() < 3 {
        return Err("usage: workflow_comparison <base-case.json> <revised-case.json> <new-output-dir> [bundle.json ...]".into());
    }
    let base_case = read(&args[0])?;
    let revised_case = read(&args[1])?;
    let output = Path::new(&args[2]);
    fs::create_dir(output)?;
    let bundle_texts: Vec<String> = args[3..]
        .iter()
        .map(|p| read(p))
        .collect::<Result<_, _>>()?;
    let options = CoupledSearchOptions { threads: Some(1) };
    let mut operations = Vec::new();
    let total_start = Instant::now();
    let mut workspace = StudyWorkspace::new("Frozen representative workflow comparison");
    let base_id = workspace.add_variant(
        "Measured baseline",
        base_case.clone(),
        bundle_texts.clone(),
        options,
    )?;
    operations.push("add_variant");

    let started = Instant::now();
    let base_preflight = workspace.preflight_variant(&base_id)?;
    let preflight_base_ms = started.elapsed().as_secs_f64() * 1000.0;
    operations.push("preflight_variant(base)");
    if !base_preflight.ready_to_run {
        return Err("base preflight not ready".into());
    }

    let mut engine = StudyEngineSession::default();
    let solve_start = Instant::now();
    let base_result_id = engine.run_variant(&mut workspace, &base_id)?;
    let base_run_operation_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
    operations.push("run_variant(base)");

    let revised_id = workspace.duplicate_variant(&base_id, "Named price revision +5%")?;
    operations.push("duplicate_variant");
    workspace.revise_variant(&revised_id, revised_case, bundle_texts.clone(), options)?;
    operations.push("revise_variant");
    let started = Instant::now();
    let revised_preflight = workspace.preflight_variant(&revised_id)?;
    let preflight_revised_ms = started.elapsed().as_secs_f64() * 1000.0;
    operations.push("preflight_variant(revised)");
    if !revised_preflight.ready_to_run {
        return Err("revised preflight not ready".into());
    }
    let solve_start = Instant::now();
    let revised_result_id = engine.run_variant(&mut workspace, &revised_id)?;
    let revised_run_operation_ms = solve_start.elapsed().as_secs_f64() * 1000.0;
    operations.push("run_variant(revised)");

    let diff = workspace.case_diff(&base_id, &revised_id)?;
    operations.push("case_diff");
    let base_variant = workspace.variant(&base_id)?;
    if engine.cached_result(base_variant)?.is_none() {
        return Err("base result missing from live session before exact repeat".into());
    }
    operations.push("cached_result(base)");
    let repeat_start = Instant::now();
    let repeated_id = engine.run_variant(&mut workspace, &base_id)?;
    let exact_repeat_operation_ms = repeat_start.elapsed().as_secs_f64() * 1000.0;
    operations.push("run_variant(exact_repeat)");
    if repeated_id != base_result_id {
        return Err("exact repeat did not return the existing result".into());
    }

    let workspace_json = workspace.to_json()?;
    write_new(&output.join("workspace.json"), workspace_json.as_bytes())?;
    operations.push("to_json/save_workspace");
    let reopened = StudyWorkspace::from_json(&workspace_json)?;
    let reopened_engine = StudyEngineSession::default();
    let reopened_cache_empty = reopened_engine
        .cached_result(reopened.variant(&base_id)?)?
        .is_none();
    if !reopened_cache_empty {
        return Err("reopened workspace unexpectedly populated a fresh execution cache".into());
    }
    operations.push("from_json/reopen_workspace");
    operations.push("cached_result(reopened)");

    let package = reopened.review_package_variant(&revised_id, &revised_result_id)?;
    let package_dir = output.join("review-package");
    fs::create_dir(&package_dir)?;
    let mut package_artifacts = Vec::new();
    for (name, bytes) in package.artifacts {
        let path = Path::new(&name);
        let destination = package_dir.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        write_new(&destination, bytes.as_bytes())?;
        package_artifacts.push(name);
    }
    operations.push("review_package_variant/export");

    let record_json = |variant_id: &str, result_id: &str| -> Result<String, Box<dyn Error>> {
        let variant = reopened.variant(variant_id)?;
        let result = variant
            .results
            .iter()
            .find(|r| r.id == result_id)
            .ok_or("result missing")?;
        Ok(result.record_json.clone())
    };
    let base_record = record_json(&base_id, &base_result_id)?;
    let revised_record = record_json(&revised_id, &revised_result_id)?;
    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
    let measured_run_ms =
        base_run_operation_ms + revised_run_operation_ms + exact_repeat_operation_ms;
    let operation_count = operations.len();
    let package_artifact_count = package_artifacts.len();
    let receipt = json!({
        "schema": "converra-workflow-comparison-workspace/v1",
        "scope": "same frozen case, revised case, raw bundles, and single-thread option as the explicit CLI workflow",
        "variant_ids": { "base": base_id, "revised": revised_id },
        "result_ids": { "base": base_result_id, "revised": revised_result_id, "exact_repeat": repeated_id },
        "preflight": { "base_ready": base_preflight.ready_to_run, "revised_ready": revised_preflight.ready_to_run },
        "diff": diff,
        "records": { "base": serde_json::from_str::<serde_json::Value>(&base_record)?, "revised": serde_json::from_str::<serde_json::Value>(&revised_record)? },
        "operations": operations,
        "operation_count": operation_count,
        "package_artifacts": package_artifacts,
        "package_artifact_count": package_artifact_count,
        "timing_ms": {
            "workflow_total_including_solves": total_ms,
            "base_run_variant_wall": base_run_operation_ms,
            "revised_run_variant_wall": revised_run_operation_ms,
            "exact_repeat_operation_wall": exact_repeat_operation_ms,
            "preflight_base": preflight_base_ms,
            "preflight_revised": preflight_revised_ms,
            "orchestration_excluding_run_variant_calls": (total_ms - measured_run_ms).max(0.0),
            "base_search_elapsed_from_record": serde_json::from_str::<serde_json::Value>(&base_record)?["elapsed_ms"],
            "revised_search_elapsed_from_record": serde_json::from_str::<serde_json::Value>(&revised_record)?["elapsed_ms"]
        },
        "exact_repeat_reused_result_id": repeated_id == base_result_id,
        "reopened_execution_cache_empty": reopened_cache_empty,
        "memory_measurement": "not captured by this process-level harness"
    });
    write_new(
        &output.join("workspace-receipt.json"),
        serde_json::to_string_pretty(&receipt)?.as_bytes(),
    )?;
    write_new(&output.join("base-record.json"), base_record.as_bytes())?;
    write_new(
        &output.join("revised-record.json"),
        revised_record.as_bytes(),
    )?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}
