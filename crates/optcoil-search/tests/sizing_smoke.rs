use serde_json::json;

#[test]
fn sizing_estimate_on_oc023_seam() {
    let case = optcoil_model::coupled_search::CoupledSearchCase::from_json(
        &std::fs::read_to_string("../../benchmarks/coupled/oc-023-seam.json").unwrap(),
    )
    .unwrap();
    let hint = optcoil_search::sizing::sizing_estimate(&case).unwrap();
    println!(
        "unit_bz={:.3e} NI={:.0} ic_opt={:?} ic_mid={:?} ic_pess={:?}",
        hint.unit_bore_field_t_per_at,
        hint.ni_needed_a,
        hint.ic_floor_optimistic_a,
        hint.ic_floor_mid_a,
        hint.ic_floor_pessimistic_a
    );
    println!(
        "table={:?} turns={:?} count={}",
        hint.min_turns_table, hint.suggested_turns, hint.candidate_count
    );
    for n in &hint.notes {
        println!("note: {n}");
    }
}

/// A declared-map case sizes from the map producer's anchor — no kernel
/// evaluation — and the map's own peak node supplies the pessimistic
/// conductor-field proxy. Grafted onto oc-023-seam's JSON: fixed pack
/// extents = baseline counts × pitch/width, a coarse Cartesian map
/// covering the swept domain, and the good-field region removed (maps
/// forbid it).
#[test]
fn sizing_estimate_on_a_declared_map_case() {
    let mut v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("../../benchmarks/coupled/oc-029-vendor.json").unwrap(),
    )
    .unwrap();
    v["requirement"]
        .as_object_mut()
        .unwrap()
        .remove("good_field_region");
    let turns = v["baseline"]["turns_along_normal"].as_f64().unwrap();
    let tapes = v["baseline"]["tapes_along_width"].as_f64().unwrap();
    let pitch = v["fixed_geometry"]["radial_pitch_m"].as_f64().unwrap();
    let width = v["fixed_geometry"]["tape_width_m"].as_f64().unwrap();
    v["fixed_geometry"]["pack_radial_width_m"] = json!(turns * pitch);
    v["fixed_geometry"]["pack_axial_height_m"] = json!(tapes * width);
    // Coarse Cartesian map: uniform Bz = 0.9 T at the declared
    // reference ampere-turns — a synthetic declared field, not data.
    let entries: Vec<serde_json::Value> = (0..8)
        .map(|i| {
            json!({"x_index": i / 4, "y_index": (i / 2) % 2, "z_index": i % 2,
                   "bx_t": 0.0, "by_t": 0.0, "bz_t": 0.9})
        })
        .collect();
    v["field_map"] = json!({
        "map": {
            "components": "cartesian_bx_by_bz",
            "source_sha256": "0".repeat(64),
            "reference_ampere_turns_a": 100_000.0,
            "x_levels_m": [-0.9, 0.9],
            "y_levels_m": [-0.9, 0.9],
            "z_levels_m": [-0.2, 0.2],
            "entries": entries,
        },
        "bore_field_at_reference_t": 0.9,
    });
    let json = serde_json::to_string(&v).unwrap();
    let case = optcoil_model::coupled_search::CoupledSearchCase::from_json(&json).unwrap();
    let hint = optcoil_search::sizing::sizing_estimate(&case).unwrap();
    // NI = 0.9 / (0.9 / 100k) = 100k ampere-turns — the declared anchor
    // verbatim, no engine evaluation.
    assert!((hint.ni_needed_a - 100_000.0).abs() < 1.0);
    assert!(hint.notes.iter().any(|n| n.contains("declared map anchor")));
    assert!(
        hint.notes.iter().any(|n| n.contains("map's own peak node")),
        "{:?}",
        hint.notes
    );
    for n in &hint.notes {
        println!("map note: {n}");
    }
}
