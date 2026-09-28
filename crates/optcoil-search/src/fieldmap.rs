//! Declared `field_map` generation for `path3d` cases.
//!
//! `path3d` windings need a declared Cartesian map — the engine's
//! evaluator is planar-only. This module generates one with
//! `optcoil_physics::filament` (the band-smeared Biot-Savart the OC-031
//! python generator runs), emitted in the schema's own terms: a
//! `FieldMap::CartesianBxByBz` covering the swept pack hull, the
//! `bore_field_at_reference_t` anchor the NI solve consumes, and the
//! source CSV whose sha256 binds the declaration.
//!
//! Provenance: a generated map is a filament-model field, not an FEA
//! export — the CSV's fingerprint line names the model, and callers
//! must carry that label into case `provenance` text. The
//! `reference_ampere_turns_a` here is true ampere-turns (filament loop
//! current = NI / winding turns); see `optcoil_physics::filament`'s
//! docstring for the OC-031 convention difference.

use optcoil_model::coupled::{FieldMap, FieldMapCartesianEntry};
use optcoil_model::coupled_search::SearchFieldMap;
use optcoil_model::path3d::CoilPath3D;
use optcoil_physics::filament::{FilamentSpec, field_at, field_at_probes};
use sha2::{Digest, Sha256};

use crate::RunError;

fn invalid(msg: impl Into<String>) -> RunError {
    RunError::Invalid(msg.into())
}

/// How the map's Cartesian grid is sized. The swept pack hull comes
/// from `CoilPath3D::domain_test_points` — the same points the schema's
/// containment check uses — extended by `margin_m` and pitched at
/// `spacing_m`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MapGrid {
    /// Grid pitch (m) on every axis.
    pub spacing_m: f64,
    /// Margin (m) each side of the swept pack hull.
    pub margin_m: f64,
    /// Line samples along the centerline per filament.
    pub line_samples: usize,
    /// Filaments across the radial band.
    pub n_rho: usize,
    /// Filaments across the binormal band.
    pub n_width: usize,
}

impl Default for MapGrid {
    /// The OC-031 conventions: ~5 mm pitch, 10 mm margin, 4×2
    /// filaments, 24k line samples.
    fn default() -> Self {
        Self {
            spacing_m: 0.005,
            margin_m: 0.01,
            line_samples: 24_000,
            n_rho: 4,
            n_width: 2,
        }
    }
}

/// A generated declared map plus its provenance sidecar.
/// Serialize/Deserialize: the browser workbench passes it across the
/// search worker's postMessage boundary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneratedFieldMap {
    /// `sampling.field_map` / `field_map.map` — levels + entries.
    pub map: FieldMap,
    /// `field_map.bore_field_at_reference_t` — the NI-solve anchor:
    /// the same filament model evaluated at `requirement.bore_probe_m`.
    pub bore_field_at_reference_t: f64,
    /// The source CSV the sha binds — write it next to the case.
    pub source_csv: Vec<u8>,
    /// sha256 of `source_csv`.
    pub source_sha256: String,
}

/// Generate a declared field map for a `path3d` winding.
///
/// - `path3d` — the declared centerline (must validate; pack band
///   offsets use its declared (n, w) frame).
/// - `radial_band_m` / `width_band_m` — `pack_radial_width_m` /
///   `pack_axial_height_m` — the band the filaments are smeared over
///   and the hull the grid must contain.
/// - `bore_probe_m` — `requirement.bore_probe_m` — the anchor probe.
/// - `reference_ni_a` — true total ampere-turns the map is evaluated
///   at (convention: baseline turns × tapes × strands × I_op).
pub fn generate_path3d_field_map(
    path3d: &CoilPath3D,
    radial_band_m: f64,
    width_band_m: f64,
    bore_probe_m: [f64; 3],
    reference_ni_a: f64,
    grid: &MapGrid,
) -> Result<GeneratedFieldMap, RunError> {
    path3d
        .validate()
        .map_err(|e| invalid(format!("path3d cannot carry a map: {e}")))?;
    if !(radial_band_m > 0.0 && width_band_m > 0.0) {
        return Err(invalid("pack extents must be positive to size a map"));
    }
    if !(reference_ni_a.is_finite() && reference_ni_a > 0.0) {
        return Err(invalid("reference ampere-turns must be positive"));
    }
    if !(grid.spacing_m > 0.0 && grid.margin_m >= 0.0) {
        return Err(invalid("grid spacing must be positive"));
    }
    let spec = FilamentSpec {
        n_rho: grid.n_rho,
        n_width: grid.n_width,
        line_samples: grid.line_samples,
        ..FilamentSpec::new(path3d, radial_band_m, width_band_m, reference_ni_a)
    };

    // Levels: the swept-pack hull ± margin, pitched at spacing_m and
    // aligned to spacing multiples so the grid is reproducible from
    // the declared geometry alone.
    let hull_pts = path3d
        .domain_test_points(radial_band_m / 2.0, width_band_m / 2.0)
        .map_err(|e| invalid(format!("path3d hull failed: {e}")))?;
    let levels = |axis: usize| -> Vec<f64> {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in &hull_pts {
            lo = lo.min(p[axis]);
            hi = hi.max(p[axis]);
        }
        let i0 = ((lo - grid.margin_m) / grid.spacing_m).floor() as i64;
        let i1 = ((hi + grid.margin_m) / grid.spacing_m).ceil() as i64;
        (i0..=i1)
            .map(|i| (i as f64 * grid.spacing_m * 1e6).round() / 1e6)
            .collect()
    };
    let (x_levels, y_levels, z_levels) = (levels(0), levels(1), levels(2));
    let mut probes = Vec::with_capacity(x_levels.len() * y_levels.len() * z_levels.len());
    for &x in &x_levels {
        for &y in &y_levels {
            for &z in &z_levels {
                probes.push([x, y, z]);
            }
        }
    }
    let fields = field_at_probes(&spec, &probes)
        .map_err(|e| invalid(format!("filament evaluation failed: {e}")))?;
    let bore = field_at(&spec, bore_probe_m)
        .map_err(|e| invalid(format!("filament bore anchor failed: {e}")))?;
    let bore_field_at_reference_t =
        (bore[0] * bore[0] + bore[1] * bore[1] + bore[2] * bore[2]).sqrt();

    // The same CSV shape the python generator emits — comment
    // fingerprint first, then x,y,z,bx,by,bz rows.
    let fingerprint = format!(
        "optcoil-filament-map/v1 turns={} NI={} band={}x{} K={}x{} N={} spacing={} margin={}",
        path3d.winding_turns(),
        reference_ni_a,
        radial_band_m,
        width_band_m,
        grid.n_rho,
        grid.n_width,
        grid.line_samples,
        grid.spacing_m,
        grid.margin_m,
    );
    let mut csv = format!("# {fingerprint}\nx_m,y_m,z_m,bx_t,by_t,bz_t\n");
    for (p, b) in probes.iter().zip(&fields) {
        csv.push_str(&format!(
            "{:.10},{:.10},{:.10},{:.10e},{:.10e},{:.10e}\n",
            p[0], p[1], p[2], b[0], b[1], b[2]
        ));
    }
    let source_csv = csv.into_bytes();
    let source_sha256 = format!("{:x}", Sha256::digest(&source_csv));

    // Entries must cover the complete product grid in x-outermost
    // order — the probe loop is already in that order.
    let ny = y_levels.len();
    let nz = z_levels.len();
    let entries = fields
        .iter()
        .enumerate()
        .map(|(i, b)| FieldMapCartesianEntry {
            x_index: (i / (ny * nz)) as u32,
            y_index: ((i / nz) % ny) as u32,
            z_index: (i % nz) as u32,
            bx_t: b[0],
            by_t: b[1],
            bz_t: b[2],
        })
        .collect();
    Ok(GeneratedFieldMap {
        map: FieldMap::CartesianBxByBz {
            source_sha256: source_sha256.clone(),
            reference_ampere_turns_a: reference_ni_a,
            x_levels_m: x_levels,
            y_levels_m: y_levels,
            z_levels_m: z_levels,
            entries,
        },
        bore_field_at_reference_t,
        source_csv,
        source_sha256,
    })
}

/// Assemble the `field_map` declaration block a case carries.
pub fn declared_field_map(generated: &GeneratedFieldMap) -> SearchFieldMap {
    SearchFieldMap {
        map: generated.map.clone(),
        bore_field_at_reference_t: generated.bore_field_at_reference_t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::path3d::PathSegment3D;

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

    /// A generated map must pass the schema's own gates: complete
    /// product grid, ascending levels, hull containing the swept pack,
    /// finite entries — plus a positive bore anchor.
    #[test]
    fn generated_map_satisfies_the_schema_invariants() {
        let g = generate_path3d_field_map(
            &oc031_path(),
            0.004,
            0.008,
            [0.0, 0.0, 0.1],
            1.0e4,
            &MapGrid {
                spacing_m: 0.02,
                ..MapGrid::default()
            },
        )
        .unwrap();
        let FieldMap::CartesianBxByBz {
            x_levels_m,
            y_levels_m,
            z_levels_m,
            entries,
            ..
        } = &g.map
        else {
            panic!("generated map is always Cartesian");
        };
        assert_eq!(
            entries.len(),
            x_levels_m.len() * y_levels_m.len() * z_levels_m.len()
        );
        for levels in [x_levels_m, y_levels_m, z_levels_m] {
            assert!(levels.windows(2).all(|w| w[0] < w[1]));
        }
        // Hull containment: every swept-pack corner inside the levels.
        let path = oc031_path();
        for p in path.domain_test_points(0.002, 0.004).unwrap() {
            for (axis, levels) in [(0, x_levels_m), (1, y_levels_m), (2, z_levels_m)] {
                assert!(
                    p[axis] >= *levels.first().unwrap() && p[axis] <= *levels.last().unwrap(),
                    "axis {axis} point {} outside levels",
                    p[axis]
                );
            }
        }
        assert!(g.bore_field_at_reference_t > 0.0);
        assert_eq!(g.source_sha256.len(), 64);
    }
}
