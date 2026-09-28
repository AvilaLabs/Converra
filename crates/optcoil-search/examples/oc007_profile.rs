//! OC-008 contract §2 B7 (measurement only, no optimization, no kernel
//! change beyond Stage K): kernel evaluations per sampled point of the
//! OC-007 primary run's selected optimum (120 turns x 4 tapes), broken down
//! by turn index (near-face vs interior) and by station.
//!
//! `SearchCandidateResult`/`CoupledSearchRunRecord` (OC-007's own types,
//! required unchanged by the contract) never retain a per-point kernel
//! evaluation count -- only an aggregate total per candidate -- so this
//! reconstructs the optimum's exact geometry and sampling plan and calls
//! `optcoil_physics::racetrack::RacetrackEvaluator` directly at each point,
//! the same way `evaluate_one_candidate`/`run_coupled_case` already do
//! internally, purely to capture the per-point granularity those run
//! records do not themselves keep. This is not the OC-007/OC-008 search
//! itself: no material/Ic interpolation, no screening decision and no cost
//! computation happens here, only field-kernel evaluation counts.
//!
//! Usage: `cargo run --release --offline -p optcoil-search --example
//! oc007_profile -- <output.json>`.

use std::{env, fs};

use optcoil_model::{
    coupled::{Pack, Winding, tape_center_position_m, width_offset_m},
    coupled_search::{CoupledSearchCase, expand_relative_turn_indices},
    magnetics::{CurrentModel, Racetrack},
};
use optcoil_physics::{racetrack::RacetrackEvaluator, tape_frame};
use serde::Serialize;

/// OC-007's own reported optimum (docs/OC007.md: "Optimum: index 2, 120
/// turns x 4 tapes"; `runs/oc-007-2026-09-10.json`, `best_index: 2`,
/// `candidates[2].geometry`). Hardcoded from that already-frozen,
/// already-documented result rather than re-run here.
const OPTIMUM_TURNS: u32 = 120;
const OPTIMUM_TAPES: u32 = 4;

/// From `docs/OC004.md`'s kernel-revision section: the near-face graded
/// quadrature panels were observed to matter for tape centers within
/// roughly 0.05-1.05 mm of a radial pack face. Used here only to label
/// points for the table, not as a physics threshold this program enforces.
const NEAR_FACE_THRESHOLD_M: f64 = 0.00105;

#[derive(Serialize)]
struct PointRecord {
    station: String,
    turn_index: u32,
    tape_index: u32,
    width_index: u32,
    distance_to_nearest_radial_face_m: f64,
    near_face: bool,
    kernel_evaluations: u64,
}

#[derive(Serialize)]
struct TurnStationCell {
    station: String,
    turn_index: u32,
    distance_to_nearest_radial_face_m: f64,
    near_face: bool,
    points: u32,
    kernel_evaluations_total: u64,
    kernel_evaluations_per_point: f64,
}

#[derive(Serialize)]
struct StationSummary {
    station: String,
    points: u32,
    kernel_evaluations_total: u64,
    kernel_evaluations_per_point: f64,
}

#[derive(Serialize)]
struct NearFaceSummary {
    class: String,
    points: u32,
    kernel_evaluations_total: u64,
    kernel_evaluations_per_point: f64,
}

#[derive(Serialize)]
struct ProfileReport {
    note: String,
    optimum_turns_along_normal: u32,
    optimum_tapes_along_width: u32,
    quadrature_orders: [u32; 2],
    total_points: usize,
    total_kernel_evaluations: u64,
    near_face_threshold_m: f64,
    reference_run_record: String,
    reference_full_points_evaluated: u64,
    reference_full_kernel_evaluations_pre_stage_k: u64,
    kernel_version_note: String,
    by_turn_and_station: Vec<TurnStationCell>,
    by_station: Vec<StationSummary>,
    by_near_face_class: Vec<NearFaceSummary>,
    points: Vec<PointRecord>,
}

fn main() {
    let output_path = env::args()
        .nth(1)
        .expect("usage: oc007_profile <output.json>");

    let search = CoupledSearchCase::embedded().expect("embedded OC-007 case parses");

    let radial_pitch_m = search.fixed_geometry.radial_pitch_m;
    let radial_width_m = f64::from(OPTIMUM_TURNS) * radial_pitch_m;
    let axial_height_m = f64::from(OPTIMUM_TAPES) * search.fixed_geometry.tape_width_m;
    let bend_radius_m = search
        .fixed_geometry
        .bend_radius_m
        .expect("OC-007 is a racetrack case");
    let inner_rho_m = bend_radius_m - radial_width_m / 2.0;
    let outer_rho_m = bend_radius_m + radial_width_m / 2.0;

    let racetrack = Racetrack {
        straight_half_length_m: search
            .fixed_geometry
            .straight_half_length_m
            .expect("OC-007 is a racetrack case"),
        bend_radius_m,
        radial_width_m,
        axial_height_m,
        ampere_turns_a: 1.0,
        current_model: CurrentModel::UniformWindingPack,
    };
    let orders = search.numerics.quadrature_orders;
    let evaluators: Vec<RacetrackEvaluator> = orders
        .iter()
        .map(|&o| RacetrackEvaluator::new(&racetrack, o).expect("evaluator builds"))
        .collect();

    let pack = Pack {
        straight_half_length_m: search.fixed_geometry.straight_half_length_m,
        bend_radius_m: search.fixed_geometry.bend_radius_m,
        radial_width_m,
        axial_height_m,
        path: None,
        path3d: None,
    };
    let winding = Winding {
        tape_width_m: search.fixed_geometry.tape_width_m,
        tape_normal: search.fixed_geometry.tape_normal,
        turns_along_normal: OPTIMUM_TURNS,
        tapes_along_width: OPTIMUM_TAPES,
        strands_parallel: 1,
        regions: None,
    };

    let turn_indices =
        expand_relative_turn_indices(&search.sampling.relative_turn_indices, OPTIMUM_TURNS);
    let tape_indices: Vec<u32> = (1..=OPTIMUM_TAPES).collect();
    let nodes = tape_frame::lobatto_nodes();

    let mut points: Vec<PointRecord> = Vec::new();
    let mut total_kernel_evaluations: u64 = 0;

    for station in &search.sampling.stations {
        let frame = station
            .frame(search.fixed_geometry.tape_normal, pack.centerline())
            .expect("station frame resolves");
        for &turn_index in &turn_indices {
            let rho_m = inner_rho_m + (f64::from(turn_index) - 0.5) * radial_pitch_m;
            let distance_to_nearest_radial_face_m = (rho_m - inner_rho_m).min(outer_rho_m - rho_m);
            let near_face = distance_to_nearest_radial_face_m <= NEAR_FACE_THRESHOLD_M;
            for &tape_index in &tape_indices {
                let center =
                    tape_center_position_m(&pack, &winding, station, tape_index, turn_index)
                        .expect("tape center resolves");
                for (width_index, &xi) in nodes.iter().enumerate() {
                    let offset = width_offset_m(winding.tape_width_m, xi);
                    let position = [
                        center[0] + frame.w[0] * offset,
                        center[1] + frame.w[1] * offset,
                        center[2] + frame.w[2] * offset,
                    ];
                    let mut kernel_evaluations = 0_u64;
                    for evaluator in &evaluators {
                        let value = evaluator.evaluate(position).expect("evaluation succeeds");
                        kernel_evaluations += value.kernel_evaluations;
                    }
                    total_kernel_evaluations += kernel_evaluations;
                    points.push(PointRecord {
                        station: station.id().to_owned(),
                        turn_index,
                        tape_index,
                        width_index: width_index as u32,
                        distance_to_nearest_radial_face_m,
                        near_face,
                        kernel_evaluations,
                    });
                }
            }
        }
    }

    // Aggregate by (station, turn_index): sum over tape_index and width_index.
    let mut by_turn_and_station: Vec<TurnStationCell> = Vec::new();
    for station in &search.sampling.stations {
        for &turn_index in &turn_indices {
            let rows: Vec<&PointRecord> = points
                .iter()
                .filter(|p| p.station == station.id() && p.turn_index == turn_index)
                .collect();
            let n = rows.len() as u32;
            let total: u64 = rows.iter().map(|p| p.kernel_evaluations).sum();
            by_turn_and_station.push(TurnStationCell {
                station: station.id().to_owned(),
                turn_index,
                distance_to_nearest_radial_face_m: rows[0].distance_to_nearest_radial_face_m,
                near_face: rows[0].near_face,
                points: n,
                kernel_evaluations_total: total,
                kernel_evaluations_per_point: total as f64 / f64::from(n),
            });
        }
    }

    let mut by_station: Vec<StationSummary> = Vec::new();
    for station in &search.sampling.stations {
        let rows: Vec<&PointRecord> = points
            .iter()
            .filter(|p| p.station == station.id())
            .collect();
        let n = rows.len() as u32;
        let total: u64 = rows.iter().map(|p| p.kernel_evaluations).sum();
        by_station.push(StationSummary {
            station: station.id().to_owned(),
            points: n,
            kernel_evaluations_total: total,
            kernel_evaluations_per_point: total as f64 / f64::from(n),
        });
    }

    let mut by_near_face_class: Vec<NearFaceSummary> = Vec::new();
    for (class, near_face) in [("near_face", true), ("interior", false)] {
        let rows: Vec<&PointRecord> = points.iter().filter(|p| p.near_face == near_face).collect();
        let n = rows.len() as u32;
        let total: u64 = rows.iter().map(|p| p.kernel_evaluations).sum();
        by_near_face_class.push(NearFaceSummary {
            class: class.into(),
            points: n,
            kernel_evaluations_total: total,
            kernel_evaluations_per_point: if n > 0 {
                total as f64 / f64::from(n)
            } else {
                0.0
            },
        });
    }

    let report = ProfileReport {
        note: "OC-008 contract B7 (measurement only, no optimization, no kernel change beyond Stage K). Computed by independently re-evaluating the OC-007 primary optimum's (120x4) exact sampling grid through RacetrackEvaluator directly -- OC-007's own run record never retains a per-point kernel-evaluation breakdown, only an aggregate total per candidate. This repository's kernel was already at v3 (OC-008 contract Stage K, metric-consistent transverse grading) when this measurement ran, so total_kernel_evaluations here is NOT expected to match the pre-Stage-K figure recorded in runs/oc-007-2026-09-10.json (reference_full_kernel_evaluations_pre_stage_k below) -- see kernel_version_note.".into(),
        optimum_turns_along_normal: OPTIMUM_TURNS,
        optimum_tapes_along_width: OPTIMUM_TAPES,
        quadrature_orders: orders,
        total_points: points.len(),
        total_kernel_evaluations,
        near_face_threshold_m: NEAR_FACE_THRESHOLD_M,
        reference_run_record: "runs/oc-007-2026-09-10.json (candidates[2], the 120x4 optimum)".into(),
        reference_full_points_evaluated: 1120,
        reference_full_kernel_evaluations_pre_stage_k: 1_918_934_784,
        kernel_version_note: "runs/oc-007-2026-09-10.json predates the OC-008 contract's Stage K kernel v3 fix (metric-consistent transverse grading ratio on curved cells); Stage K measured a ~20% kernel-evaluation reduction on OC-004's own 900-point run (1,659,872,448 -> 1,324,975,392) from the identical fix, so a similar reduction here versus the pre-Stage-K total is expected and not a discrepancy. The main session re-runs OC-007's primary case under kernel v3 separately; this program was not run against that re-run (it did not exist yet), only against the pre-Stage-K record's own known candidate identity and point count.".into(),
        by_turn_and_station,
        by_station,
        by_near_face_class,
        points,
    };

    fs::write(&output_path, serde_json::to_vec_pretty(&report).unwrap())
        .expect("writing the profile report");
    eprintln!(
        "oc007_profile: wrote {output_path} ({} points, {} total kernel evaluations)",
        report.total_points, report.total_kernel_evaluations
    );
}
