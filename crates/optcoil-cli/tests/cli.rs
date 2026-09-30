use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_optcoil"))
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "optcoil-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// A minimal, fast-to-evaluate coupled case: one station, one tape index,
/// two turn indices, one current candidate and coarse quadrature orders.
/// Uses the real embedded material identity so it passes dataset validation,
/// but its tiny sampling grid runs in a fraction of a second, unlike the
/// full frozen OC-004 benchmark (900 sampled points x 3 candidates).
fn small_custom_case_json() -> &'static str {
    r#"{
  "schema": "optcoil-coupled-conductor/v1",
  "id": "cli-test-small-custom-case",
  "provenance": "CLI integration test fixture; not a frozen benchmark.",
  "geometry_data_class": "synthetic",
  "pack": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_width_m": 0.02,
    "axial_height_m": 0.036
  },
  "winding": {
    "tape_width_m": 0.012,
    "tape_normal": "radial",
    "turns_along_normal": 5,
    "tapes_along_width": 3
  },
  "operating": {
    "temperature_k": 21.0,
    "current_candidates_a": [870.0],
    "electric_field_criterion_v_per_m": 0.0001
  },
  "material": {
    "dataset_id": "robinson-superpower-ap-v3",
    "csv_sha256": "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
    "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
    "angle_mapping": "period_180_field_reversal",
    "mirror_policy": "minimum_of_mirror_pair",
    "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
    "field_magnitude_policy": "total_magnitude_with_transverse_angle",
    "low_field_policy": "monotone_field_lower_bound",
    "low_field_clamp_t": 1.001,
    "monotonicity_tolerance": 0.001
  },
  "sampling": {
    "stations": [
      {"id": "s0", "kind": "straight", "x_m": 0.0}
    ],
    "turn_indices_along_normal": [1, 2],
    "tape_indices_along_width": [1],
    "width_quadrature": "gauss_lobatto",
    "width_points": 5
  },
  "limits": {
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 0.5,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  },
  "numerics": {
    "quadrature_orders": [2, 4],
    "field_scale_t": 1.0,
    "max_refinement_change_fraction": 0.5,
    "max_reference_refinement_fraction": 0.5,
    "max_reference_field_error_fraction": 0.5,
    "max_reference_angle_error_deg": 90.0,
    "max_reference_capacity_relative_error": 0.5,
    "reference_subset": [
      {"station": "s0", "tape_index": 1, "turn_index": 1}
    ]
  },
  "width_transfer": {
    "basis": "none"
  }
}
"#
}

#[test]
fn demo_json_and_exclusive_record_creation_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!(
        "optcoil-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let output_path = dir.join("run.json");
    let output = cli()
        .args(["demo", "--json", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["best"]["cost"]["total_usd"], 824.0);
    assert_eq!(stdout["best"]["engineering_status"], "NOT_EVALUATED");
    let bytes = fs::read(&output_path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stdout, saved);
    let overwrite = cli()
        .args(["demo", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(bytes, fs::read(&output_path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_budget_and_missing_case_fail_cleanly() {
    assert!(
        !cli()
            .args(["demo", "--max-evaluations", "0"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = cli()
        .args(["run", "/optcoil-deliberately-missing-case.json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OptCoil:"));
}

#[test]
fn field_benchmark_reports_reference_checks_and_protects_saved_evidence() {
    let dir = std::env::temp_dir().join(format!(
        "optcoil-field-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("field.json");
    let output = cli()
        .args(["field-benchmark", "--json", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["schema"], "optcoil-field-run/v1");
    assert_eq!(record["numerical_status"], "PASS");
    assert_eq!(record["engineering_status"], "NOT_EVALUATED");
    assert_eq!(record["results"].as_array().unwrap().len(), 14);
    assert_eq!(record["options"]["orders"], serde_json::json!([6, 10, 14]));
    assert!(record["max_reference_error_fraction"].as_f64().unwrap() <= 0.01);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        record,
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    );
    let overwrite = cli()
        .args(["field-benchmark", "--orders", "2,3,4", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(bytes, fs::read(&path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn field_cli_preserves_diagnostics_but_never_passes_unresolved_validation() {
    let output = cli()
        .args(["field-benchmark", "--orders", "2,3,4", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(record["numerical_status"], "PASS");
    assert_eq!(record["engineering_status"], "NOT_EVALUATED");
    assert!(String::from_utf8_lossy(&output.stderr).contains("field validation"));
    let output = cli()
        .args(["field-benchmark", "--orders", "4,4,6"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("increasing"));
}

#[test]
fn measured_material_suite_preserves_failed_baseline_and_passed_reserved_test() {
    let dir = std::env::temp_dir().join(format!(
        "optcoil-material-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("material.json");
    let output = cli()
        .args(["material-benchmark", "--json", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["linear_baseline"]["interpolation_validation_status"],
        "FAIL"
    );
    assert_eq!(
        record["logarithmic_development"]["interpolation_validation_status"],
        "PASS"
    );
    assert_eq!(
        record["reserved_challenge"]["interpolation_validation_status"],
        "PASS"
    );
    assert_eq!(record["engineering_status"], "NOT_EVALUATED");
    let bytes = fs::read(&path).unwrap();
    assert_eq!(
        record,
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    );
    assert!(
        !cli()
            .args(["material-benchmark", "--output"])
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(bytes, fs::read(&path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn material_queries_reject_missing_coverage_and_criterion_changes_without_fabricating_current() {
    let args = [
        "material-query",
        "--temperature-k",
        "30",
        "--applied-field-t",
        "5",
        "--angle-from-normal-deg",
        "90",
        "--json",
    ];
    let output = cli().args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["domain_status"], "PASS");
    assert_eq!(record["dataset"]["metadata"]["data_class"], "measured");
    assert!(record["estimate"]["ic_a_per_m"].as_f64().unwrap() > 0.0);
    assert_eq!(record["engineering_status"], "NOT_EVALUATED");
    let output = cli()
        .args(args)
        .args(["--electric-field-criterion-v-per-m", "0.00001"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["criterion_status"], "INCONCLUSIVE");
    assert!(record["estimate"].is_null());
    let output = cli()
        .args([
            "material-query",
            "--temperature-k",
            "30",
            "--applied-field-t",
            "20",
            "--angle-from-normal-deg",
            "90",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["domain_status"], "INCONCLUSIVE");
    assert!(record["estimate"].is_null());
}

#[test]
fn material_query_dataset_flag_selects_the_lowfield_dataset_and_the_default_stays_original() {
    let base_args = [
        "material-query",
        "--temperature-k",
        "30",
        "--applied-field-t",
        "0.1",
        "--angle-from-normal-deg",
        "90",
        "--json",
    ];

    // No --dataset: defaults to the original characterization region, which
    // does not cover 0.1 T -- INCONCLUSIVE, not a fabricated estimate.
    let output = cli().args(base_args).output().unwrap();
    assert!(!output.status.success());
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["dataset"]["metadata"]["id"],
        "robinson-superpower-ap-v3"
    );
    assert_eq!(record["domain_status"], "INCONCLUSIVE");

    // --dataset selects the low-field extension explicitly, which covers it.
    let output = cli()
        .args(base_args)
        .args(["--dataset", "robinson-superpower-ap-v3-lowfield"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["dataset"]["metadata"]["id"],
        "robinson-superpower-ap-v3-lowfield"
    );
    assert_eq!(record["domain_status"], "PASS");
    assert!(record["estimate"]["ic_a_per_m"].as_f64().unwrap() > 0.0);

    // An unknown dataset id is rejected explicitly, not silently ignored.
    let output = cli()
        .args(base_args)
        .args(["--dataset", "not-a-real-dataset"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown embedded material dataset id")
    );

    // --dataset conflicts with an explicit --metadata/--csv file pair.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/materials/robinson-superpower-ap-v3");
    let output = cli()
        .args(base_args)
        .args([
            "--dataset",
            "robinson-superpower-ap-v3-lowfield",
            "--metadata",
        ])
        .arg(root.join("material.json"))
        .arg("--csv")
        .arg(root.join("measurements.csv"))
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn material_validate_dataset_flag_defaults_to_the_benchmark_and_rejects_a_mismatch() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/measured");

    // No --dataset: material-validate resolves the embedded dataset named by
    // the benchmark file's own dataset_id (here, the original).
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .arg("--json")
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["dataset"]["metadata"]["id"],
        "robinson-superpower-ap-v3"
    );

    // --dataset agreeing with the benchmark's own dataset_id is accepted.
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .args(["--dataset", "robinson-superpower-ap-v3", "--json"])
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["dataset"]["metadata"]["id"],
        "robinson-superpower-ap-v3"
    );

    // --dataset naming a different (even if real) dataset than the
    // benchmark's own declared dataset_id is rejected explicitly, before any
    // validation runs -- never a silent dataset switch.
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .args(["--dataset", "robinson-superpower-ap-v3-lowfield"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not match") && stderr.contains("dataset_id"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn material_validate_dataset_flag_defaults_to_the_lowfield_benchmark_and_rejects_a_mismatch() {
    // oc-005.json's own dataset_id is the original dataset, so it cannot
    // distinguish default-resolution logic from a hypothetically fixed
    // embedded() call. oc-006.json's own dataset_id is the low-field
    // extension, so this exercises the actual new resolution path.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/measured");

    // No --dataset: material-validate resolves the embedded dataset named by
    // oc-006.json's own declared dataset_id, the low-field extension.
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-006.json"))
        .arg("--json")
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        record["dataset"]["metadata"]["id"],
        "robinson-superpower-ap-v3-lowfield"
    );
    assert!(record["interpolation_validation_status"].is_string());

    // --dataset naming the original dataset -- real, but not oc-006.json's
    // own declared dataset_id -- is rejected explicitly, before any
    // validation runs, never a silent switch to the original dataset.
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-006.json"))
        .args(["--dataset", "robinson-superpower-ap-v3"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not match") && stderr.contains("dataset_id"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn material_csv_and_metadata_import_are_available_through_cli() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/materials/robinson-superpower-ap-v3");
    let output = cli()
        .arg("material-inspect")
        .arg(root.join("material.json"))
        .arg(root.join("measurements.csv"))
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["points"].as_array().unwrap().len(), 1505);
    let output = cli()
        .args([
            "material-query",
            "--temperature-k",
            "30",
            "--applied-field-t",
            "5",
            "--angle-from-normal-deg",
            "90",
            "--metadata",
        ])
        .arg(root.join("material.json"))
        .arg("--csv")
        .arg(root.join("measurements.csv"))
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn material_validate_on_oc005_emits_v2_schema_and_strata_with_a_status_consistent_exit_code() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/measured");
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .arg("--json")
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["schema"], "optcoil-material-validation/v3");
    assert_eq!(
        record["checker_id"],
        "withheld-nominal-planes-measured-coordinate-validation/v2"
    );
    let folds = record["folds"].as_array().unwrap();
    assert_eq!(folds.len(), 1);
    let fold = &folds[0];
    assert_eq!(fold["fold"]["axis"], "multi");
    // Counts pinned by the search-crate unit test, re-checked end to end
    // through the CLI's JSON output.
    assert_eq!(fold["training_source_rows"].as_array().unwrap().len(), 264);
    assert_eq!(fold["points"].as_array().unwrap().len(), 575);
    assert_eq!(
        fold["unscored_boundary_source_rows"]
            .as_array()
            .unwrap()
            .len(),
        666
    );
    let strata = fold["strata"].as_array().unwrap();
    assert_eq!(strata.len(), 3);
    let by_axes: std::collections::BTreeMap<u64, u64> = strata
        .iter()
        .map(|s| {
            (
                s["off_grid_axes"].as_u64().unwrap(),
                s["points"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        by_axes,
        std::collections::BTreeMap::from([(1, 182), (2, 267), (3, 126)])
    );
    let allowed_statuses = ["PASS", "FAIL", "INCONCLUSIVE"];
    assert!(
        allowed_statuses.contains(&record["interpolation_validation_status"].as_str().unwrap())
    );
    let is_pass = record["interpolation_validation_status"] == "PASS";
    assert_eq!(output.status.success(), is_pass);
}

#[test]
fn material_validate_output_is_exclusive_and_matches_v1_and_v2_files() {
    let dir = temp_dir("material-validate-cli");
    fs::create_dir_all(&dir).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/measured");

    // An existing v1 (single-axis) benchmark file runs unchanged through the
    // new generic subcommand: schema v1 still parses and validates.
    let v1_path = dir.join("out-v1.json");
    let output = cli()
        .args(["material-validate"])
        .arg(root.join("oc-003-log.json"))
        .args(["--json", "--output"])
        .arg(&v1_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["schema"], "optcoil-material-validation/v3");
    assert!(
        record["folds"].as_array().unwrap()[0]["strata"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let saved = fs::read(&v1_path).unwrap();
    assert_eq!(
        record,
        serde_json::from_slice::<serde_json::Value>(&saved).unwrap()
    );

    // --output is exclusive creation: a second run to the same path fails
    // and leaves the first saved report untouched.
    let overwrite = cli()
        .args(["material-validate"])
        .arg(root.join("oc-003-log.json"))
        .arg("--output")
        .arg(&v1_path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(saved, fs::read(&v1_path).unwrap());

    // The v2 oc-005.json file behaves the same way for --output exclusivity.
    // The record is saved before the PASS/FAIL/INCONCLUSIVE exit check runs
    // (matching finish_field/finish_coupled's pattern), so the file exists
    // regardless of the run's own exit code.
    let v2_path = dir.join("out-v2.json");
    cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .arg("--output")
        .arg(&v2_path)
        .output()
        .unwrap();
    let saved_v2 = fs::read(&v2_path).unwrap();
    let overwrite_v2 = cli()
        .args(["material-validate"])
        .arg(root.join("oc-005.json"))
        .arg("--output")
        .arg(&v2_path)
        .output()
        .unwrap();
    assert!(!overwrite_v2.status.success());
    assert_eq!(saved_v2, fs::read(&v2_path).unwrap());

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn coupled_benchmark_json_has_the_expected_schema_and_statuses() {
    // The full frozen OC-004 grid (900 sampled points x 3 candidates) at
    // deliberately low quadrature orders so the debug-profile test stays fast.
    // Low orders exercise every code path but may leave the numerical status
    // unresolved; the frozen orders and actual statuses are established by the
    // release `coupled-benchmark` command, whose record docs/OC004.md cites.
    let output = cli()
        .args(["coupled-benchmark", "--json", "--orders", "4,6"])
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["schema"], "optcoil-coupled-run/v8");
    assert_eq!(
        record["coupled_model_id"],
        "coupled-racetrack-pack-bridge-law-screening/v7"
    );
    assert_eq!(record["conductor_qualification_status"], "INCONCLUSIVE");
    assert_eq!(record["engineering_status"], "NOT_EVALUATED");
    let candidates = record["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    let allowed_statuses = ["PASS", "FAIL", "INCONCLUSIVE"];
    for candidate in candidates {
        assert!(allowed_statuses.contains(&candidate["status"].as_str().unwrap()));
    }
    assert!(allowed_statuses.contains(&record["numerical_status"].as_str().unwrap()));
    assert!(allowed_statuses.contains(&record["coverage_status"].as_str().unwrap()));
    // Exit status follows the OC-002/OC-003 pattern: nonzero unless numerical
    // and coverage both PASS and every candidate PASSes.
    let all_pass = record["numerical_status"] == "PASS"
        && record["coverage_status"] == "PASS"
        && candidates.iter().all(|c| c["status"] == "PASS");
    assert_eq!(output.status.success(), all_pass);
}

#[test]
fn coupled_output_is_exclusive_and_protects_saved_evidence() {
    let dir = temp_dir("coupled-cli-output");
    let case_path = dir.join("case.json");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&case_path, small_custom_case_json()).unwrap();
    let output_path = dir.join("run.json");

    let output = cli()
        .args(["coupled"])
        .arg(&case_path)
        .args(["--json", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    // No reference is supplied, so numerical_status can never reach PASS and
    // the exit code is nonzero (contract A10) — but the record is still
    // computed and saved before that gate is checked (diagnostics survive
    // a non-PASS exit, matching finish_field/finish_material's pattern).
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["numerical_status"], "INCONCLUSIVE");
    let bytes = fs::read(&output_path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stdout, saved);

    let overwrite = cli()
        .args(["coupled"])
        .arg(&case_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(bytes, fs::read(&output_path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn custom_case_without_reference_is_inconclusive_with_nonzero_exit() {
    let dir = temp_dir("coupled-cli-no-reference");
    fs::create_dir_all(&dir).unwrap();
    let case_path = dir.join("case.json");
    fs::write(&case_path, small_custom_case_json()).unwrap();

    let output = cli()
        .args(["coupled"])
        .arg(&case_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let record: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(record["numerical_status"], "INCONCLUSIVE");
    assert!(record["reference"].is_null());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OptCoil:"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stale_reference_against_edited_case_bytes_is_rejected_explicitly() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/coupled");
    let mut case_bytes = fs::read(root.join("oc-004.json")).unwrap();
    // A single trailing byte changes the case's SHA-256 while leaving it
    // structurally identical JSON, simulating a stale reference generated
    // against an earlier (or differently formatted) version of the case.
    case_bytes.push(b'\n');

    let dir = temp_dir("coupled-cli-stale-reference");
    fs::create_dir_all(&dir).unwrap();
    let case_path = dir.join("edited-case.json");
    fs::write(&case_path, &case_bytes).unwrap();

    let output = cli()
        .args(["coupled"])
        .arg(&case_path)
        .arg("--reference")
        .arg(root.join("oc-004.reference.json"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("coupled reference") && stderr.contains("SHA-256"),
        "unexpected stderr: {stderr}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// A minimal, fast-to-evaluate OC-007-shaped coupled search case: two
/// geometries (one whose enormous implied NI drives every sampled point's
/// utilization far over the limit, one whose small implied NI passes
/// cleanly), one station, two turn indices, orders [2, 4] and no pruning.
/// Never the frozen OC-007/OC-007-control cases, which run at the full
/// declared orders [10, 14] and are far too slow for a debug-profile test.
fn small_coupled_search_case_json() -> &'static str {
    r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "cli-test-small-search-case",
  "provenance": "CLI integration test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [3, 60], "tapes_along_width": [2]},
  "operating": {"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001},
  "material": {
    "dataset_id": "robinson-superpower-ap-v3",
    "csv_sha256": "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
    "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
    "angle_mapping": "period_180_field_reversal",
    "mirror_policy": "minimum_of_mirror_pair",
    "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
    "field_magnitude_policy": "total_magnitude_with_transverse_angle",
    "low_field_policy": "monotone_field_lower_bound",
    "low_field_clamp_t": 1.001,
    "monotonicity_tolerance": 0.001
  },
  "sampling": {
    "stations": [{"id": "s0", "kind": "straight", "x_m": 0.0}],
    "relative_turn_indices": [
      {"kind": "from_start", "offset": 1},
      {"kind": "from_end", "offset": 0}
    ],
    "width_points": 5
  },
  "limits": {
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  },
  "numerics": {"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5},
  "pruning": null,
  "cost": {
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  },
  "baseline": {"turns_along_normal": 60, "tapes_along_width": 2},
  "refined_plan": {
    "additional_stations": [
      {"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0},
      {"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0},
      {"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0},
      {"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0},
      {"id": "straight_015", "kind": "straight", "x_m": 0.15},
      {"id": "straight_025", "kind": "straight", "x_m": 0.25}
    ],
    "max_sampling_shortfall_fraction": 0.02
  },
  "execution": {"max_threads": 3}
}
"#
}

#[test]
fn coupled_search_cli_runs_on_a_reduced_case_and_protects_saved_evidence() {
    let dir = temp_dir("coupled-search-cli");
    fs::create_dir_all(&dir).unwrap();
    let case_path = dir.join("case.json");
    fs::write(&case_path, small_coupled_search_case_json()).unwrap();
    let output_path = dir.join("run.json");

    let output = cli()
        .args(["coupled-search"])
        .arg(&case_path)
        .args(["--json", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "optcoil-coupled-search-run/v26");
    assert_eq!(stdout["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(stdout["baseline_index"], 1); // 60-turn geometry is index 1
    let bytes = fs::read(&output_path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stdout, saved);
    // Exit status follows search_status PASS AND acceptance agreement PASS.
    let all_pass =
        stdout["search_status"] == "PASS" && stdout["acceptance"]["agreement_status"] == "PASS";
    assert_eq!(output.status.success(), all_pass);

    // --output is exclusive creation; a second run to the same path fails
    // and leaves the first saved report untouched (matches finish_coupled's
    // pattern).
    let overwrite = cli()
        .args(["coupled-search"])
        .arg(&case_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(bytes, fs::read(&output_path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn coupled_search_threads_may_only_lower_the_case_declared_maximum() {
    let dir = temp_dir("coupled-search-cli-threads");
    fs::create_dir_all(&dir).unwrap();
    let case_path = dir.join("case.json");
    fs::write(&case_path, small_coupled_search_case_json()).unwrap();

    let too_many = cli()
        .args(["coupled-search"])
        .arg(&case_path)
        .args(["--threads", "99", "--json"])
        .output()
        .unwrap();
    assert!(!too_many.status.success());
    assert!(String::from_utf8_lossy(&too_many.stderr).contains("OptCoil:"));

    let lowered = cli()
        .args(["coupled-search"])
        .arg(&case_path)
        .args(["--threads", "1", "--json"])
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&lowered.stdout).unwrap();
    assert_eq!(record["runtime"]["execution_threads"], 1);
    fs::remove_dir_all(dir).unwrap();
}

/// A minimal, fast-to-evaluate schema-v2 coupled refine case: the same tiny
/// OC-007-shaped geometry as `small_coupled_search_case_json` (one whose
/// enormous implied NI FAILs at 3 turns, one whose small implied NI PASSes
/// at 60 turns), extended with a `refinement` block over a single pancake
/// count whose bracket is exactly that known FAIL/PASS pair. Never the
/// frozen OC-008 case, which declares orders [10, 14] and is far too slow
/// for a debug-profile test.
fn small_coupled_refine_case_json() -> &'static str {
    r#"{
  "schema": "optcoil-coupled-search/v2",
  "id": "cli-test-small-refine-case",
  "provenance": "CLI integration test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [3, 60], "tapes_along_width": [2]},
  "operating": {"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001},
  "material": {
    "dataset_id": "robinson-superpower-ap-v3",
    "csv_sha256": "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
    "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
    "angle_mapping": "period_180_field_reversal",
    "mirror_policy": "minimum_of_mirror_pair",
    "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
    "field_magnitude_policy": "total_magnitude_with_transverse_angle",
    "low_field_policy": "monotone_field_lower_bound",
    "low_field_clamp_t": 1.001,
    "monotonicity_tolerance": 0.001
  },
  "sampling": {
    "stations": [{"id": "s0", "kind": "straight", "x_m": 0.0}],
    "relative_turn_indices": [
      {"kind": "from_start", "offset": 1},
      {"kind": "from_end", "offset": 0}
    ],
    "width_points": 5
  },
  "limits": {
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 1.0e6,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  },
  "numerics": {"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5},
  "pruning": null,
  "cost": {
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  },
  "baseline": {"turns_along_normal": 60, "tapes_along_width": 2},
  "refined_plan": {
    "additional_stations": [
      {"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0},
      {"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0},
      {"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0},
      {"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0},
      {"id": "straight_015", "kind": "straight", "x_m": 0.15},
      {"id": "straight_025", "kind": "straight", "x_m": 0.25}
    ],
    "max_sampling_shortfall_fraction": 0.02
  },
  "refinement": {
    "pancake_counts": [2],
    "turn_resolution": 5,
    "turn_bounds": {"min": 1, "max": 100},
    "brackets": [{"tapes": 2, "fail_turns": 3, "pass_turns": 60}],
    "monotonicity_check": true
  },
  "execution": {"max_threads": 3}
}
"#
}

#[test]
fn coupled_refine_cli_runs_on_a_reduced_case_and_protects_saved_evidence() {
    let dir = temp_dir("coupled-refine-cli");
    fs::create_dir_all(&dir).unwrap();
    let case_path = dir.join("case.json");
    fs::write(&case_path, small_coupled_refine_case_json()).unwrap();
    let output_path = dir.join("run.json");

    let output = cli()
        .args(["coupled-refine"])
        .arg(&case_path)
        .args(["--json", "--output"])
        .arg(&output_path)
        .output()
        .unwrap();
    let stdout: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema"], "optcoil-coupled-refine-run/v3");
    assert_eq!(stdout["pancakes"].as_array().unwrap().len(), 1);
    assert_eq!(stdout["pancakes"][0]["bracket_status"], "VALID");
    let bytes = fs::read(&output_path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(stdout, saved);
    // Exit status follows search_status PASS AND acceptance agreement PASS.
    let all_pass =
        stdout["search_status"] == "PASS" && stdout["acceptance"]["agreement_status"] == "PASS";
    assert_eq!(output.status.success(), all_pass);

    // --output is exclusive creation; a second run to the same path fails
    // and leaves the first saved report untouched.
    let overwrite = cli()
        .args(["coupled-refine"])
        .arg(&case_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert_eq!(bytes, fs::read(&output_path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn coupled_refine_rejects_a_v1_case_and_threads_may_only_lower_the_declared_maximum() {
    let dir = temp_dir("coupled-refine-cli-v1-and-threads");
    fs::create_dir_all(&dir).unwrap();
    let v1_case_path = dir.join("v1-case.json");
    fs::write(&v1_case_path, small_coupled_search_case_json()).unwrap();

    let v1_rejected = cli()
        .args(["coupled-refine"])
        .arg(&v1_case_path)
        .args(["--json"])
        .output()
        .unwrap();
    assert!(!v1_rejected.status.success());
    assert!(String::from_utf8_lossy(&v1_rejected.stderr).contains("OptCoil:"));

    let v2_case_path = dir.join("v2-case.json");
    fs::write(&v2_case_path, small_coupled_refine_case_json()).unwrap();

    let too_many = cli()
        .args(["coupled-refine"])
        .arg(&v2_case_path)
        .args(["--threads", "99", "--json"])
        .output()
        .unwrap();
    assert!(!too_many.status.success());
    assert!(String::from_utf8_lossy(&too_many.stderr).contains("OptCoil:"));

    let lowered = cli()
        .args(["coupled-refine"])
        .arg(&v2_case_path)
        .args(["--threads", "1", "--json"])
        .output()
        .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&lowered.stdout).unwrap();
    assert_eq!(record["runtime"]["execution_threads"], 1);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dataset_attestation_lifecycle() {
    let dir = temp_dir("dataset-attest");
    fs::create_dir_all(&dir).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/materials/robinson-superpower-ap-v3");
    let key = dir.join("issuer.key.json");
    let unsigned = dir.join("unsigned.json");
    let signed = dir.join("bundle.json");
    let registry = dir.join("datasets.json");

    // keygen -> prints a public key, writes the secret file
    let output = cli()
        .args([
            "dataset",
            "keygen",
            "--issuer",
            "test-issuer",
            "--key-id",
            "t1",
        ])
        .arg("--output")
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("public key: ed25519:"));
    assert!(key.exists());

    // build -> unsigned v2 bundle
    let output = cli()
        .args(["dataset", "build"])
        .arg(root.join("material.json"))
        .arg(root.join("measurements.csv"))
        .arg("--output")
        .arg(&unsigned)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // attest -> signed v2 bundle
    let output = cli()
        .args(["dataset", "attest"])
        .arg(&unsigned)
        .arg("--key")
        .arg(&key)
        .arg("--output")
        .arg(&signed)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let signed_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&signed).unwrap()).unwrap();
    assert_eq!(signed_json["schema"], "optcoil-material-dataset/v2");
    assert_eq!(signed_json["attestation"]["issuer"], "test-issuer");

    // unsigned bundle verifies with NOT_CHECKED attestation, zero FAIL
    let output = cli()
        .args(["dataset", "verify"])
        .arg(&unsigned)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("NOT_CHECKED"), "{stdout}");

    // registry-upsert -> countersigned entry + issuer key
    let output = cli()
        .args(["dataset", "registry-upsert"])
        .arg(&signed)
        .arg("--registry")
        .arg(&registry)
        .arg("--key")
        .arg(&key)
        .args(["--status", "current", "--add-key"])
        .arg(&key)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // verify -> all PASS against the registry
    let output = cli()
        .args(["dataset", "verify"])
        .arg(&signed)
        .arg("--registry")
        .arg(&registry)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("0 FAIL"), "{stdout}");
    assert!(stdout.contains("registry.status: current"), "{stdout}");

    // tampered signature -> FAIL + nonzero exit
    let mut tampered = signed_json.clone();
    tampered["attestation"]["signature"] =
        serde_json::Value::String(format!("ed25519:{}", "ab".repeat(64)));
    let tampered_path = dir.join("tampered.json");
    fs::write(&tampered_path, serde_json::to_string(&tampered).unwrap()).unwrap();
    let output = cli()
        .args(["dataset", "verify"])
        .arg(&tampered_path)
        .arg("--registry")
        .arg(&registry)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL"), "{stdout}");

    fs::remove_dir_all(&dir).ok();
}
