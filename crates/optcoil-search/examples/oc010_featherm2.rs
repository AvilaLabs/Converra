//! OC-010 real-coil benchmark: OptCoil vs a coil that was actually built.
//!
//! Reference: Feather-M2 (CERN / EuCARD-2 WP10), a real REBCO racetrack
//! dipole -- ~800 mm magnet, ~250 mm straight section, 40 mm bore, two
//! poles of aligned-block Roebel cable (15 x ~2 mm Sunam strands), achieved
//! 3.1 T at 6.5 kA / 5.7 K standalone (low-performance tape batch; the
//! design target was 5 T).
//!
//! This example expresses the Feather-M2 *requirement* as a schema-v3
//! coupled-search case -- 3.1 T minimum over a usable volume matching the
//! 40 mm aperture x ~250 mm straight -- and runs the real screened search
//! over a small (L, R) grid near the bore-forced minimum radius. The
//! honest comparison metric is conductor usage (REBCO tape-metres), not
//! dollars: it is price-model-independent and survives the placeholder
//! cost caveat. Their conductor total is estimated from published pole/
//! deck layout -- approximate, declared as such in provenance.
//!
//! `cargo run --release -p optcoil-search --example oc010_featherm2`

use optcoil_search::coupled_search::{CoupledSearchOptions, run_coupled_search_case};

/// Feather-M2 achieved standalone field (measured, not the 5 T design
/// target -- 5 T would push conductor fields past the material dataset's
/// ~8 T validated coverage).
const B_TARGET_T: f64 = 3.1;

/// Usable volume: the 40 mm clear aperture along the straight section.
/// half_extents = [along-straight, in-plane gap half, axial half]. The
/// x-extent must stay inside the straights (0.8*l per cell): the beam
/// cannot reach into the bend annulus where conductor physically lives.
/// (First grid attempt used a fixed 0.125 -- every cell correctly
/// reported pack_overlap FAIL.)
const REGION_YZ_HALF_M: [f64; 2] = [0.02, 0.02];

/// Bore forces inner pack face >= 20 mm; R ranges just above the floor.
const GRID_R: &[f64] = &[0.024, 0.028, 0.034];
/// Straight half-length around the real ~250 mm section.
const GRID_L: &[f64] = &[0.10, 0.125, 0.15];

/// Turns bounded by the 40 mm bore (inner face = R - n*pitch/2 >= 0.02 m
/// caps n < ~(R-0.02)/5e-5); the range is deliberately wide so the overlap
/// and manufacturing gates visibly bind.
const TURNS: &[u32] = &[40, 80, 120, 160, 240, 320];
/// Axial aperture coverage needs p >= 4 (4 x 12 mm = 48 mm >= 40 mm);
/// smaller values stay in the grid so the region-min gate does the work.
const TAPES: &[u32] = &[2, 3, 4, 6, 8];

/// `wide` mode (argv[1]): after the first grid showed only n<=79 clears the
/// bore at R=0.024 and the feasible corner is thin-n x wide-p -- the same
/// tall narrow pack shape a Roebel cable is. Probe it directly.
const TURNS_WIDE: &[u32] = &[40, 60, 80, 100, 120];
const TAPES_WIDE: &[u32] = &[8, 10, 12, 16];

/// `cable` mode (argv[1]): schema v4 strands_per_turn -- the actual
/// Roebel degree of freedom Feather-M2 used (15 strands sharing 6.5 kA).
/// Series turns stay thin (the bore forces it); current is shared across
/// strands instead. 5x3x4 = 60 candidates <= MAX_CANDIDATES.
const TURNS_CABLE: &[u32] = &[40, 60, 80, 100, 120];
const TAPES_CABLE: &[u32] = &[4, 6, 8];
const STRANDS_CABLE: &[u32] = &[1, 4, 8, 15];

const CELLS_IN_FLIGHT: usize = 2;
const THREADS_PER_CELL: u32 = 4;

/// Feather-M2 tape usage, estimated from published geometry: ~2 poles x
/// 2 decks x ~12 cable turns x ~1.6 m mean turn x 15 strands x 2 mm tape.
/// ~1,150 m of 2 mm tape ~ 1,150 x (2/12) ~ 190 m of 12 mm-equivalent tape.
/// APPROXIMATE -- the exact deck turn counts sit in paywalled sources; the
/// comparison is stated as a band, not a point.
const FEATHER_M2_TAPE_M_APPROX: f64 = 190.0;

fn case_json(l: f64, r: f64, turns: &[u32], tapes: &[u32], strands: &[u32]) -> String {
    let mut case: serde_json::Value =
        serde_json::from_str(optcoil_model::coupled_search::OC007_JSON).expect("OC-007 parses");
    case["schema"] = if strands != [1] {
        "optcoil-coupled-search/v4".into()
    } else {
        "optcoil-coupled-search/v3".into()
    };
    case["id"] = format!("oc010-featherm2-L{l:.3}-R{r:.3}").into();
    let region_half_m = [0.8 * l, REGION_YZ_HALF_M[0], REGION_YZ_HALF_M[1]];
    case["provenance"] = format!(
        "OC-010 real-coil benchmark cell (study, not a frozen benchmark): OC-007 physics and \
         synthetic cost model; requirement re-derived from the published Feather-M2 spec \
         (CERN/EuCARD-2): {B_TARGET_T} T achieved standalone over a 40 mm aperture x ~250 mm \
         straight usable volume, here as a +-{:?} m region (x-extent 0.8*l) with a 3x3x3 \
         lattice. Their magnet: ~800 mm racetrack, 2 poles x 2 decks of 15-strand Roebel cable \
         (~2 mm Sunam tape), ~{FEATHER_M2_TAPE_M_APPROX:.0} m 12 mm-equivalent tape \
         (approximate). Manufacturing floor 0.005 m -- measured easy-way REBCO bend limit \
         (~4.5-6.5 mm at 77 K); the 40 mm bore forces the real minimum R anyway.",
        region_half_m
    )
    .into();
    case["requirement"]["b_target_t"] = B_TARGET_T.into();
    case["requirement"]["good_field_region"] = serde_json::json!({
        "half_extents_m": region_half_m,
        "points_per_axis": 3
    });
    case["fixed_geometry"]["straight_half_length_m"] = l.into();
    case["fixed_geometry"]["bend_radius_m"] = r.into();
    case["choices"]["turns_along_normal"] = serde_json::json!(turns.to_vec());
    case["choices"]["tapes_along_width"] = serde_json::json!(tapes.to_vec());
    if strands != [1] {
        case["choices"]["strands_parallel"] = serde_json::json!(strands.to_vec());
    }
    case["baseline"] = serde_json::json!({
        "turns_along_normal": turns[turns.len()-1],
        "tapes_along_width": tapes[tapes.len()-1],
        "strands_parallel": strands[strands.len()-1]
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
        "pancake_counts": [2, 3, 4, 6, 8],
        "turn_resolution": 5,
        "turn_bounds": {"min": 1, "max": 400},
        "brackets": [
            {"tapes": 2, "fail_turns": 40, "pass_turns": 320},
            {"tapes": 3, "fail_turns": 40, "pass_turns": 240},
            {"tapes": 4, "fail_turns": 40, "pass_turns": 200},
            {"tapes": 6, "fail_turns": 40, "pass_turns": 140},
            {"tapes": 8, "fail_turns": 40, "pass_turns": 100},
        ],
        "monotonicity_check": true
    });
    case["manufacturing"] = serde_json::json!({"min_inner_bend_radius_m": 0.005});
    serde_json::to_string(&case).expect("serializable")
}

fn main() {
    // argv[1]: "wide" probes the thin-n x wide-p corner; "cable" adds the
    // v4 strands dimension (the Roebel degree of freedom).
    let mode = std::env::args().nth(1).unwrap_or_default();
    let (turns, tapes, strands): (&[u32], &[u32], &[u32]) = match mode.as_str() {
        "wide" => (TURNS_WIDE, TAPES_WIDE, &[1]),
        "cable" => (TURNS_CABLE, TAPES_CABLE, STRANDS_CABLE),
        _ => (TURNS, TAPES, &[1]),
    };
    let out_dir = std::path::PathBuf::from(format!("runs/oc010-featherm2{mode}"));
    std::fs::create_dir_all(&out_dir).expect("create runs/oc010-featherm2");

    let grid_l: &[f64] = if mode == "wide" || mode == "cable" {
        &[0.125]
    } else {
        GRID_L
    };
    let grid_r: &[f64] = if mode == "wide" || mode == "cable" {
        &[0.024, 0.028]
    } else {
        GRID_R
    };
    let cells: Vec<(f64, f64)> = grid_l
        .iter()
        .flat_map(|&l| grid_r.iter().map(move |&r| (l, r)))
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
                        &case_json(l, r, turns, tapes, strands),
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
                                let tape_m = b.cost.installed_length_m;
                                println!(
                                    "cell L={l:.3} R={r:.3}: best {}x{} ${:.0}, {:.0} m tape \
                                 ({:.1}x Feather-M2's ~{:.0} m), status {:?}, acceptance {:?}",
                                    b.geometry.turns_along_normal,
                                    b.geometry.tapes_along_width,
                                    b.cost.total_usd,
                                    tape_m,
                                    tape_m / FEATHER_M2_TAPE_M_APPROX,
                                    FEATHER_M2_TAPE_M_APPROX,
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

    println!(
        "\n=== OC-010 summary (vs Feather-M2 ~{FEATHER_M2_TAPE_M_APPROX:.0} m 12 mm-equiv tape) ==="
    );
    for &l in grid_l {
        for &r in grid_r {
            let path = out_dir.join(format!("run-L{l:.3}-R{r:.3}.json"));
            if let Ok(text) = std::fs::read_to_string(&path) {
                let record: serde_json::Value = serde_json::from_str(&text).expect("record parses");
                match record["best_index"].as_u64() {
                    Some(b) => {
                        let c = &record["candidates"][b as usize];
                        let tape_m = c["cost"]["installed_length_m"].as_f64().unwrap_or(f64::NAN);
                        println!(
                            "L={l:.3} R={r:.3}: {}x{} ${:.0} | {:.0} m = {:.1}x theirs | {:?}",
                            c["geometry"]["turns_along_normal"],
                            c["geometry"]["tapes_along_width"],
                            c["cost"]["total_usd"].as_f64().unwrap_or(f64::NAN),
                            tape_m,
                            tape_m / FEATHER_M2_TAPE_M_APPROX,
                            record["search_status"],
                        );
                    }
                    None => println!("L={l:.3} R={r:.3}: no optimum"),
                }
            }
        }
    }
}
