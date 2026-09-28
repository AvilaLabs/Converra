//! Non-planar 3D winding paths (coupled schema v9 / search schema v19):
//! a centerline built from helix segments — the CCT- and CORC-class
//! winding primitive, where the tape's broad face tracks the winding's
//! cylinder-radial direction.
//!
//! A `CoilPath3D` need not close: a helical layer is one continuous
//! conductor whose ends leave through leads, so closure is declared
//! (`closed`) rather than assumed. The path's reference normal `n_ref`
//! is the cylinder-radial direction, pointing outward — for a helix the
//! Frenet normal is exactly that direction — and the in-surface
//! binormal `w_ref = n_ref × t̂` completes the tape frame exactly the
//! way `ẑ` completes a planar path's.
//!
//! Every helix segment carries its full axis declaration; consecutive
//! segments must join position- and tangent-continuously (the same
//! convention `CoilPath` enforces by construction — here it is checked,
//! since each segment is declared absolutely).
//!
//! Non-planar paths evaluate under a declared Cartesian field map only:
//! the engine's pack field evaluator is planar, and a helix pack's
//! self-field solve is out of scope. The case schema enforces that.

use serde::{Deserialize, Serialize};

use crate::ModelError;
use crate::path::PathPose;

fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// One segment of a non-planar winding path. Each segment declares its
/// full geometry absolutely; the path validates that consecutive
/// segments join continuously.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PathSegment3D {
    /// A helix on a declared axis: starts at `start_azimuth_deg` on the
    /// cylinder of `radius_m` about (`axis_origin_m`, `axis_dir`), winds
    /// `turns` signed turns while rising `rise_per_turn_m` per turn along
    /// `+axis_dir`. `turns` sets the winding sense about the axis
    /// (signed — negative winds the opposite hand); `rise_per_turn_m`
    /// is the axial advance per turn and may itself be signed. This is
    /// the CCT/CORC element: one helical conductor of `|turns|` turns.
    Helix {
        /// A point on the winding axis (m).
        axis_origin_m: [f64; 3],
        /// The winding axis direction — normalized at validation; need
        /// not be unit on input but must be nonzero.
        axis_dir: [f64; 3],
        /// Cylinder radius (m), > 0.
        radius_m: f64,
        /// Azimuth of the segment start about (`e1`, `e2`), the
        /// orthonormal basis the validation resolves perpendicular to
        /// `axis_dir`. Degrees.
        start_azimuth_deg: f64,
        /// Signed turns swept: the winding sense about `axis_dir`.
        /// `|turns|·2π` radians of azimuth.
        turns: f64,
        /// Axial advance per turn along `+axis_dir` (m) — the helix
        /// pitch; signed.
        rise_per_turn_m: f64,
    },
}

/// A resolved helix: the declared axis reduced to an orthonormal
/// (`e1`, `e2`, `a`) basis plus the parametrization constants. All pose
/// and length arithmetic runs on this form.
#[derive(Debug, Clone, Copy)]
struct ResolvedHelix {
    origin_m: [f64; 3],
    e1: [f64; 3],
    e2: [f64; 3],
    a: [f64; 3],
    radius_m: f64,
    /// Axial advance per radian of sweep — `rise_per_turn_m / 2π`.
    rise_per_rad_m: f64,
    /// Arc length per radian — `hypot(R, c)`.
    lambda: f64,
    start_azimuth_rad: f64,
    /// Signed swept angle (rad) — `turns · 2π`.
    sweep_rad: f64,
}

impl ResolvedHelix {
    fn resolve(segment: &PathSegment3D, index: usize) -> Result<Self, ModelError> {
        let PathSegment3D::Helix {
            axis_origin_m,
            axis_dir,
            radius_m,
            start_azimuth_deg,
            turns,
            rise_per_turn_m,
        } = *segment;
        for (value, name) in [
            (axis_origin_m[0], "axis_origin_m[0]"),
            (axis_origin_m[1], "axis_origin_m[1]"),
            (axis_origin_m[2], "axis_origin_m[2]"),
            (axis_dir[0], "axis_dir[0]"),
            (axis_dir[1], "axis_dir[1]"),
            (axis_dir[2], "axis_dir[2]"),
            (start_azimuth_deg, "start_azimuth_deg"),
            (turns, "turns"),
            (rise_per_turn_m, "rise_per_turn_m"),
        ] {
            if !value.is_finite() {
                return Err(invalid(&format!(
                    "path3d segment {index}: {name} must be finite"
                )));
            }
        }
        let a_norm = norm(axis_dir);
        if a_norm <= 0.0 {
            return Err(invalid(&format!(
                "path3d segment {index}: axis_dir must be nonzero"
            )));
        }
        if radius_m <= 0.0 {
            return Err(invalid(&format!(
                "path3d segment {index}: radius_m must be positive"
            )));
        }
        if turns == 0.0 {
            return Err(invalid(&format!(
                "path3d segment {index}: turns must be nonzero"
            )));
        }
        let a = scale(axis_dir, 1.0 / a_norm);
        // Deterministic orthonormal (e1, e2, a): project the lab axis
        // least aligned with a onto the plane ⊥ a, so the azimuth zero
        // sits on a canonical lab direction — for a = ẑ this is exactly
        // the standard cylindrical convention (e1 = x̂, e2 = ŷ, φ from
        // +x̂ about +ẑ), and every other axis orientation gets the same
        // rule rather than an arbitrary phase.
        let seed = if a[0].abs() <= a[1].abs() && a[0].abs() <= a[2].abs() {
            [1.0, 0.0, 0.0]
        } else if a[1].abs() <= a[2].abs() {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, 0.0, 1.0]
        };
        let e1 = {
            let d = dot(seed, a);
            let v = sub(seed, scale(a, d));
            scale(v, 1.0 / norm(v))
        };
        let e2 = cross(a, e1);
        let rise_per_rad_m = rise_per_turn_m / (2.0 * std::f64::consts::PI);
        Ok(Self {
            origin_m: axis_origin_m,
            e1,
            e2,
            a,
            radius_m,
            rise_per_rad_m,
            lambda: radius_m.hypot(rise_per_rad_m),
            start_azimuth_rad: start_azimuth_deg.to_radians(),
            sweep_rad: turns * 2.0 * std::f64::consts::PI,
        })
    }

    fn length_m(&self) -> f64 {
        self.sweep_rad.abs() * self.lambda
    }

    /// The pose at signed azimuth offset `theta` (rad) from the segment
    /// start — `theta` runs over `[0, sweep_rad]` for positive sweeps
    /// (`[sweep_rad, 0]` for negative).
    fn pose_at_theta(&self, theta: f64) -> PathPose {
        let phi = self.start_azimuth_rad + theta;
        let (sin, cos) = phi.sin_cos();
        // r̂(φ) — cylinder-radial outward; φ̂(φ) — azimuthal.
        let r_hat = add(scale(self.e1, cos), scale(self.e2, sin));
        let phi_hat = add(scale(self.e1, -sin), scale(self.e2, cos));
        let position = add(
            self.origin_m,
            add(
                scale(r_hat, self.radius_m),
                scale(self.a, self.rise_per_rad_m * theta),
            ),
        );
        // t̂ = dc/ds = sign(sweep)·(R·φ̂ + c·â)/λ — for a reversed sweep
        // the current runs the helix the other way.
        let sense = self.sweep_rad.signum();
        let tangent = scale(
            add(
                scale(phi_hat, self.radius_m),
                scale(self.a, self.rise_per_rad_m),
            ),
            sense / self.lambda,
        );
        PathPose {
            position_m: position,
            tangent,
            // Outward cylinder-radial — the helix's curvature center is
            // on the axis side, so this is the same "away from center"
            // convention the planar path's normal carries.
            normal: r_hat,
            // |κ| = R/λ² — signed by the sweep sense, matching the
            // planar signed-curvature convention.
            curvature_inv_m: sense * self.radius_m / (self.lambda * self.lambda),
        }
    }

    /// The pose at arc length `s` ∈ `[0, length]` from the segment start.
    fn pose_at_length(&self, s_m: f64) -> PathPose {
        self.pose_at_theta(s_m / self.lambda * self.sweep_rad.signum())
    }

    /// The corner curve `c(θ) + u·r̂(θ) + v·ŵ(θ)` at signed azimuth
    /// `theta`, where `ŵ = cosα·â − sinα·φ̂` is the in-surface binormal
    /// (`cosα = R/λ`, `sinα = c/λ`).
    fn corner_at_theta(&self, theta: f64, u_m: f64, v_m: f64) -> [f64; 3] {
        let phi = self.start_azimuth_rad + theta;
        let (sin, cos) = phi.sin_cos();
        let r_hat = add(scale(self.e1, cos), scale(self.e2, sin));
        let phi_hat = add(scale(self.e1, -sin), scale(self.e2, cos));
        let cos_a = self.radius_m / self.lambda;
        let sin_a = self.rise_per_rad_m / self.lambda;
        add(
            self.origin_m,
            add(
                add(
                    scale(r_hat, self.radius_m + u_m),
                    scale(self.a, self.rise_per_rad_m * theta + v_m * cos_a),
                ),
                scale(phi_hat, -v_m * sin_a),
            ),
        )
    }

    /// Axis-extremum candidates for the swept band's bounding box —
    /// the azimuths where a corner curve's lab-axis component
    /// stationarizes. The band's AABB is attained on the four corner
    /// curves; their extrema solve `H_k·cos(φ − ψ_k) = −c·a_k` per axis
    /// `k`, exactly the planar cardinal-angle machinery lifted to a
    /// rotating (r̂, φ̂) basis.
    fn domain_test_points(&self, half_width_m: f64, half_height_m: f64) -> Vec<[f64; 3]> {
        let sin_a = self.rise_per_rad_m / self.lambda;
        let lo = self.sweep_rad.min(0.0);
        let hi = self.sweep_rad.max(0.0);
        let mut points = Vec::new();
        for &u in &[-half_width_m, half_width_m] {
            for &v in &[-half_height_m, half_height_m] {
                let mut thetas = vec![lo, hi];
                for axis in 0..3 {
                    // f'_k = (R+u)·φ̂_k + v·sinα·r̂_k + c·a_k
                    //      = P_k·cosφ + Q_k·sinφ + c·a_k
                    let p_k = (self.radius_m + u) * self.e2[axis] + v * sin_a * self.e1[axis];
                    let q_k = -(self.radius_m + u) * self.e1[axis] + v * sin_a * self.e2[axis];
                    let h_k = p_k.hypot(q_k);
                    let rhs = -self.rise_per_rad_m * self.a[axis];
                    if h_k <= 0.0 || rhs.abs() > h_k {
                        continue;
                    }
                    let psi = q_k.atan2(p_k);
                    let delta = (rhs / h_k).acos();
                    for sign in [-1.0_f64, 1.0] {
                        // φ = ψ ± δ + 2πj inside [lo, hi].
                        let base = psi + sign * delta - self.start_azimuth_rad;
                        let j_lo = ((lo - base) / (2.0 * std::f64::consts::PI)).ceil() as i64;
                        let j_hi = ((hi - base) / (2.0 * std::f64::consts::PI)).floor() as i64;
                        for j in j_lo..=j_hi {
                            thetas.push(base + 2.0 * std::f64::consts::PI * j as f64);
                        }
                    }
                }
                for theta in thetas {
                    points.push(self.corner_at_theta(theta, u, v));
                }
            }
        }
        points
    }
}

/// A non-planar winding centerline: ordered helix segments joined
/// position- and tangent-continuously. Open by default — a helical
/// layer's ends are its leads — with `closed` available for the rare
/// loop that does return to its start pose.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoilPath3D {
    pub segments: Vec<PathSegment3D>,
    /// `true` requires the traced end pose to return to the start pose
    /// (position and tangent), the planar closure rule. `false` — the
    /// norm for helical conductors — declares an open path whose ends
    /// are the conductor's leads.
    #[serde(default)]
    pub closed: bool,
}

impl CoilPath3D {
    fn resolved(&self) -> Result<Vec<ResolvedHelix>, ModelError> {
        self.validate()?;
        self.segments
            .iter()
            .enumerate()
            .map(|(i, s)| ResolvedHelix::resolve(s, i))
            .collect::<Result<Vec<_>, _>>()
    }

    /// Total centerline length (m).
    pub fn length_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    turns,
                    rise_per_turn_m,
                    ..
                } = *s;
                (turns * 2.0 * std::f64::consts::PI).abs()
                    * radius_m.hypot(rise_per_turn_m / (2.0 * std::f64::consts::PI))
            })
            .sum()
    }

    /// The pose at arc length `s_m` (`0 <= s <= length`). The closing
    /// pose of a `closed` path equals its start pose by validation.
    pub fn pose_at(&self, s_m: f64) -> Result<PathPose, ModelError> {
        let segments = self.resolved()?;
        let length = self.length_m();
        if !s_m.is_finite() || s_m < 0.0 || s_m > length {
            return Err(invalid("path3d arc length s_m out of range"));
        }
        let mut remaining = s_m;
        for helix in &segments {
            let seg_len = helix.length_m();
            if remaining > seg_len {
                remaining -= seg_len;
                continue;
            }
            return Ok(helix.pose_at_length(remaining));
        }
        // s == total length: the closing pose.
        let last = segments[segments.len() - 1];
        Ok(last.pose_at_theta(last.sweep_rad))
    }

    /// Centerline length of the conductor at cylinder-radial offset
    /// `delta_m` — the CORC/CCT stacking direction: the offset turn is
    /// a helix of radius `R + delta` and unchanged pitch.
    pub fn length_at_offset_m(&self, delta_m: f64) -> f64 {
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    turns,
                    rise_per_turn_m,
                    ..
                } = *s;
                (turns * 2.0 * std::f64::consts::PI).abs()
                    * (radius_m + delta_m).hypot(rise_per_turn_m / (2.0 * std::f64::consts::PI))
            })
            .sum()
    }

    /// Smallest effective bend radius `λ²/R` over all segments — the
    /// helix curvature radius the bend bound applies to. Always finite.
    pub fn min_radius_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    rise_per_turn_m,
                    ..
                } = *s;
                let c = rise_per_turn_m / (2.0 * std::f64::consts::PI);
                (radius_m * radius_m + c * c) / radius_m
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Largest effective bend radius over all segments — the
    /// conservative characteristic turn radius for the hoop bound.
    pub fn max_radius_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    rise_per_turn_m,
                    ..
                } = *s;
                let c = rise_per_turn_m / (2.0 * std::f64::consts::PI);
                (radius_m * radius_m + c * c) / radius_m
            })
            .fold(0.0, f64::max)
    }

    /// Smallest inner-face curvature radius for a pack of
    /// `radial_width_m`: the inner edge at cylinder `R − w/2` is itself
    /// a helix of the same pitch, so its curvature radius is
    /// `((R−w/2)² + c²)/(R−w/2)`, not `R_eff − w/2`. Negative when an
    /// inner edge crosses the axis — the pack-degeneracy screen fails
    /// on it, same convention as the planar inner radius.
    pub fn min_inner_radius_m(&self, radial_width_m: f64) -> f64 {
        let half = radial_width_m / 2.0;
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    rise_per_turn_m,
                    ..
                } = *s;
                let inner = radius_m - half;
                let c = rise_per_turn_m / (2.0 * std::f64::consts::PI);
                (inner * inner + c * c) / inner
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Largest outer-face curvature radius for a pack of
    /// `radial_width_m` — the hoop bound's characteristic radius.
    pub fn max_outer_radius_m(&self, radial_width_m: f64) -> f64 {
        let half = radial_width_m / 2.0;
        self.segments
            .iter()
            .map(|s| {
                let PathSegment3D::Helix {
                    radius_m,
                    rise_per_turn_m,
                    ..
                } = *s;
                let outer = radius_m + half;
                let c = rise_per_turn_m / (2.0 * std::f64::consts::PI);
                (outer * outer + c * c) / outer
            })
            .fold(0.0, f64::max)
    }

    /// Axis-extremum candidate points of the swept pack domain — the
    /// same contract `pack_domain_test_points` fulfils for planar
    /// paths: the domain's axis-aligned bounding box is attained on
    /// these points, so checking their containment in a Cartesian map
    /// hull confines the whole band.
    pub fn domain_test_points(
        &self,
        half_width_m: f64,
        half_height_m: f64,
    ) -> Result<Vec<[f64; 3]>, ModelError> {
        let segments = self.resolved()?;
        let mut points = Vec::new();
        for helix in &segments {
            points.extend(helix.domain_test_points(half_width_m, half_height_m));
        }
        Ok(points)
    }

    /// Structural validation: every segment well-formed, consecutive
    /// segments joining position- and tangent-continuously, and — for
    /// `closed` paths — the end pose returning to the start pose.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.segments.is_empty() {
            return Err(invalid("path3d requires at least one segment"));
        }
        let segments: Vec<ResolvedHelix> = self
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| ResolvedHelix::resolve(s, i))
            .collect::<Result<Vec<_>, _>>()?;
        let scale = self.length_m().max(1.0);
        for w in segments.windows(2) {
            let end = w[0].pose_at_theta(w[0].sweep_rad);
            let start = w[1].pose_at_theta(0.0);
            let pos_err = norm(sub(end.position_m, start.position_m));
            let tan_err = norm(sub(end.tangent, start.tangent));
            if pos_err > 1e-9 * scale {
                return Err(invalid(&format!(
                    "path3d segments do not join: position gap {pos_err:.3e} m"
                )));
            }
            if tan_err > 1e-9 {
                return Err(invalid(
                    "path3d segments join with a tangent break — a hard bend is \
                     not modeled (declare continuous segments)",
                ));
            }
        }
        if self.closed {
            let start = segments[0].pose_at_theta(0.0);
            let end =
                segments[segments.len() - 1].pose_at_theta(segments[segments.len() - 1].sweep_rad);
            let pos_err = norm(sub(end.position_m, start.position_m));
            let tan_err = norm(sub(end.tangent, start.tangent));
            if pos_err > 1e-9 * scale {
                return Err(invalid(&format!(
                    "path3d declared closed but its end is {pos_err:.3e} m from the start"
                )));
            }
            if tan_err > 1e-9 {
                return Err(invalid(
                    "path3d declared closed but its tangent does not close",
                ));
            }
        }
        Ok(())
    }

    /// Pack-aware validation: every segment's inner pack face must stay
    /// off the winding axis — `radius_m − radial_width/2 > 0` — the
    /// same rule `CoilPath::validate_against_width` applies to arc
    /// centers.
    pub fn validate_against_width(&self, radial_width_m: f64) -> Result<(), ModelError> {
        self.validate()?;
        for (i, s) in self.segments.iter().enumerate() {
            let PathSegment3D::Helix { radius_m, .. } = *s;
            if radius_m - radial_width_m / 2.0 < 1e-9 {
                return Err(invalid(&format!(
                    "path3d segment {i}: helix radius {radius_m} cannot carry a pack \
                     of radial width {radial_width_m} — the inner face reaches the \
                     winding axis"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One right-handed helix about +z through the origin: R = 0.05 m,
    /// 4 turns, 0.02 m rise per turn, start on +x.
    fn z_helix() -> CoilPath3D {
        CoilPath3D {
            segments: vec![PathSegment3D::Helix {
                axis_origin_m: [0.0, 0.0, 0.0],
                axis_dir: [0.0, 0.0, 1.0],
                radius_m: 0.05,
                start_azimuth_deg: 0.0,
                turns: 4.0,
                rise_per_turn_m: 0.02,
            }],
            closed: false,
        }
    }

    fn helix_segment(
        radius_m: f64,
        start_azimuth_deg: f64,
        turns: f64,
        rise_per_turn_m: f64,
    ) -> PathSegment3D {
        PathSegment3D::Helix {
            axis_origin_m: [0.0, 0.0, 0.0],
            axis_dir: [0.0, 0.0, 1.0],
            radius_m,
            start_azimuth_deg,
            turns,
            rise_per_turn_m,
        }
    }

    #[test]
    fn helix_length_and_offset_length() {
        let path = z_helix();
        let c = 0.02 / (2.0 * std::f64::consts::PI);
        let per_turn = (0.05_f64).hypot(c) * 2.0 * std::f64::consts::PI;
        assert!((path.length_m() - 4.0 * per_turn).abs() < 1e-12);
        // Cylinder-radial offset: same pitch, radius R + delta.
        let offset_len = (0.06_f64).hypot(c) * 2.0 * std::f64::consts::PI;
        assert!((path.length_at_offset_m(0.01) - 4.0 * offset_len).abs() < 1e-12);
    }

    #[test]
    fn helix_pose_at_start_and_end() {
        let path = z_helix();
        let start = path.pose_at(0.0).unwrap();
        assert!((start.position_m[0] - 0.05).abs() < 1e-12);
        assert!(start.position_m[1].abs() < 1e-12);
        assert!(start.position_m[2].abs() < 1e-12);
        // n_ref is the outward cylinder radial.
        assert!((start.normal[0] - 1.0).abs() < 1e-12);
        // t̂ is unit and leans slightly +z (the pitch).
        let t_norm = norm(start.tangent);
        assert!((t_norm - 1.0).abs() < 1e-12);
        assert!(start.tangent[2] > 0.0);
        let end = path.pose_at(path.length_m()).unwrap();
        assert!((end.position_m[0] - 0.05).abs() < 1e-9);
        assert!((end.position_m[2] - 4.0 * 0.02).abs() < 1e-9);
        // A quarter-turn in: position sits at azimuth 90 deg on +y.
        let quarter = path.pose_at(path.length_m() / 16.0).unwrap();
        assert!(quarter.position_m[0].abs() < 1e-9);
        assert!((quarter.position_m[1] - 0.05).abs() < 1e-9);
    }

    #[test]
    fn helix_in_surface_binormal_is_n_cross_t() {
        // The width direction w = n × t tilts out of the xy plane —
        // the property that makes this genuinely non-planar.
        let path = z_helix();
        let pose = path.pose_at(0.0).unwrap();
        let w = cross(pose.normal, pose.tangent);
        assert!(w[2].abs() > 0.05, "binormal must leave the plane");
        // w is unit and orthogonal to both.
        assert!((norm(w) - 1.0).abs() < 1e-12);
        assert!(dot(w, pose.tangent).abs() < 1e-12);
        assert!(dot(w, pose.normal).abs() < 1e-12);
    }

    #[test]
    fn helix_curvature_radius_matches_lambda_squared_over_r() {
        let path = z_helix();
        let c = 0.02 / (2.0 * std::f64::consts::PI);
        let expected = (0.05 * 0.05 + c * c) / 0.05;
        assert!((path.min_radius_m() - expected).abs() < 1e-12);
        assert!((path.max_radius_m() - expected).abs() < 1e-12);
        // Inner-edge curvature: ((R−w/2)²+c²)/(R−w/2), not R_eff−w/2.
        let inner = 0.05 - 0.005;
        let expected_inner = (inner * inner + c * c) / inner;
        assert!((path.min_inner_radius_m(0.01) - expected_inner).abs() < 1e-12);
    }

    #[test]
    fn validation_rejects_malformed_segments() {
        // Empty path.
        let empty = CoilPath3D {
            segments: vec![],
            closed: false,
        };
        assert!(empty.validate().is_err());
        // Zero axis direction.
        let mut bad = z_helix();
        let PathSegment3D::Helix { axis_dir, .. } = &mut bad.segments[0];
        *axis_dir = [0.0, 0.0, 0.0];
        assert!(bad.validate().is_err());
        // Nonpositive radius.
        let mut bad = z_helix();
        let PathSegment3D::Helix { radius_m, .. } = &mut bad.segments[0];
        *radius_m = -0.05;
        assert!(bad.validate().is_err());
        // Zero turns.
        let mut bad = z_helix();
        let PathSegment3D::Helix { turns, .. } = &mut bad.segments[0];
        *turns = 0.0;
        assert!(bad.validate().is_err());
        // Nonfinite value.
        let mut bad = z_helix();
        let PathSegment3D::Helix {
            rise_per_turn_m, ..
        } = &mut bad.segments[0];
        *rise_per_turn_m = f64::NAN;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn validation_checks_segment_continuity() {
        // Two quarter-turns joined: the second starts where the first
        // ends — same cylinder, azimuth advanced 90 deg, z advanced by
        // one quarter's rise.
        let mut continuous = CoilPath3D {
            segments: vec![
                helix_segment(0.05, 0.0, 0.25, 0.02),
                PathSegment3D::Helix {
                    axis_origin_m: [0.0, 0.0, 0.25 * 0.02],
                    axis_dir: [0.0, 0.0, 1.0],
                    radius_m: 0.05,
                    start_azimuth_deg: 90.0,
                    turns: 0.25,
                    rise_per_turn_m: 0.02,
                },
            ],
            closed: false,
        };
        assert!(continuous.validate().is_ok());
        // Break the join: wrong start azimuth.
        let PathSegment3D::Helix {
            start_azimuth_deg, ..
        } = &mut continuous.segments[1];
        *start_azimuth_deg = 45.0;
        assert!(continuous.validate().is_err());
    }

    #[test]
    fn closure_rule_only_when_declared() {
        // An open helix is valid as declared.
        assert!(z_helix().validate().is_ok());
        // Marked closed, it fails — the end is 0.08 m above the start.
        let mut closed = z_helix();
        closed.closed = true;
        assert!(closed.validate().is_err());
        // Integer turns with zero rise do close.
        let looped = CoilPath3D {
            segments: vec![helix_segment(0.05, 0.0, 2.0, 0.0)],
            closed: true,
        };
        assert!(looped.validate().is_ok());
    }

    #[test]
    fn validate_against_width_guards_the_axis() {
        // Pack half-width reaching the helix axis is degenerate.
        assert!(z_helix().validate_against_width(0.01).is_ok());
        assert!(z_helix().validate_against_width(0.1).is_err());
    }

    #[test]
    fn domain_points_cover_the_swept_band() {
        // The band's AABB must contain every corner-curve test point —
        // trivially true — and the map-hull check consumes exactly this
        // set, so pin the z extent: [−h·cosα, turns·rise + h·cosα].
        let path = z_helix();
        let points = path.domain_test_points(0.005, 0.01).unwrap();
        assert!(!points.is_empty());
        let z_lo = points.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
        let z_hi = points
            .iter()
            .map(|p| p[2])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(z_lo < 0.0, "band extends below the helix start");
        assert!(z_hi > 0.08, "band extends above the helix end");
        // The binormal's φ̂ component tips corner points off pure r̂:
        // the outer in-plane radius is hypot(R+w/2, h·sinα).
        let sin_a = (0.02 / (2.0 * std::f64::consts::PI))
            / 0.05f64.hypot(0.02 / (2.0 * std::f64::consts::PI));
        let r_max = points
            .iter()
            .map(|p| p[0].hypot(p[1]))
            .fold(0.0_f64, f64::max);
        assert!((r_max - (0.055_f64).hypot(0.01 * sin_a)).abs() < 1e-9);
    }

    #[test]
    fn reversed_sweep_keeps_valid_geometry() {
        // Negative turns wind the other hand — length and poses stay sane.
        let path = CoilPath3D {
            segments: vec![helix_segment(0.05, 0.0, -1.0, 0.02)],
            closed: false,
        };
        path.validate().unwrap();
        let c = 0.02 / (2.0 * std::f64::consts::PI);
        let expected = (0.05_f64).hypot(c) * 2.0 * std::f64::consts::PI;
        assert!((path.length_m() - expected).abs() < 1e-12);
        let end = path.pose_at(path.length_m()).unwrap();
        // One reversed turn ends back at azimuth 0, risen −0.02... the
        // rise is per turn along +z regardless of winding sense.
        assert!((end.position_m[0] - 0.05).abs() < 1e-9);
    }
}
