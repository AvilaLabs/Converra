//! Filament-model Biot-Savart over a declared centerline.
//!
//! Two surfaces:
//!
//! - `path_filament_field` / `path3d_filament_field` — a single
//!   centerline filament, used as the physics-invariant cross-check
//!   the pack evaluator's tests call (a thin pack at standoff distance
//!   must approach the filament).
//! - `FilamentSpec` + `field_at_probes` — the winding's ampere-turns
//!   smeared over K offset filaments spanning the pack's (radial ×
//!   binormal) band, the same model `tools/oc031_helix_fieldmap.py`
//!   runs to produce the shipped OC-031 declared field map. This is
//!   what generates a `field_map` for a `path3d` case without an
//!   external solver.
//!
//! This is a centerline-filament approximation — not a
//! tape-finite-width or iron correction. Declared maps record their
//! source; a filament map must be labeled as such wherever it lands.

use optcoil_model::{ModelError, path::CoilPath, path::PathPose, path3d::CoilPath3D};

const MU0: f64 = 4e-7 * std::f64::consts::PI;

fn invalid(msg: impl Into<String>) -> ModelError {
    ModelError::Invalid(msg.into())
}

/// Line-sample count for a single-filament walk — sized so the chord
/// sag (~ds²/8R) stays far under the cross-check tolerances.
fn single_filament_samples(length_m: f64) -> usize {
    ((length_m / 1e-4).ceil() as usize).clamp(4_096, 2_000_000)
}

/// B at `probe_m` from one current loop walked over `pos` — dl by
/// centered differences, periodic when `closed` (a loop's dl wraps;
/// an open helix's ends use one-sided differences, np.gradient
/// edge_order=1 parity).
fn filament_field_poses(
    pos: &[[f64; 3]],
    closed: bool,
    current_a: f64,
    probe_m: [f64; 3],
) -> [f64; 3] {
    let n = pos.len();
    let mut acc = [0.0; 3];
    for i in 0..n {
        let (lo, hi, scale) = if closed {
            let lo = if i == 0 { n - 1 } else { i - 1 };
            let hi = (i + 1) % n;
            (lo, hi, 0.5)
        } else {
            let lo = i.saturating_sub(1);
            let hi = (i + 1).min(n - 1);
            (lo, hi, 1.0 / (hi - lo).max(1) as f64)
        };
        let dl = [
            (pos[hi][0] - pos[lo][0]) * scale,
            (pos[hi][1] - pos[lo][1]) * scale,
            (pos[hi][2] - pos[lo][2]) * scale,
        ];
        let d = [
            probe_m[0] - pos[i][0],
            probe_m[1] - pos[i][1],
            probe_m[2] - pos[i][2],
        ];
        let r2 = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1e-18);
        let inv = r2.powf(-1.5);
        // dl × r̂ / r²
        acc[0] += (dl[1] * d[2] - dl[2] * d[1]) * inv;
        acc[1] += (dl[2] * d[0] - dl[0] * d[2]) * inv;
        acc[2] += (dl[0] * d[1] - dl[1] * d[0]) * inv;
    }
    [
        MU0 * current_a / (4.0 * std::f64::consts::PI) * acc[0],
        MU0 * current_a / (4.0 * std::f64::consts::PI) * acc[1],
        MU0 * current_a / (4.0 * std::f64::consts::PI) * acc[2],
    ]
}

/// Walk `pose_at` uniformly in arc length, returning one position per
/// sample. Returns `Err` when the centerline itself won't validate —
/// a degenerate path cannot carry a field.
fn centerline_positions(
    length_m: f64,
    samples: usize,
    closed: bool,
    pose_at: impl Fn(f64) -> Result<PathPose, ModelError>,
    offset: impl Fn(&PathPose) -> [f64; 3],
) -> Result<Vec<[f64; 3]>, ModelError> {
    if !(length_m.is_finite() && length_m > 0.0) {
        return Err(invalid("filament model needs a positive-length centerline"));
    }
    let mut pos = Vec::with_capacity(samples);
    for i in 0..samples {
        let s = i as f64 * length_m / samples as f64;
        // A closed loop's last sample stops one ds short of the
        // (duplicate) start pose; an open path stops at L−ds.
        let _ = closed;
        pos.push(offset(&pose_at(s)?));
    }
    Ok(pos)
}

/// The field of the centerline filament of a racetrack winding at
/// `probe_m` — `None` when the geometry won't validate or the probe
/// lies essentially on the filament (the Biot-Savart singularity;
/// callers report it rather than silently returning a clamped value).
///
/// This is the `thin_filament` model the kernel cross-check declares:
/// the winding's ampere-turns carried by a single centerline loop —
/// for a planar loop turns ≡ 1, so filament current = `ampere_turns_a`.
pub fn racetrack_filament_field(
    racetrack: &optcoil_model::magnetics::Racetrack,
    probe_m: [f64; 3],
) -> Option<[f64; 3]> {
    if !probe_m.iter().all(|v| v.is_finite()) {
        return None;
    }
    let path = CoilPath::racetrack(racetrack.straight_half_length_m, racetrack.bend_radius_m);
    path.validate().ok()?;
    let length = path.length_m();
    let pos = centerline_positions(
        length,
        single_filament_samples(length),
        true,
        |s| path.pose_at(s),
        |pose| pose.position_m,
    )
    .ok()?;
    // Singular check: a probe inside ~1 µm of a filament sample sits
    // on the conductor — the physical answer there is a cross-section
    // problem, not a filament one.
    let min_r2 = pos
        .iter()
        .map(|p| {
            (probe_m[0] - p[0]).powi(2) + (probe_m[1] - p[1]).powi(2) + (probe_m[2] - p[2]).powi(2)
        })
        .fold(f64::INFINITY, f64::min);
    if min_r2 < 1e-12 {
        return None;
    }
    Some(filament_field_poses(
        &pos,
        true,
        racetrack.ampere_turns_a,
        probe_m,
    ))
}

/// The field of a single centerline filament of a planar `path`
/// winding carrying `ni_a` ampere-turns, at `probe_m` (tesla).
///
/// This is the physics cross-check `racetrack`'s tests call — a thin
/// pack at standoff distance converges to this value.
pub fn path_filament_field(
    path: &CoilPath,
    ni_a: f64,
    probe_m: [f64; 3],
) -> Result<[f64; 3], ModelError> {
    path.validate()
        .map_err(|e| invalid(format!("filament path invalid: {e}")))?;
    if !ni_a.is_finite() {
        return Err(invalid("filament ni_a must be finite"));
    }
    if probe_m.iter().any(|v| !v.is_finite()) {
        return Err(invalid("filament probe must be finite"));
    }
    let length = path.length_m();
    let pos = centerline_positions(
        length,
        single_filament_samples(length),
        true,
        |s| path.pose_at(s),
        |pose| pose.position_m,
    )?;
    Ok(filament_field_poses(&pos, true, ni_a, probe_m))
}

/// The field of a `path3d` winding carrying total `ni_a`
/// ampere-turns, evaluated as a single centerline filament whose loop
/// current is `ni_a / winding_turns` — at `probe_m` (tesla).
pub fn path3d_filament_field(
    path3d: &CoilPath3D,
    ni_a: f64,
    probe_m: [f64; 3],
) -> Result<[f64; 3], ModelError> {
    path3d
        .validate()
        .map_err(|e| invalid(format!("filament path3d invalid: {e}")))?;
    if !ni_a.is_finite() {
        return Err(invalid("filament ni_a must be finite"));
    }
    let turns = path3d.winding_turns();
    if turns <= 0.0 {
        return Err(invalid("filament path3d must declare nonzero turns"));
    }
    if probe_m.iter().any(|v| !v.is_finite()) {
        return Err(invalid("filament probe must be finite"));
    }
    let length = path3d.length_m();
    let pos = centerline_positions(
        length,
        single_filament_samples(length),
        path3d.closed,
        |s| path3d.pose_at(s),
        |pose| pose.position_m,
    )?;
    Ok(filament_field_poses(
        &pos,
        path3d.closed,
        ni_a / turns,
        probe_m,
    ))
}

/// Parameters for the band-smeared filament evaluation of one `path3d`
/// winding — what a declared `field_map` carries.
pub struct FilamentSpec<'a> {
    /// The declared non-planar centerline.
    pub path3d: &'a CoilPath3D,
    /// Pack band the smearing filaments span: radial (normal) extent.
    pub radial_band_m: f64,
    /// In-surface binormal extent — the tape-width direction.
    pub width_band_m: f64,
    /// Filaments across each band (total filaments = n_rho × n_width).
    pub n_rho: usize,
    pub n_width: usize,
    /// Uniform arc-length samples along the whole centerline per
    /// filament — Biot-Savart convergence needs ~1e4+ on long helixes.
    pub line_samples: usize,
    /// Total ampere-turns the field is evaluated at; map entries
    /// scale linearly with candidate NI downstream.
    pub reference_ni_a: f64,
}

impl<'a> FilamentSpec<'a> {
    /// The OC-031 conventions: 4×2 band filaments, 24k line samples.
    /// `reference_ni_a` is caller-chosen — the map's declared
    /// evaluation current.
    pub fn new(
        path3d: &'a CoilPath3D,
        radial_band_m: f64,
        width_band_m: f64,
        reference_ni_a: f64,
    ) -> Self {
        Self {
            path3d,
            radial_band_m,
            width_band_m,
            n_rho: 4,
            n_width: 2,
            line_samples: 24_000,
            reference_ni_a,
        }
    }
}

/// Offset-filament band positions within a band of `band_m`: the
/// middle-half convention from the reference generator — fractions
/// `-0.5 + (k+0.5)/n`, offsets `frac × band/2`.
fn band_offsets(n: usize, band_m: f64) -> Vec<f64> {
    (0..n)
        .map(|k| (-0.5 + (k as f64 + 0.5) / n as f64) * band_m * 0.5)
        .collect()
}

/// The band-smeared field of the winding at `probe_m`, tesla.
pub fn field_at(spec: &FilamentSpec, probe_m: [f64; 3]) -> Result<[f64; 3], ModelError> {
    Ok(field_at_probes(spec, &[probe_m])?[0])
}

/// B at every probe: `probes` rows of `[x,y,z]` m → same-length
/// `[bx,by,bz]` tesla rows at `reference_ni_a`.
pub fn field_at_probes(
    spec: &FilamentSpec,
    probes: &[[f64; 3]],
) -> Result<Vec<[f64; 3]>, ModelError> {
    spec.path3d
        .validate()
        .map_err(|e| invalid(format!("filament path3d invalid: {e}")))?;
    let turns = spec.path3d.winding_turns();
    if turns <= 0.0 {
        return Err(invalid("filament path3d must declare nonzero turns"));
    }
    let k = (spec.n_rho * spec.n_width) as f64;
    // True-NI semantics: each filament traces every turn of the
    // winding, so its loop current is NI/(turns·k). (The reference
    // python generator divided by k only — OC-031's shipped entries
    // therefore carry 50×-stronger fields than their label; the case
    // stays self-consistent because its bore anchor was generated the
    // same way. New maps generated here use the corrected convention.)
    let current = spec.reference_ni_a / (turns * k);
    let length = spec.path3d.length_m();
    let n_samples = spec.line_samples.max(4);
    let ds = length / n_samples as f64;
    let mut out = vec![[0.0; 3]; probes.len()];
    for dr in band_offsets(spec.n_rho, spec.radial_band_m) {
        for dw in band_offsets(spec.n_width, spec.width_band_m) {
            // Offset this filament in the declared (n, w=n×t) frame.
            let mut pos = Vec::with_capacity(n_samples);
            for i in 0..n_samples {
                let pose = spec.path3d.pose_at(i as f64 * ds)?;
                let n = pose.normal;
                let t = pose.tangent;
                let w = [
                    n[1] * t[2] - n[2] * t[1],
                    n[2] * t[0] - n[0] * t[2],
                    n[0] * t[1] - n[1] * t[0],
                ];
                pos.push([
                    pose.position_m[0] + dr * n[0] + dw * w[0],
                    pose.position_m[1] + dr * n[1] + dw * w[1],
                    pose.position_m[2] + dr * n[2] + dw * w[2],
                ]);
            }
            for (out_b, probe) in out.iter_mut().zip(probes) {
                let b = filament_field_poses(&pos, spec.path3d.closed, current, *probe);
                out_b[0] += b[0];
                out_b[1] += b[1];
                out_b[2] += b[2];
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::path3d::PathSegment3D;

    /// OC-031's declared winding: 50 turns at 4 mm/turn on a 25 mm
    /// former, 4 mm × 8 mm pack band.
    fn oc031_path() -> CoilPath3D {
        CoilPath3D {
            segments: vec![PathSegment3D::Helix {
                axis_origin_m: [0.0, 0.0, 0.0],
                axis_dir: [0.0, 0.0, 1.0],
                radius_m: 0.025,
                start_azimuth_deg: 0.0,
                turns: 50.0,
                rise_per_turn_m: 0.004,
            }],
            closed: false,
        }
    }

    /// Parity against the shipped artifact: the case's `b_target_t` IS
    /// this filament model's field at the bore probe — the python
    /// generator anchored the requirement to it. Same algorithm, so
    /// the port must reproduce it tightly.
    #[test]
    fn filament_model_reproduces_oc031_bore_anchor() {
        let path = oc031_path();
        // OC-031's shipped map was generated with the loop-current
        // convention (reference 1e4 as filament current), i.e. an
        // effective NI of 50 × 1e4 — parity is asserted against the
        // field that map actually encodes.
        let spec = FilamentSpec::new(&path, 0.004, 0.008, 50.0 * 1.0e4);
        let b = field_at(&spec, [0.0, 0.0, 0.1]).unwrap();
        let norm = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
        let expected = 3.04764663027809; // oc-031 requirement.b_target_t
        assert!(
            (norm - expected).abs() / expected < 1e-4,
            "filament bore anchor {norm} vs shipped {expected}"
        );
    }

    /// A long dense helix approaches the finite-solenoid on-axis bound
    /// B = μ0 n I · L/√(L²+D²) — sanity for the port independent of the
    /// shipped artifact.
    #[test]
    fn dense_helix_approaches_the_finite_solenoid_bound() {
        let turns = 200.0;
        let rise = 0.004;
        let length_z = turns * rise; // 0.8 m on R=0.025
        let path = CoilPath3D {
            segments: vec![PathSegment3D::Helix {
                axis_origin_m: [0.0, 0.0, 0.0],
                axis_dir: [0.0, 0.0, 1.0],
                radius_m: 0.025,
                start_azimuth_deg: 0.0,
                turns,
                rise_per_turn_m: rise,
            }],
            closed: false,
        };
        let current_per_turn = 200.0;
        let spec = FilamentSpec {
            // A single centerline filament — band smearing is a
            // second-order correction to the on-axis field.
            n_rho: 1,
            n_width: 1,
            line_samples: 96_000,
            ..FilamentSpec::new(&path, 0.004, 0.008, turns * current_per_turn)
        };
        let b = field_at(&spec, [0.0, 0.0, length_z / 2.0]).unwrap();
        let bz = b[2];
        let n_per_m = 1.0 / rise;
        let d = 2.0 * 0.025;
        let exact =
            MU0 * n_per_m * current_per_turn * length_z / (length_z * length_z + d * d).sqrt();
        assert!(
            (bz - exact).abs() / exact < 0.02,
            "filament on-axis {bz} vs solenoid limit {exact}"
        );
    }

    /// The same helix evaluated through both entry points agrees —
    /// the band-smearing spec at 1×1 is the single filament.
    #[test]
    fn band_spec_at_one_filament_matches_the_single_filament_walk() {
        let path = oc031_path();
        let spec = FilamentSpec {
            n_rho: 1,
            n_width: 1,
            ..FilamentSpec::new(&path, 0.004, 0.008, 1.0e4)
        };
        let band = field_at(&spec, [0.05, -0.03, 0.12]).unwrap();
        let single = path3d_filament_field(&path, 1.0e4, [0.05, -0.03, 0.12]).unwrap();
        let num = (0..3).map(|i| (band[i] - single[i]).abs()).sum::<f64>();
        let den = single.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!(num / den < 1e-3, "band {band:?} vs single {single:?}");
    }
}
