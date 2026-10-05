//! Truth evaluation and measurement planning on top of the allocation
//! (CR-03 slice C).
//!
//! `optcoil-allocation-truth-evaluation/v1` re-walks every piece of an
//! allocation record, and of its baseline, against a synthetic truth file:
//! a reel's true capacity is its scale factor s(x) (no derate) times its
//! hidden transfer factor. Wherever that is below the demand's s_req, with
//! no margin, the placed tape exceeds the utilization limit.
//!
//! `optcoil-measurement-plan/v1` asks which reels are worth a low-temperature
//! short-sample measurement. It re-runs the slice-B allocator with one
//! reel's derate changed (through the allocator's internal per-reel derate
//! map; the public params are untouched), ranks the reels by what the change
//! saves, and builds the cumulative plan. Every allocation it produces goes
//! through the independent checker; a FAIL aborts the plan.

use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path};

use optcoil_model::{
    Status,
    material::MaterialDataset,
    reel::{EvidenceClass, ProductMapRef, ReelInventory, passport_sha256},
    synthetic::{SYNTHETIC_TRUTH_SCHEMA, SyntheticTruth},
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    allocation::AllocationDemand,
    allocation_check::{check_allocation_json, check_allocation_parsed},
    allocation_run::{
        AllocationInputs, AllocationParams, AllocationRecord, LENGTH_TOLERANCE_M, Prepared,
        ReelCode, StreamAllocation, build_record, merged_cuts, parse_demand, position_requirement,
        prepare_reel, resolve_demand_map, segment_at,
    },
    hash,
    reel::Resolved,
};

pub const TRUTH_EVALUATION_SCHEMA: &str = "optcoil-allocation-truth-evaluation/v1";
pub const MEASUREMENT_PLAN_SCHEMA: &str = "optcoil-measurement-plan/v1";
pub const DEFAULT_MAX_CANDIDATES: usize = 200;

fn invalid(message: impl Into<String>) -> RunError {
    RunError::Invalid(message.into())
}

fn write_new_json<T: Serialize>(value: &T, path: &Path) -> Result<(), RunError> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Truth evaluation
// ---------------------------------------------------------------------

/// Where the true capacity sits lowest against the requirement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthWorst {
    pub reel_id: String,
    /// 0-based stream index.
    pub stream: u32,
    /// Stream coordinate where the worst interval starts.
    pub stream_m: f64,
    pub turn: u32,
    /// `s_true / s_req`; below 1 is a violation.
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthReelRow {
    pub reel_id: String,
    pub transfer_factor: f64,
    pub violated_length_m: f64,
}

/// The truth result for one plan (the allocation or the baseline).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TruthSide {
    pub pieces_total: u32,
    pub violated_pieces: u32,
    pub violated_length_m: f64,
    /// None when the plan placed nothing.
    pub worst: Option<TruthWorst>,
    pub reels: Vec<TruthReelRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthInputs {
    pub demand_sha256: String,
    pub inventory_sha256: String,
    pub allocation_sha256: String,
    pub truth_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruthEvaluation {
    pub schema: String,
    pub optcoil_version: String,
    pub inputs: TruthInputs,
    /// Always synthetic: the truth is a generator's hidden draw.
    pub evidence_class: EvidenceClass,
    pub declared_derate_factor: f64,
    pub margin: f64,
    pub allocation: TruthSide,
    pub baseline: TruthSide,
    pub limitations: Vec<String>,
}

impl TruthEvaluation {
    pub fn to_json(&self) -> Result<String, RunError> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_new_json(self, path.as_ref())
    }
}

/// Parses a truth file and binds it to the inventory bytes.
fn parse_truth(
    truth_json: &str,
    inventory: &ReelInventory,
    inventory_json: &str,
) -> Result<(SyntheticTruth, BTreeMap<String, f64>), RunError> {
    let truth: SyntheticTruth = serde_json::from_str(truth_json)
        .map_err(|e| invalid(format!("synthetic truth does not parse: {e}")))?;
    if truth.schema != SYNTHETIC_TRUTH_SCHEMA {
        return Err(invalid(format!(
            "truth schema must be '{SYNTHETIC_TRUTH_SCHEMA}'"
        )));
    }
    let actual = passport_sha256(inventory_json.as_bytes());
    if truth.inventory_sha256 != actual {
        return Err(invalid(format!(
            "the truth file belongs to inventory {} but the inventory given hashes to {actual}",
            truth.inventory_sha256
        )));
    }
    let mut factors = BTreeMap::new();
    for reel in &truth.reels {
        if !(reel.transfer_factor.is_finite() && reel.transfer_factor > 0.0) {
            return Err(invalid(format!(
                "truth reel '{}' has a transfer factor that is not finite and positive",
                reel.reel_id
            )));
        }
        if factors
            .insert(reel.reel_id.clone(), reel.transfer_factor)
            .is_some()
        {
            return Err(invalid(format!(
                "truth names reel '{}' twice",
                reel.reel_id
            )));
        }
    }
    for passport in &inventory.passports {
        if !factors.contains_key(&passport.reel_id) {
            return Err(invalid(format!(
                "the truth file has no entry for inventory reel '{}'",
                passport.reel_id
            )));
        }
    }
    Ok((truth, factors))
}

/// Re-walks the pieces of `streams` on the merged breakpoints with the true
/// capacity `s(x) x t_R` and no margin. `true_prepared` holds each reel
/// prepared with its transfer factor as the only multiplier.
fn truth_side(
    demand: &AllocationDemand,
    inventory: &ReelInventory,
    true_prepared: &[Prepared],
    factors: &BTreeMap<String, f64>,
    streams: &[StreamAllocation],
) -> Result<TruthSide, RunError> {
    let index_of: BTreeMap<&str, usize> = inventory
        .passports
        .iter()
        .enumerate()
        .map(|(i, p)| (p.reel_id.as_str(), i))
        .collect();
    let turn_ends: Vec<f64> = demand
        .turns
        .iter()
        .map(|t| t.start_m + t.length_m)
        .collect();
    let turn_at = |x: f64| {
        turn_ends
            .partition_point(|end| *end <= x)
            .min(turn_ends.len() - 1)
    };
    let mut violated_by_reel: BTreeMap<&str, f64> = BTreeMap::new();
    let (mut pieces_total, mut violated_pieces, mut violated_length) = (0_u32, 0_u32, 0.0_f64);
    let mut worst: Option<TruthWorst> = None;
    for stream in streams {
        let module = stream.module as usize - 1;
        for piece in &stream.pieces {
            pieces_total += 1;
            let reel = *index_of.get(piece.reel_id.as_str()).ok_or_else(|| {
                invalid(format!(
                    "the allocation uses reel '{}', which is not in the inventory",
                    piece.reel_id
                ))
            })?;
            let prepared = &true_prepared[reel];
            let sub = prepared
                .sub_reels
                .iter()
                .find(|(from, to, _)| {
                    piece.reel_from_m >= from - LENGTH_TOLERANCE_M
                        && piece.reel_to_m <= to + LENGTH_TOLERANCE_M
                })
                .ok_or_else(|| {
                    invalid(format!(
                        "a piece of reel '{}' lies outside its usable length",
                        piece.reel_id
                    ))
                })?;
            let length = piece.reel_to_m - piece.reel_from_m;
            let cuts = merged_cuts(
                &sub.2,
                piece.reel_from_m,
                &turn_ends,
                piece.stream_from_m,
                length,
            );
            let mut piece_violated = 0.0_f64;
            for pair in cuts.windows(2) {
                let (lo, hi) = (pair[0], pair[1]);
                if hi - lo <= LENGTH_TOLERANCE_M {
                    continue;
                }
                let mid = 0.5 * (lo + hi);
                let turn = turn_at(piece.stream_from_m + mid);
                let s_true = segment_at(&sub.2, piece.reel_from_m + mid).s;
                match position_requirement(demand, prepared, module, turn) {
                    Some(s_req) => {
                        let ratio = s_true / s_req;
                        if worst.as_ref().is_none_or(|w| ratio < w.ratio) {
                            worst = Some(TruthWorst {
                                reel_id: piece.reel_id.clone(),
                                stream: stream.stream,
                                stream_m: piece.stream_from_m + lo,
                                turn: turn as u32 + 1,
                                ratio,
                            });
                        }
                        if s_true < s_req {
                            piece_violated += hi - lo;
                        }
                    }
                    // A position the demand blocks cannot carry any tape.
                    None => piece_violated += hi - lo,
                }
            }
            if piece_violated > 0.0 {
                violated_pieces += 1;
                violated_length += piece_violated;
                *violated_by_reel
                    .entry(piece.reel_id.as_str())
                    .or_insert(0.0) += piece_violated;
            }
        }
    }
    let reels = inventory
        .passports
        .iter()
        .map(|passport| TruthReelRow {
            reel_id: passport.reel_id.clone(),
            transfer_factor: factors[&passport.reel_id],
            violated_length_m: violated_by_reel
                .get(passport.reel_id.as_str())
                .copied()
                .unwrap_or(0.0),
        })
        .collect();
    Ok(TruthSide {
        pieces_total,
        violated_pieces,
        violated_length_m: violated_length,
        worst,
        reels,
    })
}

/// Every reel prepared at its true factor (`t_R`, no derate).
fn prepare_true(
    demand: &AllocationDemand,
    inventory: &ReelInventory,
    factors: &BTreeMap<String, f64>,
    map: &Resolved,
) -> Vec<Prepared> {
    let every_sensitive = demand
        .turns
        .iter()
        .all(|t| t.modules.iter().all(|m| m.offset_sensitive));
    inventory
        .passports
        .iter()
        .map(|passport| {
            prepare_reel(
                passport,
                demand,
                factors[&passport.reel_id],
                map,
                every_sensitive,
            )
        })
        .collect()
}

/// Evaluates an allocation record and its baseline against a synthetic
/// truth. The record must pass the independent check first; the truth file
/// must carry the inventory's SHA-256.
pub fn evaluate_truth_json(
    demand_json: &str,
    inventory_json: &str,
    allocation_json: &str,
    truth_json: &str,
    bundle_jsons: &[&str],
) -> Result<TruthEvaluation, RunError> {
    let demand = parse_demand(demand_json)?;
    let inventory = ReelInventory::from_json(inventory_json)?;
    let (_, factors) = parse_truth(truth_json, &inventory, inventory_json)?;
    let record: AllocationRecord = serde_json::from_str(allocation_json)
        .map_err(|e| invalid(format!("allocation record does not parse: {e}")))?;
    let check = check_allocation_json(demand_json, inventory_json, allocation_json, bundle_jsons)?;
    if check.verdict != Status::Pass {
        return Err(invalid(format!(
            "the allocation record fails the independent check, so it is not evaluated: {}",
            check.mismatches().join("; ")
        )));
    }
    let datasets = bundle_jsons
        .iter()
        .map(|b| MaterialDataset::from_bundle_json(b))
        .collect::<Result<Vec<_>, _>>()?;
    let map = resolve_demand_map(&demand, &datasets);
    let true_prepared = prepare_true(&demand, &inventory, &factors, &map);
    let allocation = truth_side(
        &demand,
        &inventory,
        &true_prepared,
        &factors,
        &record.streams,
    )?;
    let baseline = truth_side(
        &demand,
        &inventory,
        &true_prepared,
        &factors,
        &record.baseline_streams,
    )?;
    Ok(TruthEvaluation {
        schema: TRUTH_EVALUATION_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        inputs: TruthInputs {
            demand_sha256: hash(demand_json.as_bytes()),
            inventory_sha256: passport_sha256(inventory_json.as_bytes()),
            allocation_sha256: hash(allocation_json.as_bytes()),
            truth_sha256: hash(truth_json.as_bytes()),
        },
        evidence_class: EvidenceClass::Synthetic,
        declared_derate_factor: record.derate_factor,
        margin: record.params.margin,
        allocation,
        baseline,
        limitations: truth_limitations(),
    })
}

fn truth_limitations() -> Vec<String> {
    vec![
        "Synthetic. The truth is the hidden transfer factor a generator drew for each reel. This measures how the declared derate performs against that draw. It is not evidence about real tape.".to_string(),
        "True capacity is the reel's scale factor s(x) times its transfer factor, with no derate and no margin. A violation is an interval where that is below the demand's s_req; the demand's own sampling basis and conservatism apply.".to_string(),
        "The scale factor is read between profile points as the smaller endpoint value, as in the allocation. The generator's underlying continuous profile is not used.".to_string(),
        "The baseline is evaluated against the position-dependent s_req, not against the uniform requirement it was accepted with.".to_string(),
    ]
}

// ---------------------------------------------------------------------
// Measurement plan
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanInputs {
    pub demand_sha256: String,
    pub inventory_sha256: String,
    pub params_sha256: String,
    pub truth_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanOptions {
    pub measurement_sigma: f64,
    pub max_candidates: usize,
}

/// What a plan state achieves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanOutcome {
    pub streams_covered: u32,
    pub pieces: u32,
    pub splices: u32,
    pub allocated_m: f64,
    pub shortfall_m: f64,
    pub shortfall_usd: f64,
    /// The baseline under the same reel factors.
    pub baseline_shortfall_m: f64,
    pub baseline_scrap_m: f64,
}

/// Change against a reference state (new minus reference).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanDelta {
    pub shortfall_m: f64,
    pub splices: i64,
    /// Change in the allocation's shortfall cost at the declared price;
    /// negative is a saving.
    pub money_usd: f64,
    pub baseline_scrap_m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCandidate {
    pub reel_id: String,
    /// Against the base allocation.
    pub delta: PlanDelta,
    /// False when the allocation's pieces are identical to the base's.
    pub changes_allocation: bool,
    /// 1-based rank among the reels whose measurement improves an outcome.
    pub rank: Option<u32>,
    /// Improves at least one outcome and worsens another (for example fewer
    /// splices but a larger shortfall). Ranked by the same key.
    pub mixed: bool,
    pub checker: Status,
}

/// A reel whose measurement changes the allocation without improving any
/// outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoBenefit {
    pub reel_id: String,
    pub delta: PlanDelta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotAssessed {
    pub reel_id: String,
    pub reason: String,
}

/// What the plan step would reveal on a synthetic inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTruth {
    /// The planned allocation (measured reels at the measured derate)
    /// evaluated against the hidden truth.
    pub planned: TruthSide,
    /// The allocation re-run with the measured reels at their true scale
    /// factors, which is what the measurements would actually reveal.
    pub realized: PlanOutcome,
    pub realized_vs_planned_shortfall_m: f64,
    /// True when the realized shortfall exceeds the planned one.
    pub worse_than_planned: bool,
    pub realized_checker: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    /// 0 is the base allocation, with no reel measured.
    pub step: u32,
    pub reel_id: Option<String>,
    pub outcome: PlanOutcome,
    pub vs_base: PlanDelta,
    pub vs_previous: PlanDelta,
    pub checker: Status,
    pub truth: Option<StepTruth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementPlan {
    pub schema: String,
    pub optcoil_version: String,
    pub inputs: PlanInputs,
    pub params: AllocationParams,
    pub declared_derate_factor: f64,
    pub measurement_sigma: f64,
    pub measured_derate_factor: f64,
    pub max_candidates: u32,
    pub evidence_class: EvidenceClass,
    pub price_usd_per_m: f64,
    pub price_source: String,
    /// In inventory order.
    pub candidates: Vec<PlanCandidate>,
    pub not_assessed: Vec<NotAssessed>,
    /// Assessed reels whose measurement leaves the allocation unchanged.
    pub no_change_reels: Vec<String>,
    /// Assessed reels whose measurement changes the allocation but improves
    /// no outcome; they are not ranked.
    pub no_benefit_reels: Vec<NoBenefit>,
    /// The cumulative plan, step 0 first.
    pub steps: Vec<PlanStep>,
    pub limitations: Vec<String>,
}

impl MeasurementPlan {
    pub fn to_json(&self) -> Result<String, RunError> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        write_new_json(self, path.as_ref())
    }
}

struct Planner<'a> {
    demand: &'a AllocationDemand,
    demand_json: &'a str,
    inventory: &'a ReelInventory,
    inventory_json: &'a str,
    params: &'a AllocationParams,
    map: &'a Resolved,
    inputs: AllocationInputs,
}

impl Planner<'_> {
    /// One allocation with the given per-reel factors, checked by the
    /// independent checker. A FAIL aborts the plan.
    fn run(&self, overrides: &BTreeMap<String, f64>) -> Result<AllocationRecord, RunError> {
        let record = build_record(
            self.demand,
            self.inventory,
            self.params,
            self.map,
            self.inputs.clone(),
            overrides,
        )?;
        let check = check_allocation_parsed(
            self.demand,
            self.demand_json,
            self.inventory,
            self.inventory_json,
            &record,
            self.map,
            overrides,
        )?;
        if check.verdict != Status::Pass {
            return Err(invalid(format!(
                "the independent check failed on an allocation the planner produced: {}",
                check.mismatches().join("; ")
            )));
        }
        Ok(record)
    }
}

fn outcome(record: &AllocationRecord) -> PlanOutcome {
    PlanOutcome {
        streams_covered: record.allocation.streams_covered,
        pieces: record.allocation.pieces,
        splices: record.allocation.splices,
        allocated_m: record.allocation.allocated_m,
        shortfall_m: record.allocation.shortfall_m,
        shortfall_usd: record.allocation.shortfall_usd,
        baseline_shortfall_m: record.baseline.shortfall_m,
        baseline_scrap_m: record.baseline.scrap_m,
    }
}

fn delta(new: &PlanOutcome, reference: &PlanOutcome) -> PlanDelta {
    PlanDelta {
        shortfall_m: new.shortfall_m - reference.shortfall_m,
        splices: i64::from(new.splices) - i64::from(reference.splices),
        money_usd: new.shortfall_usd - reference.shortfall_usd,
        baseline_scrap_m: new.baseline_scrap_m - reference.baseline_scrap_m,
    }
}

/// Floats closer than this to zero are no change.
const IMPROVEMENT_TOLERANCE: f64 = 1e-9;

/// `(improves, worsens)` over shortfall, splices, money and baseline scrap.
pub(crate) fn improvement(delta: &PlanDelta) -> (bool, bool) {
    let floats = [delta.shortfall_m, delta.money_usd, delta.baseline_scrap_m];
    let improves = floats.iter().any(|v| *v < -IMPROVEMENT_TOLERANCE) || delta.splices < 0;
    let worsens = floats.iter().any(|v| *v > IMPROVEMENT_TOLERANCE) || delta.splices > 0;
    (improves, worsens)
}

/// Whether two allocations place exactly the same pieces.
fn same_pieces(a: &AllocationRecord, b: &AllocationRecord) -> bool {
    a.streams.len() == b.streams.len()
        && a.streams.iter().zip(&b.streams).all(|(x, y)| {
            x.stream == y.stream
                && x.pieces.len() == y.pieces.len()
                && x.pieces.iter().zip(&y.pieces).all(|(p, q)| {
                    p.reel_id == q.reel_id
                        && p.reel_from_m.to_bits() == q.reel_from_m.to_bits()
                        && p.reel_to_m.to_bits() == q.reel_to_m.to_bits()
                        && p.stream_from_m.to_bits() == q.stream_from_m.to_bits()
                        && p.stream_to_m.to_bits() == q.stream_to_m.to_bits()
                })
        })
}

/// Builds the measurement plan. `truth_json`, when given, adds the
/// truth-evaluated columns of each step.
pub fn plan_measurements_json(
    demand_json: &str,
    inventory_json: &str,
    params_json: &str,
    options: &PlanOptions,
    truth_json: Option<&str>,
    bundle_jsons: &[&str],
) -> Result<MeasurementPlan, RunError> {
    if !(options.measurement_sigma.is_finite() && options.measurement_sigma > 0.0) {
        return Err(invalid("measurement_sigma must be finite and above 0"));
    }
    if options.max_candidates == 0 {
        return Err(invalid("max_candidates must be at least 1"));
    }
    let demand = parse_demand(demand_json)?;
    let inventory = ReelInventory::from_json(inventory_json)?;
    let params = AllocationParams::from_json(params_json)?;
    if options.measurement_sigma > params.transfer_derate.sigma {
        return Err(invalid(format!(
            "measurement_sigma {} exceeds the declared transfer_derate sigma {}: a measurement cannot be less certain than the 77 K transfer it replaces",
            options.measurement_sigma, params.transfer_derate.sigma
        )));
    }
    let truth = truth_json
        .map(|text| parse_truth(text, &inventory, inventory_json))
        .transpose()?
        .map(|(_, factors)| factors);
    let datasets = bundle_jsons
        .iter()
        .map(|b| MaterialDataset::from_bundle_json(b))
        .collect::<Result<Vec<_>, _>>()?;
    let map = resolve_demand_map(&demand, &datasets);
    let inputs = AllocationInputs {
        demand_sha256: hash(demand_json.as_bytes()),
        inventory_sha256: passport_sha256(inventory_json.as_bytes()),
        params_sha256: hash(params_json.as_bytes()),
        inventory_id: inventory.inventory_id.clone(),
        demand_record_sha256: demand.identities.record_sha256.clone(),
        product_map: ProductMapRef {
            dataset_id: demand.identities.product_map.dataset_id.clone(),
            csv_sha256: demand.identities.product_map.csv_sha256.clone(),
        },
        tape_width_m: demand.geometry.tape_width_m,
    };
    let planner = Planner {
        demand: &demand,
        demand_json,
        inventory: &inventory,
        inventory_json,
        params: &params,
        map: &map,
        inputs,
    };
    let true_prepared = truth
        .as_ref()
        .map(|factors| prepare_true(&demand, &inventory, factors, &map));
    let measured_factor = (-params.transfer_derate.z * options.measurement_sigma).exp();

    // Base allocation and its candidates.
    let no_overrides = BTreeMap::new();
    let base = planner.run(&no_overrides)?;
    let base_outcome = outcome(&base);
    let mut candidates: Vec<PlanCandidate> = Vec::new();
    let mut not_assessed: Vec<NotAssessed> = Vec::new();
    let mut assessed = 0_usize;
    for (reel, passport) in base.reels.iter().zip(&inventory.passports) {
        if reel.code != ReelCode::Eligible {
            not_assessed.push(NotAssessed {
                reel_id: reel.reel_id.clone(),
                reason: format!(
                    "not_eligible: {}",
                    serde_json::to_value(reel.code)?
                        .as_str()
                        .unwrap_or("unknown")
                ),
            });
            continue;
        }
        if assessed >= options.max_candidates {
            not_assessed.push(NotAssessed {
                reel_id: reel.reel_id.clone(),
                reason: format!("beyond_max_candidates ({})", options.max_candidates),
            });
            continue;
        }
        assessed += 1;
        let overrides = BTreeMap::from([(passport.reel_id.clone(), measured_factor)]);
        let record = planner.run(&overrides)?;
        let change = delta(&outcome(&record), &base_outcome);
        let (improves, worsens) = improvement(&change);
        candidates.push(PlanCandidate {
            reel_id: passport.reel_id.clone(),
            delta: change,
            changes_allocation: !same_pieces(&base, &record),
            rank: None,
            mixed: improves && worsens,
            checker: Status::Pass,
        });
    }
    // Rank the reels whose measurement improves an outcome: largest saving,
    // then fewest splices, then reel id.
    let mut order: Vec<usize> = (0..candidates.len())
        .filter(|&i| improvement(&candidates[i].delta).0)
        .collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&candidates[a], &candidates[b]);
        x.delta
            .money_usd
            .total_cmp(&y.delta.money_usd)
            .then(x.delta.splices.cmp(&y.delta.splices))
            .then_with(|| x.reel_id.cmp(&y.reel_id))
    });
    for (rank, &index) in order.iter().enumerate() {
        candidates[index].rank = Some(rank as u32 + 1);
    }
    let no_change_reels: Vec<String> = candidates
        .iter()
        .filter(|c| !c.changes_allocation)
        .map(|c| c.reel_id.clone())
        .collect();
    let no_benefit_reels: Vec<NoBenefit> = candidates
        .iter()
        .filter(|c| c.changes_allocation && !improvement(&c.delta).0)
        .map(|c| NoBenefit {
            reel_id: c.reel_id.clone(),
            delta: c.delta.clone(),
        })
        .collect();

    // The cumulative plan.
    let step_truth = |measured: &BTreeMap<String, f64>,
                      planned: &AllocationRecord,
                      planned_outcome: &PlanOutcome|
     -> Result<Option<StepTruth>, RunError> {
        let (Some(factors), Some(prepared)) = (&truth, &true_prepared) else {
            return Ok(None);
        };
        let planned_side = truth_side(&demand, &inventory, prepared, factors, &planned.streams)?;
        // What the measurements would reveal: the measured reels at their
        // true scale factors, everything else as declared.
        let revealed: BTreeMap<String, f64> = measured
            .keys()
            .map(|id| (id.clone(), factors[id]))
            .collect();
        let realized_record = planner.run(&revealed)?;
        let realized = outcome(&realized_record);
        let gap = realized.shortfall_m - planned_outcome.shortfall_m;
        Ok(Some(StepTruth {
            planned: planned_side,
            realized,
            realized_vs_planned_shortfall_m: gap,
            worse_than_planned: gap > LENGTH_TOLERANCE_M,
            realized_checker: Status::Pass,
        }))
    };
    let mut steps: Vec<PlanStep> = Vec::new();
    let mut previous = base_outcome.clone();
    steps.push(PlanStep {
        step: 0,
        reel_id: None,
        outcome: base_outcome.clone(),
        vs_base: delta(&base_outcome, &base_outcome),
        vs_previous: delta(&base_outcome, &base_outcome),
        checker: Status::Pass,
        truth: step_truth(&no_overrides, &base, &base_outcome)?,
    });
    let mut measured: BTreeMap<String, f64> = BTreeMap::new();
    for (number, &index) in order.iter().enumerate() {
        let reel_id = candidates[index].reel_id.clone();
        measured.insert(reel_id.clone(), measured_factor);
        let record = planner.run(&measured)?;
        let now = outcome(&record);
        steps.push(PlanStep {
            step: number as u32 + 1,
            reel_id: Some(reel_id),
            vs_base: delta(&now, &base_outcome),
            vs_previous: delta(&now, &previous),
            checker: Status::Pass,
            truth: step_truth(&measured, &record, &now)?,
            outcome: now.clone(),
        });
        previous = now;
    }

    let evidence = if truth.is_some() || base.evidence_class == EvidenceClass::Synthetic {
        EvidenceClass::Synthetic
    } else {
        EvidenceClass::ModelInformed
    };
    Ok(MeasurementPlan {
        schema: MEASUREMENT_PLAN_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        inputs: PlanInputs {
            demand_sha256: hash(demand_json.as_bytes()),
            inventory_sha256: passport_sha256(inventory_json.as_bytes()),
            params_sha256: hash(params_json.as_bytes()),
            truth_sha256: truth_json.map(|t| hash(t.as_bytes())),
        },
        declared_derate_factor: params.derate_factor(),
        measurement_sigma: options.measurement_sigma,
        measured_derate_factor: measured_factor,
        max_candidates: options.max_candidates as u32,
        evidence_class: evidence,
        price_usd_per_m: params.price_usd_per_m,
        price_source: params.price_source.clone(),
        params,
        candidates,
        not_assessed,
        no_change_reels,
        no_benefit_reels,
        steps,
        limitations: plan_limitations(evidence, truth_json.is_some()),
    })
}

fn plan_limitations(evidence: EvidenceClass, with_truth: bool) -> Vec<String> {
    let mut list = vec![
        "Each change assumes the measurement confirms the scaled value: the measured reel's derate falls from exp(-z x sigma) to exp(-z x measurement_sigma) and its scale factor is otherwise unchanged. A real measurement can come out lower than the scaled value, in which case the reel would lose the length the plan gave it.".to_string(),
        "Each candidate re-runs the greedy allocator, which has no optimality claim, so a change can include greedy effects that are not caused by the measured reel alone. Splice and shortfall changes are differences between two greedy runs.".to_string(),
        "Money is the change in the allocation's shortfall cost at the declared price and source; negative is a saving. The cost of the measurement is not included. The baseline column shows what the same factors do to the uniform-specification baseline's scrap.".to_string(),
        "Ranking: largest saving first, then fewest splices, then reel id. Only reels whose measurement improves at least one outcome (shortfall, splices, money or baseline scrap, strictly, beyond 1e-9) are ranked and enter the cumulative plan. A reel that improves one outcome and worsens another is ranked by the same key and flagged mixed. Reels whose allocation is unchanged are listed as not changing it; reels whose allocation changes with no improving outcome are listed as no benefit. The plan ends when the ranking is exhausted.".to_string(),
        "measurement_sigma must not exceed the declared transfer_derate sigma (equal is allowed): a measurement cannot be less certain than the 77 K transfer it replaces. It must also be finite and above 0.".to_string(),
        "Every allocation in this plan passed the independent check with the per-reel derate changes applied.".to_string(),
        format!("Evidence class: {}.", evidence.as_str()),
    ];
    if with_truth {
        list.push("Truth columns are synthetic: planned is the planned allocation evaluated against the generator's hidden transfer factors; realized re-runs the allocation with the measured reels at their true scale factors. This quantifies how the declared derate and the measurement assumption perform on this draw and is not evidence about real tape.".to_string());
    }
    list
}
