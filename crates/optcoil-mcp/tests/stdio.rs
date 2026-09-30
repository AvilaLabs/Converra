use std::{fs, path::PathBuf, time::Duration};

use optcoil_search::coupled_search::{CoupledSearchOptions, run_coupled_search_case};
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
