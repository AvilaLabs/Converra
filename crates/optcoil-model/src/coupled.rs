//! `optcoil-coupled-conductor/v1`: the OC-004 screening case schema and pure
//! geometry helpers connecting a homogenized racetrack pack (OC-002 family)
//! to a per-tape width/orientation model that will be queried against a
//! measured conductor law (OC-003). This module holds schema, validation and
//! geometry only: there is no field evaluator and no Ic interpolator here, so
//! the model crate stays free of physics. Dataset-dependent identity checks
//! (csv hash, method, criterion, temperature interiority) take an already
//! loaded `MaterialDataset` rather than reaching into optcoil-physics.
//!
//! Everything this schema describes is a declared screening assumption, not
//! a production operating-current limit; see the OC-004 contract and
//! docs/OC004.md for the physical justification behind each field.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::path::CoilPath;
use crate::{DataClass, ModelError, material::MaterialDataset};

pub const COUPLED_CASE_SCHEMA: &str = "optcoil-coupled-conductor/v1";
/// v2 adds `limits.self_field_correction` (OC-014). v1 cases must not
/// declare it — the field silently changes screening semantics.
pub const COUPLED_CASE_SCHEMA_V2: &str = "optcoil-coupled-conductor/v2";
/// v3: v2 plus `limits.along_current_model` (OC-017).
pub const COUPLED_CASE_SCHEMA_V3: &str = "optcoil-coupled-conductor/v3";
/// v4: v3 plus `limits.self_field_correction = "critical_state_strip"`
/// and its declared `limits.critical_state_layer_thickness_m` (OC-014
/// Phase 2). v3 and below must not declare them — the value changes the
/// screening query in the self-field-dominated regime.
pub const COUPLED_CASE_SCHEMA_V4: &str = "optcoil-coupled-conductor/v4";
/// v5: v4 plus general planar geometry — `pack.path` (a `CoilPath`) and
/// `Station::Path { s_m }` for non-racetrack windings (coupled-search
/// schema v9 generates these cases). v4 and below must not declare them —
/// the pack's centerline semantics change (rho becomes a signed offset).
pub const COUPLED_CASE_SCHEMA_V5: &str = "optcoil-coupled-conductor/v5";
/// v6: v5 plus graded material bindings — `tape_specs` (named
/// `MaterialSettings` beyond the case-level `material`) and
/// `winding.regions` assigning turn ranges to specs (coupled-search
/// schema v10 generates these cases). v5 and below must not declare
/// them — the material law becomes position-dependent.
pub const COUPLED_CASE_SCHEMA_V6: &str = "optcoil-coupled-conductor/v6";
/// v7: v6 plus `sampling.field_map` — an externally supplied,
/// customer-declared field map replacing engine-computed fields at every
/// sampled point. v6 and below must not declare it — field provenance
/// changes (declared, not computed).
pub const COUPLED_CASE_SCHEMA_V7: &str = "optcoil-coupled-conductor/v7";
/// v8: v7 plus `field_map` component `cartesian_bx_by_bz` — a full
/// lab-frame (B_x, B_y, B_z) map on a rectilinear (x, y, z) grid for
/// non-axisymmetric windings and general solver exports. Confinement is
/// checked against the swept pack domain of any planar `pack.path` (or
/// the racetrack dims, which synthesize one).
pub const COUPLED_CASE_SCHEMA_V8: &str = "optcoil-coupled-conductor/v8";
/// v9: v8 plus `pack.path3d` — a non-planar `CoilPath3D` helix centerline
/// for CCT/CORC-class windings. v8 and below must not declare it — the
/// pack's centerline semantics change again (the reference normal is the
/// cylinder radial, the width direction the in-surface binormal), and
/// `tape_normal` must be `radial` (the stacking direction a helical
/// conductor actually has). Non-planar packs evaluate under a declared
/// Cartesian `field_map` only — the engine's pack field evaluator is
/// planar and a helix pack's self-field solve is out of scope.
pub const COUPLED_CASE_SCHEMA_V9: &str = "optcoil-coupled-conductor/v9";

/// Hard ceiling of the `transverse_bound` along-current model (OC-017):
/// above this `|B_t|/|B|` fraction the "parallel component counts as
/// transverse" bound loses its footing (the transverse drive is no longer
/// dominant and force-free/flux-cutting physics enters), so the point is
/// still excluded. A model constant, not a case-declared limit.
pub const ALONG_CURRENT_BOUND_CEILING: f64 = 0.5;
pub const OC004_JSON: &str = include_str!("../../../benchmarks/coupled/oc-004.json");
/// OC-014 Phase-2 parity fixture: the OC-004 geometry and sampling plan
/// re-pointed at the modelext dataset with `critical_state_strip` +
/// `critical_state_layer_thickness_m` (schema v4), so the frozen reference
/// exercises the perpendicular-sector floor bound.
pub const OC014_JSON: &str = include_str!("../../../benchmarks/coupled/oc-014.json");

/// Duplicated from `optcoil_physics::critical_current::LOG_IC_MODEL_ID` so
/// this crate does not depend on optcoil-physics; a physics-side test
/// asserts the two constants agree.
pub const EXPECTED_MATERIAL_METHOD_ID: &str = "measured-coordinate-tetrahedral-log-field-log-ic/v1";

// Numerical work limits (contract §2), not manufacturing or physical limits.
const MAX_STATIONS: usize = 16;
const MAX_SAMPLED_INDICES: usize = 64;
/// Solenoid-class windings stack a few hundred pancake positions along
/// the pack width, and the generated full plan enumerates every one —
/// the width-axis list cap must admit that enumeration (OC-024's 72
/// pancakes could not be sampled under the turn-axis cap). Still a work
/// limit, not a manufacturing claim; entries remain pinned to the
/// winding's own declared `tapes_along_width` (<= 1000).
const MAX_SAMPLED_TAPE_INDICES: usize = 256;
const MAX_CANDIDATES: usize = 8;
const MAX_TURNS_ALONG_NORMAL: u32 = 100_000;
const MAX_TAPES_ALONG_WIDTH: u32 = 1_000;
/// Cable-class conductors run to a few hundred tape strands per
/// position; 4096 is generous headroom, not a physical claim.
const MAX_STRANDS_PARALLEL: u32 = 4_096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledCase {
    pub schema: String,
    pub id: String,
    pub provenance: String,
    pub geometry_data_class: DataClass,
    pub pack: Pack,
    pub winding: Winding,
    pub operating: Operating,
    /// The base conductor binding: dataset identity, interpolation method
    /// and tape-frame policies applied to every turn that no
    /// `winding.regions` entry covers — and to every turn at all when no
    /// regions are declared. Always required: a fully graded case still
    /// declares the conductor its ungraded turns would use.
    pub material: MaterialSettings,
    /// Schema v6 only: named alternative conductor bindings for graded
    /// regions. Each value is a full `MaterialSettings` — dataset identity,
    /// method and policies are per-spec, since different tape products
    /// carry different measured data. Required iff `winding.regions`
    /// declares a spec; only specs actually referenced belong here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape_specs: Option<std::collections::BTreeMap<String, MaterialSettings>>,
    pub sampling: Sampling,
    pub limits: Limits,
    pub numerics: Numerics,
    pub width_transfer: WidthTransfer,
}

/// Homogenized winding-pack geometry. The centerline is declared one of
/// three ways, exactly one per case:
///
/// * `straight_half_length_m` + `bend_radius_m` — the legacy racetrack,
///   same lab-frame convention as `magnetics::Racetrack` (straights along
///   X, bends centered at `(+/- straight_half_length_m, 0, 0)`); a tape's
///   `rho` coordinate is its absolute radius from the origin.
/// * `path` — a general closed planar `CoilPath` (coupled-conductor
///   schema v5); a tape's `rho` coordinate is its signed offset from the
///   centerline along the path normal, so the pack occupies
///   `[-radial_width_m/2, +radial_width_m/2]`.
/// * `path3d` — a non-planar `CoilPath3D` helix centerline
///   (coupled-conductor schema v9) for CCT/CORC-class windings; a tape's
///   `rho` coordinate is its signed cylinder-radial offset and the
///   `z`-coordinate its offset along the in-surface binormal
///   `n_ref × t̂`. `tape_normal` must be `radial` — the face-toward-axis
///   stacking direction helical conductors actually wind in.
///
/// `radial_width_m` / `axial_height_m` are the pack cross-section in all
/// representations. Current is supplied per candidate by the runner, not
/// stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straight_half_length_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_radius_m: Option<f64>,
    pub radial_width_m: f64,
    pub axial_height_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<CoilPath>,
    /// Schema v9 only: non-planar helix centerline — see `CoilPath3D`.
    /// Mutually exclusive with `path` and the racetrack dims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path3d: Option<crate::path3d::CoilPath3D>,
}

/// A winding-pack centerline as `Station::Path` consumes it: poses and
/// lengths only, so the planar and non-planar representations share one
/// sampling path. `Planar` carries a `CoilPath` whose reference normal is
/// the in-plane normal and whose width direction is `ẑ`; `Helical`
/// carries a `CoilPath3D` whose reference normal is the cylinder radial
/// and whose width direction is the in-surface binormal `n_ref × t̂` —
/// which reduces to `ẑ` on a planar path, so one formula serves both.
#[derive(Debug, Clone, Copy)]
pub enum Centerline<'a> {
    Planar(&'a CoilPath),
    Helical(&'a crate::path3d::CoilPath3D),
}

impl Centerline<'_> {
    /// The pose at arc length `s_m` along the centerline.
    pub fn pose_at(&self, s_m: f64) -> Result<crate::path::PathPose, ModelError> {
        match self {
            Centerline::Planar(path) => path.pose_at(s_m),
            Centerline::Helical(path) => path.pose_at(s_m),
        }
    }

    /// Total centerline length (m).
    pub fn length_m(&self) -> f64 {
        match self {
            Centerline::Planar(path) => path.length_m(),
            Centerline::Helical(path) => path.length_m(),
        }
    }

    /// The (t, n, w) tape frame at arc length `s_m`. `n_ref` is the
    /// path's reference normal (in-plane normal / cylinder radial);
    /// `w_ref = n_ref × t̂` is the path's width direction (`ẑ` on planar
    /// paths, the in-surface binormal on helices). `Radial` faces the
    /// tape along `n_ref`; `Axial` faces it along `w_ref`.
    pub fn tape_frame_at(
        &self,
        s_m: f64,
        tape_normal: TapeNormal,
    ) -> Result<TapeFrame, ModelError> {
        let pose = self.pose_at(s_m)?;
        let w_ref = [
            pose.normal[1] * pose.tangent[2] - pose.normal[2] * pose.tangent[1],
            pose.normal[2] * pose.tangent[0] - pose.normal[0] * pose.tangent[2],
            pose.normal[0] * pose.tangent[1] - pose.normal[1] * pose.tangent[0],
        ];
        Ok(match tape_normal {
            TapeNormal::Radial => TapeFrame {
                t: pose.tangent,
                n: pose.normal,
                w: w_ref,
            },
            TapeNormal::Axial => TapeFrame {
                t: pose.tangent,
                n: w_ref,
                w: pose.normal,
            },
        })
    }

    /// Lab-frame position of a tape centre: `rho_m` along the reference
    /// normal, `z_m` along the width direction `n_ref × t̂` — identical
    /// to the planar `pos + rho·n̂ + z·ẑ` on a planar path.
    pub fn position_at(&self, s_m: f64, rho_m: f64, z_m: f64) -> Result<[f64; 3], ModelError> {
        let pose = self.pose_at(s_m)?;
        let w_ref = [
            pose.normal[1] * pose.tangent[2] - pose.normal[2] * pose.tangent[1],
            pose.normal[2] * pose.tangent[0] - pose.normal[0] * pose.tangent[2],
            pose.normal[0] * pose.tangent[1] - pose.normal[1] * pose.tangent[0],
        ];
        Ok([
            pose.position_m[0] + rho_m * pose.normal[0] + z_m * w_ref[0],
            pose.position_m[1] + rho_m * pose.normal[1] + z_m * w_ref[1],
            pose.position_m[2] + rho_m * pose.normal[2] + z_m * w_ref[2],
        ])
    }
}

/// Which pack direction is the tape's wide-face normal (contract A4).
/// `Radial`: pancake/layer winding, wide face on the cylindrical pack
/// surface. `Axial`: tapes stacked side by side along the radial direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TapeNormal {
    Radial,
    Axial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Winding {
    pub tape_width_m: f64,
    pub tape_normal: TapeNormal,
    pub turns_along_normal: u32,
    pub tapes_along_width: u32,
    /// Conductors per turn sharing the operating current (schema v1
    /// extension, defaults to 1): a Roebel-style cable of `s` strands
    /// carries `I_op/s` per strand, so screened capacity scales by `s`
    /// while the ampere-turns and winding-cell geometry are unchanged
    /// (cable footprint approximated as one tape cell).
    #[serde(default = "default_one_u32")]
    pub strands_parallel: u32,
    /// Schema v6 only: graded material assignment along the stacking
    /// direction — a sorted, non-overlapping list of turn ranges each
    /// bound to a `tape_specs` id. Turns no region covers use the
    /// case-level `material`. Region ranges are 1-based inclusive over
    /// `turns_along_normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Vec<WindingRegion>>,
}

/// Schema v6: one graded turn range. `first_turn..=last_turn` (1-based
/// along `turns_along_normal`) is screened and priced under
/// `tape_specs[tape_spec]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindingRegion {
    pub first_turn: u32,
    pub last_turn: u32,
    pub tape_spec: String,
}

fn default_one_u32() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operating {
    pub temperature_k: f64,
    pub current_candidates_a: Vec<f64>,
    pub electric_field_criterion_v_per_m: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AngleMapping {
    #[serde(rename = "period_180_field_reversal")]
    Period180FieldReversal,
    /// Same 180-degree field-reversal fold, plus a declared modeling
    /// assumption: the measured hull is treated as periodic, so the mesh
    /// carries seam cells interpolating *between* the measured points on
    /// either side of the 0/180 fold — never extrapolating past them.
    /// Declared per binding; the record preserves it verbatim.
    #[serde(rename = "period_180_field_reversal_seam_stitched")]
    Period180FieldReversalSeamStitched,
}

impl AngleMapping {
    /// The declared angular seam period in degrees, when this mapping
    /// stitches the fold; `None` for the plain fold.
    pub fn seam_period_deg(self) -> Option<f64> {
        match self {
            Self::Period180FieldReversal => None,
            Self::Period180FieldReversalSeamStitched => Some(180.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MirrorPolicy {
    MinimumOfMirrorPair,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldBasisMapping {
    PackFieldAsAppliedFieldSelfFieldConsistent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldMagnitudePolicy {
    TotalMagnitudeWithTransverseAngle,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LowFieldPolicy {
    MonotoneFieldLowerBound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialSettings {
    pub dataset_id: String,
    pub csv_sha256: String,
    /// Compared against `EXPECTED_MATERIAL_METHOD_ID` in
    /// `validate_against_dataset`; a plain string (not an enum) because the
    /// canonical value lives in optcoil-physics, not here.
    pub method: String,
    pub angle_mapping: AngleMapping,
    pub mirror_policy: MirrorPolicy,
    pub field_basis_mapping: FieldBasisMapping,
    pub field_magnitude_policy: FieldMagnitudePolicy,
    pub low_field_policy: LowFieldPolicy,
    pub low_field_clamp_t: f64,
    pub monotonicity_tolerance: f64,
}

/// A sampled winding-path point. `Straight`/`Arc` address the legacy
/// racetrack: only the top straight (`y > 0`) and the right bend are
/// represented, matching the prescribed-current model's geometric symmetry
/// (contract A4) — the bottom straight and left bend are mirror images
/// that are never independently sampled. `Path` addresses a general
/// `pack.path` centerline by arc length (coupled-conductor schema v5):
/// path stations carry no symmetry assumption, so a path pack samples
/// every region it needs explicitly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Station {
    Straight { id: String, x_m: f64 },
    Arc { id: String, azimuth_deg: f64 },
    Path { id: String, s_m: f64 },
}

/// Local (t, n, w) frame at a winding-path point: `t` is the current
/// direction, `n` the tape-normal direction, `w` completes the right-handed
/// set (contract A4).
#[derive(Debug, Clone, Copy)]
pub struct TapeFrame {
    pub t: [f64; 3],
    pub n: [f64; 3],
    pub w: [f64; 3],
}

impl Station {
    pub fn id(&self) -> &str {
        match self {
            Station::Straight { id, .. } | Station::Arc { id, .. } | Station::Path { id, .. } => id,
        }
    }

    pub fn validate(&self, pack: &Pack) -> Result<(), ModelError> {
        match self {
            Station::Straight { id, x_m } => {
                require(!id.trim().is_empty(), "station id is empty")?;
                let straight = pack.straight_half_length_m.ok_or_else(|| {
                    ModelError::Invalid(
                        "straight station requires racetrack pack geometry".to_owned(),
                    )
                })?;
                require(
                    x_m.is_finite() && x_m.abs() <= straight,
                    "straight station x_m must lie within +/- straight_half_length_m",
                )?;
            }
            Station::Arc { id, azimuth_deg } => {
                require(!id.trim().is_empty(), "station id is empty")?;
                require(
                    pack.bend_radius_m.is_some(),
                    "arc station requires racetrack pack geometry",
                )?;
                require(
                    azimuth_deg.is_finite() && (-90.0..=90.0).contains(azimuth_deg),
                    "arc station azimuth_deg must lie within [-90, 90]",
                )?;
            }
            Station::Path { id, s_m } => {
                require(!id.trim().is_empty(), "station id is empty")?;
                let centerline = pack.centerline().ok_or_else(|| {
                    ModelError::Invalid("path station requires pack.path or pack.path3d".to_owned())
                })?;
                require(
                    s_m.is_finite() && *s_m >= 0.0 && *s_m <= centerline.length_m(),
                    "path station s_m must be finite and within [0, path length]",
                )?;
            }
        }
        Ok(())
    }

    /// Local (t, n, w) frame per contract A4. Continuous with the straight's
    /// frame at `azimuth_deg = 90`, since `t = (-1,0,0)` and `r = (0,1,0)`
    /// there for either representation. `centerline` is the pack's
    /// declared centerline (`pack.centerline()`) — required only for
    /// `Station::Path`.
    pub fn frame(
        &self,
        tape_normal: TapeNormal,
        centerline: Option<Centerline>,
    ) -> Result<TapeFrame, ModelError> {
        let (t, r) = match self {
            Station::Straight { .. } => ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            Station::Arc { azimuth_deg, .. } => {
                let (sin, cos) = azimuth_deg.to_radians().sin_cos();
                ([-sin, cos, 0.0], [cos, sin, 0.0])
            }
            Station::Path { s_m, .. } => {
                let centerline = centerline.ok_or_else(|| {
                    ModelError::Invalid("path station requires a path pack".to_owned())
                })?;
                return centerline.tape_frame_at(*s_m, tape_normal);
            }
        };
        let z = [0.0, 0.0, 1.0];
        Ok(match tape_normal {
            TapeNormal::Radial => TapeFrame { t, n: r, w: z },
            TapeNormal::Axial => TapeFrame { t, n: z, w: r },
        })
    }

    /// Lab-frame tape center position given its (rho, z) coordinates. For
    /// a racetrack pack `rho` is the absolute radius from the origin; for
    /// a path pack it is the signed offset from the centerline along the
    /// reference normal (in-plane normal on planar paths, cylinder radial
    /// on helices), `z` the offset along the path's width direction
    /// (`ẑ` planar, in-surface binormal on helices).
    pub fn position_m(&self, pack: &Pack, rho_m: f64, z_m: f64) -> Result<[f64; 3], ModelError> {
        require(
            rho_m.is_finite() && z_m.is_finite(),
            "station position coordinates must be finite",
        )?;
        match self {
            Station::Straight { x_m, .. } => Ok([*x_m, rho_m, z_m]),
            Station::Arc { azimuth_deg, .. } => {
                let straight = pack.straight_half_length_m.ok_or_else(|| {
                    ModelError::Invalid("arc station requires racetrack pack geometry".to_owned())
                })?;
                let (sin, cos) = azimuth_deg.to_radians().sin_cos();
                Ok([straight + rho_m * cos, rho_m * sin, z_m])
            }
            Station::Path { s_m, .. } => {
                let centerline = pack.centerline().ok_or_else(|| {
                    ModelError::Invalid("path station requires a path pack".to_owned())
                })?;
                centerline.position_at(*s_m, rho_m, z_m)
            }
        }
    }
}

impl Winding {
    /// Pack extent along this winding's stacking ("normal") direction:
    /// `radial_width_m` when tapes stack radially, `axial_height_m` when
    /// they stack axially (contract A4).
    pub fn normal_extent_m(&self, pack: &Pack) -> f64 {
        match self.tape_normal {
            TapeNormal::Radial => pack.radial_width_m,
            TapeNormal::Axial => pack.axial_height_m,
        }
    }

    /// Pack extent along the tape's width direction, the complement of
    /// `normal_extent_m`.
    pub fn width_extent_m(&self, pack: &Pack) -> f64 {
        match self.tape_normal {
            TapeNormal::Radial => pack.axial_height_m,
            TapeNormal::Axial => pack.radial_width_m,
        }
    }

    /// Turn-to-turn pitch along the stacking direction.
    pub fn pitch_n_m(&self, pack: &Pack) -> f64 {
        self.normal_extent_m(pack) / f64::from(self.turns_along_normal)
    }

    /// Tape-to-tape pitch across the width direction; equals `tape_width_m`
    /// once `validate` has confirmed the width-fill identity (contract A1).
    pub fn pitch_w_m(&self, pack: &Pack) -> f64 {
        self.width_extent_m(pack) / f64::from(self.tapes_along_width)
    }

    /// Total physical turns: every (turn_index, tape_index) pair is one
    /// separately positioned turn carrying the full operating current, so
    /// `ampere_turns = total_turns() * operating_current_a`.
    pub fn total_turns(&self) -> u64 {
        u64::from(self.turns_along_normal) * u64::from(self.tapes_along_width)
    }

    /// The `tape_specs` id covering `turn_index` (1-based along
    /// `turns_along_normal`), or `None` for the case-level `material`.
    /// Only meaningful on a validated case (regions are sorted and
    /// non-overlapping); an uncovered or out-of-range index yields `None`.
    pub fn spec_for_turn(&self, turn_index: u32) -> Option<&str> {
        self.regions.as_ref().and_then(|regions| {
            regions
                .iter()
                .find(|r| r.first_turn <= turn_index && turn_index <= r.last_turn)
                .map(|r| r.tape_spec.as_str())
        })
    }

    /// A tape center's (rho, z) coordinates for 1-based `(tape_index,
    /// turn_index)`, before mapping through a station's `position_m`
    /// (contract A4). `rho` is an absolute radius for a racetrack pack and
    /// a signed centerline offset for a path pack — see `Pack::inner_rho_m`.
    /// Callers must have already validated the case; indices
    /// are not range-checked here since this is a pure geometry helper.
    pub fn tape_radial_axial_m(&self, pack: &Pack, tape_index: u32, turn_index: u32) -> (f64, f64) {
        let pitch_n = self.pitch_n_m(pack);
        let pitch_w = self.pitch_w_m(pack);
        // A geometry-less pack yields NaN here, and `position_m` rejects
        // non-finite coordinates — fail closed, never silently place a
        // tape at the origin.
        let inner_rho = pack.inner_rho_m().unwrap_or(f64::NAN);
        let low_z = -pack.axial_height_m / 2.0;
        match self.tape_normal {
            TapeNormal::Radial => (
                inner_rho + (f64::from(turn_index) - 0.5) * pitch_n,
                low_z + (f64::from(tape_index) - 0.5) * pitch_w,
            ),
            TapeNormal::Axial => (
                inner_rho + (f64::from(tape_index) - 0.5) * pitch_w,
                low_z + (f64::from(turn_index) - 0.5) * pitch_n,
            ),
        }
    }

    pub fn validate(&self, pack: &Pack) -> Result<(), ModelError> {
        require(
            self.tape_width_m.is_finite() && self.tape_width_m > 0.0 && self.tape_width_m <= 1e6,
            "tape_width_m must be finite and positive",
        )?;
        require(
            (1..=MAX_TURNS_ALONG_NORMAL).contains(&self.turns_along_normal),
            "turns_along_normal outside its numerical work limit",
        )?;
        require(
            (1..=MAX_TAPES_ALONG_WIDTH).contains(&self.tapes_along_width),
            "tapes_along_width outside its numerical work limit",
        )?;
        require(
            (1..=MAX_STRANDS_PARALLEL).contains(&self.strands_parallel),
            "strands_parallel outside its numerical work limit",
        )?;
        let extent = self.width_extent_m(pack);
        let fill = f64::from(self.tapes_along_width) * self.tape_width_m;
        require(
            extent.is_finite() && extent > 0.0 && (fill - extent).abs() <= 1e-9 * extent,
            "tapes_along_width * tape_width_m must equal the pack's width extent within 1e-9 relative (contract A1)",
        )?;
        Ok(())
    }
}

/// Physical offset along ŵ for a Gauss-Lobatto abscissa `xi` in `[-1, 1]`
/// (contract A7). The abscissas/weights themselves live in
/// `optcoil_physics::tape_frame`, keeping this crate free of physics.
pub fn width_offset_m(tape_width_m: f64, xi: f64) -> f64 {
    xi * tape_width_m / 2.0
}

/// Convenience combining `Winding::tape_radial_axial_m` and
/// `Station::position_m` for a full `(station, tape_index, turn_index)`
/// tuple (contract A4).
pub fn tape_center_position_m(
    pack: &Pack,
    winding: &Winding,
    station: &Station,
    tape_index: u32,
    turn_index: u32,
) -> Result<[f64; 3], ModelError> {
    let (rho_m, z_m) = winding.tape_radial_axial_m(pack, tape_index, turn_index);
    station.position_m(pack, rho_m, z_m)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WidthQuadrature {
    GaussLobatto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    pub stations: Vec<Station>,
    pub turn_indices_along_normal: Vec<u32>,
    pub tape_indices_along_width: Vec<u32>,
    pub width_quadrature: WidthQuadrature,
    pub width_points: u32,
    /// Schema v7 only: an externally supplied, customer-declared field
    /// map. When present, every sampled point's field comes from this map
    /// (nearest node on its declared grid) instead of the engine's field
    /// solve — the record's `field_map` block carries its provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_map: Option<FieldMap>,
}

/// How a field-map entry's components are expressed.
/// `CylindricalBrBz` — (B_r, B_z) components of an axisymmetric field on
/// a rectilinear (rho_m, z_m) grid, where `rho_m` is the absolute radius
/// from the machine axis and `z_m` the axial coordinate.
/// `CartesianBxByBz` — full lab-frame (B_x, B_y, B_z) components on a
/// rectilinear (x, y, z) grid, for non-axisymmetric windings and general
/// solver exports (coupled schema v8 / search schema v14).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldMapComponents {
    CylindricalBrBz,
    CartesianBxByBz,
}

/// One node of a declared cylindrical field map: the field at
/// `(rho_levels_m[rho_index], z_levels_m[z_index])` in tesla at
/// `reference_ampere_turns_a`. Entries must be sorted by
/// `(rho_index, z_index)` and cover the complete product grid exactly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMapEntry {
    pub rho_index: u32,
    pub z_index: u32,
    pub br_t: f64,
    pub bz_t: f64,
}

/// One node of a declared Cartesian field map: the lab-frame field at
/// `(x_levels_m[x_index], y_levels_m[y_index], z_levels_m[z_index])` in
/// tesla at `reference_ampere_turns_a`. Entries must be sorted by
/// `(x_index, y_index, z_index)` — x outermost — and cover the complete
/// product grid exactly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMapCartesianEntry {
    pub x_index: u32,
    pub y_index: u32,
    pub z_index: u32,
    pub bx_t: f64,
    pub by_t: f64,
    pub bz_t: f64,
}

/// A customer-declared field map replacing the engine's field solve at
/// sampled points. This is the customer-privacy interface: the map is the
/// customer's own field solution (e.g. their FEA export), bound to the
/// record by `source_sha256`, and no geometry or source detail beyond the
/// declared grid enters the case.
///
/// The map is linear: entries are tesla at `reference_ampere_turns_a`
/// total ampere-turns, and the engine scales by candidate ampere-turns
/// exactly as it scales a computed field. Every sampled tape centre is
/// assigned its nearest declared node, so the map's own mesh is the field
/// resolution — there is no in-map detail to refine toward, and the
/// record reports refinement-change evidence as zero with `field_source`
/// set to the declared map.
///
/// The `components` tag selects the grid. `cylindrical_br_bz` maps
/// describe an axisymmetric field and restrict the winding to an
/// origin-centred circular `pack.path`: every segment an arc of the same
/// radius, every arc centre the origin, total sweep +360 deg so the path
/// normal is the outward radial. `cartesian_bx_by_bz` maps carry the
/// full lab-frame field vector and accept any planar `pack.path` — or
/// the racetrack dims, which synthesize one — with confinement checked
/// exactly against the swept pack domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "components", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldMap {
    CylindricalBrBz {
        /// SHA-256 of the source map file the customer produced — the
        /// binding between this case and their field artifact.
        source_sha256: String,
        /// Total ampere-turns the declared field was computed at. For a
        /// smeared-`J` solve this is `J·A_pack`; for a per-ampere-turn
        /// export it is 1.0.
        reference_ampere_turns_a: f64,
        /// Absolute-radius grid nodes (metres), strictly ascending.
        rho_levels_m: Vec<f64>,
        /// Axial grid nodes (metres), strictly ascending.
        z_levels_m: Vec<f64>,
        entries: Vec<FieldMapEntry>,
    },
    CartesianBxByBz {
        source_sha256: String,
        reference_ampere_turns_a: f64,
        /// Lab-frame grid nodes (metres), strictly ascending.
        x_levels_m: Vec<f64>,
        y_levels_m: Vec<f64>,
        z_levels_m: Vec<f64>,
        entries: Vec<FieldMapCartesianEntry>,
    },
}

impl FieldMap {
    /// The declared grid's component convention.
    pub fn components(&self) -> FieldMapComponents {
        match self {
            FieldMap::CylindricalBrBz { .. } => FieldMapComponents::CylindricalBrBz,
            FieldMap::CartesianBxByBz { .. } => FieldMapComponents::CartesianBxByBz,
        }
    }

    /// SHA-256 of the customer-produced source file.
    pub fn source_sha256(&self) -> &str {
        match self {
            FieldMap::CylindricalBrBz { source_sha256, .. }
            | FieldMap::CartesianBxByBz { source_sha256, .. } => source_sha256,
        }
    }

    /// Total ampere-turns the declared field was computed at.
    pub fn reference_ampere_turns_a(&self) -> f64 {
        match self {
            FieldMap::CylindricalBrBz {
                reference_ampere_turns_a,
                ..
            }
            | FieldMap::CartesianBxByBz {
                reference_ampere_turns_a,
                ..
            } => *reference_ampere_turns_a,
        }
    }

    /// Half-spacing-extended rho hull — `Some` for a cylindrical map,
    /// `None` for Cartesian.
    pub fn rho_hull_m(&self) -> Option<(f64, f64)> {
        match self {
            FieldMap::CylindricalBrBz { rho_levels_m, .. } => Some(hull(rho_levels_m)),
            FieldMap::CartesianBxByBz { .. } => None,
        }
    }

    /// Half-spacing-extended z hull — both grids carry a z axis.
    pub fn z_hull_m(&self) -> (f64, f64) {
        match self {
            FieldMap::CylindricalBrBz { z_levels_m, .. }
            | FieldMap::CartesianBxByBz { z_levels_m, .. } => hull(z_levels_m),
        }
    }

    /// Half-spacing-extended x/y hulls — `Some` for a Cartesian map.
    pub fn x_hull_m(&self) -> Option<(f64, f64)> {
        match self {
            FieldMap::CartesianBxByBz { x_levels_m, .. } => Some(hull(x_levels_m)),
            FieldMap::CylindricalBrBz { .. } => None,
        }
    }

    pub fn y_hull_m(&self) -> Option<(f64, f64)> {
        match self {
            FieldMap::CartesianBxByBz { y_levels_m, .. } => Some(hull(y_levels_m)),
            FieldMap::CylindricalBrBz { .. } => None,
        }
    }

    /// Build the lookup structure. Requires a validated map (complete,
    /// sorted grid); `Err` on any inconsistency — never a silent lookup.
    pub fn resolve(&self) -> Result<ResolvedFieldMap, ModelError> {
        match self {
            FieldMap::CylindricalBrBz {
                reference_ampere_turns_a,
                rho_levels_m,
                z_levels_m,
                entries,
                ..
            } => {
                let n_r = rho_levels_m.len();
                let n_z = z_levels_m.len();
                require(
                    entries.len() == n_r * n_z,
                    "field_map entries must fill the rho x z product grid exactly",
                )?;
                let mut field = vec![(0.0, 0.0); n_r * n_z];
                for (k, e) in entries.iter().enumerate() {
                    require(
                        e.rho_index as usize == k / n_z && e.z_index as usize == k % n_z,
                        "field_map entries must be sorted by (rho_index, z_index) covering the full grid",
                    )?;
                    require(
                        e.br_t.is_finite() && e.bz_t.is_finite(),
                        "field_map entry fields must be finite",
                    )?;
                    field[k] = (e.br_t, e.bz_t);
                }
                Ok(ResolvedFieldMap::CylindricalBrBz(
                    ResolvedCylindricalFieldMap {
                        reference_ampere_turns_a: *reference_ampere_turns_a,
                        rho_levels_m: rho_levels_m.clone(),
                        z_levels_m: z_levels_m.clone(),
                        field,
                    },
                ))
            }
            FieldMap::CartesianBxByBz {
                reference_ampere_turns_a,
                x_levels_m,
                y_levels_m,
                z_levels_m,
                entries,
                ..
            } => {
                let n_x = x_levels_m.len();
                let n_y = y_levels_m.len();
                let n_z = z_levels_m.len();
                require(
                    entries.len() == n_x * n_y * n_z,
                    "field_map entries must fill the x x y x z product grid exactly",
                )?;
                let mut field = vec![(0.0, 0.0, 0.0); n_x * n_y * n_z];
                for (k, e) in entries.iter().enumerate() {
                    require(
                        e.x_index as usize == k / (n_y * n_z)
                            && e.y_index as usize == (k / n_z) % n_y
                            && e.z_index as usize == k % n_z,
                        "field_map entries must be sorted by (x_index, y_index, z_index) covering the full grid",
                    )?;
                    require(
                        e.bx_t.is_finite() && e.by_t.is_finite() && e.bz_t.is_finite(),
                        "field_map entry fields must be finite",
                    )?;
                    field[k] = (e.bx_t, e.by_t, e.bz_t);
                }
                Ok(ResolvedFieldMap::CartesianBxByBz(
                    ResolvedCartesianFieldMap {
                        reference_ampere_turns_a: *reference_ampere_turns_a,
                        x_levels_m: x_levels_m.clone(),
                        y_levels_m: y_levels_m.clone(),
                        z_levels_m: z_levels_m.clone(),
                        field,
                    },
                ))
            }
        }
    }
}

/// A `FieldMap` resolved for lookups: validated once, then nearest-node
/// answers in O(log n) per axis. Built by `FieldMap::resolve` after
/// `validate`.
#[derive(Debug, Clone)]
pub enum ResolvedFieldMap {
    CylindricalBrBz(ResolvedCylindricalFieldMap),
    CartesianBxByBz(ResolvedCartesianFieldMap),
}

/// Resolved cylindrical (rho, z) map — `field[rho_index * n_z +
/// z_index]` = (B_r, B_z).
#[derive(Debug, Clone)]
pub struct ResolvedCylindricalFieldMap {
    pub reference_ampere_turns_a: f64,
    rho_levels_m: Vec<f64>,
    z_levels_m: Vec<f64>,
    field: Vec<(f64, f64)>,
}

/// Resolved Cartesian (x, y, z) map — `field[(x_index * n_y + y_index) *
/// n_z + z_index]` = (B_x, B_y, B_z).
#[derive(Debug, Clone)]
pub struct ResolvedCartesianFieldMap {
    pub reference_ampere_turns_a: f64,
    x_levels_m: Vec<f64>,
    y_levels_m: Vec<f64>,
    z_levels_m: Vec<f64>,
    field: Vec<(f64, f64, f64)>,
}

/// `[first - half_first_gap, last + half_last_gap]` — the nearest-node
/// hull of a strictly-ascending level list read as cell centres.
fn hull(levels: &[f64]) -> (f64, f64) {
    let first = levels[0];
    let last = levels[levels.len() - 1];
    let lo = if levels.len() > 1 {
        first - (levels[1] - first) / 2.0
    } else {
        first
    };
    let hi = if levels.len() > 1 {
        last + (last - levels[levels.len() - 2]) / 2.0
    } else {
        last
    };
    (lo, hi)
}

impl ResolvedCylindricalFieldMap {
    /// (B_r, B_z) at the grid node nearest `(rho_m, z_m)` — the map's own
    /// mesh is the field resolution. Callers must have confined the query
    /// to the declared hull (case validation guarantees this for pack
    /// tape centres).
    pub fn nearest(&self, rho_m: f64, z_m: f64) -> (f64, f64) {
        let i = nearest_index(&self.rho_levels_m, rho_m);
        let j = nearest_index(&self.z_levels_m, z_m);
        self.field[i * self.z_levels_m.len() + j]
    }

    /// The half-spacing-extended nearest-node hull — the region queries
    /// must stay inside. A query outside it is out of domain: the caller
    /// must fail closed, never extrapolate.
    pub fn rho_hull_m(&self) -> (f64, f64) {
        hull(&self.rho_levels_m)
    }

    pub fn z_hull_m(&self) -> (f64, f64) {
        hull(&self.z_levels_m)
    }
}

impl ResolvedCartesianFieldMap {
    /// (B_x, B_y, B_z) at the grid node nearest `(x_m, y_m, z_m)`.
    pub fn nearest(&self, x_m: f64, y_m: f64, z_m: f64) -> (f64, f64, f64) {
        let i = nearest_index(&self.x_levels_m, x_m);
        let j = nearest_index(&self.y_levels_m, y_m);
        let k = nearest_index(&self.z_levels_m, z_m);
        self.field[(i * self.y_levels_m.len() + j) * self.z_levels_m.len() + k]
    }

    pub fn x_hull_m(&self) -> (f64, f64) {
        hull(&self.x_levels_m)
    }

    pub fn y_hull_m(&self) -> (f64, f64) {
        hull(&self.y_levels_m)
    }

    pub fn z_hull_m(&self) -> (f64, f64) {
        hull(&self.z_levels_m)
    }
}

impl ResolvedFieldMap {
    /// Total ampere-turns the declared field was computed at.
    pub fn reference_ampere_turns_a(&self) -> f64 {
        match self {
            ResolvedFieldMap::CylindricalBrBz(m) => m.reference_ampere_turns_a,
            ResolvedFieldMap::CartesianBxByBz(m) => m.reference_ampere_turns_a,
        }
    }

    /// Peak declared field magnitude over the map's own nodes, per
    /// ampere-turn of the reference winding — the worst conductor-field
    /// proxy a declared map can honestly offer. A read of declared data,
    /// not an engine evaluation.
    pub fn peak_unit_field_t_per_at(&self) -> f64 {
        let peak = match self {
            ResolvedFieldMap::CylindricalBrBz(m) => m
                .field
                .iter()
                .map(|(br, bz)| br.hypot(*bz))
                .fold(0.0_f64, f64::max),
            ResolvedFieldMap::CartesianBxByBz(m) => m
                .field
                .iter()
                .map(|(bx, by, bz)| (bx * bx + by * by + bz * bz).sqrt())
                .fold(0.0_f64, f64::max),
        };
        peak / self.reference_ampere_turns_a()
    }

    /// Unit field (tesla per ampere-turn of the reference winding) at a
    /// lab-frame position. `Err` outside the declared hull — never an
    /// extrapolated field.
    pub fn unit_field_lab_t_per_ampere_turn(
        &self,
        position_m: [f64; 3],
    ) -> Result<[f64; 3], ModelError> {
        match self {
            // The point's cylindrical `(r, z)` addresses its nearest
            // declared node and `B_r` maps to the lab x/y plane along the
            // point's own azimuth.
            ResolvedFieldMap::CylindricalBrBz(map) => {
                let (x, y, z) = (position_m[0], position_m[1], position_m[2]);
                let r = (x * x + y * y).sqrt();
                let (r_lo, r_hi) = map.rho_hull_m();
                let (z_lo, z_hi) = map.z_hull_m();
                let tol = 1e-9;
                require(
                    r >= r_lo - tol && r <= r_hi + tol && z >= z_lo - tol && z <= z_hi + tol,
                    &format!(
                        "field_map query at r={r:.6} m, z={z:.6} m lies outside the declared hull r=[{r_lo:.6},{r_hi:.6}] z=[{z_lo:.6},{z_hi:.6}]"
                    ),
                )?;
                let (br, bz) = map.nearest(r, z);
                let inv_at = 1.0 / map.reference_ampere_turns_a;
                let (rx, ry) = if r > 0.0 { (x / r, y / r) } else { (0.0, 0.0) };
                Ok([br * rx * inv_at, br * ry * inv_at, bz * inv_at])
            }
            // The node values are already lab-frame components.
            ResolvedFieldMap::CartesianBxByBz(map) => {
                let (x, y, z) = (position_m[0], position_m[1], position_m[2]);
                let (x_lo, x_hi) = map.x_hull_m();
                let (y_lo, y_hi) = map.y_hull_m();
                let (z_lo, z_hi) = map.z_hull_m();
                let tol = 1e-9;
                require(
                    x >= x_lo - tol
                        && x <= x_hi + tol
                        && y >= y_lo - tol
                        && y <= y_hi + tol
                        && z >= z_lo - tol
                        && z <= z_hi + tol,
                    &format!(
                        "field_map query at x={x:.6} m, y={y:.6} m, z={z:.6} m lies outside the declared hull x=[{x_lo:.6},{x_hi:.6}] y=[{y_lo:.6},{y_hi:.6}] z=[{z_lo:.6},{z_hi:.6}]"
                    ),
                )?;
                let (bx, by, bz) = map.nearest(x, y, z);
                let inv_at = 1.0 / map.reference_ampere_turns_a;
                Ok([bx * inv_at, by * inv_at, bz * inv_at])
            }
        }
    }
}

fn nearest_index(levels: &[f64], x: f64) -> usize {
    // First index whose level is >= x; compare against predecessor.
    let up = levels.partition_point(|&v| v < x);
    if up == 0 {
        0
    } else if up >= levels.len() {
        levels.len() - 1
    } else if x - levels[up - 1] <= levels[up] - x {
        up - 1
    } else {
        up
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_along_current_field_fraction: f64,
    pub max_self_field_ratio: f64,
    pub interpolation_overprediction_budget: f64,
    pub utilization_limit: f64,
    /// OC-014 self-field correction model. Absent (or None): the query
    /// magnitude is the applied field alone, and the `max_self_field_ratio`
    /// gate applies as before. `"uniform_transport"`: the query magnitude
    /// is `|B_applied| + μ0·K_tape/2` — the conservative uniform-sheet
    /// bound (higher field → lower Ic) — and the INCONCLUSIVE gate moves
    /// to the physical dominance boundary (ratio > 1, where the
    /// uniform-K bound stops being a bound: critical-state edge
    /// crowding). Coupled schema v2 / search schema v5 only.
    #[serde(default)]
    pub self_field_correction: Option<String>,
    /// OC-017 along-current model. Absent/`None`: a point whose
    /// along-current field fraction exceeds
    /// `max_along_current_field_fraction` is excluded (INCONCLUSIVE) — the
    /// measured law only covers maximum-Lorentz geometry. `"transverse_bound"`:
    /// fractions in `(limit, ALONG_CURRENT_BOUND_CEILING]` still query the
    /// measured law at the full field magnitude and the transverse-plane
    /// angle, marked `along_current_bounded` in the record — the declared
    /// assumption is that an along-current component degrades Ic no more
    /// than an equal transverse component (vortex density fully counted,
    /// depinning drive fully counted; the real tape keeps only the
    /// perpendicular-to-J drive, so the bound direction is conservative).
    /// Above the ceiling the point is still excluded.
    /// Coupled schema v3 / search schema v6 only.
    #[serde(default)]
    pub along_current_model: Option<String>,
    /// OC-014 Phase 2: declared current-carrying (REBCO) layer thickness
    /// in metres — the finite-thickness standoff that regularizes the
    /// `critical_state_strip` edge self-field bound. Required iff
    /// `self_field_correction == "critical_state_strip"`; must be absent
    /// otherwise. Not a measured value — a declared modeling constant
    /// (~1–4 µm for a production REBCO film; smaller is more
    /// conservative, since the bound's ln(2w/d) grows as d shrinks).
    /// Coupled schema v4 / search schema v8 only.
    #[serde(default)]
    pub critical_state_layer_thickness_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSubsetEntry {
    pub station: String,
    pub tape_index: u32,
    pub turn_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Numerics {
    pub quadrature_orders: [u32; 2],
    pub field_scale_t: f64,
    pub max_refinement_change_fraction: f64,
    pub max_reference_refinement_fraction: f64,
    pub max_reference_field_error_fraction: f64,
    pub max_reference_angle_error_deg: f64,
    pub max_reference_capacity_relative_error: f64,
    pub reference_subset: Vec<ReferenceSubsetEntry>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WidthTransferBasis {
    #[serde(rename = "none")]
    NoTransfer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WidthTransfer {
    pub basis: WidthTransferBasis,
}

impl Operating {
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.temperature_k.is_finite()
                && self.temperature_k > 0.0
                && self.temperature_k <= 400.0,
            "operating temperature_k must be finite, positive and at most 400 K",
        )?;
        require(
            (1..=MAX_CANDIDATES).contains(&self.current_candidates_a.len()),
            "expected 1..=8 current candidates",
        )?;
        require(
            self.current_candidates_a
                .iter()
                .all(|i| i.is_finite() && *i > 0.0),
            "current candidates must be finite and positive",
        )?;
        require(
            self.current_candidates_a.windows(2).all(|w| w[0] < w[1]),
            "current candidates must be strictly increasing",
        )?;
        require(
            self.electric_field_criterion_v_per_m.is_finite()
                && self.electric_field_criterion_v_per_m > 0.0,
            "electric_field_criterion_v_per_m must be finite and positive",
        )?;
        Ok(())
    }
}

impl MaterialSettings {
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            !self.dataset_id.trim().is_empty(),
            "material dataset_id is empty",
        )?;
        require(
            is_lowercase_sha256(&self.csv_sha256),
            "material csv_sha256 must be a lowercase SHA-256",
        )?;
        require(!self.method.trim().is_empty(), "material method is empty")?;
        require(
            self.low_field_clamp_t.is_finite() && self.low_field_clamp_t > 0.0,
            "low_field_clamp_t must be finite and positive",
        )?;
        require(
            self.monotonicity_tolerance.is_finite()
                && self.monotonicity_tolerance > 0.0
                && self.monotonicity_tolerance < 1.0,
            "monotonicity_tolerance must be a finite fraction in (0, 1)",
        )?;
        Ok(())
    }
}

impl Sampling {
    pub fn validate(&self, pack: &Pack, winding: &Winding) -> Result<(), ModelError> {
        require(
            (1..=MAX_STATIONS).contains(&self.stations.len()),
            "expected 1..=16 stations",
        )?;
        let mut ids = HashSet::new();
        for station in &self.stations {
            station.validate(pack)?;
            require(ids.insert(station.id()), "duplicate station id")?;
        }
        validate_index_plan(
            &self.turn_indices_along_normal,
            winding.turns_along_normal,
            "turn_indices_along_normal",
            MAX_SAMPLED_INDICES,
        )?;
        validate_index_plan(
            &self.tape_indices_along_width,
            winding.tapes_along_width,
            "tape_indices_along_width",
            MAX_SAMPLED_TAPE_INDICES,
        )?;
        match self.width_quadrature {
            WidthQuadrature::GaussLobatto => require(
                self.width_points == 5,
                "width_points must equal 5 for gauss_lobatto in v1",
            )?,
        }
        if let Some(map) = &self.field_map {
            map.validate(pack)?;
        }
        Ok(())
    }
}

const MAX_FIELD_MAP_LEVELS: usize = 4096;
const MAX_FIELD_MAP_ENTRIES: usize = 1_048_576;

/// The lab-frame corner points that attain the swept pack domain's
/// axis-aligned bounding box: each segment's entry and exit pose plus,
/// for arcs, the poses at the cardinal angles crossed by the sweep
/// (where the radial direction aligns with ±x or ±y). The Cartesian hull
/// is a convex box, so checking these corners is exact confinement of
/// `{pose + u·n̂ + v·ẑ : |u| ≤ half_w, |v| ≤ half_h}`, not a sampling
/// heuristic.
fn push_pose_corners(
    points: &mut Vec<[f64; 3]>,
    pos: [f64; 3],
    normal: [f64; 3],
    half_width_m: f64,
    half_height_m: f64,
) {
    for su in [-1.0_f64, 1.0] {
        for sv in [-1.0_f64, 1.0] {
            points.push([
                pos[0] + su * half_width_m * normal[0],
                pos[1] + su * half_width_m * normal[1],
                pos[2] + sv * half_height_m,
            ]);
        }
    }
}

fn pack_domain_test_points(
    centerline: Centerline,
    half_width_m: f64,
    half_height_m: f64,
) -> Result<Vec<[f64; 3]>, ModelError> {
    match centerline {
        // Schema v9: the helix pack's own extremum construction — the
        // corner curves' axis extrema in the rotating (r̂, φ̂) basis.
        Centerline::Helical(path) => path.domain_test_points(half_width_m, half_height_m),
        Centerline::Planar(path) => {
            let frames = path.segment_frames()?;
            let mut points = Vec::new();
            for frame in &frames {
                let entry = frame.entry;
                push_pose_corners(
                    &mut points,
                    entry.position_m,
                    entry.normal,
                    half_width_m,
                    half_height_m,
                );
                match frame.segment {
                    crate::path::PathSegment::Line { length_m } => {
                        let exit = [
                            entry.position_m[0] + length_m * entry.tangent[0],
                            entry.position_m[1] + length_m * entry.tangent[1],
                            entry.position_m[2] + length_m * entry.tangent[2],
                        ];
                        push_pose_corners(
                            &mut points,
                            exit,
                            entry.normal,
                            half_width_m,
                            half_height_m,
                        );
                    }
                    crate::path::PathSegment::Arc {
                        radius_m,
                        sweep_deg,
                    } => {
                        // On an arc the normal is the radial direction, so a
                        // corner at sweep angle θ sits at centre + (R ± half_w)·
                        // r̂(θ): an annular sector. Its axis extrema occur at the
                        // sweep endpoints and the cardinal angles it crosses.
                        let (cx, cy) = (frame.center_m[0], frame.center_m[1]);
                        let theta0 = (entry.position_m[1] - cy).atan2(entry.position_m[0] - cx);
                        let delta = sweep_deg.to_radians();
                        let mut thetas = vec![theta0, theta0 + delta];
                        let (lo, hi) = if delta >= 0.0 {
                            (theta0, theta0 + delta)
                        } else {
                            (theta0 + delta, theta0)
                        };
                        let quarter = std::f64::consts::FRAC_PI_2;
                        let k_lo = (lo / quarter).floor() as i64 + 1;
                        let k_hi = (hi / quarter).ceil() as i64 - 1;
                        for k in k_lo..=k_hi {
                            thetas.push(k as f64 * quarter);
                        }
                        for &theta in &thetas {
                            let (sin, cos) = theta.sin_cos();
                            for &su in &[-1.0_f64, 1.0] {
                                for &sv in &[-1.0_f64, 1.0] {
                                    points.push([
                                        cx + (radius_m + su * half_width_m) * cos,
                                        cy + (radius_m + su * half_width_m) * sin,
                                        sv * half_height_m,
                                    ]);
                                }
                            }
                        }
                    }
                }
            }
            Ok(points)
        }
    }
}

impl FieldMap {
    /// Structural + geometric validation: a well-formed grid whose hull
    /// contains the pack's swept domain. For a cylindrical map that
    /// domain is an origin-centred circular path; a Cartesian map takes
    /// any planar `pack.path`, the racetrack dims synthesized into one,
    /// or a non-planar `pack.path3d` (schema v9). Schema gating (v7/v8/v9)
    /// lives in `CoupledCase::validate`.
    pub fn validate(&self, pack: &Pack) -> Result<(), ModelError> {
        // A Cartesian map can cover a racetrack pack: synthesize the
        // equivalent path from the declared racetrack dims.
        let synthesized;
        let centerline = match pack.centerline() {
            Some(centerline) => Some(centerline),
            None => match (self, pack.straight_half_length_m, pack.bend_radius_m) {
                (FieldMap::CartesianBxByBz { .. }, Some(half_length), Some(bend)) => {
                    synthesized = crate::path::CoilPath::racetrack(half_length, bend);
                    Some(Centerline::Planar(&synthesized))
                }
                _ => None,
            },
        };
        self.validate_against_extents(centerline, pack.radial_width_m, pack.axial_height_m)
    }

    /// The same validation against explicitly declared extents — the
    /// coupled-search fixed-extents path (schema v13) declares pack
    /// dimensions that differ per generated case only in discretization,
    /// so the map is checked against the declared extents themselves.
    /// `centerline` may be `None` to fail closed identically to a missing
    /// centerline; a non-planar `Centerline::Helical` is supported by the
    /// Cartesian component only (a cylindrical map cannot describe a
    /// non-axisymmetric winding's field).
    pub fn validate_against_extents(
        &self,
        centerline: Option<Centerline>,
        radial_width_m: f64,
        axial_height_m: f64,
    ) -> Result<(), ModelError> {
        require(
            self.source_sha256().len() == 64
                && self
                    .source_sha256()
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "field_map source_sha256 must be a lowercase SHA-256",
        )?;
        require(
            self.reference_ampere_turns_a().is_finite() && self.reference_ampere_turns_a() > 0.0,
            "field_map reference_ampere_turns_a must be finite and positive",
        )?;
        match self {
            FieldMap::CylindricalBrBz {
                rho_levels_m,
                z_levels_m,
                entries,
                ..
            } => {
                for (levels, name) in [(rho_levels_m, "rho_levels_m"), (z_levels_m, "z_levels_m")] {
                    require(
                        (1..=MAX_FIELD_MAP_LEVELS).contains(&levels.len()),
                        &format!("field_map {name} must declare 1..={MAX_FIELD_MAP_LEVELS} nodes"),
                    )?;
                    require(
                        levels.iter().all(|v| v.is_finite()),
                        &format!("field_map {name} must be finite"),
                    )?;
                    require(
                        levels.windows(2).all(|w| w[0] < w[1]),
                        &format!("field_map {name} must be strictly ascending"),
                    )?;
                }
                require(
                    rho_levels_m.iter().all(|&r| r >= 0.0),
                    "field_map rho_levels_m are absolute radii and must be >= 0",
                )?;
                require(
                    entries.len() <= MAX_FIELD_MAP_ENTRIES,
                    "field_map entries exceed the v1 cap",
                )?;
                // Completeness + ordering + finiteness of the grid entries.
                self.resolve()?;

                // Cylindrical maps describe an axisymmetric field; the
                // only supported centerline is an origin-centred circle
                // (every arc same radius, centres at the origin, total
                // sweep +360 deg so the path normal is outward-radial).
                // A non-planar helix is not axisymmetric — its field is
                // not (B_r, B_z)-describable — so it cannot be checked
                // against this component at all.
                let path = match centerline {
                    Some(Centerline::Planar(path)) => path,
                    Some(Centerline::Helical(_)) => {
                        return Err(ModelError::Invalid(
                            "cylindrical field_map cannot describe a non-planar pack.path3d winding — declare a cartesian_bx_by_bz map".into(),
                        ));
                    }
                    None => {
                        return Err(ModelError::Invalid(
                            "field_map requires a pack.path circle".into(),
                        ));
                    }
                };
                let frames = path.segment_frames()?;
                let mut radius: Option<f64> = None;
                let mut total_sweep_deg = 0.0;
                for frame in &frames {
                    let (r, sweep) = match frame.segment {
                        crate::path::PathSegment::Arc {
                            radius_m,
                            sweep_deg,
                        } => (radius_m, sweep_deg),
                        crate::path::PathSegment::Line { .. } => {
                            return Err(ModelError::Invalid(
                                "field_map v1 requires every path segment to be an arc".into(),
                            ));
                        }
                    };
                    match radius {
                        None => radius = Some(r),
                        Some(r0) => require(
                            (r - r0).abs() <= 1e-9 * r0.max(1.0),
                            "field_map v1 requires a single-radius circular path",
                        )?,
                    }
                    let c2 = frame.center_m[0] * frame.center_m[0]
                        + frame.center_m[1] * frame.center_m[1];
                    require(
                        c2 <= (1e-9 * r.max(1.0)).powi(2),
                        "field_map v1 requires every arc centre at the origin",
                    )?;
                    total_sweep_deg += sweep;
                }
                require(
                    (total_sweep_deg - 360.0).abs() < 1e-6,
                    "field_map v1 requires a +360 deg (CCW) circular path so the normal is outward-radial",
                )?;
                let radius = radius.expect("validated non-empty path");

                // Containment: every sampled tape centre lives inside the
                // nearest-node hull — pack rho extent [R - w/2, R + w/2]
                // and z extent [-h/2, h/2] for a path pack.
                let (r_lo, r_hi) = hull(rho_levels_m);
                let (z_lo, z_hi) = hull(z_levels_m);
                let half_w = radial_width_m / 2.0;
                let half_h = axial_height_m / 2.0;
                require(
                    radius - half_w >= r_lo - 1e-12 && radius + half_w <= r_hi + 1e-12,
                    "field_map rho hull must contain the pack's radial extent",
                )?;
                require(
                    -half_h >= z_lo - 1e-12 && half_h <= z_hi + 1e-12,
                    "field_map z hull must contain the pack's axial extent",
                )?;
            }
            FieldMap::CartesianBxByBz {
                x_levels_m,
                y_levels_m,
                z_levels_m,
                entries,
                ..
            } => {
                for (levels, name) in [
                    (x_levels_m, "x_levels_m"),
                    (y_levels_m, "y_levels_m"),
                    (z_levels_m, "z_levels_m"),
                ] {
                    require(
                        (1..=MAX_FIELD_MAP_LEVELS).contains(&levels.len()),
                        &format!("field_map {name} must declare 1..={MAX_FIELD_MAP_LEVELS} nodes"),
                    )?;
                    require(
                        levels.iter().all(|v| v.is_finite()),
                        &format!("field_map {name} must be finite"),
                    )?;
                    require(
                        levels.windows(2).all(|w| w[0] < w[1]),
                        &format!("field_map {name} must be strictly ascending"),
                    )?;
                }
                require(
                    entries.len() <= MAX_FIELD_MAP_ENTRIES,
                    "field_map entries exceed the cap",
                )?;
                self.resolve()?;

                // Confinement: the swept pack domain `{pose + u·n_ref +
                // v·w_ref}` must lie inside the nearest-node hull box —
                // checked exactly at the domain's own extremal corners.
                // The same rule on a helix pack: `w_ref` is the
                // in-surface binormal instead of `ẑ`, and the corner
                // extrema come from `CoilPath3D::domain_test_points`.
                let centerline = centerline.ok_or_else(|| {
                    ModelError::Invalid(
                        "cartesian field_map requires a declared pack.path or pack.path3d (or racetrack dims)"
                            .into(),
                    )
                })?;
                let (x_lo, x_hi) = hull(x_levels_m);
                let (y_lo, y_hi) = hull(y_levels_m);
                let (z_lo, z_hi) = hull(z_levels_m);
                for p in
                    pack_domain_test_points(centerline, radial_width_m / 2.0, axial_height_m / 2.0)?
                {
                    require(
                        p[0] >= x_lo - 1e-12
                            && p[0] <= x_hi + 1e-12
                            && p[1] >= y_lo - 1e-12
                            && p[1] <= y_hi + 1e-12
                            && p[2] >= z_lo - 1e-12
                            && p[2] <= z_hi + 1e-12,
                        &format!(
                            "field_map cartesian hull x=[{x_lo:.6},{x_hi:.6}] y=[{y_lo:.6},{y_hi:.6}] z=[{z_lo:.6},{z_hi:.6}] must contain the swept pack domain (corner at x={:.6}, y={:.6}, z={:.6})",
                            p[0], p[1], p[2]
                        ),
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn validate_index_plan(
    indices: &[u32],
    max: u32,
    name: &str,
    cap: usize,
) -> Result<(), ModelError> {
    require(
        (1..=cap).contains(&indices.len()),
        &format!("{name} must list 1..={cap} indices"),
    )?;
    require(
        indices.windows(2).all(|w| w[0] < w[1]),
        &format!("{name} must be strictly ascending (which also implies uniqueness)"),
    )?;
    require(
        indices.iter().all(|&i| i >= 1 && i <= max),
        &format!("{name} entries must be 1-based and within the winding's declared count"),
    )?;
    Ok(())
}

impl Limits {
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.max_along_current_field_fraction.is_finite()
                && self.max_along_current_field_fraction > 0.0,
            "max_along_current_field_fraction must be finite and positive",
        )?;
        require(
            self.max_self_field_ratio.is_finite() && self.max_self_field_ratio > 0.0,
            "max_self_field_ratio must be finite and positive",
        )?;
        require(
            self.interpolation_overprediction_budget.is_finite()
                && self.interpolation_overprediction_budget > 0.0
                && self.interpolation_overprediction_budget < 1.0,
            "interpolation_overprediction_budget must lie strictly within (0, 1)",
        )?;
        require(
            self.utilization_limit.is_finite()
                && self.utilization_limit > 0.0
                && self.utilization_limit < 1.0,
            "utilization_limit must lie strictly within (0, 1)",
        )?;
        if let Some(model) = &self.self_field_correction {
            require(
                model == "uniform_transport" || model == "critical_state_strip",
                "self_field_correction must be \"uniform_transport\" or \"critical_state_strip\"",
            )?;
        }
        if let Some(model) = &self.along_current_model {
            require(
                model == "transverse_bound",
                "along_current_model must be \"transverse_bound\" (the only implemented model)",
            )?;
        }
        // OC-014 Phase 2: `critical_state_strip` needs its declared
        // current-layer thickness; every other correction state forbids it.
        let critical_state = self.self_field_correction.as_deref() == Some("critical_state_strip");
        require(
            critical_state == self.critical_state_layer_thickness_m.is_some(),
            "critical_state_layer_thickness_m is required iff \
             self_field_correction == \"critical_state_strip\"",
        )?;
        if let Some(d) = self.critical_state_layer_thickness_m {
            require(
                d.is_finite() && d > 0.0,
                "critical_state_layer_thickness_m must be finite and positive",
            )?;
        }
        Ok(())
    }
}

impl Numerics {
    pub fn validate(&self, sampling: &Sampling) -> Result<(), ModelError> {
        require(
            self.quadrature_orders[0] < self.quadrature_orders[1],
            "quadrature_orders must be strictly increasing",
        )?;
        require(
            self.quadrature_orders
                .iter()
                .all(|&o| (2..=24).contains(&o)),
            "quadrature_orders must lie within 2..=24",
        )?;
        require(
            self.field_scale_t.is_finite() && self.field_scale_t > 0.0,
            "field_scale_t must be finite and positive",
        )?;
        for (value, name) in [
            (
                self.max_refinement_change_fraction,
                "max_refinement_change_fraction",
            ),
            (
                self.max_reference_refinement_fraction,
                "max_reference_refinement_fraction",
            ),
            (
                self.max_reference_field_error_fraction,
                "max_reference_field_error_fraction",
            ),
            (
                self.max_reference_capacity_relative_error,
                "max_reference_capacity_relative_error",
            ),
        ] {
            require(
                value.is_finite() && value > 0.0 && value < 1.0,
                &format!("{name} must be a finite fraction in (0, 1)"),
            )?;
        }
        require(
            self.max_reference_angle_error_deg.is_finite()
                && self.max_reference_angle_error_deg > 0.0
                && self.max_reference_angle_error_deg < 180.0,
            "max_reference_angle_error_deg must be finite and within (0, 180)",
        )?;
        require(
            !self.reference_subset.is_empty(),
            "reference_subset must not be empty",
        )?;
        let mut seen = HashSet::new();
        for entry in &self.reference_subset {
            require(
                sampling.stations.iter().any(|s| s.id() == entry.station),
                "reference_subset station is not part of the sampling plan",
            )?;
            require(
                sampling
                    .tape_indices_along_width
                    .contains(&entry.tape_index),
                "reference_subset tape_index is not part of the sampling plan",
            )?;
            require(
                sampling
                    .turn_indices_along_normal
                    .contains(&entry.turn_index),
                "reference_subset turn_index is not part of the sampling plan",
            )?;
            require(
                seen.insert((entry.station.clone(), entry.tape_index, entry.turn_index)),
                "duplicate reference_subset entry",
            )?;
        }
        Ok(())
    }
}

impl CoupledCase {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let case: Self = crate::encoding::parse_case(json)?;
        case.validate()?;
        Ok(case)
    }

    pub fn embedded() -> Result<Self, ModelError> {
        Self::from_json(OC004_JSON)
    }

    /// Structural validation that does not require a loaded material
    /// dataset. Dataset-dependent identity checks live in
    /// `validate_against_dataset`.
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == COUPLED_CASE_SCHEMA
                || self.schema == COUPLED_CASE_SCHEMA_V2
                || self.schema == COUPLED_CASE_SCHEMA_V3
                || self.schema == COUPLED_CASE_SCHEMA_V4
                || self.schema == COUPLED_CASE_SCHEMA_V5
                || self.schema == COUPLED_CASE_SCHEMA_V6
                || self.schema == COUPLED_CASE_SCHEMA_V7
                || self.schema == COUPLED_CASE_SCHEMA_V8
                || self.schema == COUPLED_CASE_SCHEMA_V9,
            "unsupported coupled conductor schema",
        )?;
        let schema_at_least_v5 = self.schema == COUPLED_CASE_SCHEMA_V5
            || self.schema == COUPLED_CASE_SCHEMA_V6
            || self.schema == COUPLED_CASE_SCHEMA_V7
            || self.schema == COUPLED_CASE_SCHEMA_V8
            || self.schema == COUPLED_CASE_SCHEMA_V9;
        require(
            schema_at_least_v5 || self.pack.path.is_none(),
            "schema v1-v4 coupled cases may not declare pack.path",
        )?;
        // Schema v9: non-planar helix centerline — the v9-only gate plus
        // the two constraints the machinery genuinely needs: the radial
        // tape normal (the only stacking a helical conductor has) and a
        // declared field map (the engine's pack self-field evaluator is
        // planar).
        let schema_at_least_v9 = self.schema == COUPLED_CASE_SCHEMA_V9;
        require(
            schema_at_least_v9 || self.pack.path3d.is_none(),
            "schema v1-v8 coupled cases may not declare pack.path3d",
        )?;
        if self.pack.path3d.is_some() {
            require(
                self.winding.tape_normal == TapeNormal::Radial,
                "pack.path3d requires tape_normal radial — the cylinder-radial \
                 stacking direction a helical conductor winds in (axial tape \
                 normal on a non-planar path is not modeled)",
            )?;
            require(
                self.sampling.field_map.is_some(),
                "pack.path3d requires sampling.field_map — non-planar packs \
                 evaluate under a declared Cartesian field map only; the \
                 engine's pack self-field evaluator is planar",
            )?;
        }
        let schema_at_least_v7 = self.schema == COUPLED_CASE_SCHEMA_V7
            || self.schema == COUPLED_CASE_SCHEMA_V8
            || schema_at_least_v9;
        require(
            schema_at_least_v7 || self.sampling.field_map.is_none(),
            "schema v1-v6 coupled cases may not declare sampling.field_map",
        )?;
        require(
            self.schema == COUPLED_CASE_SCHEMA_V8
                || schema_at_least_v9
                || self.sampling.field_map.as_ref().map(|m| m.components())
                    != Some(FieldMapComponents::CartesianBxByBz),
            "field_map components cartesian_bx_by_bz requires coupled schema v8",
        )?;
        require(
            self.schema != COUPLED_CASE_SCHEMA || self.limits.self_field_correction.is_none(),
            "schema v1 coupled cases may not declare limits.self_field_correction",
        )?;
        let schema_at_least_v3 = self.schema == COUPLED_CASE_SCHEMA_V3
            || self.schema == COUPLED_CASE_SCHEMA_V4
            || schema_at_least_v5;
        let schema_at_least_v4 = self.schema == COUPLED_CASE_SCHEMA_V4 || schema_at_least_v5;
        require(
            schema_at_least_v3 || self.limits.along_current_model.is_none(),
            "schema v1-v2 coupled cases may not declare limits.along_current_model",
        )?;
        let declares_critical_state = self.limits.self_field_correction.as_deref()
            == Some("critical_state_strip")
            || self.limits.critical_state_layer_thickness_m.is_some();
        require(
            schema_at_least_v4 || !declares_critical_state,
            "schema v1-v3 coupled cases may not declare \
             self_field_correction \"critical_state_strip\" or \
             limits.critical_state_layer_thickness_m",
        )?;
        require(!self.id.trim().is_empty(), "coupled case id is empty")?;
        require(
            !self.provenance.trim().is_empty(),
            "coupled case provenance is empty",
        )?;
        self.pack.validate()?;
        self.winding.validate(&self.pack)?;
        self.operating.validate()?;
        self.material.validate()?;
        // Schema v6 grading: `tape_specs` and `winding.regions` are declared
        // together or not at all; every region is a real, non-overlapping,
        // in-range turn span bound to a declared spec.
        let schema_at_least_v6 = self.schema == COUPLED_CASE_SCHEMA_V6 || schema_at_least_v7;
        require(
            schema_at_least_v6 || self.tape_specs.is_none(),
            "schema v1-v5 coupled cases may not declare tape_specs",
        )?;
        require(
            schema_at_least_v6 || self.winding.regions.is_none(),
            "schema v1-v5 coupled cases may not declare winding.regions",
        )?;
        if let Some(specs) = &self.tape_specs {
            require(!specs.is_empty(), "tape_specs must not be empty")?;
            require(
                specs.len() <= 32,
                "tape_specs exceeds the numerical work limit of 32 specs",
            )?;
            require(
                self.winding.regions.is_some(),
                "tape_specs requires winding.regions — a spec nothing references implies an evaluation that never happens",
            )?;
            for (id, settings) in specs {
                require(
                    !id.trim().is_empty() && id.len() <= 64,
                    "tape_spec ids must be non-empty and <= 64 chars",
                )?;
                settings.validate()?;
            }
        }
        if let Some(regions) = &self.winding.regions {
            let specs = self.tape_specs.as_ref().ok_or_else(|| {
                ModelError::Invalid(
                    "winding.regions requires tape_specs to resolve its tape_spec ids".to_owned(),
                )
            })?;
            for id in specs.keys() {
                require(
                    regions.iter().any(|r| &r.tape_spec == id),
                    "every tape_specs entry must be referenced by a winding.regions entry",
                )?;
            }
            require(
                !regions.is_empty() && regions.len() <= 64,
                "winding.regions must hold 1..=64 entries",
            )?;
            let mut covered = 0_u32;
            for (i, region) in regions.iter().enumerate() {
                require(
                    region.first_turn >= 1
                        && region.first_turn <= region.last_turn
                        && region.last_turn <= self.winding.turns_along_normal,
                    "winding.regions turn ranges must be non-empty within turns_along_normal",
                )?;
                if i > 0 {
                    require(
                        region.first_turn > regions[i - 1].last_turn,
                        "winding.regions must be sorted and non-overlapping",
                    )?;
                }
                require(
                    specs.contains_key(&region.tape_spec),
                    "winding.regions tape_spec must resolve to a declared tape_specs id",
                )?;
                covered += region.last_turn - region.first_turn + 1;
            }
            require(
                covered <= self.winding.turns_along_normal,
                "winding.regions cover more turns than turns_along_normal",
            )?;
        }
        self.sampling.validate(&self.pack, &self.winding)?;
        self.limits.validate()?;
        self.numerics.validate(&self.sampling)?;
        Ok(())
    }

    /// Dataset-dependent identity checks (contract A6, A11, §2): dataset_id,
    /// csv_sha256 and method must match the supplied dataset exactly, the
    /// electric-field criterion must match within roundoff, and the
    /// operating temperature must be strictly interior to the dataset's
    /// nominal temperature span (never on its boundary). The model crate has
    /// no Ic interpolator, so this takes an already-loaded `MaterialDataset`
    /// rather than reaching into optcoil-physics.
    ///
    /// This checks the case-level `material` binding only; on a v6 graded
    /// case call `validate_against_dataset_map` instead so every
    /// referenced spec's dataset identity is checked.
    pub fn validate_against_dataset(&self, dataset: &MaterialDataset) -> Result<(), ModelError> {
        self.material.validate_against_dataset(
            dataset,
            self.operating.temperature_k,
            self.operating.electric_field_criterion_v_per_m,
        )
    }

    /// Every referenced binding's dataset checks (schema v6): each
    /// material binding — the base `material` plus each `tape_specs`
    /// entry a region references — is validated against the dataset its
    /// own `dataset_id` resolves to. `datasets` is keyed by
    /// `MaterialDataset::metadata.id`; a declared id with no entry is an
    /// error, never a silent substitute.
    pub fn validate_against_dataset_map(
        &self,
        datasets: &std::collections::BTreeMap<String, MaterialDataset>,
    ) -> Result<(), ModelError> {
        for (_, material) in self.material_bindings() {
            let dataset = datasets.get(&material.dataset_id).ok_or_else(|| {
                ModelError::Invalid(format!(
                    "no dataset resolves material binding's declared dataset_id '{}'",
                    material.dataset_id
                ))
            })?;
            material.validate_against_dataset(
                dataset,
                self.operating.temperature_k,
                self.operating.electric_field_criterion_v_per_m,
            )?;
        }
        Ok(())
    }

    /// The material binding a 1-based `turn_index` screens under: the
    /// covering region's spec (v6) or the case-level `material` when no
    /// region covers it. Only meaningful on a validated case.
    pub fn material_for_turn(&self, turn_index: u32) -> &MaterialSettings {
        match self.winding.spec_for_turn(turn_index) {
            Some(spec) => self
                .tape_specs
                .as_ref()
                .and_then(|specs| specs.get(spec))
                .unwrap_or(&self.material),
            None => &self.material,
        }
    }

    /// Every material binding this case can evaluate: the base `material`
    /// plus each `tape_specs` entry a region actually references.
    /// Unreferenced specs do not appear — they are rejected by `validate`,
    /// which requires declared specs to be used.
    pub fn material_bindings(&self) -> Vec<(&str, &MaterialSettings)> {
        let mut out: Vec<(&str, &MaterialSettings)> = Vec::new();
        out.push(("material", &self.material));
        if let (Some(specs), Some(regions)) = (&self.tape_specs, &self.winding.regions) {
            for id in specs.keys() {
                if regions.iter().any(|r| &r.tape_spec == id) {
                    out.push((id.as_str(), &specs[id]));
                }
            }
        }
        out
    }
}

impl MaterialSettings {
    /// The per-binding half of `CoupledCase::validate_against_dataset`:
    /// this binding's dataset identity/method/criterion checks plus the
    /// operating-temperature interiority check. Scalar operating
    /// parameters (not `Operating`) so `CoupledSearchCase` — whose
    /// `SearchOperating` is a different type — validates its bindings
    /// through the same code.
    pub fn validate_against_dataset(
        &self,
        dataset: &MaterialDataset,
        temperature_k: f64,
        criterion_v_per_m: f64,
    ) -> Result<(), ModelError> {
        require(
            self.dataset_id == dataset.metadata.id,
            "material dataset_id does not match the supplied dataset",
        )?;
        require(
            self.csv_sha256 == dataset.metadata.csv_sha256,
            "material csv_sha256 does not match the supplied dataset",
        )?;
        require(
            self.method == EXPECTED_MATERIAL_METHOD_ID,
            "unsupported material interpolation method",
        )?;
        let criterion = dataset.metadata.electric_field_criterion_v_per_m;
        require(
            (criterion_v_per_m - criterion).abs() <= 1e-12 * criterion.max(1.0),
            "electric_field_criterion_v_per_m does not match the material dataset",
        )?;
        let temperatures = &dataset.metadata.selection.nominal_temperature_k;
        let min_t = temperatures.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_t = temperatures
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        require(
            temperature_k > min_t && temperature_k < max_t,
            "operating temperature_k must be strictly interior to the dataset's nominal temperature span",
        )?;
        Ok(())
    }
}

impl Pack {
    /// The pack's declared centerline, if any: the planar `path` or the
    /// non-planar `path3d`. `None` on a racetrack pack (no general-path
    /// representation) and on an invalid pack that declares nothing —
    /// `validate` rejects both before a runner can reach them.
    pub fn centerline(&self) -> Option<Centerline<'_>> {
        if let Some(path) = &self.path {
            Some(Centerline::Planar(path))
        } else {
            self.path3d.as_ref().map(Centerline::Helical)
        }
    }

    /// `rho` coordinate of the pack's inner face along the winding's
    /// stacking direction: the absolute radius `bend_radius - w/2` for a
    /// racetrack pack, the signed centerline offset `-w/2` for a path or
    /// path3d pack. `None` when no geometry is declared — `validate`
    /// rejects that before any runner can reach it.
    pub fn inner_rho_m(&self) -> Option<f64> {
        match (self.bend_radius_m, self.centerline().is_some()) {
            (Some(bend_radius_m), false) => Some(bend_radius_m - self.radial_width_m / 2.0),
            (None, true) => Some(-self.radial_width_m / 2.0),
            _ => None,
        }
    }

    /// Numerical support bounds mirroring `magnetics::Racetrack::validate`
    /// (racetrack packs), `CoilPath::validate_against_width` (planar path
    /// packs) or `CoilPath3D::validate_against_width` (helix packs); not a
    /// manufacturing or engineering geometry limit.
    pub fn validate(&self) -> Result<(), ModelError> {
        for (value, name) in [
            (self.radial_width_m, "radial_width_m"),
            (self.axial_height_m, "axial_height_m"),
        ] {
            require(
                value.is_finite() && (1e-9..=1e6).contains(&value),
                &format!("pack {name} must be finite and within 1e-9..=1e6 m"),
            )?;
        }
        require(
            self.straight_half_length_m.is_some() == self.bend_radius_m.is_some(),
            "pack straight_half_length_m and bend_radius_m must be declared together",
        )?;
        require(
            u8::from(self.bend_radius_m.is_some())
                + u8::from(self.path.is_some())
                + u8::from(self.path3d.is_some())
                == 1,
            "pack must declare exactly one of racetrack dimensions, path, or path3d",
        )?;
        if let (Some(straight), Some(bend_radius_m)) =
            (self.straight_half_length_m, self.bend_radius_m)
        {
            require(
                bend_radius_m.is_finite() && (1e-9..=1e6).contains(&bend_radius_m),
                "pack bend_radius_m must be finite and within 1e-9..=1e6 m",
            )?;
            require(
                straight.is_finite() && (0.0..=1e6).contains(&straight),
                "pack straight_half_length_m must be finite and within 0..=1e6 m",
            )?;
            require(
                bend_radius_m - self.radial_width_m / 2.0 >= 1e-9,
                "pack inner bend radius must be at least 1e-9 m",
            )?;
        }
        if let Some(path) = &self.path {
            path.validate_against_width(self.radial_width_m)?;
        }
        if let Some(path3d) = &self.path3d {
            path3d.validate_against_width(self.radial_width_m)?;
        }
        Ok(())
    }
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn require(ok: bool, detail: &str) -> Result<(), ModelError> {
    if ok {
        Ok(())
    } else {
        Err(ModelError::Invalid(detail.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_mapping_variants_round_trip_and_carry_the_seam_period() {
        let plain: AngleMapping = serde_json::from_str("\"period_180_field_reversal\"").unwrap();
        assert_eq!(plain, AngleMapping::Period180FieldReversal);
        assert_eq!(plain.seam_period_deg(), None);
        let stitched: AngleMapping =
            serde_json::from_str("\"period_180_field_reversal_seam_stitched\"").unwrap();
        assert_eq!(stitched, AngleMapping::Period180FieldReversalSeamStitched);
        assert_eq!(stitched.seam_period_deg(), Some(180.0));
        assert_eq!(
            serde_json::to_string(&stitched).unwrap(),
            "\"period_180_field_reversal_seam_stitched\""
        );
    }

    #[test]
    fn embedded_case_parses_and_matches_hand_derived_geometry() {
        let case = CoupledCase::embedded().unwrap();
        assert_eq!(case.winding.total_turns(), 600);
        assert!((case.winding.pitch_n_m(&case.pack) - 0.0001).abs() < 1e-15);
        assert!((case.winding.pitch_w_m(&case.pack) - 0.012).abs() < 1e-15);
        // Hand-derived from contract A4 for turn_index=1, tape_index=3 at the
        // straight_center station (x_m = 0.0); matches reference_subset[0]:
        // rho = (0.2 - 0.01) + 0.5 * 0.0001 = 0.19005
        // z   = -0.018 + 2.5 * 0.012        = 0.012
        let station = case
            .sampling
            .stations
            .iter()
            .find(|s| s.id() == "straight_center")
            .unwrap();
        let position = tape_center_position_m(&case.pack, &case.winding, station, 3, 1).unwrap();
        assert!((position[0] - 0.0).abs() < 1e-12);
        assert!((position[1] - 0.19005).abs() < 1e-12);
        assert!((position[2] - 0.012).abs() < 1e-12);
    }

    #[test]
    fn embedded_oc014_case_parses_as_a_v4_critical_state_fixture() {
        let case = CoupledCase::from_json(OC014_JSON).unwrap();
        assert_eq!(case.schema, COUPLED_CASE_SCHEMA_V4);
        assert_eq!(
            case.limits.self_field_correction.as_deref(),
            Some("critical_state_strip")
        );
        assert_eq!(case.limits.critical_state_layer_thickness_m, Some(1.0e-6));
        assert_eq!(
            case.material.dataset_id,
            crate::material::SUPERPOWER_MODELEXT_ID
        );
    }

    #[test]
    fn frame_vectors_at_named_stations_are_continuous_at_the_straight_to_arc_junction() {
        let straight = Station::Straight {
            id: "s".into(),
            x_m: 0.0,
        };
        let arc_at_90 = Station::Arc {
            id: "a".into(),
            azimuth_deg: 90.0,
        };
        let straight_frame = straight.frame(TapeNormal::Radial, None).unwrap();
        let arc_frame = arc_at_90.frame(TapeNormal::Radial, None).unwrap();
        assert_eq!(straight_frame.t, [-1.0, 0.0, 0.0]);
        assert_eq!(straight_frame.n, [0.0, 1.0, 0.0]);
        assert_eq!(straight_frame.w, [0.0, 0.0, 1.0]);
        for k in 0..3 {
            assert!((arc_frame.t[k] - straight_frame.t[k]).abs() < 1e-12);
            assert!((arc_frame.n[k] - straight_frame.n[k]).abs() < 1e-12);
            assert!((arc_frame.w[k] - straight_frame.w[k]).abs() < 1e-12);
        }

        let arc_at_45 = Station::Arc {
            id: "a".into(),
            azimuth_deg: 45.0,
        };
        let frame_45 = arc_at_45.frame(TapeNormal::Radial, None).unwrap();
        let s = std::f64::consts::FRAC_1_SQRT_2; // sqrt(2)/2, an independent literal
        assert!((frame_45.t[0] - (-s)).abs() < 1e-12);
        assert!((frame_45.t[1] - s).abs() < 1e-12);
        assert!((frame_45.n[0] - s).abs() < 1e-12);
        assert!((frame_45.n[1] - s).abs() < 1e-12);
    }

    #[test]
    fn axial_normal_variant_swaps_the_normal_and_width_directions() {
        let straight = Station::Straight {
            id: "s".into(),
            x_m: 0.1,
        };
        let frame = straight.frame(TapeNormal::Axial, None).unwrap();
        assert_eq!(frame.t, [-1.0, 0.0, 0.0]);
        assert_eq!(frame.n, [0.0, 0.0, 1.0]);
        assert_eq!(frame.w, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn station_validate_enforces_the_declared_azimuth_and_x_m_boundaries() {
        // First-line structural input validation on the path from a
        // hand-edited or externally-supplied case JSON; previously
        // untested (only Station::frame at azimuth_deg=90 was exercised,
        // never Station::validate's own accept/reject boundary).
        let pack = CoupledCase::embedded().unwrap().pack;

        let arc = |azimuth_deg: f64| Station::Arc {
            id: "a".into(),
            azimuth_deg,
        };
        assert!(arc(90.0).validate(&pack).is_ok());
        assert!(arc(-90.0).validate(&pack).is_ok());
        assert!(arc(90.0 + 1e-7).validate(&pack).is_err());
        assert!(arc(-90.0 - 1e-7).validate(&pack).is_err());

        let straight = |x_m: f64| Station::Straight {
            id: "s".into(),
            x_m,
        };
        let half_length = pack.straight_half_length_m.unwrap();
        assert!(straight(half_length).validate(&pack).is_ok());
        assert!(straight(-half_length).validate(&pack).is_ok());
        assert!(straight(half_length + 1e-9).validate(&pack).is_err());
        assert!(straight(-(half_length + 1e-9)).validate(&pack).is_err());
    }

    #[test]
    fn width_fill_mismatch_is_rejected() {
        let mut case = CoupledCase::embedded().unwrap();
        case.winding.tape_width_m = 0.011;
        assert!(case.validate().is_err());
    }

    #[test]
    fn boundary_temperature_is_rejected_against_the_dataset() {
        let case = CoupledCase::embedded().unwrap();
        let dataset = MaterialDataset::embedded().unwrap();
        case.validate_against_dataset(&dataset).unwrap();
        let mut boundary = case.clone();
        boundary.operating.temperature_k = 20.0; // exactly the nominal minimum
        assert!(boundary.validate_against_dataset(&dataset).is_err());
        let mut boundary = case;
        boundary.operating.temperature_k = 40.0; // exactly the nominal maximum
        assert!(boundary.validate_against_dataset(&dataset).is_err());
    }

    #[test]
    fn bad_indices_are_rejected() {
        let mut case = CoupledCase::embedded().unwrap();
        case.sampling.turn_indices_along_normal.push(0);
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        *case.sampling.turn_indices_along_normal.last_mut().unwrap() = 201; // > turns_along_normal (200)
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        case.sampling.turn_indices_along_normal.swap(0, 1); // no longer ascending
        assert!(case.validate().is_err());
    }

    #[test]
    fn width_axis_index_list_admits_pancake_class_enumeration() {
        // OC-024's 72-pancake coil needs a 72-entry width enumeration;
        // the width axis carries its own cap (256) while the turn axis
        // stays at the shared 64 the refined-plan contract fixes.
        let mut case = CoupledCase::embedded().unwrap();
        // Contract A1: tapes x tape_width must fill the pack's 0.036 m
        // width extent — rescale the tape width to keep it invariant.
        case.winding.tapes_along_width = 72;
        case.winding.tape_width_m = 0.036 / 72.0;
        case.sampling.tape_indices_along_width = (1..=72).collect();
        case.validate().unwrap();

        let mut case = CoupledCase::embedded().unwrap();
        case.winding.tapes_along_width = 300;
        case.winding.tape_width_m = 0.036 / 300.0;
        case.sampling.tape_indices_along_width = (1..=257).collect();
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        case.sampling.turn_indices_along_normal = (1..=65).collect();
        assert!(case.validate().is_err());
    }

    #[test]
    fn non_increasing_current_candidates_are_rejected() {
        let mut case = CoupledCase::embedded().unwrap();
        case.operating.current_candidates_a = vec![700.0, 700.0, 870.0];
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        case.operating.current_candidates_a = vec![870.0, 700.0];
        assert!(case.validate().is_err());
    }

    #[test]
    fn unknown_enum_strings_fail_to_parse() {
        assert!(CoupledCase::from_json(&OC004_JSON.replace("\"radial\"", "\"planar\"")).is_err());
        assert!(
            CoupledCase::from_json(&OC004_JSON.replace("\"gauss_lobatto\"", "\"simpson\""))
                .is_err()
        );
        assert!(CoupledCase::from_json(&OC004_JSON.replace("\"straight\"", "\"curved\"")).is_err());
    }

    #[test]
    fn reference_subset_entries_outside_the_sampling_plan_are_rejected() {
        let mut case = CoupledCase::embedded().unwrap();
        case.numerics.reference_subset[0].station = "not_a_station".into();
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        case.numerics.reference_subset[0].tape_index = 99;
        assert!(case.validate().is_err());

        let mut case = CoupledCase::embedded().unwrap();
        case.numerics.reference_subset[0].turn_index = 999;
        assert!(case.validate().is_err());
    }

    #[test]
    fn width_offset_maps_the_lobatto_domain_endpoints_and_center() {
        assert_eq!(width_offset_m(0.012, -1.0), -0.006);
        assert_eq!(width_offset_m(0.012, 0.0), 0.0);
        assert_eq!(width_offset_m(0.012, 1.0), 0.006);
    }

    /// A minimal v7 field-map case: circle path R = 0.25 m, pack
    /// radial width 0.02 m / axial height 0.036 m, so the sampled hull is
    /// r in [0.24, 0.26] m and z in [-0.018, 0.018] m. The declared map
    /// covers it with margin: rho cell centres [0.235, 0.265] (hull
    /// [0.22, 0.28]) and z centres [-0.02, -0.005, 0.005, 0.02] (hull
    /// [-0.0275, 0.0275]).
    fn field_map() -> FieldMap {
        let rho_levels_m = vec![0.235, 0.265];
        let z_levels_m = vec![-0.02, -0.005, 0.005, 0.02];
        let mut entries = Vec::new();
        for (i, _) in rho_levels_m.iter().enumerate() {
            for (j, _) in z_levels_m.iter().enumerate() {
                entries.push(FieldMapEntry {
                    rho_index: i as u32,
                    z_index: j as u32,
                    br_t: 0.1 * (i as f64) - 0.02 * (j as f64),
                    bz_t: 1.0 + 0.5 * (i as f64),
                });
            }
        }
        FieldMap::CylindricalBrBz {
            source_sha256: "a".repeat(64),
            reference_ampere_turns_a: 1.0e6,
            rho_levels_m,
            z_levels_m,
            entries,
        }
    }

    /// Mutable access to a cylindrical fixture's innards for the
    /// negative-case tests.
    fn cylindrical_mut(
        map: &mut FieldMap,
    ) -> (&mut Vec<f64>, &mut Vec<FieldMapEntry>, &mut String) {
        match map {
            FieldMap::CylindricalBrBz {
                rho_levels_m,
                entries,
                source_sha256,
                ..
            } => (rho_levels_m, entries, source_sha256),
            FieldMap::CartesianBxByBz { .. } => panic!("test fixture is cylindrical"),
        }
    }

    /// OC-014's case body re-pointed at a circular path pack on schema
    /// v7 with the test field map. The racetrack dims are dropped, the
    /// winding becomes axial-normal (4 axial turns x 2 tapes), and the
    /// sampling plan uses one path station.
    fn field_map_case() -> CoupledCase {
        let mut case = CoupledCase::from_json(OC014_JSON).unwrap();
        case.schema = COUPLED_CASE_SCHEMA_V7.to_owned();
        case.pack.straight_half_length_m = None;
        case.pack.bend_radius_m = None;
        case.pack.path = Some(CoilPath {
            segments: vec![crate::path::PathSegment::Arc {
                radius_m: 0.25,
                sweep_deg: 360.0,
            }],
            start_position_m: [0.25, 0.0],
            start_heading_deg: 90.0,
        });
        case.winding.tape_normal = TapeNormal::Axial;
        case.winding.turns_along_normal = 4;
        case.winding.tapes_along_width = 2;
        case.winding.tape_width_m = 0.01;
        case.sampling.stations = vec![Station::Path {
            id: "p0".into(),
            s_m: 0.0,
        }];
        case.sampling.turn_indices_along_normal = vec![1, 4];
        case.sampling.tape_indices_along_width = vec![1, 2];
        case.numerics.reference_subset[0].station = "p0".into();
        case.numerics.reference_subset[0].turn_index = 1;
        case.numerics.reference_subset[0].tape_index = 1;
        case.numerics.reference_subset.truncate(1);
        case.sampling.field_map = Some(field_map());
        case
    }

    #[test]
    fn field_map_v7_case_validates_and_resolves() {
        let case = field_map_case();
        case.validate().unwrap();
        let map = case.sampling.field_map.as_ref().unwrap();
        // Hull: half-spacing extension past the declared cell centres.
        let (r_lo, r_hi) = map.rho_hull_m().unwrap();
        assert!((r_lo - 0.22).abs() < 1e-12 && (r_hi - 0.28).abs() < 1e-12);
        let (z_lo, z_hi) = map.z_hull_m();
        assert!((z_lo + 0.0275).abs() < 1e-12 && (z_hi - 0.0275).abs() < 1e-12);
        // nearest() picks the declared node exactly: (0.24, 0.019) ->
        // (i=0, j=3); (0.25, -0.01) -> (i=0 on the midpoint tie, j=1).
        let resolved = map.resolve().unwrap();
        match resolved {
            ResolvedFieldMap::CylindricalBrBz(resolved) => {
                assert_eq!(resolved.nearest(0.24, 0.019), (-0.06, 1.0));
                assert_eq!(resolved.nearest(0.25, -0.01), (-0.02, 1.0));
            }
            _ => panic!("test fixture is cylindrical"),
        }
    }

    #[test]
    fn field_map_is_rejected_on_pre_v7_schemas() {
        let mut case = field_map_case();
        case.schema = COUPLED_CASE_SCHEMA_V6.to_owned();
        let err = case.validate().unwrap_err().to_string();
        assert!(err.contains("field_map"), "{err}");
    }

    #[test]
    fn field_map_requires_an_origin_centred_circle() {
        // No path at all.
        let mut case = field_map_case();
        case.pack.path = None;
        assert!(case.validate().is_err());
        // A racetrack path (line segments) is not axisymmetric.
        let mut case = field_map_case();
        case.pack.path = Some(CoilPath::racetrack(0.3, 0.2));
        assert!(case.validate().is_err());
        // A clockwise (-360) circle's normal points inward — rejected.
        let mut case = field_map_case();
        case.pack.path = Some(CoilPath {
            segments: vec![crate::path::PathSegment::Arc {
                radius_m: 0.25,
                sweep_deg: -360.0,
            }],
            start_position_m: [0.25, 0.0],
            start_heading_deg: -90.0,
        });
        assert!(case.validate().is_err());
    }

    #[test]
    fn field_map_rejects_bad_grids_and_out_of_hull_packs() {
        // Non-ascending rho levels.
        let mut case = field_map_case();
        *cylindrical_mut(case.sampling.field_map.as_mut().unwrap()).0 = vec![0.265, 0.235];
        assert!(case.validate().is_err());
        // Grid that does not contain the pack's radial extent.
        let mut case = field_map_case();
        *cylindrical_mut(case.sampling.field_map.as_mut().unwrap()).0 = vec![0.25, 0.26];
        assert!(case.validate().is_err());
        // Incomplete entry grid.
        let mut case = field_map_case();
        cylindrical_mut(case.sampling.field_map.as_mut().unwrap())
            .1
            .pop();
        assert!(case.validate().is_err());
        // Uppercase hash.
        let mut case = field_map_case();
        *cylindrical_mut(case.sampling.field_map.as_mut().unwrap()).2 = "A".repeat(64);
        assert!(case.validate().is_err());
        // Non-finite field value.
        let mut case = field_map_case();
        cylindrical_mut(case.sampling.field_map.as_mut().unwrap()).1[0].br_t = f64::NAN;
        assert!(case.validate().is_err());
    }

    /// A Cartesian map over a racetrack path: the fixture covers the
    /// CoilPath::racetrack(0.3, 0.2) swept domain (half-width 0.02,
    /// half-height 0.036) with margin.
    fn cartesian_field_map() -> FieldMap {
        // Domain extent: the racetrack spans x in [-0.52, 0.52]
        // (half_length + bend + half_width) and y in [-0.22, 0.22]
        // (bend + half_width); z in [-0.036, 0.036].
        let x_levels_m = vec![-0.6, -0.3, 0.0, 0.3, 0.6];
        let y_levels_m = vec![-0.3, 0.0, 0.3];
        let z_levels_m = vec![-0.05, 0.0, 0.05];
        let (nx, ny, nz) = (x_levels_m.len(), y_levels_m.len(), z_levels_m.len());
        let mut entries = Vec::new();
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    entries.push(FieldMapCartesianEntry {
                        x_index: i as u32,
                        y_index: j as u32,
                        z_index: k as u32,
                        bx_t: 0.01 * (i as f64),
                        by_t: -0.02 * (j as f64),
                        bz_t: 2.0 + 0.1 * (k as f64),
                    });
                }
            }
        }
        FieldMap::CartesianBxByBz {
            source_sha256: "b".repeat(64),
            reference_ampere_turns_a: 2.0e6,
            x_levels_m,
            y_levels_m,
            z_levels_m,
            entries,
        }
    }

    /// A v8 coupled case on a racetrack pack with the Cartesian map.
    fn cartesian_field_map_case() -> CoupledCase {
        let mut case = CoupledCase::from_json(OC014_JSON).unwrap();
        case.schema = COUPLED_CASE_SCHEMA_V8.to_owned();
        case.sampling.field_map = Some(cartesian_field_map());
        case
    }

    #[test]
    fn cartesian_field_map_v8_validates_resolves_and_queries() {
        let case = cartesian_field_map_case();
        case.validate().unwrap();
        let map = case.sampling.field_map.as_ref().unwrap();
        assert_eq!(map.components(), FieldMapComponents::CartesianBxByBz);
        let resolved = map.resolve().unwrap();
        match &resolved {
            ResolvedFieldMap::CartesianBxByBz(resolved) => {
                // (0.29, 0.01, -0.04) -> node (i=3, j=1, k=0).
                assert_eq!(resolved.nearest(0.29, 0.01, -0.04), (0.03, -0.02, 2.0));
            }
            _ => panic!("test fixture is cartesian"),
        }
        // Lab-frame unit field is the node value scaled by the declared
        // reference ampere-turns — no basis rotation.
        let b = resolved
            .unit_field_lab_t_per_ampere_turn([0.29, 0.01, -0.04])
            .unwrap();
        assert!((b[2] - 2.0 / 2.0e6).abs() < 1e-18);
        // Outside the hull: fail closed.
        assert!(
            resolved
                .unit_field_lab_t_per_ampere_turn([0.9, 0.0, 0.0])
                .is_err()
        );
    }

    #[test]
    fn cartesian_field_map_needs_v8_and_hull_confinement() {
        // The component tag is a v8 widening — a v7 case must reject it.
        let mut case = cartesian_field_map_case();
        case.schema = COUPLED_CASE_SCHEMA_V7.to_owned();
        let err = case.validate().unwrap_err().to_string();
        assert!(err.contains("cartesian"), "{err}");
        // A hull that does not contain the swept domain must fail.
        let mut case = cartesian_field_map_case();
        match case.sampling.field_map.as_mut().unwrap() {
            FieldMap::CartesianBxByBz { x_levels_m, .. } => {
                *x_levels_m = vec![-0.3, -0.1, 0.0, 0.1, 0.3]
            }
            _ => panic!("test fixture is cartesian"),
        }
        assert!(case.validate().is_err());
        // A declared (non-circular) path satisfies confinement; with no
        // path and no racetrack dims there is nothing to confine against.
        let mut case = field_map_case();
        case.schema = COUPLED_CASE_SCHEMA_V8.to_owned();
        case.sampling.field_map = Some(cartesian_field_map());
        case.validate().unwrap();
        case.pack.path = None;
        assert!(case.validate().is_err());
    }

    #[test]
    fn field_map_wire_format_is_unchanged_for_cylindrical() {
        // The internally-tagged enum must reproduce the v7 flat layout:
        // "components" sits beside the grid fields, not nested.
        let value = serde_json::to_value(field_map()).unwrap();
        assert_eq!(value["components"], "cylindrical_br_bz");
        assert!(value.get("rho_levels_m").is_some());
        assert!(value.get("z_levels_m").is_some());
        assert!(value.get("entries").is_some());
        assert!(value.get("x_levels_m").is_none());
        // And round-trips.
        let back: FieldMap = serde_json::from_value(value).unwrap();
        assert_eq!(back.components(), FieldMapComponents::CylindricalBrBz);
    }
}
