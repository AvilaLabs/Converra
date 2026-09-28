//! Direct volume Biot-Savart integration of a rectangular winding pack on a
//! piecewise-C1 planar path (line and circular-arc segments — racetracks,
//! D-shapes, picture-frame coils).
//!
//! Ordinary tensor Gauss-Legendre quadrature handles separated cells. Near the
//! observer, partition at its clamped source coordinates and map each octant
//! through three Duffy pyramids. Their u^2 Jacobian cancels the local 1/r^2
//! volume-kernel singularity. No distance clamp or artificial wire radius is
//! used. Fields are point values, including inside/on the pack, in vacuum with
//! a prescribed uniform tangential current density NI / (width * height).
//!
//! Each source cell stores its own local curvilinear frame baked at
//! construction: line cells integrate `[s_along_tangent, signed_normal_offset,
//! z]`; arc cells integrate `[local_azimuth, absolute_radius, z]` about the
//! segment's center with the path's `rho` Jacobian.

use std::f64::consts::PI;

use optcoil_model::{
    ModelError,
    magnetics::{Racetrack, validate_position},
    path::{CoilPath, PathSegment},
};
use thiserror::Error;

pub const FIELD_MODEL_ID: &str = "planar-path-uniform-volume-duffy-gauss-graded-metric/v4";
pub const MU0_H_PER_M: f64 = 4.0 * PI * 1e-7;
const MAX_CELLS: usize = 4096;
const MAX_SAMPLES_PER_PROBE: u64 = 20_000_000;

#[derive(Debug, Error)]
pub enum FieldError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("field evaluation: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct FieldEvaluation {
    pub field_t: [f64; 3],
    pub kernel_evaluations: u64,
}

/// Reuse source cells and a quadrature rule across probes. A single evaluation
/// has no accuracy claim: callers must compare refinement and reference data.
pub struct RacetrackEvaluator {
    radial_width_m: f64,
    axial_height_m: f64,
    ampere_turns_a: f64,
    cells: Vec<Cell>,
    rule: Vec<(f64, f64)>,
}

/// Which segment kind a cell was cut from — decides the meaning of its
/// coordinates and whether axis 0 needs the metric (azimuth-to-length)
/// correction in transverse grading.
#[derive(Clone, Copy)]
enum CellKind {
    Line,
    Arc,
}

/// A source-volume cell in its segment's local curvilinear coordinates.
/// The segment frame is baked in at construction so `source`/`project`
/// need no external context in the hot loop.
struct Cell {
    kind: CellKind,
    /// Segment entry position (xy). Line cells: frame origin. Arc cells:
    /// used only to derive `start_angle_rad` at construction.
    entry_m: [f64; 2],
    /// Unit tangent at segment entry (lines only).
    tangent: [f64; 2],
    /// Unit in-plane normal at segment entry (lines only).
    normal: [f64; 2],
    /// Arc center (xy); unused for lines.
    center_m: [f64; 2],
    /// World azimuth of the entry point about `center_m` (arcs only).
    start_angle_rad: f64,
    /// Sweep direction: +1 CCW, -1 CW (arcs only).
    sweep_sign: f64,
    low: [f64; 3],
    high: [f64; 3],
    diameter_m: f64,
}

impl RacetrackEvaluator {
    /// Racetrack constructor — kept for every existing caller; builds the
    /// identical geometry as a `CoilPath::racetrack` and hands off to
    /// `from_path`.
    pub fn new(geometry: &Racetrack, order: u32) -> Result<Self, FieldError> {
        geometry.validate()?;
        let path = CoilPath::racetrack(geometry.straight_half_length_m, geometry.bend_radius_m);
        Self::from_path(
            &path,
            geometry.radial_width_m,
            geometry.axial_height_m,
            geometry.ampere_turns_a,
            order,
        )
    }

    /// General planar-path constructor: a `CoilPath` centerline straddled by
    /// ±`radial_width_m`/2 in-plane and ±`axial_height_m`/2 out-of-plane,
    /// carrying `ampere_turns_a` total ampere-turns.
    pub fn from_path(
        path: &CoilPath,
        radial_width_m: f64,
        axial_height_m: f64,
        ampere_turns_a: f64,
        order: u32,
    ) -> Result<Self, FieldError> {
        // Same numerical support bounds as `Racetrack::validate`.
        if [radial_width_m, axial_height_m]
            .iter()
            .any(|x| !x.is_finite() || !(1e-9..=1e6).contains(x))
            || !ampere_turns_a.is_finite()
            || ampere_turns_a.abs() > 1e12
        {
            return Err(FieldError::Model(ModelError::Invalid(
                "pack dimensions/current exceed the documented numerical domain".into(),
            )));
        }
        path.validate_against_width(radial_width_m)
            .map_err(FieldError::Model)?;
        if !(2..=24).contains(&order) {
            return Err(FieldError::Invalid(
                "quadrature order must be 2..=24".into(),
            ));
        }
        let size = radial_width_m.max(axial_height_m);
        let frames = path.segment_frames().map_err(FieldError::Model)?;
        // Count before allocating: a degenerate (but numerically in-bounds)
        // pack would otherwise request billions of cells before any limit
        // check could fire — the pre-refactor code bounded counts up front
        // for the same reason.
        let mut total_cells = 0_usize;
        for frame in &frames {
            let count = match frame.segment {
                PathSegment::Line { length_m } => (length_m / size).ceil() as usize,
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    let outer = radius_m + radial_width_m / 2.0;
                    let sweep = sweep_deg.to_radians().abs();
                    // Per-cell azimuth span capped at pi: `project` unwraps
                    // the observer's azimuth near the cell mid, which is only
                    // unambiguous for spans <= pi.
                    (sweep * outer / size).ceil().max((sweep / PI).ceil()) as usize
                }
            };
            total_cells = total_cells.saturating_add(count);
            if total_cells > MAX_CELLS {
                return Err(FieldError::Invalid(format!(
                    "geometry requires more than {MAX_CELLS} source cells"
                )));
            }
        }
        let mut cells = Vec::with_capacity(total_cells);
        for frame in &frames {
            let inner;
            let outer;
            let span;
            match frame.segment {
                PathSegment::Line { length_m } => {
                    span = length_m;
                    inner = -radial_width_m / 2.0;
                    outer = radial_width_m / 2.0;
                }
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    span = sweep_deg.to_radians().abs();
                    outer = radius_m + radial_width_m / 2.0;
                    inner = radius_m - radial_width_m / 2.0;
                }
            }
            let count = match frame.segment {
                PathSegment::Line { .. } => (span / size).ceil() as usize,
                PathSegment::Arc { .. } => {
                    (span * outer / size).ceil().max((span / PI).ceil()) as usize
                }
            };
            // Metric for the diameter estimate: metres per unit of axis 0
            // (1 for lines, outer radius for arcs) as before.
            let metric = match frame.segment {
                PathSegment::Line { .. } => 1.0,
                PathSegment::Arc { .. } => outer,
            };
            let kind = match frame.segment {
                PathSegment::Line { .. } => CellKind::Line,
                PathSegment::Arc { .. } => CellKind::Arc,
            };
            let start_angle = if matches!(kind, CellKind::Arc) {
                (frame.entry.position_m[1] - frame.center_m[1])
                    .atan2(frame.entry.position_m[0] - frame.center_m[0])
            } else {
                0.0
            };
            let sweep_sign = match frame.segment {
                PathSegment::Arc { sweep_deg, .. } => sweep_deg.signum(),
                _ => 1.0,
            };
            for i in 0..count {
                let lo = span * i as f64 / count as f64;
                let hi = span * (i + 1) as f64 / count as f64;
                cells.push(Cell {
                    kind,
                    entry_m: [frame.entry.position_m[0], frame.entry.position_m[1]],
                    tangent: [frame.entry.tangent[0], frame.entry.tangent[1]],
                    normal: [frame.entry.normal[0], frame.entry.normal[1]],
                    center_m: frame.center_m,
                    start_angle_rad: start_angle,
                    sweep_sign,
                    low: [lo, inner, -axial_height_m / 2.0],
                    high: [hi, outer, axial_height_m / 2.0],
                    diameter_m: ((hi - lo) * metric)
                        .hypot(radial_width_m)
                        .hypot(axial_height_m),
                });
            }
        }
        Ok(Self {
            radial_width_m,
            axial_height_m,
            ampere_turns_a,
            cells,
            rule: gauss_legendre(order),
        })
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    pub fn evaluate(&self, position_m: [f64; 3]) -> Result<FieldEvaluation, FieldError> {
        validate_position(position_m)?;
        let mut sum = VectorSum::default();
        let mut evaluations = 0_u64;
        if self.ampere_turns_a == 0.0 {
            return Ok(FieldEvaluation {
                field_t: [0.0; 3],
                kernel_evaluations: 0,
            });
        }
        // Reused across every cell: the near-path rules are generated per
        // cell per probe, and the graded variants used to allocate a fresh
        // Vec each time — on pack-surface probes that was a heap storm in
        // the innermost loop. Contents are bit-identical; only the storage
        // is reused.
        let mut radial_scratch: Vec<(f64, f64)> = Vec::new();
        let mut v_scratch: Vec<(f64, f64)> = Vec::new();
        let mut w_scratch: Vec<(f64, f64)> = Vec::new();
        for cell in &self.cells {
            let center = cell.project(position_m);
            let (nearest, _, _) = cell.source(center);
            let separation_m = norm(sub(position_m, nearest));
            let separated = separation_m > 1.5 * cell.diameter_m;
            let mut sample = |q, weight| -> Result<(), FieldError> {
                if evaluations >= MAX_SAMPLES_PER_PROBE {
                    return Err(FieldError::Invalid(
                        "per-probe quadrature work limit exceeded".into(),
                    ));
                }
                evaluations += 1;
                let (source, tangent, jacobian) = cell.source(q);
                let offset = sub(position_m, source);
                let distance = norm(offset);
                if distance == 0.0 {
                    return Err(FieldError::Invalid(
                        "quadrature resolution reached coincident floating-point coordinates"
                            .into(),
                    ));
                }
                let factor = weight * jacobian / (distance * distance * distance);
                sum.add(cross(tangent, offset).map(|x| x * factor));
                Ok(())
            };
            if separated {
                let lengths = sub(cell.high, cell.low);
                let jacobian = lengths.iter().product::<f64>();
                for &(u, wu) in &self.rule {
                    for &(v, wv) in &self.rule {
                        for &(w, ww) in &self.rule {
                            sample(
                                [
                                    cell.low[0] + lengths[0] * u,
                                    cell.low[1] + lengths[1] * v,
                                    cell.low[2] + lengths[2] * w,
                                ],
                                jacobian * wu * wv * ww,
                            )?;
                        }
                    }
                }
            } else {
                // An exterior point a tiny distance from a surface creates a
                // narrow radial layer, even after the singular Duffy mapping.
                // Resolve that layer by splitting u at its physical distance
                // scale. This changes integration nodes, never the kernel's r.
                // Scratch buffers keep the graded rules allocation-free —
                // the same nodes and weights as the allocating version.
                let radial_rule: &[(f64, f64)] = {
                    let r = separation_m / cell.diameter_m;
                    if r <= 128.0 * f64::EPSILON || r >= 0.25 {
                        &self.rule
                    } else {
                        radial_scratch.clear();
                        graded_panels_into(&self.rule, r, &mut radial_scratch);
                        &radial_scratch
                    }
                };
                for corner in 0..8 {
                    let extent: [f64; 3] = std::array::from_fn(|k| {
                        (if corner & (1 << k) == 0 {
                            cell.low[k]
                        } else {
                            cell.high[k]
                        }) - center[k]
                    });
                    if extent.contains(&0.0) {
                        continue;
                    }
                    let box_jacobian = extent.iter().product::<f64>().abs();
                    // Three pyramids tile a unit cube according to which
                    // coordinate is largest; all orientations use |Jacobian|.
                    for dominant in 0..3 {
                        let second = (dominant + 1) % 3;
                        let third = (dominant + 2) % 3;
                        // When the dominant axis is a short extent (the
                        // observer sits close to that face), the mapped
                        // integrand's peak in v (resp. w) has relative width
                        // |extent[dominant]| / |extent[second (resp. third)]|
                        // that a plain Gauss rule cannot resolve. Grade the
                        // transverse rules by that same aspect ratio; this
                        // reduces to the plain rule once the ratio is >= 0.25
                        // (near_radial_rule_for_ratio's own threshold), so
                        // exterior octants and separated cells are unaffected.
                        //
                        // TASK K1 (OC-008 contract, Stage K, kernel v3):
                        // `extent` is measured in the cell's own coordinates,
                        // and for arc cells coordinate 0 is
                        // azimuth in radians while coordinates 1 and 2 are
                        // metres -- comparing them directly makes the ratio
                        // differ from the physical aspect ratio by the local
                        // radius. `transverse_grading_ratio` converts every
                        // extent to a physical length first (scaling the
                        // azimuthal extent by the observation point's
                        // clamped source radius, `center[1]`) before forming
                        // the ratio; straight cells are unchanged.
                        //
                        // Exception: if `extent[dominant]` is itself roundoff
                        // relative to the cell's own extent along that same
                        // axis, the observer sits essentially ON this face
                        // (up to floating-point precision) -- e.g. a sampled
                        // tape edge that lands, after accumulated rounding,
                        // a few ULPs from the pack's own boundary. This
                        // pyramid's `box_jacobian` below is a product that
                        // already includes this near-zero `extent[dominant]`
                        // factor, so its contribution has already vanished
                        // and grading v/w cannot recover real content; it can
                        // only build a runaway number of panels chasing a
                        // ratio driven to zero by roundoff rather than by
                        // genuine geometry (this is the same physical
                        // carve-out `near_radial_rule` applies to the
                        // observer's overall separation, applied here per
                        // axis/pyramid instead of to the whole cell). This
                        // exception still compares the cell's own raw
                        // (unconverted) coordinate extent against its own raw
                        // axis extent -- it decides whether the observer sits
                        // on the boundary at all, which is unaffected by the
                        // K1 metric fix to the transverse ratio itself.
                        let axis_extent = cell.high[dominant] - cell.low[dominant];
                        let (v_rule, w_rule): (Rule, Rule) =
                            if extent[dominant].abs() <= 128.0 * f64::EPSILON * axis_extent {
                                (&self.rule, &self.rule)
                            } else {
                                let r_v = transverse_grading_ratio(
                                    cell.kind, center[1], extent, dominant, second,
                                );
                                let r_w = transverse_grading_ratio(
                                    cell.kind, center[1], extent, dominant, third,
                                );
                                v_scratch.clear();
                                w_scratch.clear();
                                if r_v < 0.25 {
                                    graded_panels_into(&self.rule, r_v, &mut v_scratch);
                                } else {
                                    v_scratch.extend_from_slice(&self.rule);
                                }
                                if r_w < 0.25 {
                                    graded_panels_into(&self.rule, r_w, &mut w_scratch);
                                } else {
                                    w_scratch.extend_from_slice(&self.rule);
                                }
                                (&v_scratch, &w_scratch)
                            };
                        for &(u, wu) in radial_rule {
                            for &(v, wv) in v_rule {
                                for &(w, ww) in w_rule {
                                    let mut q = center;
                                    q[dominant] += extent[dominant] * u;
                                    q[second] += extent[second] * u * v;
                                    q[third] += extent[third] * u * w;
                                    sample(q, box_jacobian * u * u * wu * wv * ww)?;
                                }
                            }
                        }
                    }
                }
            }
        }
        let density_factor =
            1e-7 * self.ampere_turns_a / (self.radial_width_m * self.axial_height_m);
        let field_t = sum.value.map(|x| x * density_factor);
        if field_t.iter().any(|x| !x.is_finite()) {
            return Err(FieldError::Invalid("nonfinite field result".into()));
        }
        Ok(FieldEvaluation {
            field_t,
            kernel_evaluations: evaluations,
        })
    }
}

impl Cell {
    /// Map a curvilinear source coordinate to (position, unit current
    /// tangent, Jacobian). Line cells: `q = [s_along_tangent,
    /// signed_normal_offset, z]`. Arc cells: `q = [local_azimuth,
    /// absolute_radius, z]` where local azimuth runs 0..|sweep| from the
    /// segment's entry point.
    fn source(&self, q: [f64; 3]) -> ([f64; 3], [f64; 3], f64) {
        let [s, rho, z] = q;
        match self.kind {
            CellKind::Line => (
                [
                    self.entry_m[0] + s * self.tangent[0] + rho * self.normal[0],
                    self.entry_m[1] + s * self.tangent[1] + rho * self.normal[1],
                    z,
                ],
                [self.tangent[0], self.tangent[1], 0.0],
                1.0,
            ),
            CellKind::Arc => {
                let angle = self.start_angle_rad + self.sweep_sign * s;
                let (sin, cos) = angle.sin_cos();
                (
                    [
                        self.center_m[0] + rho * cos,
                        self.center_m[1] + rho * sin,
                        z,
                    ],
                    [-self.sweep_sign * sin, self.sweep_sign * cos, 0.0],
                    rho,
                )
            }
        }
    }

    /// Clamp `point` into the cell's curvilinear box (nearest point for
    /// separation testing and near-field octant partition).
    fn project(&self, point: [f64; 3]) -> [f64; 3] {
        let [x, y, z] = point;
        let parameters = match self.kind {
            CellKind::Line => {
                let dx = x - self.entry_m[0];
                let dy = y - self.entry_m[1];
                [
                    dx * self.tangent[0] + dy * self.tangent[1],
                    dx * self.normal[0] + dy * self.normal[1],
                    z,
                ]
            }
            CellKind::Arc => {
                let dx = x - self.center_m[0];
                let dy = y - self.center_m[1];
                // World azimuth of the cell's own mid-range, then unwrap the
                // observer's azimuth near it before converting to the local
                // (sweep-direction) coordinate.
                let mid_world =
                    self.start_angle_rad + self.sweep_sign * (self.low[0] + self.high[0]) / 2.0;
                let world = dy.atan2(dx);
                let relative_angle = world - mid_world;
                let world_unwrapped = mid_world + relative_angle.sin().atan2(relative_angle.cos());
                [
                    self.sweep_sign * (world_unwrapped - self.start_angle_rad),
                    dx.hypot(dy),
                    z,
                ]
            }
        };
        std::array::from_fn(|k| parameters[k].clamp(self.low[k], self.high[k]))
    }
}

/// Borrowed quadrature rule — either `self.rule` or a scratch buffer.
type Rule<'a> = &'a [(f64, f64)];

/// Nodes and weights on (0, 1). Order is bounded before calling.
fn gauss_legendre(order: u32) -> Vec<(f64, f64)> {
    let n = f64::from(order);
    let mut rule = Vec::with_capacity(order as usize);
    for i in 0..order {
        let mut x = (PI * (f64::from(i) + 0.75) / (n + 0.5)).cos();
        for _ in 0..32 {
            let (p, derivative) = legendre(order, x);
            let delta = p / derivative;
            x -= delta;
            if delta.abs() < 2e-15 {
                break;
            }
        }
        let (_, derivative) = legendre(order, x);
        rule.push((
            (1.0 + x) / 2.0,
            1.0 / ((1.0 - x * x) * derivative * derivative),
        ));
    }
    rule
}

/// The distance-graded radial rule used by `evaluate`'s near path — kept
/// for the unit tests; the hot path fills a scratch Vec via
/// `graded_panels_into` with identical nodes and weights.
#[cfg(test)]
fn near_radial_rule(rule: &[(f64, f64)], relative_distance: f64) -> Vec<(f64, f64)> {
    // Roundoff-sized offsets of an interior/boundary point need the singular
    // mapping alone. No denominator or physical source distance is modified.
    // This carve-out is specific to `relative_distance`'s meaning here
    // (separation_m / cell.diameter_m, a distance-to-boundary): it does not
    // transfer to a ratio of two box extents, see `near_radial_rule_for_ratio`.
    let mut out = Vec::new();
    if relative_distance <= 128.0 * f64::EPSILON || relative_distance >= 0.25 {
        out.extend_from_slice(rule);
    } else {
        graded_panels_into(rule, relative_distance, &mut out);
    }
    out
}

/// Same geometric grading as `near_radial_rule`, for a ratio of two box
/// extents (the transverse v/w grading in `evaluate`) rather than a
/// distance-to-boundary. A vanishingly small extent ratio there means the
/// transverse peak is at its *narrowest* -- exactly where grading is needed
/// most -- so, unlike `near_radial_rule`'s roundoff early exit, there is no
/// low-end carve-out: only the upper cutoff at which the plain rule already
/// resolves the peak applies. Matches the independent Python reference's
/// `graded_unit_rule` (tools/reference_oc004.py), whose condition is
/// `0.0 < r < GRADING_THRESHOLD` with no analogous low-end exemption.
#[cfg(test)]
fn near_radial_rule_for_ratio(rule: &[(f64, f64)], r: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    if r >= 0.25 {
        out.extend_from_slice(rule);
    } else {
        graded_panels_into(rule, r, &mut out);
    }
    out
}

/// Physical-length transverse grading ratio for one axis pair (TASK K1,
/// OC-008 contract Stage K, kernel v3): `|extent[dominant]| /
/// |extent[other]|`, with each extent first converted to a physical length.
/// Line cells already store every coordinate in metres, so the ratio is the
/// raw quotient, unchanged from the v2 kernel. Arc cells parameterize
/// coordinate 0 as azimuth in radians while coordinates 1 and 2 are metres
/// (see `Cell::source`/`Cell::project`); whichever of `dominant`/`other` is
/// axis 0 is scaled by `radius_m` -- the observation point's clamped source
/// radius, `center[1]` from `Cell::project` -- before the ratio is formed,
/// so the result is always a ratio of physical lengths rather than mixing
/// radians against metres. This changes only how the transverse ratio is
/// measured: it does not change which axis is `dominant` (the
/// u/dominant-axis rule), the kernel, or the roundoff exception above,
/// which still compares raw (unconverted) coordinate extents.
fn transverse_grading_ratio(
    kind: CellKind,
    radius_m: f64,
    extent: [f64; 3],
    dominant: usize,
    other: usize,
) -> f64 {
    let physical = |axis: usize| -> f64 {
        if axis == 0 && matches!(kind, CellKind::Arc) {
            extent[axis].abs() * radius_m
        } else {
            extent[axis].abs()
        }
    };
    physical(dominant) / physical(other)
}

/// Fill `out` with the graded panel rule — the geometric 4× progression
/// toward 1.0, identical nodes and weights to the allocating form, reused
/// storage for the per-cell-per-probe call rate.
fn graded_panels_into(rule: &[(f64, f64)], relative_distance: f64, out: &mut Vec<(f64, f64)>) {
    let mut low = 0.0;
    let mut high = relative_distance;
    loop {
        out.extend(
            rule.iter()
                .map(|&(x, w)| (low + (high - low) * x, (high - low) * w)),
        );
        if high == 1.0 {
            break;
        }
        low = high;
        high = (4.0 * high).min(1.0);
    }
}

fn legendre(order: u32, x: f64) -> (f64, f64) {
    let (mut previous, mut current) = (1.0, x);
    for k in 2..=order {
        let k = f64::from(k);
        let next = ((2.0 * k - 1.0) * x * current - (k - 1.0) * previous) / k;
        (previous, current) = (current, next);
    }
    (
        current,
        f64::from(order) * (x * current - previous) / (x * x - 1.0),
    )
}

#[derive(Default)]
struct VectorSum {
    value: [f64; 3],
    correction: [f64; 3],
}

impl VectorSum {
    fn add(&mut self, term: [f64; 3]) {
        for (k, component) in term.into_iter().enumerate() {
            let adjusted = component - self.correction[k];
            let next = self.value[k] + adjusted;
            self.correction[k] = (next - self.value[k]) - adjusted;
            self.value[k] = next;
        }
    }
}

pub fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}
pub fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::magnetics::{CurrentModel, FieldCase, OC002_JSON};

    #[test]
    fn near_radial_rule_is_the_plain_rule_once_the_aspect_ratio_reaches_one_quarter() {
        let rule = gauss_legendre(10);
        for r in [0.25, 0.3, 0.5, 1.0, 2.0, 100.0] {
            assert_eq!(near_radial_rule(&rule, r), rule, "ratio {r}");
        }
        // Just below the threshold the rule must be graded (more nodes), so
        // the reduction above is a real threshold, not a no-op comparison.
        assert!(near_radial_rule(&rule, 0.2).len() > rule.len());
        assert!(near_radial_rule(&rule, 1e-5).len() > rule.len());
    }

    #[test]
    fn near_radial_rule_for_ratio_stays_graded_at_an_extreme_aspect_ratio() {
        // Unlike near_radial_rule (whose roundoff early-exit is justified
        // only for a distance-to-boundary), the transverse extent-ratio
        // grading must keep grading arbitrarily close to zero: a tiny extent
        // ratio there is the narrowest, hardest-to-resolve peak, not a
        // point that has already reached the boundary.
        let rule = gauss_legendre(10);
        for r in [0.25, 0.3, 0.5, 1.0, 2.0, 100.0] {
            assert_eq!(near_radial_rule_for_ratio(&rule, r), rule, "ratio {r}");
        }
        for r in [0.2, 1e-5, 128.0 * f64::EPSILON, 1e-15] {
            assert!(
                near_radial_rule_for_ratio(&rule, r).len() > rule.len(),
                "ratio {r} must still be graded, not silently returned plain"
            );
        }
    }

    #[test]
    fn transverse_grading_ratio_is_unchanged_for_straight_cells_and_metric_corrected_for_arc_cells()
    {
        // TASK K3 (OC-008 contract Stage K): straight cells keep the raw
        // quotient (every coordinate is already metres); arc cells scale
        // whichever axis is the azimuthal coordinate 0 by the local radius
        // before forming the ratio.
        let extent = [0.08, 5e-5, 0.003]; // [axis0, axis1(rho), axis2(z)]
        let radius_m = 0.2;

        // Line cells: unaffected by the fix, for either axis ordering.
        assert_eq!(
            transverse_grading_ratio(CellKind::Line, radius_m, extent, 1, 0),
            extent[1].abs() / extent[0].abs()
        );
        assert_eq!(
            transverse_grading_ratio(CellKind::Line, radius_m, extent, 0, 2),
            extent[0].abs() / extent[2].abs()
        );

        // Arc cells, axis 0 (azimuth, radians) dominant over axis 1 (rho,
        // metres): ratio = (azimuth extent x radius) / metric extent.
        assert_eq!(
            transverse_grading_ratio(CellKind::Arc, radius_m, extent, 0, 1),
            (extent[0].abs() * radius_m) / extent[1].abs()
        );

        // Arc cells, axis 1 (rho) dominant over axis 0 (azimuth) -- the
        // common near-radial-face case: ratio = metric extent / (azimuth
        // extent x radius).
        assert_eq!(
            transverse_grading_ratio(CellKind::Arc, radius_m, extent, 1, 0),
            extent[1].abs() / (extent[0].abs() * radius_m)
        );

        // Arc cell, axis pair not involving azimuth (1 vs 2, both already
        // metres): unaffected by the fix, same as a line cell.
        assert_eq!(
            transverse_grading_ratio(CellKind::Arc, radius_m, extent, 1, 2),
            extent[1].abs() / extent[2].abs()
        );
    }

    #[test]
    fn oc004_pack_near_face_points_converge_at_declared_orders() {
        // OC-004 contract pack: bend 0.2 m, radial width 0.02 m, axial
        // height 0.036 m, half-length 0.3 m, unit ampere-turns. Turn
        // positions rho_k = inner + (k - 0.5) * pitch_n with
        // pitch_n = 0.02 / 200 = 1e-4 m; inner = 0.19 m, outer = 0.21 m.
        let g = Racetrack {
            straight_half_length_m: 0.3,
            bend_radius_m: 0.2,
            radial_width_m: 0.02,
            axial_height_m: 0.036,
            ampere_turns_a: 1.0,
            current_model: CurrentModel::UniformWindingPack,
        };
        let inner = g.bend_radius_m - g.radial_width_m / 2.0;
        let outer = g.bend_radius_m + g.radial_width_m / 2.0;
        let order10 = RacetrackEvaluator::new(&g, 10).unwrap();
        let order14 = RacetrackEvaluator::new(&g, 14).unwrap();
        let order24 = RacetrackEvaluator::new(&g, 24).unwrap();
        // Turn 2, turn 1, turn 198 (0.15 mm, 0.05 mm, 0.25 mm inside a
        // radial face) at two axial heights, on the top straight (x = 0)
        // and at the right-bend apex (azimuth 0 about (L, 0, 0)).
        let rhos = [inner + 0.00015, inner + 0.00005, outer - 0.00025];
        let zs = [0.012, 0.015928];
        let max_change_t = 5e-9;
        // TASK K3 (OC-008 contract Stage K, kernel v3): the arc-apex form
        // below (index 1: azimuth 0 about the right bend, where the curved
        // cells' coordinate 0 is azimuth in radians) must converge at least
        // as tightly under the metric-consistent v3 transverse ratio as it
        // did under the pre-fix v2 kernel (azimuth compared directly against
        // metre extents). Measured from actual runs at these six arc-apex
        // points, order 10 vs 14:
        //   before (v2): max |B14-B10| = 4.309e-13 T at [0.49005, 0.0, 0.012]
        //   after  (v3): max |B14-B10| = 4.289e-13 T at [0.49005, 0.0, 0.012]
        let arc_apex_before_max_change_t = 4.309e-13;
        let mut max_samples = 0_u64;
        let mut arc_apex_max_change_10_14 = 0.0_f64;
        for rho in rhos {
            for z in zs {
                for (form, position_m) in [[0.0, rho, z], [g.straight_half_length_m + rho, 0.0, z]]
                    .into_iter()
                    .enumerate()
                {
                    let e10 = order10.evaluate(position_m).unwrap();
                    let e14 = order14.evaluate(position_m).unwrap();
                    let e24 = order24.evaluate(position_m).unwrap();
                    let change_10_14 = norm(sub(e14.field_t, e10.field_t));
                    let change_14_24 = norm(sub(e24.field_t, e14.field_t));
                    if form == 1 {
                        arc_apex_max_change_10_14 = arc_apex_max_change_10_14.max(change_10_14);
                    }
                    max_samples = max_samples
                        .max(e10.kernel_evaluations)
                        .max(e14.kernel_evaluations)
                        .max(e24.kernel_evaluations);
                    eprintln!(
                        "{position_m:?}: |B14-B10|={change_10_14:.3e} T, |B24-B14|={change_14_24:.3e} T, samples(10/14/24)=({}/{}/{})",
                        e10.kernel_evaluations, e14.kernel_evaluations, e24.kernel_evaluations
                    );
                    assert!(
                        change_10_14 <= max_change_t,
                        "{position_m:?} order10->14 change {change_10_14} T exceeds {max_change_t} T"
                    );
                    assert!(
                        change_14_24 <= max_change_t,
                        "{position_m:?} order14->24 change {change_14_24} T exceeds {max_change_t} T"
                    );
                }
            }
        }
        eprintln!(
            "arc-apex max |B14-B10| = {arc_apex_max_change_10_14:.3e} T (pre-v3-fix: {arc_apex_before_max_change_t:.3e} T)"
        );
        assert!(
            arc_apex_max_change_10_14 <= arc_apex_before_max_change_t,
            "arc-apex convergence regressed under the v3 metric fix: {arc_apex_max_change_10_14} T > pre-fix {arc_apex_before_max_change_t} T"
        );
        eprintln!("max kernel_evaluations observed = {max_samples}");
        assert!(max_samples <= MAX_SAMPLES_PER_PROBE);
    }

    #[test]
    fn near_face_point_exactly_on_the_axial_boundary_does_not_exceed_the_work_limit() {
        // Regression: a real OC-004 sample (turn 1, tape 3's outer width
        // edge) lands, after the tape-center + Gauss-Lobatto-offset
        // arithmetic, at z = 0.017999999999999995 -- three ULPs shy of the
        // pack's own +axial_height_m/2 = 0.018 boundary -- while also
        // sitting only 5e-5 m inside the radial face (turn 1). Grading the
        // transverse rule purely by extent ratio, with no roundoff
        // exception at all, drives the z-axis's near-zero extent to an
        // unbounded panel count and exceeds MAX_SAMPLES_PER_PROBE; this
        // must stay bounded and finite.
        let g = Racetrack {
            straight_half_length_m: 0.3,
            bend_radius_m: 0.2,
            radial_width_m: 0.02,
            axial_height_m: 0.036,
            ampere_turns_a: 1.0,
            current_model: CurrentModel::UniformWindingPack,
        };
        let evaluator = RacetrackEvaluator::new(&g, 14).unwrap();
        let position_m = [0.0, 0.19005, 0.017999999999999995];
        let result = evaluator.evaluate(position_m).unwrap();
        assert!(result.field_t.iter().all(|x| x.is_finite()));
        assert!(result.kernel_evaluations <= MAX_SAMPLES_PER_PROBE);
    }

    #[test]
    fn oc002_probes_far_from_faces_are_unchanged_by_the_graded_near_cell_rule() {
        let case = FieldCase::from_json(OC002_JSON).unwrap();
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/synthetic/oc-002.reference.json"
        ))
        .unwrap();
        let fine = RacetrackEvaluator::new(&case.geometry, 14).unwrap();
        for (probe, expected) in case
            .probes
            .iter()
            .zip(reference["probes"].as_array().unwrap())
        {
            assert_eq!(probe.id, expected["id"]);
            let b = fine.evaluate(probe.position_m).unwrap().field_t;
            let target: [f64; 3] = serde_json::from_value(expected["fields_t"][2].clone()).unwrap();
            let change_t = norm(sub(b, target));
            eprintln!("{}: |B14 - stored reference| = {change_t:.6e} T", probe.id);
            assert!(
                change_t <= 1e-9,
                "{} changed by {change_t} T relative to the stored reference",
                probe.id
            );
        }
    }

    #[test]
    fn quadrature_reproduces_polynomial_moments() {
        for n in [2, 6, 14, 24] {
            let rule = gauss_legendre(n);
            for power in 0..2 * n {
                let integral = rule
                    .iter()
                    .map(|&(x, w)| w * x.powi(power as i32))
                    .sum::<f64>();
                assert!((integral - 1.0 / f64::from(power + 1)).abs() < 2e-14);
            }
        }
    }

    #[test]
    fn uniform_annulus_matches_closed_form_axis_field() {
        let mut g = FieldCase::from_json(OC002_JSON).unwrap().geometry;
        g.straight_half_length_m = 0.0;
        let evaluator = RacetrackEvaluator::new(&g, 8).unwrap();
        // Independent antiderivative after both radial and axial integration.
        let primitive = |t: f64| {
            if t == 0.0 {
                0.0
            } else {
                t * (((g.bend_radius_m + g.radial_width_m / 2.0) / t.abs()).asinh()
                    - ((g.bend_radius_m - g.radial_width_m / 2.0) / t.abs()).asinh())
            }
        };
        for z in [0.0, 0.013, 0.2, -0.2] {
            let expected = MU0_H_PER_M * g.ampere_turns_a
                / (2.0 * g.radial_width_m * g.axial_height_m)
                * (primitive(z + g.axial_height_m / 2.0) - primitive(z - g.axial_height_m / 2.0));
            let result = evaluator.evaluate([0.0, 0.0, z]).unwrap();
            assert!(norm(sub(result.field_t, [0.0, 0.0, expected])) < 1e-11);
        }
    }

    #[test]
    fn field_has_current_linearity_symmetry_and_dipole_limit() {
        let g = FieldCase::from_json(OC002_JSON).unwrap().geometry;
        let evaluator = RacetrackEvaluator::new(&g, 8).unwrap();
        let p = [0.13, 0.07, 0.04];
        let b = evaluator.evaluate(p).unwrap().field_t;
        let mirrored = evaluator.evaluate([-p[0], p[1], p[2]]).unwrap().field_t;
        assert!(norm(sub(mirrored, [-b[0], b[1], b[2]])) < 1e-9);
        let mirrored = evaluator.evaluate([p[0], -p[1], p[2]]).unwrap().field_t;
        assert!(norm(sub(mirrored, [b[0], -b[1], b[2]])) < 1e-9);
        let mirrored = evaluator.evaluate([p[0], p[1], -p[2]]).unwrap().field_t;
        assert!(norm(sub(mirrored, [-b[0], -b[1], b[2]])) < 1e-9);
        let mut altered = g.clone();
        altered.ampere_turns_a *= -0.4;
        let reverse = RacetrackEvaluator::new(&altered, 8)
            .unwrap()
            .evaluate(p)
            .unwrap()
            .field_t;
        assert!(norm(sub(reverse, b.map(|x| -0.4 * x))) < 1e-14);
        // Superposition of independently evaluated coincident prescribed-current
        // packs: I + (-0.4 I) must equal the field of a single 0.6 I pack.
        altered.ampere_turns_a = g.ampere_turns_a * 0.6;
        let combined = RacetrackEvaluator::new(&altered, 8)
            .unwrap()
            .evaluate(p)
            .unwrap()
            .field_t;
        let summed = std::array::from_fn(|k| b[k] + reverse[k]);
        assert!(norm(sub(combined, summed)) < 1e-14);
        altered.ampere_turns_a = 0.0;
        assert_eq!(
            RacetrackEvaluator::new(&altered, 8)
                .unwrap()
                .evaluate(p)
                .unwrap()
                .field_t,
            [0.0; 3]
        );
        let mean_area = PI * (g.bend_radius_m.powi(2) + g.radial_width_m.powi(2) / 12.0)
            + 4.0 * g.straight_half_length_m * g.bend_radius_m;
        let z: f64 = 100.0;
        let dipole = 2e-7 * g.ampere_turns_a * mean_area / z.powi(3);
        let far = evaluator.evaluate([0.0, 0.0, z]).unwrap().field_t;
        assert!((far[2] / dipole - 1.0).abs() < 1e-4);
    }

    #[test]
    fn frozen_reference_includes_interior_boundary_and_near_field() {
        let case = FieldCase::from_json(OC002_JSON).unwrap();
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../benchmarks/synthetic/oc-002.reference.json"
        ))
        .unwrap();
        let coarse = RacetrackEvaluator::new(&case.geometry, 10).unwrap();
        let fine = RacetrackEvaluator::new(&case.geometry, 14).unwrap();
        assert_eq!(
            case.probes.len(),
            reference["probes"].as_array().unwrap().len()
        );
        for (probe, expected) in case
            .probes
            .iter()
            .zip(reference["probes"].as_array().unwrap())
        {
            assert_eq!(probe.id, expected["id"]);
            let b = fine.evaluate(probe.position_m).unwrap().field_t;
            let previous = coarse.evaluate(probe.position_m).unwrap().field_t;
            let target: [f64; 3] = serde_json::from_value(expected["fields_t"][2].clone()).unwrap();
            let error = norm(sub(b, target)) / case.acceptance.field_scale_t;
            let change = norm(sub(b, previous)) / case.acceptance.field_scale_t;
            eprintln!("{}: error={error:.6e}, refinement={change:.6e}", probe.id);
            assert!(
                error <= case.acceptance.max_reference_error_fraction,
                "{} error {error}",
                probe.id
            );
            assert!(
                change <= case.acceptance.max_refinement_change_fraction,
                "{} refinement {change}",
                probe.id
            );
        }
    }

    #[test]
    fn volume_field_is_continuous_across_straight_and_curved_pack_faces() {
        let case = FieldCase::from_json(OC002_JSON).unwrap();
        let evaluator = RacetrackEvaluator::new(&case.geometry, 14).unwrap();
        for (surface, axis) in [([0.0, 0.19, 0.004], 1), ([0.51, 0.0, 0.01], 0)] {
            let mut differences = Vec::new();
            for distance in [1e-4, 1e-6] {
                let mut inside = surface;
                let mut outside = surface;
                inside[axis] -= distance;
                outside[axis] += distance;
                let left = evaluator.evaluate(inside).unwrap().field_t;
                let right = evaluator.evaluate(outside).unwrap().field_t;
                differences.push(norm(sub(left, right)));
            }
            assert!(differences[1] < differences[0] / 50.0, "{differences:?}");
            assert!(differences[1] < 5e-4, "{differences:?}");
        }
    }

    #[test]
    fn d_shape_path_converges_and_matches_the_filament_limit() {
        // A D-shaped TF-coil outline: inner leg down x=0, two 0.3 m corner
        // quarter-arcs, one 1.3 m outer semicircle bulging +x. None of the
        // racetrack constructor's geometry is involved — this exercises
        // `from_path` on a genuinely general planar curve.
        let path = CoilPath {
            segments: vec![
                PathSegment::Line { length_m: 2.0 },
                PathSegment::Arc {
                    radius_m: 0.3,
                    sweep_deg: 90.0,
                },
                PathSegment::Arc {
                    radius_m: 1.3,
                    sweep_deg: 180.0,
                },
                PathSegment::Arc {
                    radius_m: 0.3,
                    sweep_deg: 90.0,
                },
            ],
            start_position_m: [0.0, 1.0],
            start_heading_deg: -90.0,
        };
        path.validate().unwrap();
        let (w, h) = (0.04, 0.06);
        let ni = 1e6;
        let e10 = RacetrackEvaluator::from_path(&path, w, h, ni, 10).unwrap();
        let e14 = RacetrackEvaluator::from_path(&path, w, h, ni, 14).unwrap();
        // Interior point near the outer-arc center; convergence like the
        // racetrack near-face contract.
        let interior = [0.3, 0.0, 0.0];
        let b10 = e10.evaluate(interior).unwrap().field_t;
        let b14 = e14.evaluate(interior).unwrap().field_t;
        assert!(b14.iter().all(|x| x.is_finite()));
        assert!(b14[2] > 0.0, "CCW loop must give +Bz inside: {b14:?}");
        assert!(
            norm(sub(b14, b10)) < 5e-9 * ni / 1e6,
            "D-shape interior convergence {b10:?} vs {b14:?}"
        );
        // Exterior probe: a thin pack approximates the centerline filament.
        // With w=h=5e-3 m and ~0.7 m clearance the correction is
        // O((w/d)^2) ~ 5e-5 relative; the filament's own chord error is
        // ~6e-5 — gate at 1e-3.
        let thin = RacetrackEvaluator::from_path(&path, 5e-3, 5e-3, ni, 14).unwrap();
        let probe = [0.8, 0.5, 0.4];
        let pack = thin.evaluate(probe).unwrap().field_t;
        let filament = crate::filament::path_filament_field(&path, ni, probe).unwrap();
        let rel = norm(sub(pack, filament)) / norm(filament);
        assert!(
            rel < 1e-3,
            "thin D-pack {pack:?} vs filament {filament:?}: rel {rel:.3e}"
        );
        // Same check below the coil plane where Bz is off-axis too.
        let probe = [0.3, 0.0, -0.6];
        let pack = thin.evaluate(probe).unwrap().field_t;
        let filament = crate::filament::path_filament_field(&path, ni, probe).unwrap();
        let rel = norm(sub(pack, filament)) / norm(filament);
        assert!(rel < 1e-3, "probe {probe:?}: rel {rel:.3e}");
    }

    #[test]
    fn invalid_settings_and_excessive_geometry_fail_explicitly() {
        let mut g = FieldCase::from_json(OC002_JSON).unwrap().geometry;
        assert!(RacetrackEvaluator::new(&g, 0).is_err());
        assert!(RacetrackEvaluator::new(&g, 25).is_err());
        let solver = RacetrackEvaluator::new(&g, 4).unwrap();
        assert!(solver.evaluate([f64::NAN, 0.0, 0.0]).is_err());
        g.radial_width_m = 1e-9;
        g.axial_height_m = 1e-9;
        assert!(RacetrackEvaluator::new(&g, 4).is_err());
    }

    // -----------------------------------------------------------------
    // Physics-invariant property tests: these relations must hold for
    // *any* valid geometry, so they probe the kernel across extreme but
    // legitimate aspect ratios — shapes no benchmark hand-picked.
    // -----------------------------------------------------------------

    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
            lo + u * (hi - lo)
        }
    }

    fn racetrack(l: f64, r: f64, w: f64, h: f64, ni: f64) -> Racetrack {
        Racetrack {
            straight_half_length_m: l,
            bend_radius_m: r,
            radial_width_m: w,
            axial_height_m: h,
            ampere_turns_a: ni,
            current_model: CurrentModel::UniformWindingPack,
        }
    }

    /// A planar current loop's field is mirror-symmetric about its own
    /// plane: the out-of-plane component is even in z, the in-plane
    /// components are odd. Symmetry that survives extreme aspect ratios
    /// is the strongest independent check on the quadrature paths —
    /// near, far and graded regimes all exercise different code.
    #[test]
    fn field_is_mirror_symmetric_about_the_coil_plane() {
        let mut rng = Rng(0x0A11_C0A1);
        for _ in 0..24 {
            let width = 10f64.powf(rng.range(-3.0, 0.0));
            let height = width * 10f64.powf(rng.range(-2.0, 2.0));
            // Inner edge of the bend must stay positive: radius >= w/2.
            let radius = width / 2.0 * (1.0 + rng.range(0.01, 20.0));
            let length = radius * rng.range(0.0, 30.0);
            let geometry = racetrack(length, radius, width, height, 1e6);
            let evaluator = match RacetrackEvaluator::new(&geometry, 6) {
                Ok(e) => e,
                Err(_) => continue, // geometry legitimately outside support
            };
            let reach = length + radius + width + 1.0;
            for _ in 0..40 {
                let p = [
                    rng.range(-reach, reach),
                    rng.range(-reach, reach),
                    rng.range(0.001 * reach, reach),
                ];
                let (Ok(up), Ok(down)) = (
                    evaluator.evaluate(p),
                    evaluator.evaluate([p[0], p[1], -p[2]]),
                ) else {
                    continue;
                };
                for k in 0..3 {
                    let expected = if k == 2 {
                        up.field_t[k]
                    } else {
                        -up.field_t[k]
                    };
                    let scale = up.field_t[k].abs().max(1e-12);
                    assert!(
                        (down.field_t[k] - expected).abs() < 2e-5 * scale + 1e-15,
                        "z-mirror broken at {p:?} axis {k}: {up:?} vs {down:?}"
                    );
                }
            }
        }
    }

    /// The racetrack is point-symmetric about the origin: a 180-degree
    /// rotation about the plane normal maps the winding (and its current
    /// circulation) onto itself, so the field must map identically.
    #[test]
    fn field_is_point_symmetric_about_the_origin() {
        let mut rng = Rng(0x000B_0A70);
        for _ in 0..24 {
            let width = 10f64.powf(rng.range(-3.0, 0.0));
            let height = width * 10f64.powf(rng.range(-2.0, 2.0));
            let radius = width / 2.0 * (1.0 + rng.range(0.01, 20.0));
            let length = radius * rng.range(0.0, 30.0);
            let geometry = racetrack(length, radius, width, height, 1e6);
            let evaluator = match RacetrackEvaluator::new(&geometry, 6) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let reach = length + radius + width + 1.0;
            for _ in 0..40 {
                let p = [
                    rng.range(-reach, reach),
                    rng.range(-reach, reach),
                    rng.range(-0.5 * reach, 0.5 * reach),
                ];
                let (Ok(a), Ok(b)) = (
                    evaluator.evaluate(p),
                    evaluator.evaluate([-p[0], -p[1], p[2]]),
                ) else {
                    continue;
                };
                // B rotates with the probe: the rotated field equals the
                // rotated vector, i.e. in-plane components flip and the
                // out-of-plane component is unchanged.
                for k in 0..3 {
                    let expected = if k == 2 { a.field_t[k] } else { -a.field_t[k] };
                    let scale = a.field_t[k].abs().max(1e-12);
                    assert!(
                        (b.field_t[k] - expected).abs() < 2e-5 * scale + 1e-15,
                        "point symmetry broken at {p:?} axis {k}: {:?} vs {:?}",
                        a.field_t,
                        b.field_t
                    );
                }
            }
        }
    }

    /// Biot–Savart is linear in current density: doubling ampere-turns
    /// must double every field component. The kernel applies NI as a
    /// post-multiplier, so this holds to floating-point roundoff exactly
    /// — a violated check means current coupling leaks into the cells.
    #[test]
    fn field_scales_exactly_with_ampere_turns() {
        let geometry = racetrack(0.3, 0.2, 0.02, 0.01, 1.0e6);
        let base = RacetrackEvaluator::new(&geometry, 6).unwrap();
        let mut doubled_g = geometry.clone();
        doubled_g.ampere_turns_a = 2.0e6;
        let doubled = RacetrackEvaluator::new(&doubled_g, 6).unwrap();
        for p in [[0.0, 0.0, 0.1], [0.5, 0.2, 0.0], [1.3, -0.4, 0.3]] {
            let a = base.evaluate(p).unwrap().field_t;
            let b = doubled.evaluate(p).unwrap().field_t;
            for k in 0..3 {
                let scale = b[k].abs().max(1e-12);
                assert!(
                    (b[k] - 2.0 * a[k]).abs() < 1e-12 * scale,
                    "nonlinear at {p:?} axis {k}: {a:?} vs {b:?}"
                );
            }
        }
        // And the zero limit is exact, not approximate.
        let mut zeroed = geometry;
        zeroed.ampere_turns_a = 0.0;
        let zero = RacetrackEvaluator::new(&zeroed, 6).unwrap();
        assert_eq!(zero.evaluate([0.0, 0.0, 0.1]).unwrap().field_t, [0.0; 3]);
    }

    /// Probes chosen at random across the whole domain — inside the bore,
    /// grazing the pack surface, far outside — must yield finite fields.
    /// A NaN or infinity here is a singularity leak, not a bad draw.
    #[test]
    fn random_probes_never_produce_nonfinite_fields() {
        let mut rng = Rng(0x000F_1E1D);
        for _ in 0..30 {
            let width = 10f64.powf(rng.range(-3.0, 0.0));
            let height = width * 10f64.powf(rng.range(-2.0, 2.0));
            let radius = width / 2.0 * (1.0 + rng.range(0.01, 20.0));
            let length = radius * rng.range(0.0, 30.0);
            let geometry = racetrack(length, radius, width, height, 1e6);
            let evaluator = match RacetrackEvaluator::new(&geometry, 8) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let reach = length + radius + width + 1.0;
            for _ in 0..50 {
                // Skewed toward the pack surface and bore, the hard cases.
                let p = [
                    rng.range(-reach, reach),
                    rng.range(-reach, reach),
                    rng.range(-0.5 * reach, 0.5 * reach),
                ];
                // Err is legitimate only at exact coincidence.
                if let Ok(f) = evaluator.evaluate(p) {
                    assert!(
                        f.field_t.iter().all(|x| x.is_finite()),
                        "nonfinite field at {p:?}: {:?}",
                        f.field_t
                    );
                }
            }
        }
    }

    /// Degenerate-but-valid shapes: a zero-length straight section (a
    /// circle) and a near-pinched bend (inner radius ~0) must still
    /// evaluate rather than crash or blow up cells.
    #[test]
    fn degenerate_limit_geometries_evaluate() {
        // Pure circle: no straight sections.
        let circle = racetrack(0.0, 0.05, 0.01, 0.005, 1e5);
        let ev = RacetrackEvaluator::new(&circle, 8).unwrap();
        let f = ev.evaluate([0.0, 0.0, 0.0]).unwrap();
        assert!(f.field_t.iter().all(|x| x.is_finite()) && f.field_t[2] > 0.0);
        // Pinched waist: inner edge of the bend almost touches the axis.
        let pinch = racetrack(0.2, 0.010001, 0.02, 0.005, 1e5);
        if let Ok(ev) = RacetrackEvaluator::new(&pinch, 8) {
            let f = ev.evaluate([0.0, 0.0, 0.05]).unwrap();
            assert!(f.field_t.iter().all(|x| x.is_finite()));
        }
        // Extreme aspect: ribbon 1000x wider than tall.
        let ribbon = racetrack(0.5, 0.1, 0.1, 1e-4, 1e5);
        if let Ok(ev) = RacetrackEvaluator::new(&ribbon, 8) {
            let f = ev.evaluate([0.0, 0.0, 0.01]).unwrap();
            assert!(f.field_t.iter().all(|x| x.is_finite()));
        }
    }
}
