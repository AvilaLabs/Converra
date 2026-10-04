use std::{fs, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

use optcoil_search::{
    coupled_search::{CoupledSearchOptions, run_coupled_search_case},
    robustness::{RobustnessSpec, run_study_robustness},
    study::StudyWorkspace,
};
use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Value, json};

fn temp_workspace(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "optcoil-mcp-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn small_case() -> String {
    let mut case: Value =
        serde_json::from_str(include_str!("../../../benchmarks/coupled/oc-019.json")).unwrap();
    case["choices"]["turns_along_normal"] = json!([case["baseline"]["turns_along_normal"]]);
    case["choices"]["tapes_along_width"] = json!([case["baseline"]["tapes_along_width"]]);
    serde_json::to_string(&case).unwrap()
}

fn read_artifacts(
    root: &std::path::Path,
    current: &std::path::Path,
    out: &mut Vec<(String, String)>,
) {
    for entry in fs::read_dir(current).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            read_artifacts(root, &entry.path(), out);
        } else {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, fs::read_to_string(entry.path()).unwrap()));
        }
    }
}

async fn launch(path: &PathBuf) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_optcoil-mcp")).configure(|command| {
            command.arg("--workspace-dir").arg(path);
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    args: Value,
) -> Value {
    let arguments = serde_json::from_value(args).unwrap();
    let response = client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
        .await
        .unwrap();
    response.structured_content.unwrap_or_else(|| {
        response
            .content
            .first()
            .and_then(|item| match item {
                rmcp::model::ContentBlock::Text(text) => serde_json::from_str(&text.text)
                    .ok()
                    .or_else(|| Some(json!({ "text": text.text }))),
                _ => None,
            })
            .unwrap_or_else(|| json!({ "is_error": response.is_error }))
    })
}

fn robustness_spec() -> Value {
    json!({
        "schema": "optcoil-robustness-spec/v1",
        "scenarios": [
            {
                "id": "nominal",
                "name": "Nominal conditions",
                "price_multipliers": {},
                "ic_multipliers": {},
                "temperature_offset_k": 0.0
            },
            {
                "id": "bounded-what-if",
                "name": "Explicit price, Ic, and temperature what-if",
                "price_multipliers": { "robinson-superpower-ap-v3": 1.05 },
                "ic_multipliers": { "robinson-superpower-ap-v3": 1.02 },
                "temperature_offset_k": 0.5
            }
        ]
    })
}

fn robustness_semantics(record: &Value) -> Value {
    let mut normalized = record.clone();
    let root = normalized.as_object_mut().unwrap();
    root.remove("started_unix_ms");
    root.remove("elapsed_ms");
    if let Some(rows) = root.get_mut("rows").and_then(Value::as_array_mut) {
        for row in rows {
            let Some(text) = row
                .get("run_record_json")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let mut run: Value = serde_json::from_str(&text).unwrap();
            if let Some(run_root) = run.as_object_mut() {
                run_root.remove("started_unix_ms");
                run_root.remove("elapsed_ms");
                if let Some(candidates) =
                    run_root.get_mut("candidates").and_then(Value::as_array_mut)
                {
                    for candidate in candidates {
                        if let Some(candidate) = candidate.as_object_mut() {
                            candidate.remove("field_timing_ms");
                        }
                    }
                }
            }
            row["run_record_json"] = Value::String(serde_json::to_string(&run).unwrap());
        }
    }
    normalized
}

async fn wait_for_job(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    job_id: &str,
) -> Value {
    let mut status = Value::Null;
    for _ in 0..600 {
        status = call(client, "get_job", json!({ "job_id": job_id })).await;
        if status["phase"] != "running" {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job did not reach a terminal state: {status}");
}

#[tokio::test]
async fn stdio_discovery_calls_restart_and_fresh_calculation() {
    let root = temp_workspace("restart");
    let client = launch(&root).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|tool| tool.name == "start_search"));
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "export_review_package")
    );
    let capabilities = call(&client, "engine_capabilities", json!({})).await;
    assert_eq!(
        capabilities["engineering_acceptance"],
        "not established by search or screening PASS"
    );

    // Real small declared benchmark input verifies the schema at the stdio boundary.
    let case_json = small_case();
    let created = call(
        &client,
        "create_variant",
        json!({ "name": "stdio fixture", "case_json": case_json.clone(), "dataset_bundles": [], "threads": 1 }),
    )
    .await;
    let variant_id = created["variant_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create response: {created}"))
        .to_owned();
    let first_job = call(&client, "start_search", json!({ "variant_id": variant_id })).await;
    let first_job_id = first_job["job_id"].as_str().unwrap().to_owned();
    let mut first_status = Value::Null;
    for _ in 0..240 {
        first_status = call(&client, "get_job", json!({ "job_id": first_job_id })).await;
        if first_status["phase"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(first_status["phase"], "completed", "{first_status}");
    let first_results = call(&client, "list_results", json!({ "variant_id": variant_id })).await;
    assert_eq!(first_results.as_array().unwrap().len(), 1);
    client.cancel().await.unwrap();

    // Persisted source records remain visible on restart, but are not the runner cache.
    let client = launch(&root).await;
    let before = call(&client, "list_results", json!({ "variant_id": variant_id })).await;
    assert_eq!(before.as_array().unwrap().len(), 1);
    let second_job = call(&client, "start_search", json!({ "variant_id": variant_id })).await;
    assert_eq!(second_job["reused_calculation"], false);
    let second_job_id = second_job["job_id"].as_str().unwrap().to_owned();
    let mut second_status = Value::Null;
    for _ in 0..240 {
        second_status = call(&client, "get_job", json!({ "job_id": second_job_id })).await;
        if second_status["phase"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(second_status["phase"], "completed", "{second_status}");
    let after = call(&client, "list_results", json!({ "variant_id": variant_id })).await;
    assert_eq!(after.as_array().unwrap().len(), 2);
    let original_result_id = first_results[0]["result_id"].as_str().unwrap();
    let latest_result = after.as_array().unwrap().last().unwrap()["result_id"]
        .as_str()
        .unwrap();
    assert_ne!(
        latest_result, original_result_id,
        "restart should calculate a fresh record"
    );
    let latest_result_uri = after.as_array().unwrap().last().unwrap()["resource"]
        .as_str()
        .unwrap();
    let latest_record_resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            latest_result_uri,
        ))
        .await
        .unwrap();
    let latest_record_text = match &latest_record_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("fresh run record resource was not text JSON"),
    };
    let export = call(
        &client,
        "export_review_package",
        json!({ "variant_id": variant_id, "result_id": latest_result }),
    )
    .await;
    assert!(
        export["resources"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "{export}"
    );
    let export_dir = root
        .join("exports")
        .join(export["export_id"].as_str().unwrap());
    let mut exported_artifacts = Vec::new();
    read_artifacts(&export_dir, &export_dir, &mut exported_artifacts);
    optcoil_search::review::verify_review_package(&exported_artifacts).unwrap();
    let summary = call(
        &client,
        "summarize_variant",
        json!({ "variant_id": variant_id }),
    )
    .await;
    let summarized_index = summary["latest_decision"]["selected_candidate_index"].clone();
    let result_uri = first_results[0]["resource"].as_str().unwrap();
    let record_resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(result_uri))
        .await
        .unwrap();
    let record_text = match &record_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("run record resource was not text JSON"),
    };
    let mut source_record: Value = serde_json::from_str(record_text).unwrap();
    assert_eq!(summarized_index, source_record["best_index"]);
    let mut headless = serde_json::to_value(
        run_coupled_search_case(&case_json, &CoupledSearchOptions { threads: Some(1) }).unwrap(),
    )
    .unwrap();
    for record in [&mut source_record, &mut headless] {
        record.as_object_mut().unwrap().remove("started_unix_ms");
        record.as_object_mut().unwrap().remove("elapsed_ms");
        if let Some(candidates) = record["candidates"].as_array_mut() {
            for candidate in candidates {
                candidate.as_object_mut().unwrap().remove("field_timing_ms");
            }
        }
    }
    assert_eq!(
        source_record, headless,
        "MCP record differs from the headless runner"
    );
    let renamed = call(
        &client,
        "rename_variant",
        json!({ "variant_id": variant_id, "name": "renamed stdio fixture" }),
    )
    .await;
    assert_eq!(renamed["variant_id"], variant_id);

    let repeat_job = call(&client, "start_search", json!({ "variant_id": variant_id })).await;
    let repeat_job_id = repeat_job["job_id"].as_str().unwrap().to_owned();
    let mut repeat_status = Value::Null;
    for _ in 0..240 {
        repeat_status = call(&client, "get_job", json!({ "job_id": repeat_job_id })).await;
        if repeat_status["phase"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(repeat_status["phase"], "completed", "{repeat_status}");
    assert_eq!(repeat_status["reused_calculation"], true, "{repeat_status}");
    let repeat_result = call(
        &client,
        "get_job_result",
        json!({ "job_id": repeat_job_id }),
    )
    .await;
    assert_eq!(repeat_result["result_id"], latest_result);
    let same_results = call(&client, "list_results", json!({ "variant_id": variant_id })).await;
    assert_eq!(same_results.as_array().unwrap().len(), 2);
    let repeated_resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            latest_result_uri,
        ))
        .await
        .unwrap();
    let repeated_text = match &repeated_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("cached run record resource was not text JSON"),
    };
    assert_eq!(repeated_text, &latest_record_text);
    let manifest_uri = export["manifest_resource"].as_str().unwrap();
    let manifest = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(manifest_uri))
        .await
        .unwrap();
    assert!(!manifest.contents.is_empty());
    let manifest_text = match &manifest.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("export manifest resource was not text JSON"),
    };
    client.cancel().await.unwrap();
    let client = launch(&root).await;
    let restarted_manifest = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(manifest_uri))
        .await
        .unwrap();
    let restarted_text = match &restarted_manifest.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("restarted export manifest resource was not text JSON"),
    };
    assert_eq!(restarted_text, &manifest_text);
    client.cancel().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stdio_schema_errors_cancellation_and_resources() {
    let root = temp_workspace("cancel");
    let client = launch(&root).await;
    let tools = client.list_all_tools().await.unwrap();
    let create = tools
        .iter()
        .find(|tool| tool.name == "create_variant")
        .unwrap();
    assert!(!create.input_schema.is_empty());

    let bad = call(
        &client,
        "preflight_variant",
        json!({ "variant_id": "missing" }),
    )
    .await;
    assert!(bad.get("text").is_some() || bad["is_error"] == true || bad.is_null());

    let case_json = small_case();
    let created = call(
        &client,
        "create_variant",
        json!({ "name": "cancel fixture", "case_json": case_json, "dataset_bundles": [] }),
    )
    .await;
    let variant_id = created["variant_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create response: {created}"));
    let job = call(&client, "start_search", json!({ "variant_id": variant_id })).await;
    let job_id = job["job_id"].as_str().unwrap();
    let cancel = call(&client, "cancel_job", json!({ "job_id": job_id })).await;
    assert_eq!(cancel["cancellation_requested"], true);
    let mut status = Value::Null;
    for _ in 0..240 {
        status = call(&client, "get_job", json!({ "job_id": job_id })).await;
        if status["phase"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(matches!(
        status["phase"].as_str(),
        Some("cancelled" | "completed")
    ));
    let resources = client.list_all_resources().await.unwrap();
    assert!(
        resources
            .iter()
            .any(|r| r.uri == "optcoil://study/workspace")
    );
    assert!(
        resources
            .iter()
            .any(|r| r.uri == "optcoil://examples/first-study")
    );
    let example = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "optcoil://examples/first-study",
        ))
        .await
        .unwrap();
    let example_json = match &example.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("example case resource was not JSON text"),
    };
    optcoil_model::coupled_search::CoupledSearchCase::from_json(example_json).unwrap();

    let sensitivity_case = small_case();
    let sensitivity_variant = call(
        &client,
        "create_variant",
        json!({ "name": "sensitivity fixture", "case_json": sensitivity_case, "dataset_bundles": [], "threads": 1 }),
    )
    .await;
    let sensitivity_variant_id = sensitivity_variant["variant_id"].as_str().unwrap();
    let spec_json = json!({
        "schema": "optcoil-sensitivity/v1",
        "id": "mcp-single-point-price",
        "provenance": "bounded stdio wiring fixture",
        "axes": [{ "kind": "price_usd_per_m", "values": [30.0] }]
    })
    .to_string();
    let sensitivity = call(
        &client,
        "start_sensitivity",
        json!({ "variant_id": sensitivity_variant_id, "spec_json": spec_json }),
    )
    .await;
    let sensitivity_job_id = sensitivity["job_id"].as_str().unwrap();
    assert_eq!(sensitivity["kind"], "sensitivity");
    assert!(
        sensitivity["phase_label"]
            .as_str()
            .unwrap()
            .contains("progress unavailable")
    );
    assert_eq!(
        call(
            &client,
            "cancel_job",
            json!({ "job_id": sensitivity_job_id })
        )
        .await["cancellation_requested"],
        true
    );
    let mut sensitivity_status = Value::Null;
    for _ in 0..240 {
        sensitivity_status =
            call(&client, "get_job", json!({ "job_id": sensitivity_job_id })).await;
        if sensitivity_status["phase"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(matches!(
        sensitivity_status["phase"].as_str(),
        Some("cancelled" | "completed")
    ));
    client.cancel().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stdio_declared_multi_dataset_bindings_keep_exact_sources() {
    let root = temp_workspace("datasets");
    let client = launch(&root).await;
    let case_json = include_str!("../../../benchmarks/coupled/oc-022.json");
    let base_bundle = include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json");
    let tape_bundle = include_str!("../../../data/materials/robinson-shanghai-hflt-v3/bundle.json");
    let created = call(
        &client,
        "create_variant",
        json!({
            "name": "two binding stdio fixture",
            "case_json": case_json,
            "dataset_bundles": [base_bundle, tape_bundle],
            "threads": 1
        }),
    )
    .await;
    let variant_id = created["variant_id"]
        .as_str()
        .unwrap_or_else(|| panic!("create response: {created}"));
    let preflight = call(
        &client,
        "preflight_variant",
        json!({ "variant_id": variant_id }),
    )
    .await;
    assert_eq!(preflight["ready_to_run"], true, "{preflight}");
    let datasets = preflight["datasets"].as_array().unwrap();
    assert_eq!(datasets.len(), 2);
    assert!(
        datasets
            .iter()
            .any(|d| d["dataset_id"] == "robinson-superpower-ap-v3")
    );
    assert!(
        datasets
            .iter()
            .any(|d| d["dataset_id"] == "robinson-shanghai-hflt-v3")
    );
    let mut missing_case: Value = serde_json::from_str(case_json).unwrap();
    missing_case["tape_specs"]["hts-shanghai"]["material"]["dataset_id"] =
        Value::String("undeclared-external-fixture".into());
    missing_case["tape_specs"]["hts-shanghai"]["material"]["csv_sha256"] =
        Value::String("0000000000000000000000000000000000000000000000000000000000000000".into());
    let missing = call(
        &client,
        "create_variant",
        json!({
            "name": "missing second binding",
            "case_json": serde_json::to_string(&missing_case).unwrap(),
            "dataset_bundles": [base_bundle],
            "threads": 1
        }),
    )
    .await;
    let missing_id = missing["variant_id"].as_str().unwrap();
    let blocked = call(
        &client,
        "preflight_variant",
        json!({ "variant_id": missing_id }),
    )
    .await;
    assert_eq!(blocked["ready_to_run"], false, "{blocked}");
    let source = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(format!(
            "optcoil://study/variant/{variant_id}/case"
        )))
        .await
        .unwrap();
    assert!(!source.contents.is_empty());
    client.cancel().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stdio_robustness_preview_jobs_resources_and_history() {
    let root = temp_workspace("robustness");
    let client = launch(&root).await;
    let tools = client.list_all_tools().await.unwrap();
    for name in [
        "preview_robustness",
        "start_robustness",
        "list_robustness_results",
    ] {
        assert!(
            tools.iter().any(|tool| tool.name == name),
            "missing MCP tool {name}"
        );
    }

    let case_json = small_case();
    let variant_name = "stdio robustness fixture";
    let created = call(
        &client,
        "create_variant",
        json!({
            "name": variant_name,
            "case_json": case_json,
            "dataset_bundles": [],
            "threads": 1
        }),
    )
    .await;
    let variant_id = created["variant_id"].as_str().unwrap().to_owned();
    let spec_value = robustness_spec();
    let spec_json = spec_value.to_string();
    let preview = call(
        &client,
        "preview_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": spec_json }),
    )
    .await;
    assert_eq!(
        preview["schema"], "optcoil-robustness-preflight/v1",
        "{preview}"
    );
    assert_eq!(preview["ready_to_run"], true, "{preview}");
    assert_eq!(preview["run_count"], 2, "{preview}");
    assert_eq!(preview["engineering_acceptance_claim"], false, "{preview}");
    assert_eq!(preview["scenario_ids"][0], "nominal");
    assert_eq!(preview["scenario_ids"][1], "bounded-what-if");

    let malformed = call(
        &client,
        "preview_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": "not json" }),
    )
    .await;
    assert!(malformed["text"].as_str().is_some(), "{malformed}");
    let mut missing_nominal = spec_value.clone();
    missing_nominal["scenarios"] = json!([missing_nominal["scenarios"][1].clone()]);
    let missing_nominal = call(
        &client,
        "preview_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": missing_nominal.to_string() }),
    )
    .await;
    assert!(
        missing_nominal["text"].as_str().is_some(),
        "{missing_nominal}"
    );

    let mut too_many = spec_value.clone();
    let scenarios = too_many["scenarios"].as_array_mut().unwrap();
    for index in 0..15 {
        scenarios.push(json!({
            "id": format!("bounded-{index}"),
            "name": format!("Bounded scenario {index}"),
            "price_multipliers": {},
            "ic_multipliers": {},
            "temperature_offset_k": 0.0
        }));
    }
    let over_limit = call(
        &client,
        "preview_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": too_many.to_string() }),
    )
    .await;
    assert!(over_limit["text"].as_str().is_some(), "{over_limit}");

    let spec: RobustnessSpec = serde_json::from_value(spec_value).unwrap();
    let mut control = StudyWorkspace::new("Converra engineering study");
    let control_variant_id = control
        .add_variant(
            variant_name,
            case_json.clone(),
            Vec::new(),
            CoupledSearchOptions { threads: Some(1) },
        )
        .unwrap();
    assert_eq!(control_variant_id, variant_id);
    let expected = run_study_robustness(
        &control,
        std::slice::from_ref(&control_variant_id),
        &spec,
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    let expected = serde_json::to_value(expected).unwrap();

    // Request cancellation immediately after dispatch. If the worker still
    // reaches completion first, its completed evidence is used below; a
    // cancelled worker must leave the retained-results list untouched.
    let cancel_attempt = call(
        &client,
        "start_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": serde_json::to_string(&spec).unwrap() }),
    )
    .await;
    assert_eq!(cancel_attempt["kind"], "robustness", "{cancel_attempt}");
    let cancel_job_id = cancel_attempt["job_id"].as_str().unwrap().to_owned();
    let first_phase = call(&client, "get_job", json!({ "job_id": cancel_job_id })).await;
    let mut completed_job = if first_phase["phase"] == "completed" {
        Some(first_phase)
    } else {
        assert_eq!(first_phase["phase"], "running", "{first_phase}");
        let cancel_result = call(&client, "cancel_job", json!({ "job_id": cancel_job_id })).await;
        if cancel_result["cancellation_requested"] == true {
            let status = wait_for_job(&client, &cancel_job_id).await;
            match status["phase"].as_str() {
                Some("cancelled") => {
                    let retained = call(&client, "list_robustness_results", json!({})).await;
                    assert_eq!(
                        retained.as_array().unwrap().len(),
                        0,
                        "cancelled job attached partial evidence"
                    );
                    None
                }
                Some("completed") => Some(status),
                other => panic!("unexpected cancellation terminal phase {other:?}: {status}"),
            }
        } else {
            let status = wait_for_job(&client, &cancel_job_id).await;
            assert_eq!(status["phase"], "completed", "{cancel_result}; {status}");
            Some(status)
        }
    };
    if completed_job.is_none() {
        let started = call(
            &client,
            "start_robustness",
            json!({ "variant_ids": [variant_id], "spec_json": serde_json::to_string(&spec).unwrap() }),
        )
        .await;
        let job_id = started["job_id"].as_str().unwrap();
        let status = wait_for_job(&client, job_id).await;
        assert_eq!(status["phase"], "completed", "{status}");
        completed_job = Some(status);
    }
    let completed_job = completed_job.unwrap();
    assert_eq!(completed_job["phase"], "completed", "{completed_job}");
    assert!(
        completed_job["result_resource"].as_str().is_some(),
        "{completed_job}"
    );

    let summaries = call(&client, "list_robustness_results", json!({})).await;
    assert_eq!(summaries.as_array().unwrap().len(), 1, "{summaries}");
    let summary = &summaries[0];
    assert_eq!(summary["current"], true, "{summary}");
    let artifact_sha256 = summary["artifact_sha256"].as_str().unwrap();
    let resource_uri = format!("optcoil://study/robustness/{artifact_sha256}");
    assert_eq!(summary["resource"], resource_uri);
    assert_eq!(completed_job["result_resource"], resource_uri);
    let resources = client.list_all_resources().await.unwrap();
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == resource_uri)
    );
    let resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            resource_uri.clone(),
        ))
        .await
        .unwrap();
    let actual_text = match &resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("robustness resource was not text JSON"),
    };
    let actual: Value = serde_json::from_str(&actual_text).unwrap();
    assert_eq!(
        optcoil_model::attestation::sha256_hex(actual_text.as_bytes()),
        artifact_sha256
    );
    assert_eq!(actual["input_fingerprint"], expected["input_fingerprint"]);
    assert_eq!(
        actual["source_fingerprints"],
        expected["source_fingerprints"]
    );
    assert_eq!(actual["rows"][0]["source_case_json"], case_json);
    assert_eq!(actual["rows"][1]["source_case_json"], case_json);
    assert_eq!(actual["rows"][0]["source_options"]["threads"], 1);
    assert_eq!(
        robustness_semantics(&actual),
        robustness_semantics(&expected),
        "MCP job result differs from direct run_study_robustness on the exact same workspace and spec"
    );
    assert_eq!(
        actual["all_scenarios_have_supported_winner"],
        expected["all_scenarios_have_supported_winner"]
    );

    // Identical inputs can produce distinct timing metadata. A completed job's
    // resource must continue returning its exact artifact after another rerun.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let repeated = call(
        &client,
        "start_robustness",
        json!({ "variant_ids": [variant_id], "spec_json": serde_json::to_string(&spec).unwrap() }),
    )
    .await;
    let repeated_job = wait_for_job(&client, repeated["job_id"].as_str().unwrap()).await;
    assert_eq!(repeated_job["phase"], "completed", "{repeated_job}");
    assert_ne!(repeated_job["result_resource"], resource_uri);
    let retained = call(&client, "list_robustness_results", json!({})).await;
    assert_eq!(retained.as_array().unwrap().len(), 2, "{retained}");
    assert_eq!(
        retained[0]["input_fingerprint"],
        retained[1]["input_fingerprint"]
    );
    let original_resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            resource_uri.clone(),
        ))
        .await
        .unwrap();
    match &original_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, &actual_text)
        }
        _ => panic!("original robustness resource was not text JSON"),
    }

    let mut revised_case: Value = serde_json::from_str(&case_json).unwrap();
    let old_price = revised_case["cost"]["price_usd_per_m"].as_f64().unwrap();
    revised_case["cost"]["price_usd_per_m"] = json!(old_price * 1.01);
    let revised = call(
        &client,
        "revise_variant",
        json!({
            "variant_id": variant_id,
            "case_json": serde_json::to_string(&revised_case).unwrap(),
            "dataset_bundles": [],
            "threads": 1
        }),
    )
    .await;
    assert_eq!(revised["variant_id"], variant_id, "{revised}");
    let historical = call(&client, "list_robustness_results", json!({})).await;
    assert_eq!(
        historical.as_array().unwrap().len(),
        2,
        "revision removed scenario evidence"
    );
    assert_eq!(
        historical[0]["current"], false,
        "revision did not mark prior evidence historical"
    );
    assert_eq!(historical[1]["current"], false);
    let historical_resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            resource_uri.clone(),
        ))
        .await
        .unwrap();
    let historical_text = match &historical_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("historical robustness resource was not text JSON"),
    };
    assert_eq!(historical_text, &actual_text);

    client.cancel().await.unwrap();
    let restarted = launch(&root).await;
    let after_restart = call(&restarted, "list_robustness_results", json!({})).await;
    assert_eq!(
        after_restart.as_array().unwrap().len(),
        2,
        "restart lost retained robustness evidence"
    );
    assert_eq!(
        after_restart[0]["current"], false,
        "restart lost historical binding state"
    );
    assert_eq!(after_restart[1]["current"], false);
    let restarted_resource = restarted
        .read_resource(rmcp::model::ReadResourceRequestParams::new(resource_uri))
        .await
        .unwrap();
    let restarted_text = match &restarted_resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        _ => panic!("restarted robustness resource was not text JSON"),
    };
    assert_eq!(restarted_text, &actual_text);
    restarted.cancel().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stdio_reel_tools_validate_and_rate() {
    let root = temp_workspace("reel");
    let client = launch(&root).await;
    let tools = client.list_all_tools().await.unwrap();
    for name in [
        "validate_reel_passport",
        "validate_reel_inventory",
        "rate_reel_inventory",
        "synthesize_reel_inventory",
    ] {
        assert!(
            tools.iter().any(|tool| tool.name == name),
            "missing MCP tool {name}"
        );
    }
    let passport = include_str!("../../../examples/reels/passport-illustrative.json");
    let inventory = include_str!("../../../examples/reels/inventory-illustrative.json");

    let validated = call(
        &client,
        "validate_reel_passport",
        json!({ "passport_json": passport }),
    )
    .await;
    assert_eq!(validated["reel_id"], "ILLUSTRATIVE-REEL-A");
    assert_eq!(validated["usable_length_m"], 45.0);
    let summary = call(
        &client,
        "validate_reel_inventory",
        json!({ "inventory_json": inventory }),
    )
    .await;
    assert_eq!(summary["summary"]["reel_count"], 3);

    let rated = call(
        &client,
        "rate_reel_inventory",
        json!({
            "inventory_json": inventory,
            "temperature_k": 25.0,
            "field_t": 2.0,
            "angle_deg": 0.0
        }),
    )
    .await;
    assert_eq!(rated["schema"], "optcoil-reel-rating/v1");
    assert_eq!(rated["status_counts"]["rated"], 2);
    assert_eq!(rated["reels"][0]["evidence_class"], "synthetic");

    let outside = call(
        &client,
        "rate_reel_inventory",
        json!({
            "inventory_json": inventory,
            "temperature_k": 25.0,
            "field_t": 12.0,
            "angle_deg": 0.0
        }),
    )
    .await;
    assert_eq!(outside["reels"][0]["status"], "outside_map_domain");

    let spec = include_str!("../../../examples/reels/synthetic-spec.json");
    let synthesized = call(
        &client,
        "synthesize_reel_inventory",
        json!({ "spec_json": spec }),
    )
    .await;
    let inventory_json = synthesized["inventory_json"].as_str().unwrap();
    let truth: Value = serde_json::from_str(synthesized["truth_json"].as_str().unwrap()).unwrap();
    assert_eq!(truth["reels"].as_array().unwrap().len(), 20);
    let rated_synthetic = call(
        &client,
        "rate_reel_inventory",
        json!({
            "inventory_json": inventory_json,
            "temperature_k": 25.0,
            "field_t": 2.0,
            "angle_deg": 0.0
        }),
    )
    .await;
    assert_eq!(rated_synthetic["status_counts"]["rated"], 20);
    let again = call(
        &client,
        "synthesize_reel_inventory",
        json!({ "spec_json": spec }),
    )
    .await;
    assert_eq!(again["inventory_json"], synthesized["inventory_json"]);

    let invalid = client
        .call_tool(
            CallToolRequestParams::new("validate_reel_passport".to_owned())
                .with_arguments(serde_json::from_value(json!({ "passport_json": "{}" })).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(invalid.is_error, Some(true));
    client.cancel().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}
