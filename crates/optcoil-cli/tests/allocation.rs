use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use optcoil_model::material::MaterialDataset;
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

/// A reduced coupled search with a single 12-turn, 2-module candidate whose
/// small implied NI passes screening, so the record has a PASS optimum.
const SEARCH_CASE: &str = r#"{
  "schema": "optcoil-coupled-search/v1",
  "id": "cli-allocation-demand-case",
  "provenance": "CLI integration test fixture; not a frozen benchmark.",
  "requirement": {"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.05, "tolerance_fraction": 1e-6},
  "fixed_geometry": {
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  },
  "choices": {"turns_along_normal": [12], "tapes_along_width": [2]},
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
  "baseline": {"turns_along_normal": 12, "tapes_along_width": 2},
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
"#;

#[test]
fn allocation_demand_builds_from_a_search_record_and_protects_the_output() {
    let dir = temp_dir("allocation-demand");
    let case_path = dir.join("case.json");
    let record_path = dir.join("run.json");
    fs::write(&case_path, SEARCH_CASE).unwrap();
    // The exit status follows the search verdict and acceptance, which this
    // test does not depend on; the record is what the demand reads.
    let search = cli()
        .arg("coupled-search")
        .arg(&case_path)
        .arg("--output")
        .arg(&record_path)
        .output()
        .unwrap();
    assert!(
        record_path.exists(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );

    let output_path = dir.join("demand.json");
    let run = cli()
        .args(["allocation", "demand"])
        .arg(&record_path)
        .args(["--angle-offsets-deg", "0,2,4"])
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let bytes = fs::read(&output_path).unwrap();
    let demand: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(demand["schema"], "optcoil-allocation-demand/v1");
    assert_eq!(
        demand["angle_offsets_deg"],
        serde_json::json!([0.0, 2.0, 4.0])
    );
    assert_eq!(demand["geometry"]["turns_along_normal"], 12);
    assert_eq!(demand["geometry"]["modules"], 2);
    assert_eq!(demand["gate"]["status"], "PASS");
    assert_eq!(demand["identities"]["map_substituted"], false);
    assert_eq!(demand["turns"].as_array().unwrap().len(), 12);
    assert_eq!(
        demand["identities"]["record_sha256"].as_str().unwrap(),
        optcoil_model::attestation::sha256_hex(&fs::read(&record_path).unwrap())
    );
    let first = &demand["turns"][0]["modules"][0]["s_req"];
    assert_eq!(first.as_array().unwrap().len(), 3);
    assert!(first[0]["s_req"].as_f64().unwrap() > 0.0);
    assert!(demand["limitations"].as_array().unwrap().len() >= 6);

    // The output is a new file: a second run refuses and leaves it intact.
    let again = cli()
        .args(["allocation", "demand"])
        .arg(&record_path)
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(bytes, fs::read(&output_path).unwrap());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn allocation_demand_refuses_a_substituted_map_without_the_flag() {
    let dir = temp_dir("allocation-demand-map");
    let case_path = dir.join("case.json");
    let record_path = dir.join("run.json");
    fs::write(&case_path, SEARCH_CASE).unwrap();
    cli()
        .arg("coupled-search")
        .arg(&case_path)
        .arg("--output")
        .arg(&record_path)
        .output()
        .unwrap();

    let lowfield = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
    let map = format!("{}:{}", lowfield.metadata.id, lowfield.metadata.csv_sha256);
    let refused_path = dir.join("refused.json");
    let refused = cli()
        .args(["allocation", "demand"])
        .arg(&record_path)
        .args(["--product-map", &map])
        .arg("--output")
        .arg(&refused_path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--allow-map-substitution"));
    assert!(!refused_path.exists());

    let allowed_path = dir.join("allowed.json");
    let allowed = cli()
        .args(["allocation", "demand"])
        .arg(&record_path)
        .args(["--product-map", &map, "--allow-map-substitution"])
        .arg("--output")
        .arg(&allowed_path)
        .output()
        .unwrap();
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    let demand: Value = serde_json::from_slice(&fs::read(&allowed_path).unwrap()).unwrap();
    assert_eq!(demand["identities"]["map_substituted"], true);
    assert_eq!(
        demand["identities"]["product_map"]["dataset_id"],
        "robinson-superpower-ap-v3-lowfield"
    );

    let bad = cli()
        .args(["allocation", "demand"])
        .arg(&record_path)
        .args(["--product-map", "no-colon"])
        .arg("--output")
        .arg(dir.join("bad.json"))
        .output()
        .unwrap();
    assert!(!bad.status.success());
    fs::remove_dir_all(dir).unwrap();
}
