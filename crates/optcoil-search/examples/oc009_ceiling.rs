//! OC-009 pre-study (analysis only): for each racetrack outline
//! `(straight_half_length, bend_radius)` on a grid, evaluate the unit
//! `B_z` on a declared good-field-region lattice, solve the `NI` that puts
//! the region minimum at the 0.9 T target, evaluate the self-field at the
//! pack's inner winding faces, and bound `I_op` two ways:
//!
//! * `bound_opt`: `0.8 x (1-0.1) x w x ic_max_angle(B_peak)` — the most
//!   generous screening the dataset permits. A cell whose *optimistic*
//!   bound already exceeds 50% of baseline is unreachable by any real
//!   search; this prunes dead regions without proving the live ones.
//! * `bound_real`: the same form against the *min-over-angles* envelope.
//!   On the incumbent geometry this reproduces the measured screening
//!   margin within ~10% (the tape's weakest width point sees a
//!   near-transverse field at the arc apexes), so it is a calibrated
//!   estimate of where the real search could land — not a bound.
//!
//! This is a limit study, not a design output: no screening, no material
//! interpolation, no cost recompute, no PASS/FAIL verdict. The pack used
//! for the field evaluations is fixed at OC-007's incumbent optimum
//! (120 turns x 4 tapes); the conductor-length bound then chooses `n x p`
//! freely per outline. Pack-size dependence of the unit field is a
//! second-order correction this study does not model.
//!
//! Usage: `cargo run --release -p optcoil-search --example oc009_ceiling`.

use optcoil_model::{
    coupled_search::GoodFieldRegion,
    magnetics::{CurrentModel, Racetrack},
};
use optcoil_physics::racetrack::RacetrackEvaluator;

/// OC-007's fixed requirement: 0.9 T at the bore probe (0,0,0).
const B_TARGET_T: f64 = 0.9;
/// OC-007's declared utilization limit.
const UTILIZATION_LIMIT: f64 = 0.8;
/// OC-007's interpolation-overprediction budget; `allowed` is derated by
/// `(1 - budget)` before the utilization limit applies.
const OVERPREDICTION_BUDGET: f64 = 0.1;
/// OC-007 cost block (synthetic placeholder prices, unchanged).
const PRICE_USD_PER_M: f64 = 30.0;
const SCRAP_FRACTION: f64 = 0.1;
const ASSEMBLY_USD_PER_PANCAKE: f64 = 500.0;
const JOINT_USD: f64 = 200.0;
/// OC-007's recorded baseline cost (200 turns x 3 tapes).
const BASELINE_TOTAL_USD: f64 = 50_541.41;
/// OC-007's incumbent optimum (120 x 4, $41,513) supplies the pack for the
/// field evaluations.
const OPTIMUM_TOTAL_USD: f64 = 41_513.13;
const PACK_TURNS: u32 = 120;
const PACK_TAPES: u32 = 4;
/// OC-007 winding pack parameters.
const RADIAL_PITCH_M: f64 = 1e-4;
const TAPE_WIDTH_M: f64 = 0.012;
/// Placeholder usable-volume spec for the study: a 10 cm cube centered on
/// the bore probe, sampled on a 3x3x3 lattice. OC-009's real region is not
/// yet declared; this is a stand-in to size the effect, not a spec.
const REGION_HALF_M: f64 = 0.05;
const QUADRATURE_ORDER: u32 = 8;
/// Radial clearance between the region's in-plane corner and the pack's
/// inner face (a bore tube / winding clearance stand-in).
const BORE_CLEARANCE_M: f64 = 0.005;

/// `ic_a_per_m` envelopes near 21 K from
/// `data/materials/robinson-superpower-ap-v3/measurements.csv`: the largest
/// and smallest measured values per nominal field bin (over angles).
/// Piecewise constant; beyond the dataset's 8 T range the bound is reported
/// but flagged, since extrapolating would be dishonest.
const IC_ENVELOPE_MAX_A_PER_M: [(f64, f64); 7] = [
    (1.0, 396_705.0),
    (1.5, 382_444.0),
    (2.0, 372_560.0),
    (3.0, 358_108.0),
    (5.0, 340_808.0),
    (7.0, 328_832.0),
    (8.0, 318_510.0),
];
const IC_ENVELOPE_MIN_A_PER_M: [(f64, f64); 7] = [
    (1.0, 286_783.0),
    (1.5, 242_856.0),
    (2.0, 210_323.0),
    (3.0, 164_746.0),
    (5.0, 113_005.0),
    (7.0, 84_389.0),
    (8.0, 74_529.0),
];

/// Piecewise-constant envelope lookup: the envelope value at the highest
/// level `<= b` (the measured value just above `b`), conservative between
/// bins. Returns `(ic_a_per_m, extrapolated)`.
fn ic_envelope_a_per_m(b: f64, table: &[(f64, f64)]) -> (f64, bool) {
    let mut best = table[0].1;
    for &(level, ic) in table {
        if level <= b {
            best = ic;
        }
    }
    (best, b > 8.0)
}

/// Total installed conductor length for `n` radial turns: sum over turns of
/// `4L + 2 pi rho_k`, times `p` tapes -- the same arithmetic as
/// `compute_cost_ledger` in `optcoil-search`.
fn installed_length_m(l: f64, r: f64, n: u32, p: u32) -> f64 {
    let inner_rho = r - f64::from(n) * RADIAL_PITCH_M / 2.0;
    (1..=n)
        .map(|k| {
            4.0 * l
                + 2.0 * std::f64::consts::PI * (inner_rho + (f64::from(k) - 0.5) * RADIAL_PITCH_M)
        })
        .sum::<f64>()
        * f64::from(p)
}

/// The largest radial turn count whose pack still leaves the usable volume
/// open: the region's in-plane corner radius plus clearance must fit inside
/// the inner winding face `r - n*pitch/2`. Cells where even `n = 1` fails
/// cannot host the declared region at all.
fn max_turns_for_region(r: f64) -> u32 {
    let corner_radius_m = REGION_HALF_M * f64::sqrt(2.0) + BORE_CLEARANCE_M;
    ((r - corner_radius_m) / (RADIAL_PITCH_M / 2.0))
        .floor()
        .max(0.0) as u32
}

/// Cost lower bound for outline (l, r): the smallest `n x p` satisfying
/// `n*p >= NI / I_op_bound` and the aperture constraint, priced with the
/// OC-007 ledger. `None` when no pack satisfies both.
fn cost_lower_bound(l: f64, r: f64, ni_a: f64, i_op_bound: f64) -> Option<(f64, u32, u32)> {
    let need_turns = (ni_a / i_op_bound).ceil() as u64;
    let n_max = u64::from(max_turns_for_region(r));
    let mut best = (f64::INFINITY, 0_u32, 0_u32);
    for p in 1u32..=8 {
        let n = need_turns.div_ceil(u64::from(p));
        if n > n_max {
            continue;
        }
        let n = n.max(1) as u32;
        let installed = installed_length_m(l, r, n, p);
        let total = installed * PRICE_USD_PER_M * (1.0 + SCRAP_FRACTION)
            + ASSEMBLY_USD_PER_PANCAKE * f64::from(p)
            + JOINT_USD * f64::from(p - 1);
        if total < best.0 {
            best = (total, n, p);
        }
    }
    (best.0.is_finite()).then_some(best)
}

struct UnitFields {
    /// Lattice minimum of unit `B_z` over the region (the NI solve basis).
    region_min_bz: f64,
    /// Max unit |B| at the pack's inner winding faces (the optimistic
    /// self-field proxy).
    peak_surface_b: f64,
}

/// Unit fields for outline (l, r) with the fixed incumbent pack: the
/// region lattice minimum plus the strongest field on the winding's inner
/// faces (straight midpoints and bend apexes), all per A-turn.
fn unit_fields(l: f64, r: f64, region: &GoodFieldRegion) -> UnitFields {
    let width = f64::from(PACK_TURNS) * RADIAL_PITCH_M;
    let height = f64::from(PACK_TAPES) * TAPE_WIDTH_M;
    let racetrack = Racetrack {
        straight_half_length_m: l,
        bend_radius_m: r,
        radial_width_m: width,
        axial_height_m: height,
        ampere_turns_a: 1.0,
        current_model: CurrentModel::UniformWindingPack,
    };
    let evaluator =
        RacetrackEvaluator::new(&racetrack, QUADRATURE_ORDER).expect("evaluator construction");
    let region_min = region
        .lattice_points([0.0, 0.0, 0.0])
        .iter()
        .map(|&point| {
            evaluator
                .evaluate(point)
                .expect("region point evaluation")
                .field_t[2]
        })
        .fold(f64::INFINITY, f64::min);
    // Inner winding faces: midpoints of the two straights' inner surfaces
    // and the two bend apexes' inner surfaces, at pack axial center.
    let inner = r - width / 2.0;
    let surface_probes = [
        [0.0, inner, 0.0],
        [0.0, -inner, 0.0],
        [l + inner, 0.0, 0.0],
        [-(l + inner), 0.0, 0.0],
    ];
    let peak_surface = surface_probes
        .iter()
        .map(|&point| {
            let field = evaluator
                .evaluate(point)
                .expect("surface point evaluation")
                .field_t;
            (field[0] * field[0] + field[1] * field[1] + field[2] * field[2]).sqrt()
        })
        .fold(0.0_f64, f64::max);
    UnitFields {
        region_min_bz: region_min,
        peak_surface_b: peak_surface,
    }
}

fn main() {
    let region = GoodFieldRegion {
        half_extents_m: [REGION_HALF_M; 3],
        points_per_axis: 3,
        max_relative_deviation: None,
        harmonics: None,
    };
    let half_baseline = BASELINE_TOTAL_USD * 0.5;
    println!(
        "OC-009 ceiling study: {B_TARGET_T} T over a {REGION_HALF_M} m half-extent box (3x3x3 lattice)"
    );
    println!(
        "bound: I_op <= 0.8 x 0.9 x {TAPE_WIDTH_M} m x ic_env(B_peak); ic_env = max-over-angles 21 K envelope"
    );
    println!(
        "baseline ${BASELINE_TOTAL_USD:.0}; 50% threshold ${half_baseline:.0}; incumbent optimum ${OPTIMUM_TOTAL_USD:.0} (17.9%)"
    );
    println!();
    println!(
        "{:>7} {:>7} {:>11} {:>10} {:>9} {:>11} {:>11}",
        "L_m", "R_m", "unit_min", "NI_A", "B_peak_T", "bound_opt$", "bound_real$"
    );
    let mut opt_dead = 0;
    let mut real_under = Vec::new();
    for li in 0..=10 {
        let l = 0.05 + 0.025 * f64::from(li);
        for ri in 0..=10 {
            let r = 0.05 + 0.015 * f64::from(ri);
            let fields = unit_fields(l, r, &region);
            if !(fields.region_min_bz > 0.0 && fields.region_min_bz.is_finite()) {
                continue;
            }
            let ni = B_TARGET_T / fields.region_min_bz;
            let b_peak = fields.peak_surface_b * ni;
            let bound_factor = UTILIZATION_LIMIT * (1.0 - OVERPREDICTION_BUDGET) * TAPE_WIDTH_M;
            let i_opt = bound_factor * ic_envelope_a_per_m(b_peak, &IC_ENVELOPE_MAX_A_PER_M).0;
            let i_real = bound_factor * ic_envelope_a_per_m(b_peak, &IC_ENVELOPE_MIN_A_PER_M).0;
            let opt = cost_lower_bound(l, r, ni, i_opt);
            let real = cost_lower_bound(l, r, ni, i_real);
            let extrapolated = b_peak > 8.0;
            match (opt, real) {
                (Some((co, _, _)), Some((cr, _, _))) => {
                    let flag = if extrapolated { "*" } else { "" };
                    println!(
                        "{l:7.3} {r:7.3} {unit:11.4e} {ni:10.0} {b_peak:9.2} {co:11.0} {cr:11.0}{flag}",
                        unit = fields.region_min_bz,
                    );
                    if co > half_baseline {
                        opt_dead += 1;
                    }
                    if cr <= half_baseline {
                        real_under.push((l, r, cr));
                    }
                }
                _ => {
                    println!(
                        "{l:7.3} {r:7.3} {unit:11.4e} {ni:10.0} {b_peak:9.2}      infeasible: aperture too small for NI",
                        unit = fields.region_min_bz,
                    );
                }
            }
        }
    }
    println!();
    println!(
        "(* = B_peak beyond the dataset's 8 T coverage; the bound is an extrapolation, not evidence)"
    );
    println!(
        "{opt_dead} feasible cell(s) unreachable even optimistically (bound > 50% of baseline)."
    );
    if real_under.is_empty() {
        println!("No cell's calibrated (min-angle) estimate reaches 50% of baseline.");
    } else {
        println!("Cells whose calibrated (min-angle) estimate reaches 50% of baseline:");
        for (l, r, c) in &real_under {
            println!(
                "  L={l:.3} R={r:.3} -> estimate ${c:.0} ({:.1}% of baseline)",
                c / BASELINE_TOTAL_USD * 100.0
            );
        }
    }
}
