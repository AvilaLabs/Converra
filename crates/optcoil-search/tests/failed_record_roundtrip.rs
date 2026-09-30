use optcoil_model::Status;
use optcoil_search::{
    coupled_search::{CoupledSearchOptions, CoupledSearchRunRecord, run_coupled_search_case},
    study::{StudyEngineSession, StudyWorkspace},
    verify::{Outcome, verify_record_checks},
};
use serde_json::{Value, json};

#[test]
fn failed_geometry_remains_saveable_but_null_cannot_replace_evaluated_current() {
    let mut case: Value =
        serde_json::from_str(include_str!("../../../benchmarks/coupled/first-study.json")).unwrap();
    case["id"] = json!("software-regression-mixed-invalid-pack");
    case["provenance"] = json!(
        "Software fixture: one evaluable and one impossible winding; no engineering validation claim."
    );
    case["choices"]["turns_along_normal"] = json!([1, 8000]);
    case["baseline"]["turns_along_normal"] = json!(1);
    let source = serde_json::to_string(&case).unwrap();
    let options = CoupledSearchOptions { threads: Some(1) };
    let record = run_coupled_search_case(&source, &options).unwrap();
    assert_eq!(record.candidates.len(), 2);
    assert!(record.candidates[0].operating_current_a.is_finite());
    assert!(!record.candidates[1].operating_current_a.is_finite());
    assert_eq!(record.candidates[1].status, Status::Fail);
    let encoded = serde_json::to_string(&record).unwrap();
    let decoded: CoupledSearchRunRecord = serde_json::from_str(&encoded).unwrap();
    assert!(!decoded.candidates[1].operating_current_a.is_finite());
    assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    let checks = verify_record_checks(&encoded, Some(&source), None, None, &[]).unwrap();
    assert!(checks.iter().all(|check| check.outcome != Outcome::Fail));
    assert!(checks.iter().any(|check| {
        check.name == "candidate.quantity_availability" && check.outcome == Outcome::Pass
    }));

    let mut workspace = StudyWorkspace::new("Failed alternatives stay inspectable");
    let id = workspace
        .add_variant("Mixed geometry", source, vec![], options)
        .unwrap();
    workspace.attach_result(&id, encoded.clone()).unwrap();
    let summary = workspace.compact_summary(&id).unwrap();
    assert_eq!(summary.latest_search_status, Some(record.search_status));
    assert!(summary.diagnosis.issues.iter().any(|issue| {
        issue.code == "candidate_status"
            && issue.candidate_index == Some(1)
            && issue.status == Status::Fail
    }));
    let persisted = workspace.to_json().unwrap();
    let reopened = StudyWorkspace::from_json(&persisted).unwrap();
    assert_eq!(reopened.variant(&id).unwrap().results.len(), 1);
    assert!(
        StudyEngineSession::default()
            .cached_result(reopened.variant(&id).unwrap())
            .unwrap()
            .is_none()
    );

    let mut tampered: Value = serde_json::from_str(&encoded).unwrap();
    tampered["candidates"][0]["operating_current_a"] = Value::Null;
    let tampered = serde_json::to_string(&tampered).unwrap();
    let checks = verify_record_checks(&tampered, None, None, None, &[]).unwrap();
    assert!(checks.iter().any(|check| {
        check.name == "candidate.quantity_availability" && check.outcome == Outcome::Fail
    }));
    assert!(workspace.attach_result(&id, tampered).is_err());
}
