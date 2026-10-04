//! Allocation of an inventory's reels to the positions of a coil design
//! (`optcoil-allocation/v1`, CR-03 slice B), with the uniform worst-case
//! baseline. The demand table gives the scale factor s_req each (turn,
//! module) needs relative to the product map; a reel's scale factor s(x)
//! comes from its length profile (CR-02 rule 7). A piece of a reel can be
//! placed where `s_used >= s_req * (1 + margin)` along its whole length.
//!
//! The allocator is a deterministic greedy with no optimality claim. The
//! independent acceptance module (`allocation_check`) does not call anything
//! here except the shared input and record types.

use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path};

use optcoil_model::{
    material::MaterialDataset,
    reel::{EvidenceClass, ProductMapRef, ReelInventory, ReelPassport, passport_sha256},
};
use serde::{Deserialize, Serialize};

use crate::{
    RunError,
    allocation::{ALLOCATION_DEMAND_SCHEMA, AllocationDemand},
    hash,
    reel::{Resolved, resolve_map, scale_series},
};

pub const ALLOCATION_SCHEMA: &str = "optcoil-allocation/v1";
pub const ALLOCATION_PARAMS_SCHEMA: &str = "optcoil-allocation-params/v1";
pub const ALLOCATION_MODEL_ID: &str = "allocation/greedy-step-function/v1";

/// Lengths at or below this (m) are rounding slivers: elementary intervals
/// of that size are not tested, and a head within it of an end is spent.
pub const LENGTH_TOLERANCE_M: f64 = 1e-9;
/// Reel width must equal the demand's tape width within this (m).
pub const WIDTH_TOLERANCE_M: f64 = 1e-9;

fn invalid(message: impl Into<String>) -> RunError {
    RunError::Invalid(message.into())
}

// ---------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransferDerate {
    pub sigma: f64,
    pub z: f64,
}

/// `optcoil-allocation-params/v1`. Every field is required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AllocationParams {
    pub schema: String,
    /// Placement requires `s_used >= s_req * (1 + margin)`.
    pub margin: f64,
    /// Each reel's s is multiplied by `exp(-z * sigma)`.
    pub transfer_derate: TransferDerate,
    pub min_piece_length_m: f64,
    pub price_usd_per_m: f64,
    pub price_source: String,
}

impl AllocationParams {
    pub fn from_json(json: &str) -> Result<Self, RunError> {
        let params: Self = serde_json::from_str(json)
            .map_err(|e| invalid(format!("allocation params do not parse: {e}")))?;
        params.validate()?;
        Ok(params)
    }

    pub fn validate(&self) -> Result<(), RunError> {
        if self.schema != ALLOCATION_PARAMS_SCHEMA {
            return Err(invalid(format!(
                "params schema must be '{ALLOCATION_PARAMS_SCHEMA}'"
            )));
        }
        let finite_min =
            |v: f64, lo: f64, strict: bool| v.is_finite() && if strict { v > lo } else { v >= lo };
        if !finite_min(self.margin, 0.0, false) {
            return Err(invalid("margin must be finite and at least 0"));
        }
        if !finite_min(self.transfer_derate.sigma, 0.0, false)
            || !finite_min(self.transfer_derate.z, 0.0, false)
        {
            return Err(invalid(
                "transfer_derate sigma and z must be finite and at least 0",
            ));
        }
        if !finite_min(self.min_piece_length_m, 0.0, true) {
            return Err(invalid("min_piece_length_m must be finite and above 0"));
        }
        if !finite_min(self.price_usd_per_m, 0.0, true) {
            return Err(invalid("price_usd_per_m must be finite and above 0"));
        }
        if self.price_source.trim().is_empty() {
            return Err(invalid("price_source must name where the price comes from"));
        }
        Ok(())
    }

    /// `exp(-z * sigma)`.
    pub fn derate_factor(&self) -> f64 {
        (-self.transfer_derate.z * self.transfer_derate.sigma).exp()
    }

    /// True when the derate does nothing (sigma or z is 0).
    pub fn derate_is_unsafe(&self) -> bool {
        self.transfer_derate.sigma == 0.0 || self.transfer_derate.z == 0.0
    }
}

/// Parses and checks a demand document.
pub fn parse_demand(json: &str) -> Result<AllocationDemand, RunError> {
    let demand: AllocationDemand = serde_json::from_str(json)
        .map_err(|e| invalid(format!("allocation demand does not parse: {e}")))?;
    if demand.schema != ALLOCATION_DEMAND_SCHEMA {
        return Err(invalid(format!(
            "demand schema must be '{ALLOCATION_DEMAND_SCHEMA}'"
        )));
    }
    if demand.gate.status != optcoil_model::Status::Pass {
        return Err(invalid("the demand's offset-0 gate did not pass"));
    }
    let offsets = &demand.angle_offsets_deg;
    if offsets.first() != Some(&0.0) || offsets.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid(
            "demand angle offsets must start at 0 and ascend strictly",
        ));
    }
    let (n, p) = (
        demand.geometry.turns_along_normal as usize,
        demand.geometry.modules as usize,
    );
    if n == 0 || p == 0 || demand.turns.len() != n || demand.geometry.strands == 0 {
        return Err(invalid(
            "demand turn, module or strand counts are inconsistent",
        ));
    }
    let mut expected_start = 0.0_f64;
    for (index, turn) in demand.turns.iter().enumerate() {
        if turn.turn as usize != index + 1
            || !(turn.length_m.is_finite() && turn.length_m > 0.0)
            || (turn.start_m - expected_start).abs() > LENGTH_TOLERANCE_M
            || turn.modules.len() != p
        {
            return Err(invalid(format!(
                "demand turn {} is inconsistent",
                index + 1
            )));
        }
        expected_start = turn.start_m + turn.length_m;
        for module in &turn.modules {
            if module.s_req.len() != offsets.len() {
                return Err(invalid(format!(
                    "demand turn {} has a module with the wrong number of offsets",
                    index + 1
                )));
            }
        }
    }
    Ok(demand)
}

// ---------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReelCode {
    Eligible,
    NoProductMap,
    MapMismatch,
    MapUnavailable,
    WidthMismatch,
    ProfileUnscalable,
    NoUsableLength,
    OffsetSensitiveEverywhere,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReelUse {
    Used,
    PartlyUsed,
    UnusedEligible,
    Unallocatable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BaselineReelStatus {
    Accepted,
    /// Eligible but below the uniform requirement: scrap under the baseline.
    Rejected,
    Unallocatable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReelAllocation {
    pub reel_id: String,
    pub code: ReelCode,
    pub usage: ReelUse,
    pub evidence_class: EvidenceClass,
    pub explanation: Option<String>,
    pub next_evidence: Option<String>,
    pub length_m: f64,
    pub usable_length_m: f64,
    /// Largest |ab offset| + uncertainty on the reel; none without offsets.
    pub offset_bound_deg: Option<f64>,
    /// The tabulated offset the reel is evaluated at; none means the reel
    /// can only be placed at positions that are not offset sensitive.
    pub delta_deg: Option<f64>,
    pub profile_id: Option<String>,
    pub map_reference_row: Option<u32>,
    /// Smallest s_used (after the derate) over the usable length.
    pub min_s_used: Option<f64>,
    pub used_length_m: f64,
    pub remnant_length_m: f64,
    pub baseline: BaselineReelStatus,
    /// The uniform requirement the baseline applied to this reel
    /// (max s_req over the whole coil at its offset); none if unplaceable
    /// somewhere.
    pub baseline_s_req: Option<f64>,
    pub baseline_used_length_m: f64,
    /// Rejected (scrap) under the baseline yet used by the allocation.
    pub scrapped_by_baseline_but_used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Piece {
    pub reel_id: String,
    pub reel_from_m: f64,
    pub reel_to_m: f64,
    pub stream_from_m: f64,
    pub stream_to_m: f64,
    pub turn_from: u32,
    pub turn_to: u32,
    /// Smallest `s_used / s_req` within the piece.
    pub min_margin_ratio: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    Covered,
    InfeasibleAt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfeasibleAt {
    pub turn: u32,
    pub stream_m: f64,
    /// s_req at offset 0 here; none if the position is blocked in the demand.
    pub s_req: Option<f64>,
    /// The best s_used any remaining sub-reel offers at its head.
    pub best_s_used: Option<f64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAllocation {
    /// 0-based stream index: `(module - 1) * strands + (strand - 1)`.
    pub stream: u32,
    pub module: u32,
    pub strand: u32,
    /// Peak s_req over the stream at offset 0; none if any position is
    /// blocked.
    pub peak_s_req: Option<f64>,
    pub pieces: Vec<Piece>,
    pub covered_m: f64,
    pub shortfall_m: f64,
    pub status: StreamStatus,
    pub infeasible_at: Option<InfeasibleAt>,
    pub splices: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accounting {
    pub streams_covered: u32,
    pub streams_infeasible: u32,
    pub pieces: u32,
    pub splices: u32,
    pub allocated_m: f64,
    /// Usable length of the reels available to this plan, not allocated.
    pub remnant_m: f64,
    /// Usable length of eligible reels the plan rejected as scrap
    /// (baseline only; 0 for the allocation).
    pub scrap_m: f64,
    /// Stream length left uncovered: conductor that would have to be bought.
    pub shortfall_m: f64,
    pub shortfall_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReelCounts {
    pub used: u32,
    pub partly_used: u32,
    pub unused_eligible: u32,
    pub unallocatable: u32,
    pub scrapped_by_baseline_but_used: u32,
    pub baseline_accepted: u32,
    pub baseline_rejected: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    pub price_usd_per_m: f64,
    pub price_source: String,
    /// Baseline shortfall cost minus allocation shortfall cost: what the
    /// allocation saves at this price. Reels already in stock are not priced.
    pub difference_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationInputs {
    pub demand_sha256: String,
    pub inventory_sha256: String,
    pub params_sha256: String,
    pub inventory_id: String,
    pub demand_record_sha256: String,
    pub product_map: ProductMapRef,
    pub tape_width_m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationRecord {
    pub schema: String,
    pub optcoil_version: String,
    pub model_id: String,
    pub implementation_sha256: String,
    pub inputs: AllocationInputs,
    pub params: AllocationParams,
    pub derate_factor: f64,
    pub evidence_class: EvidenceClass,
    pub angle_offsets_deg: Vec<f64>,
    pub streams_total: u32,
    pub stream_length_m: f64,
    pub reels: Vec<ReelAllocation>,
    /// In fill order (descending peak s_req).
    pub streams: Vec<StreamAllocation>,
    /// In stream index order.
    pub baseline_streams: Vec<StreamAllocation>,
    pub allocation: Accounting,
    pub baseline: Accounting,
    pub reel_counts: ReelCounts,
    pub money: Money,
    pub limitations: Vec<String>,
}

impl AllocationRecord {
    pub fn to_json(&self) -> Result<String, RunError> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }

    pub fn write_new(&self, path: impl AsRef<Path>) -> Result<(), RunError> {
        let bytes = self.to_json()?;
        let path = path.as_ref();
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        if let Err(error) = file
            .write_all(bytes.as_bytes())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(error.into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Reel preparation
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Segment {
    a: f64,
    b: f64,
    /// s after the transfer derate.
    s: f64,
}

#[derive(Debug, Clone)]
struct SubReel {
    reel: usize,
    to: f64,
    head: f64,
    segments: Vec<Segment>,
}

struct Prepared {
    code: ReelCode,
    explanation: Option<String>,
    next_evidence: Option<String>,
    delta_index: Option<usize>,
    offset_bound: Option<f64>,
    profile_id: Option<String>,
    map_reference_row: Option<u32>,
    min_s_used: Option<f64>,
    /// The baseline's uniform requirement: max s_req over the whole coil at
    /// this reel's offset; none when it cannot be placed somewhere.
    uniform: Option<f64>,
    usable_length: f64,
    sub_reels: Vec<(f64, f64, Vec<Segment>)>,
}

fn unallocatable(
    code: ReelCode,
    explanation: impl Into<String>,
    next_evidence: impl Into<String>,
) -> Prepared {
    Prepared {
        code,
        explanation: Some(explanation.into()),
        next_evidence: Some(next_evidence.into()),
        delta_index: None,
        offset_bound: None,
        profile_id: None,
        map_reference_row: None,
        min_s_used: None,
        uniform: None,
        usable_length: 0.0,
        sub_reels: Vec::new(),
    }
}

fn offset_bound(passport: &ReelPassport) -> Option<f64> {
    passport
        .ab_offsets
        .iter()
        .map(|o| o.offset_deg.abs() + o.uncertainty_deg)
        .reduce(f64::max)
}

fn prepare_reel(
    passport: &ReelPassport,
    demand: &AllocationDemand,
    factor: f64,
    map: &Resolved,
    all_positions_sensitive: bool,
) -> Prepared {
    let Some(reference) = &passport.product.product_map else {
        return unallocatable(
            ReelCode::NoProductMap,
            "The passport names no product_map, so its length profile cannot be scaled against the demand's product map.",
            "Add a product_map that names the demand's product map.",
        );
    };
    let wanted = &demand.identities.product_map;
    if reference.dataset_id != wanted.dataset_id || reference.csv_sha256 != wanted.csv_sha256 {
        return unallocatable(
            ReelCode::MapMismatch,
            format!(
                "The reel's product map '{}' is not the demand's product map '{}'. v1 allocates one product map only.",
                reference.dataset_id, wanted.dataset_id
            ),
            "A reel of the demand's product, or a demand computed with the reel's product map.",
        );
    }
    let model = match map {
        Resolved::Model(model) => model,
        Resolved::Unavailable(reason) => {
            return unallocatable(
                ReelCode::MapUnavailable,
                format!("The product map is unavailable: {reason}."),
                "Supply the dataset bundle whose id and csv_sha256 match the demand's product map.",
            );
        }
    };
    if (passport.geometry.width_m - demand.geometry.tape_width_m).abs() > WIDTH_TOLERANCE_M {
        return unallocatable(
            ReelCode::WidthMismatch,
            format!(
                "The reel width {} m differs from the coil's tape width {} m by more than {WIDTH_TOLERANCE_M} m.",
                passport.geometry.width_m, demand.geometry.tape_width_m
            ),
            "A reel of the coil's tape width.",
        );
    }
    let series = match scale_series(passport, model) {
        Ok(series) => series,
        Err(reason) => {
            return unallocatable(
                ReelCode::ProfileUnscalable,
                format!("No scaled rating basis (CR-02): {reason}"),
                "A length profile measured at a condition the product map covers, or a map_reference to a matching map row.",
            );
        }
    };

    // Step function between profile points: the smaller endpoint value.
    let points = &series.points;
    let mut steps: Vec<Segment> = Vec::new();
    for pair in points.windows(2) {
        steps.push(Segment {
            a: pair[0].0,
            b: pair[1].0,
            s: pair[0].1.min(pair[1].1) * factor,
        });
    }
    // Usable region: the profile's coverage minus excluded and cut spans.
    let mut usable: Vec<(f64, f64)> = Vec::new();
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        let mut cursor = first.0;
        for (start, end) in passport.unusable_spans() {
            if start > cursor {
                usable.push((cursor, start.min(last.0)));
            }
            cursor = cursor.max(end);
        }
        if cursor < last.0 {
            usable.push((cursor, last.0));
        }
    }
    usable.retain(|(a, b)| b - a > LENGTH_TOLERANCE_M);
    let mut sub_reels = Vec::new();
    let mut usable_length = 0.0;
    let mut min_s = f64::INFINITY;
    for (from, to) in usable {
        let segments: Vec<Segment> = steps
            .iter()
            .filter(|seg| seg.b > from && seg.a < to)
            .map(|seg| Segment {
                a: seg.a.max(from),
                b: seg.b.min(to),
                s: seg.s,
            })
            .filter(|seg| seg.b - seg.a > 0.0)
            .collect();
        for seg in &segments {
            min_s = min_s.min(seg.s);
        }
        usable_length += to - from;
        sub_reels.push((from, to, segments));
    }
    if sub_reels.is_empty() {
        return unallocatable(
            ReelCode::NoUsableLength,
            "No usable length remains: the length profile's coverage is empty or lies inside excluded or cut spans.",
            "A length profile with points in the usable length of the reel.",
        );
    }

    let bound = offset_bound(passport);
    let delta_index = bound.and_then(|b| demand.angle_offsets_deg.iter().position(|d| *d >= b));
    if delta_index.is_none() && all_positions_sensitive {
        let why = match bound {
            None => "The reel has no ab-plane offsets".to_string(),
            Some(b) => format!(
                "The reel's offset bound {b} degrees exceeds the largest tabulated offset {} degrees",
                demand.angle_offsets_deg.last().copied().unwrap_or(0.0)
            ),
        };
        return unallocatable(
            ReelCode::OffsetSensitiveEverywhere,
            format!(
                "{why}, and every position of this coil lies within 15 degrees of the tape plane, so no position can take it."
            ),
            "Measured ab-plane offsets for this reel within the tabulated range, or a demand tabulated to larger offsets.",
        );
    }
    Prepared {
        code: ReelCode::Eligible,
        explanation: None,
        next_evidence: None,
        delta_index,
        offset_bound: bound,
        profile_id: Some(series.profile_id),
        map_reference_row: series.map_reference_row,
        min_s_used: Some(min_s),
        uniform: None,
        usable_length,
        sub_reels,
    }
}

// ---------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------

struct Context<'a> {
    demand: &'a AllocationDemand,
    margin: f64,
    min_piece: f64,
    turn_ends: Vec<f64>,
    stream_length: f64,
    reels: &'a [Prepared],
    /// Requirement base (before the margin) for a reel at (module, turn),
    /// or None when the reel cannot be placed there.
    requirement: &'a dyn Fn(&Prepared, usize, usize) -> Option<f64>,
}

impl Context<'_> {
    /// The turn index (0-based) containing stream coordinate `x`.
    fn turn_at(&self, x: f64) -> usize {
        let index = self.turn_ends.partition_point(|end| *end <= x);
        index.min(self.turn_ends.len() - 1)
    }
}

struct RunEval {
    run: f64,
    mean_surplus: f64,
    min_ratio: f64,
    turn_from: usize,
    turn_to: usize,
}

fn segment_at(segments: &[Segment], x: f64) -> &Segment {
    let index = segments.partition_point(|seg| seg.b <= x);
    &segments[index.min(segments.len() - 1)]
}

/// The longest feasible run from sub-reel head `sub.head` placed at stream
/// coordinate `ell`, up to `limit`, evaluated exactly on the merged
/// breakpoints of the reel's step function and the turn boundaries.
fn evaluate_run(
    ctx: &Context,
    sub: &SubReel,
    prepared: &Prepared,
    module: usize,
    ell: f64,
    limit: f64,
) -> RunEval {
    let mut cuts = vec![0.0, limit];
    for seg in &sub.segments {
        for edge in [seg.a, seg.b] {
            let u = edge - sub.head;
            if u > 0.0 && u < limit {
                cuts.push(u);
            }
        }
    }
    for end in &ctx.turn_ends {
        let u = end - ell;
        if u > 0.0 && u < limit {
            cuts.push(u);
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();
    let mut run = limit;
    let (mut weighted, mut measured, mut min_ratio) = (0.0, 0.0, f64::INFINITY);
    let (mut turn_from, mut turn_to) = (None, 0);
    for pair in cuts.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        if hi - lo <= LENGTH_TOLERANCE_M {
            continue;
        }
        let mid = 0.5 * (lo + hi);
        let turn = ctx.turn_at(ell + mid);
        let s_used = segment_at(&sub.segments, sub.head + mid).s;
        let feasible = (ctx.requirement)(prepared, module, turn)
            .filter(|base| s_used >= base * (1.0 + ctx.margin));
        let Some(base) = feasible else {
            run = lo;
            break;
        };
        let ratio = s_used / base;
        weighted += ratio * (hi - lo);
        measured += hi - lo;
        min_ratio = min_ratio.min(ratio);
        turn_from.get_or_insert(turn);
        turn_to = turn;
    }
    if run <= LENGTH_TOLERANCE_M || measured == 0.0 {
        return RunEval {
            run: 0.0,
            mean_surplus: f64::INFINITY,
            min_ratio: 0.0,
            turn_from: 0,
            turn_to: 0,
        };
    }
    RunEval {
        run,
        mean_surplus: weighted / measured,
        min_ratio,
        turn_from: turn_from.unwrap_or(0),
        turn_to,
    }
}

struct StreamSpec {
    stream: u32,
    module: usize,
    strand: u32,
    peak_s_req: Option<f64>,
}

/// Fills one stream from coordinate 0, advancing the sub-reel heads it uses.
fn fill_stream(
    ctx: &Context,
    subs: &mut [SubReel],
    ids: &[String],
    spec: &StreamSpec,
) -> StreamAllocation {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut ell = 0.0_f64;
    let mut infeasible_at = None;
    while ctx.stream_length - ell > LENGTH_TOLERANCE_M {
        let remaining = ctx.stream_length - ell;
        let need = remaining.min(ctx.min_piece);
        struct Candidate {
            index: usize,
            eval: RunEval,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut best_s_used: Option<f64> = None;
        let mut any_material = false;
        for (index, sub) in subs.iter().enumerate() {
            let left = sub.to - sub.head;
            if left <= LENGTH_TOLERANCE_M {
                continue;
            }
            any_material = true;
            let s_head = segment_at(&sub.segments, sub.head).s;
            best_s_used = Some(best_s_used.map_or(s_head, |b: f64| b.max(s_head)));
            let eval = evaluate_run(
                ctx,
                sub,
                &ctx.reels[sub.reel],
                spec.module,
                ell,
                remaining.min(left),
            );
            if eval.run > 0.0 {
                candidates.push(Candidate { index, eval });
            }
        }
        let by_id = |a: &Candidate, b: &Candidate| {
            ids[subs[a.index].reel]
                .cmp(&ids[subs[b.index].reel])
                .then(subs[a.index].head.total_cmp(&subs[b.index].head))
        };
        let pick = {
            let qualifying: Vec<&Candidate> = candidates
                .iter()
                .filter(|c| c.eval.run + LENGTH_TOLERANCE_M >= need)
                .collect();
            if qualifying.is_empty() {
                candidates
                    .iter()
                    .min_by(|a, b| b.eval.run.total_cmp(&a.eval.run).then_with(|| by_id(a, b)))
            } else {
                qualifying.into_iter().min_by(|a, b| {
                    a.eval
                        .mean_surplus
                        .total_cmp(&b.eval.mean_surplus)
                        .then(b.eval.run.total_cmp(&a.eval.run))
                        .then_with(|| by_id(a, b))
                })
            }
        };
        let Some(chosen) = pick else {
            let turn = ctx.turn_at(ell + LENGTH_TOLERANCE_M);
            let s_req = ctx.demand.turns[turn].modules[spec.module].s_req[0].s_req;
            let reason = if !any_material {
                "inventory_exhausted: no usable reel length is left"
            } else if s_req.is_none() {
                "blocked_position: the demand has no requirement value here"
            } else {
                "no_feasible_reel: no remaining reel meets the requirement at this position"
            };
            infeasible_at = Some(InfeasibleAt {
                turn: turn as u32 + 1,
                stream_m: ell,
                s_req,
                best_s_used,
                reason: reason.to_string(),
            });
            break;
        };
        let sub = &mut subs[chosen.index];
        let left = sub.to - sub.head;
        let run = chosen.eval.run;
        let full = run >= remaining.min(left);
        let (stream_to, reel_to) = if full && remaining <= left {
            (
                ctx.stream_length,
                if remaining == left {
                    sub.to
                } else {
                    sub.head + remaining
                },
            )
        } else if full {
            (ell + left, sub.to)
        } else {
            (ell + run, sub.head + run)
        };
        pieces.push(Piece {
            reel_id: ids[sub.reel].clone(),
            reel_from_m: sub.head,
            reel_to_m: reel_to,
            stream_from_m: ell,
            stream_to_m: stream_to,
            turn_from: chosen.eval.turn_from as u32 + 1,
            turn_to: chosen.eval.turn_to as u32 + 1,
            min_margin_ratio: chosen.eval.min_ratio,
        });
        sub.head = reel_to;
        ell = stream_to;
    }
    let covered_m = pieces.last().map_or(0.0, |p| p.stream_to_m);
    let status = if infeasible_at.is_some() {
        StreamStatus::InfeasibleAt
    } else {
        StreamStatus::Covered
    };
    StreamAllocation {
        stream: spec.stream,
        module: spec.module as u32 + 1,
        strand: spec.strand,
        peak_s_req: spec.peak_s_req,
        splices: pieces.len().saturating_sub(1) as u32,
        shortfall_m: (ctx.stream_length - covered_m).max(0.0),
        covered_m,
        pieces,
        status,
        infeasible_at,
    }
}

// ---------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------

/// Runs the allocation and the baseline. `inventory_json`, `demand_json` and
/// `params_json` are the exact document text; their SHA-256 are recorded.
/// `bundle_jsons` supply product-map datasets (else the embedded store).
pub fn run_allocation_json(
    demand_json: &str,
    inventory_json: &str,
    params_json: &str,
    bundle_jsons: &[&str],
) -> Result<AllocationRecord, RunError> {
    let demand = parse_demand(demand_json)?;
    let inventory = ReelInventory::from_json(inventory_json)?;
    let params = AllocationParams::from_json(params_json)?;
    let datasets = bundle_jsons
        .iter()
        .map(|bundle| MaterialDataset::from_bundle_json(bundle))
        .collect::<Result<Vec<_>, _>>()?;
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
    build_record(&demand, &inventory, &params, &datasets, inputs)
}

/// Resolves the demand's product map from supplied datasets or the embedded
/// store, under the identity contract.
pub(crate) fn resolve_demand_map(
    demand: &AllocationDemand,
    datasets: &[MaterialDataset],
) -> Resolved {
    let supplied: BTreeMap<String, MaterialDataset> = datasets
        .iter()
        .map(|d| (d.metadata.id.clone(), d.clone()))
        .collect();
    resolve_map(
        &demand.identities.product_map.dataset_id,
        &demand.identities.product_map.csv_sha256,
        &supplied,
    )
}

fn build_record(
    demand: &AllocationDemand,
    inventory: &ReelInventory,
    params: &AllocationParams,
    datasets: &[MaterialDataset],
    inputs: AllocationInputs,
) -> Result<AllocationRecord, RunError> {
    let factor = params.derate_factor();
    let n = demand.turns.len();
    let p = demand.geometry.modules as usize;
    let strands = demand.geometry.strands;
    let all_positions_sensitive = demand
        .turns
        .iter()
        .all(|t| t.modules.iter().all(|m| m.offset_sensitive));
    let map = resolve_demand_map(demand, datasets);
    let mut prepared: Vec<Prepared> = inventory
        .passports
        .iter()
        .map(|passport| prepare_reel(passport, demand, factor, &map, all_positions_sensitive))
        .collect();
    let ids: Vec<String> = inventory
        .passports
        .iter()
        .map(|passport| passport.reel_id.clone())
        .collect();
    let turn_ends: Vec<f64> = demand
        .turns
        .iter()
        .map(|t| t.start_m + t.length_m)
        .collect();
    let stream_length = turn_ends[n - 1];

    // The demand's requirement for a reel at (module, turn).
    let position_requirement = |reel: &Prepared, module: usize, turn: usize| -> Option<f64> {
        let entry = &demand.turns[turn].modules[module];
        match reel.delta_index {
            Some(index) => entry.s_req[index].s_req,
            None if entry.offset_sensitive => None,
            None => entry.s_req[0].s_req,
        }
    };

    // Streams: one per (module, strand), ordered by descending peak s_req.
    let mut specs: Vec<StreamSpec> = Vec::new();
    for module in 0..p {
        let mut peak = Some(0.0_f64);
        for turn in &demand.turns {
            match (peak, turn.modules[module].s_req[0].s_req) {
                (Some(current), Some(value)) => peak = Some(current.max(value)),
                _ => peak = None,
            }
        }
        for strand in 0..strands {
            specs.push(StreamSpec {
                stream: module as u32 * strands + strand,
                module,
                strand: strand + 1,
                peak_s_req: peak,
            });
        }
    }
    let rank = |s: &StreamSpec| s.peak_s_req.unwrap_or(f64::INFINITY);
    let mut fill_order: Vec<usize> = (0..specs.len()).collect();
    fill_order.sort_by(|&a, &b| {
        rank(&specs[b])
            .total_cmp(&rank(&specs[a]))
            .then(specs[a].stream.cmp(&specs[b].stream))
    });

    // Baseline: uniform worst-case specification.
    let mut uniform: Vec<Option<f64>> = vec![None; prepared.len()];
    let mut accepted = vec![false; prepared.len()];
    for (index, reel) in prepared.iter().enumerate() {
        if reel.code != ReelCode::Eligible {
            continue;
        }
        let mut worst = Some(0.0_f64);
        'coil: for module in 0..p {
            for turn in 0..n {
                match position_requirement(reel, module, turn) {
                    Some(value) => worst = worst.map(|w| w.max(value)),
                    None => {
                        worst = None;
                        break 'coil;
                    }
                }
            }
        }
        uniform[index] = worst;
        accepted[index] = worst.is_some_and(|w| {
            reel.min_s_used
                .is_some_and(|s| s >= w * (1.0 + params.margin))
        });
    }
    for (reel, value) in prepared.iter_mut().zip(&uniform) {
        reel.uniform = *value;
    }

    let fresh_subs = |accept: &dyn Fn(usize) -> bool| -> Vec<SubReel> {
        let mut subs = Vec::new();
        for (index, reel) in prepared.iter().enumerate() {
            if reel.code != ReelCode::Eligible || !accept(index) {
                continue;
            }
            for (from, to, segments) in &reel.sub_reels {
                subs.push(SubReel {
                    reel: index,
                    to: *to,
                    head: *from,
                    segments: segments.clone(),
                });
            }
        }
        subs
    };

    // Allocation.
    let mut subs = fresh_subs(&|_| true);
    let allocation_ctx = Context {
        demand,
        margin: params.margin,
        min_piece: params.min_piece_length_m,
        turn_ends: turn_ends.clone(),
        stream_length,
        reels: &prepared,
        requirement: &position_requirement,
    };
    let mut filled: Vec<StreamAllocation> = Vec::with_capacity(specs.len());
    for &index in &fill_order {
        filled.push(fill_stream(&allocation_ctx, &mut subs, &ids, &specs[index]));
    }

    let uniform_requirement =
        |reel: &Prepared, _module: usize, _turn: usize| -> Option<f64> { reel.uniform };
    let mut baseline_subs = fresh_subs(&|index| accepted[index]);
    let baseline_ctx = Context {
        demand,
        margin: params.margin,
        min_piece: params.min_piece_length_m,
        turn_ends,
        stream_length,
        reels: &prepared,
        requirement: &uniform_requirement,
    };
    let mut baseline_streams: Vec<StreamAllocation> = Vec::with_capacity(specs.len());
    for spec in &specs {
        baseline_streams.push(fill_stream(&baseline_ctx, &mut baseline_subs, &ids, spec));
    }
    filled.sort_by_key(|s| s.stream);
    let allocation_streams = {
        // Keep the fill order in the record.
        let mut ordered = Vec::with_capacity(filled.len());
        for &index in &fill_order {
            let stream = specs[index].stream;
            ordered.push(
                filled
                    .iter()
                    .find(|s| s.stream == stream)
                    .expect("every stream was filled")
                    .clone(),
            );
        }
        ordered
    };

    // Per-reel usage.
    let mut used: BTreeMap<&str, f64> = BTreeMap::new();
    let mut baseline_used: BTreeMap<&str, f64> = BTreeMap::new();
    for stream in &allocation_streams {
        for piece in &stream.pieces {
            *used.entry(piece.reel_id.as_str()).or_insert(0.0) +=
                piece.reel_to_m - piece.reel_from_m;
        }
    }
    for stream in &baseline_streams {
        for piece in &stream.pieces {
            *baseline_used.entry(piece.reel_id.as_str()).or_insert(0.0) +=
                piece.reel_to_m - piece.reel_from_m;
        }
    }
    let mut evidence = if inventory.evidence_class == EvidenceClass::Synthetic
        || matches!(
            demand.identities.product_map.data_class,
            optcoil_model::material::MaterialDataClass::SyntheticSensitivity
        ) {
        EvidenceClass::Synthetic
    } else {
        EvidenceClass::ModelInformed
    };
    let mut reels = Vec::with_capacity(prepared.len());
    let mut counts = ReelCounts {
        used: 0,
        partly_used: 0,
        unused_eligible: 0,
        unallocatable: 0,
        scrapped_by_baseline_but_used: 0,
        baseline_accepted: 0,
        baseline_rejected: 0,
    };
    for (index, (passport, reel)) in inventory.passports.iter().zip(&prepared).enumerate() {
        let used_m = used.get(passport.reel_id.as_str()).copied().unwrap_or(0.0);
        let baseline_used_m = baseline_used
            .get(passport.reel_id.as_str())
            .copied()
            .unwrap_or(0.0);
        let eligible = reel.code == ReelCode::Eligible;
        let usage = if !eligible {
            ReelUse::Unallocatable
        } else if used_m <= LENGTH_TOLERANCE_M {
            ReelUse::UnusedEligible
        } else if reel.usable_length - used_m <= LENGTH_TOLERANCE_M * reel.sub_reels.len() as f64 {
            ReelUse::Used
        } else {
            ReelUse::PartlyUsed
        };
        let baseline = if !eligible {
            BaselineReelStatus::Unallocatable
        } else if accepted[index] {
            BaselineReelStatus::Accepted
        } else {
            BaselineReelStatus::Rejected
        };
        match usage {
            ReelUse::Used => counts.used += 1,
            ReelUse::PartlyUsed => counts.partly_used += 1,
            ReelUse::UnusedEligible => counts.unused_eligible += 1,
            ReelUse::Unallocatable => counts.unallocatable += 1,
        }
        let scrapped_used = baseline == BaselineReelStatus::Rejected && used_m > LENGTH_TOLERANCE_M;
        match baseline {
            BaselineReelStatus::Accepted => counts.baseline_accepted += 1,
            BaselineReelStatus::Rejected => counts.baseline_rejected += 1,
            BaselineReelStatus::Unallocatable => {}
        }
        if scrapped_used {
            counts.scrapped_by_baseline_but_used += 1;
        }
        if passport.evidence_class == EvidenceClass::Synthetic {
            evidence = EvidenceClass::Synthetic;
        }
        reels.push(ReelAllocation {
            reel_id: passport.reel_id.clone(),
            code: reel.code,
            usage,
            evidence_class: passport.evidence_class,
            explanation: reel.explanation.clone(),
            next_evidence: reel.next_evidence.clone(),
            length_m: passport.geometry.length_m,
            usable_length_m: reel.usable_length,
            offset_bound_deg: reel.offset_bound,
            delta_deg: reel.delta_index.map(|i| demand.angle_offsets_deg[i]),
            profile_id: reel.profile_id.clone(),
            map_reference_row: reel.map_reference_row,
            min_s_used: reel.min_s_used,
            used_length_m: used_m,
            remnant_length_m: if eligible {
                (reel.usable_length - used_m).max(0.0)
            } else {
                0.0
            },
            baseline,
            baseline_s_req: uniform[index],
            baseline_used_length_m: baseline_used_m,
            scrapped_by_baseline_but_used: scrapped_used,
        });
    }

    let account = |streams: &[StreamAllocation], pool_usable: f64, scrap_m: f64| -> Accounting {
        let allocated: f64 = streams
            .iter()
            .flat_map(|s| &s.pieces)
            .map(|piece| piece.reel_to_m - piece.reel_from_m)
            .fold(0.0, |total, length| total + length);
        let shortfall: f64 = streams.iter().map(|s| s.shortfall_m).sum();
        Accounting {
            streams_covered: streams
                .iter()
                .filter(|s| s.status == StreamStatus::Covered)
                .count() as u32,
            streams_infeasible: streams
                .iter()
                .filter(|s| s.status == StreamStatus::InfeasibleAt)
                .count() as u32,
            pieces: streams.iter().map(|s| s.pieces.len() as u32).sum(),
            splices: streams.iter().map(|s| s.splices).sum(),
            allocated_m: allocated,
            remnant_m: (pool_usable - allocated).max(0.0),
            scrap_m,
            shortfall_m: shortfall,
            shortfall_usd: shortfall * params.price_usd_per_m,
        }
    };
    let eligible_usable: f64 = prepared
        .iter()
        .filter(|r| r.code == ReelCode::Eligible)
        .map(|r| r.usable_length)
        .sum();
    let accepted_usable: f64 = prepared
        .iter()
        .zip(&accepted)
        .filter(|(_, a)| **a)
        .map(|(r, _)| r.usable_length)
        .sum();
    let allocation = account(&allocation_streams, eligible_usable, 0.0);
    let baseline = account(
        &baseline_streams,
        accepted_usable,
        eligible_usable - accepted_usable,
    );
    let money = Money {
        price_usd_per_m: params.price_usd_per_m,
        price_source: params.price_source.clone(),
        difference_usd: baseline.shortfall_usd - allocation.shortfall_usd,
    };

    Ok(AllocationRecord {
        schema: ALLOCATION_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        model_id: ALLOCATION_MODEL_ID.into(),
        implementation_sha256: implementation_hash()?,
        inputs,
        params: params.clone(),
        derate_factor: factor,
        evidence_class: evidence,
        angle_offsets_deg: demand.angle_offsets_deg.clone(),
        streams_total: specs.len() as u32,
        stream_length_m: stream_length,
        reels,
        streams: allocation_streams,
        baseline_streams,
        allocation,
        baseline,
        reel_counts: counts,
        money,
        limitations: limitations(params, evidence),
    })
}

fn limitations(params: &AllocationParams, evidence: EvidenceClass) -> Vec<String> {
    let mut list = vec![
        "Allocation rests on scaled ratings (CR-02 rule 7): a reel's deviation at its profile condition is assumed to carry over to the coil's operating condition. That transfer is untested, so no result here is better than model_informed.".to_string(),
        "The allocator is a deterministic greedy with no optimality claim. The baseline uses the same piece rules with a position-independent requirement.".to_string(),
        "Between profile points a reel's capacity is the smaller endpoint value. Length outside the profile's coverage, and excluded or cut spans, is not used. Accepted defect spans stay in with their profile values.".to_string(),
        "Streams are the demand's (module, strand) conductors, each running through turns 1 to n. Module joints are structural and are not counted as splices here. Pieces within a stream are joined by splices.".to_string(),
        "Offsets: a reel is evaluated at the smallest tabulated offset bound at or above its largest |ab offset| plus uncertainty. A reel with no ab offsets, or a bound beyond the largest tabulated offset, is placed only at positions that are not offset sensitive (within 15 degrees of the tape plane). No flip choice is made.".to_string(),
        "Positions the demand blocks (outside the map's domain, or along-current excluded) take no reel; a stream reaching one is infeasible_at there.".to_string(),
        "Money values only the shortfall, at the declared price and source; stock already owned is not priced, and scrap is not credited.".to_string(),
        format!(
            "Placement requires s_used >= s_req x (1 + {}). Elementary intervals of {LENGTH_TOLERANCE_M} m or less are rounding slivers and are not tested.",
            params.margin
        ),
        format!("Evidence class: {}.", evidence.as_str()),
    ];
    if params.derate_is_unsafe() {
        list.push("UNSAFE: no transfer derate. sigma or z is 0, so reel scale factors are used as read from the length profile with no allowance for the published 13 to 20 percent within-product transfer scatter at 20 K (CR-02).".to_string());
    }
    list
}

fn implementation_hash() -> Result<String, RunError> {
    Ok(hash(&serde_json::to_vec(&(
        include_str!("allocation_run.rs"),
        include_str!("allocation.rs"),
        include_str!("reel.rs"),
        include_str!("../../optcoil-model/src/reel.rs"),
        include_str!("../../../Cargo.lock"),
        include_str!("../../../rust-toolchain.toml"),
    ))?))
}
