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
