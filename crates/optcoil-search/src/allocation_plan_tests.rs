//! Tests of the truth evaluation and the measurement plan (CR-03 slice C):
//! hand-built cases with known answers, refusals, determinism, and the
//! binding scenario on a real slice-A demand.

use std::time::Instant;

use serde_json::{Value, json};

use optcoil_model::{Status, reel::passport_sha256};

use crate::{
    allocation_check::check_allocation_json,
    allocation_plan::{
        MeasurementPlan, PlanOptions, TruthEvaluation, evaluate_truth_json, plan_measurements_json,
    },
    allocation_tests::{
        Reel, binding_inventory, flat_demand, inventory_json, params_json, real_demand, run,
    },
    synthetic::synthesize_inventory_json,
};

/// A truth file for exactly these inventory bytes: `factors` in inventory
/// order.
fn truth_json(inventory: &str, factors: &[f64]) -> String {
    let value: Value = serde_json::from_str(inventory).unwrap();
    let reels: Vec<Value> = value["passports"]
        .as_array()
        .unwrap()
        .iter()
        .zip(factors)
        .map(|(passport, t)| {
            json!({
                "reel_id": passport["reel_id"], "lot_factor": 1.0,
                "transfer_factor": t, "ab_offset_deg": null
            })
        })
        .collect();
    json!({
        "schema": "optcoil-synthetic-truth/v1",
        "inventory_id": "hand",
        "inventory_sha256": passport_sha256(inventory.as_bytes()),
        "spec_sha256": "00",
        "seed": 1,
        "reels": reels,
        "limitations": []
    })
    .to_string()
}

fn evaluate(demand: &str, inventory: &str, allocation: &str, truth: &str) -> TruthEvaluation {
    evaluate_truth_json(demand, inventory, allocation, truth, &[]).unwrap()
}

fn plan(
    demand: &str,
    inventory: &str,
    params: &str,
    measurement_sigma: f64,
    truth: Option<&str>,
) -> MeasurementPlan {
    plan_measurements_json(
        demand,
        inventory,
        params,
        &PlanOptions {
            measurement_sigma,
            max_candidates: 200,
        },
        truth,
        &[],
    )
    .unwrap()
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "{a} vs {b}");
}

/// The truth example of the allocation's own hand case (see
/// `allocation_tests::hand_example`): one stream of three 10 m turns needing
/// 0.3, 0.8, 0.3; reel H (15 m, s = 0.9) and reel M (40 m, s = 0.5); the
/// allocation places M 0..10 -> stream 0..10, H 0..15 -> stream 10..25 and
/// M 10..15 -> stream 25..30; the baseline puts only H on stream 0..15.
/// True factors t_M = 0.5 and t_H = 0.85 give true capacities 0.25 and 0.765.
///
/// - M against 0.3: violated on both M pieces, 10 m + 5 m, worst ratio
///   0.25 / 0.3.
/// - H against 0.8 (turn 2, stream 10..20): 0.765 < 0.8, violated 10 m; the
///   last 5 m of H against 0.3 is fine.
/// - Allocation: 25 m violated, 3 of 3 pieces. Baseline: H on stream 0..15
///   is violated over stream 10..15 only, 5 m, 1 of 1 piece.
#[test]
fn truth_evaluation_matches_the_hand_computed_violations() {
    let demand = flat_demand(&[(10.0, 0.3), (10.0, 0.8), (10.0, 0.3)]);
    let inventory = inventory_json(&[Reel::flat("H", 15.0, 0.9), Reel::flat("M", 40.0, 0.5)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 10.0));
    let text = record.to_json().unwrap();
    let truth = truth_json(&inventory, &[0.85, 0.5]);
    let result = evaluate(&demand, &inventory, &text, &truth);
    assert_eq!(result.schema, "optcoil-allocation-truth-evaluation/v1");
    assert_eq!(
        result.evidence_class,
        optcoil_model::reel::EvidenceClass::Synthetic
    );
    let a = &result.allocation;
    assert_eq!((a.pieces_total, a.violated_pieces), (3, 3));
    close(a.violated_length_m, 25.0);
    let worst = a.worst.as_ref().unwrap();
    assert_eq!(worst.reel_id, "M");
    assert_eq!(worst.turn, 1);
    close(worst.stream_m, 0.0);
    close(worst.ratio, 0.25 / 0.3);
    let by_reel = |side: &crate::allocation_plan::TruthSide, id: &str| {
        side.reels
            .iter()
            .find(|r| r.reel_id == id)
            .map(|r| (r.transfer_factor, r.violated_length_m))
            .unwrap()
    };
    assert_eq!(by_reel(a, "M"), (0.5, 15.0));
    assert_eq!(by_reel(a, "H"), (0.85, 10.0));
    let b = &result.baseline;
    assert_eq!((b.pieces_total, b.violated_pieces), (1, 1));
    close(b.violated_length_m, 5.0);
    close(b.worst.as_ref().unwrap().ratio, 0.765 / 0.8);
    assert_eq!(by_reel(b, "M"), (0.5, 0.0));
    assert_eq!(by_reel(b, "H"), (0.85, 5.0));
}

#[test]
fn a_true_factor_of_one_with_no_margin_violates_nothing_where_the_scale_equals_the_need() {
    // The placed tape carries exactly the requirement: not a violation.
    let demand = flat_demand(&[(10.0, 0.5)]);
    let inventory = inventory_json(&[Reel::flat("A", 10.0, 0.5)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 10.0));
    let text = record.to_json().unwrap();
    let result = evaluate(&demand, &inventory, &text, &truth_json(&inventory, &[1.0]));
    assert_eq!(result.allocation.violated_pieces, 0);
    assert_eq!(result.allocation.violated_length_m, 0.0);
    close(result.allocation.worst.as_ref().unwrap().ratio, 1.0);
}

#[test]
fn truth_evaluation_refuses_a_truth_for_another_inventory_and_a_failing_record() {
    let demand = flat_demand(&[(10.0, 0.3)]);
    let inventory = inventory_json(&[Reel::flat("A", 10.0, 0.9)]);
    let other = inventory_json(&[Reel::flat("B", 10.0, 0.9)]);
    let record = run(&demand, &inventory, &params_json(0.0, 0.0, 1.0, 5.0, 10.0));
    let text = record.to_json().unwrap();
    let error = evaluate_truth_json(&demand, &inventory, &text, &truth_json(&other, &[1.0]), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("belongs to inventory"), "{error}");
    // A tampered record is not evaluated.
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["allocation"]["pieces"] = json!(7);
    let tampered = serde_json::to_string_pretty(&value).unwrap();
    let error = evaluate_truth_json(
        &demand,
        &inventory,
        &tampered,
        &truth_json(&inventory, &[1.0]),
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("independent check"), "{error}");
    // A truth file missing a reel.
    let mut value: Value = serde_json::from_str(&truth_json(&inventory, &[1.0])).unwrap();
    value["reels"] = json!([]);
    let error = evaluate_truth_json(&demand, &inventory, &text, &value.to_string(), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("no entry"), "{error}");
}

/// The measurement example. One stream of two 10 m turns, both needing
/// s_req 0.7. Declared derate: sigma 0.1, z 2, factor exp(-0.2) = 0.818731;
/// measurement sigma 0.01, factor exp(-0.02) = 0.980199. Price 10 USD/m.
/// Reel A (20 m, s = 0.8): derated 0.8 x 0.818731 = 0.65498 < 0.7, so it
/// cannot be placed; measured 0.8 x 0.980199 = 0.78416 >= 0.7, so it covers
/// the whole stream. Reel B (20 m, s = 0.5) is below 0.7 either way.
///
/// Base: shortfall 20 m = 200 USD, no pieces. Measuring A: shortfall 0, one
/// piece, no splices: delta shortfall -20 m, delta splices 0, delta money
/// -200 USD. The baseline's uniform requirement is 0.7 as well, so A moves
/// from scrap to accepted: baseline scrap 40 m -> 20 m. Measuring B changes
/// nothing.
fn measurement_example() -> (String, String, String) {
    (
        flat_demand(&[(10.0, 0.7), (10.0, 0.7)]),
        inventory_json(&[Reel::flat("A", 20.0, 0.8), Reel::flat("B", 20.0, 0.5)]),
        params_json(0.0, 0.1, 2.0, 5.0, 10.0),
    )
}

#[test]
fn measuring_one_reel_converts_a_shortfall_into_coverage_by_the_hand_computed_delta() {
    let (demand, inventory, params) = measurement_example();
    let result = plan(&demand, &inventory, &params, 0.01, None);
    close(result.measured_derate_factor, (-0.02_f64).exp());
    close(result.declared_derate_factor, (-0.2_f64).exp());
    let base = &result.steps[0];
    assert_eq!(base.step, 0);
    assert!(base.reel_id.is_none());
    close(base.outcome.shortfall_m, 20.0);
    close(base.outcome.shortfall_usd, 200.0);
    close(base.outcome.baseline_scrap_m, 40.0);
    // Candidates in inventory order.
    let a = &result.candidates[0];
    let b = &result.candidates[1];
    assert_eq!((a.reel_id.as_str(), b.reel_id.as_str()), ("A", "B"));
    assert!(a.changes_allocation);
    close(a.delta.shortfall_m, -20.0);
    assert_eq!(a.delta.splices, 0);
    close(a.delta.money_usd, -200.0);
    close(a.delta.baseline_scrap_m, -20.0);
    assert_eq!(a.rank, Some(1));
    assert!(!b.changes_allocation);
    assert_eq!(b.rank, None);
    close(b.delta.shortfall_m, 0.0);
    assert_eq!(result.no_change_reels, vec!["B".to_string()]);
    assert!(result.no_benefit_reels.is_empty());
    assert!(!a.mixed);
    // The cumulative plan: the base, then A.
    assert_eq!(result.steps.len(), 2);
    let step = &result.steps[1];
    assert_eq!(step.reel_id.as_deref(), Some("A"));
    close(step.outcome.shortfall_m, 0.0);
    assert_eq!(step.outcome.streams_covered, 1);
    assert_eq!(step.outcome.pieces, 1);
    close(step.vs_base.money_usd, -200.0);
    close(step.vs_previous.money_usd, -200.0);
    assert_eq!(step.checker, Status::Pass);
    assert!(result.steps.iter().all(|s| s.truth.is_none()));
    assert!(result.not_assessed.is_empty());
}

#[test]
fn the_plan_is_deterministic() {
    let (demand, inventory, params) = measurement_example();
    let first = plan(&demand, &inventory, &params, 0.01, None)
        .to_json()
        .unwrap();
    let second = plan(&demand, &inventory, &params, 0.01, None)
        .to_json()
        .unwrap();
    assert_eq!(first, second);
    assert!(first.ends_with("}\n"));
}

#[test]
fn ranking_orders_by_saving_then_splices_then_reel_id() {
    // Two identical reels A1, A2 (each can cover one 10 m stream turn when
    // measured) on a stream of two 10 m turns: measuring either covers 10 m
    // of the 20 m; the tie is broken by reel id, and the plan measures both.
    let demand = flat_demand(&[(10.0, 0.7), (10.0, 0.7)]);
    let inventory = inventory_json(&[
        Reel::flat("A2", 10.0, 0.8),
        Reel::flat("A1", 10.0, 0.8),
        Reel::flat("C", 20.0, 0.4),
    ]);
    let params = params_json(0.0, 0.1, 2.0, 5.0, 10.0);
    let result = plan(&demand, &inventory, &params, 0.01, None);
    let rank = |id: &str| {
        result
            .candidates
            .iter()
            .find(|c| c.reel_id == id)
            .unwrap()
            .rank
    };
    assert_eq!(
        (rank("A1"), rank("A2"), rank("C")),
        (Some(1), Some(2), None)
    );
    assert_eq!(result.steps.len(), 3);
    close(result.steps[1].outcome.shortfall_m, 10.0);
    close(result.steps[2].outcome.shortfall_m, 0.0);
    assert_eq!(result.steps[2].outcome.splices, 1);
    close(result.steps[2].vs_previous.money_usd, -100.0);
    close(result.steps[2].vs_base.money_usd, -200.0);
}

/// The truth "worse than planned" example. On the measurement example with
/// true transfer factors t_A = 0.8 and t_B = 1.0: reel A measures at
/// 0.8 x 0.8 = 0.64 of the map, not the assumed 0.78416. The plan puts A on
/// the whole 20 m stream (planned shortfall 0), but the true capacity 0.64
/// is below 0.7 everywhere: 20 m violated, worst ratio 0.64 / 0.7. Once the
/// true value is known the allocation cannot use A, so the realized
/// shortfall is 20 m against the planned 0.
#[test]
fn a_low_true_factor_makes_the_realized_result_worse_than_planned() {
    let (demand, inventory, params) = measurement_example();
    let truth = truth_json(&inventory, &[0.8, 1.0]);
    let result = plan(&demand, &inventory, &params, 0.01, Some(&truth));
    assert_eq!(
        result.evidence_class,
        optcoil_model::reel::EvidenceClass::Synthetic
    );
    assert_eq!(result.steps.len(), 2);
    let base_truth = result.steps[0].truth.as_ref().unwrap();
    assert!(!base_truth.worse_than_planned);
    close(base_truth.realized.shortfall_m, 20.0);
    let step_truth = result.steps[1].truth.as_ref().unwrap();
    close(step_truth.planned.violated_length_m, 20.0);
    assert_eq!(step_truth.planned.violated_pieces, 1);
    close(step_truth.planned.worst.as_ref().unwrap().ratio, 0.64 / 0.7);
    close(step_truth.realized.shortfall_m, 20.0);
    close(step_truth.realized_vs_planned_shortfall_m, 20.0);
    assert!(step_truth.worse_than_planned);
    // With a high true factor the measurement is confirmed.
    let truth = truth_json(&inventory, &[1.0, 1.0]);
    let result = plan(&demand, &inventory, &params, 0.01, Some(&truth));
    let step_truth = result.steps[1].truth.as_ref().unwrap();
    close(step_truth.planned.violated_length_m, 0.0);
    close(step_truth.realized.shortfall_m, 0.0);
    assert!(!step_truth.worse_than_planned);
}

#[test]
fn the_plan_refuses_a_foreign_truth_and_bad_options() {
    let (demand, inventory, params) = measurement_example();
    let other = inventory_json(&[Reel::flat("Z", 20.0, 0.8)]);
    let truth = truth_json(&other, &[1.0]);
    let options = PlanOptions {
        measurement_sigma: 0.01,
        max_candidates: 200,
    };
    let error = plan_measurements_json(&demand, &inventory, &params, &options, Some(&truth), &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("belongs to inventory"), "{error}");
    for sigma in [0.0, -0.1, f64::NAN] {
        let options = PlanOptions {
            measurement_sigma: sigma,
            max_candidates: 200,
        };
        assert!(plan_measurements_json(&demand, &inventory, &params, &options, None, &[]).is_err());
    }
}

#[test]
fn measurement_sigma_may_not_exceed_the_declared_sigma() {
    let (demand, inventory, params) = measurement_example();
    let run_with = |sigma: f64| {
        plan_measurements_json(
            &demand,
            &inventory,
            &params,
            &PlanOptions {
                measurement_sigma: sigma,
                max_candidates: 200,
            },
            None,
            &[],
        )
    };
    // The declared sigma is 0.1: equal is allowed, above is refused.
    assert!(run_with(0.1).is_ok());
    let error = run_with(0.1000001).unwrap_err().to_string();
    assert!(error.contains("less certain"), "{error}");
    for bad in [-0.01, f64::NAN, f64::INFINITY] {
        assert!(run_with(bad).is_err(), "{bad}");
    }
}

/// A reel whose measurement changes the allocation without improving
/// anything. Demand 10 m at s_req 0.5; declared factor 0.81873, measured
/// 0.98020. P (s = 0.70, derated surplus 1.146) and Q (s = 0.75, derated
/// surplus 1.228) are both accepted by the baseline. The greedy takes the
/// smaller surplus, P. Measuring P lifts its surplus to 1.372, above Q's, so
/// the allocation moves to Q: same shortfall (0), same splices (0), same
/// money, same baseline scrap. Measuring Q leaves P in place.
#[test]
fn a_changed_allocation_without_an_improvement_is_listed_as_no_benefit() {
    let demand = flat_demand(&[(10.0, 0.5)]);
    let inventory = inventory_json(&[Reel::flat("P", 10.0, 0.70), Reel::flat("Q", 10.0, 0.75)]);
    let params = params_json(0.0, 0.1, 2.0, 5.0, 10.0);
    let result = plan(&demand, &inventory, &params, 0.01, None);
    let p = &result.candidates[0];
    assert!(p.changes_allocation);
    assert_eq!(p.rank, None);
    assert!(!p.mixed);
    assert_eq!(result.no_benefit_reels.len(), 1);
    assert_eq!(result.no_benefit_reels[0].reel_id, "P");
    close(result.no_benefit_reels[0].delta.shortfall_m, 0.0);
    assert_eq!(result.no_change_reels, vec!["Q".to_string()]);
    // Nothing is ranked, so the plan is the base alone.
    assert_eq!(result.steps.len(), 1);
}

#[test]
fn improvement_is_strict_and_flags_mixed_cases() {
    use crate::allocation_plan::PlanDelta;
    let delta = |shortfall_m: f64, splices: i64, money_usd: f64, scrap: f64| PlanDelta {
        shortfall_m,
        splices,
        money_usd,
        baseline_scrap_m: scrap,
    };
    let imp = |d: &PlanDelta| crate::allocation_plan::improvement(d);
    assert_eq!(imp(&delta(0.0, 0, 0.0, 0.0)), (false, false));
    assert_eq!(imp(&delta(-1e-10, 0, 0.0, 0.0)), (false, false));
    assert_eq!(imp(&delta(-1.0, 0, -10.0, 0.0)), (true, false));
    assert_eq!(imp(&delta(0.0, -1, 0.0, 0.0)), (true, false));
    assert_eq!(imp(&delta(0.0, 0, 0.0, -5.0)), (true, false));
    // Fewer splices but a larger shortfall: mixed.
    assert_eq!(imp(&delta(2.0, -1, 20.0, 0.0)), (true, true));
    assert_eq!(imp(&delta(1.0, 1, 10.0, 0.0)), (false, true));
}

#[test]
fn max_candidates_limits_the_assessed_reels_and_lists_the_rest() {
    let (demand, inventory, params) = measurement_example();
    let result = plan_measurements_json(
        &demand,
        &inventory,
        &params,
        &PlanOptions {
            measurement_sigma: 0.01,
            max_candidates: 1,
        },
        None,
        &[],
    )
    .unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.not_assessed.len(), 1);
    assert_eq!(result.not_assessed[0].reel_id, "B");
    assert!(
        result.not_assessed[0]
            .reason
            .starts_with("beyond_max_candidates")
    );
}

#[test]
fn the_checker_needs_the_planners_derate_changes_and_passes_with_them() {
    use std::collections::BTreeMap;

    use optcoil_model::reel::ReelInventory;

    use crate::allocation_run::{
        AllocationInputs, AllocationParams, build_record, parse_demand, resolve_demand_map,
    };
    let (demand_json, inventory_json_text, params_text) = measurement_example();
    let demand = parse_demand(&demand_json).unwrap();
    let inventory = ReelInventory::from_json(&inventory_json_text).unwrap();
    let params = AllocationParams::from_json(&params_text).unwrap();
    let map = resolve_demand_map(&demand, &[]);
    let inputs = AllocationInputs {
        demand_sha256: crate::hash(demand_json.as_bytes()),
        inventory_sha256: passport_sha256(inventory_json_text.as_bytes()),
        params_sha256: crate::hash(params_text.as_bytes()),
        inventory_id: inventory.inventory_id.clone(),
        demand_record_sha256: demand.identities.record_sha256.clone(),
        product_map: optcoil_model::reel::ProductMapRef {
            dataset_id: demand.identities.product_map.dataset_id.clone(),
            csv_sha256: demand.identities.product_map.csv_sha256.clone(),
        },
        tape_width_m: demand.geometry.tape_width_m,
    };
    let overrides = BTreeMap::from([("A".to_string(), (-0.02_f64).exp())]);
    let record = build_record(&demand, &inventory, &params, &map, inputs, &overrides).unwrap();
    assert_eq!(record.allocation.shortfall_m, 0.0);
    let text = record.to_json().unwrap();
    // The public checker knows only the declared derate and rejects it.
    let public = check_allocation_json(&demand_json, &inventory_json_text, &text, &[]).unwrap();
    assert_eq!(public.verdict, Status::Fail);
    let with = crate::allocation_check::check_allocation_parsed(
        &demand,
        &demand_json,
        &inventory,
        &inventory_json_text,
        &record,
        &map,
        &overrides,
    )
    .unwrap();
    assert_eq!(with.verdict, Status::Pass, "{:#?}", with.mismatches());
}

/// The binding scenario on the real 70-turn demand: 24 synthetic reels, a
/// declared derate of 0.1 (z = 1) and a measurement sigma of 0.02. Reports
/// the time of the plan with and without truth.
#[test]
fn the_binding_scenario_plans_with_and_without_truth() {
    let (demand, _) = real_demand();
    let inventory = binding_inventory(0.0710, 5);
    let params = params_json(0.0, 0.1, 1.0, 5.0, 25.0);
    let factors: Vec<f64> = (0..24)
        .map(|i| [1.02, 0.97, 0.92, 1.0, 0.88, 0.95][i % 6])
        .collect();
    let truth = truth_json(&inventory, &factors);
    let start = Instant::now();
    let without = plan(demand, &inventory, &params, 0.02, None);
    let t_without = start.elapsed();
    let start = Instant::now();
    let with = plan(demand, &inventory, &params, 0.02, Some(&truth));
    let t_with = start.elapsed();
    eprintln!(
        "PLAN24 without truth {:?} ({} candidates, {} steps); with truth {:?}",
        t_without,
        without.candidates.len(),
        without.steps.len(),
        t_with
    );
    for (a, b) in without.steps.iter().zip(&with.steps) {
        assert_eq!(a.outcome, b.outcome);
    }
    eprintln!(
        "PLAN24 steps {} mixed {:?} no_benefit {:?} no_change {}",
        without.steps.len(),
        without
            .candidates
            .iter()
            .filter(|c| c.mixed)
            .map(|c| (&c.reel_id, &c.delta))
            .collect::<Vec<_>>(),
        without
            .no_benefit_reels
            .iter()
            .map(|n| &n.reel_id)
            .collect::<Vec<_>>(),
        without.no_change_reels.len()
    );
    // The declared derate leaves a shortfall that measurements close.
    assert!(without.steps[0].outcome.shortfall_m > 1.0);
    assert_eq!(without.steps.last().unwrap().outcome.shortfall_m, 0.0);
    assert!(without.steps.iter().all(|s| s.checker == Status::Pass));
    assert!(without.steps.iter().all(|s| s.truth.is_none()));
    assert!(with.steps.iter().all(|s| s.truth.is_some()));
    // Some measurement does not hold up against the hidden factors.
    assert!(
        with.steps
            .iter()
            .any(|s| s.truth.as_ref().is_some_and(|t| t.worse_than_planned))
    );
    // Candidate deltas agree with and without truth; truth only adds columns.
    for (a, b) in without.candidates.iter().zip(&with.candidates) {
        assert_eq!(a.reel_id, b.reel_id);
        assert_eq!(a.delta, b.delta);
    }
}

/// Timing of the plan on a 200-reel synthetic inventory over the 70-turn
/// demand.
#[test]
fn a_200_reel_plan_is_timed() {
    let (demand, _) = real_demand();
    let spec = json!({
        "schema": "optcoil-synthetic-inventory-spec/v1",
        "inventory_id": "BIG",
        "seed": 9,
        "product": {
            "vendor": "v", "product": "p",
            "product_map": {"dataset_id": crate::allocation_tests::MAP_ID, "csv_sha256": crate::allocation_tests::MAP_SHA}
        },
        "reel_count": 200,
        "reel_length_m": {"min": 40.0, "max": 90.0},
        "width_m": 0.012,
        "profile": {
            "temperature_k": 20.0, "field_t": 1.0, "angle_from_normal_deg": 0.0,
            "electric_field_criterion_v_per_m": 1e-4, "resolution_m": 1.0,
            "map_reference_row": 1817
        },
        "length_relative_sd": 0.03,
        "length_correlation_m": 5.0,
        "ab_offset_group": "cheng2025_F1"
    });
    let (inventory, truth) = synthesize_inventory_json(&spec.to_string(), &[]).unwrap();
    let params = params_json(0.0, 0.1, 1.0, 5.0, 25.0);
    let start = Instant::now();
    let result = plan(demand, &inventory, &params, 0.02, Some(&truth));
    eprintln!(
        "PLAN200 {:?}: {} candidates, {} ranked steps",
        start.elapsed(),
        result.candidates.len(),
        result.steps.len() - 1
    );
    assert_eq!(result.candidates.len(), 200);
}
