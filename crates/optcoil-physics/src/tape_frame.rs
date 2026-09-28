//! Per-tape field decomposition, angle folding, self-field diagnostics, the
//! Gauss-Lobatto width rule, and the low-field lower-bound query sequence
//! that connect a homogenized pack field (`racetrack.rs`) to the measured
//! bridge law (`critical_current.rs`) for OC-004. Every rule here encodes a
//! declared screening assumption (contract A2-A9); none of it is a
//! production current law, and none of it extrapolates past the measured
//! material envelope.

use std::f64::consts::PI;

use optcoil_model::{ModelError, Status, coupled::TapeFrame, material::MaterialPoint};

use crate::{critical_current::IcInterpolator, racetrack::MU0_H_PER_M};

/// Projections of a field vector onto a tape's local (t, n, w) frame.
#[derive(Debug, Clone, Copy)]
pub struct FieldComponents {
    pub b_n: f64,
    pub b_w: f64,
    pub b_t: f64,
}

/// Decompose a lab-frame field vector into a tape's local frame components.
pub fn decompose(field_t: [f64; 3], frame: &TapeFrame) -> FieldComponents {
    FieldComponents {
        b_n: dot(field_t, frame.n),
        b_w: dot(field_t, frame.w),
        b_t: dot(field_t, frame.t),
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Euclidean magnitude of a lab-frame field vector.
pub fn magnitude_t(field_t: [f64; 3]) -> f64 {
    field_t[0].hypot(field_t[1]).hypot(field_t[2])
}

/// Raw in-plane angle from the tape normal: `atan2(B_w, B_n)` in degrees,
/// in `(-180, 180]` (contract A5).
pub fn theta_raw_deg(components: &FieldComponents) -> f64 {
    components.b_w.atan2(components.b_n).to_degrees()
}

/// Period-180 field-reversal fold into `[0, 180)` (contract A5): implemented
/// as `((theta_raw % 180) + 180) % 180`, then a raw angle whose fold lands
/// within `1e-9` deg of the 180 boundary snaps to 0 (the same physical
/// direction, reached from the floating-point boundary rather than exactly).
pub fn theta_fold_deg(theta_raw_deg: f64) -> f64 {
    let folded = ((theta_raw_deg % 180.0) + 180.0) % 180.0;
    if (180.0 - folded).abs() < 1e-9 {
        0.0
    } else {
        folded
    }
}

/// The mirror-ambiguity partner angle (contract A5): `180 - theta_fold`. At
/// `theta_fold = 0` this evaluates to 180, i.e. the pair is `{0, 180}` as the
/// contract states.
pub fn theta_mirror_deg(theta_fold_deg: f64) -> f64 {
    180.0 - theta_fold_deg
}

/// `|B_t| / |B|`, the along-current field fraction (contract A5). Zero field
/// has no defined direction and is reported as 0 by convention; the log-field
/// material law already refuses zero-field queries regardless.
pub fn along_current_fraction(components: &FieldComponents, magnitude_t: f64) -> f64 {
    if magnitude_t == 0.0 {
        0.0
    } else {
        (components.b_t / magnitude_t).abs()
    }
}

/// Fixed points on `[-1, 1]`, both edges included (contract A7).
pub const LOBATTO_POINT_COUNT: usize = 5;

/// Weights halved from the standard 5-point Gauss-Lobatto value on `[-1, 1]`
/// (`{1/10, 49/90, 32/45, 49/90, 1/10}`, which sum to 2) so that these sum to
/// 1 and act directly as a weighted-average rule: for a per-width quantity
/// `K(xi)`, `sum(WEIGHTS[q] * K(xi_q))` is the width-averaged `K`, and
/// `i_strip_a` multiplies that average by the tape width with no further
/// factor of 2 (contract A7).
pub const LOBATTO_WEIGHTS: [f64; LOBATTO_POINT_COUNT] = [
    1.0 / 20.0,
    49.0 / 180.0,
    16.0 / 45.0,
    49.0 / 180.0,
    1.0 / 20.0,
];

/// The fixed 5-point Gauss-Lobatto abscissas on `[-1, 1]`: `{-1, -sqrt(3/7),
/// 0, sqrt(3/7), 1}`. `f64::sqrt` is not a stable const fn under the pinned
/// toolchain, so the interior node is computed at call time rather than
/// hardcoded as a literal; a test cross-checks it against the literature
/// value.
pub fn lobatto_nodes() -> [f64; LOBATTO_POINT_COUNT] {
    let interior = (3.0_f64 / 7.0).sqrt();
    [-1.0, -interior, 0.0, interior, 1.0]
}

/// Self-field ratio `r = mu0 * K / (2 * |B_query|)` (contract A3), reported
/// for every width point as a diagnostic; the declared limit applies only to
/// the limiting point of the limiting tape of an otherwise-passing candidate.
pub fn self_field_ratio(k_used_a_per_m: f64, query_field_t: f64) -> f64 {
    MU0_H_PER_M * k_used_a_per_m / (2.0 * query_field_t)
}

/// The A2(i) residual scale at the limiting point:
/// `(mu0 * K_used / (2 pi)) * ln(tape_width_m / pitch_n_m)`. This is a
/// diagnostic reported alongside the self-field ratio; it never gates
/// PASS/FAIL on its own (contract A3).
pub fn own_tape_edge_field_scale_t(k_used_a_per_m: f64, tape_width_m: f64, pitch_n_m: f64) -> f64 {
    (MU0_H_PER_M * k_used_a_per_m / (2.0 * PI)) * (tape_width_m / pitch_n_m).ln()
}

/// OC-014 Phase 2 (`critical_state_strip`): the conservative bound on a
/// tape's own critical-state edge self-field —
/// `(mu0 * K_c_floor / (2 pi)) * ln(2 * half_width / layer_thickness)`.
///
/// In the Bean/Norris critical state the strip's penetrated edge zone
/// carries the *critical* sheet current K_c regardless of how much
/// transport current flows — the operating current controls only how
/// far the zone penetrates, not its density. The self-field at the
/// conductor edge is therefore set by K_c, not by the carried K_op
/// (which is why the `uniform_transport` μ0·K_op/2 term is not a bound
/// once self-field dominates). `k_c_floor_a_per_m` must be the strip's
/// critical sheet current evaluated at the dataset's low-field floor —
/// since Ic(B) is monotone decreasing, that floor value is the largest
/// sheet current the strip can carry at any field, so the resulting
/// edge field is a strict upper bound valid at every applied-field
/// level including the interior field-null pockets. `layer_thickness_m`
/// is the declared current-carrying (REBCO) film thickness — the
/// physical standoff that regularizes the thin-strip edge-field
/// logarithm.
pub fn critical_state_edge_field_bound_t(
    k_c_floor_a_per_m: f64,
    tape_width_m: f64,
    layer_thickness_m: f64,
) -> f64 {
    (MU0_H_PER_M * k_c_floor_a_per_m / (2.0 * PI)) * (tape_width_m / layer_thickness_m).ln()
}

/// The basis on which one angle query at one width point was determined
/// (contract A8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryBasis {
    /// Queried directly at the actual field magnitude.
    Estimate,
    /// The actual magnitude was unsupported and below the clamp; queried
    /// instead at the clamp value, which monotonicity makes a valid
    /// conservative lower bound, never an extrapolated estimate.
    LowerBound,
    /// Neither the actual magnitude nor (if applicable) the clamp value is
    /// covered by the measured envelope. No upper clamp exists.
    Unsupported,
}

/// One mirror angle's query result at one width point.
#[derive(Debug, Clone)]
pub struct AngleQuery {
    pub basis: QueryBasis,
    /// The field magnitude actually used in the query: the true `|B|` for
    /// `Estimate`, the clamp value for `LowerBound`, and the requested
    /// (unused) `|B|` for `Unsupported`.
    pub query_field_t: f64,
    /// The angle representation actually queried: the requested angle, or
    /// its period-180 equivalent (`angle - 180`) when only that
    /// representation lies inside the measured angular hull (the 0-degree
    /// plane is measured at slightly negative Hall angles, so orientations
    /// within about a degree of 180 degrees are covered there). For
    /// `Unsupported` this is the requested angle.
    pub angle_query_deg: f64,
    pub k_a_per_m: Option<f64>,
    /// The measured E–J exponent from the same estimate as `k_a_per_m`
    /// (`None` alongside it). Carried per-angle so the mirror-pair minimum
    /// selects the governing branch's transition law with its capacity.
    pub n_value: Option<f64>,
}

/// A width point's combined mirror-pair result (contract A5's
/// `minimum_of_mirror_pair` policy composed with A8's low-field clamp).
#[derive(Debug, Clone)]
pub struct ClampedPoint {
    pub folded: AngleQuery,
    pub mirror: AngleQuery,
    /// `Estimate` iff both angles are `Estimate`; `LowerBound` if either
    /// angle used the clamp; `Unsupported` if either angle is unsupported
    /// (the minimum over a bound and an estimate is still a valid
    /// conservative value; contract A8).
    pub basis: QueryBasis,
    pub k_used_a_per_m: Option<f64>,
    /// The query field magnitude associated with whichever angle produced
    /// `k_used_a_per_m` (ties keep the folded angle's value); this is the
    /// `B_query` the self-field ratio (contract A3) is computed against.
    pub query_field_t: f64,
    /// The governing branch's measured E–J exponent — the `n_value` of the
    /// mirror-pair angle that produced `k_used_a_per_m` (`None` alongside
    /// it). Where a low-field clamp bound the capacity the exponent is
    /// measured at the clamp query: honest measured data, never a guess.
    pub n_value: Option<f64>,
}

/// Contract A8's order of operations for one angle, applied to each
/// period-180 representation of that angle in turn: (1) query at the actual
/// magnitude; (2) if unsupported and the magnitude is below the clamp, retry
/// at the clamp value; then repeat both for `angle - 180`, the equivalent
/// orientation under the declared field-reversal symmetry (contract A5);
/// (3) otherwise unsupported. Trying the second representation is not
/// extrapolation: the measured 0-degree plane sits at slightly negative Hall
/// angles, so the hull genuinely covers orientations that fold to just
/// below 180 degrees only in their negative-angle form. Errors propagate
/// rather than being folded into `Unsupported`, since they indicate a
/// malformed query (nonfinite input, coordinates outside the interpolator's
/// structurally valid range), not a coverage hole.
fn query_angle_with_clamp(
    interpolator: &IcInterpolator,
    temperature_k: f64,
    magnitude_t: f64,
    angle_deg: f64,
    low_field_clamp_t: f64,
) -> Result<AngleQuery, ModelError> {
    for representation in [angle_deg, angle_deg - 180.0] {
        if let Some(estimate) =
            interpolator.evaluate([temperature_k, magnitude_t, representation])?
        {
            return Ok(AngleQuery {
                basis: QueryBasis::Estimate,
                query_field_t: magnitude_t,
                angle_query_deg: representation,
                k_a_per_m: Some(estimate.ic_a_per_m),
                n_value: Some(estimate.n_value),
            });
        }
        if magnitude_t < low_field_clamp_t
            && let Some(estimate) =
                interpolator.evaluate([temperature_k, low_field_clamp_t, representation])?
        {
            return Ok(AngleQuery {
                basis: QueryBasis::LowerBound,
                query_field_t: low_field_clamp_t,
                angle_query_deg: representation,
                k_a_per_m: Some(estimate.ic_a_per_m),
                n_value: Some(estimate.n_value),
            });
        }
    }
    Ok(AngleQuery {
        basis: QueryBasis::Unsupported,
        query_field_t: magnitude_t,
        angle_query_deg: angle_deg,
        k_a_per_m: None,
        n_value: None,
    })
}

/// Combine both mirror-pair angle queries into one width point's result
/// (contract A5, A8). This is the low-field lower-bound query sequence: a
/// declared conservative screening substitution, never an extrapolated
/// estimate past the measured envelope.
pub fn query_mirror_pair_with_clamp(
    interpolator: &IcInterpolator,
    temperature_k: f64,
    magnitude_t: f64,
    theta_fold_deg: f64,
    theta_mirror_deg: f64,
    low_field_clamp_t: f64,
) -> Result<ClampedPoint, ModelError> {
    let folded = query_angle_with_clamp(
        interpolator,
        temperature_k,
        magnitude_t,
        theta_fold_deg,
        low_field_clamp_t,
    )?;
    let mirror = query_angle_with_clamp(
        interpolator,
        temperature_k,
        magnitude_t,
        theta_mirror_deg,
        low_field_clamp_t,
    )?;
    let basis = match (folded.basis, mirror.basis) {
        (QueryBasis::Unsupported, _) | (_, QueryBasis::Unsupported) => QueryBasis::Unsupported,
        (QueryBasis::Estimate, QueryBasis::Estimate) => QueryBasis::Estimate,
        _ => QueryBasis::LowerBound,
    };
    let (k_used_a_per_m, query_field_t, n_value) = match (folded.k_a_per_m, mirror.k_a_per_m) {
        (Some(f), Some(m)) if m < f => (Some(m), mirror.query_field_t, mirror.n_value),
        (Some(f), Some(_)) => (Some(f), folded.query_field_t, folded.n_value),
        _ => (None, magnitude_t, None),
    };
    Ok(ClampedPoint {
        folded,
        mirror,
        basis,
        k_used_a_per_m,
        query_field_t,
        n_value,
    })
}

/// Result of auditing `Ic` monotonicity in `|B|` at fixed nominal
/// `(T, angle)` (contract A8).
#[derive(Debug, Clone)]
pub struct MonotonicityAudit {
    pub checked_pairs: u64,
    pub violations: u64,
    pub worst_relative_increase: f64,
    pub tolerance: f64,
    /// `Pass` iff `violations == 0`; `Fail` otherwise. A `Fail` here must
    /// make every lower-bound point unsupported (contract A8): a monotone
    /// lower bound is only valid where monotonicity itself is verified.
    pub status: Status,
}

/// Groups measured points by nominal `(temperature, angle)` and checks that
/// `Ic` is nonincreasing in the actual applied field within `tolerance`
/// (relative). This function takes the measured points directly rather than
/// a whole `MaterialDataset`, since that is all it needs; callers typically
/// pass `&dataset.points`. Below 1 T this dataset does not itself carry
/// coverage (contract A8); the separate assumption-audit tool checks the
/// archived workbook's sub-1 T rows instead.
pub fn audit_monotonicity(points: &[MaterialPoint], tolerance: f64) -> MonotonicityAudit {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(u64, u64), Vec<&MaterialPoint>> = BTreeMap::new();
    for point in points {
        let key = (
            point.nominal_temperature_k.to_bits(),
            point.nominal_angle_deg.to_bits(),
        );
        groups.entry(key).or_default().push(point);
    }
    let mut checked_pairs = 0_u64;
    let mut violations = 0_u64;
    let mut worst_relative_increase = 0.0_f64;
    for group in groups.values() {
        let mut sorted = group.clone();
        sorted.sort_by(|a, b| a.applied_field_t.total_cmp(&b.applied_field_t));
        for pair in sorted.windows(2) {
            checked_pairs += 1;
            let (lower, higher) = (pair[0], pair[1]);
            if higher.ic_a_per_m > lower.ic_a_per_m {
                let relative_increase = (higher.ic_a_per_m - lower.ic_a_per_m) / lower.ic_a_per_m;
                worst_relative_increase = worst_relative_increase.max(relative_increase);
                if relative_increase > tolerance {
                    violations += 1;
                }
            }
        }
    }
    let status = if violations == 0 {
        Status::Pass
    } else {
        Status::Fail
    };
    MonotonicityAudit {
        checked_pairs,
        violations,
        worst_relative_increase,
        tolerance,
        status,
    }
}

/// Conservative per-tape screening current: tape width times the minimum
/// `K` over the `Q` width points (contract A7). Defined only when the
/// caller has confirmed all `Q` points are determined.
pub fn i_min_a(tape_width_m: f64, k_used_a_per_m: [f64; LOBATTO_POINT_COUNT]) -> f64 {
    tape_width_m * k_used_a_per_m.iter().cloned().fold(f64::INFINITY, f64::min)
}

/// Parallel-strip estimate: tape width times the Lobatto-weighted average
/// `K` across the width (contract A7). `LOBATTO_WEIGHTS` already sum to 1,
/// so no extra normalization factor is applied here.
pub fn i_strip_a(tape_width_m: f64, k_used_a_per_m: [f64; LOBATTO_POINT_COUNT]) -> f64 {
    tape_width_m
        * k_used_a_per_m
            .iter()
            .zip(LOBATTO_WEIGHTS)
            .map(|(k, w)| k * w)
            .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::material::{CellSpanLimits, MaterialPoint};

    use crate::critical_current::{IcInterpolationMethod, LOG_IC_MODEL_ID};

    #[test]
    fn coupled_case_material_method_matches_the_physics_interpolation_method_id() {
        // These two constants must never drift apart: the model crate keeps
        // its own copy since it cannot depend on optcoil-physics.
        assert_eq!(
            LOG_IC_MODEL_ID,
            optcoil_model::coupled::EXPECTED_MATERIAL_METHOD_ID
        );
    }

    #[test]
    fn field_decomposition_and_angle_and_along_current_fraction_match_known_geometry() {
        let frame = TapeFrame {
            t: [0.0, 0.0, 1.0],
            n: [1.0, 0.0, 0.0],
            w: [0.0, 1.0, 0.0],
        };
        let field = [3.0, 4.0, 12.0]; // 3-4-5 and 5-12-13 triangles, independently checkable
        let components = decompose(field, &frame);
        assert_eq!(components.b_n, 3.0);
        assert_eq!(components.b_w, 4.0);
        assert_eq!(components.b_t, 12.0);
        let magnitude = magnitude_t(field);
        assert!((magnitude - 13.0).abs() < 1e-12); // sqrt(9+16+144) = 13
        let raw = theta_raw_deg(&components);
        assert!((raw - 53.13010235415598).abs() < 1e-9); // atan2(4,3), literature 3-4-5 angle
        let fraction = along_current_fraction(&components, magnitude);
        assert!((fraction - 12.0 / 13.0).abs() < 1e-12);
    }

    #[test]
    fn folding_matches_the_declared_examples_and_mirror_pairs() {
        for (raw, expected_fold) in [(-35.0, 145.0), (180.0, 0.0), (-180.0, 0.0), (90.0, 90.0)] {
            let fold = theta_fold_deg(raw);
            assert!((fold - expected_fold).abs() < 1e-9, "raw={raw} fold={fold}");
        }
        // Mirror pairs: mirror(fold) = 180 - fold; fold = 0 pairs with 180.
        assert_eq!(theta_mirror_deg(0.0), 180.0);
        assert_eq!(theta_mirror_deg(30.0), 150.0);
        assert_eq!(theta_mirror_deg(90.0), 90.0); // self-mirrored at the fold midpoint
    }

    #[test]
    fn lobatto_rule_sums_to_one_and_integrates_degree_six_but_not_degree_eight() {
        assert!((LOBATTO_WEIGHTS.iter().sum::<f64>() - 1.0).abs() < 1e-15);
        let nodes = lobatto_nodes();
        assert_eq!(nodes[0], -1.0);
        assert_eq!(nodes[2], 0.0);
        assert_eq!(nodes[4], 1.0);
        assert_eq!(nodes[1], -nodes[3]);
        assert!((nodes[3] - (3.0_f64 / 7.0).sqrt()).abs() < 1e-15);
        // True average value of x^n over [-1, 1] is 1/(n+1).
        let weighted_power = |power: i32| -> f64 {
            nodes
                .iter()
                .zip(LOBATTO_WEIGHTS)
                .map(|(x, w)| w * x.powi(power))
                .sum()
        };
        assert!((weighted_power(6) - 1.0 / 7.0).abs() < 1e-13);
        assert!((weighted_power(8) - 1.0 / 9.0).abs() > 1e-4);
    }

    fn synthetic_dataset_points() -> Vec<MaterialPoint> {
        // A minimal 2x2x2 measured-coordinate grid (8 points, one hexahedron)
        // with actual coordinates equal to nominal. Only the (20, 1.001, 0)
        // node's value (200_000.0 A/m) matters to the clamp/mirror
        // assertions below; the rest are arbitrary distinct positive numbers,
        // chosen nonincreasing in field so the "clean" grid also satisfies
        // the monotonicity audit used elsewhere in this module's tests.
        let mut points = Vec::new();
        let mut row = 1_u32;
        for &t in &[20.0_f64, 30.0] {
            for &b in &[1.001_f64, 2.0] {
                for &a in &[0.0_f64, 60.0] {
                    let mut ic = 200_000.0;
                    if t == 30.0 {
                        ic += 10_000.0;
                    }
                    if b == 2.0 {
                        ic -= 20_000.0;
                    }
                    if a == 60.0 {
                        ic += 30_000.0;
                    }
                    points.push(MaterialPoint {
                        source_row: row,
                        nominal_temperature_k: t,
                        nominal_field_t: b,
                        nominal_angle_deg: a,
                        temperature_k: t,
                        applied_field_t: b,
                        angle_from_normal_deg: a,
                        ic_a_per_m: ic,
                        bridge_ic_a: ic * 0.001,
                        // A distinct measured exponent per node, so tests
                        // can tell which query branch supplied it.
                        n_value: 15.0 + (ic - 200_000.0) / 10_000.0,
                    });
                    row += 1;
                }
            }
        }
        points
    }

    fn synthetic_interpolator() -> IcInterpolator {
        let limits = CellSpanLimits {
            temperature_k: 15.0,
            field_ratio: 3.0,
            angle_deg: 90.0,
        };
        IcInterpolator::with_method(
            &synthetic_dataset_points(),
            limits,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap()
    }

    #[test]
    fn clamp_sequence_follows_the_declared_order_of_operations_on_a_synthetic_dataset() {
        let interpolator = synthetic_interpolator();
        let clamp = 1.001;

        // Step 1 succeeds directly: an exact grid node queried at its own
        // field — the node's own measured exponent rides along.
        let estimate = query_angle_with_clamp(&interpolator, 20.0, 1.001, 0.0, clamp).unwrap();
        assert_eq!(estimate.basis, QueryBasis::Estimate);
        assert_eq!(estimate.query_field_t, 1.001);
        assert_eq!(estimate.k_a_per_m, Some(200_000.0));
        assert_eq!(estimate.n_value, Some(15.0));

        // Step 2: below the clamp and outside coverage at the true field, but
        // the clamp value itself is a covered (indeed exact) node — its
        // measured exponent comes along with the clamped current.
        let lower_bound = query_angle_with_clamp(&interpolator, 20.0, 0.5, 0.0, clamp).unwrap();
        assert_eq!(lower_bound.basis, QueryBasis::LowerBound);
        assert_eq!(lower_bound.query_field_t, 1.001);
        assert_eq!(lower_bound.k_a_per_m, Some(200_000.0));
        assert_eq!(lower_bound.n_value, Some(15.0));

        // Step 3: above the clamp, so no fallback is attempted; unsupported —
        // no capacity and no exponent.
        let unsupported = query_angle_with_clamp(&interpolator, 20.0, 5.0, 0.0, clamp).unwrap();
        assert_eq!(unsupported.basis, QueryBasis::Unsupported);
        assert_eq!(unsupported.k_a_per_m, None);
        assert_eq!(unsupported.n_value, None);
    }

    #[test]
    fn mirror_pair_combination_takes_the_minimum_and_the_weakest_basis() {
        let interpolator = synthetic_interpolator();
        // theta_fold = 0 (node value 200_000), theta_mirror = 60 (node value
        // 230_000): both angles are exact grid nodes at the true field, so
        // both are Estimate; the minimum of the pair is the fold value.
        let point =
            query_mirror_pair_with_clamp(&interpolator, 20.0, 1.001, 0.0, 60.0, 1.001).unwrap();
        assert_eq!(point.basis, QueryBasis::Estimate);
        assert_eq!(point.k_used_a_per_m, Some(200_000.0));
        // The 0-degree node governs — its own n = 15, not the mirror's 18.
        assert_eq!(point.n_value, Some(15.0));

        // Swapping the argument order must not change which branch's
        // exponent is reported: the mirror branch (0 degrees, k = 200_000,
        // n = 15) now governs the 60-degree fold (k = 230_000, n = 18).
        let point =
            query_mirror_pair_with_clamp(&interpolator, 20.0, 1.001, 60.0, 0.0, 1.001).unwrap();
        assert_eq!(point.basis, QueryBasis::Estimate);
        assert_eq!(point.k_used_a_per_m, Some(200_000.0));
        assert_eq!(point.n_value, Some(15.0));

        // Same angles, but the field is now below the clamp: both fall back
        // to the clamp value, so the point becomes LowerBound while keeping
        // the same minimum current — and the clamp node's measured exponent.
        let point =
            query_mirror_pair_with_clamp(&interpolator, 20.0, 0.5, 0.0, 60.0, 1.001).unwrap();
        assert_eq!(point.basis, QueryBasis::LowerBound);
        assert_eq!(point.k_used_a_per_m, Some(200_000.0));
        assert_eq!(point.n_value, Some(15.0));

        // If either angle is unsupported, the whole point is unsupported
        // even though the other angle carries a valid estimate.
        let point =
            query_mirror_pair_with_clamp(&interpolator, 20.0, 5.0, 0.0, 60.0, 1.001).unwrap();
        assert_eq!(point.basis, QueryBasis::Unsupported);
        assert_eq!(point.k_used_a_per_m, None);
        assert_eq!(point.n_value, None);
    }

    #[test]
    fn monotonicity_audit_catches_a_planted_field_increase() {
        let points = synthetic_dataset_points();
        let clean = audit_monotonicity(&points, 1e-3);
        assert_eq!(clean.status, Status::Pass);
        assert_eq!(clean.violations, 0);
        assert_eq!(clean.checked_pairs, 4); // 2 T levels x 2 angle levels, 1 field pair each

        let mut corrupted = points;
        // The comparison baseline for this (T, angle) sweep is the lower
        // field's own node, not the point we are about to corrupt.
        let low_field_value = corrupted
            .iter()
            .find(|p| {
                p.nominal_temperature_k == 20.0
                    && p.nominal_angle_deg == 0.0
                    && p.nominal_field_t == 1.001
            })
            .unwrap()
            .ic_a_per_m;
        let target = corrupted
            .iter()
            .position(|p| {
                p.nominal_temperature_k == 20.0
                    && p.nominal_angle_deg == 0.0
                    && p.nominal_field_t == 2.0
            })
            .unwrap();
        corrupted[target].ic_a_per_m = 500_000.0;
        let audited = audit_monotonicity(&corrupted, 1e-3);
        assert_eq!(audited.status, Status::Fail);
        assert_eq!(audited.violations, 1);
        let expected_increase = (500_000.0 - low_field_value) / low_field_value;
        assert!((audited.worst_relative_increase - expected_increase).abs() < 1e-9);
    }

    #[test]
    fn i_min_never_exceeds_i_strip() {
        let k = [100.0, 200.0, 150.0, 300.0, 50.0]; // arbitrary distinct A/m values
        let width = 0.012;
        let min = i_min_a(width, k);
        let strip = i_strip_a(width, k);
        assert!(min <= strip + 1e-12);
        assert_eq!(min, width * 50.0);
        // Independent cross-check: manually halve the contract's literally
        // quoted standard GL5 weights (which sum to 2) rather than using
        // LOBATTO_WEIGHTS, and confirm the same weighted average results.
        let raw_weights = [
            1.0 / 10.0,
            49.0 / 90.0,
            32.0 / 45.0,
            49.0 / 90.0,
            1.0 / 10.0,
        ];
        assert!((raw_weights.iter().sum::<f64>() - 2.0).abs() < 1e-14);
        let expected_strip =
            width * k.iter().zip(raw_weights).map(|(k, w)| k * w).sum::<f64>() / 2.0;
        assert!((strip - expected_strip).abs() < 1e-9);
    }

    #[test]
    fn self_field_ratio_matches_a_hand_calculation() {
        let r = self_field_ratio(300_000.0, 0.2);
        // mu0 K / (2B) computed independently with a literal mu0.
        assert!((r - 0.9424777960769379).abs() < 1e-9);
    }

    #[test]
    fn own_tape_edge_field_scale_matches_a_hand_calculation() {
        let scale = own_tape_edge_field_scale_t(300_000.0, 0.012, 0.0001);
        // mu0 K / (2 pi) = 2e-7 * K exactly; times ln(120).
        assert!((scale - 0.28724950453811324).abs() < 1e-8);
    }

    #[test]
    fn critical_state_edge_field_bound_matches_a_hand_calculation() {
        // OC-014 Phase 2: mu0 K / (2 pi) = 2e-7 * K; times ln(w / d_layer).
        let bound = critical_state_edge_field_bound_t(300_000.0, 0.012, 1.0e-6);
        assert!((bound - 0.06 * 12_000.0_f64.ln()).abs() < 1e-12);
        // The bound grows with a thinner current-carrying layer and a wider
        // tape, and shrinks with a smaller critical sheet current.
        assert!(critical_state_edge_field_bound_t(300_000.0, 0.012, 0.5e-6) > bound);
        assert!(critical_state_edge_field_bound_t(150_000.0, 0.012, 1.0e-6) < bound);
    }
}

#[cfg(test)]
mod angle_representation_tests {
    use super::*;
    use optcoil_model::material::{CellSpanLimits, MaterialPoint};

    /// The measured 0-degree plane sits at -1 degree and the 180-degree plane
    /// at 179 degrees, as in the SuperPower data. A query at 179.5 degrees is
    /// outside the [0, 180) representation's hull but inside the hull at its
    /// period-180 equivalent, -0.5 degrees.
    #[test]
    fn angles_just_below_180_are_served_by_their_negative_representation() {
        let mut points = Vec::new();
        let mut row = 1_u32;
        for &t in &[20.0_f64, 30.0] {
            for &b in &[1.0_f64, 2.0] {
                for &(nominal, actual) in &[(0.0_f64, -1.0_f64), (90.0, 90.0), (180.0, 179.0)] {
                    let ic = 100_000.0 + 1000.0 * t + 5000.0 * b + 10.0 * actual;
                    points.push(MaterialPoint {
                        source_row: row,
                        nominal_temperature_k: t,
                        nominal_field_t: b,
                        nominal_angle_deg: nominal,
                        temperature_k: t,
                        applied_field_t: b,
                        angle_from_normal_deg: actual,
                        ic_a_per_m: ic,
                        bridge_ic_a: ic * 0.001,
                        n_value: 20.0,
                    });
                    row += 1;
                }
            }
        }
        let limits = CellSpanLimits {
            temperature_k: 10.0,
            field_ratio: 3.0,
            angle_deg: 100.0,
        };
        let model =
            IcInterpolator::with_method(&points, limits, optcoil_physics_ic_method()).unwrap();
        let served = query_angle_with_clamp(&model, 25.0, 1.5, 179.5, 0.5).unwrap();
        assert_eq!(served.basis, QueryBasis::Estimate);
        assert!((served.angle_query_deg - (-0.5)).abs() < 1e-12);
        let direct = query_angle_with_clamp(&model, 25.0, 1.5, 178.0, 0.5).unwrap();
        assert_eq!(direct.basis, QueryBasis::Estimate);
        assert!((direct.angle_query_deg - 178.0).abs() < 1e-12);
        // Beyond both representations (below -1 degree) stays unsupported.
        let outside = query_angle_with_clamp(&model, 25.0, 1.5, 178.5 + 180.0, 0.5).unwrap();
        assert_eq!(
            outside.basis,
            QueryBasis::Estimate,
            "358.5 folds to 178.5 via the second representation"
        );
        let unsupported = query_angle_with_clamp(&model, 25.0, 1.5, -1.5, 0.5).unwrap();
        assert_eq!(unsupported.basis, QueryBasis::Unsupported);
        assert!((unsupported.angle_query_deg - (-1.5)).abs() < 1e-12);
    }

    fn optcoil_physics_ic_method() -> crate::critical_current::IcInterpolationMethod {
        crate::critical_current::IcInterpolationMethod::Linear
    }
}
