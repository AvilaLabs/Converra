//! Continuous linear/logarithmic Ic interpolation on measured coordinates.
//!
//! Nominal grid coordinates define connectivity only. Each complete nominal
//! hexahedron is split into six consistently oriented tetrahedra. Interpolation
//! uses actual temperature, applied field and oriented Hall angle at its nodes.
//! No nearest-neighbor fallback, angle folding, width scaling or extrapolation.
//! The logarithmic variant transforms B and Ic; T and oriented angle stay linear.

use std::collections::{BTreeMap, HashSet};

use optcoil_model::{
    ModelError,
    material::{CellSpanLimits, MaterialPoint, validate_coordinates},
};
use serde::{Deserialize, Serialize};

pub const IC_MODEL_ID: &str = "measured-coordinate-tetrahedral-linear-ic/v1";
pub const LOG_IC_MODEL_ID: &str = "measured-coordinate-tetrahedral-log-field-log-ic/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcInterpolationMethod {
    Linear,
    LogFieldLogCurrent,
}

impl IcInterpolationMethod {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            IC_MODEL_ID => Some(Self::Linear),
            LOG_IC_MODEL_ID => Some(Self::LogFieldLogCurrent),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Linear => IC_MODEL_ID,
            Self::LogFieldLogCurrent => LOG_IC_MODEL_ID,
        }
    }
    fn coordinates(self, mut position: [f64; 3]) -> [f64; 3] {
        if self == Self::LogFieldLogCurrent {
            position[1] = position[1].ln();
        }
        position
    }
}
const MAX_CANDIDATE_CELLS: usize = 100_000;
const CONTAINMENT_ROUNDOFF: f64 = 1e-10;
const PERMUTATIONS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IcEstimate {
    pub ic_a_per_m: f64,
    /// The measured E–J transition exponent at the query point, carried
    /// through the same tetrahedron weights as `ic_a_per_m` — a linear
    /// channel in both interpolation methods (`n` is a local exponent;
    /// only B and Ic transform under the logarithmic variant).
    pub n_value: f64,
    pub exact_measurement: bool,
    pub support: Vec<MeasurementWeight>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementWeight {
    pub source_row: u32,
    pub weight: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IcMeshSummary {
    pub training_points: usize,
    pub nominal_axis_sizes: [usize; 3],
    pub candidate_cells: usize,
    pub supported_cells: usize,
    pub missing_corner_cells: usize,
    pub span_rejected_cells: usize,
    pub tetrahedra: usize,
    /// Cells stitched across the periodic angular fold: the last nominal
    /// angle level's measured points joined to the first level's, the
    /// first level's measured angle shifted by the declared period.
    /// Only built when the interpolator is constructed with a seam
    /// period (a declared case-side assumption), and only ever between
    /// measured points — the seam hull is the convex span of both sides'
    /// measurements, not extrapolation.
    #[serde(default)]
    pub seam_cells: usize,
    #[serde(default)]
    pub seam_missing_corner_cells: usize,
    #[serde(default)]
    pub seam_span_rejected_cells: usize,
}

pub struct IcInterpolator {
    method: IcInterpolationMethod,
    seam_period_deg: Option<f64>,
    points: Vec<MaterialPoint>,
    tetrahedra: Vec<Tetrahedron>,
    summary: IcMeshSummary,
    nominal_axes: [Vec<f64>; 3],
}

struct Tetrahedron {
    nodes: [usize; 4],
    origin: [f64; 3],
    scale: [f64; 3],
    inverse: [[f64; 3]; 3],
    low: [f64; 3],
    high: [f64; 3],
}

impl IcInterpolator {
    pub fn new(points: &[MaterialPoint], limits: CellSpanLimits) -> Result<Self, ModelError> {
        Self::with_method(points, limits, IcInterpolationMethod::Linear)
    }

    pub fn with_method(
        points: &[MaterialPoint],
        limits: CellSpanLimits,
        method: IcInterpolationMethod,
    ) -> Result<Self, ModelError> {
        Self::with_method_and_seam(points, limits, method, None)
    }

    /// `with_method`, plus the declared periodic-angle seam treatment:
    /// when `seam_period_deg` is `Some(period)` (the case's
    /// `angle_mapping` declares it), each complete nominal (T, B) cell
    /// column gains seam cells joining its last angle level to its
    /// first, with the first level's measured angle shifted by `period`.
    /// A seam tetrahedron interpolates between measured points on both
    /// sides of the fold; its angular extent is the measured gap and is
    /// span-gated by `limits.angle_deg` like any other cell. Datasets
    /// whose extreme measured layers *overlap* across the fold (negative
    /// gap) or leave a hole at either level produce no seam cell — the
    /// summary records why, and queries at the fold still fail closed.
    pub fn with_method_and_seam(
        points: &[MaterialPoint],
        limits: CellSpanLimits,
        method: IcInterpolationMethod,
        seam_period_deg: Option<f64>,
    ) -> Result<Self, ModelError> {
        limits.validate()?;
        if points.len() < 8 || points.len() > 100_000 {
            return Err(invalid(
                "3D interpolation requires 8..=100000 training measurements",
            ));
        }
        let mut source_rows = HashSet::new();
        let mut actual_nodes = HashSet::new();
        for point in points {
            point.validate()?;
            if !source_rows.insert(point.source_row)
                || !actual_nodes.insert(
                    point
                        .position()
                        .map(|x| if x == 0.0 { 0 } else { x.to_bits() }),
                )
            {
                return Err(invalid("duplicate source identity or measured coordinate"));
            }
        }
        let axes: [Vec<f64>; 3] = std::array::from_fn(|k| {
            let mut values: Vec<_> = points.iter().map(|p| p.nominal_position()[k]).collect();
            values.sort_by(f64::total_cmp);
            values.dedup();
            values
        });
        if axes.iter().any(|a| a.len() < 2) {
            return Err(invalid(
                "each interpolation axis needs at least two nominal levels",
            ));
        }
        let sizes = axes.each_ref().map(|a| a.len());
        let candidates = sizes
            .iter()
            .try_fold(1_usize, |n, &size| n.checked_mul(size - 1))
            .ok_or_else(|| invalid("nominal interpolation grid exceeds work limit"))?;
        if candidates > MAX_CANDIDATE_CELLS {
            return Err(invalid("nominal interpolation grid exceeds 100000 cells"));
        }
        let mut lookup = BTreeMap::new();
        for (index, point) in points.iter().enumerate() {
            let key: [usize; 3] = std::array::from_fn(|k| {
                axes[k]
                    .binary_search_by(|v| v.total_cmp(&point.nominal_position()[k]))
                    .expect("axis constructed from these nodes")
            });
            if lookup.insert(key, index).is_some() {
                return Err(invalid(
                    "duplicate nominal node; no implicit averaging is allowed",
                ));
            }
        }
        let mut summary = IcMeshSummary {
            training_points: points.len(),
            nominal_axis_sizes: sizes,
            candidate_cells: candidates,
            ..Default::default()
        };
        let mut tetrahedra = Vec::new();
        for t in 0..sizes[0] - 1 {
            for b in 0..sizes[1] - 1 {
                for angle in 0..sizes[2] - 1 {
                    let base = [t, b, angle];
                    let scale: [f64; 3] =
                        std::array::from_fn(|k| axes[k][base[k] + 1] - axes[k][base[k]]);
                    if scale[0] > limits.temperature_k
                        || axes[1][b + 1] / axes[1][b] > limits.field_ratio
                        || scale[2] > limits.angle_deg
                    {
                        summary.span_rejected_cells += 1;
                        continue;
                    }
                    let mut corners = [0_usize; 8];
                    let mut complete = true;
                    for (bits, corner) in corners.iter_mut().enumerate() {
                        let key = std::array::from_fn(|k| base[k] + ((bits >> k) & 1));
                        if let Some(&index) = lookup.get(&key) {
                            *corner = index;
                        } else {
                            complete = false;
                        }
                    }
                    if !complete {
                        summary.missing_corner_cells += 1;
                        continue;
                    }
                    for permutation in PERMUTATIONS {
                        let first = 1 << permutation[0];
                        let second = first | (1 << permutation[1]);
                        let nodes = [corners[0], corners[first], corners[second], corners[7]];
                        let actual =
                            nodes.map(|index| method.coordinates(points[index].position()));
                        let nominal =
                            nodes.map(|index| method.coordinates(points[index].nominal_position()));
                        let mut coordinate_scale = scale;
                        if method == IcInterpolationMethod::LogFieldLogCurrent {
                            coordinate_scale[1] = (axes[1][b + 1] / axes[1][b]).ln();
                        }
                        tetrahedra.push(Tetrahedron::new(
                            nodes,
                            actual,
                            nominal,
                            coordinate_scale,
                        )?);
                    }
                    summary.supported_cells += 1;
                }
            }
        }
        if let Some(period) = seam_period_deg {
            // The periodic fold's stitch cells: nominal cell [t,b] x
            // [last angle level -> first angle level + period]. The
            // nominal angular span is degenerate *by construction* (the
            // grid is closed under the period), so these tetrahedra are
            // built and gated on measured coordinates only: the angular
            // extent is the measured gap between the two layers, the
            // same `limits.angle_deg` gate applies, and an overlapping
            // (nonpositive) or holed layer produces no cell.
            let a_last = sizes[2] - 1;
            for t in 0..sizes[0] - 1 {
                for b in 0..sizes[1] - 1 {
                    let mut corners = [0_usize; 8];
                    let mut complete = true;
                    for (bits, corner) in corners.iter_mut().enumerate() {
                        let angle_level = if (bits >> 2) & 1 == 0 { a_last } else { 0 };
                        let key = [t + (bits & 1), b + ((bits >> 1) & 1), angle_level];
                        if let Some(&index) = lookup.get(&key) {
                            *corner = index;
                        } else {
                            complete = false;
                        }
                    }
                    if !complete {
                        summary.seam_missing_corner_cells += 1;
                        continue;
                    }
                    // The cell's measured angular extent across the fold:
                    // bit2 == 0 selects the last angle level (unshifted),
                    // bit2 == 1 the first level (shifted +period).
                    let measured_angles = |first_level: bool| -> Vec<f64> {
                        (0..8)
                            .filter(|&bits| ((bits >> 2) & 1 == 1) == first_level)
                            .map(|bits| {
                                points[corners[bits]].position()[2]
                                    + if first_level { period } else { 0.0 }
                            })
                            .collect()
                    };
                    let first_angles = measured_angles(true);
                    let last_angles = measured_angles(false);
                    let first_min = first_angles.iter().cloned().fold(f64::INFINITY, f64::min);
                    let last_max = last_angles
                        .iter()
                        .cloned()
                        .fold(f64::NEG_INFINITY, f64::max);
                    let gap = first_min - last_max;
                    let (min, max) = first_angles
                        .iter()
                        .chain(last_angles.iter())
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| {
                            (lo.min(v), hi.max(v))
                        });
                    let extent = max - min;
                    if gap <= 0.0 || extent > limits.angle_deg {
                        summary.seam_span_rejected_cells += 1;
                        continue;
                    }
                    let scale: [f64; 3] = [
                        axes[0][t + 1] - axes[0][t],
                        axes[1][b + 1] - axes[1][b],
                        extent.max(f64::EPSILON),
                    ];
                    let mut coordinate_scale = scale;
                    if method == IcInterpolationMethod::LogFieldLogCurrent {
                        coordinate_scale[1] = (axes[1][b + 1] / axes[1][b]).ln();
                    }
                    for permutation in PERMUTATIONS {
                        let first = 1 << permutation[0];
                        let second = first | (1 << permutation[1]);
                        let corner_bits = [0_usize, first, second, 7];
                        let nodes = corner_bits.map(|bits| corners[bits]);
                        // First-layer (bit2 = 1) corners are shifted one
                        // period in the angle coordinate — actual and
                        // nominal alike — so the seam tetrahedron lives
                        // at the fold with measured endpoints on both
                        // sides. The nominal angular span collapses to
                        // zero (the grid is closed under the period),
                        // which is why `new_seam` checks the measured
                        // determinant alone.
                        let shift = |bits: usize, mut p: [f64; 3]| -> [f64; 3] {
                            if (bits >> 2) & 1 == 1 {
                                p[2] += period;
                            }
                            p
                        };
                        let actual = std::array::from_fn(|i| {
                            shift(
                                corner_bits[i],
                                method.coordinates(points[nodes[i]].position()),
                            )
                        });
                        let nominal = std::array::from_fn(|i| {
                            shift(
                                corner_bits[i],
                                method.coordinates(points[nodes[i]].nominal_position()),
                            )
                        });
                        tetrahedra.push(Tetrahedron::new_seam(
                            nodes,
                            actual,
                            nominal,
                            coordinate_scale,
                        )?);
                    }
                    summary.seam_cells += 1;
                }
            }
        }
        summary.tetrahedra = tetrahedra.len();
        if tetrahedra.is_empty() {
            return Err(invalid(
                "no complete interpolation cells within the declared span limits",
            ));
        }
        Ok(Self {
            method,
            seam_period_deg,
            points: points.to_vec(),
            tetrahedra,
            summary,
            nominal_axes: axes,
        })
    }

    pub fn summary(&self) -> &IcMeshSummary {
        &self.summary
    }

    /// The sorted nominal measurement levels on each axis —
    /// `[temperature_k, applied_field_t, angle_from_normal_deg]`. Callers
    /// that need coverage-true sweeps (e.g. "the largest resolvable Ic at
    /// this field over every measured angle") should iterate these levels
    /// rather than a guessed ladder.
    pub fn nominal_axes(&self) -> &[Vec<f64>; 3] {
        &self.nominal_axes
    }

    /// None means outside the union of supported measured cells (including
    /// holes), not a zero or physically infeasible critical current.
    pub fn evaluate(&self, position: [f64; 3]) -> Result<Option<IcEstimate>, ModelError> {
        validate_coordinates(position, true)?;
        if let Some(point) = self.points.iter().find(|p| p.position() == position) {
            if !point.n_value.is_finite() || point.n_value <= 1.0 {
                return Err(invalid("nonfinite or out-of-range measured n-value"));
            }
            return Ok(Some(IcEstimate {
                ic_a_per_m: point.ic_a_per_m,
                n_value: point.n_value,
                exact_measurement: true,
                support: vec![MeasurementWeight {
                    source_row: point.source_row,
                    weight: 1.0,
                }],
            }));
        }
        if self.method == IcInterpolationMethod::LogFieldLogCurrent && position[1] == 0.0 {
            return Ok(None);
        }
        let position = self.method.coordinates(position);
        // Declared seam stitching: the fold-shifted representations of
        // the query angle can land inside seam cells. Unshifted first —
        // interior queries take the same path they always did.
        let shifts: &[f64] = match self.seam_period_deg {
            Some(period) => &[0.0, -period, period],
            None => &[0.0],
        };
        for &shift in shifts {
            let mut position = position;
            position[2] += shift;
            for cell in &self.tetrahedra {
                let Some(weights) = cell.weights(position) else {
                    continue;
                };
                let mut current = 0.0;
                let mut n_value = 0.0;
                let mut support = Vec::new();
                for (&index, weight) in cell.nodes.iter().zip(weights) {
                    let node = &self.points[index];
                    current += weight
                        * if self.method == IcInterpolationMethod::LogFieldLogCurrent {
                            node.ic_a_per_m.ln()
                        } else {
                            node.ic_a_per_m
                        };
                    n_value += weight * node.n_value;
                    if weight > 0.0 {
                        support.push(MeasurementWeight {
                            source_row: node.source_row,
                            weight,
                        });
                    }
                }
                if self.method == IcInterpolationMethod::LogFieldLogCurrent {
                    current = current.exp();
                }
                if !current.is_finite() || current <= 0.0 {
                    return Err(invalid("nonfinite or nonpositive interpolated current"));
                }
                if !n_value.is_finite() || n_value <= 1.0 {
                    return Err(invalid("nonfinite or out-of-range interpolated n-value"));
                }
                return Ok(Some(IcEstimate {
                    ic_a_per_m: current,
                    n_value,
                    exact_measurement: false,
                    support,
                }));
            }
        }
        Ok(None)
    }
}

impl Tetrahedron {
    fn new(
        nodes: [usize; 4],
        actual: [[f64; 3]; 4],
        nominal: [[f64; 3]; 4],
        scale: [f64; 3],
    ) -> Result<Self, ModelError> {
        Self::construct(nodes, actual, nominal, scale, true)
    }

    /// A fold-seam tetrahedron: its nominal angular span is degenerate
    /// *by construction* (the grid is closed under the declared period),
    /// so the measured-determinant check stands alone — there is no
    /// meaningful nominal topology to compare orientation against.
    /// Non-degenerate measured volume is still required.
    fn new_seam(
        nodes: [usize; 4],
        actual: [[f64; 3]; 4],
        nominal: [[f64; 3]; 4],
        scale: [f64; 3],
    ) -> Result<Self, ModelError> {
        Self::construct(nodes, actual, nominal, scale, false)
    }

    fn construct(
        nodes: [usize; 4],
        actual: [[f64; 3]; 4],
        nominal: [[f64; 3]; 4],
        scale: [f64; 3],
        check_nominal_orientation: bool,
    ) -> Result<Self, ModelError> {
        let actual_edges = edges(actual, scale);
        let nominal_edges = edges(nominal, scale);
        let determinant = dot(actual_edges[0], cross(actual_edges[1], actual_edges[2]));
        let nominal_determinant = dot(nominal_edges[0], cross(nominal_edges[1], nominal_edges[2]));
        if !determinant.is_finite()
            || determinant.abs() < 1e-10
            || (check_nominal_orientation && determinant * nominal_determinant <= 0.0)
        {
            return Err(invalid(
                "measured coordinates create a degenerate or inverted tetrahedron",
            ));
        }
        let inverse = [
            cross(actual_edges[1], actual_edges[2]),
            cross(actual_edges[2], actual_edges[0]),
            cross(actual_edges[0], actual_edges[1]),
        ]
        .map(|v| v.map(|x| x / determinant));
        Ok(Self {
            nodes,
            origin: actual[0],
            scale,
            inverse,
            low: std::array::from_fn(|k| actual.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min)),
            high: std::array::from_fn(|k| {
                actual
                    .iter()
                    .map(|p| p[k])
                    .fold(f64::NEG_INFINITY, f64::max)
            }),
        })
    }

    fn weights(&self, position: [f64; 3]) -> Option<[f64; 4]> {
        if (0..3).any(|k| {
            position[k] < self.low[k] - self.scale[k] * CONTAINMENT_ROUNDOFF
                || position[k] > self.high[k] + self.scale[k] * CONTAINMENT_ROUNDOFF
        }) {
            return None;
        }
        let relative: [f64; 3] =
            std::array::from_fn(|k| (position[k] - self.origin[k]) / self.scale[k]);
        let tail = self.inverse.map(|row| dot(row, relative));
        let mut weights = [1.0 - tail.iter().sum::<f64>(), tail[0], tail[1], tail[2]];
        if weights.iter().any(|&w| {
            !w.is_finite() || !(-CONTAINMENT_ROUNDOFF..=1.0 + CONTAINMENT_ROUNDOFF).contains(&w)
        }) {
            return None;
        }
        // Only repair roundoff-sized boundary weights; no physical clipping or
        // nearest-point extrapolation. The result remains a convex combination.
        weights = weights.map(|w| w.clamp(0.0, 1.0));
        let sum: f64 = weights.iter().sum();
        Some(weights.map(|w| w / sum))
    }
}

fn edges(points: [[f64; 3]; 4], scale: [f64; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|k| (points[i + 1][k] - points[0][k]) / scale[k]))
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(format!("critical-current interpolation: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::material::{CellSpanLimits, MaterialDataset};

    fn affine(position: [f64; 3]) -> f64 {
        100_000.0 + 2000.0 * position[0] - 3000.0 * position[1] + 50.0 * position[2]
    }

    fn points(axes: [&[f64]; 3]) -> Vec<MaterialPoint> {
        let mut points = Vec::new();
        for &t in axes[0] {
            for &b in axes[1] {
                for &a in axes[2] {
                    // A deliberately non-Cartesian measured grid, distinct from nominal.
                    let actual = [
                        t + 0.01 * a / 60.0,
                        b + 0.001 * (t - 20.0),
                        a + 0.1 * (b - 1.0),
                    ];
                    let current = affine(actual);
                    points.push(MaterialPoint {
                        source_row: points.len() as u32 + 1,
                        nominal_temperature_k: t,
                        nominal_field_t: b,
                        nominal_angle_deg: a,
                        temperature_k: actual[0],
                        applied_field_t: actual[1],
                        angle_from_normal_deg: actual[2],
                        ic_a_per_m: current,
                        bridge_ic_a: current * 0.001,
                        n_value: 20.0,
                    });
                }
            }
        }
        points
    }

    fn limits() -> CellSpanLimits {
        CellSpanLimits {
            temperature_k: 10.0,
            field_ratio: 3.0,
            angle_deg: 60.0,
        }
    }

    #[test]
    fn reproduces_affine_fields_on_measured_coordinates_and_convex_weights() {
        let points = points([&[20.0, 30.0], &[1.0, 3.0], &[0.0, 60.0]]);
        let model = IcInterpolator::new(&points, limits()).unwrap();
        assert_eq!(model.summary().tetrahedra, 6);
        for p in [
            [25.005, 2.005, 30.1],
            [23.007, 1.8, 42.0],
            [28.003, 2.6, 18.0],
        ] {
            let value = model.evaluate(p).unwrap().unwrap();
            assert!((value.ic_a_per_m - affine(p)).abs() < 1e-8);
            assert!((value.support.iter().map(|v| v.weight).sum::<f64>() - 1.0).abs() < 1e-14);
            assert!(
                value
                    .support
                    .iter()
                    .all(|v| v.weight >= 0.0 && v.weight <= 1.0)
            );
        }
        // Inside the nominal box but outside the warped measured hull.
        assert!(model.evaluate([20.0, 1.005, 30.0]).unwrap().is_none());
        assert!(model.evaluate([25.0, 2.0, 270.0]).unwrap().is_none());
        assert!(model.evaluate([25.0, 0.0, 30.0]).unwrap().is_none());
        assert!(model.evaluate([f64::NAN, 2.0, 30.0]).is_err());
    }

    /// A fold-seam grid: nominal angle levels {0, 90, 180} whose measured
    /// extremes jitter inside the fold — nominal-0 measured at −0.04 deg,
    /// nominal-180 at 179.65 — leaving a measured gap between 179.65 and
    /// −0.04+180 = 179.96. Queries near 180/0 are the documented OC-023
    /// knife-edge: without the declared seam they are unsupported; with
    /// it they interpolate between the measured points on both sides.
    fn seam_points(hi_measured: f64, lo_measured: f64) -> Vec<MaterialPoint> {
        let mut points = Vec::new();
        for &t in &[20.0_f64, 30.0] {
            for &b in &[1.0_f64, 3.0] {
                for &a in &[0.0_f64, 90.0, 180.0] {
                    let actual_angle = match a as i64 {
                        0 => lo_measured,
                        180 => hi_measured,
                        _ => a + 0.05 * (t - 20.0),
                    };
                    let actual = [t + 0.001 * b, b + 0.001 * (t - 20.0), actual_angle];
                    let current = affine(actual);
                    points.push(MaterialPoint {
                        source_row: points.len() as u32 + 1,
                        nominal_temperature_k: t,
                        nominal_field_t: b,
                        nominal_angle_deg: a,
                        temperature_k: actual[0],
                        applied_field_t: actual[1],
                        angle_from_normal_deg: actual[2],
                        ic_a_per_m: current,
                        bridge_ic_a: current * 0.001,
                        n_value: 20.0,
                    });
                }
            }
        }
        points
    }

    #[test]
    fn seam_stitches_only_across_measured_points_and_only_when_declared() {
        // The nominal grid's 90-degree angle steps need a wider declared
        // span gate than the shared fixture's 60.
        let limits = || CellSpanLimits {
            temperature_k: 10.0,
            field_ratio: 3.0,
            angle_deg: 95.0,
        };
        let points = seam_points(179.65, -0.04);
        let plain =
            IcInterpolator::with_method(&points, limits(), IcInterpolationMethod::Linear).unwrap();
        // The OC-023 knife-edge misses the unwarped hull by ~0.01 deg.
        for query in [[25.0, 2.0, -0.049], [25.0, 2.0, 179.95]] {
            assert!(plain.evaluate(query).unwrap().is_none(), "{query:?}");
        }

        let stitched = IcInterpolator::with_method_and_seam(
            &points,
            limits(),
            IcInterpolationMethod::Linear,
            Some(180.0),
        )
        .unwrap();
        assert_eq!(stitched.summary().seam_cells, 1); // one (t, b) nominal column
        let ic_by_row: std::collections::BTreeMap<u32, f64> = points
            .iter()
            .map(|p| (p.source_row, p.ic_a_per_m))
            .collect();
        let layer_of = |row: u32| {
            points
                .iter()
                .find(|p| p.source_row == row)
                .unwrap()
                .nominal_angle_deg
        };
        for query in [[25.0, 2.0, -0.049], [25.0, 2.0, 179.95]] {
            let estimate = stitched.evaluate(query).unwrap().unwrap();
            // The estimate is the convex blend of the *measured* node
            // values — under the declared periodicity a point measured at
            // −0.04 deg is the value at 179.96.
            let expected: f64 = estimate
                .support
                .iter()
                .map(|s| s.weight * ic_by_row[&s.source_row])
                .sum();
            assert!(
                (estimate.ic_a_per_m - expected).abs() < 1e-9,
                "{query:?} -> {} vs {expected}",
                estimate.ic_a_per_m
            );
            // Support draws from *both* measured sides of the fold.
            let layers: std::collections::BTreeSet<i64> = estimate
                .support
                .iter()
                .map(|s| layer_of(s.source_row) as i64)
                .collect();
            assert!(layers.contains(&0) && layers.contains(&180), "{layers:?}");
        }
        // The stitched dataset measures the whole 180-degree period, so
        // the closed seam covers the full angular circle — but only the
        // fold: field and temperature still gate. A query outside the
        // measured field hull stays unsupported in every representation.
        assert!(stitched.evaluate([25.0, 5.0, 179.95]).unwrap().is_none());
        assert!(stitched.evaluate([25.0, 5.0, -0.049]).unwrap().is_none());

        // Measured layers overlapping across the fold: no seam cell is
        // legal (the gap is nonpositive), the rejection is counted — and
        // coverage then comes only from ordinary cells, never a
        // cross-fold blend. Here the 179.98-measured layer already
        // covers the query's 179.951 representation outright.
        let overlapped = seam_points(179.98, -0.04);
        let stitched_overlap = IcInterpolator::with_method_and_seam(
            &overlapped,
            limits(),
            IcInterpolationMethod::Linear,
            Some(180.0),
        )
        .unwrap();
        assert_eq!(stitched_overlap.summary().seam_cells, 0);
        assert_eq!(stitched_overlap.summary().seam_span_rejected_cells, 1);
        let estimate = stitched_overlap
            .evaluate([25.0, 2.0, -0.049])
            .unwrap()
            .unwrap();
        let layers: std::collections::BTreeSet<i64> = estimate
            .support
            .iter()
            .map(|s| {
                overlapped
                    .iter()
                    .find(|p| p.source_row == s.source_row)
                    .unwrap()
                    .nominal_angle_deg as i64
            })
            .collect();
        assert!(
            !layers.contains(&0),
            "fold query must not blend nominal-0: {layers:?}"
        );
    }

    /// The documented OC-023 knife-edge on the real THEVA bundle: at
    /// 25 K, |B| = 6.28 T, theta ~ +/-0.05 deg misses the measured hull
    /// by ~0.01 deg. Without the declared seam the query is unsupported
    /// (the record's INCONCLUSIVE); with it the query interpolates
    /// between the measured nominal-0 and nominal-180 layers.
    #[test]
    #[allow(clippy::approx_constant)] // 6.28 T is OC-023's documented field, not TAU
    fn theva_fold_query_resolves_only_under_the_declared_seam() {
        let dataset = MaterialDataset::embedded_by_id(optcoil_model::material::THEVA_AP_ID)
            .expect("embedded THEVA bundle");
        let limits = dataset.metadata.max_cell_spans;
        let plain = IcInterpolator::with_method(
            &dataset.points,
            limits,
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let stitched = IcInterpolator::with_method_and_seam(
            &dataset.points,
            limits,
            IcInterpolationMethod::LogFieldLogCurrent,
            Some(180.0),
        )
        .unwrap();
        assert!(stitched.summary().seam_cells > 0);
        for query in [[25.0, 6.28, -0.049], [25.0, 6.28, 179.95]] {
            assert!(plain.evaluate(query).unwrap().is_none(), "{query:?}");
            let estimate = stitched.evaluate(query).unwrap().unwrap();
            assert!(estimate.ic_a_per_m.is_finite() && estimate.ic_a_per_m > 0.0);
        }
    }

    /// `n_value` is a second measured channel on the same tetrahedron
    /// weights: exact nodes return their own exponent, and an interior
    /// query returns the support-weighted linear blend — under the
    /// logarithmic method too, since only B and Ic transform.
    #[test]
    fn n_value_interpolates_linearly_on_the_same_support_weights() {
        // An n that varies per node — affine in the measured position, so
        // the linear method's tetrahedron weights reproduce it exactly.
        let n_law = |p: [f64; 3]| 15.0 + 0.2 * (p[0] - 20.0) + 1.5 * (p[1] - 1.0) + 0.05 * p[2];
        let mut points = points([&[20.0, 30.0], &[1.0, 3.0], &[0.0, 60.0]]);
        for point in &mut points {
            point.n_value = n_law(point.position());
        }
        let model = IcInterpolator::new(&points, limits()).unwrap();
        // Exact measurement: the node's own n, not a blend.
        let node = points[3].position();
        let exact = model.evaluate(node).unwrap().unwrap();
        assert!(exact.exact_measurement);
        assert_eq!(exact.n_value, points[3].n_value);
        // Interior query: affine n is reproduced exactly by linear weights.
        let query = [25.005, 2.005, 30.1];
        let value = model.evaluate(query).unwrap().unwrap();
        assert!((value.n_value - n_law(query)).abs() < 1e-9);

        // Under the logarithmic method the tetrahedron weights live in
        // transformed space, so n is no longer the affine value — but it
        // must still equal the support-weighted blend of node exponents.
        let n_by_row: std::collections::BTreeMap<u32, f64> =
            points.iter().map(|p| (p.source_row, p.n_value)).collect();
        let log_model = IcInterpolator::with_method(
            &points,
            limits(),
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let value = log_model.evaluate(query).unwrap().unwrap();
        let expected: f64 = value
            .support
            .iter()
            .map(|s| s.weight * n_by_row[&s.source_row])
            .sum();
        assert!((value.n_value - expected).abs() < 1e-12);
        assert!(value.n_value > 1.0);

        // A nonfinite or nonphysical measured exponent is rejected at
        // build time — it can never reach an estimate.
        let mut corrupted = points.clone();
        corrupted[0].n_value = f64::NAN;
        assert!(IcInterpolator::new(&corrupted, limits()).is_err());
        corrupted[0].n_value = 0.5; // n <= 1 is not a power-law exponent
        assert!(IcInterpolator::new(&corrupted, limits()).is_err());
    }

    #[test]
    fn missing_nodes_and_span_limits_cannot_be_bridged_silently() {
        let mut points = points([
            &[20.0, 25.0, 30.0, 35.0],
            &[1.0, 2.0, 3.0, 4.0],
            &[0.0, 30.0, 60.0, 90.0],
        ]);
        let removed = points.remove(
            points
                .iter()
                .position(|p| p.nominal_position() == [25.0, 2.0, 30.0])
                .unwrap(),
        );
        let model = IcInterpolator::new(&points, limits()).unwrap();
        assert_eq!(model.summary().missing_corner_cells, 8);
        assert!(model.evaluate(removed.position()).unwrap().is_none());
        let mut narrow = limits();
        narrow.temperature_k = 1.0;
        assert!(IcInterpolator::new(&points, narrow).is_err());
        points[1].source_row = points[0].source_row;
        assert!(IcInterpolator::new(&points, limits()).is_err());
    }

    #[test]
    fn published_measurements_build_without_inverted_cells_and_reproduce_nodes() {
        let data = MaterialDataset::embedded().unwrap();
        let model = IcInterpolator::new(&data.points, data.metadata.max_cell_spans).unwrap();
        assert_eq!(model.summary().supported_cells, 1008);
        assert_eq!(model.summary().missing_corner_cells, 0);
        for point in &data.points {
            let value = model.evaluate(point.position()).unwrap().unwrap();
            assert_eq!(value.ic_a_per_m, point.ic_a_per_m);
            assert!(value.exact_measurement);
            assert_eq!(value.support[0].source_row, point.source_row);
        }
    }

    /// OC-006's low-field extension: the full 5 x 15 x 43 nominal grid is a
    /// complete rectangular grid (every combination measured) and every
    /// adjacent-level span stays within the dataset's declared cell spans,
    /// so this must build with zero missing-corner and zero span-rejected
    /// cells, exactly like the original dataset's full-mesh build above.
    #[test]
    fn lowfield_measurements_build_without_missing_corners_or_span_rejections() {
        let data = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
        assert_eq!(data.points.len(), 3225);
        assert_eq!(data.metadata.selection.nominal_temperature_k.len(), 5);
        assert_eq!(data.metadata.selection.nominal_field_t.len(), 15);
        let model = IcInterpolator::new(&data.points, data.metadata.max_cell_spans).unwrap();
        let summary = model.summary();
        assert_eq!(summary.nominal_axis_sizes, [5, 15, 43]);
        assert_eq!(summary.candidate_cells, 4 * 14 * 42);
        assert_eq!(summary.missing_corner_cells, 0);
        assert_eq!(summary.span_rejected_cells, 0);
        assert_eq!(summary.supported_cells, summary.candidate_cells);
        assert_eq!(summary.tetrahedra, summary.supported_cells * 6);
        for point in &data.points {
            let value = model.evaluate(point.position()).unwrap().unwrap();
            assert_eq!(value.ic_a_per_m, point.ic_a_per_m);
            assert!(value.exact_measurement);
            assert_eq!(value.support[0].source_row, point.source_row);
        }
    }

    #[test]
    fn logarithmic_method_reproduces_a_power_law_with_exponential_temperature_dependence() {
        let mut points = points([&[20.0, 30.0], &[1.0, 3.0], &[0.0, 60.0]]);
        let law = |p: [f64; 3]| (12.0 - 0.015 * p[0] - 0.7 * p[1].ln() + 0.001 * p[2]).exp();
        for point in &mut points {
            point.ic_a_per_m = law(point.position());
            point.bridge_ic_a = point.ic_a_per_m * 0.001;
        }
        let model = IcInterpolator::with_method(
            &points,
            limits(),
            IcInterpolationMethod::LogFieldLogCurrent,
        )
        .unwrap();
        let p = [25.005, 2.005, 30.1];
        assert!((model.evaluate(p).unwrap().unwrap().ic_a_per_m / law(p) - 1.0).abs() < 1e-12);
        assert!(model.evaluate([25.0, 0.0, 30.0]).unwrap().is_none());
    }
}
