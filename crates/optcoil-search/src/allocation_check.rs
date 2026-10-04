//! Independent acceptance of an allocation record (`optcoil-allocation/v1`).
//!
//! This module recomputes, from the demand table, the inventory and the
//! allocation record, whether the record is valid: every piece's feasibility
//! on the merged breakpoints, coverage with no overlaps, reel-length
//! conservation, splice counts and every accounting total. It does not call
//! the allocator. It shares only the input and record types and the CR-02
//! scale-factor extraction (`reel::scale_series`); reel preparation, the
//! feasibility walk and the accounting are written again here.
//!
//! It verifies what the record claims; it does not re-run the greedy, so it
//! does not judge whether another allocation would have been better.
//!
//! PASS requires exact agreement on integers and verdicts and agreement
//! within 1e-9 relative on floats.

use std::collections::BTreeMap;

use optcoil_model::{
    Status,
    material::{MaterialDataClass, MaterialDataset},
    reel::{EvidenceClass, ReelInventory, ReelPassport, passport_sha256},
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    allocation::AllocationDemand,
    allocation_run::{
        ALLOCATION_SCHEMA, AllocationRecord, BaselineReelStatus, LENGTH_TOLERANCE_M, ReelCode,
        ReelUse, StreamAllocation, StreamStatus, WIDTH_TOLERANCE_M, parse_demand,
    },
    hash,
    reel::{Resolved, resolve_map, scale_series},
};

const FLOAT_TOLERANCE: f64 = 1e-9;
/// Absolute slack on stream and reel coordinates (m).
const COORDINATE_SLACK_M: f64 = 2e-9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckGroup {
    pub id: String,
    pub status: Status,
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationCheck {
    /// PASS iff every group passed.
    pub verdict: Status,
    pub groups: Vec<CheckGroup>,
}

impl AllocationCheck {
    pub fn mismatches(&self) -> Vec<String> {
        self.groups
            .iter()
            .flat_map(|g| g.mismatches.iter().map(|m| format!("[{}] {m}", g.id)))
            .collect()
    }
}

struct Collector {
    groups: BTreeMap<&'static str, Vec<String>>,
}

impl Collector {
    fn new() -> Self {
        let mut groups = BTreeMap::new();
        for id in [
            "identity",
            "reels",
            "feasibility",
            "coverage",
            "conservation",
            "counts",
            "totals",
            "baseline",
        ] {
            groups.insert(id, Vec::new());
        }
        Self { groups }
    }

    fn fail(&mut self, group: &'static str, detail: String) {
        self.groups
            .get_mut(group)
            .expect("known group")
            .push(detail);
    }

    fn eq_int<T: PartialEq + std::fmt::Display>(
        &mut self,
        group: &'static str,
        what: &str,
        recorded: T,
        recomputed: T,
    ) {
        if recorded != recomputed {
            self.fail(
                group,
                format!("{what}: record says {recorded}, recomputed {recomputed}"),
            );
        }
    }

    fn eq_float(&mut self, group: &'static str, what: &str, recorded: f64, recomputed: f64) {
        if !close(recorded, recomputed) {
            self.fail(
                group,
                format!("{what}: record says {recorded}, recomputed {recomputed}"),
            );
        }
    }

    fn eq_opt_float(
        &mut self,
        group: &'static str,
        what: &str,
        recorded: Option<f64>,
        recomputed: Option<f64>,
    ) {
        match (recorded, recomputed) {
            (None, None) => {}
            (Some(a), Some(b)) => self.eq_float(group, what, a, b),
            (a, b) => self.fail(
                group,
                format!("{what}: record says {a:?}, recomputed {b:?}"),
            ),
        }
    }

    fn finish(self) -> AllocationCheck {
        let groups: Vec<CheckGroup> = self
            .groups
            .into_iter()
            .map(|(id, mismatches)| CheckGroup {
                id: id.to_string(),
                status: if mismatches.is_empty() {
                    Status::Pass
                } else {
                    Status::Fail
                },
                mismatches,
            })
            .collect();
        let verdict = if groups.iter().all(|g| g.status == Status::Pass) {
            Status::Pass
        } else {
            Status::Fail
        };
        AllocationCheck { verdict, groups }
    }
}

fn close(a: f64, b: f64) -> bool {
    if !(a.is_finite() && b.is_finite()) {
        return a == b;
    }
    // Relative, with an absolute floor of the tolerance itself so sums that
    // cancel to zero (a remnant of 5e-14 m against 0) still agree.
    (a - b).abs() <= FLOAT_TOLERANCE * a.abs().max(b.abs()).max(1.0)
}

/// A usable sub-reel as `(from, to, steps)` with steps `(a, b, s_used)`.
type SubModel = (f64, f64, Vec<(f64, f64, f64)>);

/// The checker's own model of one reel.
struct ReelModel {
    code: ReelCode,
    delta_index: Option<usize>,
    bound: Option<f64>,
    min_s: Option<f64>,
    usable: f64,
    /// Sub-reels as `(from, to, steps)` with steps `(a, b, s_used)`.
    subs: Vec<SubModel>,
    /// Max s_req over the whole coil at the reel's offset.
    uniform: Option<f64>,
}

fn model_reel(
    passport: &ReelPassport,
    demand: &AllocationDemand,
    factor: f64,
    map: &Resolved,
    every_position_sensitive: bool,
) -> ReelModel {
    let empty = |code| ReelModel {
        code,
        delta_index: None,
        bound: None,
        min_s: None,
        usable: 0.0,
        subs: Vec::new(),
        uniform: None,
    };
    let Some(reference) = &passport.product.product_map else {
        return empty(ReelCode::NoProductMap);
    };
    let wanted = &demand.identities.product_map;
    if reference.dataset_id != wanted.dataset_id || reference.csv_sha256 != wanted.csv_sha256 {
        return empty(ReelCode::MapMismatch);
    }
    let Resolved::Model(model) = map else {
        return empty(ReelCode::MapUnavailable);
    };
    if (passport.geometry.width_m - demand.geometry.tape_width_m).abs() > WIDTH_TOLERANCE_M {
        return empty(ReelCode::WidthMismatch);
    }
    let Ok(series) = scale_series(passport, model) else {
        return empty(ReelCode::ProfileUnscalable);
    };
    let pts = &series.points;
    // Usable intervals: coverage minus the excluded and cut spans.
    let removed = passport.unusable_spans();
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    if pts.len() >= 2 {
        let (lo, hi) = (pts[0].0, pts[pts.len() - 1].0);
        let mut left = lo;
        for (start, end) in &removed {
            if *start > left {
                intervals.push((left, start.min(hi)));
            }
            if *end > left {
                left = *end;
            }
        }
        if left < hi {
            intervals.push((left, hi));
        }
    }
    intervals.retain(|(a, b)| b - a > LENGTH_TOLERANCE_M);
    if intervals.is_empty() {
        return empty(ReelCode::NoUsableLength);
    }
    let mut subs = Vec::new();
    let mut usable = 0.0;
    let mut min_s = f64::INFINITY;
    for (from, to) in intervals {
        let mut steps = Vec::new();
        for i in 0..pts.len() - 1 {
            let (a, b) = (pts[i].0.max(from), pts[i + 1].0.min(to));
            if b > a {
                let s = pts[i].1.min(pts[i + 1].1) * factor;
                min_s = min_s.min(s);
                steps.push((a, b, s));
            }
        }
        usable += to - from;
        subs.push((from, to, steps));
    }
    let bound = passport
        .ab_offsets
        .iter()
        .map(|o| o.offset_deg.abs() + o.uncertainty_deg)
        .reduce(f64::max);
    let mut delta_index = None;
    if let Some(b) = bound {
        for (index, d) in demand.angle_offsets_deg.iter().enumerate() {
            if *d >= b {
                delta_index = Some(index);
                break;
            }
        }
    }
    if delta_index.is_none() && every_position_sensitive {
        return empty(ReelCode::OffsetSensitiveEverywhere);
    }
    ReelModel {
        code: ReelCode::Eligible,
        delta_index,
        bound,
        min_s: Some(min_s),
        usable,
        subs,
        uniform: None,
    }
}

/// The requirement base for a reel at (module, turn), before the margin.
fn base_need(
    demand: &AllocationDemand,
    reel: &ReelModel,
    module: usize,
    turn: usize,
) -> Option<f64> {
    let cell = &demand.turns[turn].modules[module];
    match reel.delta_index {
        Some(index) => cell.s_req[index].s_req,
        None => {
            if cell.offset_sensitive {
                None
            } else {
                cell.s_req[0].s_req
            }
        }
    }
}

/// What the walk of one piece found.
struct PieceWalk {
    feasible: bool,
    min_ratio: f64,
    turn_from: usize,
    turn_to: usize,
}

/// Walks a piece over the merged breakpoints. `need` gives the requirement
/// base for a turn (0-based), or None if the piece cannot sit there.
#[allow(clippy::too_many_arguments)]
fn walk_piece(
    demand: &AllocationDemand,
    steps: &[(f64, f64, f64)],
    reel_from: f64,
    stream_from: f64,
    length: f64,
    margin: f64,
    need: &dyn Fn(usize) -> Option<f64>,
) -> PieceWalk {
    let mut cuts = vec![0.0, length];
    for (a, b, _) in steps {
        for edge in [a, b] {
            let u = edge - reel_from;
            if u > 0.0 && u < length {
                cuts.push(u);
            }
        }
    }
    for turn in &demand.turns {
        let u = turn.start_m + turn.length_m - stream_from;
        if u > 0.0 && u < length {
            cuts.push(u);
        }
    }
    cuts.sort_by(|a, b| a.total_cmp(b));
    cuts.dedup();
    let mut walk = PieceWalk {
        feasible: true,
        min_ratio: f64::INFINITY,
        turn_from: 0,
        turn_to: 0,
    };
    let mut seen = false;
    for pair in cuts.windows(2) {
        if pair[1] - pair[0] <= LENGTH_TOLERANCE_M {
            continue;
        }
        let mid = 0.5 * (pair[0] + pair[1]);
        let x_reel = reel_from + mid;
        let x_stream = stream_from + mid;
        let Some(&(_, _, s_used)) = steps.iter().find(|(a, b, _)| *a <= x_reel && x_reel < *b)
        else {
            walk.feasible = false;
            return walk;
        };
        let turn = demand
            .turns
            .iter()
            .position(|t| t.start_m + t.length_m > x_stream)
            .unwrap_or(demand.turns.len() - 1);
        match need(turn) {
            Some(base) if s_used >= base * (1.0 + margin) => {
                walk.min_ratio = walk.min_ratio.min(s_used / base);
            }
            _ => {
                walk.feasible = false;
                return walk;
            }
        }
        if !seen {
            walk.turn_from = turn;
            seen = true;
        }
        walk.turn_to = turn;
    }
    if !seen {
        walk.feasible = false;
    }
    walk
}

/// Which plan a set of streams belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Plan {
    Allocation,
    Baseline,
}

/// Checks an allocation record against the exact demand and inventory
/// documents. `allocation_json` is parsed as the record; `bundle_jsons`
/// supply the product map when it is not embedded.
pub fn check_allocation_json(
    demand_json: &str,
    inventory_json: &str,
    allocation_json: &str,
    bundle_jsons: &[&str],
) -> Result<AllocationCheck, RunError> {
    let demand = parse_demand(demand_json)?;
    let inventory = ReelInventory::from_json(inventory_json)?;
    let record: AllocationRecord = serde_json::from_str(allocation_json)
        .map_err(|e| RunError::Invalid(format!("allocation record does not parse: {e}")))?;
    let datasets = bundle_jsons
        .iter()
        .map(|b| MaterialDataset::from_bundle_json(b))
        .collect::<Result<Vec<_>, _>>()?;
    let mut c = Collector::new();

    // Identity.
    if record.schema != ALLOCATION_SCHEMA {
        c.fail("identity", format!("schema is '{}'", record.schema));
    }
    if record.params.validate().is_err() {
        c.fail("identity", "the recorded params are invalid".into());
    }
    c.eq_int(
        "identity",
        "demand_sha256",
        record.inputs.demand_sha256.as_str(),
        hash(demand_json.as_bytes()).as_str(),
    );
    c.eq_int(
        "identity",
        "inventory_sha256",
        record.inputs.inventory_sha256.as_str(),
        passport_sha256(inventory_json.as_bytes()).as_str(),
    );
    c.eq_int(
        "identity",
        "demand record sha256",
        record.inputs.demand_record_sha256.as_str(),
        demand.identities.record_sha256.as_str(),
    );
    c.eq_float(
        "identity",
        "tape width",
        record.inputs.tape_width_m,
        demand.geometry.tape_width_m,
    );
    let params = &record.params;
    let factor = (-params.transfer_derate.z * params.transfer_derate.sigma).exp();
    c.eq_float("identity", "derate factor", record.derate_factor, factor);
    if params.margin < 0.0 || !params.margin.is_finite() {
        c.fail(
            "identity",
            "margin is not a finite value of at least 0".into(),
        );
    }
    let unsafe_derate = params.transfer_derate.sigma == 0.0 || params.transfer_derate.z == 0.0;
    let flagged = record.limitations.iter().any(|l| l.starts_with("UNSAFE"));
    if unsafe_derate != flagged {
        c.fail(
            "identity",
            "the UNSAFE no-derate limitation does not match the derate".into(),
        );
    }
    let n = demand.turns.len();
    let p = demand.geometry.modules as usize;
    let strands = demand.geometry.strands;
    let stream_length = demand.turns[n - 1].start_m + demand.turns[n - 1].length_m;
    c.eq_int(
        "identity",
        "streams_total",
        record.streams_total,
        p as u32 * strands,
    );
    c.eq_float(
        "identity",
        "stream_length_m",
        record.stream_length_m,
        stream_length,
    );

    // Reel models.
    let supplied: BTreeMap<String, MaterialDataset> = datasets
        .iter()
        .map(|d| (d.metadata.id.clone(), d.clone()))
        .collect();
    let map = resolve_map(
        &demand.identities.product_map.dataset_id,
        &demand.identities.product_map.csv_sha256,
        &supplied,
    );
    let every_sensitive = demand
        .turns
        .iter()
        .all(|t| t.modules.iter().all(|m| m.offset_sensitive));
    let mut models: Vec<ReelModel> = inventory
        .passports
        .iter()
        .map(|passport| model_reel(passport, &demand, factor, &map, every_sensitive))
        .collect();
    let margin = params.margin;
    for model in &mut models {
        if model.code != ReelCode::Eligible {
            continue;
        }
        let mut worst = Some(0.0_f64);
        'coil: for module in 0..p {
            for turn in 0..n {
                match base_need(&demand, model, module, turn) {
                    Some(v) => worst = worst.map(|w| w.max(v)),
                    None => {
                        worst = None;
                        break 'coil;
                    }
                }
            }
        }
        model.uniform = worst;
    }
    let accepted = |m: &ReelModel| -> bool {
        m.code == ReelCode::Eligible
            && m.uniform
                .zip(m.min_s)
                .is_some_and(|(u, s)| s >= u * (1.0 + margin))
    };

    if record.reels.len() != inventory.passports.len() {
        c.fail(
            "reels",
            format!(
                "record lists {} reels, the inventory has {}",
                record.reels.len(),
                inventory.passports.len()
            ),
        );
        return Ok(c.finish());
    }
    let index_of: BTreeMap<&str, usize> = inventory
        .passports
        .iter()
        .enumerate()
        .map(|(i, p)| (p.reel_id.as_str(), i))
        .collect();
    for ((passport, model), rec) in inventory.passports.iter().zip(&models).zip(&record.reels) {
        if rec.reel_id != passport.reel_id {
            c.fail(
                "reels",
                format!("reel '{}' is out of order or renamed", rec.reel_id),
            );
            continue;
        }
        let id = &rec.reel_id;
        if rec.code != model.code {
            c.fail(
                "reels",
                format!(
                    "{id}: record code {:?}, recomputed {:?}",
                    rec.code, model.code
                ),
            );
        }
        c.eq_float(
            "reels",
            &format!("{id} usable_length_m"),
            rec.usable_length_m,
            model.usable,
        );
        c.eq_opt_float(
            "reels",
            &format!("{id} offset_bound_deg"),
            rec.offset_bound_deg,
            model.bound,
        );
        c.eq_opt_float(
            "reels",
            &format!("{id} delta_deg"),
            rec.delta_deg,
            model.delta_index.map(|i| demand.angle_offsets_deg[i]),
        );
        c.eq_opt_float(
            "reels",
            &format!("{id} min_s_used"),
            rec.min_s_used,
            model.min_s,
        );
        c.eq_opt_float(
            "reels",
            &format!("{id} baseline_s_req"),
            rec.baseline_s_req,
            model.uniform,
        );
        let baseline = if model.code != ReelCode::Eligible {
            BaselineReelStatus::Unallocatable
        } else if accepted(model) {
            BaselineReelStatus::Accepted
        } else {
            BaselineReelStatus::Rejected
        };
        if rec.baseline != baseline {
            c.fail(
                "baseline",
                format!(
                    "{id}: baseline status {:?}, recomputed {:?}",
                    rec.baseline, baseline
                ),
            );
        }
        if model.code != ReelCode::Eligible
            && (rec.explanation.is_none() || rec.next_evidence.is_none())
        {
            c.fail(
                "reels",
                format!("{id}: an unallocatable reel needs an explanation and next evidence"),
            );
        }
    }

    // Streams: every (module, strand) exactly once in each plan.
    let mut used_by_reel: [BTreeMap<usize, f64>; 2] = [BTreeMap::new(), BTreeMap::new()];
    let mut tallies = [(0u32, 0u32, 0u32, 0u32, 0.0f64, 0.0f64); 2];
    // (covered, infeasible, pieces, splices, allocated, shortfall)
    for (plan, streams) in [
        (Plan::Allocation, &record.streams),
        (Plan::Baseline, &record.baseline_streams),
    ] {
        let slot = usize::from(plan == Plan::Baseline);
        let group: &'static str = if plan == Plan::Baseline {
            "baseline"
        } else {
            "coverage"
        };
        if streams.len() != (p as u32 * strands) as usize {
            c.fail(
                group,
                format!(
                    "{} streams recorded, expected {}",
                    streams.len(),
                    p as u32 * strands
                ),
            );
        }
        let mut seen = vec![false; (p as u32 * strands) as usize];
        let mut reel_spans: Vec<(usize, f64, f64)> = Vec::new();
        let mut previous_peak = f64::INFINITY;
        let mut previous_index: Option<u32> = None;
        for stream in streams {
            let valid = (stream.module as usize) >= 1
                && (stream.module as usize) <= p
                && stream.strand >= 1
                && stream.strand <= strands
                && stream.stream == (stream.module - 1) * strands + (stream.strand - 1);
            if !valid || seen.get(stream.stream as usize).copied().unwrap_or(true) {
                c.fail(
                    group,
                    format!("stream {} is invalid or repeated", stream.stream),
                );
                continue;
            }
            seen[stream.stream as usize] = true;
            let module = stream.module as usize - 1;
            // Peak and the fill order of the allocation.
            let mut peak = Some(0.0_f64);
            for turn in &demand.turns {
                match (peak, turn.modules[module].s_req[0].s_req) {
                    (Some(a), Some(b)) => peak = Some(a.max(b)),
                    _ => peak = None,
                }
            }
            c.eq_opt_float(
                group,
                &format!("stream {} peak_s_req", stream.stream),
                stream.peak_s_req,
                peak,
            );
            let key = peak.unwrap_or(f64::INFINITY);
            if plan == Plan::Allocation {
                if key > previous_peak
                    || (key == previous_peak && previous_index.is_some_and(|i| i > stream.stream))
                {
                    c.fail(
                        group,
                        format!("stream {} is out of fill order", stream.stream),
                    );
                }
                previous_peak = key;
                previous_index = Some(stream.stream);
            } else if previous_index.is_some_and(|i| i > stream.stream) {
                c.fail(
                    group,
                    format!("baseline stream {} is out of index order", stream.stream),
                );
            } else {
                previous_index = Some(stream.stream);
            }
            check_stream(
                &mut c,
                plan,
                stream,
                &demand,
                &models,
                &index_of,
                margin,
                stream_length,
                &mut reel_spans,
                &accepted,
            );
            let last_to = stream.pieces.last().map_or(0.0, |piece| piece.stream_to_m);
            let covered = last_to;
            let t = &mut tallies[slot];
            if stream.status == StreamStatus::Covered {
                t.0 += 1;
            } else {
                t.1 += 1;
            }
            t.2 += stream.pieces.len() as u32;
            t.3 += stream.pieces.len().saturating_sub(1) as u32;
            t.5 += (stream_length - covered).max(0.0);
        }
        if seen.iter().any(|s| !s) {
            c.fail(group, "a stream is missing from the record".into());
        }
        // Reel-length conservation: no segment of a reel used twice.
        reel_spans.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        for pair in reel_spans.windows(2) {
            if pair[0].0 == pair[1].0 && pair[1].1 < pair[0].2 - COORDINATE_SLACK_M {
                c.fail(
                    "conservation",
                    format!(
                        "{:?} plan: reel '{}' is used twice over {:.6} to {:.6} m",
                        if plan == Plan::Baseline {
                            "baseline"
                        } else {
                            "allocation"
                        },
                        inventory.passports[pair[0].0].reel_id,
                        pair[1].1,
                        pair[0].2.min(pair[1].2)
                    ),
                );
            }
        }
        for (reel, from, to) in &reel_spans {
            *used_by_reel[slot].entry(*reel).or_insert(0.0) += to - from;
            tallies[slot].4 += to - from;
        }
    }

    // Per-reel usage and counts.
    let mut counts = [0u32; 7];
    for (index, (model, rec)) in models.iter().zip(&record.reels).enumerate() {
        let used = used_by_reel[0].get(&index).copied().unwrap_or(0.0);
        let baseline_used = used_by_reel[1].get(&index).copied().unwrap_or(0.0);
        let usage = if model.code != ReelCode::Eligible {
            ReelUse::Unallocatable
        } else if used <= LENGTH_TOLERANCE_M {
            ReelUse::UnusedEligible
        } else if model.usable - used <= LENGTH_TOLERANCE_M * model.subs.len() as f64 {
            ReelUse::Used
        } else {
            ReelUse::PartlyUsed
        };
        if rec.usage != usage {
            c.fail(
                "counts",
                format!(
                    "{}: usage {:?}, recomputed {:?}",
                    rec.reel_id, rec.usage, usage
                ),
            );
        }
        c.eq_float(
            "conservation",
            &format!("{} used_length_m", rec.reel_id),
            rec.used_length_m,
            used,
        );
        c.eq_float(
            "conservation",
            &format!("{} baseline_used_length_m", rec.reel_id),
            rec.baseline_used_length_m,
            baseline_used,
        );
        let remnant = if model.code == ReelCode::Eligible {
            (model.usable - used).max(0.0)
        } else {
            0.0
        };
        c.eq_float(
            "conservation",
            &format!("{} remnant_length_m", rec.reel_id),
            rec.remnant_length_m,
            remnant,
        );
        let scrapped_used =
            model.code == ReelCode::Eligible && !accepted(model) && used > LENGTH_TOLERANCE_M;
        if rec.scrapped_by_baseline_but_used != scrapped_used {
            c.fail(
                "counts",
                format!("{}: scrapped-but-used flag disagrees", rec.reel_id),
            );
        }
        match usage {
            ReelUse::Used => counts[0] += 1,
            ReelUse::PartlyUsed => counts[1] += 1,
            ReelUse::UnusedEligible => counts[2] += 1,
            ReelUse::Unallocatable => counts[3] += 1,
        }
        counts[4] += u32::from(scrapped_used);
        counts[5] += u32::from(accepted(model));
        counts[6] += u32::from(model.code == ReelCode::Eligible && !accepted(model));
    }
    let rc = &record.reel_counts;
    for (what, recorded, recomputed) in [
        ("used", rc.used, counts[0]),
        ("partly_used", rc.partly_used, counts[1]),
        ("unused_eligible", rc.unused_eligible, counts[2]),
        ("unallocatable", rc.unallocatable, counts[3]),
        (
            "scrapped_by_baseline_but_used",
            rc.scrapped_by_baseline_but_used,
            counts[4],
        ),
        ("baseline_accepted", rc.baseline_accepted, counts[5]),
        ("baseline_rejected", rc.baseline_rejected, counts[6]),
    ] {
        c.eq_int("counts", what, recorded, recomputed);
    }

    // Accounting totals.
    let eligible_usable: f64 = models
        .iter()
        .filter(|m| m.code == ReelCode::Eligible)
        .map(|m| m.usable)
        .sum();
    let accepted_usable: f64 = models
        .iter()
        .filter(|m| accepted(m))
        .map(|m| m.usable)
        .sum();
    for (slot, name, account, pool, scrap) in [
        (
            0usize,
            "allocation",
            &record.allocation,
            eligible_usable,
            0.0,
        ),
        (
            1usize,
            "baseline",
            &record.baseline,
            accepted_usable,
            eligible_usable - accepted_usable,
        ),
    ] {
        let t = tallies[slot];
        c.eq_int(
            "totals",
            &format!("{name} streams_covered"),
            account.streams_covered,
            t.0,
        );
        c.eq_int(
            "totals",
            &format!("{name} streams_infeasible"),
            account.streams_infeasible,
            t.1,
        );
        c.eq_int("totals", &format!("{name} pieces"), account.pieces, t.2);
        c.eq_int("totals", &format!("{name} splices"), account.splices, t.3);
        c.eq_float(
            "totals",
            &format!("{name} allocated_m"),
            account.allocated_m,
            t.4,
        );
        c.eq_float(
            "totals",
            &format!("{name} remnant_m"),
            account.remnant_m,
            (pool - t.4).max(0.0),
        );
        c.eq_float("totals", &format!("{name} scrap_m"), account.scrap_m, scrap);
        c.eq_float(
            "totals",
            &format!("{name} shortfall_m"),
            account.shortfall_m,
            t.5,
        );
        c.eq_float(
            "totals",
            &format!("{name} shortfall_usd"),
            account.shortfall_usd,
            t.5 * params.price_usd_per_m,
        );
    }
    let difference = tallies[1].5 * params.price_usd_per_m - tallies[0].5 * params.price_usd_per_m;
    c.eq_float(
        "totals",
        "money difference_usd",
        record.money.difference_usd,
        difference,
    );
    c.eq_float(
        "totals",
        "money price_usd_per_m",
        record.money.price_usd_per_m,
        params.price_usd_per_m,
    );
    if record.money.price_source != params.price_source {
        c.fail(
            "totals",
            "the money price_source differs from the params".into(),
        );
    }

    // Evidence class.
    let mut evidence = if inventory.evidence_class == EvidenceClass::Synthetic
        || matches!(
            demand.identities.product_map.data_class,
            MaterialDataClass::SyntheticSensitivity
        ) {
        EvidenceClass::Synthetic
    } else {
        EvidenceClass::ModelInformed
    };
    if inventory
        .passports
        .iter()
        .any(|p| p.evidence_class == EvidenceClass::Synthetic)
    {
        evidence = EvidenceClass::Synthetic;
    }
    if record.evidence_class != evidence {
        c.fail(
            "identity",
            format!(
                "evidence_class {:?}, recomputed {:?}",
                record.evidence_class, evidence
            ),
        );
    }
    Ok(c.finish())
}

/// Verifies one stream's pieces, coverage and bookkeeping. Pieces' reel
/// spans are appended to `spans` for the conservation check.
#[allow(clippy::too_many_arguments)]
fn check_stream(
    c: &mut Collector,
    plan: Plan,
    stream: &StreamAllocation,
    demand: &AllocationDemand,
    models: &[ReelModel],
    index_of: &BTreeMap<&str, usize>,
    margin: f64,
    stream_length: f64,
    spans: &mut Vec<(usize, f64, f64)>,
    accepted: &dyn Fn(&ReelModel) -> bool,
) {
    let (group, plan_name): (&'static str, &str) = match plan {
        Plan::Allocation => ("feasibility", "allocation"),
        Plan::Baseline => ("baseline", "baseline"),
    };
    let module = stream.module as usize - 1;
    let label = format!("{plan_name} stream {}", stream.stream);
    let mut expected_from = 0.0_f64;
    for (number, piece) in stream.pieces.iter().enumerate() {
        let Some(&reel) = index_of.get(piece.reel_id.as_str()) else {
            c.fail(
                group,
                format!("{label} piece {number}: unknown reel '{}'", piece.reel_id),
            );
            continue;
        };
        let model = &models[reel];
        if model.code != ReelCode::Eligible {
            c.fail(
                group,
                format!(
                    "{label} piece {number}: reel '{}' is not allocatable",
                    piece.reel_id
                ),
            );
            continue;
        }
        if plan == Plan::Baseline && !accepted(model) {
            c.fail(
                "baseline",
                format!(
                    "{label} piece {number}: reel '{}' was rejected by the baseline",
                    piece.reel_id
                ),
            );
            continue;
        }
        let length = piece.stream_to_m - piece.stream_from_m;
        if length.is_nan()
            || length <= 0.0
            || piece.reel_to_m.is_nan()
            || piece.reel_to_m <= piece.reel_from_m
        {
            c.fail(
                group,
                format!("{label} piece {number}: non-positive length"),
            );
            continue;
        }
        if ((piece.reel_to_m - piece.reel_from_m) - length).abs() > COORDINATE_SLACK_M {
            c.fail(
                group,
                format!("{label} piece {number}: reel and stream lengths differ"),
            );
        }
        if (piece.stream_from_m - expected_from).abs() > COORDINATE_SLACK_M {
            c.fail(
                "coverage",
                format!(
                    "{label} piece {number}: starts at {} m but the previous piece ended at {expected_from} m (gap or overlap)",
                    piece.stream_from_m
                ),
            );
        }
        expected_from = piece.stream_to_m;
        if piece.stream_to_m > stream_length + COORDINATE_SLACK_M {
            c.fail(
                "coverage",
                format!("{label} piece {number}: runs past the stream end"),
            );
        }
        // The piece must lie inside one usable sub-reel.
        let Some((_, _, steps)) = model.subs.iter().find(|(from, to, _)| {
            piece.reel_from_m >= from - COORDINATE_SLACK_M
                && piece.reel_to_m <= to + COORDINATE_SLACK_M
        }) else {
            c.fail(
                "conservation",
                format!(
                    "{label} piece {number}: reel '{}' {} to {} m is not inside a usable span (excluded, cut or outside the profile)",
                    piece.reel_id, piece.reel_from_m, piece.reel_to_m
                ),
            );
            continue;
        };
        spans.push((reel, piece.reel_from_m, piece.reel_to_m));
        let walk = match plan {
            Plan::Allocation => walk_piece(
                demand,
                steps,
                piece.reel_from_m,
                piece.stream_from_m,
                length,
                margin,
                &|turn| base_need(demand, model, module, turn),
            ),
            Plan::Baseline => walk_piece(
                demand,
                steps,
                piece.reel_from_m,
                piece.stream_from_m,
                length,
                margin,
                &|_| model.uniform,
            ),
        };
        if !walk.feasible {
            c.fail(
                group,
                format!(
                    "{label} piece {number}: reel '{}' is not feasible over stream {:.6} to {:.6} m",
                    piece.reel_id, piece.stream_from_m, piece.stream_to_m
                ),
            );
            continue;
        }
        c.eq_float(
            group,
            &format!("{label} piece {number} min_margin_ratio"),
            piece.min_margin_ratio,
            walk.min_ratio,
        );
        c.eq_int(
            group,
            &format!("{label} piece {number} turn_from"),
            piece.turn_from,
            walk.turn_from as u32 + 1,
        );
        c.eq_int(
            group,
            &format!("{label} piece {number} turn_to"),
            piece.turn_to,
            walk.turn_to as u32 + 1,
        );
    }
    let covered = stream.pieces.last().map_or(0.0, |piece| piece.stream_to_m);
    c.eq_float(
        "coverage",
        &format!("{label} covered_m"),
        stream.covered_m,
        covered,
    );
    c.eq_float(
        "coverage",
        &format!("{label} shortfall_m"),
        stream.shortfall_m,
        (stream_length - covered).max(0.0),
    );
    c.eq_int(
        "coverage",
        &format!("{label} splices"),
        stream.splices,
        stream.pieces.len().saturating_sub(1) as u32,
    );
    let complete = stream_length - covered <= LENGTH_TOLERANCE_M;
    match (stream.status, complete, &stream.infeasible_at) {
        (StreamStatus::Covered, true, None) => {}
        (StreamStatus::InfeasibleAt, false, Some(at)) => {
            if (at.stream_m - covered).abs() > COORDINATE_SLACK_M {
                c.fail("coverage", format!("{label}: infeasible_at {} m is not where coverage ends ({covered} m)", at.stream_m));
            }
            if demand
                .turns
                .get(at.turn as usize - 1)
                .is_none_or(|t| at.stream_m + COORDINATE_SLACK_M < t.start_m || at.stream_m > t.start_m + t.length_m + COORDINATE_SLACK_M)
            {
                c.fail("coverage", format!("{label}: infeasible_at turn {} does not contain {} m", at.turn, at.stream_m));
            }
        }
        _ => c.fail(
            "coverage",
            format!("{label}: status {:?} disagrees with the covered length {covered} of {stream_length} m", stream.status),
        ),
    }
}
