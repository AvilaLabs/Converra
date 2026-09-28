//! First-order search-space suggestion for the authoring wizard.
//!
//! `sizing_estimate` runs *one* field evaluation — the case's baseline
//! geometry at the bore probe, lowest declared quadrature order — plus
//! coarse `Ic` lookups at two honest field proxies, and turns that into
//! a suggested `(turns, tapes, strands)` choice grid. It is a sizing
//! hint only: it does not screen, does not know the self-field
//! correction, and uses bore field / hull-edge field as the conductor
//! proxies (conductor field runs between them). The exhaustive search
//! it feeds is unchanged — the hint proposes the enumeration, it does
//! not prune it.
//!
//! Honesty contract: a hint that cannot be computed reports why (no
//! baseline racetrack dims, no resolvable dataset) rather than
//! fabricating a grid; when `Ic` coverage is partial the two floors are
//! reported as a range, not a single number.

use optcoil_model::{
    coupled_search::{CandidateDims, CoupledSearchCase},
    magnetics::{CurrentModel, Racetrack},
};
use optcoil_physics::{
    critical_current::{IcInterpolationMethod, IcInterpolator},
    racetrack::RacetrackEvaluator,
};

use crate::coupled::resolve_datasets;

/// One row of the minimum-turns table: `(tapes, optimistic, mid,
/// pessimistic)` — turns needed to hold `utilization_limit` at each
/// per-tape capacity floor.
pub type MinTurnsRow = (u32, Option<u32>, Option<u32>, Option<u32>);

/// The suggested search space plus the arithmetic behind it — every
/// number is shown to the user, none silently applied.
#[derive(Debug, Clone)]
pub struct SizingHint {
    /// Unit bore field, T per ampere-turn. Under a racetrack envelope
    /// this is the one real field evaluation the estimate runs; under a
    /// declared `field_map` it is the map producer's own declared anchor
    /// (`bore_field_at_reference_t / reference_ampere_turns_a`) — the
    /// `notes` say which.
    pub unit_bore_field_t_per_at: f64,
    /// `b_target / unit_bz` — the ampere-turns the requirement implies.
    pub ni_needed_a: f64,
    /// Per-tape capacities (`Ic × tape width`) at three field proxies —
    /// optimistic `max(b_target, hull floor)`, geometric-mean mid, and
    /// the dataset's highest measured field — because conductor field
    /// lies between bore field and the hull edge. `None` where the hull
    /// does not cover the proxy.
    pub ic_floor_optimistic_a: Option<f64>,
    pub ic_floor_mid_a: Option<f64>,
    pub ic_floor_pessimistic_a: Option<f64>,
    /// The three field proxies used above, T.
    pub field_proxies_t: (f64, f64, f64),
    /// Minimum turns to hold `utilization_limit` per `tapes` value at
    /// each floor: `(tapes, optimistic, mid, pessimistic)`.
    pub min_turns_table: Vec<MinTurnsRow>,
    pub suggested_turns: Vec<u32>,
    pub suggested_tapes: Vec<u32>,
    pub suggested_strands: Vec<u32>,
    /// `len(turns) × len(tapes) × len(strands)` — the enumeration size.
    pub candidate_count: usize,
    /// Honest caveat text shown alongside the suggestion.
    pub notes: Vec<String>,
}

/// Estimate the search space for `case`. Errors only when the case
/// cannot be evaluated at all (no racetrack dims, no resolvable
/// dataset); partial `Ic` coverage degrades to a wider grid and notes.
pub fn sizing_estimate(case: &CoupledSearchCase) -> Result<SizingHint, String> {
    let mut notes = Vec::new();
    let fg = case.fixed_geometry.resolved_for(CandidateDims {
        bend_radius_m: case.baseline.bend_radius_m,
        straight_half_length_m: case.baseline.straight_half_length_m,
    });
    // Unit bore field: racetrack envelopes get one real evaluation;
    // declared maps use the producer's own anchor
    // (`bore_field_at_reference_t / reference_ampere_turns_a`) — the
    // note below says which produced the number. Maps additionally
    // supply their own peak conductor-field proxy from declared nodes.
    let (unit_bz, map_peak_t) = if let Some(fm) = &case.field_map {
        let resolved = fm
            .map
            .resolve()
            .map_err(|e| format!("declared field_map does not resolve: {e}"))?;
        let ref_at = resolved.reference_ampere_turns_a();
        let anchor = fm.bore_field_at_reference_t;
        if !(ref_at.is_finite() && ref_at > 0.0 && anchor.is_finite() && anchor > 0.0) {
            return Err(
                "the field_map anchor (bore_field_at_reference_t · reference_ampere_turns_a) \
                 must be positive — it is the map producer's declared value and there is no \
                 engine evaluation to fall back on"
                    .to_owned(),
            );
        }
        notes.push(
            "unit bore field is the declared map anchor — the map producer's own figure, not an \
             engine evaluation; this hint trusts it and the search is what screens anything"
                .into(),
        );
        (anchor / ref_at, Some(resolved.peak_unit_field_t_per_at()))
    } else {
        let (Some(straight), Some(bend)) = (fg.straight_half_length_m, fg.bend_radius_m) else {
            return Err(
                "sizing estimate needs a racetrack envelope (straight_half_length_m + \
                 bend_radius_m) or a declared field_map — path and path3d cases size from their \
                 own geometry"
                    .to_owned(),
            );
        };
        let baseline = &case.baseline;
        let (radial_width_m, axial_height_m) =
            fg.candidate_extents_m(baseline.turns_along_normal, baseline.tapes_along_width);
        let racetrack = Racetrack {
            straight_half_length_m: straight,
            bend_radius_m: bend,
            radial_width_m,
            axial_height_m,
            ampere_turns_a: 1.0,
            current_model: CurrentModel::UniformWindingPack,
        };
        let order = *case.numerics.quadrature_orders.first().unwrap_or(&10);
        let evaluator = RacetrackEvaluator::new(&racetrack, order)
            .map_err(|e| format!("baseline geometry does not evaluate: {e}"))?;
        let probe = evaluator
            .evaluate(case.requirement.bore_probe_m)
            .map_err(|e| format!("bore probe evaluation failed: {e}"))?;
        let unit_bz = probe.field_t[2];
        if !(unit_bz.is_finite() && unit_bz > 0.0) {
            return Err(
                "the bore probe sees no axial field from the baseline geometry — check the probe \
                 position lies inside the bore"
                    .to_owned(),
            );
        }
        (unit_bz, None)
    };
    let ni_needed = case.requirement.b_target_t / unit_bz;

    // Coarse Ic floors: the base binding's dataset at the operating
    // temperature, worst (tape-normal) angle, at two field proxies —
    // `max(b_target, hull floor)` optimistic, hull top pessimistic —
    // because conductor field lies between bore field and the dataset
    // edge. `ic_a_per_m` is width-normalized: per-tape capacity is
    // `ic_a_per_m × tape_width_m`.
    let datasets =
        resolve_datasets(std::iter::once(&case.material), &[]).map_err(|e| format!("{e}"))?;
    let dataset = &datasets[&case.material.dataset_id];
    let interpolator = IcInterpolator::with_method(
        &dataset.points,
        dataset.metadata.max_cell_spans,
        IcInterpolationMethod::LogFieldLogCurrent,
    )
    .map_err(|e| format!("{e}"))?;
    let hull_min_b = dataset
        .metadata
        .selection
        .nominal_field_t
        .first()
        .copied()
        .unwrap_or(0.0);
    let hull_max_b = dataset
        .metadata
        .selection
        .nominal_field_t
        .last()
        .copied()
        .unwrap_or(0.0);
    let optimistic_b = case.requirement.b_target_t.max(hull_min_b).min(hull_max_b);
    // Pessimistic proxy: under a declared map, the map's own peak node
    // field at the required NI — the customer's solve, far more honest
    // than the dataset hull edge. Without a map the hull top stays the
    // cap. The lookup itself is clamped to the hull; a peak beyond it
    // is a coverage note, not an extrapolated Ic.
    let map_peak_at_ni = map_peak_t.map(|p| p * ni_needed);
    let pessimistic_b = map_peak_at_ni
        .map(|p| p.max(optimistic_b).min(hull_max_b))
        .unwrap_or(hull_max_b);
    // Mid proxy: geometric mean of the two ends — compact coils run
    // conductor field far above the bore field, so the mid point is the
    // more honest anchor for the grid.
    let mid_b = (optimistic_b * pessimistic_b).sqrt().min(hull_max_b);
    // Worst-angle Ic — try the tape-normal angle first, then walk in
    // toward tape-parallel; a hull edge that lacks 0° is not a reason
    // to give up on the floor.
    let ic_at = |b: f64| {
        [0.0_f64, 15.0, 165.0, 180.0, 30.0, 60.0, 90.0]
            .iter()
            .find_map(|&a| {
                interpolator
                    .evaluate([case.operating.temperature_k, b, a])
                    .ok()
                    .flatten()
                    .map(|e| e.ic_a_per_m * fg.tape_width_m)
            })
            .filter(|ic| ic.is_finite() && *ic > 0.0)
    };
    let ic_optimistic = ic_at(optimistic_b);
    let ic_mid = ic_at(mid_b);
    let ic_pessimistic = ic_at(pessimistic_b);
    if let Some(raw) = map_peak_at_ni {
        notes.push(if raw > hull_max_b {
            format!(
                "the map's peak conductor field {raw:.2} T at the required ampere-turns exceeds \
                 the bound dataset's measured hull {hull_max_b:.2} T — at these sizes screening \
                 will mark those points unsupported, not wrong; the pessimistic floor is evaluated \
                 at the hull edge"
            )
        } else {
            format!(
                "pessimistic conductor-field proxy is the declared map's own peak node \
                 ({raw:.2} T at the required ampere-turns) — better than the dataset hull edge"
            )
        });
    }
    if optimistic_b > case.requirement.b_target_t {
        notes.push(format!(
            "the requirement field {:.2} T sits below the dataset's measured floor {hull_min_b:.2} T — \
             the optimistic floor is evaluated at the hull floor, conservative since true \
             low-field Ic is higher",
            case.requirement.b_target_t
        ));
    }
    if ic_optimistic.is_none() && ic_mid.is_none() && ic_pessimistic.is_none() {
        notes.push(format!(
            "the bound dataset covers no usable Ic at {:.0} K — the turns estimate below is \
             geometry-only; bind a dataset covering the operating point for a capacity-aware grid",
            case.operating.temperature_k
        ));
    }

    // NI = turns × tapes × I_op; a candidate holds when
    // I_op ≤ utilization_limit × Ic_floor, so
    // turns_min(t) = NI / (t × util × Ic). Both floors reported.
    let suggested_tapes: Vec<u32> = vec![1, 2, 3, 4];
    let suggested_strands: Vec<u32> = vec![1];
    let util = case.limits.utilization_limit;
    let turns_min = |t: u32, ic: Option<f64>| {
        ic.map(|ic| (ni_needed / (f64::from(t) * util * ic)).ceil().max(1.0) as u32)
    };
    let mut min_turns_table = Vec::new();
    for &t in &suggested_tapes {
        min_turns_table.push((
            t,
            turns_min(t, ic_optimistic),
            turns_min(t, ic_mid),
            turns_min(t, ic_pessimistic),
        ));
    }

    let mut suggested_turns = Vec::new();
    let push = |v: f64, list: &mut Vec<u32>| {
        let t = v.round().max(1.0) as u32;
        if !list.contains(&t) {
            list.push(t);
        }
    };
    if ic_optimistic.is_some() || ic_mid.is_some() || ic_pessimistic.is_some() {
        // Anchor on the mid floor (the honest conductor-field proxy):
        // for each tapes value, bracket its mid-based minimum ×0.6/×1.0/
        // ×1.6; the pessimistic floor at mid-tape caps the ladder so the
        // grid also reaches the conservative-feasible region.
        let anchor_ic = ic_mid.or(ic_pessimistic).or(ic_optimistic);
        for &t in &suggested_tapes {
            if let Some(lo) = turns_min(t, anchor_ic) {
                push(f64::from(lo) * 0.6, &mut suggested_turns);
                push(f64::from(lo), &mut suggested_turns);
                push(f64::from(lo) * 1.6, &mut suggested_turns);
            }
        }
        if let Some(hi) = turns_min(2, ic_pessimistic) {
            push(f64::from(hi) * 1.25, &mut suggested_turns);
        }
        notes.push(format!(
            "conductor field lies between the {:.2} T optimistic and {:.1} T pessimistic proxies; \
             the grid anchors on the {:.2} T mid proxy — the search's screening checks feasibility, \
             this only proposes where to look",
            optimistic_b, hull_max_b, mid_b
        ));
    } else {
        // No capacity floor at all: a broad geometry-only band around
        // NI / 500 A — a pivot, not a capacity claim.
        let pivot = (ni_needed / 500.0).max(4.0);
        for scale in [0.5_f64, 1.0, 2.0, 4.0] {
            push(pivot * scale, &mut suggested_turns);
        }
        notes.push(
            "without Ic coverage this band brackets ampere-turns only — it says nothing about \
             capacity"
                .to_owned(),
        );
    }
    suggested_turns.sort_unstable();

    let candidate_count = suggested_turns.len() * suggested_tapes.len() * suggested_strands.len();
    if candidate_count > 64 {
        notes.push(format!(
            "{candidate_count} candidates — the exhaustive screen runs every combination; expect \
             tens of minutes at this size"
        ));
    }
    notes.push(
        "Sizing hint only: one field evaluation at the baseline, no self-field correction, no \
         screening. The search re-derives everything it reports."
            .to_owned(),
    );

    Ok(SizingHint {
        unit_bore_field_t_per_at: unit_bz,
        ni_needed_a: ni_needed,
        ic_floor_optimistic_a: ic_optimistic,
        ic_floor_mid_a: ic_mid,
        ic_floor_pessimistic_a: ic_pessimistic,
        field_proxies_t: (optimistic_b, mid_b, hull_max_b),
        min_turns_table,
        suggested_turns,
        suggested_tapes,
        suggested_strands,
        candidate_count,
        notes,
    })
}
