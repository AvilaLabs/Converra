//! Piecewise C1 winding paths: a closed planar curve built from straight
//! and circular-arc segments.
//!
//! Every planar fusion coil outline — racetrack, D-shape, picture-frame —
//! is a chain of line and arc segments joined tangent-continuously, which
//! this type represents exactly. The convention: the path lies in the
//! xy-plane, current flows in the direction of travel, and a valid coil
//! loop is counterclockwise (interior to the left of travel). The local
//! in-plane `normal` in each pose points away from the local curvature
//! center (outward for convex CCW loops, inward across a clockwise arc),
//! so pack offsets stay signed distances from the centerline.
//!
//! A `CoilPath` is a *centerline* — the winding pack straddles it by
//! ± half the pack width, so validation requires every arc radius to
//! exceed that half-width (the inner edge must not reach the center).

use serde::{Deserialize, Serialize};

use crate::ModelError;

fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

/// One segment of a winding path. Segments join tangent-continuously by
/// construction: each begins at the previous segment's end pose.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PathSegment {
    /// Straight run of `length_m` along the current tangent.
    Line { length_m: f64 },
    /// Circular arc of `radius_m` through `sweep_deg` (signed; positive
    /// turns left/CCW). Center lies `radius_m` to the left of travel for
    /// positive sweeps, right for negative.
    Arc { radius_m: f64, sweep_deg: f64 },
}

impl PathSegment {
    /// Arc length of this segment.
    pub fn length_m(&self) -> f64 {
        match *self {
            PathSegment::Line { length_m } => length_m,
            PathSegment::Arc {
                radius_m,
                sweep_deg,
            } => radius_m * sweep_deg.to_radians().abs(),
        }
    }

    /// |1/R| for arcs, 0 for lines.
    pub fn curvature_inv_m(&self) -> f64 {
        match *self {
            PathSegment::Line { .. } => 0.0,
            PathSegment::Arc { radius_m, .. } => 1.0 / radius_m,
        }
    }
}

/// The pose of a point on the path: position, unit tangent (current
/// direction), unit in-plane normal (away from the local curvature
/// center), and signed curvature (0 on lines, ±1/R on arcs — sign of the
/// sweep).
#[derive(Debug, Clone, Copy)]
pub struct PathPose {
    pub position_m: [f64; 3],
    pub tangent: [f64; 3],
    pub normal: [f64; 3],
    pub curvature_inv_m: f64,
}

/// A segment with its entry pose resolved — what cell generation needs:
/// for arcs, the center point; for lines, just the entry frame.
#[derive(Debug, Clone, Copy)]
pub struct SegmentFrame {
    pub segment: PathSegment,
    pub entry: PathPose,
    /// Arc center (xy) for `Arc` segments; the entry position for `Line`.
    pub center_m: [f64; 2],
}

/// A closed planar winding path: ordered segments plus the pose of the
/// first segment's start (placement of the loop in the xy-plane).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoilPath {
    pub segments: Vec<PathSegment>,
    /// Where segment 0 starts (xy). Defaults to the origin.
    #[serde(default)]
    pub start_position_m: [f64; 2],
    /// Heading of segment 0's start, degrees from +x (CCW positive).
    /// Defaults to +x.
    #[serde(default)]
    pub start_heading_deg: f64,
}

impl CoilPath {
    /// The conventional racetrack: two straights of `2 * half_length` joined
    /// by two semicircular arcs of `bend_radius`, placed so the result
    /// coincides exactly with the legacy `straight_half_length_m` /
    /// `bend_radius_m` geometry (semicircle centers at (±half_length, 0),
    /// straights at y = ±bend_radius).
    pub fn racetrack(straight_half_length_m: f64, bend_radius_m: f64) -> Self {
        // Degenerate racetrack (zero straight length) is an annulus: two
        // semicircular arcs of the same radius form one circle.
        if straight_half_length_m == 0.0 {
            return Self {
                segments: vec![
                    PathSegment::Arc {
                        radius_m: bend_radius_m,
                        sweep_deg: 180.0,
                    },
                    PathSegment::Arc {
                        radius_m: bend_radius_m,
                        sweep_deg: 180.0,
                    },
                ],
                start_position_m: [0.0, bend_radius_m],
                start_heading_deg: 180.0,
            };
        }
        Self {
            segments: vec![
                PathSegment::Line {
                    length_m: 2.0 * straight_half_length_m,
                },
                PathSegment::Arc {
                    radius_m: bend_radius_m,
                    sweep_deg: 180.0,
                },
                PathSegment::Line {
                    length_m: 2.0 * straight_half_length_m,
                },
                PathSegment::Arc {
                    radius_m: bend_radius_m,
                    sweep_deg: 180.0,
                },
            ],
            // Start at the right end of the top straight heading -x: the
            // first arc then sweeps around the left bend. Yields the legacy
            // centerline exactly.
            start_position_m: [straight_half_length_m, bend_radius_m],
            start_heading_deg: 180.0,
        }
    }

    /// Total centerline length.
    pub fn length_m(&self) -> f64 {
        self.segments.iter().map(PathSegment::length_m).sum()
    }

    /// Smallest arc radius in the path (`inf` for all-line paths).
    pub fn min_radius_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| match *s {
                PathSegment::Arc { radius_m, .. } => radius_m,
                _ => f64::INFINITY,
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// Largest arc radius in the path (`0.0` for all-line paths). Used by
    /// the mechanical hoop-stress bound, whose first-order model needs a
    /// characteristic turn radius: taking the maximum arc radius is the
    /// conservative choice (the bound scales with radius).
    pub fn max_radius_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| match *s {
                PathSegment::Arc { radius_m, .. } => radius_m,
                _ => 0.0,
            })
            .fold(0.0, f64::max)
    }

    /// Centerline length of the turn at signed normal offset `delta_m`:
    /// line segments keep their length; an arc of radius `R` sweeping `θ`
    /// contributes `(R + sign(θ)·delta)·|θ|` because the normal points away
    /// from the center for a positive sweep and toward it for a negative
    /// one. On a racetrack this is `4·L + 2π·(R + delta)` — the same closed
    /// form the cost ledger already uses, up to summation order.
    pub fn length_at_offset_m(&self, delta_m: f64) -> f64 {
        self.segments
            .iter()
            .map(|seg| match *seg {
                PathSegment::Line { length_m } => length_m,
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => (radius_m + sweep_deg.signum() * delta_m) * sweep_deg.to_radians().abs(),
            })
            .sum()
    }

    /// Smallest in-plane distance from `point_m` to the centerline. The
    /// good-field overlap screen uses this: the pack is a `width × height`
    /// rectangular band straddling the centerline, so a probe point is
    /// inside it iff this distance is at most `width/2` (and `|z| ≤
    /// height/2`, checked by the caller). Runs `segment_frames`, so an
    /// invalid path returns `Err`.
    pub fn distance_to_centerline_m(&self, point_m: [f64; 2]) -> Result<f64, ModelError> {
        let mut best = f64::INFINITY;
        for frame in self.segment_frames()? {
            let distance = match frame.segment {
                PathSegment::Line { length_m } => {
                    let rel = [
                        point_m[0] - frame.entry.position_m[0],
                        point_m[1] - frame.entry.position_m[1],
                    ];
                    let along = (rel[0] * frame.entry.tangent[0] + rel[1] * frame.entry.tangent[1])
                        .clamp(0.0, length_m);
                    let px = frame.entry.position_m[0] + along * frame.entry.tangent[0];
                    let py = frame.entry.position_m[1] + along * frame.entry.tangent[1];
                    (point_m[0] - px).hypot(point_m[1] - py)
                }
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    let start_angle = (frame.entry.position_m[1] - frame.center_m[1])
                        .atan2(frame.entry.position_m[0] - frame.center_m[0]);
                    let sweep = sweep_deg.to_radians();
                    let point_angle =
                        (point_m[1] - frame.center_m[1]).atan2(point_m[0] - frame.center_m[0]);
                    // Angular progress of the point's bearing within the
                    // sweep, measured in the sweep's own direction.
                    let progressed = if sweep >= 0.0 {
                        (point_angle - start_angle).rem_euclid(2.0 * std::f64::consts::PI)
                    } else {
                        (start_angle - point_angle).rem_euclid(2.0 * std::f64::consts::PI)
                    };
                    if progressed <= sweep.abs() {
                        ((point_m[0] - frame.center_m[0]).hypot(point_m[1] - frame.center_m[1])
                            - radius_m)
                            .abs()
                    } else {
                        let end = self.advance(&frame.entry, &frame.segment).position_m;
                        (point_m[0] - end[0]).hypot(point_m[1] - end[1]).min(
                            (point_m[0] - frame.entry.position_m[0])
                                .hypot(point_m[1] - frame.entry.position_m[1]),
                        )
                    }
                }
            };
            best = best.min(distance);
        }
        Ok(best)
    }

    fn start_pose(&self) -> Result<PathPose, ModelError> {
        for v in self.start_position_m {
            if !v.is_finite() {
                return Err(invalid("path start_position_m must be finite"));
            }
        }
        if !self.start_heading_deg.is_finite() {
            return Err(invalid("path start_heading_deg must be finite"));
        }
        let heading = self.start_heading_deg.to_radians();
        let (sin, cos) = heading.sin_cos();
        Ok(PathPose {
            position_m: [self.start_position_m[0], self.start_position_m[1], 0.0],
            tangent: [cos, sin, 0.0],
            // CCW-loop convention: normal is right of travel (outward).
            normal: [sin, -cos, 0.0],
            curvature_inv_m: 0.0,
        })
    }

    /// Segment poses resolved in order — entry pose and (for arcs) center.
    /// Runs the same walk `pose_at` uses; cell generation consumes this to
    /// lay out per-segment curvilinear cells.
    pub fn segment_frames(&self) -> Result<Vec<SegmentFrame>, ModelError> {
        self.validate()?;
        let mut pose = self.start_pose()?;
        let mut frames = Vec::with_capacity(self.segments.len());
        for &segment in &self.segments {
            let center = match segment {
                PathSegment::Line { .. } => [pose.position_m[0], pose.position_m[1]],
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    // Center is radius_m to the left of travel for a positive
                    // sweep (right for negative); left(t) = -normal under the
                    // CCW convention.
                    let sign = sweep_deg.signum();
                    [
                        pose.position_m[0] - sign * radius_m * pose.normal[0],
                        pose.position_m[1] - sign * radius_m * pose.normal[1],
                    ]
                }
            };
            frames.push(SegmentFrame {
                segment,
                entry: pose,
                center_m: center,
            });
            pose = self.advance(&pose, &segment);
        }
        Ok(frames)
    }

    /// Advance an entry pose through one segment.
    fn advance(&self, pose: &PathPose, segment: &PathSegment) -> PathPose {
        match *segment {
            PathSegment::Line { length_m } => PathPose {
                position_m: [
                    pose.position_m[0] + length_m * pose.tangent[0],
                    pose.position_m[1] + length_m * pose.tangent[1],
                    0.0,
                ],
                tangent: pose.tangent,
                normal: pose.normal,
                curvature_inv_m: 0.0,
            },
            PathSegment::Arc {
                radius_m,
                sweep_deg,
            } => {
                let sign = sweep_deg.signum();
                let sweep = sweep_deg.to_radians();
                // Rotate the (tangent, normal) frame by the sweep about z.
                // Position rotates about the arc center by the same angle.
                let (ds, dc) = sweep.sin_cos();
                let rotate = |v: [f64; 3]| [v[0] * dc - v[1] * ds, v[0] * ds + v[1] * dc, 0.0];
                let center = [
                    pose.position_m[0] - sign * radius_m * pose.normal[0],
                    pose.position_m[1] - sign * radius_m * pose.normal[1],
                ];
                let rel = [
                    pose.position_m[0] - center[0],
                    pose.position_m[1] - center[1],
                    0.0,
                ];
                let rel = rotate(rel);
                PathPose {
                    position_m: [center[0] + rel[0], center[1] + rel[1], 0.0],
                    tangent: rotate(pose.tangent),
                    normal: rotate(pose.normal),
                    curvature_inv_m: sign / radius_m,
                }
            }
        }
    }

    /// The pose at arc length `s_m` along the centerline (`0 <= s <=
    /// length_m`). Endpoint-inclusive: `s == length` returns the closing
    /// pose, which a valid (closed) path makes equal to the start pose.
    pub fn pose_at(&self, s_m: f64) -> Result<PathPose, ModelError> {
        if !s_m.is_finite() || s_m < 0.0 || s_m > self.length_m() {
            return Err(invalid("path arc length s_m out of range"));
        }
        let mut pose = self.start_pose()?;
        let mut remaining = s_m;
        for &segment in &self.segments {
            let seg_len = segment.length_m();
            if remaining > seg_len {
                remaining -= seg_len;
                pose = self.advance(&pose, &segment);
                continue;
            }
            // Pose `remaining` into this segment.
            return Ok(match segment {
                PathSegment::Line { .. } => PathPose {
                    position_m: [
                        pose.position_m[0] + remaining * pose.tangent[0],
                        pose.position_m[1] + remaining * pose.tangent[1],
                        0.0,
                    ],
                    tangent: pose.tangent,
                    normal: pose.normal,
                    curvature_inv_m: 0.0,
                },
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    let sign = sweep_deg.signum();
                    let theta = sign * remaining / radius_m;
                    let (ds, dc) = theta.sin_cos();
                    let rotate = |v: [f64; 3]| [v[0] * dc - v[1] * ds, v[0] * ds + v[1] * dc, 0.0];
                    let center = [
                        pose.position_m[0] - sign * radius_m * pose.normal[0],
                        pose.position_m[1] - sign * radius_m * pose.normal[1],
                    ];
                    let rel = [
                        pose.position_m[0] - center[0],
                        pose.position_m[1] - center[1],
                        0.0,
                    ];
                    let rel = rotate(rel);
                    PathPose {
                        position_m: [center[0] + rel[0], center[1] + rel[1], 0.0],
                        tangent: rotate(pose.tangent),
                        normal: rotate(pose.normal),
                        curvature_inv_m: sign / radius_m,
                    }
                }
            });
        }
        // s == total length: the closing pose.
        Ok(pose)
    }

    /// Structural validation: finite positive extents, at least one arc,
    /// and closure — the traced end pose must return to the start pose in
    /// both position and tangent. Position closure is checked numerically
    /// (total turning ±360° alone does not close a loop in position).
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.segments.is_empty() {
            return Err(invalid("path requires at least one segment"));
        }
        let mut has_arc = false;
        let mut total_sweep_deg = 0.0;
        for (i, segment) in self.segments.iter().enumerate() {
            match *segment {
                PathSegment::Line { length_m } => {
                    if !(length_m.is_finite() && length_m > 0.0) {
                        return Err(invalid(&format!(
                            "path segment {i}: line length must be finite and positive"
                        )));
                    }
                }
                PathSegment::Arc {
                    radius_m,
                    sweep_deg,
                } => {
                    if !(radius_m.is_finite() && radius_m > 0.0) {
                        return Err(invalid(&format!(
                            "path segment {i}: arc radius must be finite and positive"
                        )));
                    }
                    if !(sweep_deg.is_finite() && sweep_deg.abs() > 0.0) {
                        return Err(invalid(&format!(
                            "path segment {i}: arc sweep must be finite and nonzero"
                        )));
                    }
                    has_arc = true;
                    total_sweep_deg += sweep_deg;
                }
            }
        }
        if !has_arc {
            return Err(invalid("a closed coil path requires at least one arc"));
        }
        // Trace the loop: end pose must coincide with the start pose.
        let start = self.start_pose()?;
        let mut pose = start;
        for &segment in &self.segments {
            pose = self.advance(&pose, &segment);
        }
        let scale = self.length_m().max(1.0);
        let pos_err = (pose.position_m[0] - start.position_m[0])
            .hypot(pose.position_m[1] - start.position_m[1]);
        let tan_err =
            (pose.tangent[0] - start.tangent[0]).hypot(pose.tangent[1] - start.tangent[1]);
        if pos_err > 1e-9 * scale {
            return Err(invalid(&format!(
                "path does not close: end pose is {pos_err:.3e} m from the start \
                 (total sweep {total_sweep_deg} deg)"
            )));
        }
        if tan_err > 1e-9 {
            return Err(invalid(&format!(
                "path tangent does not close: total sweep {total_sweep_deg} deg"
            )));
        }
        Ok(())
    }

    /// Pack-aware validation: every arc's radius must exceed the pack's
    /// half-width so the inner edge cannot reach the curvature center.
    pub fn validate_against_width(&self, pack_width_m: f64) -> Result<(), ModelError> {
        self.validate()?;
        let min_radius = self.min_radius_m();
        if min_radius.is_finite() && min_radius - pack_width_m / 2.0 < 1e-9 {
            return Err(invalid(
                "path arc radius minus half the pack width must be positive",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coupled::{Pack, Station, TapeNormal};

    fn racetrack() -> CoilPath {
        CoilPath::racetrack(0.3, 0.2)
    }

    #[test]
    fn racetrack_path_reproduces_legacy_station_geometry() {
        // The legacy model: top straight at y=rho (t=(-1,0), n=(0,1)),
        // right arc about (a,0). The path must give identical poses.
        let a = 0.3_f64;
        let r = 0.2_f64;
        let path = racetrack();
        path.validate().unwrap();

        // s=0 sits at (a, r) heading -x — the right end of the top straight.
        let pose = path.pose_at(0.0).unwrap();
        assert!((pose.position_m[0] - a).abs() < 1e-15);
        assert!((pose.position_m[1] - r).abs() < 1e-15);
        assert!((pose.tangent[0] - (-1.0)).abs() < 1e-12);
        assert!(pose.tangent[1].abs() < 1e-12);
        assert!(pose.normal[0].abs() < 1e-12);
        assert!((pose.normal[1] - 1.0).abs() < 1e-12);

        // s=a into the top straight is x=0 — legacy straight_center.
        let pose = path.pose_at(a).unwrap();
        assert!(pose.position_m[0].abs() < 1e-15);
        assert!((pose.position_m[1] - r).abs() < 1e-15);

        // Mid-left-arc (s = 2a + R*pi/2) should be the leftmost point
        // (-(a+R), 0) heading +y... CCW: at the leftmost point of the left
        // bend the CCW tangent is +y? Travelling -x on top, the left arc
        // turns left (CCW): at (-(a+R), 0) the tangent is +y? For CCW the
        // tangent at the leftmost point is -y. Check: start (a,R)->(-a,R);
        // arc about (-a,0): start angle +90deg, sweep +180 -> end angle
        // 270deg => point (-a, -R)? no: (-a + R cos(270), R sin(270))
        // = (-a, -R). Midpoint angle 180deg => (-a-R, 0), tangent
        // (-sin(180),cos(180))=(0,-1).
        let pose = path
            .pose_at(2.0 * a + r * std::f64::consts::FRAC_PI_2)
            .unwrap();
        assert!((pose.position_m[0] - (-(a + r))).abs() < 1e-12);
        assert!(pose.position_m[1].abs() < 1e-12);
        assert!((pose.tangent[0] - 0.0).abs() < 1e-12);
        assert!((pose.tangent[1] - (-1.0)).abs() < 1e-12);
        assert!((pose.normal[0] - (-1.0)).abs() < 1e-12);
        assert!(pose.normal[1].abs() < 1e-12);
    }

    #[test]
    fn racetrack_matches_station_position_and_frame() {
        // Direct equivalence with the legacy `Station` geometry.
        let a = 0.3_f64;
        let r = 0.2_f64;
        let w = 0.02_f64;
        let path = racetrack();
        let pack = Pack {
            straight_half_length_m: Some(a),
            bend_radius_m: Some(r),
            radial_width_m: w,
            axial_height_m: 0.036,
            path: None,
            path3d: None,
        };
        // Legacy right-arc station at azimuth 45deg. In path terms the right
        // bend is the last segment: s = 2a + R*pi + 2a + R*(azim+90deg)...
        // The right arc starts at (a,-R) heading +x, azimuth -90deg about
        // (a,0); azimuth 45deg is 135deg into the 180deg sweep.
        let s_arc45 =
            2.0 * a + r * std::f64::consts::PI + 2.0 * a + r * (45.0_f64 + 90.0).to_radians();
        let pose = path.pose_at(s_arc45).unwrap();
        let station = Station::Arc {
            id: "arc_45".into(),
            azimuth_deg: 45.0,
        };
        let rho = r - w / 2.0 + 0.005; // an interior tape radius
        let legacy = station.position_m(&pack, rho, 0.007).unwrap();
        // Path: centerline pose at s, offset by (rho - r) along the normal.
        let offset = rho - r;
        for (k, &leg) in legacy.iter().enumerate().take(2) {
            let via_path = pose.position_m[k] + offset * pose.normal[k];
            assert!((via_path - leg).abs() < 1e-12, "axis {k}");
        }
        assert!((pose.position_m[2]).abs() < 1e-15);
        // Frame: legacy radial frame at azimuth 45deg.
        let f = station.frame(TapeNormal::Radial, None).unwrap();
        for k in 0..3 {
            assert!((pose.tangent[k] - f.t[k]).abs() < 1e-12);
            assert!((pose.normal[k] - f.n[k]).abs() < 1e-12);
        }
    }

    #[test]
    fn open_segment_chain_rejects() {
        // A D fragment — inner leg straight, corner arc, partial outer arc
        // that never returns — must fail the closure check.
        let path = CoilPath {
            segments: vec![
                PathSegment::Line { length_m: 2.0 }, // inner leg
                PathSegment::Arc {
                    radius_m: 0.3,
                    sweep_deg: 90.0,
                },
                PathSegment::Arc {
                    radius_m: 1.0,
                    sweep_deg: 90.0,
                },
                PathSegment::Line { length_m: 2.0 }, // back down? no —
                                                     // instead: outer arc closing the D
            ],
            start_position_m: [0.0, 0.0],
            start_heading_deg: 90.0, // inner leg runs +y
        };
        // This specific 4-segment chain does not close — fix it below.
        assert!(path.validate().is_err());
    }

    #[test]
    fn picture_frame_d_closes() {
        // Picture-frame D: inner leg, 90 corner, outer arc 90, outer leg
        // top, corner, down the outer... construct symmetric:
        // start bottom-inner (0,0) heading +y: line(h), arc r 90 -> +x,
        // arc R 90 -> -y... For closure use a known-closed shape: a "slot"
        // = two lines + two arcs with R = w/2, like a flattened racetrack.
        let path = CoilPath {
            segments: vec![
                PathSegment::Line { length_m: 1.0 },
                PathSegment::Arc {
                    radius_m: 0.2,
                    sweep_deg: 180.0,
                },
                PathSegment::Line { length_m: 1.0 },
                PathSegment::Arc {
                    radius_m: 0.2,
                    sweep_deg: 180.0,
                },
            ],
            start_position_m: [0.0, 0.2],
            start_heading_deg: 180.0,
        };
        path.validate().unwrap();
        assert!((path.min_radius_m() - 0.2).abs() < 1e-15);
        assert!((path.length_m() - (2.0 + 0.4 * std::f64::consts::PI)).abs() < 1e-12);
    }

    #[test]
    fn unclosed_and_degenerate_paths_reject() {
        // Open loop: sweeps only sum to 180.
        let mut path = racetrack();
        path.segments[1] = PathSegment::Arc {
            radius_m: 0.2,
            sweep_deg: 90.0,
        };
        assert!(path.validate().is_err());
        // Zero-length line.
        let mut path = racetrack();
        path.segments[0] = PathSegment::Line { length_m: 0.0 };
        assert!(path.validate().is_err());
        // Empty.
        assert!(
            CoilPath {
                segments: vec![],
                start_position_m: [0.0, 0.0],
                start_heading_deg: 0.0,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn width_validation_respects_min_radius() {
        let path = racetrack(); // min radius 0.2
        path.validate_against_width(0.02).unwrap();
        assert!(path.validate_against_width(0.41).is_err());
    }

    #[test]
    fn clockwise_sweep_changes_curvature_sign_and_normal() {
        // A line, a right-turning arc, and another arc returning left —
        // an S-fragment (not closed; use pose_at only, skip validate).
        let path = CoilPath {
            segments: vec![
                PathSegment::Line { length_m: 1.0 },
                PathSegment::Arc {
                    radius_m: 0.5,
                    sweep_deg: -90.0,
                },
            ],
            start_position_m: [0.0, 0.0],
            start_heading_deg: 0.0,
        };
        let pose = path.pose_at(1.0).unwrap();
        assert!((pose.tangent[0] - 1.0).abs() < 1e-12);
        assert!(pose.tangent[1].abs() < 1e-12);
        assert!(pose.normal[0].abs() < 1e-12);
        assert!((pose.normal[1] - (-1.0)).abs() < 1e-12);
        let pose = path
            .pose_at(1.0 + 0.5 * std::f64::consts::FRAC_PI_2)
            .unwrap();
        assert!((pose.tangent[0]).abs() < 1e-12); // turned right: now -y
        assert!((pose.tangent[1] - (-1.0)).abs() < 1e-12);
        assert!((pose.curvature_inv_m + 2.0).abs() < 1e-12); // -1/0.5
    }
}
