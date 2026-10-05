//! Tests of the allocation run and its independent check (CR-03 slice B).
//! Hand-built demand tables and inventories with known answers, plus a
//! binding scenario on a real slice-A demand.

use std::sync::OnceLock;

use serde_json::{Value, json};

use optcoil_model::Status;

use crate::{
    allocation::{DemandOptions, build_allocation_demand},
    allocation_check::check_allocation_json,
    allocation_run::{
        AllocationRecord, BaselineReelStatus, ReelCode, ReelUse, StreamStatus, run_allocation_json,
    },
    coupled_search::{CoupledSearchOptions, run_coupled_search_case, tests::reduced_case_json},
    synthetic::synthesize_inventory_json,
};

pub(crate) const MAP_ID: &str = "robinson-superpower-ap-v3";
pub(crate) const MAP_SHA: &str = "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354";
const WIDTH: f64 = 0.012;
/// The embedded map's Ic per width at the profile condition below (rows
/// 1817 and 1818), so a profile value of `s * PROFILE_MAP_A` is a scale
/// factor of `s`.
const PROFILE_MAP_A: f64 = 293_111.0 * WIDTH;

/// `(length_m, per-module (offset_sensitive, s_req per tabulated offset))`.
type TurnSpec = (f64, Vec<(bool, Vec<Option<f64>>)>);

/// A hand-built demand: `turns` are `(length_m, per-module list of
/// (offset_sensitive, s_req per tabulated offset))`.
pub(crate) fn demand_json(offsets: &[f64], strands: u32, turns: &[TurnSpec]) -> String {
    let mut start = 0.0_f64;
    let mut turn_values = Vec::new();
    for (index, (length, modules)) in turns.iter().enumerate() {
        let module_values: Vec<Value> = modules
            .iter()
            .enumerate()
            .map(|(m, (sensitive, s_req))| {
                let entries: Vec<Value> = s_req
                    .iter()
                    .zip(offsets)
                    .map(|(s, d)| match s {
                        Some(s) => json!({"offset_deg": d, "s_req": s, "status": "ok"}),
                        None => json!({
                            "offset_deg": d, "s_req": null, "status": "outside_map_domain",
                            "explanation": "test block", "next_step": "test"
                        }),
                    })
                    .collect();
                json!({
                    "module": m + 1,
                    "offset_sensitive": sensitive,
                    "status": if s_req[0].is_some() { "ok" } else { "outside_map_domain" },
                    "s_req": entries
                })
            })
            .collect();
        turn_values.push(json!({
            "turn": index + 1, "length_m": length, "start_m": start,
            "offset_sensitive": modules.iter().any(|m| m.0),
            "status": "ok", "modules": module_values
        }));
        start += length;
    }
    let value = json!({
        "schema": "optcoil-allocation-demand/v1",
        "optcoil_version": "test", "model_id": "test", "ic_model_id": "test",
        "implementation_sha256": "00",
        "identities": {
            "record_sha256": "11", "case_sha256": "22", "search_case_id": "hand",
            "record_search_status": "PASS", "optimum_index": 0,
            "product_map": {
                "dataset_id": MAP_ID, "csv_sha256": MAP_SHA, "source": "embedded",
                "data_class": "measured", "interpolation_method": "test",
                "low_field_clamp_t": 1.001
            },
            "screened_materials": [{"dataset_id": MAP_ID, "csv_sha256": MAP_SHA}],
            "map_substituted": false
        },
        "geometry": {
            "turns_along_normal": turns.len(), "modules": turns[0].1.len(),
            "strands": strands, "streams": turns[0].1.len() as u32 * strands,
            "tape_width_m": WIDTH, "tape_normal": "radial", "stream_length_m": start,
            "operating_current_a": 100.0, "operating_temperature_k": 20.0,
            "electric_field_criterion_v_per_m": 1e-4, "stations": ["s0"]
        },
        "utilization_limit": 0.8, "budget": 0.1,
        "angle_offsets_deg": offsets, "offset_window_deg": 15.0,
        "gate": {"status": "PASS", "relative_tolerance": 1e-12, "tapes_compared": 1, "max_relative_difference": 0.0},
        "ledger_check": {"installed_length_ledger_m": start, "installed_length_from_turns_m": start, "relative_difference": 0.0},
        "turns": turn_values,
        "summary": {"peak_s_req": [], "blocked_positions": 0, "offset_sensitive_turns": 0},
        "limitations": []
    });
    serde_json::to_string_pretty(&value).unwrap()
}

/// One module, no strands beyond 1; same requirement for every offset.
pub(crate) fn flat_demand(turns: &[(f64, f64)]) -> String {
    let rows: Vec<_> = turns
        .iter()
        .map(|(length, s)| (*length, vec![(false, vec![Some(*s), Some(*s)])]))
        .collect();
    demand_json(&[0.0, 2.0], 1, &rows)
}

pub(crate) struct Reel {
    pub(crate) id: &'static str,
    pub(crate) length: f64,
    /// `(position_m, s)` profile points.
    pub(crate) points: Vec<(f64, f64)>,
    pub(crate) defects: Vec<(f64, f64, &'static str)>,
    pub(crate) offsets: Vec<(f64, f64)>,
}

impl Reel {
    pub(crate) fn flat(id: &'static str, length: f64, s: f64) -> Self {
        Reel {
            id,
            length,
            points: vec![(0.0, s), (length, s)],
            defects: vec![],
            offsets: vec![],
        }
    }
}

pub(crate) fn inventory_json(reels: &[Reel]) -> String {
    let passports: Vec<Value> = reels
        .iter()
        .map(|r| {
            json!({
                "schema": "optcoil-reel-passport/v1",
                "reel_id": r.id,
                "evidence_class": "synthetic",
                "product": {
                    "vendor": "v", "product": "p", "batch": null,
                    "product_map": {"dataset_id": MAP_ID, "csv_sha256": MAP_SHA}
                },
                "geometry": {"length_m": r.length, "width_m": WIDTH},
                "length_profiles": [{
                    "id": "p1", "method": "test", "evidence_class": "synthetic",
                    "temperature_k": 19.98, "field_t": 1.0, "angle_from_normal_deg": -0.64,
                    "electric_field_criterion_v_per_m": 1e-4, "resolution_m": 1.0,
                    "source_sha256": null,
                    "points": r.points.iter().map(|(x, s)| json!([x, s * PROFILE_MAP_A])).collect::<Vec<_>>()
                }],
                "in_field_points": [],
                "ab_offsets": r.offsets.iter().map(|(o, u)| json!({
                    "position_m": 0.0, "offset_deg": o, "uncertainty_deg": u,
                    "method": "xrd_rocking_curve", "evidence_class": "synthetic"
                })).collect::<Vec<_>>(),
                "defects": r.defects.iter().map(|(a, b, action)| json!({
                    "start_m": a, "end_m": b, "kind": "dropout", "action": action,
                    "min_ic_fraction": null, "note": "test"
                })).collect::<Vec<_>>(),
                "provenance": {"issuer": "test", "issued_at": "2026-10-04", "sources": []},
                "limitations": []
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "schema": "optcoil-reel-inventory/v1",
        "inventory_id": "hand",
        "evidence_class": "synthetic",
        "passports": passports,
        "limitations": []
    }))
    .unwrap()
}

pub(crate) fn params_json(margin: f64, sigma: f64, z: f64, min_piece: f64, price: f64) -> String {
    json!({
        "schema": "optcoil-allocation-params/v1",
        "margin": margin,
        "transfer_derate": {"sigma": sigma, "z": z},
        "min_piece_length_m": min_piece,
        "price_usd_per_m": price,
        "price_source": "test price list"
    })
    .to_string()
}

pub(crate) fn run(demand: &str, inventory: &str, params: &str) -> AllocationRecord {
    run_allocation_json(demand, inventory, params, &[]).unwrap()
}

pub(crate) fn assert_check_passes(demand: &str, inventory: &str, record: &AllocationRecord) {
    let text = record.to_json().unwrap();
    let check = check_allocation_json(demand, inventory, &text, &[]).unwrap();
    assert_eq!(check.verdict, Status::Pass, "{:#?}", check.mismatches());
}

fn check_fails(demand: &str, inventory: &str, record: &Value, needle: &str) {
    let text = serde_json::to_string_pretty(record).unwrap();
    let check = check_allocation_json(demand, inventory, &text, &[]).unwrap();
    assert_eq!(check.verdict, Status::Fail, "tampered record passed");
    assert!(
        check.mismatches().iter().any(|m| m.contains(needle)),
        "no mismatch mentions '{needle}': {:#?}",
        check.mismatches()
    );
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "{a} vs {b}");
}

/// The hand-computed example. One stream of three 10 m turns needing s_req
/// 0.3, 0.8, 0.3. Reel H (15 m, s = 0.9) and reel M (40 m, s = 0.5).
///
/// Greedy, margin 0, minimum piece 5 m:
/// - head 0: H runs 15 m (turn 1 and half of turn 2), mean surplus
///   (10 x 3 + 5 x 1.125) / 15 = 2.375; M runs 10 m (it fails turn 2), mean
///   surplus 0.5 / 0.3 = 1.667. Both reach the 5 m minimum; the smaller mean
///   surplus wins: M 0 to 10 -> stream 0 to 10.
/// - head 10: M fails turn 2; H runs 15 m -> reel 0 to 15, stream 10 to 25.
/// - head 25: M (head 10) runs the last 5 m -> reel 10 to 15, stream 25 to 30.
///
/// 3 pieces, 2 splices, 30 m allocated, no shortfall. Baseline: the uniform
/// requirement is 0.8, so H is accepted, M is scrap (40 m); H covers 15 m and
/// 15 m is short. At 10 USD/m the allocation saves 150 USD.
fn hand_example() -> (String, String, AllocationRecord) {
    let demand = flat_demand(&[(10.0, 0.3), (10.0, 0.8), (10.0, 0.3)]);
    let inventory = inventory_json(&[Reel::flat("H", 15.0, 0.9), Reel::flat("M", 40.0, 0.5)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 10.0));
    (demand, inventory, record)
}

#[test]
fn hand_computed_greedy_matches_and_the_checker_passes() {
    let (demand, inventory, record) = hand_example();
    assert_eq!(record.streams.len(), 1);
    let stream = &record.streams[0];
    assert_eq!(stream.status, StreamStatus::Covered);
    let got: Vec<(&str, f64, f64, f64, f64, u32, u32)> = stream
        .pieces
        .iter()
        .map(|p| {
            (
                p.reel_id.as_str(),
                p.reel_from_m,
                p.reel_to_m,
                p.stream_from_m,
                p.stream_to_m,
                p.turn_from,
                p.turn_to,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("M", 0.0, 10.0, 0.0, 10.0, 1, 1),
            ("H", 0.0, 15.0, 10.0, 25.0, 2, 3),
            ("M", 10.0, 15.0, 25.0, 30.0, 3, 3),
        ]
    );
    // Minimum margin ratios: M 0.5/0.3, H min(0.9/0.8, 0.9/0.3), M 0.5/0.3.
    close(stream.pieces[0].min_margin_ratio, 0.5 / 0.3);
    close(stream.pieces[1].min_margin_ratio, 0.9 / 0.8);
    assert_eq!(
        (record.allocation.pieces, record.allocation.splices),
        (3, 2)
    );
    close(record.allocation.allocated_m, 30.0);
    close(record.allocation.shortfall_m, 0.0);
    // M keeps 25 m of its 40 m.
    close(record.allocation.remnant_m, 55.0 - 30.0);
    let m = record.reels.iter().find(|r| r.reel_id == "M").unwrap();
    assert_eq!(m.usage, ReelUse::PartlyUsed);
    assert_eq!(m.baseline, BaselineReelStatus::Rejected);
    assert!(m.scrapped_by_baseline_but_used);
    let h = record.reels.iter().find(|r| r.reel_id == "H").unwrap();
    assert_eq!(h.usage, ReelUse::Used);
    assert_eq!(h.baseline, BaselineReelStatus::Accepted);
    close(h.baseline_s_req.unwrap(), 0.8);
    // Baseline.
    close(record.baseline.allocated_m, 15.0);
    close(record.baseline.shortfall_m, 15.0);
    close(record.baseline.scrap_m, 40.0);
    assert_eq!(record.baseline.pieces, 1);
    assert_eq!(record.reel_counts.scrapped_by_baseline_but_used, 1);
    // Money: (15 - 0) m x 10 USD/m.
    close(record.money.difference_usd, 150.0);
    assert_eq!(record.money.price_source, "test price list");
    assert!(record.limitations.iter().any(|l| l.starts_with("UNSAFE")));
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn output_is_deterministic_pretty_json_with_a_trailing_newline() {
    let (demand, inventory, record) = hand_example();
    let again = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 10.0));
    let (a, b) = (record.to_json().unwrap(), again.to_json().unwrap());
    assert_eq!(a, b);
    assert!(a.ends_with("}\n") && a.contains("\n  \"schema\""));
    assert_eq!(record.inputs.demand_sha256, crate::hash(demand.as_bytes()));
    assert_eq!(
        record.inputs.inventory_sha256,
        optcoil_model::reel::passport_sha256(inventory.as_bytes())
    );
}

#[test]
fn minimum_piece_length_steers_the_choice_and_infeasible_at_is_reported() {
    // With a 12 m minimum, M's 10 m run no longer qualifies at head 0, so H
    // (15 m) is taken first; then nothing can serve turn 2 at 15 m.
    let demand = flat_demand(&[(10.0, 0.3), (10.0, 0.8), (10.0, 0.3)]);
    let inventory = inventory_json(&[Reel::flat("H", 15.0, 0.9), Reel::flat("M", 40.0, 0.5)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 12.0, 10.0));
    let stream = &record.streams[0];
    assert_eq!(stream.pieces.len(), 1);
    assert_eq!(stream.pieces[0].reel_id, "H");
    assert_eq!(stream.status, StreamStatus::InfeasibleAt);
    let at = stream.infeasible_at.as_ref().unwrap();
    assert_eq!(at.turn, 2);
    close(at.stream_m, 15.0);
    close(at.s_req.unwrap(), 0.8);
    close(at.best_s_used.unwrap(), 0.5);
    assert!(at.reason.starts_with("no_feasible_reel"));
    close(record.allocation.shortfall_m, 15.0);
    assert_eq!(record.allocation.streams_infeasible, 1);
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn a_dip_exactly_at_a_turn_boundary_ends_the_run_exactly_there() {
    // Reel steps: s = 1.0 on [0,10), 0.2 on [10,30). Turns need 0.5, 0.1,
    // 0.5 over 10 m each. The dip starts at the first turn boundary and the
    // second turn tolerates it; the third does not, so the run is 20 m.
    let demand = flat_demand(&[(10.0, 0.5), (10.0, 0.1), (10.0, 0.5)]);
    let reel = Reel {
        id: "D",
        length: 30.0,
        points: vec![(0.0, 1.0), (10.0, 1.0), (20.0, 0.2), (30.0, 1.0)],
        defects: vec![],
        offsets: vec![],
    };
    let inventory = inventory_json(&[reel]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let stream = &record.streams[0];
    assert_eq!(stream.pieces.len(), 1);
    assert_eq!(stream.pieces[0].stream_to_m, 20.0);
    assert_eq!(stream.pieces[0].reel_to_m, 20.0);
    assert_eq!(
        (stream.pieces[0].turn_from, stream.pieces[0].turn_to),
        (1, 2)
    );
    close(stream.pieces[0].min_margin_ratio, 2.0);
    let at = stream.infeasible_at.as_ref().unwrap();
    assert_eq!((at.turn, at.stream_m), (3, 20.0));
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn margin_and_derate_tighten_placement() {
    // s_req 0.5; reel s = 0.9. With margin 0.5 the threshold is 0.75: fine.
    // With derate exp(-z sigma) = exp(-0.5) = 0.6065 the reel is 0.546: a
    // margin of 0.2 (threshold 0.6) then fails everywhere.
    let demand = flat_demand(&[(10.0, 0.5), (10.0, 0.5)]);
    let inventory = inventory_json(&[Reel::flat("A", 40.0, 0.9)]);
    let ok = run(&demand, &inventory, &params_json(0.5, 0.0, 1.0, 5.0, 1.0));
    assert_eq!(ok.streams[0].status, StreamStatus::Covered);
    let derated = run(&demand, &inventory, &params_json(0.2, 0.5, 1.0, 5.0, 1.0));
    assert!((derated.derate_factor - (-0.5_f64).exp()).abs() < 1e-15);
    assert_eq!(derated.streams[0].status, StreamStatus::InfeasibleAt);
    assert!(!derated.limitations.iter().any(|l| l.starts_with("UNSAFE")));
    assert_check_passes(&demand, &inventory, &derated);
    assert_check_passes(&demand, &inventory, &ok);
}

#[test]
fn excluded_and_cut_spans_are_skipped_and_accepted_spans_stay() {
    let demand = flat_demand(&[(25.0, 0.3)]);
    // 30 m reel, excluded 10 to 15: usable [0,10] and [15,30].
    let mut reel = Reel::flat("X", 30.0, 0.9);
    reel.defects = vec![(10.0, 15.0, "excluded")];
    let inventory = inventory_json(&[reel]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let x = &record.reels[0];
    close(x.usable_length_m, 25.0);
    let pieces = &record.streams[0].pieces;
    assert_eq!(pieces.len(), 2);
    for piece in pieces {
        assert!(
            piece.reel_to_m <= 10.0 || piece.reel_from_m >= 15.0,
            "{piece:?}"
        );
    }
    assert_eq!(x.usage, ReelUse::Used);
    assert_check_passes(&demand, &inventory, &record);

    // A cut span behaves the same.
    let mut cut = Reel::flat("C", 30.0, 0.9);
    cut.defects = vec![(10.0, 15.0, "cut")];
    let inventory_cut = inventory_json(&[cut]);
    let record_cut = run(
        &demand,
        &inventory_cut,
        &params_json(0.0, 0.0, 1.0, 5.0, 1.0),
    );
    close(record_cut.reels[0].usable_length_m, 25.0);
    assert_check_passes(&demand, &inventory_cut, &record_cut);

    // An accepted span stays in with its profile values: the dip to 0.2
    // inside it blocks the 0.3 requirement there, so the run stops.
    let reel = Reel {
        id: "A",
        length: 30.0,
        points: vec![
            (0.0, 0.9),
            (10.0, 0.9),
            (11.0, 0.2),
            (14.0, 0.2),
            (15.0, 0.9),
            (30.0, 0.9),
        ],
        defects: vec![(11.0, 14.0, "accepted")],
        offsets: vec![],
    };
    let inventory_accepted = inventory_json(&[reel]);
    let record_accepted = run(
        &demand,
        &inventory_accepted,
        &params_json(0.0, 0.0, 1.0, 5.0, 1.0),
    );
    close(record_accepted.reels[0].usable_length_m, 30.0);
    close(record_accepted.reels[0].min_s_used.unwrap(), 0.2);
    // Steps are the smaller endpoint: [10,11)=0.2, [11,14)=0.2, [14,15)=0.2.
    let first = &record_accepted.streams[0].pieces[0];
    assert_eq!((first.reel_from_m, first.reel_to_m), (0.0, 10.0));
    assert_check_passes(&demand, &inventory_accepted, &record_accepted);
}

#[test]
fn reels_without_offsets_are_kept_off_offset_sensitive_turns() {
    // Turn 2 is offset sensitive. Offsets tabulated 0 and 2 degrees; at 2
    // degrees turn 2 needs 0.6 (0.5 at 0).
    let rows = vec![
        (10.0, vec![(false, vec![Some(0.3), Some(0.3)])]),
        (10.0, vec![(true, vec![Some(0.5), Some(0.6)])]),
        (10.0, vec![(false, vec![Some(0.3), Some(0.3)])]),
    ];
    let demand = demand_json(&[0.0, 2.0], 1, &rows);
    let plain = Reel::flat("PLAIN", 30.0, 0.95);
    let mut offs = Reel::flat("OFFS", 30.0, 0.95);
    offs.offsets = vec![(1.0, 0.5)]; // bound 1.5 -> tabulated 2.0
    let inventory = inventory_json(&[plain, offs]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let offs_row = record.reels.iter().find(|r| r.reel_id == "OFFS").unwrap();
    assert_eq!(offs_row.delta_deg, Some(2.0));
    assert_eq!(offs_row.offset_bound_deg, Some(1.5));
    let plain_row = record.reels.iter().find(|r| r.reel_id == "PLAIN").unwrap();
    assert_eq!(plain_row.delta_deg, None);
    // PLAIN would be the "smaller surplus" tie-breaker by id, but it cannot
    // sit on turn 2, so every piece covering turn 2 comes from OFFS.
    for piece in &record.streams[0].pieces {
        if piece.turn_from <= 2 && piece.turn_to >= 2 {
            assert_eq!(piece.reel_id, "OFFS");
        }
    }
    assert_eq!(record.streams[0].status, StreamStatus::Covered);
    // Baseline: PLAIN cannot take turn 2 anywhere, so it is rejected.
    assert_eq!(plain_row.baseline, BaselineReelStatus::Rejected);
    assert_eq!(offs_row.baseline, BaselineReelStatus::Accepted);
    close(offs_row.baseline_s_req.unwrap(), 0.6);
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn a_bound_beyond_the_largest_offset_or_no_offsets_everywhere_is_unallocatable() {
    let rows = vec![(10.0, vec![(true, vec![Some(0.3), Some(0.3)])])];
    let demand = demand_json(&[0.0, 2.0], 1, &rows);
    let mut wide = Reel::flat("WIDE", 30.0, 0.95);
    wide.offsets = vec![(3.0, 0.5)];
    let inventory = inventory_json(&[Reel::flat("NONE", 30.0, 0.95), wide]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    for reel in &record.reels {
        assert_eq!(reel.code, ReelCode::OffsetSensitiveEverywhere);
        assert_eq!(reel.usage, ReelUse::Unallocatable);
        assert!(reel.explanation.is_some() && reel.next_evidence.is_some());
    }
    let at = record.streams[0].infeasible_at.as_ref().unwrap();
    assert!(at.reason.starts_with("inventory_exhausted"));
    assert_eq!(record.reel_counts.unallocatable, 2);
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn blocked_positions_stop_a_stream_and_are_reported() {
    let rows = vec![
        (10.0, vec![(false, vec![Some(0.3), Some(0.3)])]),
        (10.0, vec![(false, vec![None, None])]),
    ];
    let demand = demand_json(&[0.0, 2.0], 1, &rows);
    let inventory = inventory_json(&[Reel::flat("A", 50.0, 0.95)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let stream = &record.streams[0];
    assert!(stream.peak_s_req.is_none());
    let at = stream.infeasible_at.as_ref().unwrap();
    assert_eq!(at.turn, 2);
    assert!(at.s_req.is_none());
    assert!(at.reason.starts_with("blocked_position"));
    // The baseline cannot form a uniform requirement over a blocked turn.
    assert_eq!(record.reels[0].baseline, BaselineReelStatus::Rejected);
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn unallocatable_reasons_follow_cr02_and_the_contract() {
    let demand = flat_demand(&[(10.0, 0.3)]);
    let mut wrong_width: Value =
        serde_json::from_str(&inventory_json(&[Reel::flat("W", 30.0, 0.9)])).unwrap();
    wrong_width["passports"][0]["geometry"]["width_m"] = json!(0.0121);
    wrong_width["passports"][0]["length_profiles"][0]["points"][0][1] =
        json!(0.9 * 0.0121 * 293_111.0);
    let mut other_map = wrong_width.clone();
    other_map["passports"][0]["geometry"]["width_m"] = json!(WIDTH);
    other_map["passports"][0]["product"]["product_map"] =
        json!({"dataset_id": "robinson-superpower-ap-v3-lowfield", "csv_sha256": "0".repeat(64)});
    let mut no_profile: Value =
        serde_json::from_str(&inventory_json(&[Reel::flat("N", 30.0, 0.9)])).unwrap();
    no_profile["passports"][0]["length_profiles"] = json!([]);
    let mut cold: Value =
        serde_json::from_str(&inventory_json(&[Reel::flat("K", 30.0, 0.9)])).unwrap();
    cold["passports"][0]["length_profiles"][0]["temperature_k"] = json!(77.0);
    let mut no_map: Value =
        serde_json::from_str(&inventory_json(&[Reel::flat("Z", 30.0, 0.9)])).unwrap();
    no_map["passports"][0]["product"]["product_map"] = Value::Null;
    for (inv, code) in [
        (wrong_width, ReelCode::WidthMismatch),
        (other_map, ReelCode::MapMismatch),
        (no_profile, ReelCode::ProfileUnscalable),
        (cold, ReelCode::ProfileUnscalable),
        (no_map, ReelCode::NoProductMap),
    ] {
        let text = serde_json::to_string(&inv).unwrap();
        let record = run(&demand, &text, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
        assert_eq!(record.reels[0].code, code);
        assert!(record.reels[0].explanation.is_some());
        assert!(record.reels[0].next_evidence.is_some());
        assert_check_passes(&demand, &text, &record);
    }
}

#[test]
fn params_are_validated_and_the_derate_is_required() {
    let demand = flat_demand(&[(10.0, 0.3)]);
    let inventory = inventory_json(&[Reel::flat("A", 30.0, 0.9)]);
    let mut missing: Value = serde_json::from_str(&params_json(0.0, 0.1, 1.0, 5.0, 1.0)).unwrap();
    missing.as_object_mut().unwrap().remove("transfer_derate");
    for bad in [
        missing.to_string(),
        params_json(-0.1, 0.1, 1.0, 5.0, 1.0),
        params_json(0.0, -0.1, 1.0, 5.0, 1.0),
        params_json(0.0, 0.1, 1.0, 0.0, 1.0),
        params_json(0.0, 0.1, 1.0, 5.0, 0.0),
        params_json(0.0, 0.1, 1.0, 5.0, 1.0).replace("test price list", " "),
    ] {
        assert!(
            run_allocation_json(&demand, &inventory, &bad, &[]).is_err(),
            "{bad}"
        );
    }
    let safe = run(&demand, &inventory, &params_json(0.0, 0.15, 1.0, 5.0, 1.0));
    assert!(!safe.limitations.iter().any(|l| l.starts_with("UNSAFE")));
    let unsafe_run = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    assert!(
        unsafe_run
            .limitations
            .iter()
            .any(|l| l.starts_with("UNSAFE"))
    );
}

#[test]
fn streams_are_ordered_by_descending_peak_and_strands_share_a_module() {
    // Module 2 is the more demanding; two strands per module -> 4 streams.
    let rows = vec![
        (
            10.0,
            vec![
                (false, vec![Some(0.2), Some(0.2)]),
                (false, vec![Some(0.6), Some(0.6)]),
            ],
        ),
        (
            10.0,
            vec![
                (false, vec![Some(0.2), Some(0.2)]),
                (false, vec![Some(0.7), Some(0.7)]),
            ],
        ),
    ];
    let demand = demand_json(&[0.0, 2.0], 2, &rows);
    let inventory = inventory_json(&[Reel::flat("LOW", 40.0, 0.3), Reel::flat("HIGH", 40.0, 0.9)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let order: Vec<(u32, u32)> = record
        .streams
        .iter()
        .map(|s| (s.module, s.strand))
        .collect();
    assert_eq!(order, vec![(2, 1), (2, 2), (1, 1), (1, 2)]);
    // HIGH serves one demanding stream (20 m); LOW only suits module 1.
    assert_eq!(record.streams[0].status, StreamStatus::Covered);
    assert_eq!(record.streams[0].pieces[0].reel_id, "HIGH");
    assert_eq!(record.streams[1].status, StreamStatus::Covered);
    assert_eq!(record.streams[1].pieces[0].reel_id, "HIGH");
    assert_check_passes(&demand, &inventory, &record);
}

#[test]
fn the_checker_fails_every_kind_of_tampering() {
    let (demand, inventory, record) = hand_example();
    let base: Value = serde_json::from_str(&record.to_json().unwrap()).unwrap();

    // Overlap in the stream: the second piece starts early.
    let mut v = base.clone();
    v["streams"][0]["pieces"][1]["stream_from_m"] = json!(8.0);
    check_fails(&demand, &inventory, &v, "gap or overlap");

    // Reel reuse: the third piece takes reel M 5 to 10 again.
    let mut v = base.clone();
    v["streams"][0]["pieces"][2]["reel_from_m"] = json!(5.0);
    v["streams"][0]["pieces"][2]["reel_to_m"] = json!(10.0);
    check_fails(&demand, &inventory, &v, "used twice");

    // Infeasible piece: M stretched across turn 2.
    let mut v = base.clone();
    v["streams"][0]["pieces"][0]["reel_to_m"] = json!(20.0);
    v["streams"][0]["pieces"][0]["stream_to_m"] = json!(20.0);
    check_fails(&demand, &inventory, &v, "not feasible");

    // Wrong totals.
    let mut v = base.clone();
    v["allocation"]["splices"] = json!(5);
    check_fails(&demand, &inventory, &v, "splices");
    let mut v = base.clone();
    v["money"]["difference_usd"] = json!(151.0);
    check_fails(&demand, &inventory, &v, "money difference_usd");
    let mut v = base.clone();
    v["allocation"]["shortfall_m"] = json!(3.0);
    check_fails(&demand, &inventory, &v, "shortfall_m");

    // A gap: the last piece ends short without a status change.
    let mut v = base.clone();
    v["streams"][0]["pieces"][2]["stream_to_m"] = json!(28.0);
    v["streams"][0]["pieces"][2]["reel_to_m"] = json!(13.0);
    check_fails(&demand, &inventory, &v, "covered length");

    // A forged reel status and a forged baseline verdict.
    let mut v = base.clone();
    v["reels"][1]["usage"] = json!("used");
    check_fails(&demand, &inventory, &v, "usage");
    let mut v = base.clone();
    v["reels"][1]["baseline"] = json!("accepted");
    check_fails(&demand, &inventory, &v, "baseline status");

    // Hash binding: a different inventory.
    let mut v = base.clone();
    v["inputs"]["inventory_sha256"] = json!("00");
    check_fails(&demand, &inventory, &v, "inventory_sha256");

    // Baseline piece from a rejected reel.
    let mut v = base.clone();
    v["baseline_streams"][0]["pieces"][0]["reel_id"] = json!("M");
    check_fails(&demand, &inventory, &v, "rejected by the baseline");
}

#[test]
fn the_checker_fails_a_piece_inside_a_cut_span() {
    let demand = flat_demand(&[(25.0, 0.3)]);
    let mut reel = Reel::flat("X", 30.0, 0.9);
    reel.defects = vec![(10.0, 15.0, "cut")];
    let inventory = inventory_json(&[reel]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 1.0));
    let mut v: Value = serde_json::from_str(&record.to_json().unwrap()).unwrap();
    // Shift the first piece's reel span into the cut.
    v["streams"][0]["pieces"][0]["reel_from_m"] = json!(5.0);
    v["streams"][0]["pieces"][0]["reel_to_m"] = json!(20.0);
    check_fails(&demand, &inventory, &v, "not inside a usable span");
}

// ---------------------------------------------------------------------
// A binding scenario on a real slice-A demand.
// ---------------------------------------------------------------------

pub(crate) fn real_demand() -> &'static (String, String) {
    static FIXTURE: OnceLock<(String, String)> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let case = reduced_case_json("[70]", 30.0).replace(
            r#""baseline": {"turns_along_normal": 3"#,
            r#""baseline": {"turns_along_normal": 70"#,
        );
        let record =
            run_coupled_search_case(&case, &CoupledSearchOptions { threads: None }).unwrap();
        let record_json = serde_json::to_string(&record).unwrap();
        let demand = build_allocation_demand(&record_json, &DemandOptions::default(), &[]).unwrap();
        (serde_json::to_string_pretty(&demand).unwrap(), record_json)
    })
}

/// A synthetic inventory with profile values scaled so reel scale factors
/// straddle the demand's 0.067 to 0.069 range: the requirement binds.
pub(crate) fn binding_inventory(scale: f64, seed: u64) -> String {
    let spec = json!({
        "schema": "optcoil-synthetic-inventory-spec/v1",
        "inventory_id": "BINDING",
        "seed": seed,
        "product": {
            "vendor": "v", "product": "p",
            "product_map": {"dataset_id": MAP_ID, "csv_sha256": MAP_SHA}
        },
        "reel_count": 24,
        "reel_length_m": {"min": 40.0, "max": 90.0},
        "width_m": WIDTH,
        "profile": {
            "temperature_k": 20.0, "field_t": 1.0, "angle_from_normal_deg": 0.0,
            "electric_field_criterion_v_per_m": 1e-4, "resolution_m": 1.0,
            "map_reference_row": 1817
        },
        "length_relative_sd": 0.03,
        "length_correlation_m": 5.0,
        "ab_offset_group": "cheng2025_F1"
    });
    let (inventory, _) = synthesize_inventory_json(&spec.to_string(), &[]).unwrap();
    let mut value: Value = serde_json::from_str(&inventory).unwrap();
    for (index, passport) in value["passports"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        // Offset bounds the demand tabulates: 0.3 degrees (even reels) or
        // 0.8 degrees (odd reels).
        let offset = if index % 2 == 0 { 0.2 } else { 0.7 };
        passport["ab_offsets"] = json!([{
            "position_m": 0.0, "offset_deg": offset, "uncertainty_deg": 0.1,
            "method": "xrd_rocking_curve", "evidence_class": "synthetic"
        }]);
        for profile in passport["length_profiles"].as_array_mut().unwrap() {
            for point in profile["points"].as_array_mut().unwrap() {
                let ic = point[1].as_f64().unwrap();
                point[1] = json!(ic * scale);
            }
        }
    }
    serde_json::to_string_pretty(&value).unwrap()
}

#[test]
fn a_binding_scenario_on_a_real_demand_allocates_checks_and_beats_the_baseline() {
    // 24 synthetic reels scaled so their scale factors straddle the
    // demand's 0.0673 to 0.0687 range. A reel that dips below the
    // requirement anywhere fails the baseline's uniform worst case, so the
    // baseline scraps most of the inventory and runs 53 m short, while the
    // allocation places the good stretches of the same reels.
    let (demand, _) = real_demand();
    let inventory = binding_inventory(0.0676, 5);
    let record = run(demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 25.0));
    assert_check_passes(demand, &inventory, &record);
    assert_eq!(
        record.evidence_class,
        optcoil_model::reel::EvidenceClass::Synthetic
    );
    assert_eq!(record.allocation.streams_covered, 2);
    assert_eq!(record.allocation.streams_infeasible, 0);
    assert!(record.allocation.shortfall_m.abs() < 1e-9);
    assert!(
        record.baseline.shortfall_m > 1.0,
        "the baseline should run short"
    );
    close(
        record.money.difference_usd,
        record.baseline.shortfall_m * 25.0,
    );
    assert!(record.reel_counts.scrapped_by_baseline_but_used >= 1);
    assert!(record.reel_counts.baseline_rejected > record.reel_counts.baseline_accepted);
    // The requirement binds: pieces carry margins barely above 1, and the
    // stream is spliced from several reels.
    let tightest = record
        .streams
        .iter()
        .flat_map(|s| &s.pieces)
        .map(|p| p.min_margin_ratio)
        .fold(f64::INFINITY, f64::min);
    assert!((1.0..1.2).contains(&tightest), "tightest margin {tightest}");
    assert!(record.allocation.splices >= 4);
    // Every reel used by the allocation has an offset the demand tabulates.
    for reel in record.reels.iter().filter(|r| r.used_length_m > 0.0) {
        assert!(reel.delta_deg.is_some());
    }
}

#[test]
fn a_blocked_offset_keeps_wide_offset_reels_off_the_positions_it_blocks() {
    // The fixture demand has no blocked position, so turns 30 to 40 of both
    // modules are blocked by hand at the 1.0 degree entry; reels with a 0.8
    // degree bound (tabulated 1.0) cannot be placed there.
    let (demand, _) = real_demand();
    let mut parsed: Value = serde_json::from_str(demand).unwrap();
    let mut blocked: Vec<u32> = Vec::new();
    for turn in parsed["turns"].as_array_mut().unwrap() {
        let number = turn["turn"].as_u64().unwrap() as u32;
        if !(30..=40).contains(&number) {
            continue;
        }
        blocked.push(number);
        for module in turn["modules"].as_array_mut().unwrap() {
            let entry = &mut module["s_req"][2];
            assert_eq!(entry["offset_deg"], json!(1.0));
            *entry = json!({
                "offset_deg": 1.0, "s_req": null, "status": "outside_map_domain",
                "explanation": "test block", "next_step": "test"
            });
        }
    }
    assert_eq!(blocked.len(), 11);
    let demand = &serde_json::to_string_pretty(&parsed).unwrap();
    let inventory = binding_inventory(0.0676, 5);
    let record = run(demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 25.0));
    let wide: Vec<&str> = record
        .reels
        .iter()
        .filter(|r| r.delta_deg == Some(1.0))
        .map(|r| r.reel_id.as_str())
        .collect();
    assert!(!wide.is_empty());
    for piece in record.streams.iter().flat_map(|s| &s.pieces) {
        if wide.contains(&piece.reel_id.as_str()) {
            assert!(
                !blocked
                    .iter()
                    .any(|t| (piece.turn_from..=piece.turn_to).contains(t)),
                "{piece:?}"
            );
        }
    }
}
