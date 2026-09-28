//! OC-009 geometry scan: real screened coupled searches over an
//! (straight_half_length, bend_radius) grid on case schema v3.
//!
//! Every cell takes the OC-007 case, frees the outline to (l, r), declares
//! the same 10 cm usable volume (`good_field_region`), a manufacturing
//! inner-bend-radius floor, and a pack-choice grid covering the ceiling
//! study's predicted optimum, then runs the real
//! `run_coupled_search_case` -- pruning, screening, acceptance and all.
//! Each cell's complete run record is written to `runs/oc009-grid/`;
//! cells with an existing record are skipped, so the scan is resumable.
//!
//! This is a study harness, not a benchmark: the costs are the OC-007
//! synthetic ledger and the result is modeled, not engineering evidence.
//!
//! `cargo run --release -p optcoil-search --example oc009_grid`

use optcoil_search::coupled_search::{CoupledSearchOptions, run_coupled_search_case};

/// The frozen OC-007 baseline's modeled cost (benchmarks/coupled/oc-007.json,
/// 200x3 = $50,541.41 under the synthetic ledger). Used only to express
/// each cell's optimum as a fraction of the incumbent -- the case's own
/// declared baseline remains the within-cell reference.
const OC007_BASELINE_USD: f64 = 50_541.41;

/// The spec the first scan ran: a +-5 cm usable cube. Other sizes scale the
/// (L, R) grid off the region's in-plane corner rho = h*sqrt(2) so the same
/// relative surface is probed; see `grid_for`.
const DEFAULT_HALF_EXTENT_M: f64 = 0.05;

/// Per-cell pack grid: total turns n*p must span both the small-spec
/// optimum (~240 turns at h=0.05) and the larger NI a bigger usable volume
/// or wider bends demand. 40-turn packs screened FAIL at every recorded
/// cell, so the range starts at 80.
const TURNS: &[u32] = &[80, 120, 160, 240, 320, 400];
const TAPES: &[u32] = &[1, 2, 3, 4];

/// Two cells at a time; each cell's own search is capped at THREADS.
const CELLS_IN_FLIGHT: usize = 2;
const THREADS_PER_CELL: u32 = 4;

/// (straight_half_length, bend_radius) cells for a usable-volume half extent
/// `h`. The original h=0.05 scan keeps its absolute grid so its recorded
/// cells stay valid; any other spec scales off the corner radius so a coil
/// can physically clear the region (inner pack face must exceed h*sqrt(2))
/// while still probing near-floor and relaxed shapes.
fn grid_for(h: f64) -> (Vec<f64>, Vec<f64>) {
    if (h - DEFAULT_HALF_EXTENT_M).abs() < 1e-9 {
        return (vec![0.05, 0.075, 0.10, 0.125], vec![0.09, 0.11, 0.13]);
    }
    let corner = h * std::f64::consts::SQRT_2;
    (
        vec![1.0 * h, 1.5 * h, 2.0 * h, 2.5 * h],
        vec![1.3 * corner, 1.55 * corner, 1.85 * corner],
    )
}

fn case_json(h: f64, l: f64, r: f64) -> String {
    let mut case: serde_json::Value =
        serde_json::from_str(optcoil_model::coupled_search::OC007_JSON).expect("OC-007 parses");
    case["schema"] = "optcoil-coupled-search/v3".into();
    case["id"] = format!("oc009-grid-L{l:.3}-R{r:.3}").into();
    case["provenance"] = format!(
        "OC-009 geometry-scan cell (synthetic study, not a frozen benchmark): OC-007 physics and \
         cost model with the outline freed to L={l} m, R={r} m. Declares the schema-v3 usable \
         volume (+-{h} m cube about the bore probe, 3x3x3 lattice) and a manufacturing limit \
         (min_inner_bend_radius_m 0.05 -- a conservative stand-in; measured easy-way REBCO bend \
         limits are ~5 mm). The case's own baseline (largest pack on the grid) is the within-cell \
         reference; the OC-007 incumbent is applied externally when expressing savings."
    )
    .into();
    case["requirement"]["good_field_region"] = serde_json::json!({
        "half_extents_m": [h, h, h],
        "points_per_axis": 3
    });
    case["fixed_geometry"]["straight_half_length_m"] = l.into();
    case["fixed_geometry"]["bend_radius_m"] = r.into();
    case["choices"]["turns_along_normal"] = serde_json::json!(TURNS.to_vec());
    case["choices"]["tapes_along_width"] = serde_json::json!(TAPES.to_vec());
    case["baseline"] = serde_json::json!({"turns_along_normal": 400, "tapes_along_width": 4});
    // Straight stations must satisfy |x| <= l; scale OC-007's layout
    // (junction probe at ~0.9 l, refined straights at ~0.4/0.8 l).
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
    // v3 requires a refinement block (used by coupled-refine, not by this
    // search); brackets stay plausible around the predicted optima.
    case["refinement"] = serde_json::json!({
        "pancake_counts": [1, 2, 3, 4],
        "turn_resolution": 5,
        "turn_bounds": {"min": 1, "max": 400},
        "brackets": [
            {"tapes": 1, "fail_turns": 60, "pass_turns": 300},
            {"tapes": 2, "fail_turns": 40, "pass_turns": 200},
            {"tapes": 3, "fail_turns": 20, "pass_turns": 140},
            {"tapes": 4, "fail_turns": 20, "pass_turns": 110},
        ],
        "monotonicity_check": true
    });
    case["manufacturing"] = serde_json::json!({"min_inner_bend_radius_m": 0.05});
    serde_json::to_string(&case).expect("serializable")
}

fn main() {
    // argv[1]: usable-volume half extent in metres (default 0.05).
    let h: f64 = std::env::args()
        .nth(1)
        .map(|a| a.parse().expect("argv[1] is the region half extent in m"))
        .unwrap_or(DEFAULT_HALF_EXTENT_M);
    let out_dir = std::path::PathBuf::from(if (h - DEFAULT_HALF_EXTENT_M).abs() < 1e-9 {
        "runs/oc009-grid".to_string()
    } else {
        format!("runs/oc009-grid-h{h:.3}")
    });
    std::fs::create_dir_all(&out_dir).expect("create runs/oc009-grid*");
    let (mut grid_l, mut grid_r) = grid_for(h);
    // argv[2]/argv[3]: optional comma-separated L and R subsets (e.g.
    // "0.161" runs only the minimum-R column -- the observed optimum row).
    if let Some(sel) = std::env::args().nth(2).filter(|s| !s.is_empty()) {
        let keep: Vec<f64> = sel
            .split(',')
            .map(|s| s.parse().expect("argv[2] L subset"))
            .collect();
        grid_l.retain(|l| keep.iter().any(|k| (l - k).abs() < 5e-4));
    }
    if let Some(sel) = std::env::args().nth(3).filter(|s| !s.is_empty()) {
        let keep: Vec<f64> = sel
            .split(',')
            .map(|s| s.parse().expect("argv[3] R subset"))
            .collect();
        grid_r.retain(|r| keep.iter().any(|k| (r - k).abs() < 5e-4));
    }
    println!("spec: +-{h} m cube; L grid {grid_l:?}, R grid {grid_r:?}; -> {out_dir:?}");

    let cells: Vec<(f64, f64)> = grid_l
        .iter()
        .flat_map(|&l| grid_r.iter().map(move |&r| (l, r)))
        .filter(|(l, r)| !out_dir.join(format!("run-L{l:.3}-R{r:.3}.json")).exists())
        .collect();
    if cells.is_empty() {
        println!("All cells already have records -- nothing to do.");
    }

    // Pull cells off a shared queue, two searches in flight.
    let next = &std::sync::atomic::AtomicUsize::new(0);
    let cells_ref = &cells;
    let out_dir_ref = &out_dir;
    std::thread::scope(|scope| {
        for _ in 0..CELLS_IN_FLIGHT {
            scope.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&(l, r)) = cells_ref.get(i) else { break };
                let path = out_dir_ref.join(format!("run-L{l:.3}-R{r:.3}.json"));
                println!("cell L={l:.3} R={r:.3}: running...");
                let json = case_json(h, l, r);
                match run_coupled_search_case(
                    &json,
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
                        let best = record
                            .best_index
                            .map(|b| &record.candidates[b]);
                        match best {
                            Some(b) => println!(
                                "cell L={l:.3} R={r:.3}: best {}x{} ${:.0} ({:.1}% of OC-007 baseline), status {:?}, acceptance {:?}",
                                b.geometry.turns_along_normal,
                                b.geometry.tapes_along_width,
                                b.cost.total_usd,
                                100.0 * b.cost.total_usd / OC007_BASELINE_USD,
                                record.search_status,
                                record.acceptance.agreement_status,
                            ),
                            None => println!(
                                "cell L={l:.3} R={r:.3}: no optimum (status {:?})",
                                record.search_status
                            ),
                        }
                    }
                    Err(e) => println!("cell L={l:.3} R={r:.3}: RUN ERROR {e:?}"),
                }
            });
        }
    });

    // Summary across all recorded cells (including pre-existing ones).
    println!("\nL \\ R            {}", {
        let mut s = String::new();
        for &r in &grid_r {
            s.push_str(&format!("R={r:.3}      "));
        }
        s
    });
    for &l in &grid_l {
        let mut line = format!("L={l:.3}  ");
        for &r in &grid_r {
            let path = out_dir.join(format!("run-L{l:.3}-R{r:.3}.json"));
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let record: serde_json::Value =
                        serde_json::from_str(&text).expect("record parses");
                    match record["best_index"].as_u64() {
                        Some(b) => {
                            let cost = record["candidates"][b as usize]["cost"]["total_usd"]
                                .as_f64()
                                .unwrap_or(f64::NAN);
                            line.push_str(&format!(
                                "${:>7.0} {:>4.1}% ",
                                cost,
                                100.0 * cost / OC007_BASELINE_USD
                            ));
                        }
                        None => line.push_str("  (no best)  "),
                    }
                }
                Err(_) => line.push_str("   (missing)  "),
            }
        }
        println!("{line}");
    }
    println!(
        "\nAll figures: modeled cost under OC-007's synthetic ledger; % is vs the OC-007 baseline ${OC007_BASELINE_USD:.0}."
    );
}
