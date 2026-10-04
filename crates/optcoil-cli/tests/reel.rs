use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use optcoil_model::{attestation::sha256_hex, material::MaterialDataset};
use serde_json::Value;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_optcoil"))
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "optcoil-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/reels")
        .join(name)
}

fn stdout_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn rate_args_at(inventory: &Path, t: &str, b: &str, angle: &str) -> Vec<String> {
    vec![
        "inventory".into(),
        "rate".into(),
        inventory.display().to_string(),
        "--temperature-k".into(),
        t.into(),
        "--field-t".into(),
        b.into(),
        "--angle-deg".into(),
        angle.into(),
    ]
}

fn rate_args(inventory: &Path) -> Vec<String> {
    rate_args_at(inventory, "25.0", "2.0", "0.0")
}

fn statuses(record: &Value) -> Vec<(String, String)> {
    record["reels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["reel_id"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn reel_validate_reports_the_hash_of_the_exact_bytes() {
    let path = example("passport-illustrative.json");
    let output = cli()
        .args(["reel", "validate", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let json = stdout_json(&output);
    assert_eq!(json["reel_id"], "ILLUSTRATIVE-REEL-A");
    assert_eq!(
        json["passport_sha256"].as_str().unwrap(),
        sha256_hex(&fs::read(&path).unwrap())
    );
    assert_eq!(json["usable_length_m"], 45.0);
    assert_eq!(json["evidence_class"], "synthetic");
    let text = cli()
        .args(["reel", "validate"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("is valid"));
}

#[test]
fn reel_validate_rejects_an_invalid_passport() {
    let dir = temp_dir("reel-invalid");
    let text = fs::read_to_string(example("passport-illustrative.json")).unwrap();
    let bad = dir.join("bad.json");
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["length_profiles"][0]["points"][1][0] = Value::from(0.0);
    fs::write(&bad, value.to_string()).unwrap();
    let output = cli().args(["reel", "validate"]).arg(&bad).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("strictly increase"));
    let unknown = dir.join("unknown.json");
    fs::write(&unknown, text.replace("\"reel_id\"", "\"reel_ident\"")).unwrap();
    assert!(
        !cli()
            .args(["reel", "validate"])
            .arg(&unknown)
            .output()
            .unwrap()
            .status
            .success()
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn inventory_validate_reports_totals() {
    let path = example("inventory-illustrative.json");
    let output = cli()
        .args(["inventory", "validate", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let json = stdout_json(&output);
    assert_eq!(json["summary"]["reel_count"], 3);
    assert_eq!(json["summary"]["total_length_m"], 130.0);
    assert_eq!(json["summary"]["usable_length_m"], 125.0);
    assert_eq!(json["summary"]["evidence_class_counts"]["synthetic"], 3);
    assert_eq!(json["evidence_class"], "synthetic");
}

#[test]
fn inventory_rate_rates_the_example_with_embedded_datasets() {
    let output = cli()
        .args(rate_args(&example("inventory-illustrative.json")))
        .output()
        .unwrap();
    let record = stdout_json(&output);
    assert_eq!(record["schema"], "optcoil-reel-rating/v1");
    assert_eq!(
        statuses(&record),
        vec![
            ("ILLUSTRATIVE-REEL-A".to_string(), "rated".to_string()),
            (
                "ILLUSTRATIVE-REEL-B".to_string(),
                "no_product_map".to_string()
            ),
            ("ILLUSTRATIVE-REEL-C".to_string(), "rated".to_string()),
        ]
    );
    // The examples are invented, so every rating is synthetic.
    assert_eq!(record["reels"][0]["evidence_class"], "synthetic");
    assert_eq!(record["reels"][2]["evidence_class"], "synthetic");
    assert_eq!(
        record["reels"][0]["scaled"]["profile"]["map_reference_row"],
        1817
    );
    assert!(
        record["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("Molodyk"))
    );
    assert!(record["reels"][1]["next_evidence"].as_str().unwrap().len() > 10);
    assert_eq!(record["datasets"][0]["source"], "embedded");
    assert_eq!(
        record["inventory_sha256"].as_str().unwrap(),
        sha256_hex(&fs::read(example("inventory-illustrative.json")).unwrap())
    );
    // In-field point at the operating point: a check, never a status change.
    assert_eq!(
        record["reels"][0]["consistency_checks"][0]["sample_id"],
        "ILLUSTRATIVE-SS-1"
    );
}

#[test]
fn inventory_rate_outside_the_map_domain_and_in_the_tape_plane() {
    let inventory = example("inventory-illustrative.json");
    let record = stdout_json(
        &cli()
            .args(rate_args_at(&inventory, "25.0", "12.0", "0.0"))
            .output()
            .unwrap(),
    );
    assert_eq!(statuses(&record)[0].1, "outside_map_domain");

    let record = stdout_json(
        &cli()
            .args(rate_args_at(&inventory, "24.99", "2.0", "88.69"))
            .output()
            .unwrap(),
    );
    // Reel A carries an ab offset, so it is rated near the tape plane.
    assert_eq!(statuses(&record)[0].1, "rated");
    // Reel B has no offsets and no map: the earlier rule decides.
    assert_eq!(statuses(&record)[1].1, "no_product_map");
    assert_eq!(
        record["reels"][0]["orientation"]["in_tape_plane_window"],
        true
    );
    assert_eq!(record["reels"][0]["orientation"]["offset_bound_deg"], 1.5);
}

#[test]
fn inventory_rate_writes_a_new_file_only() {
    let dir = temp_dir("reel-output");
    let out = dir.join("rating.json");
    let mut args = rate_args(&example("inventory-illustrative.json"));
    args.extend(["--output".into(), out.display().to_string()]);
    let first = cli().args(&args).output().unwrap();
    assert!(first.status.success());
    assert!(first.stdout.is_empty());
    let written: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert_eq!(written["schema"], "optcoil-reel-rating/v1");
    let second = cli().args(&args).output().unwrap();
    assert!(!second.status.success());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn inventory_rate_enforces_dataset_identity_for_supplied_bundles() {
    let dir = temp_dir("reel-bundle");
    let id = "robinson-superpower-ap-v3";
    let genuine = MaterialDataset::embedded_bundle_json(id).unwrap();
    let genuine_path = dir.join("genuine.json");
    fs::write(&genuine_path, &genuine).unwrap();

    let mut args = rate_args(&example("inventory-illustrative.json"));
    args.extend([
        "--dataset-bundle".into(),
        genuine_path.display().to_string(),
    ]);
    let record = stdout_json(&cli().args(&args).output().unwrap());
    assert_eq!(record["reels"][0]["status"], "rated");
    assert_eq!(record["datasets"][0]["source"], "supplied");

    // Same id, different measurements (and a self-consistent new hash): the
    // bundle is valid on its own but is not the dataset the passport pins.
    let mut value: Value = serde_json::from_str(&genuine).unwrap();
    let csv = value["csv_data"]
        .as_str()
        .unwrap()
        .replace("293111.0", "293112.0");
    value["metadata"]["csv_sha256"] = Value::String(sha256_hex(csv.as_bytes()));
    value["csv_data"] = Value::String(csv);
    let altered_path = dir.join("altered.json");
    fs::write(&altered_path, value.to_string()).unwrap();
    let mut args = rate_args(&example("inventory-illustrative.json"));
    args.extend([
        "--dataset-bundle".into(),
        altered_path.display().to_string(),
    ]);
    let record = stdout_json(&cli().args(&args).output().unwrap());
    assert_eq!(record["reels"][0]["status"], "map_unavailable");
    assert!(
        record["reels"][0]["explanation"]
            .as_str()
            .unwrap()
            .contains("csv_sha256")
    );

    // Two bundles with the same id are rejected outright.
    let mut args = rate_args(&example("inventory-illustrative.json"));
    for path in [&genuine_path, &genuine_path] {
        args.extend(["--dataset-bundle".into(), path.display().to_string()]);
    }
    assert!(!cli().args(&args).output().unwrap().status.success());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn inventory_synthesize_writes_two_new_files_and_the_output_validates_and_rates() {
    let dir = temp_dir("reel-synth");
    let spec = example("synthetic-spec.json");
    let inventory = dir.join("inventory.json");
    let truth = dir.join("truth.json");
    let synth = |inventory: &Path, truth: &Path| {
        cli()
            .args(["inventory", "synthesize"])
            .arg(&spec)
            .arg("--output")
            .arg(inventory)
            .arg("--truth-output")
            .arg(truth)
            .output()
            .unwrap()
    };
    let first = synth(&inventory, &truth);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let truth_json: Value = serde_json::from_slice(&fs::read(&truth).unwrap()).unwrap();
    assert_eq!(truth_json["schema"], "optcoil-synthetic-truth/v1");
    assert_eq!(
        truth_json["inventory_sha256"].as_str().unwrap(),
        sha256_hex(&fs::read(&inventory).unwrap())
    );
    assert_eq!(truth_json["reels"].as_array().unwrap().len(), 20);

    let validated = stdout_json(
        &cli()
            .args(["inventory", "validate", "--json"])
            .arg(&inventory)
            .output()
            .unwrap(),
    );
    assert_eq!(
        validated["summary"]["evidence_class_counts"]["synthetic"],
        20
    );
    let rated = stdout_json(&cli().args(rate_args(&inventory)).output().unwrap());
    assert_eq!(rated["status_counts"]["rated"], 20);

    // Never overwritten: either output existing refuses, and nothing changes.
    let before = fs::read(&inventory).unwrap();
    assert!(
        !synth(&inventory, &dir.join("other-truth.json"))
            .status
            .success()
    );
    assert!(
        !synth(&dir.join("other-inventory.json"), &truth)
            .status
            .success()
    );
    assert_eq!(fs::read(&inventory).unwrap(), before);
    assert!(!dir.join("other-truth.json").exists());

    // Same spec, same bytes.
    let (inv2, truth2) = (dir.join("inv2.json"), dir.join("truth2.json"));
    assert!(synth(&inv2, &truth2).status.success());
    assert_eq!(fs::read(&inv2).unwrap(), before);
    assert_eq!(fs::read(&truth2).unwrap(), fs::read(&truth).unwrap());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn inventory_synthesize_rejects_a_bad_spec_without_writing() {
    let dir = temp_dir("reel-synth-bad");
    let mut spec: Value =
        serde_json::from_slice(&fs::read(example("synthetic-spec.json")).unwrap()).unwrap();
    spec["profile"]["map_reference_row"] = Value::from(999_999);
    let path = dir.join("spec.json");
    fs::write(&path, spec.to_string()).unwrap();
    let output = cli()
        .args(["inventory", "synthesize"])
        .arg(&path)
        .arg("--output")
        .arg(dir.join("i.json"))
        .arg("--truth-output")
        .arg(dir.join("t.json"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!dir.join("i.json").exists() && !dir.join("t.json").exists());
    let _ = fs::remove_dir_all(dir);
}
