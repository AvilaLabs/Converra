//! OC-012 customer-style case: a startup-scale demonstration dipole.
//!
//! The question is not "beat a built coil" — it is the product question:
//! given a spec a small fusion company would actually write down, what
//! does the verified optimum cost, and what does the *declared baseline*
//! (a conservative first-pass pack) leave on the table?
//!
//! Spec (invented but plausible — a "5 T demonstrator" class dipole):
//!   5 T minimum over an 80 mm x 80 mm usable aperture, straight
//!   section ~400 mm. Conductor fields land ~6-7 T, inside the
//!   dataset's ~8 T validated coverage (deliberately: WHAM-class
//!   17 T-at-conductor specs are outside coverage and would return
//!   INCONCLUSIVE everywhere — that coverage bound is itself a
//!   documented product limit, see docs/RESEARCH_ANCHORS.md).
//!
//! The mechanical block exercises the schema-v4 Lorentz screen at the
//! published stack-level bound (~300 kN/m, RESEARCH_ANCHORS); at this
//! scale I*B ~ 10 kN/m — the gate records a real verdict, not a waiver.
//!
//! `cargo run --release -p optcoil-search --example oc012_startup_dipole`
//!
//! Pass `v5` as argv[1] to rerun under schema v5 with
//! `limits.self_field_correction = "uniform_transport"` (OC-014): the
//! corrected query resolves candidates whose k_used self-field ratio sits
//! in the 0.1–1 band, while the transport-dominance gate (ratio > 1)
//! keeps the critical-state cluster honestly INCONCLUSIVE. Output goes to
//! `runs/oc012-startup-dipole-v5/` so v4 and v5 records stay distinct.
//!
//! Pass `v5mx` for the OC-016 sensitivity study: identical v5 semantics on
//! the labeled model-extension dataset (`robinson-superpower-ap-v3-modelext`,
//! power-law continuation above 8 T, conservative margined). Output goes to
//! `runs/oc012-startup-dipole-v5-modelext/`. These records answer "would the
//! coverage-blocked candidates pass if high-field data confirmed the
//! continuation" — they are model-informed and are never measured-data
//! verification.
//!
//! Pass `v7mx` for the OC-018 study: the v6mx stack (self-field
//! correction + modelext dataset + transverse_bound) plus the declared
//! hoop-stress bound — a self-supporting 12 mm tape section against a
//! 450 MPa assumption. Output to `runs/oc012-startup-dipole-v7mx/`.
//!
//! Pass `v8mx` for the OC-014 Phase-2 study: the v7mx stack plus
//! `limits.self_field_correction = "critical_state_strip"` and a declared
//! `critical_state_layer_thickness_m`, so a carried-self-field-dominant
//! refined-plan point resolves to a determined verdict instead of
//! INCONCLUSIVE. Output to `runs/oc012-startup-dipole-v8mx/`.
//!
//! Pass `v9mx` for the Phase-2b study: identical case, but the build's
//! field-consistent `k_bound(B_query)` bound (method /v3, model /v6)
//! replaces the strict floor — measuring whether the tighter bound
//! converts marginal FAILs to certified PASS. Output to
//! `runs/oc012-startup-dipole-v9mx/`.
//!
//! Pass `v6mx` for the OC-017 study: search schema v6 with
//! `limits.along_current_model = "transverse_bound"` on top of the v5mx
//! setup — the last remaining blocker class gets bounded estimates
//! (labeled `along_current_bounded` in the records), so every candidate
//! should return a determined verdict. Output goes to
//! `runs/oc012-startup-dipole-v6mx/`.

use optcoil_model::material::{MaterialDataset, SUPERPOWER_MODELEXT_ID};
use optcoil_search::coupled_search::{CoupledSearchOptions, run_coupled_search_case};

/// 5 T minimum across the usable volume.
const B_TARGET_T: f64 = 5.0;

/// 80 mm usable aperture (y, z halves). x-extent stays inside the
/// straights (0.8*l) — the beam cannot reach into the bend annulus.
const REGION_YZ_HALF_M: [f64; 2] = [0.04, 0.04];

/// Inner pack face must clear the 80 mm bore: R - n*pitch/2 >= 0.04.
/// At 5 T the NI demand is ~3x Feather-M2's — turns run high, so R
/// starts at 0.055 (n=480 needs R >= ~0.052 to clear the bore).
const GRID_R: &[f64] = &[0.055, 0.07, 0.09];
const GRID_L: &[f64] = &[0.15, 0.20, 0.25];

/// High turns: NI for 5 T at R~0.06 is ~1 MAt; screened current is
/// ~1-2 kA, so the optimum wants 400-800 turns. Wide range so the
/// gates do the work.
const TURNS: &[u32] = &[200, 320, 480, 640, 800];
const TAPES: &[u32] = &[4, 6, 8, 12];
const STRANDS: &[u32] = &[1];

const CELLS_IN_FLIGHT: usize = 2;
const THREADS_PER_CELL: u32 = 4;

/// Conservative first-pass baseline: the biggest pack on the grid
/// (a startup's "make it definitely work" design).
const BASELINE: (u32, u32) = (800, 12);

fn case_json(
    l: f64,
    r: f64,
    corrected: bool,
    modelext: bool,
    bound: bool,
    hoop: bool,
    cs: bool,
) -> String {
    let mut case: serde_json::Value =
        serde_json::from_str(optcoil_model::coupled_search::OC007_JSON).expect("OC-007 parses");
    case["schema"] = if cs {
        "optcoil-coupled-search/v8".into()
    } else if hoop {
        "optcoil-coupled-search/v7".into()
    } else if bound {
        "optcoil-coupled-search/v6".into()
    } else if corrected {
        "optcoil-coupled-search/v5".into()
    } else {
        "optcoil-coupled-search/v4".into()
    };
    case["id"] = format!("oc012-startup-dipole-L{l:.3}-R{r:.3}").into();
    let region_half_m = [0.8 * l, REGION_YZ_HALF_M[0], REGION_YZ_HALF_M[1]];
    case["provenance"] = format!(
        "OC-012 customer-style demonstration-dipole study (study, not a frozen benchmark): \
         OC-007 physics and synthetic $30/m cost model; invented but plausible spec: \
         {B_TARGET_T} T minimum over an 80 mm x 80 mm usable aperture (here +-{region_half_m:?} m, \
         x-extent 0.8*l) at a ~{:.0} mm straight section. Mechanical screen at 300 kN/m \
         (published stack-level bound). Baseline is a deliberately conservative first-pass \
         pack ({BASELINE:?} n,p) — the artifact under test is 'your first design vs the \
         verified optimum'.",
        2.0 * l * 1000.0
    )
    .into();
    case["requirement"]["b_target_t"] = B_TARGET_T.into();
    case["requirement"]["good_field_region"] = serde_json::json!({
        "half_extents_m": region_half_m,
        "points_per_axis": 3
    });
    case["fixed_geometry"]["straight_half_length_m"] = l.into();
    case["fixed_geometry"]["bend_radius_m"] = r.into();
    case["choices"]["turns_along_normal"] = serde_json::json!(TURNS.to_vec());
    case["choices"]["tapes_along_width"] = serde_json::json!(TAPES.to_vec());
    case["choices"]["strands_parallel"] = serde_json::json!(STRANDS.to_vec());
    case["baseline"] = serde_json::json!({
        "turns_along_normal": BASELINE.0,
        "tapes_along_width": BASELINE.1,
        "strands_parallel": 1
    });
    case["sampling"]["stations"] = serde_json::json!([
        {"id": "straight_center", "kind": "straight", "x_m": 0.0},
        {"id": "straight_near_junction", "kind": "straight", "x_m": 0.9 * l},
        {"id": "arc_apex", "kind": "arc", "azimuth_deg": 0.0},
        {"id": "arc_mid", "kind": "arc", "azimuth_deg": 45.0},
    ]);
    case["refined_plan"]["additional_stations"] = serde_json::json!([
        {"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0},
        {"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0},
        {"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0},
        {"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0},
        {"id": "straight_a", "kind": "straight", "x_m": 0.4 * l},
        {"id": "straight_b", "kind": "straight", "x_m": 0.8 * l},
    ]);
    case["refinement"] = serde_json::json!({
        "pancake_counts": [4, 6, 8, 12],
        "turn_resolution": 10,
        "turn_bounds": {"min": 1, "max": 1000},
        "brackets": [
            {"tapes": 4, "fail_turns": 100, "pass_turns": 800},
            {"tapes": 6, "fail_turns": 100, "pass_turns": 640},
            {"tapes": 8, "fail_turns": 100, "pass_turns": 480},
            {"tapes": 12, "fail_turns": 100, "pass_turns": 320},
        ],
        "monotonicity_check": true
    });
    case["manufacturing"] = serde_json::json!({"min_inner_bend_radius_m": 0.005});
    case["mechanical"] = if hoop {
        // OC-018 hoop-stress bound. Declared assumptions, not qualified
        // limits: a self-supporting single 12 mm tape's load path
        // (~100 µm substrate+stabilizer ≈ 1.2 mm²) and the conservative
        // end of published REBCO axial-tensile regimes (~450 MPa).
        serde_json::json!({
            "max_lorentz_load_n_per_m": 300_000.0,
            "max_hoop_stress_pa": 450.0e6,
            "tension_section_area_m2": 1.2e-6
        })
    } else {
        serde_json::json!({"max_lorentz_load_n_per_m": 300_000.0})
    };
    if cs {
        // OC-014 Phase 2: replace the uniform-transport correction with the
        // critical-state edge-field bound so a carried-self-field-dominant
        // point (the refined-plan null pocket) resolves to a determined
        // verdict instead of INCONCLUSIVE. The declared REBCO layer thickness
        // is the standoff in the edge-field log term — 1 µm is the
        // conservative end of a SCS12050-family layer.
        case["limits"]["self_field_correction"] = "critical_state_strip".into();
        case["limits"]["critical_state_layer_thickness_m"] = 1.0e-6.into();
    } else if corrected {
        case["limits"]["self_field_correction"] = "uniform_transport".into();
    }
    if bound {
        case["limits"]["along_current_model"] = "transverse_bound".into();
    }
    if modelext {
        // OC-016 sensitivity study: same v5 semantics, but the material
        // dataset is the labeled model extension above 8 T. The pinned
        // csv_sha256 is injected from the embedded metadata so it cannot
        // drift from the dataset actually loaded. Verdicts from these runs
        // are model-informed, not measured-data-verified.
        let dataset = MaterialDataset::embedded_by_id(SUPERPOWER_MODELEXT_ID)
            .expect("modelext dataset embeds");
        case["material"]["dataset_id"] = SUPERPOWER_MODELEXT_ID.into();
        case["material"]["csv_sha256"] = dataset.metadata.csv_sha256.clone().into();
    }
    serde_json::to_string(&case).expect("serializable")
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let cs = mode == "v8mx" || mode == "v9mx";
    let hoop = cs || mode == "v7mx";
    let bound = hoop || mode == "v6mx";
    let corrected = bound || mode == "v5" || mode == "v5mx";
    let modelext = bound || mode == "v5mx";
    let out_dir = if mode == "v9mx" {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v9mx")
    } else if cs {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v8mx")
    } else if hoop {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v7mx")
    } else if bound {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v6mx")
    } else if modelext {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v5-modelext")
    } else if corrected {
        std::path::PathBuf::from("runs/oc012-startup-dipole-v5")
    } else {
        std::path::PathBuf::from("runs/oc012-startup-dipole")
    };
    std::fs::create_dir_all(&out_dir).expect("create runs/oc012-startup-dipole");

    let cells: Vec<(f64, f64)> = GRID_L
        .iter()
        .flat_map(|&l| GRID_R.iter().map(move |&r| (l, r)))
        .filter(|(l, r)| !out_dir.join(format!("run-L{l:.3}-R{r:.3}.json")).exists())
        .collect();

    let next = &std::sync::atomic::AtomicUsize::new(0);
    let cells_ref = &cells;
    let out_dir_ref = &out_dir;
    std::thread::scope(|scope| {
        for _ in 0..CELLS_IN_FLIGHT {
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(l, r)) = cells_ref.get(i) else {
                        break;
                    };
                    let path = out_dir_ref.join(format!("run-L{l:.3}-R{r:.3}.json"));
                    println!("cell L={l:.3} R={r:.3}: running...");
                    match run_coupled_search_case(
                        &case_json(l, r, corrected, modelext, bound, hoop, cs),
                        &CoupledSearchOptions {
                            threads: Some(THREADS_PER_CELL),
                        },
                    ) {
                        Ok(record) => {
                            std::fs::write(
                                &path,
                                serde_json::to_string_pretty(&record).expect("serializable"),
                            )
                            .expect("write run record");
                            if let Some(b) = record.best_index.map(|b| &record.candidates[b]) {
                                println!(
                                    "cell L={l:.3} R={r:.3}: best {}x{}x{}s ${:.0}, {:.0} m tape \
                                     | status {:?} acceptance {:?}",
                                    b.geometry.turns_along_normal,
                                    b.geometry.tapes_along_width,
                                    b.geometry.strands_parallel,
                                    b.cost.total_usd,
                                    b.cost.installed_length_m,
                                    record.search_status,
                                    record.acceptance.agreement_status,
                                );
                            } else {
                                println!(
                                    "cell L={l:.3} R={r:.3}: no optimum (status {:?})",
                                    record.search_status
                                );
                            }
                        }
                        Err(e) => println!("cell L={l:.3} R={r:.3}: RUN ERROR {e:?}"),
                    }
                }
            });
        }
    });

    println!("\n=== OC-012 summary (5 T / 80 mm demonstrator spec) ===");
    for &l in GRID_L {
        for &r in GRID_R {
            let path = out_dir.join(format!("run-L{l:.3}-R{r:.3}.json"));
            if let Ok(text) = std::fs::read_to_string(&path) {
                let record: serde_json::Value = serde_json::from_str(&text).expect("record parses");
                match record["best_index"].as_u64() {
                    Some(b) => {
                        let c = &record["candidates"][b as usize];
                        println!(
                            "L={l:.3} R={r:.3}: {}x{}x{}s ${:.0} | {:.0} m | {:?}",
                            c["geometry"]["turns_along_normal"],
                            c["geometry"]["tapes_along_width"],
                            c["geometry"]["strands_parallel"],
                            c["cost"]["total_usd"].as_f64().unwrap_or(f64::NAN),
                            c["cost"]["installed_length_m"].as_f64().unwrap_or(f64::NAN),
                            record["search_status"],
                        );
                    }
                    None => println!("L={l:.3} R={r:.3}: no optimum"),
                }
            }
        }
    }
}
