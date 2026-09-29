use super::*;
use std::sync::atomic::AtomicBool;

fn wait_for_idle(app: &mut Workbench, ctx: &egui::Context) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while app.worker.is_some() {
        app.poll(ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "background work did not finish before the test deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn measured_case_search_revision_and_review_workflow() {
    let ctx = egui::Context::default();
    let mut app = Workbench::new(&ctx).expect("headless workbench");
    let original_json = app.search_json.clone();
    let original_case = app.search_case.as_ref().expect("measured case").clone();
    assert_eq!(original_case.id, "first-study-measured-racetrack");
    assert!(
        original_case
            .provenance
            .contains("invented teaching prices, not quotes")
    );
    assert_eq!(original_case.cost.price_usd_per_m, 30.0);
    assert!(matches!(
        app.search_dataset
            .as_ref()
            .expect("embedded measured dataset")
            .dataset
            .metadata
            .data_class,
        optcoil_model::material::MaterialDataClass::Measured
    ));
    let preflight = app.preflight.as_ref().expect("preflight");
    assert!(preflight.ready_to_run, "{:?}", preflight.errors);
    assert!(!preflight.engineering_acceptance_claim);

    // Opening and cancelling the advanced editor leaves the loaded input
    // untouched; malformed revisions are rejected before the workbench is
    // given a replacement result.
    app.edit_case(false);
    assert!(app.author.is_some());
    app.author = None;
    assert_eq!(app.search_json, original_json);
    assert!(author::CaseDraft::revision("{ malformed json").is_err());
    assert_eq!(app.search_case.as_ref().unwrap().id, original_case.id);
    assert!(app.search_record.is_none());

    // This bounded fixture has two geometry candidates and uses measured
    // low-field data with explicitly identified teaching prices.
    let dataset = app.search_dataset.as_ref().unwrap().dataset.clone();
    let first_record = optcoil_search::coupled_search::run_coupled_search_case_with_dataset(
        &app.search_json,
        &CoupledSearchOptions { threads: Some(2) },
        Some(dataset),
        &AtomicBool::new(false),
    )
    .expect("small measured first study");
    let first_record_id = first_record.case_sha256.clone();
    app.apply_result(&ctx, Ok(JobResult::SearchCompleted(Box::new(first_record))));
    assert_eq!(
        app.search_record.as_ref().unwrap().case_sha256,
        first_record_id
    );
    assert!(
        app.bom_record.is_none(),
        "the measured fixture has no PASS optimum"
    );

    app.edit_case(false);
    assert!(app.author.is_some());
    app.author = None;
    assert_eq!(app.search_json, original_json);
    assert_eq!(
        app.search_record.as_ref().unwrap().case_sha256,
        first_record_id
    );
    assert!(author::CaseDraft::revision("{ malformed json").is_err());
    assert_eq!(app.search_case.as_ref().unwrap().id, original_case.id);
    assert_eq!(
        app.search_record.as_ref().unwrap().case_sha256,
        first_record_id
    );

    // A replacement run starts while the prior completed record remains
    // available. Cancel its worker after checking this transition, then
    // drain the cancellation result without using a GUI or file dialog.
    app.start(&ctx);
    assert!(
        app.worker.is_some(),
        "preflight should allow the replacement run"
    );
    assert_eq!(
        app.search_record.as_ref().unwrap().case_sha256,
        first_record_id
    );
    app.package_after_search = true;
    app.cancel_search();
    assert!(!app.package_after_search);
    wait_for_idle(&mut app, &ctx);
    assert!(app.search_record.is_some());

    // Exercise the successful replacement transition using a genuine record
    // generated above. The outgoing completed result must remain in the
    // comparison slot when the active slot receives its replacement.
    let replacement = app.search_record.as_ref().unwrap().clone();
    app.apply_result(&ctx, Ok(JobResult::SearchCompleted(Box::new(replacement))));
    assert_eq!(
        app.compare_record.as_ref().unwrap().case_sha256,
        first_record_id
    );

    app.verify_now();
    wait_for_idle(&mut app, &ctx);
    assert!(app.verify_checks.is_some(), "{}", app.message.1);
    let artifacts = app.review_artifacts().expect("review package artifacts");
    optcoil_search::review::verify_review_package(&artifacts)
        .expect("review package integrity and record verification");
    assert!(
        artifacts
            .iter()
            .any(|(path, _)| path == "procurement-unavailable.txt")
    );
    let mut tampered = artifacts.clone();
    let case_artifact = tampered
        .iter_mut()
        .find(|(path, _)| path == "case.json")
        .unwrap();
    case_artifact.1.push(' ');
    assert!(optcoil_search::review::verify_review_package(&tampered).is_err());
    assert!(artifacts.iter().any(|(path, contents)| {
        path == "case.json" && contents.contains("invented teaching prices, not quotes")
    }));

    let prior = app.search_record.as_ref().unwrap().clone();
    app.apply_result(&ctx, Ok(JobResult::CompareLoaded(Box::new(prior.clone()))));
    assert!(app.compare_record.is_some());

    // An explicitly selected signed bundle keeps its exact source bytes in
    // the portable package rather than a reserialization of its metadata.
    let signed_bundle_json =
        include_str!("../../../data/materials/robinson-superpower-ap-v3-lowfield/bundle.json");
    let signed_bundle = MaterialBundle::from_json(signed_bundle_json).unwrap();
    app.apply_result(
        &ctx,
        Ok(JobResult::DatasetLoaded(
            Box::new(signed_bundle),
            PathBuf::from("robinson-superpower-ap-v3-lowfield.bundle.json"),
            Some(signed_bundle_json.to_owned()),
        )),
    );
    let signed_artifacts = app.review_artifacts().expect("signed bundle package");
    assert!(
        signed_artifacts.iter().any(|(path, contents)| {
            path.starts_with("datasets/") && contents == signed_bundle_json
        }),
        "the exact signed bundle JSON must be retained byte-for-byte"
    );

    let mut revised_value: serde_json::Value = serde_json::from_str(&original_json).unwrap();
    revised_value["id"] =
        serde_json::Value::String("first-study-measured-racetrack-revised".into());
    revised_value["provenance"] = serde_json::Value::String(format!(
        "{}\nRevision accepted by the headless workflow regression.",
        original_case.provenance
    ));
    let revised_json = serde_json::to_string_pretty(&revised_value).unwrap();
    let accepted = load_project_json(revised_json.clone(), PathBuf::from("revised-case.json"))
        .expect("valid revised case");
    app.apply_result(&ctx, Ok(accepted));
    assert_eq!(
        app.search_case.as_ref().unwrap().id,
        "first-study-measured-racetrack-revised"
    );
    assert_eq!(app.search_json, revised_json);
    assert_eq!(
        app.search_dataset
            .as_ref()
            .unwrap()
            .dataset
            .metadata
            .csv_sha256,
        original_case.material.csv_sha256
    );
    assert_eq!(
        app.compare_record.as_ref().unwrap().case_sha256,
        prior.case_sha256
    );
    assert!(app.search_record.is_none());
    assert!(app.bom_record.is_none());
    assert!(app.verify_checks.is_none());
}

fn aliased_software_fixture_bundle(source_json: &str, alias: &str) -> MaterialBundle {
    let mut value: serde_json::Value = serde_json::from_str(source_json).unwrap();
    value["metadata"]["id"] = serde_json::Value::String(alias.to_owned());
    value["metadata"]["limitations"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::String(
            "Software fixture alias of the cited measured dataset; CSV bytes and attribution are unchanged; this id exists only to exercise external binding resolution.".into(),
        ));
    // Re-identifying the fixture invalidates any issuer signature. The
    // software fixture is deliberately unsigned.
    value.as_object_mut().unwrap().remove("attestation");
    MaterialBundle::from_json(&serde_json::to_string(&value).unwrap()).unwrap()
}

fn loaded_fixture_bundle(bundle: MaterialBundle, name: &str) -> JobResult {
    let raw_bundle = bundle.to_json().expect("fixture bundle serializes");
    JobResult::DatasetLoaded(Box::new(bundle), PathBuf::from(name), Some(raw_bundle))
}

#[test]
fn graded_field_map_workflow_keeps_all_external_spec_bundles() {
    let ctx = egui::Context::default();
    let mut app = Workbench::new(&ctx).expect("headless workbench");

    let mut case: serde_json::Value = serde_json::from_str(include_str!(
        "../../../benchmarks/coupled/oc-031-helix-layer.json"
    ))
    .unwrap();
    let oc020: serde_json::Value =
        serde_json::from_str(include_str!("../../../benchmarks/coupled/oc-020.json")).unwrap();
    let base = aliased_software_fixture_bundle(
        include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json"),
        "software-fixture-fieldmap-base",
    );
    let lowfield = aliased_software_fixture_bundle(
        include_str!("../../../data/materials/robinson-superpower-ap-v3-lowfield/bundle.json"),
        "software-fixture-fieldmap-spec",
    );
    let base_id = base.dataset.metadata.id.clone();
    let spec_id = lowfield.dataset.metadata.id.clone();
    let spec_hash = lowfield.dataset.metadata.csv_sha256.clone();

    case["id"] = "software-fixture-graded-fieldmap".into();
    case["provenance"] = "Software workflow fixture based on OC-031 geometry and its declared field map. Material CSV rows retain source attribution; alias metadata exists only to exercise external dataset binding.".into();
    case["material"]["dataset_id"] = base_id.clone().into();
    case["material"]["csv_sha256"] = base.dataset.metadata.csv_sha256.clone().into();
    case["tape_specs"] = oc020["tape_specs"].clone();
    case["tape_specs"]["hts-lowfield"]["material"]["dataset_id"] = spec_id.clone().into();
    case["tape_specs"]["hts-lowfield"]["material"]["csv_sha256"] = spec_hash.clone().into();
    case["grading"] = serde_json::json!({
        "regions": [{
            "turn_range": [0.0, 1.0],
            "tape_spec_choices": ["base", "hts-lowfield"]
        }]
    });
    case["baseline"]["tape_spec_ids"] = serde_json::json!(["base"]);
    let case_json = serde_json::to_string_pretty(&case).unwrap();
    let loaded = load_project_json(
        case_json.clone(),
        PathBuf::from("software-fixture-case.json"),
    )
    .expect("valid field-map fixture");
    app.apply_result(&ctx, Ok(loaded));
    app.refresh_preflight();
    assert!(
        !app.preflight.as_ref().unwrap().ready_to_run,
        "external spec dependencies must be unresolved before loading their bundles"
    );
    assert_eq!(app.unresolved_dataset_bindings().len(), 2);
    app.start(&ctx);
    assert!(
        app.worker.is_none(),
        "missing dependencies must gate search"
    );
    assert!(app.message.0, "missing dependencies should be explained");

    // The first exact dependency resolves only its own binding. A
    // mismatched bundle is rejected and cannot replace a bound source.
    app.apply_result(
        &ctx,
        Ok(loaded_fixture_bundle(
            base,
            "software-fixture-base.bundle.json",
        )),
    );
    app.refresh_preflight();
    assert_eq!(app.unresolved_dataset_bindings().len(), 1);
    let previous = app
        .search_dataset
        .as_ref()
        .unwrap()
        .dataset
        .metadata
        .id
        .clone();
    let wrong = MaterialBundle::from_json(include_str!(
        "../../../data/materials/robinson-theva-ap-v2/bundle.json"
    ))
    .unwrap();
    app.apply_result(&ctx, Ok(loaded_fixture_bundle(wrong, "wrong.bundle.json")));
    assert_eq!(
        app.search_dataset.as_ref().unwrap().dataset.metadata.id,
        previous,
        "mismatched dataset must not replace the accepted binding"
    );
    assert!(app.message.0);

    // A validly shaped case with a modified declared CSV hash is a
    // tampered binding. The exact source bundle must no longer bind to it.
    let mut tampered_case = case.clone();
    tampered_case["material"]["csv_sha256"] = "0".repeat(64).into();
    let tampered_json = serde_json::to_string_pretty(&tampered_case).unwrap();
    let tampered = load_project_json(
        tampered_json,
        PathBuf::from("software-fixture-case-tampered.json"),
    )
    .expect("tampered lowercase hash remains syntactically valid");
    app.apply_result(&ctx, Ok(tampered));
    assert!(app.search_dataset.is_none());
    app.apply_result(
        &ctx,
        Ok(loaded_fixture_bundle(
            aliased_software_fixture_bundle(
                include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json"),
                "software-fixture-fieldmap-base",
            ),
            "software-fixture-base.bundle.json",
        )),
    );
    assert!(
        app.message.0,
        "tampered hash must reject the exact source bundle"
    );
    assert!(app.search_dataset.is_none());

    let restored = load_project_json(
        case_json.clone(),
        PathBuf::from("software-fixture-case.json"),
    )
    .expect("original case bytes remain valid");
    app.apply_result(&ctx, Ok(restored));
    app.apply_result(
        &ctx,
        Ok(loaded_fixture_bundle(
            aliased_software_fixture_bundle(
                include_str!("../../../data/materials/robinson-superpower-ap-v3/bundle.json"),
                "software-fixture-fieldmap-base",
            ),
            "software-fixture-base.bundle.json",
        )),
    );
    assert!(!app.message.0);

    app.apply_result(
        &ctx,
        Ok(loaded_fixture_bundle(
            lowfield,
            "software-fixture-spec.bundle.json",
        )),
    );
    app.refresh_preflight();
    assert!(
        app.preflight.as_ref().unwrap().ready_to_run,
        "{}",
        app.message.1
    );
    assert!(app.unresolved_dataset_bindings().is_empty());

    app.start(&ctx);
    wait_for_idle(&mut app, &ctx);
    assert!(app.search_record.is_some(), "{}", app.message.1);
    let artifacts = app.review_artifacts().expect("multi-dataset package");
    let dataset_files = artifacts
        .iter()
        .filter(|(path, _)| path.starts_with("datasets/") && path.ends_with(".json"))
        .count();
    assert_eq!(dataset_files, 2, "one original bundle per external binding");
    optcoil_search::review::verify_review_package(&artifacts)
        .expect("multi-dataset review package verifies");
    app.verify_now();
    wait_for_idle(&mut app, &ctx);
    let checks = app.verify_checks.as_ref().unwrap();
    let bindings = checks
        .iter()
        .filter(|check| check.name == "dataset bundle binding")
        .collect::<Vec<_>>();
    assert_eq!(bindings.len(), dataset_files);
    assert!(
        bindings
            .iter()
            .all(|check| check.outcome == optcoil_search::verify::Outcome::Pass),
        "each external bundle must bind a declared base/spec dependency: {bindings:?}"
    );
    assert!(
        !checks.iter().any(|check| {
            check.name == "dataset coverage"
                && check.outcome != optcoil_search::verify::Outcome::Pass
        }),
        "all declared datasets should be resolved by the retained bundles"
    );

    // Attaching the exact original bytes must keep both external sources,
    // and a valid revision with unchanged bindings must retain them too.
    app.apply_result(
        &ctx,
        Ok(JobResult::SourceCaseLoaded(
            case_json,
            PathBuf::from("software-fixture-case.json"),
        )),
    );
    assert_eq!(app.search_spec_datasets.len(), 1);
    assert!(
        app.search_dataset.is_some(),
        "original case attach preserves the external base bundle"
    );
    case["id"] = "software-fixture-graded-fieldmap-revision".into();
    case["provenance"] =
        "Software fixture revision; original measured CSV attributions retained.".into();
    let revised_json = serde_json::to_string_pretty(&case).unwrap();
    let revised = load_project_json(
        revised_json,
        PathBuf::from("software-fixture-case-revised.json"),
    )
    .expect("valid revised field-map fixture");
    app.apply_result(&ctx, Ok(revised));
    assert!(app.search_dataset.is_some());
    assert_eq!(app.search_spec_datasets.len(), 1);
}
