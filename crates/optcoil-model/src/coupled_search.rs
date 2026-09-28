//! `optcoil-coupled-search/v1`: the OC-007 coupled cost search case schema
//! and a pure geometry helper that produces an equivalent
//! `coupled::CoupledCase` (schema v1) for one `(turns, tapes, I_op)` triple.
//! This keeps the search's screening physics identical to OC-004's, reused
//! rather than reimplemented: every geometry this module builds is scored by
//! the same `optcoil_search::coupled` runner OC-004 already validates.
//!
//! Everything this schema describes is a declared search assumption over a
//! synthetic cost model and a screening margin, never a manufacturing
//! recommendation or a production operating-current limit; see the OC-007
//! contract and docs/OC007.md.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    DataClass, ModelError,
    coupled::{
        CoupledCase, Limits, MaterialSettings, Numerics, Operating, Pack, ReferenceSubsetEntry,
        Sampling, Station, TapeNormal, WidthQuadrature, WidthTransfer, WidthTransferBasis, Winding,
    },
    material::MaterialDataset,
    path::CoilPath,
};

/// Schema v1: OC-007's own discrete exhaustive search over declared
/// `(turns, tapes)` choices. Still fully supported; a v1 case must not
/// declare a `refinement` block.
pub const COUPLED_SEARCH_CASE_SCHEMA_V1: &str = "optcoil-coupled-search/v1";
/// Schema v2 (OC-008 contract §2, Stage B): adds the `refinement` block
/// (per-pancake-count bracketing bisection). A v2 case must declare
/// `refinement`; every other field is unchanged from v1.
pub const COUPLED_SEARCH_CASE_SCHEMA_V2: &str = "optcoil-coupled-search/v2";
/// Schema v3: v2 plus `requirement.good_field_region`, a declared usable
/// volume inside which B_z must meet `b_target_t` on a probe lattice. `NI`
/// is solved on the region's weakest lattice point, so a candidate cannot
/// pass by shrinking the usable volume away. A v3 case must declare both
/// `refinement` and `good_field_region`. v3 also admits the optional
/// top-level `manufacturing` block of geometric feasibility limits
/// (currently: a minimum inner bend radius the innermost turn must
/// respect, since pack width shrinks the inner radius with `n`).
pub const COUPLED_SEARCH_CASE_SCHEMA_V3: &str = "optcoil-coupled-search/v3";
/// Schema v4: v3 plus `choices.strands_parallel` / `baseline.strands_parallel`
/// (default 1): each turn is wound from `s` conductors sharing the operating
/// current (Roebel-cable semantics). Ampere-turns and pack geometry are
/// unchanged; screened capacity scales by `s`, conductor cost by `s`.
/// v1-v3 cases must not declare a strands list other than `[1]`.
pub const COUPLED_SEARCH_CASE_SCHEMA_V4: &str = "optcoil-coupled-search/v4";
/// Schema v5: v4 plus `limits.self_field_correction` (OC-014). The only
/// implemented model is `"uniform_transport"`: the screening query
/// magnitude becomes `|B_applied| + μ0·K_tape/2` (the conservative
/// uniform-sheet bound) and the self-field INCONCLUSIVE gate moves to
/// the physical dominance boundary (ratio > 1). Schemas v1-v4 must not
/// declare the field — it silently changes screening semantics.
pub const COUPLED_SEARCH_CASE_SCHEMA_V5: &str = "optcoil-coupled-search/v5";
/// Schema v6: v5 plus `limits.along_current_model` (OC-017). The only
/// implemented model is `"transverse_bound"`: points whose along-current
/// field fraction sits in `(max_along_current_field_fraction,
/// coupled::ALONG_CURRENT_BOUND_CEILING]` still query the measured law at
/// full magnitude and the transverse-plane angle, marked
/// `along_current_bounded`; above the ceiling they remain excluded.
/// Schemas v1-v5 must not declare the field.
pub const COUPLED_SEARCH_CASE_SCHEMA_V6: &str = "optcoil-coupled-search/v6";
/// Schema v7: v6 plus the optional `mechanical.max_hoop_stress_pa` /
/// `mechanical.tension_section_area_m2` pair (OC-018): a first-order
/// hoop-stress bound, σ = (I_op · B_peak) · R_outer / A_section. Schemas
/// v4-v6 may declare only `max_lorentz_load_n_per_m`; the hoop fields
/// silently change screening semantics so earlier schemas must not
/// declare them.
pub const COUPLED_SEARCH_CASE_SCHEMA_V7: &str = "optcoil-coupled-search/v7";
/// Schema v8: v7 plus `limits.self_field_correction =
/// "critical_state_strip"` and its declared
/// `limits.critical_state_layer_thickness_m` (OC-014 Phase 2): a
/// critical-state edge-field bound valid in the self-field-dominated
/// regime, replacing the `uniform_transport` dominance gate. Schemas
/// v5-v7 may declare only `"uniform_transport"`; the new value changes
/// the screening query in the dominance regime so earlier schemas must
/// not declare it.
pub const COUPLED_SEARCH_CASE_SCHEMA_V8: &str = "optcoil-coupled-search/v8";
/// v9: v8 plus general planar geometry — `fixed_geometry.path` (a
/// `CoilPath`) replaces the racetrack dimensions, `Station::Path { s_m }`
/// addresses the centerline by arc length, and generated coupled cases
/// carry schema `optcoil-coupled-conductor/v5`. The bend screen becomes a
/// minimum-local-curvature check and the hoop bound uses the path's
/// largest arc radius.
pub const COUPLED_SEARCH_CASE_SCHEMA_V9: &str = "optcoil-coupled-search/v9";
/// v10: v9 plus graded conductor specs — `tape_specs` (named
/// `MaterialSettings` + price) and `grading.regions` choosing a spec per
/// turn range per candidate. The generated coupled case carries schema
/// `optcoil-coupled-conductor/v6`. Earlier schemas must not declare them —
/// the material law and cost become position-dependent.
pub const COUPLED_SEARCH_CASE_SCHEMA_V10: &str = "optcoil-coupled-search/v10";
/// v11: v10 plus `refinement` on graded cases — the bisection runs per
/// (tapes, assignment) slice over the shared per-tapes bracket, so a
/// graded case at v11 must declare a `refinement` block exactly as any
/// v2+ case does. Graded cases at v10 still forbid it.
pub const COUPLED_SEARCH_CASE_SCHEMA_V11: &str = "optcoil-coupled-search/v11";
/// v12: v11 plus the optional `mechanical.max_transverse_pressure_pa`
/// bound — a first-order transverse (delamination-direction) pressure
/// screen: each sampled turn's signed normal Lorentz load per unit length
/// `I_op·⟨B·ŵ⟩` is trapezoid-accumulated along the stacking direction,
/// and the peak interface pressure (max over interfaces of both
/// face-anchored cumulative loads, divided by the turn's tape-width
/// interface) must stay within the declared bound. Earlier schemas may
/// not declare it — it changes screening semantics.
pub const COUPLED_SEARCH_CASE_SCHEMA_V12: &str = "optcoil-coupled-search/v12";
/// v13: v12 plus the optional `field_map` block — a customer-declared
/// external cylindrical field map (the coupled-schema v7 semantics)
/// evaluated instead of the engine's field solve, plus the declared
/// `bore_field_at_reference_t` anchor the NI solve uses in place of a
/// bore-probe evaluation (the map covers the pack region only). A
/// declared map is valid only for the winding it was computed on, so
/// v13 cases declare fixed pack extents
/// (`fixed_geometry.pack_radial_width_m`/`pack_axial_height_m`) and the
/// choices vary the tape discretization and material assignment, never
/// the physical footprint: the generated coupled case's pitch and tape
/// width are derived per candidate as extent/count. Earlier schemas may
/// not declare any of it — it changes both field and geometry semantics.
pub const COUPLED_SEARCH_CASE_SCHEMA_V13: &str = "optcoil-coupled-search/v13";
/// v14: v13 plus `field_map` component `cartesian_bx_by_bz` — a full
/// lab-frame (B_x, B_y, B_z) map on a rectilinear (x, y, z) grid for
/// non-axisymmetric windings and general solver exports (the
/// coupled-schema v8 semantics). The generated coupled cases are v8.
pub const COUPLED_SEARCH_CASE_SCHEMA_V14: &str = "optcoil-coupled-search/v14";
/// v15: v14 plus the optional first-order adjacent screens —
/// `thermal_margin`, `ac_loss`, `quench_hotspot`, `screening_current`.
/// Every screen is a declared-assumption bound or flag, never a
/// multiphysics simulation; each records its own status and a screen
/// whose inputs cannot resolve reports INCONCLUSIVE, never PASS.
pub const COUPLED_SEARCH_CASE_SCHEMA_V15: &str = "optcoil-coupled-search/v15";
/// v16: v15 plus `transition` — the measured E–J transition screen.
/// Every dataset's measured `n_value` now rides the same tetrahedron
/// weights as `Ic` through the interpolator (the ic model ids are
/// unchanged: the added channel is linear in both methods); the screen
/// evaluates `E/Ec = (K_demand/K_used)^n` per sampled point under the
/// same mirror-pair/clamp query policy, aggregates a per-strand
/// terminal-voltage estimate, and reports INCONCLUSIVE wherever the
/// transition law cannot resolve — never PASS.
pub const COUPLED_SEARCH_CASE_SCHEMA_V16: &str = "optcoil-coupled-search/v16";
/// v17: v16 plus the first-order winding-mechanics additions —
/// `manufacturing.tape_thickness_m` / `manufacturing.max_bend_strain`
/// (the tape's outer-fiber bend strain `t/(2·R_inner)` against a declared
/// irreversible-strain bound) and `mechanical.max_membrane_tension_n_per_m`
/// (the peak cumulative normal Lorentz load per unit interface width —
/// the undivided resultant the transverse-pressure screen normalizes —
/// against a declared tension bound). Both are declared-assumption first
/// -order bounds, not winding-mechanics FEA: stress redistribution,
/// insulation compliance, delamination paths and contact detail are
/// unmodeled.
pub const COUPLED_SEARCH_CASE_SCHEMA_V17: &str = "optcoil-coupled-search/v17";
/// v18: v17 plus the lumped adiabatic quench-transient screen —
/// `quench_transient` integrates the driven-dump trajectory
/// `dT/dt = J_m²·ρ_e(T)/C_v(T)` per sampled point under a declared
/// exponential dump, with the matrix share `I_m = max(0, I_strand −
/// I_sc(T))` resolved against the covering dataset's measured capacity
/// at the evolving temperature. Reports the peak temperature reached
/// and the per-strand resistive series-voltage signature against a
/// declared detection threshold. A lumped driven-dump bound: quench
/// initiation, normal-zone propagation, turn-to-turn thermal coupling
/// and the dump's own field decay are unmodeled.
pub const COUPLED_SEARCH_CASE_SCHEMA_V18: &str = "optcoil-coupled-search/v18";
/// v19: v18 plus `fixed_geometry.path3d` — a non-planar `CoilPath3D`
/// helix centerline for CCT/CORC-class windings, generated into coupled
/// schema v9 `pack.path3d`. v18 and below must not declare it. A v19
/// case must declare a Cartesian `field_map` (non-planar packs evaluate
/// under a declared map only — the engine's pack self-field evaluator
/// is planar) and `tape_normal` must be `radial` (the cylinder-radial
/// stacking a helical conductor winds in). Closure is optional on the
/// path itself — a helical layer's ends are its leads.
pub const COUPLED_SEARCH_CASE_SCHEMA_V19: &str = "optcoil-coupled-search/v19";
/// v20: v19 plus the optional `opex` block — declared cold-side heat
/// loads priced through a Carnot-scaled refrigeration input at the
/// declared operating point, so "colder = less tape but more cryo" is a
/// *declared* lifecycle-cost axis rather than a fixed assumption. The
/// ledger gains `opex_usd` and `lifecycle_usd`; the terms are
/// case-constant (no candidate-dependent loads are modeled), so
/// within-case rankings and savings are unchanged — the operating-point
/// trade materializes across the `temperature_k` sensitivity axis.
pub const COUPLED_SEARCH_CASE_SCHEMA_V20: &str = "optcoil-coupled-search/v20";
/// v21: v20 plus racetrack-dimension search axes —
/// `choices.bend_radius_m` / `choices.straight_half_length_m` turn
/// magnet size into a declared choice dimension (less racetrack = less
/// tape = less conductor money, at whatever field the shape yields).
/// An axis requires the corresponding `fixed_geometry` field *absent*
/// (one declaration site per dimension) and a `baseline` value drawn
/// from the list. Legacy racetrack cases only — a declared `path`,
/// `path3d`, or `field_map` fixes the winding shape, so the axes are
/// forbidden there. The OC-008 bracketing-refinement runner
/// (`coupled_refine`) rejects geometry-axis cases: its declared turn
/// brackets assume turn-monotonicity at a fixed geometry.
pub const COUPLED_SEARCH_CASE_SCHEMA_V21: &str = "optcoil-coupled-search/v21";
/// v22: v21 plus `requirement.good_field_region.max_relative_deviation` —
/// a declared uniformity bound over the usable-volume lattice. The
/// region's (Bmax − Bmin)/B_center spread joins the requirement gate:
/// a coil may hit its target at the weakest point and still fail the
/// declared homogeneity a real MRI/accelerator spec writes down.
pub const COUPLED_SEARCH_CASE_SCHEMA_V22: &str = "optcoil-coupled-search/v22";
/// v23: v22 plus `requirement.good_field_region.harmonics` — a
/// cylindrical Fourier expansion of `B_z` on a reference circle in the
/// region midplane, the normal/skew `b_n`/`a_n` coefficient form an
/// accelerator field-quality spec writes down.
pub const COUPLED_SEARCH_CASE_SCHEMA_V23: &str = "optcoil-coupled-search/v23";
/// v24: v23 plus `cost.piece_policy` and the piece-offering catalogue
/// fields — piece-quantized procurement (purchased metres are the pieces
/// actually bought, in-winding splices are counted and priced separately
/// from module joints, per-spec price menus are argmin'd per spec), plus
/// `price_source` provenance labels on prices.
pub const COUPLED_SEARCH_CASE_SCHEMA_V24: &str = "optcoil-coupled-search/v24";
/// Kept as an alias for the original v1 constant name so existing callers
/// (and the still-current OC-007 cases) are unaffected.
pub const COUPLED_SEARCH_CASE_SCHEMA: &str = COUPLED_SEARCH_CASE_SCHEMA_V1;

/// v21 is the newest schema; written as a function chain so the next
/// version's gate site stays obvious.
fn schema_at_least_v7(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V7 || schema_at_least_v8(schema)
}
/// v8 gate for the OC-014 Phase 2 `critical_state_strip` model and its
/// declared layer thickness.
fn schema_at_least_v8(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V8 || schema_at_least_v9(schema)
}
/// v9 gate for general planar `fixed_geometry.path` declarations.
fn schema_at_least_v9(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V9 || schema_at_least_v10(schema)
}
/// v10 gate for `tape_specs`/`grading` declarations.
fn schema_at_least_v10(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V10 || schema_at_least_v11(schema)
}
/// v11 gate for `refinement` on graded cases.
fn schema_at_least_v11(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V11 || schema_at_least_v12(schema)
}
/// v12 gate for the `mechanical.max_transverse_pressure_pa` bound.
fn schema_at_least_v12(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V12 || schema_at_least_v13(schema)
}
/// v13 gate for `field_map` and the fixed pack extents.
fn schema_at_least_v13(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V13 || schema_at_least_v14(schema)
}
/// v14 gate for `field_map` component `cartesian_bx_by_bz`.
fn schema_at_least_v14(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V14 || schema_at_least_v15(schema)
}
/// v15 gate for the first-order adjacent screens.
fn schema_at_least_v15(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V15 || schema_at_least_v16(schema)
}
/// v16 gate for the measured E–J `transition` screen.
fn schema_at_least_v16(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V16 || schema_at_least_v17(schema)
}
/// v17 gate for the bend-strain and membrane-tension bounds.
fn schema_at_least_v17(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V17 || schema_at_least_v18(schema)
}
/// v18 gate for the lumped adiabatic quench-transient screen.
fn schema_at_least_v18(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V18 || schema_at_least_v19(schema)
}
/// v19 gate for the non-planar `fixed_geometry.path3d` centerline.
fn schema_at_least_v19(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V19 || schema_at_least_v20(schema)
}
/// v20 gate for the `opex` lifecycle-cost block.
fn schema_at_least_v20(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V20 || schema_at_least_v21(schema)
}
/// v21 gate for the racetrack-dimension `choices` axes.
fn schema_at_least_v21(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V21 || schema_at_least_v22(schema)
}
/// v22 gate for the good-field-region uniformity bound.
fn schema_at_least_v22(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V22 || schema_at_least_v23(schema)
}
/// v23 gate for the good-field-region harmonic expansion.
fn schema_at_least_v23(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V23 || schema_at_least_v24(schema)
}
/// v24 gate for `cost.piece_policy`, piece offerings and `price_source`.
fn schema_at_least_v24(schema: &str) -> bool {
    schema == COUPLED_SEARCH_CASE_SCHEMA_V24
}
pub const OC007_JSON: &str = include_str!("../../../benchmarks/coupled/oc-007.json");
pub const OC007_CONTROL_JSON: &str =
    include_str!("../../../benchmarks/coupled/oc-007-control.json");
/// OC-008 (contract §2, Stage B): the frozen bracketing-refinement case,
/// written and hashed before any run. Embedding it here only parses/validates
/// its schema at load time; it does not run the search itself.
pub const OC008_JSON: &str = include_str!("../../../benchmarks/coupled/oc-008.json");
/// OC-019: the schema v9 planar-path fixture — a D-shape centerline
/// (inner leg + corner arcs + outer bulge), exercising `pack.path` and
/// `Station::Path` end to end.
pub const OC019_JSON: &str = include_str!("../../../benchmarks/coupled/oc-019.json");
/// OC-020: the schema v10 graded-pack fixture — two half-width radial
/// grading regions, a cheaper low-field-binned spec alongside the base
/// tape, exercising `tape_specs`/`grading` end to end.
pub const OC020_JSON: &str = include_str!("../../../benchmarks/coupled/oc-020.json");
/// OC-021: the schema v12 transverse-pressure fixture — OC-019's D-shape
/// at a 1.0 T bore target with a declared 30 MPa broad-face pressure
/// bound that eliminates the cheapest screening-passing candidate.
pub const OC021_JSON: &str = include_str!("../../../benchmarks/coupled/oc-021.json");
/// OC-022: the schema v10 two-dataset graded fixture — OC-020's graded
/// racetrack with the second spec bound to a physically distinct measured
/// dataset (`robinson-shanghai-hflt-v3`) at a declared premium price.
pub const OC022_JSON: &str = include_str!("../../../benchmarks/coupled/oc-022.json");
/// OC-023: the schema v10 three-dataset graded fixture — OC-022's case
/// with a third measured spec (`robinson-theva-ap-v2`) offered in both
/// regions on a trimmed geometry grid (the 9-assignment product must
/// stay under the 64-candidate work limit).
pub const OC023_JSON: &str = include_str!("../../../benchmarks/coupled/oc-023.json");

// Numerical work limits (contract S1/§8 D3), not manufacturing limits.
const MAX_CHOICE_VALUES: usize = 64;
const MAX_CANDIDATES: usize = 64;
const MAX_TURNS_ALONG_NORMAL: u32 = 100_000;
const MAX_TAPES_ALONG_WIDTH: u32 = 1_000;
/// Cable-class conductors run to a few hundred tape strands per
/// position; 4096 is generous headroom, not a physical claim.
const MAX_STRANDS_PARALLEL: u32 = 4_096;
const MAX_OFFSET: u32 = 1_000_000;
// OC-008 contract §2 B2 numerical work limits for the `refinement` block,
// not manufacturing or physical limits.
const MAX_PANCAKE_COUNTS: usize = 16;
const MAX_TURN_RESOLUTION: u32 = MAX_TURNS_ALONG_NORMAL;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoupledSearchCase {
    pub schema: String,
    pub id: String,
    pub provenance: String,
    pub requirement: Requirement,
    pub fixed_geometry: FixedGeometry,
    pub choices: Choices,
    pub operating: SearchOperating,
    /// The base conductor binding (reuses `coupled::MaterialSettings`
    /// verbatim: identical fields, same dataset/method/criterion identity
    /// checks). Applied to every turn no grading region covers — and to
    /// every turn at all when `grading` is absent.
    pub material: MaterialSettings,
    /// Schema v10 only: named alternative conductor bindings a grading
    /// region may select. Each spec carries its own `MaterialSettings`
    /// (dataset identity, method and policies are per-spec) and its own
    /// conductor price. Required iff `grading` is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape_specs: Option<std::collections::BTreeMap<String, TapeSpec>>,
    /// Schema v10 only: graded-material search plan — regions along
    /// `turns_along_normal` whose tape spec the search chooses per
    /// candidate. Turns outside every region keep the base `material`
    /// (the reserved choice id `"base"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grading: Option<Grading>,
    pub sampling: SearchSampling,
    /// Reuses `coupled::Limits` verbatim: the same screening limits/policies
    /// (contract D4: "exactly as OC-004").
    pub limits: Limits,
    pub numerics: SearchNumerics,
    pub pruning: Option<Pruning>,
    pub cost: Cost,
    /// Schema v20 only: declared refrigeration/opex economics for the
    /// operating point — see [`Opex`]. Absent on earlier schemas and on
    /// capex-only cases (the ledger then reports no opex/lifecycle
    /// figures at all — absent, never a defaulted zero).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opex: Option<Opex>,
    pub baseline: Baseline,
    /// OC-008 contract §2 (schema v2 only): per-pancake-count bracketing
    /// bisection of the cost optimum. `None` for a v1 case (`schema` must
    /// then be `COUPLED_SEARCH_CASE_SCHEMA_V1`); `Some` for v2 (`schema` must
    /// then be `COUPLED_SEARCH_CASE_SCHEMA_V2`). `#[serde(default)]` so every
    /// existing v1 case file (which never mentions this field) keeps parsing
    /// unchanged.
    #[serde(default)]
    pub refinement: Option<RefinementPlan>,
    /// Contract §9.4: the acceptance module's own finer re-sampling of the
    /// selected optimum and the baseline, declared here rather than derived
    /// implicitly, since the coarse sampling plan (`sampling`, above) cannot
    /// by itself establish the true weakest location.
    pub refined_plan: RefinedPlan,
    /// Schema v3 only: geometric feasibility limits the case author
    /// declares as manufacturing semantics. These are modeled screening
    /// constraints (a smaller feasible set), never evidence a real coil is
    /// manufacturable.
    #[serde(default)]
    pub manufacturing: Option<Manufacturing>,
    /// Schema v4 only: a declared mechanical load screen, applied after
    /// the field/screening evaluation. This is a first-order *screen*,
    /// not a stress analysis; passing never establishes structural
    /// soundness.
    #[serde(default)]
    pub mechanical: Option<Mechanical>,
    /// Schema v13 only: a customer-declared external field map evaluated
    /// instead of the engine's field solve — the same cylindrical-grid
    /// semantics as the coupled schema v7 `sampling.field_map`, plus the
    /// declared bore-field anchor the NI solve needs. Requiring fixed
    /// pack extents: the map is valid only for the winding it was
    /// computed on, so the search varies discretization and assignment,
    /// not the physical footprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_map: Option<SearchFieldMap>,
    /// Schema v15 only: declared current-sharing temperature-margin
    /// screen — `T_cs(B, θ) − T_op ≥ min_margin_k` at every sampled
    /// point, where `T_cs` is the temperature at which the point's
    /// interpolated critical sheet current equals the carried sheet
    /// current. A pure dataset root-find under the same query policy the
    /// capacity screen used (mirror pair + declared low-field clamp); no
    /// new physics is modeled.
    #[serde(default)]
    pub thermal_margin: Option<ThermalMarginScreen>,
    /// Schema v15 only: declared hysteretic AC-loss screen — Norris's
    /// 1970 strip transport term at the declared amplitude fraction, a
    /// Bean-slab term on the face-parallel field component using the
    /// declared layer thickness, and a rigorous perpendicular-field
    /// upper bound `4·m_sat·B_perp` from the strip's saturation moment
    /// (declared a bound, not a simulation). Aggregated as the sampled
    /// mean loss density times installed conductor length.
    #[serde(default)]
    pub ac_loss: Option<AcLossScreen>,
    /// Schema v15 only: adiabatic quench hot-spot bound under a declared
    /// exponential dump — `MIITs = (I_strand/A_stab)²·τ/2` resolved
    /// against the case-declared quench-integral table `U(T) =
    /// ∫₀^T C_p/ρ_e dT`. Not a protection model: dump waveform, normal-
    /// zone propagation and material data are all declared assumptions.
    #[serde(default)]
    pub quench_hotspot: Option<QuenchHotspotScreen>,
    /// Schema v15 only: Brandt–Indenbom strip screening-penetration flag —
    /// the flux-free core `b = a/cosh(B_perp/B_c)`, `B_c = μ₀·K_c/π` from
    /// the point's own critical sheet current, gives the penetrated width
    /// fraction `1 − 1/cosh(B_perp/B_c)`. Where the fraction is large the
    /// tape carries screening currents of order its transport sheet and
    /// the uniform-transport capacity figures are unreliable — a flag on
    /// the assumption's validity, not a screening-current simulation.
    #[serde(default)]
    pub screening_current: Option<ScreeningCurrentScreen>,
    /// Schema v16 only: the measured E–J transition screen. Each sampled
    /// point's operating ratio `u = K_demand/K_used` is evaluated against
    /// the covering dataset's own measured exponent `n` (interpolated on
    /// the same query policy as the capacity), giving
    /// `E/Ec = u^n` at the operating block's already-declared criterion
    /// `Ec` — how deep into the measured superconducting transition the
    /// point operates. The record additionally aggregates the per-strand
    /// terminal-voltage estimate `Σ E·dℓ` over the winding. Points whose
    /// capacity or exponent cannot resolve make the screen INCONCLUSIVE.
    #[serde(default)]
    pub transition: Option<TransitionScreen>,
    /// Schema v18 only: the lumped adiabatic quench-transient screen —
    /// a driven-dump `dT/dt = J_m²·ρ_e(T)/C_v(T)` trajectory per sampled
    /// point, with the matrix share `I_m = max(0, I_strand − I_sc(T))`
    /// resolved against the covering dataset's measured capacity at the
    /// evolving temperature. Reports the peak temperature reached and
    /// the per-strand resistive series voltage against a declared
    /// detection threshold. Quench initiation, normal-zone propagation
    /// and thermal coupling are unmodeled — a lumped bound, not a
    /// quench simulation.
    #[serde(default)]
    pub quench_transient: Option<QuenchTransientScreen>,
    pub execution: Execution,
}

/// Schema v18 only: declared lumped adiabatic quench-transient screen.
/// The model treats each sampled point as a uniform lumped strand:
/// under the declared exponential dump `I(t) = I_op·exp(−t/τ)` the
/// superconductor carries `I_sc = K_used(B, θ, T)·w` — re-queried at the
/// evolving temperature — and the remainder flows in the declared
/// conducting composite, heating it adiabatically by `J_m²·ρ_e(T)/C_v(T)`.
/// Properties are declared per-temperature tables for the composite the
/// case author defines (stabilizer, substrate, shared matrix folded into
/// one effective section): this is a driven-dump bound, not a protection
/// study — quench initiation, normal-zone propagation, cryogen cooling,
/// insulation, and the dump's effect on the point's field are unmodeled
/// (the sampled query field is frozen, which is conservative for
/// detection since a decaying field would raise capacity).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuenchTransientScreen {
    /// Exponential dump time constant (s): `I(t) = I_op·exp(−t/τ)`.
    pub dump_time_constant_s: f64,
    /// Optional initial lumped temperature (K): the dump begins with
    /// each sampled point already at `T₀` — the declared "quench
    /// detected, protection fires" boundary condition. Must be ≥ the
    /// operating temperature and inside both property tables' spans.
    /// Absent: the trajectory starts at `operating.temperature_k` and
    /// sharing develops only where demand exceeds capacity at T_op —
    /// a correct "dump alone" answer, though screened candidates
    /// (u < 1) then report a static, unheated winding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_temperature_k: Option<f64>,
    /// Effective conducting cross-section per unit tape width (m) —
    /// `A_c = conducting_area_per_width_m × tape_width_m` per strand:
    /// the composite section the matrix share flows through and heats.
    /// Declared, not derived: the case author folds stabilizer,
    /// substrate and any shared matrix into one effective area whose
    /// `ρ_e(T)` and `C_v(T)` tables below describe.
    pub conducting_area_per_width_m: f64,
    /// Effective resistivity table `[[T_k, ρ_k], …]` in Ω·m: ≥ 2 rows,
    /// `T` strictly increasing, `ρ` positive — piecewise-linear in `T`,
    /// bracketing `operating.temperature_k`. No extrapolation: a
    /// trajectory that leaves the table is INCONCLUSIVE.
    pub resistivity_ohm_m: Vec<[f64; 2]>,
    /// Effective volumetric heat-capacity table `[[T_k, C_k], …]` in
    /// J·m⁻³·K⁻¹: ≥ 2 rows, `T` strictly increasing, `C` positive —
    /// piecewise-linear in `T`, bracketing `operating.temperature_k`.
    pub heat_capacity_j_per_m3k: Vec<[f64; 2]>,
    /// Optional bound (K) on the peak temperature any point reaches
    /// during the dump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_temperature_k: Option<f64>,
    /// Optional per-strand resistive terminal-voltage threshold (V):
    /// the series voltage `Σ J_m·ρ_e·w_q·dℓ` the protection system is
    /// declared able to see. Recorded as the first crossing time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_voltage_v: Option<f64>,
    /// Optional bound (s): a declared requirement that the resistive
    /// signature cross `detection_voltage_v` within this time. Requires
    /// `detection_voltage_v`; a real sharing signature that never
    /// crosses fails the bound, and a declared quench
    /// (`initial_temperature_k`) that produces no signature fails
    /// detectability by construction — a dump alone with no sharing is
    /// vacuous (no quench occurred in the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_detection_time_s: Option<f64>,
}

/// Schema v16 only: declared E–J transition screen over the datasets'
/// own measured `n_value` channel. The power law `E = Ec·u^n` is the
/// empirical transition law the datasets' Ic/n pairs were fitted under;
/// `Ec` is the operating block's `electric_field_criterion_v_per_m`,
/// already validated to match the covering dataset's measurement
/// criterion. This screen applies the law at the measured exponent —
/// a first-order screen, never a current-sharing simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionScreen {
    /// Worst-point bound on `E/Ec = u^n`: how deep into the measured
    /// transition any sampled point may operate. 1.0 forbids operation
    /// above the measurement criterion; larger values tolerate declared
    /// current-sharing depth.
    pub max_e_over_ec: f64,
    /// Optional bound (V) on the per-strand terminal-voltage estimate —
    /// `Σ_points E·w_q·dℓ` over the winding's sampled tape length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_voltage_v: Option<f64>,
}

/// Schema v15 only: declared temperature-margin screen. `T_cs` is
/// evaluated per sampled point by root-finding the covering spec's
/// interpolated critical sheet current in temperature at the point's
/// recorded query field and angles; points whose `T_cs` lies beyond the
/// dataset's temperature span report the span edge as a lower bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThermalMarginScreen {
    /// Minimum allowable `T_cs − T_op` (K); a point below it fails.
    pub min_margin_k: f64,
}

/// Schema v15 only: declared hysteretic AC-loss screen (Norris 1970
/// strip transport term + Bean slab on the face-parallel component +
/// the strip saturation-moment bound on the face-normal component).
/// Power is quoted at the declared swing frequency under a full
/// 0→operating-point field swing — a conservative upper bound for any
/// partial ripple, since every term is monotone in amplitude.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcLossScreen {
    /// Swing/ramp frequency the loss power is quoted at (Hz).
    pub frequency_hz: f64,
    /// Norris transport amplitude `i = I_ac/I_c` as a fraction of the
    /// point's *local* critical current, in `(0, 1]`.
    pub transport_amplitude_fraction: f64,
    /// Declared superconducting-layer thickness (m) for the Bean-slab
    /// face-parallel term (`J_c = K_c/d_sc`); declared, never derived.
    pub sc_layer_thickness_m: f64,
    /// Declared loss budget (W) the aggregate estimate-plus-bound gates
    /// against.
    pub max_loss_w: f64,
}

/// Schema v15 only: adiabatic quench hot-spot bound under a declared
/// exponential dump. `MIITs = J_strand²·τ/2` with `J_strand =
/// I_strand/stabilizer_area_m2`; the hot spot is the interpolated `T`
/// where `U(T) = U(T_op) + MIITs`. If the declared table does not reach
/// that `U` the screen reports INCONCLUSIVE — never an extrapolated
/// temperature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuenchHotspotScreen {
    /// Exponential dump time constant (s): `I(t) = I_op·exp(−t/τ)`.
    pub dump_time_constant_s: f64,
    /// Normal-zone current-carrying cross-section per strand (m²) —
    /// declared, not derived: the case author states the load path
    /// (stabilizer, shared structure) explicitly. Where strands differ
    /// across a graded pack, declare the smallest applicable area.
    pub stabilizer_area_m2: f64,
    /// Quench-integral table `[[T_k, U_k], …]` with `U(T) = ∫₀^T C_p/ρ_e
    /// dT` in A²·s/m⁴: ≥ 2 rows, `T` strictly increasing, `U`
    /// nondecreasing, and the table must bracket the operating
    /// temperature. Interpolation is piecewise-linear in `(T, U)`.
    pub quench_function_a2s_per_m4: Vec<[f64; 2]>,
    /// Declared hot-spot temperature limit (K).
    pub max_hotspot_k: f64,
}

/// Schema v15 only: screening-current penetration flag on the
/// Brandt–Indenbom thin-strip result — where the face-normal field
/// exceeds the strip's `B_c = μ₀·K_c/π` by enough that the declared
/// penetrated-width fraction is exceeded, the uniform-transport
/// assumption behind the capacity screen is unreliable and the
/// candidate fails the flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreeningCurrentScreen {
    /// Maximum allowable `1 − 1/cosh(B_perp/B_c)` over sampled points,
    /// in `(0, 1]`.
    pub max_penetrated_width_fraction: f64,
}

/// Schema v13 only: the declared field map plus the bore-field anchor.
/// The map is a pack-region field solution; the bore probe lies outside
/// its hull, so the requirement's ampere-turns come from the map
/// producer's own declared bore field at the reference winding:
/// `NI = reference_ampere_turns_a · b_target_t / bore_field_at_reference_t`
/// (magnetostatic linearity). That anchor is declared, not verified —
/// the record says so.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchFieldMap {
    /// The declared field map, with coupled schema v7 semantics —
    /// validated against the declared fixed extents at case validation.
    pub map: crate::coupled::FieldMap,
    /// Field magnitude (tesla) the reference winding produces at
    /// `requirement.bore_probe_m` under `map.reference_ampere_turns_a` —
    /// the map producer's own calibrated value.
    pub bore_field_at_reference_t: f64,
}

/// Schema v4 only: declared mechanical screening limits applied to every
/// candidate whose evaluation produced finite field/current. A violation
/// is recorded (`mechanical_feasible: false`) and the candidate is FAIL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mechanical {
    /// Maximum allowable Lorentz load per unit length at the pack's
    /// worst sampled point: `I_op * peak_sampled_field_t` must stay <=
    /// this (N/m). Models the stack-level Lorentz-loading limit qualified
    /// conductors are published against (~300-850 kN/m for
    /// fusion-relevant HTS cable units); the declared value is the case
    /// author's assumption, not a qualified limit.
    pub max_lorentz_load_n_per_m: f64,
    /// Schema v7 only: maximum allowable hoop stress (Pa). The conductor's
    /// hoop tension under its own Lorentz load is T = f·R (the same
    /// mechanics as a pressure vessel), evaluated at the worst sampled
    /// field and the pack's outermost turn radius:
    /// `σ = (I_op · B_peak) · (bend_radius + n·radial_pitch/2) /
    /// tension_section_area_m2`. REBCO limits are published as stresses —
    /// axial tensile of order 450-700 MPa, transverse/delamination far
    /// lower; the declared value is the case author's assumption and the
    /// direction it bounds is the conductor's own hoop direction.
    /// Requires `tension_section_area_m2`.
    #[serde(default)]
    pub max_hoop_stress_pa: Option<f64>,
    /// Schema v7 only: the load-bearing cross-section (m²) associated with
    /// one conductor strand — the area the hoop tension is divided into.
    /// Deliberately declared rather than derived: winding-pack fill,
    /// substrate thickness and any support structure are unmodeled, so
    /// the case author states the load path explicitly (e.g. the tape's
    /// substrate-plus-stabilizer area for a self-supporting winding, or a
    /// larger declared section when a structure shares the load).
    /// Required iff `max_hoop_stress_pa` is declared.
    #[serde(default)]
    pub tension_section_area_m2: Option<f64>,
    /// Schema v12 only: maximum allowable transverse pressure (Pa) on the
    /// tape's broad face — the delamination direction for REBCO. The pack's
    /// signed normal Lorentz loads `I_op·⟨B·ŵ⟩` per turn are accumulated
    /// along the stacking direction; the peak interface pressure is the
    /// largest cumulative load any interface could transmit, divided by
    /// the tape-width interface area per unit conductor length. A
    /// first-order declared bound, not a stress analysis — structure,
    /// turn-level contact details and the true support share carried by
    /// hoop tension are unmodeled.
    #[serde(default)]
    pub max_transverse_pressure_pa: Option<f64>,
    /// Schema v17 only: maximum allowable membrane tension resultant
    /// (N per metre of interface width) — the peak cumulative signed
    /// normal Lorentz load any turn-boundary interface could transmit
    /// under either face-anchored support convention, *before* the
    /// division by interface width that produces
    /// `max_transverse_pressure_pa`'s pressure. For a hoop-wound pack
    /// this is the first-order diametral bursting tension per unit
    /// axial length, `∫(J×B)·dn` over the turn stack — the load a
    /// winding's structure or the conductor itself must carry as a
    /// membrane. A declared bound, not a stress analysis: which faces
    /// are anchored, friction, insulation compliance and load sharing
    /// with structure are unmodeled — the bound conservatively takes
    /// the larger of the two support conventions.
    #[serde(default)]
    pub max_membrane_tension_n_per_m: Option<f64>,
}

/// Schema v3 only: declared geometric feasibility limits, applied to every
/// candidate before its requirement evaluation. A violation is recorded
/// (`manufacturing_feasible: false`) and the candidate is FAIL -- never
/// silently dropped from the search.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manufacturing {
    /// Minimum allowable inner bend radius: `bend_radius_m -
    /// n*radial_pitch_m/2` must stay >= this for every candidate. Models a
    /// tape's minimum bend radius (REBCO degrades sharply below a
    /// width-dependent radius); the declared value is the case author's
    /// assumption, not a measured limit.
    pub min_inner_bend_radius_m: f64,
    /// Schema v17 only: the tape's total thickness (m) — conductor,
    /// substrate, stabilizer and any declared laminations as one
    /// bendable stack. Declared, not derived: tape construction is not
    /// modeled, and the strain convention below treats the declared
    /// stack as a homogeneous bent sheet. Required iff
    /// `max_bend_strain` is declared.
    #[serde(default)]
    pub tape_thickness_m: Option<f64>,
    /// Schema v17 only: maximum allowable outer-fiber bending strain
    /// (dimensionless) at the pack's innermost bend:
    /// `tape_thickness_m / (2 · inner_bend_radius_m)` — the elastic-beam
    /// estimate of the peak surface strain in the bent tape stack,
    /// compared against the declared limit (REBCO irreversible-strain
    /// limits are published this way, ~0.003-0.005). A first-order
    /// declared bound: neutral-axis offset, transverse anisotropy and
    /// residual strain from the wound state are unmodeled. Requires
    /// `tape_thickness_m`.
    #[serde(default)]
    pub max_bend_strain: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub bore_probe_m: [f64; 3],
    pub b_target_t: f64,
    /// Sanity tolerance on `NI * unit_bore_bz` reproducing `b_target_t`
    /// (contract R1's "meet the requirement exactly"); not a physical margin
    /// since NI is solved algebraically to hit the target.
    pub tolerance_fraction: f64,
    /// Schema v3 only: the declared usable volume. `B_z` must meet
    /// `b_target_t` at every point of the region's probe lattice; the
    /// operating `NI` is solved on the lattice's weakest point rather than
    /// the bare bore probe. Absent on v1/v2 cases, which keep the
    /// single-point semantics exactly.
    #[serde(default)]
    pub good_field_region: Option<GoodFieldRegion>,
}

/// A box of probe points centered on `bore_probe_m` over which the
/// field requirement holds (schema v3). Sampling a finite lattice is not a
/// guarantee over the continuum; the region's recorded weakest point is the
/// lattice minimum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoodFieldRegion {
    /// Per-axis half extents of the box, centered on `bore_probe_m`.
    pub half_extents_m: [f64; 3],
    /// Probe count per axis, `2..=9`; the lattice is the `n^3` tensor
    /// product of evenly spaced positions covering the box endpoints.
    pub points_per_axis: u32,
    /// Schema v22: declared uniformity bound over the lattice —
    /// `(Bmax − Bmin) / B_center` evaluated on unit fields (scale-free
    /// in NI). The bound lives inside the region declaration because it
    /// is a property of the usable volume itself: the field target at
    /// the weakest point and the spread across the volume are the same
    /// customer spec written two ways. `None` = unbounded (recorded
    /// deviation only). Rejected under a declared `field_map` with the
    /// region itself — the map covers the pack, not a usable volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_relative_deviation: Option<f64>,
    /// Schema v23: a declared cylindrical harmonic expansion of `B_z`
    /// on a reference circle in the region midplane (z = probe z, the
    /// x–y plane through the region center). The coefficients are the
    /// accelerator-spec form — normal `b_n` and skew `a_n` in units
    /// relative to the circle's own mean field — evaluated at the fine
    /// quadrature order. Bounds are optional: a customer may declare
    /// the expansion purely to record the spectrum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harmonics: Option<RegionHarmonics>,
}

/// Schema v23: declaration of the midplane reference-circle harmonic
/// expansion for a good-field region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionHarmonics {
    /// Radius of the evaluation circle — must lie strictly inside the
    /// region's in-plane half extents so every sample is a usable-volume
    /// point.
    pub reference_radius_m: f64,
    /// Highest harmonic order reported, `1..=16`.
    pub max_order: u32,
    /// Equally spaced azimuthal samples on the circle. Must satisfy
    /// Nyquist for the declared orders: `>= 2 * max_order + 1` — the
    /// DFT resolves orders `1..=max_order` exactly, higher content
    /// aliases by construction (declared, not hidden).
    pub theta_samples: u32,
    /// Bound on `max_{1<=n<=max_order} |b_n| / b_0` — the normal-unit
    /// spec figure (a fraction; the accelerator "units of 1e-4" divide
    /// by 1e4). `None` = unbounded, spectrum recorded only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_normal_unit_fraction: Option<f64>,
    /// Same bound applied to the skew `a_n` coefficients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_skew_unit_fraction: Option<f64>,
}

impl GoodFieldRegion {
    /// The `points_per_axis^3` probe positions: the box corners, edge
    /// midpoints, face centers and interior, centered on `probe`.
    pub fn lattice_points(&self, probe: [f64; 3]) -> Vec<[f64; 3]> {
        let n = self.points_per_axis as usize;
        let axis_positions = |h: f64| {
            (0..n)
                .map(|i| -h + 2.0 * h * i as f64 / (n - 1) as f64)
                .collect::<Vec<f64>>()
        };
        let (xs, ys, zs) = (
            axis_positions(self.half_extents_m[0]),
            axis_positions(self.half_extents_m[1]),
            axis_positions(self.half_extents_m[2]),
        );
        let mut points = Vec::with_capacity(n * n * n);
        for &x in &xs {
            for &y in &ys {
                for &z in &zs {
                    points.push([probe[0] + x, probe[1] + y, probe[2] + z]);
                }
            }
        }
        points
    }

    /// True iff any lattice point lies inside the winding pack itself --
    /// in-plane inside the racetrack's conductor band and within the
    /// pack's axial extent. A region overlapping the winding is not a
    /// usable volume: the requirement cannot be honestly evaluated there,
    /// so the candidate fails rather than reporting a meaningless lattice
    /// minimum. The band is the annulus between the inner and outer pack
    /// faces: `r - w/2 ..= r + w/2` around each straight's own axis line
    /// `y = +/-bend_radius` (for `|x| <= l`) or around each bend center
    /// `(+/-l, 0)` (for `|x| > l`).
    pub fn overlaps_pack(
        &self,
        probe: [f64; 3],
        straight_half_length_m: f64,
        bend_radius_m: f64,
        radial_width_m: f64,
        axial_height_m: f64,
    ) -> bool {
        let inner = bend_radius_m - radial_width_m / 2.0;
        let outer = bend_radius_m + radial_width_m / 2.0;
        let l = straight_half_length_m;
        self.lattice_points(probe).iter().any(|p| {
            if p[2].abs() > axial_height_m / 2.0 {
                return false;
            }
            if p[0].abs() <= l {
                (inner..=outer).contains(&p[1].abs())
            } else {
                let dx = p[0].abs() - l;
                let rho = (dx * dx + p[1] * p[1]).sqrt();
                (inner..=outer).contains(&rho)
            }
        })
    }

    /// The path analogue of `overlaps_pack` (schema v9): a lattice point
    /// lies inside the pack iff its in-plane distance to the centerline is
    /// at most `radial_width_m/2` and `|z| <= axial_height_m/2`. An
    /// un-evaluable path counts as overlap — the screen fails closed
    /// rather than certifying a region it could not inspect.
    pub fn overlaps_pack_on_path(
        &self,
        probe: [f64; 3],
        path: &CoilPath,
        radial_width_m: f64,
        axial_height_m: f64,
    ) -> bool {
        self.lattice_points(probe).iter().any(|p| {
            if p[2].abs() > axial_height_m / 2.0 {
                return false;
            }
            match path.distance_to_centerline_m([p[0], p[1]]) {
                Ok(distance_m) => distance_m <= radial_width_m / 2.0,
                Err(_) => true,
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedGeometry {
    /// Legacy racetrack dimensions — mutually exclusive with `path` and
    /// absent on schema v9 path cases (serde `Option` keeps old JSON
    /// byte-identical: `Some(0.3)` serializes as `0.3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straight_half_length_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_radius_m: Option<f64>,
    /// General planar centerline (schema v9). When present the pack is a
    /// `radial_width × axial_height` band straddling the path, `rho`
    /// coordinates become signed normal offsets, and stations address the
    /// centerline by arc length (`Station::Path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<CoilPath>,
    /// Non-planar helix centerline (schema v19) — `CoilPath3D` for
    /// CCT/CORC-class windings. Mutually exclusive with `path` and the
    /// racetrack dims; requires a declared Cartesian `field_map` and
    /// `tape_normal` `radial`. `rho` coordinates are signed
    /// cylinder-radial offsets; the width direction is the in-surface
    /// binormal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path3d: Option<crate::path3d::CoilPath3D>,
    pub radial_pitch_m: f64,
    pub tape_width_m: f64,
    pub tape_normal: TapeNormal,
    /// Schema v13 only: the fixed physical pack extents (m). Required iff
    /// `field_map` is declared and forbidden otherwise — under a declared
    /// map the pack occupies exactly the region the map was computed for.
    /// The choices then vary the tape discretization: the generated
    /// coupled case's pitch and tape width are derived per candidate as
    /// extent/count. `radial_pitch_m`/`tape_width_m` remain required and
    /// describe the *baseline* winding: consistency requires
    /// `baseline counts × declared pitch/width == these extents` in each
    /// direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_radial_width_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_axial_height_m: Option<f64>,
}

impl FixedGeometry {
    /// Smallest inner-face curvature radius for a pack of
    /// `radial_width_m`: `bend_radius - w/2` on a racetrack, `min local
    /// curvature radius - w/2` on a planar path, the inner-edge helix
    /// curvature radius `((R−w/2)²+c²)/(R−w/2)` on a `path3d`. `None`
    /// when no geometry is declared (an invalid case — `validate`
    /// rejects it first).
    pub fn min_inner_radius_m(&self, radial_width_m: f64) -> Option<f64> {
        match (&self.path, &self.path3d, self.bend_radius_m) {
            (Some(path), None, None) => Some(path.min_radius_m() - radial_width_m / 2.0),
            (None, Some(path3d), None) => Some(path3d.min_inner_radius_m(radial_width_m)),
            (None, None, Some(bend_radius_m)) => Some(bend_radius_m - radial_width_m / 2.0),
            _ => None,
        }
    }

    /// Largest outer-face curvature radius — the characteristic turn
    /// radius feeding the first-order hoop-stress bound. On a racetrack
    /// this is `bend_radius + w/2`; on a planar path the maximum arc
    /// radius is the conservative choice (the bound scales with radius);
    /// on a `path3d` the outer-edge helix curvature radius.
    pub fn max_outer_radius_m(&self, radial_width_m: f64) -> Option<f64> {
        match (&self.path, &self.path3d, self.bend_radius_m) {
            (Some(path), None, None) => Some(path.max_radius_m() + radial_width_m / 2.0),
            (None, Some(path3d), None) => Some(path3d.max_outer_radius_m(radial_width_m)),
            (None, None, Some(bend_radius_m)) => Some(bend_radius_m + radial_width_m / 2.0),
            _ => None,
        }
    }

    /// The candidate's pack extents `(radial_width_m, axial_height_m)`:
    /// the declared fixed extents under a v13 `field_map`, else
    /// `turns·radial_pitch` / `tapes·tape_width`. Under fixed extents the
    /// counts vary the discretization, not the footprint.
    pub fn candidate_extents_m(&self, turns: u32, tapes: u32) -> (f64, f64) {
        match (self.pack_radial_width_m, self.pack_axial_height_m) {
            (Some(w), Some(h)) => (w, h),
            _ => (
                f64::from(turns) * self.radial_pitch_m,
                f64::from(tapes) * self.tape_width_m,
            ),
        }
    }

    /// Schema v21: this declaration with the candidate's axis-resolved
    /// racetrack dims shadowed in — every downstream consumer reads the
    /// returned geometry, so fixed and searched dims take one path.
    /// A `None` arm leaves the field untouched (the fixed declaration
    /// stands, or stays absent on path/path3d/map cases).
    pub fn resolved_for(&self, dims: CandidateDims) -> FixedGeometry {
        let mut geometry = self.clone();
        if let Some(b) = dims.bend_radius_m {
            geometry.bend_radius_m = Some(b);
        }
        if let Some(s) = dims.straight_half_length_m {
            geometry.straight_half_length_m = Some(s);
        }
        geometry
    }

    /// The candidate's winding tape width (m): under fixed extents the
    /// width-direction extent divided by the tape count — the width
    /// direction is axial under a radial tape normal and radial under an
    /// axial one — else the declared `tape_width_m`.
    pub fn candidate_tape_width_m(&self, tapes: u32) -> f64 {
        match (self.pack_radial_width_m, self.pack_axial_height_m) {
            (Some(w), Some(h)) => {
                let width_extent = match self.tape_normal {
                    TapeNormal::Radial => h,
                    TapeNormal::Axial => w,
                };
                width_extent / f64::from(tapes)
            }
            _ => self.tape_width_m,
        }
    }
}

/// Schema v21: one candidate's axis-resolved racetrack dimensions. Each
/// arm is `Some(axis_value)` only when the case declares the
/// corresponding `choices` axis; `None` means the fixed geometry's own
/// declaration applies (and is left untouched by
/// `FixedGeometry::resolved_for`).
#[derive(Debug, Clone, Copy, Default)]
pub struct CandidateDims {
    pub bend_radius_m: Option<f64>,
    pub straight_half_length_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choices {
    pub turns_along_normal: Vec<u32>,
    pub tapes_along_width: Vec<u32>,
    /// Conductors per turn sharing the operating current (schema v4);
    /// absent/`[1]` means one tape per turn (all earlier schemas).
    #[serde(default)]
    pub strands_parallel: Option<Vec<u32>>,
    /// Schema v21 only: racetrack bend-radius values (m) the search
    /// enumerates — magnet size as a declared choice dimension. Requires
    /// the corresponding `fixed_geometry` field absent, no declared
    /// `path`/`path3d`/`field_map`, and a `baseline` value in the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_radius_m: Option<Vec<f64>>,
    /// Schema v21 only: racetrack straight half-length values (m) — same
    /// rules as `bend_radius_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straight_half_length_m: Option<Vec<f64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchOperating {
    pub temperature_k: f64,
    pub electric_field_criterion_v_per_m: f64,
}

/// A relative turn-index specification (contract R3): resolved against a
/// candidate's own `turns_along_normal` at run time, never stored as an
/// absolute index. `FromStart`/`FromEnd` offsets are 1-based turn counts
/// from either end; `FromEnd { offset: 0 }` names the last turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelativeTurnIndex {
    FromStart { offset: u32 },
    Fraction { value: f64 },
    FromEnd { offset: u32 },
}

impl RelativeTurnIndex {
    fn validate(&self) -> Result<(), ModelError> {
        match self {
            RelativeTurnIndex::FromStart { offset } => require(
                (1..=MAX_OFFSET).contains(offset),
                "relative turn index from_start offset must be within 1..=1_000_000",
            ),
            RelativeTurnIndex::FromEnd { offset } => require(
                *offset <= MAX_OFFSET,
                "relative turn index from_end offset must be within 0..=1_000_000",
            ),
            RelativeTurnIndex::Fraction { value } => require(
                value.is_finite() && (0.0..=1.0).contains(value),
                "relative turn index fraction must be finite and within 0..=1",
            ),
        }
    }

    /// Resolve against a specific candidate's turn count, clipped to
    /// `1..=total_turns` (contract §8 D4: "clipped to 1..=n").
    pub fn resolve(&self, total_turns: u32) -> u32 {
        let n = i64::from(total_turns);
        let raw = match self {
            RelativeTurnIndex::FromStart { offset } => i64::from(*offset),
            RelativeTurnIndex::Fraction { value } => (value * total_turns as f64).round() as i64,
            RelativeTurnIndex::FromEnd { offset } => n - i64::from(*offset),
        };
        raw.clamp(1, n.max(1)) as u32
    }
}

/// Expand and dedup-sort a relative turn-index list against a candidate's own
/// turn count (contract §8 D4).
pub fn expand_relative_turn_indices(specs: &[RelativeTurnIndex], total_turns: u32) -> Vec<u32> {
    let mut out: Vec<u32> = specs.iter().map(|s| s.resolve(total_turns)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Contract §9.4 (amended): the refined-plan turn selection -- the coarse plan's own indices united with every 5th turn from
/// the start (5, 10, 15, ..., up to `n`) plus 1, 2, 3, `n-2`, `n-1`, `n`,
/// deduplicated and clipped to `1..=n`. A fixed algorithm parameterized only
/// by the candidate's own turn count, not a per-case declared list: the
/// contract spells this rule out literally, unlike `relative_turn_indices`.
///
/// The literal step of 5 is widened, only as far as needed, when it would
/// list more than 64 indices -- `coupled::Sampling`'s own frozen
/// `MAX_SAMPLED_INDICES` cap on the turn axis (shared with OC-004, not
/// something this module may raise; the width axis carries its own
/// larger `MAX_SAMPLED_TAPE_INDICES` for pancake-class windings). With up to 6 mandatory endpoints reserving 6 of the
/// 64 slots, a step below `n/58` could exceed that cap; among the D3
/// choice set `{120, 160, 200, 240, 300}` this only actually widens the
/// step for `n = 300` (step 5 alone lists 65 indices there: step becomes
/// 6), every smaller declared choice keeps exactly step 5.
pub fn refined_plan_turn_indices(total_turns: u32, coarse_turn_indices: &[u32]) -> Vec<u32> {
    // Contract §9.4 (amended): the refined plan is the UNION of the coarse
    // search plan's own turn indices with every `step`-th turn and the
    // near-face turns {1, 2, 3, n-2, n-1, n}, so the refined minimum is
    // always taken over a superset of the coarse points (shortfall >= 0 by
    // construction). `step` starts at 5 and widens only as far as needed to
    // respect `coupled::Sampling`'s 64-index cap.
    let n = i64::from(total_turns.max(1));
    let mut step = 5_i64;
    loop {
        let mut raw: Vec<i64> = vec![1, 2, 3, n - 2, n - 1, n];
        raw.extend(coarse_turn_indices.iter().map(|&v| i64::from(v)));
        let mut k = step;
        while k <= n {
            raw.push(k);
            k += step;
        }
        let mut out: Vec<u32> = raw
            .into_iter()
            .filter(|&v| (1..=n).contains(&v))
            .map(|v| v as u32)
            .collect();
        out.sort_unstable();
        out.dedup();
        if out.len() <= 64 || step >= n {
            return out;
        }
        step += 1;
    }
}

/// Contract §9.4: the refined plan's full 10-station list -- the search's
/// own 4 declared stations plus `refined_plan.additional_stations`'s 6.
pub fn refined_plan_stations(search: &CoupledSearchCase) -> Vec<Station> {
    search
        .sampling
        .stations
        .iter()
        .cloned()
        .chain(search.refined_plan.additional_stations.iter().cloned())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSampling {
    pub stations: Vec<Station>,
    pub relative_turn_indices: Vec<RelativeTurnIndex>,
    /// Fixed at 5: the only width rule this schema supports is the shared
    /// 5-point Gauss-Lobatto rule `coupled::WidthQuadrature::GaussLobatto`
    /// already reuses.
    pub width_points: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchNumerics {
    pub quadrature_orders: [u32; 2],
    pub field_scale_t: f64,
    pub max_refinement_change_fraction: f64,
}

/// Conservative-only coarse pre-screen (contract §8 D5). Tapes are always
/// the geometry's own first and last pancake ({1, tapes_along_width}), not a
/// declared choice, so there is no `coarse_tapes` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pruning {
    pub coarse_stations: Vec<String>,
    pub coarse_turn_fractions: Vec<RelativeTurnIndex>,
    /// 0-based indices into the 5-point Gauss-Lobatto rule.
    pub coarse_width_indices: Vec<u32>,
}

/// Schema v24: provenance class for a declared price — keeps synthetic
/// numbers distinguishable from published or vendor-quoted figures in
/// the case (and therefore in every record that embeds the case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// Invented placeholder — present in the record so nobody mistakes
    /// the dollar figures for procurement data.
    Synthetic,
    /// An analyst's estimate (interpolation, folklore pricing).
    Estimated,
    /// A published figure (datasheet, paper, public price list).
    Published,
    /// A vendor quote for this program.
    Quoted,
}

/// Schema v24: which conductor stream a purchased piece covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceUnit {
    /// A piece is one conductor unit — a cable or a wound tape stack
    /// carrying all `strands_parallel` strands at once: a splice joins
    /// them together, so splice counts are per unit.
    ConductorUnit,
    /// Each strand is bought and spliced independently — splice counts
    /// scale by `strands_parallel`.
    PerStrand,
}

/// Schema v24: whether purchased pieces may span module (tapes-axis)
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceBoundary {
    /// Each module starts a fresh piece — stack-in-plate pancakes wound
    /// plate by plate. The `tapes - 1` module-interface joints still
    /// apply.
    PerModule,
    /// The conductor runs the whole winding — layer-wound solenoids and
    /// continuously wound coils carry pieces across module boundaries,
    /// and there are no module-interface joints inside the winding.
    Continuous,
}

/// Schema v24: one vendor piece offering — conductor-unit pieces of
/// `length_m` sold at `price_usd_per_m`. When a spec resolves to a
/// multi-entry catalogue, the ledger picks the offering minimizing that
/// spec's own piece spend plus splice cost (fewer splices at longer
/// lengths vs the length premium is the trade it prices).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PieceOffering {
    pub length_m: f64,
    pub price_usd_per_m: f64,
}

/// Schema v24: piece-quantized procurement. Absent preserves the legacy
/// `installed × (1 + scrap)` ledger bit-identically. When declared, tape
/// is bought in discrete pieces: `purchased_length_m` is the pieces
/// actually bought, the remnant beyond installed-plus-attrition is
/// reported, and in-winding splices are counted and priced at
/// `splice_cost_usd` separately from module-interface joints.
///
/// Restrictions: the piece walk is defined for the radial tape-normal
/// winding topology with no declared `field_map` (all current benchmark
/// windings); axial-normal/folded topologies keep the legacy ledger —
/// declaring the policy on those is a validation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PiecePolicy {
    /// The base conductor's piece length when no `piece_offerings`
    /// catalogue is declared. Mutually exclusive with
    /// `cost.piece_offerings`; at least one base-level piece source is
    /// required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_length_m: Option<f64>,
    /// Whether pieces may span module boundaries.
    pub boundary: PieceBoundary,
    /// Whether a piece carries all strands or one.
    pub piece_unit: PieceUnit,
    /// Cost of one in-winding splice (piece exhaustion or spec change) —
    /// distinct from `joint_cost_usd`, which prices module interfaces.
    pub splice_cost_usd: f64,
}

/// Schema v10: a named conductor spec — a full `MaterialSettings` binding
/// plus the conductor price the cost ledger applies to turns assigned it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TapeSpec {
    pub material: MaterialSettings,
    /// The unquantized per-metre rate. Under `cost.piece_policy` with no
    /// `piece_offerings` of its own, a spec prices its pieces at this;
    /// when `piece_offerings` is declared, the chosen offering's price
    /// governs the spec's spend and this field is the list price kept for
    /// schema compatibility.
    pub price_usd_per_m: f64,
    /// Schema v24: this spec's piece catalogue — `(length_m,
    /// price_usd_per_m)` offerings. Overrides `price_usd_per_m` for the
    /// spec's spend when `cost.piece_policy` is declared; the ledger
    /// argmins the catalogue on the spec's own spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_offerings: Option<Vec<PieceOffering>>,
    /// Schema v24: provenance class of `price_usd_per_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_source: Option<PriceSource>,
}

/// The reserved `tape_spec_choices` id selecting the case-level `material`
/// binding at `cost.price_usd_per_m` — the ungraded baseline conductor.
pub const BASE_TAPE_SPEC_ID: &str = "base";

/// Schema v10: one graded region — the search picks one of
/// `tape_spec_choices` for the turns `turn_range` covers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GradingRegion {
    /// `[lo, hi]` as fractions of the candidate's `turns_along_normal`:
    /// region covers turns `floor(lo*N) < k <= floor(hi*N)` (1-based),
    /// counted from the pack's inner face outward. `0 <= lo < hi <= 1`.
    pub turn_range: [f64; 2],
    /// The spec ids the search may assign this region: `tape_specs` keys
    /// or the reserved `BASE_TAPE_SPEC_ID`.
    pub tape_spec_choices: Vec<String>,
}

/// Schema v10: graded-material declaration. Regions are sorted,
/// non-overlapping, and each contributes a choice factor to the candidate
/// grid; turns no region covers are bound to `material`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grading {
    pub regions: Vec<GradingRegion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cost {
    /// Conductor price applied to every turn no grading region covers —
    /// and to all turns when `grading` is absent (schema v10 regions
    /// priced under `tape_specs[..].price_usd_per_m` instead).
    pub price_usd_per_m: f64,
    pub scrap_fraction: f64,
    pub assembly_cost_per_pancake_usd: f64,
    /// Module-interface joint cost (the `tapes - 1` pancake joints under
    /// `per_module`; unused under `continuous` — a continuous winding has
    /// no module interfaces inside it).
    pub joint_cost_usd: f64,
    /// Schema v24: piece-quantized procurement — `None` preserves the
    /// legacy ledger bit-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_policy: Option<PiecePolicy>,
    /// Schema v24: the base conductor's piece catalogue. Mutually
    /// exclusive with `piece_policy.piece_length_m`; the chosen
    /// offering's price governs the base spec's spend under the piece
    /// ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_offerings: Option<Vec<PieceOffering>>,
    /// Schema v24: provenance class of `price_usd_per_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_source: Option<PriceSource>,
}

/// One declared cold-side heat-load term (W at the operating
/// temperature): static heat leak, conduction, declared AC loss, lead
/// flow — the case author folds their own estimate into named rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeatLoadTerm {
    pub id: String,
    /// Watts deposited at the cold stage at `operating.temperature_k`.
    /// Must be finite and positive — a zero term is a declaration bug.
    pub power_w: f64,
}

/// Schema v20 only: declared refrigeration economics for the operating
/// point. The cold-side load is the declared sum of `heat_loads_w`; the
/// electrical input power is Carnot-scaled at the declared efficiency
/// fraction:
///
/// `input_w = load_w × (sink_temperature_k − T_op) / (T_op × cop_fraction)`
///
/// — the T-dependence is thermodynamics (the Carnot bound), not a claim
/// about a specific plant. Lifetime opex is undiscounted:
/// `input_kW × hours_per_year × $/kWh × operating_years`.
///
/// The terms are **case-constant** — no candidate-dependent loads (AC
/// loss vs ramp rate, conduction vs winding size) are modeled — so every
/// candidate in a case carries identical opex, rankings and savings are
/// unchanged, and the capex-vs-opex trade materializes only across the
/// `temperature_k` sensitivity axis. Cryoplant sizing, cooldown
/// transients and discounting are all unmodeled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Opex {
    /// Named heat-load terms at the cold stage — ≥ 1 row, ids unique.
    pub heat_loads_w: Vec<HeatLoadTerm>,
    /// Fraction of the Carnot COP the declared refrigeration achieves,
    /// strictly within (0, 1] — real cryoplants sit near 0.1–0.3 at
    /// low temperatures.
    pub cop_fraction_of_carnot: f64,
    /// Warm-side sink temperature (K) — must strictly exceed
    /// `operating.temperature_k` (equal gives infinite input power).
    pub sink_temperature_k: f64,
    pub electricity_usd_per_kwh: f64,
    /// Duty hours per year at the operating point, within (0, 8760].
    pub operating_hours_per_year: f64,
    /// Amortization horizon (years ≥ 1) — undiscounted lifetime opex.
    pub operating_years: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub turns_along_normal: u32,
    pub tapes_along_width: u32,
    /// Defaults to 1 (schema v4; absent on earlier schemas).
    #[serde(default)]
    pub strands_parallel: Option<u32>,
    /// Schema v10: the baseline candidate's per-region spec assignment —
    /// one `tape_spec_choices` entry per `grading.regions` entry, in
    /// declaration order. Required iff the case declares `grading`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tape_spec_ids: Option<Vec<String>>,
    /// Schema v21: the baseline candidate's bend radius — required iff
    /// `choices.bend_radius_m` is declared (must list this value),
    /// forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_radius_m: Option<f64>,
    /// Schema v21: the baseline's straight half-length — same rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straight_half_length_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub max_threads: u32,
}

/// OC-008 contract §2 B2 (schema v2 only): declares, per pancake count
/// (`tapes_along_width`), a bracketing bisection of the turn count `n` that
/// finds the smallest passing `n` (the cheapest passing design at that
/// pancake count, since cost falls and utilization rises as `n` falls under
/// this model). One `Bracket` per entry of `pancake_counts` (contract's own
/// bijection requirement, checked in `validate`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefinementPlan {
    pub pancake_counts: Vec<u32>,
    /// Bisection stops once `pass_turns - fail_turns <= turn_resolution`.
    pub turn_resolution: u32,
    pub turn_bounds: TurnBounds,
    pub brackets: Vec<Bracket>,
    /// Contract §2 B3(4): when true, the bisection runner checks that
    /// utilization is nonincreasing in `n` across every evaluated candidate
    /// for a pancake count, downgrading that count's result to INCONCLUSIVE
    /// on a violation. When false, this check is skipped entirely (the
    /// bisection result is trusted without it).
    pub monotonicity_check: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnBounds {
    pub min: u32,
    pub max: u32,
}

/// A declared starting bracket for one pancake count (contract §2 B2/B3):
/// `fail_turns` is expected to FAIL (or be INCONCLUSIVE) and `pass_turns` is
/// expected to PASS when each is run through the OC-007 candidate pipeline.
/// Whether that expectation actually holds is checked at run time (a
/// structural expectation, not something `validate` can verify without the
/// material dataset and physics); an unmet expectation makes the bracket
/// `INVALID` for that pancake count rather than silently proceeding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bracket {
    pub tapes: u32,
    pub fail_turns: u32,
    pub pass_turns: u32,
}

impl RefinementPlan {
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            (1..=MAX_PANCAKE_COUNTS).contains(&self.pancake_counts.len()),
            "refinement.pancake_counts must list 1..=16 values",
        )?;
        validate_choice_set(
            &self.pancake_counts,
            MAX_TAPES_ALONG_WIDTH,
            "refinement.pancake_counts",
        )?;
        require(
            (1..=MAX_TURN_RESOLUTION).contains(&self.turn_resolution),
            "refinement.turn_resolution must be a positive turn count within the numerical work limit",
        )?;
        require(
            self.turn_bounds.min >= 1
                && self.turn_bounds.min < self.turn_bounds.max
                && self.turn_bounds.max <= MAX_TURNS_ALONG_NORMAL,
            "refinement.turn_bounds must be an increasing range within the numerical work limit",
        )?;
        require(
            self.brackets.len() == self.pancake_counts.len(),
            "refinement.brackets must declare exactly one bracket per refinement.pancake_counts entry",
        )?;
        let mut bracket_tapes: Vec<u32> = self.brackets.iter().map(|b| b.tapes).collect();
        bracket_tapes.sort_unstable();
        require(
            bracket_tapes == self.pancake_counts,
            "refinement.brackets tapes must exactly match refinement.pancake_counts, one bracket per pancake count",
        )?;
        for bracket in &self.brackets {
            require(
                bracket.fail_turns >= 1 && bracket.fail_turns <= MAX_TURNS_ALONG_NORMAL,
                "bracket fail_turns must be a positive turn count within the numerical work limit",
            )?;
            require(
                bracket.pass_turns >= 1 && bracket.pass_turns <= MAX_TURNS_ALONG_NORMAL,
                "bracket pass_turns must be a positive turn count within the numerical work limit",
            )?;
            require(
                bracket.fail_turns < bracket.pass_turns,
                "bracket fail_turns must be strictly less than pass_turns",
            )?;
            require(
                bracket.fail_turns >= self.turn_bounds.min
                    && bracket.pass_turns <= self.turn_bounds.max,
                "bracket fail_turns/pass_turns must lie within refinement.turn_bounds",
            )?;
        }
        Ok(())
    }
}

/// Contract §9.4: the acceptance-only refined sampling plan. `sampling`
/// already declares the 4 search stations; this declares the 6 additional
/// ones (arc azimuths 15/30/60/75 deg, straight x = 0.15/0.25 m) that
/// combine with them for 10 total. Turn selection for the refined plan is a
/// fixed algorithm (`refined_plan_turn_indices`, below), not a per-case
/// declared list: the contract spells the rule out literally as a function
/// of each candidate's own turn count, unlike `relative_turn_indices`
/// (which the contract leaves as a free per-case choice).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefinedPlan {
    pub additional_stations: Vec<Station>,
    /// Contract §9.4: refined `min_allowed_screening_a` must be at least
    /// `(1 - max_sampling_shortfall_fraction) * coarse min_allowed_screening_a`,
    /// or the coarse plan is judged inadequate for that candidate
    /// (`search_status` becomes INCONCLUSIVE). 0.02 in the frozen cases.
    pub max_sampling_shortfall_fraction: f64,
}

impl CoupledSearchCase {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let case: Self = crate::encoding::parse_case(json)?;
        case.validate()?;
        Ok(case)
    }

    pub fn embedded() -> Result<Self, ModelError> {
        Self::from_json(OC007_JSON)
    }

    pub fn embedded_control() -> Result<Self, ModelError> {
        Self::from_json(OC007_CONTROL_JSON)
    }

    /// OC-008 (contract §2, Stage B): the frozen bracketing-refinement case.
    /// Parses and structurally validates the schema only; does not run the
    /// search.
    pub fn embedded_oc008() -> Result<Self, ModelError> {
        Self::from_json(OC008_JSON)
    }

    /// OC-019: the schema v9 planar-path fixture (D-shape centerline).
    /// Parses and structurally validates the schema only; does not run the
    /// search.
    pub fn embedded_oc019() -> Result<Self, ModelError> {
        Self::from_json(OC019_JSON)
    }

    /// OC-020: the schema v10 graded-pack fixture (per-region tape-spec
    /// assignment over two radial grading regions). Parses and
    /// structurally validates the schema only; does not run the search.
    pub fn embedded_oc020() -> Result<Self, ModelError> {
        Self::from_json(OC020_JSON)
    }

    /// OC-021: the schema v12 transverse-pressure fixture (declared
    /// broad-face pressure bound on OC-019's D-shape). Parses and
    /// structurally validates the schema only; does not run the search.
    pub fn embedded_oc021() -> Result<Self, ModelError> {
        Self::from_json(OC021_JSON)
    }

    /// OC-022: the schema v10 two-dataset graded fixture — the second spec
    /// binds `robinson-shanghai-hflt-v3`, a physically distinct measured
    /// dataset on the same instrument. Parses and structurally validates
    /// the schema only; does not run the search.
    pub fn embedded_oc022() -> Result<Self, ModelError> {
        Self::from_json(OC022_JSON)
    }

    /// OC-023: the schema v10 three-dataset graded fixture — both regions
    /// choose among the base AP dataset and the two other measured vendor
    /// datasets (Shanghai HFLT, THEVA AP). Parses and structurally
    /// validates the schema only; does not run the search.
    pub fn embedded_oc023() -> Result<Self, ModelError> {
        Self::from_json(OC023_JSON)
    }

    /// Number of `(turns, tapes, strands[, assignment])` candidates the
    /// exhaustive search enumerates (contract S1; strands added at schema
    /// v4, the grading assignment product at v10).
    pub fn candidate_count(&self) -> usize {
        self.choices.turns_along_normal.len()
            * self.choices.tapes_along_width.len()
            * self.strands_choices().len()
            * self.assignment_count()
            * self
                .choices
                .bend_radius_m
                .as_deref()
                .map_or(1, <[f64]>::len)
            * self
                .choices
                .straight_half_length_m
                .as_deref()
                .map_or(1, <[f64]>::len)
    }

    /// The bend-radius values (m) the grid enumerates — the declared v21
    /// axis values wrapped `Some`, or `[None]` meaning "the fixed
    /// geometry's own declaration applies".
    pub fn bend_radius_axis(&self) -> Vec<Option<f64>> {
        match &self.choices.bend_radius_m {
            Some(values) => values.iter().map(|&v| Some(v)).collect(),
            None => vec![None],
        }
    }

    /// Same for the straight half-length axis.
    pub fn straight_half_length_axis(&self) -> Vec<Option<f64>> {
        match &self.choices.straight_half_length_m {
            Some(values) => values.iter().map(|&v| Some(v)).collect(),
            None => vec![None],
        }
    }

    /// The per-region spec-assignment product a v10 graded case enumerates;
    /// 1 for ungraded cases.
    pub fn assignment_count(&self) -> usize {
        self.grading
            .as_ref()
            .map(|g| {
                g.regions
                    .iter()
                    .map(|r| r.tape_spec_choices.len().max(1))
                    .product()
            })
            .unwrap_or(1)
    }

    /// The resolved 1-based inclusive turn range a v10 grading region
    /// covers for a candidate with `turns` turns: `floor(lo*N) < k <=
    /// floor(hi*N)`. `Err` when the region collapses empty — the declared
    /// grading cannot be realized on that candidate.
    pub fn region_turn_range(region: &GradingRegion, turns: u32) -> Result<(u32, u32), ModelError> {
        let first = (region.turn_range[0] * f64::from(turns)).floor() as u32 + 1;
        let last = (region.turn_range[1] * f64::from(turns)).floor() as u32;
        if last < first {
            return Err(ModelError::Invalid(format!(
                "grading region {:?} covers no turns on a {}-turn candidate",
                region.turn_range, turns
            )));
        }
        Ok((first, last))
    }

    /// The conductor price for a spec id — `cost.price_usd_per_m` for the
    /// reserved base spec, `tape_specs[id].price_usd_per_m` otherwise.
    /// Only meaningful on a validated case.
    pub fn spec_price_usd_per_m(&self, spec_id: &str) -> Option<f64> {
        if spec_id == BASE_TAPE_SPEC_ID {
            Some(self.cost.price_usd_per_m)
        } else {
            self.tape_specs
                .as_ref()
                .and_then(|specs| specs.get(spec_id))
                .map(|spec| spec.price_usd_per_m)
        }
    }

    /// The material binding a spec id resolves to — `material` for the
    /// reserved base spec, `tape_specs[id].material` otherwise.
    pub fn spec_material(&self, spec_id: &str) -> Option<&MaterialSettings> {
        if spec_id == BASE_TAPE_SPEC_ID {
            Some(&self.material)
        } else {
            self.tape_specs
                .as_ref()
                .and_then(|specs| specs.get(spec_id))
                .map(|spec| &spec.material)
        }
    }

    /// Every material binding a run of this case can evaluate: the base
    /// `material` plus each declared spec (every declared spec is required
    /// to be reachable through some region's choices).
    pub fn material_bindings(&self) -> Vec<(&str, &MaterialSettings)> {
        let mut out: Vec<(&str, &MaterialSettings)> = vec![(BASE_TAPE_SPEC_ID, &self.material)];
        if let Some(specs) = &self.tape_specs {
            for (id, spec) in specs {
                out.push((id.as_str(), &spec.material));
            }
        }
        out
    }

    /// The material binding a 1-based `turn_index` screens under for a
    /// candidate with `turns` turns and the given per-region assignment
    /// (one choice id per `grading.regions` entry). Ungraded cases always
    /// return the base `material`.
    pub fn material_for_turn(
        &self,
        turns: u32,
        turn_index: u32,
        assignment: Option<&[String]>,
    ) -> &MaterialSettings {
        let (Some(grading), Some(assignment)) = (&self.grading, assignment) else {
            return &self.material;
        };
        for (region, spec_id) in grading.regions.iter().zip(assignment.iter()) {
            if let Ok((first, last)) = Self::region_turn_range(region, turns)
                && (first..=last).contains(&turn_index)
            {
                return self.spec_material(spec_id).unwrap_or(&self.material);
            }
        }
        &self.material
    }

    /// Every spec assignment the search enumerates, in region-declaration
    /// order (lexicographic over each region's choice list). Ungraded cases
    /// yield a single empty assignment.
    pub fn assignments(&self) -> Vec<Vec<String>> {
        let Some(grading) = &self.grading else {
            return vec![Vec::new()];
        };
        let mut out: Vec<Vec<String>> = vec![Vec::new()];
        for region in &grading.regions {
            let mut next = Vec::with_capacity(out.len() * region.tape_spec_choices.len());
            for prefix in &out {
                for choice in &region.tape_spec_choices {
                    let mut a = prefix.clone();
                    a.push(choice.clone());
                    next.push(a);
                }
            }
            out = next;
        }
        out
    }

    /// The strands-per-turn choice list, `[1]` when the case does not
    /// declare one (all schemas before v4).
    pub fn strands_choices(&self) -> &[u32] {
        self.choices.strands_parallel.as_deref().unwrap_or(&[1])
    }

    /// The baseline's strands-per-turn (1 on schemas before v4).
    pub fn baseline_strands(&self) -> u32 {
        self.baseline.strands_parallel.unwrap_or(1)
    }

    /// Structural validation not requiring a loaded material dataset.
    /// Dataset-dependent identity checks live in `validate_against_dataset`.
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == COUPLED_SEARCH_CASE_SCHEMA_V1
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V2
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V3
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V4
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V5
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V6
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V7
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V8
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V9
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V10
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V11
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V12
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V13
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V14
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V15
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V16
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V17
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V18
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V19
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V20
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V21
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V22
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V23
                || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V24,
            "unsupported coupled search case schema",
        )?;
        // OC-008 contract §2 B2: schema v2 requires the `refinement` block;
        // v1 must not declare one, so an existing v1 file that never
        // mentions `refinement` keeps validating exactly as before. v3/v4/v5
        // are supersets of v2 plus `good_field_region` (and, for v4/v5,
        // `strands_parallel`).
        let refinement_forbidden = self.schema == COUPLED_SEARCH_CASE_SCHEMA_V1
            || (self.grading.is_some() && self.schema == COUPLED_SEARCH_CASE_SCHEMA_V10);
        if refinement_forbidden {
            // v1 predates the refinement block; a graded case at v10 may
            // not declare one — assignment-aware bisection exists only
            // from v11.
            require(
                self.refinement.is_none(),
                "schema v1, and any graded (schema v10) case, must not declare a refinement block",
            )?;
        } else {
            require(
                self.refinement.is_some(),
                "schema v2+ requires a refinement block",
            )?;
        }
        if let Some(refinement) = &self.refinement {
            refinement.validate()?;
            if self.grading.is_some() {
                // A graded slice's floor()-computed region boundary shifts
                // with n during bisection, so utilization need not be
                // strictly monotone; skipping the declared check could let
                // a boundary-shift blip pass as a false n*.
                require(
                    refinement.monotonicity_check,
                    "a graded case's refinement must declare monotonicity_check: true",
                )?;
            }
        }
        // Strands are a v4 feature; earlier schemas may not declare them
        // (a stray `"strands_parallel": [4]` on a v3 case must not silently
        // change its physics).
        let schema_at_least_v4 = self.schema == COUPLED_SEARCH_CASE_SCHEMA_V4
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V5
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V6
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V7
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V8
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V9
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V10
            || schema_at_least_v11(self.schema.as_str());
        let schema_at_least_v5 = self.schema == COUPLED_SEARCH_CASE_SCHEMA_V5
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V6
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V7
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V8
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V9
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V10
            || schema_at_least_v11(self.schema.as_str());
        let schema_at_least_v6 = self.schema == COUPLED_SEARCH_CASE_SCHEMA_V6
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V7
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V8
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V9
            || self.schema == COUPLED_SEARCH_CASE_SCHEMA_V10
            || schema_at_least_v11(self.schema.as_str());
        let schema_at_least_v3 = self.schema == COUPLED_SEARCH_CASE_SCHEMA_V3 || schema_at_least_v4;
        if !schema_at_least_v4 {
            require(
                self.choices
                    .strands_parallel
                    .as_deref()
                    .is_none_or(|s| s == [1]),
                "schema v1-v3 must not declare choices.strands_parallel other than [1]",
            )?;
            require(
                self.baseline.strands_parallel.is_none_or(|s| s == 1),
                "schema v1-v3 must not declare baseline.strands_parallel other than 1",
            )?;
        }
        // OC-014: the self-field correction changes screening semantics,
        // so only v5+ cases may declare it.
        if !schema_at_least_v5 {
            require(
                self.limits.self_field_correction.is_none(),
                "schema v1-v4 must not declare limits.self_field_correction",
            )?;
        }
        // OC-017: the along-current bound model changes screening
        // semantics, so only v6+ cases may declare it.
        if !schema_at_least_v6 {
            require(
                self.limits.along_current_model.is_none(),
                "schema v1-v5 must not declare limits.along_current_model",
            )?;
        }
        // OC-014 Phase 2: `critical_state_strip` and its layer thickness
        // change the screening query in the dominance regime, so only v8+
        // cases may declare them.
        let declares_critical_state = self.limits.self_field_correction.as_deref()
            == Some("critical_state_strip")
            || self.limits.critical_state_layer_thickness_m.is_some();
        require(
            schema_at_least_v8(self.schema.as_str()) || !declares_critical_state,
            "schema v1-v7 must not declare self_field_correction \
             \"critical_state_strip\" or limits.critical_state_layer_thickness_m",
        )?;
        if schema_at_least_v3 {
            // Schema v13: a declared field map covers the pack region
            // only — the usable-volume lattice lies in the bore and
            // cannot be evaluated against it, so map cases must not
            // declare the region (the map-specific check below enforces
            // its absence).
            require(
                self.requirement.good_field_region.is_some() || self.field_map.is_some(),
                "schema v3+ requires requirement.good_field_region",
            )?;
        } else {
            require(
                self.requirement.good_field_region.is_none(),
                "schema v1/v2 must not declare requirement.good_field_region",
            )?;
            require(
                self.manufacturing.is_none(),
                "schema v1/v2 must not declare a manufacturing block",
            )?;
        }
        if let Some(manufacturing) = &self.manufacturing {
            require(
                manufacturing.min_inner_bend_radius_m.is_finite()
                    && manufacturing.min_inner_bend_radius_m > 0.0
                    && manufacturing.min_inner_bend_radius_m <= 1e6,
                "manufacturing min_inner_bend_radius_m must be finite and within (0, 1e6] m",
            )?;
            // The bend-strain bound is a v17 semantic; it cannot resolve
            // without the tape's total thickness. Thickness alone is
            // allowed — it records the strain unbounded, the same
            // convention as the transverse-pressure estimate.
            if !schema_at_least_v17(self.schema.as_str()) {
                require(
                    manufacturing.tape_thickness_m.is_none()
                        && manufacturing.max_bend_strain.is_none(),
                    "schema v3-v16 must not declare manufacturing tape_thickness_m or max_bend_strain",
                )?;
            }
            require(
                manufacturing.max_bend_strain.is_none() || manufacturing.tape_thickness_m.is_some(),
                "manufacturing max_bend_strain requires tape_thickness_m",
            )?;
            if let Some(thickness) = manufacturing.tape_thickness_m {
                require(
                    thickness.is_finite() && thickness > 0.0 && thickness <= 0.1,
                    "manufacturing tape_thickness_m must be finite and within (0, 0.1] m",
                )?;
            }
            if let Some(limit) = manufacturing.max_bend_strain {
                require(
                    limit.is_finite() && limit > 0.0 && limit <= 1.0,
                    "manufacturing max_bend_strain must be finite and within (0, 1]",
                )?;
            }
        }
        if let Some(mechanical) = &self.mechanical {
            require(
                schema_at_least_v4,
                "schema v1-v3 must not declare a mechanical block",
            )?;
            require(
                mechanical.max_lorentz_load_n_per_m.is_finite()
                    && mechanical.max_lorentz_load_n_per_m > 0.0,
                "mechanical max_lorentz_load_n_per_m must be finite and positive",
            )?;
            // OC-018: the hoop-stress bound is a v7 semantic — earlier
            // schemas' mechanical blocks carry only the Lorentz-load
            // screen. The stress needs its declared load path, so the
            // limit and the section area must appear together or not at
            // all.
            if !schema_at_least_v7(self.schema.as_str()) {
                require(
                    mechanical.max_hoop_stress_pa.is_none()
                        && mechanical.tension_section_area_m2.is_none(),
                    "schema v4-v6 must not declare mechanical hoop-stress fields",
                )?;
            }
            require(
                mechanical.max_hoop_stress_pa.is_some()
                    == mechanical.tension_section_area_m2.is_some(),
                "mechanical max_hoop_stress_pa and tension_section_area_m2 must be declared together",
            )?;
            if let Some(limit) = mechanical.max_hoop_stress_pa {
                require(
                    limit.is_finite() && limit > 0.0,
                    "mechanical max_hoop_stress_pa must be finite and positive",
                )?;
            }
            if let Some(area) = mechanical.tension_section_area_m2 {
                require(
                    area.is_finite() && area > 0.0,
                    "mechanical tension_section_area_m2 must be finite and positive",
                )?;
            }
            // The transverse-pressure bound is a v12 semantic — earlier
            // schemas' mechanical blocks carry only the load/hoop screens.
            if !schema_at_least_v12(self.schema.as_str()) {
                require(
                    mechanical.max_transverse_pressure_pa.is_none(),
                    "schema v4-v11 must not declare mechanical.max_transverse_pressure_pa",
                )?;
            }
            if let Some(limit) = mechanical.max_transverse_pressure_pa {
                require(
                    limit.is_finite() && limit > 0.0,
                    "mechanical max_transverse_pressure_pa must be finite and positive",
                )?;
            }
            // The membrane-tension bound is a v17 semantic — earlier
            // schemas' mechanical blocks carry only the load/hoop/
            // pressure screens.
            if !schema_at_least_v17(self.schema.as_str()) {
                require(
                    mechanical.max_membrane_tension_n_per_m.is_none(),
                    "schema v4-v16 must not declare mechanical.max_membrane_tension_n_per_m",
                )?;
            }
            if let Some(limit) = mechanical.max_membrane_tension_n_per_m {
                require(
                    limit.is_finite() && limit > 0.0,
                    "mechanical max_membrane_tension_n_per_m must be finite and positive",
                )?;
            }
        }
        // Schema v15: the four first-order adjacent screens. Each is a
        // declared-assumption bound or flag, gated on the v15 schema; a
        // screen whose inputs cannot resolve reports INCONCLUSIVE at run
        // time rather than silently passing.
        let screens_declared = self.thermal_margin.is_some()
            || self.ac_loss.is_some()
            || self.quench_hotspot.is_some()
            || self.screening_current.is_some();
        if screens_declared {
            require(
                schema_at_least_v15(self.schema.as_str()),
                "adjacent screens (thermal_margin, ac_loss, quench_hotspot, screening_current) require schema v15",
            )?;
        }
        if let Some(thermal_margin) = &self.thermal_margin {
            require(
                thermal_margin.min_margin_k.is_finite() && thermal_margin.min_margin_k >= 0.0,
                "thermal_margin min_margin_k must be finite and non-negative",
            )?;
        }
        if let Some(ac_loss) = &self.ac_loss {
            require(
                ac_loss.frequency_hz.is_finite() && ac_loss.frequency_hz > 0.0,
                "ac_loss frequency_hz must be finite and positive",
            )?;
            require(
                ac_loss.transport_amplitude_fraction.is_finite()
                    && ac_loss.transport_amplitude_fraction > 0.0
                    && ac_loss.transport_amplitude_fraction <= 1.0,
                "ac_loss transport_amplitude_fraction must be in (0, 1]",
            )?;
            require(
                ac_loss.sc_layer_thickness_m.is_finite() && ac_loss.sc_layer_thickness_m > 0.0,
                "ac_loss sc_layer_thickness_m must be finite and positive",
            )?;
            require(
                ac_loss.max_loss_w.is_finite() && ac_loss.max_loss_w >= 0.0,
                "ac_loss max_loss_w must be finite and non-negative",
            )?;
        }
        if let Some(quench) = &self.quench_hotspot {
            require(
                quench.dump_time_constant_s.is_finite() && quench.dump_time_constant_s > 0.0,
                "quench_hotspot dump_time_constant_s must be finite and positive",
            )?;
            require(
                quench.stabilizer_area_m2.is_finite() && quench.stabilizer_area_m2 > 0.0,
                "quench_hotspot stabilizer_area_m2 must be finite and positive",
            )?;
            require(
                quench.max_hotspot_k.is_finite() && quench.max_hotspot_k > 0.0,
                "quench_hotspot max_hotspot_k must be finite and positive",
            )?;
            require(
                quench.quench_function_a2s_per_m4.len() >= 2,
                "quench_hotspot quench_function_a2s_per_m4 must list at least 2 rows",
            )?;
            require(
                quench
                    .quench_function_a2s_per_m4
                    .iter()
                    .all(|r| r[0].is_finite() && r[0] > 0.0 && r[1].is_finite() && r[1] >= 0.0),
                "quench_hotspot table temperatures must be finite positive K and U finite non-negative",
            )?;
            require(
                quench
                    .quench_function_a2s_per_m4
                    .windows(2)
                    .all(|w| w[0][0] < w[1][0] && w[0][1] <= w[1][1]),
                "quench_hotspot table must be strictly increasing in T and nondecreasing in U",
            )?;
            // The runtime hot-spot resolve needs U(T_op) interpolable —
            // the declared table must bracket the operating temperature.
            let first_t = quench.quench_function_a2s_per_m4[0][0];
            let last_t =
                quench.quench_function_a2s_per_m4[quench.quench_function_a2s_per_m4.len() - 1][0];
            require(
                self.operating.temperature_k >= first_t && self.operating.temperature_k <= last_t,
                "quench_hotspot table must bracket operating.temperature_k",
            )?;
        }
        if let Some(screening) = &self.screening_current {
            require(
                screening.max_penetrated_width_fraction.is_finite()
                    && screening.max_penetrated_width_fraction > 0.0
                    && screening.max_penetrated_width_fraction <= 1.0,
                "screening_current max_penetrated_width_fraction must be in (0, 1]",
            )?;
        }
        // Schema v16: the measured E–J transition screen — same declared-
        // assumption discipline as the v15 screens, gated one version later.
        if let Some(transition) = &self.transition {
            require(
                schema_at_least_v16(self.schema.as_str()),
                "transition screen requires schema v16",
            )?;
            require(
                transition.max_e_over_ec.is_finite() && transition.max_e_over_ec > 0.0,
                "transition max_e_over_ec must be finite and positive",
            )?;
            if let Some(v) = transition.max_voltage_v {
                require(
                    v.is_finite() && v > 0.0,
                    "transition max_voltage_v must be finite and positive when declared",
                )?;
            }
        }
        // Schema v18: the lumped adiabatic quench-transient screen —
        // declared composite property tables, gated one version later.
        if let Some(transient) = &self.quench_transient {
            require(
                schema_at_least_v18(self.schema.as_str()),
                "quench_transient screen requires schema v18",
            )?;
            require(
                transient.dump_time_constant_s.is_finite() && transient.dump_time_constant_s > 0.0,
                "quench_transient dump_time_constant_s must be finite and positive",
            )?;
            require(
                transient.conducting_area_per_width_m.is_finite()
                    && transient.conducting_area_per_width_m > 0.0,
                "quench_transient conducting_area_per_width_m must be finite and positive",
            )?;
            for (table, name) in [
                (&transient.resistivity_ohm_m, "resistivity_ohm_m"),
                (
                    &transient.heat_capacity_j_per_m3k,
                    "heat_capacity_j_per_m3k",
                ),
            ] {
                require(
                    table.len() >= 2,
                    &format!("quench_transient {name} must list at least 2 rows"),
                )?;
                require(
                    table
                        .iter()
                        .all(|r| r[0].is_finite() && r[0] > 0.0 && r[1].is_finite() && r[1] > 0.0),
                    &format!(
                        "quench_transient {name} temperatures and values must be finite positive"
                    ),
                )?;
                require(
                    table.windows(2).all(|w| w[0][0] < w[1][0]),
                    &format!("quench_transient {name} must be strictly increasing in T"),
                )?;
                // Every trajectory starts at or above T_op — both
                // tables must bracket it, the quench_hotspot table
                // convention (a declared T0 is span-checked below).
                require(
                    self.operating.temperature_k >= table[0][0]
                        && self.operating.temperature_k <= table[table.len() - 1][0],
                    &format!("quench_transient {name} must bracket operating.temperature_k"),
                )?;
            }
            if let Some(limit) = transient.max_temperature_k {
                require(
                    limit.is_finite() && limit > 0.0,
                    "quench_transient max_temperature_k must be finite and positive",
                )?;
            }
            if let Some(t0) = transient.initial_temperature_k {
                require(
                    t0.is_finite() && t0 >= self.operating.temperature_k,
                    "quench_transient initial_temperature_k must be finite and at least operating.temperature_k",
                )?;
                for (table, name) in [
                    (&transient.resistivity_ohm_m, "resistivity_ohm_m"),
                    (
                        &transient.heat_capacity_j_per_m3k,
                        "heat_capacity_j_per_m3k",
                    ),
                ] {
                    require(
                        t0 >= table[0][0] && t0 <= table[table.len() - 1][0],
                        &format!(
                            "quench_transient initial_temperature_k must lie inside the {name} span"
                        ),
                    )?;
                }
            }
            if let Some(v) = transient.detection_voltage_v {
                require(
                    v.is_finite() && v > 0.0,
                    "quench_transient detection_voltage_v must be finite and positive",
                )?;
            }
            if let Some(t) = transient.max_detection_time_s {
                require(
                    t.is_finite() && t > 0.0,
                    "quench_transient max_detection_time_s must be finite and positive",
                )?;
                require(
                    transient.detection_voltage_v.is_some(),
                    "quench_transient max_detection_time_s requires detection_voltage_v",
                )?;
            }
        }
        // Schema v20: the declared opex block — refrigeration economics
        // for the operating point, all terms declared and positive.
        if let Some(opex) = &self.opex {
            require(
                schema_at_least_v20(self.schema.as_str()),
                "opex block requires schema v20",
            )?;
            require(
                !opex.heat_loads_w.is_empty(),
                "opex heat_loads_w must list at least one declared term",
            )?;
            let mut ids: Vec<&str> = opex.heat_loads_w.iter().map(|h| h.id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            require(
                ids.len() == opex.heat_loads_w.len(),
                "opex heat_loads_w ids must be distinct",
            )?;
            require(
                opex.heat_loads_w
                    .iter()
                    .all(|h| !h.id.is_empty() && h.power_w.is_finite() && h.power_w > 0.0),
                "opex heat_loads_w ids must be non-empty and powers finite positive",
            )?;
            require(
                opex.cop_fraction_of_carnot.is_finite()
                    && opex.cop_fraction_of_carnot > 0.0
                    && opex.cop_fraction_of_carnot <= 1.0,
                "opex cop_fraction_of_carnot must lie within (0, 1]",
            )?;
            require(
                opex.sink_temperature_k.is_finite()
                    && opex.sink_temperature_k > self.operating.temperature_k,
                "opex sink_temperature_k must strictly exceed operating.temperature_k",
            )?;
            require(
                opex.electricity_usd_per_kwh.is_finite() && opex.electricity_usd_per_kwh > 0.0,
                "opex electricity_usd_per_kwh must be finite and positive",
            )?;
            require(
                opex.operating_hours_per_year.is_finite()
                    && opex.operating_hours_per_year > 0.0
                    && opex.operating_hours_per_year <= 8760.0,
                "opex operating_hours_per_year must lie within (0, 8760]",
            )?;
            require(
                opex.operating_years >= 1,
                "opex operating_years must be >= 1",
            )?;
        }
        // Schema v21: racetrack-dimension search axes — magnet size as a
        // declared choice dimension. Each axis: v21+, nonempty, finite
        // positive distinct values, its `fixed_geometry` field absent
        // (one declaration site per dimension), racetrack-only (a
        // declared path/path3d/field_map fixes the winding shape), and a
        // baseline value drawn from the list. The axis-free dim keeps
        // the pre-v21 rule (the fixed field declares it).
        for (axis, fixed_field, baseline_field, name) in [
            (
                self.choices.bend_radius_m.as_deref(),
                self.fixed_geometry.bend_radius_m,
                self.baseline.bend_radius_m,
                "bend_radius_m",
            ),
            (
                self.choices.straight_half_length_m.as_deref(),
                self.fixed_geometry.straight_half_length_m,
                self.baseline.straight_half_length_m,
                "straight_half_length_m",
            ),
        ] {
            match axis {
                Some(values) => {
                    require(
                        schema_at_least_v21(self.schema.as_str()),
                        &format!("choices.{name} requires schema v21"),
                    )?;
                    require(
                        !values.is_empty(),
                        &format!("choices.{name} must list at least one value"),
                    )?;
                    require(
                        values.iter().all(|v| v.is_finite() && *v > 0.0),
                        &format!("choices.{name} values must be finite positive"),
                    )?;
                    let mut sorted = values.to_vec();
                    sorted.sort_by(|a, b| a.total_cmp(b));
                    sorted.dedup();
                    require(
                        sorted.len() == values.len(),
                        &format!("choices.{name} values must be distinct"),
                    )?;
                    require(
                        fixed_field.is_none(),
                        &format!(
                            "choices.{name} and fixed_geometry.{name} are alternative declarations of one dimension — declare exactly one"
                        ),
                    )?;
                    require(
                        self.fixed_geometry.path.is_none()
                            && self.fixed_geometry.path3d.is_none()
                            && self.field_map.is_none(),
                        &format!(
                            "choices.{name} applies to legacy racetrack geometry only — a declared path, path3d or field_map fixes the winding shape"
                        ),
                    )?;
                    match baseline_field {
                        Some(b) => require(
                            values.contains(&b),
                            &format!(
                                "baseline.{name} must be one of the declared choices.{name} values"
                            ),
                        )?,
                        None => {
                            return Err(ModelError::Invalid(format!(
                                "baseline.{name} is required when choices.{name} is declared"
                            )));
                        }
                    }
                }
                None => require(
                    baseline_field.is_none(),
                    &format!("baseline.{name} is forbidden when choices.{name} is not declared"),
                )?,
            }
        }
        // A case declaring any geometry axis is a racetrack — both dims
        // must resolve (axis or fixed field), never an orphan dimension.
        if self.choices.bend_radius_m.is_some() || self.choices.straight_half_length_m.is_some() {
            for (axis, fixed_field, name) in [
                (
                    self.choices.bend_radius_m.is_some(),
                    self.fixed_geometry.bend_radius_m.is_some(),
                    "bend_radius_m",
                ),
                (
                    self.choices.straight_half_length_m.is_some(),
                    self.fixed_geometry.straight_half_length_m.is_some(),
                    "straight_half_length_m",
                ),
            ] {
                require(
                    axis || fixed_field,
                    &format!(
                        "geometry-axis cases are racetrack cases — {name} must resolve via choices.{name} or fixed_geometry.{name}"
                    ),
                )?;
            }
        }
        if let Some(region) = &self.requirement.good_field_region {
            for (value, name) in [
                (region.half_extents_m[0], "x"),
                (region.half_extents_m[1], "y"),
                (region.half_extents_m[2], "z"),
            ] {
                require(
                    value.is_finite() && value > 0.0 && value <= 1e6,
                    &format!(
                        "requirement good_field_region half_extents_m[{name}] must be finite and within (0, 1e6] m"
                    ),
                )?;
            }
            require(
                (2..=9).contains(&region.points_per_axis),
                "requirement good_field_region points_per_axis must be within 2..=9",
            )?;
            if let Some(bound) = region.max_relative_deviation {
                require(
                    schema_at_least_v22(self.schema.as_str()),
                    "requirement good_field_region max_relative_deviation requires schema v22",
                )?;
                require(
                    bound.is_finite() && bound > 0.0 && bound < 1.0,
                    "requirement good_field_region max_relative_deviation must be a finite fraction in (0, 1) — a bound at or past 1 tolerates a sign reversal",
                )?;
            }
            if let Some(harmonics) = &region.harmonics {
                require(
                    schema_at_least_v23(self.schema.as_str()),
                    "requirement good_field_region harmonics requires schema v23",
                )?;
                let in_plane_limit = region.half_extents_m[0].min(region.half_extents_m[1]);
                require(
                    harmonics.reference_radius_m.is_finite()
                        && harmonics.reference_radius_m > 0.0
                        && harmonics.reference_radius_m < in_plane_limit,
                    "requirement good_field_region harmonics reference_radius_m must be finite and strictly inside the region's in-plane half extents",
                )?;
                require(
                    (1..=16).contains(&harmonics.max_order),
                    "requirement good_field_region harmonics max_order must be within 1..=16",
                )?;
                require(
                    harmonics.theta_samples > 2 * harmonics.max_order
                        && harmonics.theta_samples <= 512,
                    "requirement good_field_region harmonics theta_samples must satisfy Nyquist (> 2*max_order) and stay within 512",
                )?;
                for (name, bound) in [
                    (
                        "max_normal_unit_fraction",
                        harmonics.max_normal_unit_fraction,
                    ),
                    ("max_skew_unit_fraction", harmonics.max_skew_unit_fraction),
                ] {
                    if let Some(bound) = bound {
                        require(
                            bound.is_finite() && bound > 0.0 && bound < 1.0,
                            &format!(
                                "requirement good_field_region harmonics {name} must be a finite fraction in (0, 1)"
                            ),
                        )?;
                    }
                }
            }
        }
        require(
            !self.id.trim().is_empty(),
            "coupled search case id is empty",
        )?;
        require(
            !self.provenance.trim().is_empty(),
            "coupled search case provenance is empty",
        )?;

        for value in self.requirement.bore_probe_m {
            require(
                value.is_finite() && value.abs() <= 1e6,
                "requirement bore_probe_m must be finite and within +/- 1e6 m",
            )?;
        }
        require(
            self.requirement.b_target_t.is_finite() && self.requirement.b_target_t > 0.0,
            "requirement b_target_t must be finite and positive",
        )?;
        require(
            self.requirement.tolerance_fraction.is_finite()
                && self.requirement.tolerance_fraction > 0.0
                && self.requirement.tolerance_fraction < 1.0,
            "requirement tolerance_fraction must be a finite fraction in (0, 1)",
        )?;

        // Geometry: one of the legacy racetrack dimensions, a general
        // planar `path` (v9), or a non-planar `path3d` (v19) — never
        // more than one, never none. Schema v21: a `choices` axis counts
        // as its dimension's racetrack declaration — the dim resolves to
        // a searched value rather than a fixed one, so a bend axis plus
        // a fixed straight is still the racetrack family.
        let bend_resolved =
            self.fixed_geometry.bend_radius_m.is_some() || self.choices.bend_radius_m.is_some();
        let straight_resolved = self.fixed_geometry.straight_half_length_m.is_some()
            || self.choices.straight_half_length_m.is_some();
        require(
            straight_resolved == bend_resolved,
            "fixed_geometry straight_half_length_m and bend_radius_m must be declared together",
        )?;
        require(
            u8::from(bend_resolved)
                + u8::from(self.fixed_geometry.path.is_some())
                + u8::from(self.fixed_geometry.path3d.is_some())
                == 1,
            "fixed_geometry must declare exactly one of racetrack dimensions, path, or path3d",
        )?;
        require(
            self.fixed_geometry.path.is_none() || schema_at_least_v9(self.schema.as_str()),
            "schema v1-v8 may not declare fixed_geometry.path",
        )?;
        // Schema v19: non-planar helix centerline — v19 gate, plus the
        // two constraints the machinery genuinely needs: the radial tape
        // normal (the only stacking a helical conductor has) and a
        // declared Cartesian field map (the engine's pack self-field
        // evaluator is planar — checked against the map block below,
        // which also pins the required fixed extents and the components
        // gate).
        require(
            self.fixed_geometry.path3d.is_none() || schema_at_least_v19(self.schema.as_str()),
            "schema v1-v18 may not declare fixed_geometry.path3d",
        )?;
        if self.fixed_geometry.path3d.is_some() {
            require(
                self.fixed_geometry.tape_normal == TapeNormal::Radial,
                "fixed_geometry.path3d requires tape_normal radial — the cylinder-radial \
                 stacking a helical conductor winds in (axial normal on a non-planar \
                 path is not modeled)",
            )?;
            require(
                self.field_map.is_some(),
                "fixed_geometry.path3d requires field_map — non-planar packs evaluate \
                 under a declared Cartesian field map only; the engine's pack \
                 self-field evaluator is planar",
            )?;
            require(
                self.field_map.as_ref().is_some_and(|fm| {
                    fm.map.components() == crate::coupled::FieldMapComponents::CartesianBxByBz
                }),
                "fixed_geometry.path3d requires a cartesian_bx_by_bz field_map — a \
                 cylindrical map cannot describe a non-axisymmetric winding's field",
            )?;
        }
        for (value, name) in [
            (self.fixed_geometry.radial_pitch_m, "radial_pitch_m"),
            (self.fixed_geometry.tape_width_m, "tape_width_m"),
        ] {
            require(
                value.is_finite() && value > 0.0,
                &format!("fixed_geometry {name} must be finite and positive"),
            )?;
        }
        if let Some(bend_radius_m) = self.fixed_geometry.bend_radius_m {
            require(
                bend_radius_m.is_finite() && bend_radius_m > 0.0,
                "fixed_geometry bend_radius_m must be finite and positive",
            )?;
        }
        if let Some(straight_half_length_m) = self.fixed_geometry.straight_half_length_m {
            require(
                straight_half_length_m.is_finite() && straight_half_length_m >= 0.0,
                "fixed_geometry straight_half_length_m must be finite and nonnegative",
            )?;
        }
        if let Some(path) = &self.fixed_geometry.path {
            path.validate()?;
        }
        if let Some(path3d) = &self.fixed_geometry.path3d {
            path3d.validate()?;
        }

        // Schema v13: a declared field map is valid only for the winding
        // it was computed on — the pack extents are fixed declarations,
        // the choices vary discretization and assignment, and the NI
        // solve anchors on the map producer's declared bore field.
        match &self.field_map {
            Some(field_map) => {
                require(
                    schema_at_least_v13(self.schema.as_str()),
                    "field_map requires schema v13",
                )?;
                require(
                    schema_at_least_v14(self.schema.as_str())
                        || field_map.map.components()
                            != crate::coupled::FieldMapComponents::CartesianBxByBz,
                    "field_map components cartesian_bx_by_bz requires search schema v14",
                )?;
                require(
                    field_map.bore_field_at_reference_t.is_finite()
                        && field_map.bore_field_at_reference_t > 0.0,
                    "field_map.bore_field_at_reference_t must be finite and positive",
                )?;
                let (width_m, height_m) = match (
                    self.fixed_geometry.pack_radial_width_m,
                    self.fixed_geometry.pack_axial_height_m,
                ) {
                    (Some(w), Some(h)) => {
                        require(
                            w.is_finite() && w > 0.0 && h.is_finite() && h > 0.0,
                            "fixed pack extents must be finite and positive",
                        )?;
                        (w, h)
                    }
                    _ => {
                        return Err(ModelError::Invalid(
                            "field_map requires fixed_geometry.pack_radial_width_m and pack_axial_height_m".into(),
                        ));
                    }
                };
                // The declared pitch/width describe the baseline winding:
                // its counts must reconstruct exactly the declared
                // extents in each direction (the normal direction for
                // pitch, the width direction for tape width).
                let (normal_extent, width_extent) = match self.fixed_geometry.tape_normal {
                    TapeNormal::Radial => (width_m, height_m),
                    TapeNormal::Axial => (height_m, width_m),
                };
                let baseline_normal = f64::from(self.baseline.turns_along_normal)
                    * self.fixed_geometry.radial_pitch_m;
                let baseline_width =
                    f64::from(self.baseline.tapes_along_width) * self.fixed_geometry.tape_width_m;
                require(
                    (baseline_normal - normal_extent).abs() <= 1e-9 * normal_extent.max(1.0),
                    "under field_map, baseline turns_along_normal x radial_pitch_m must equal the normal-direction pack extent",
                )?;
                require(
                    (baseline_width - width_extent).abs() <= 1e-9 * width_extent.max(1.0),
                    "under field_map, baseline tapes_along_width x tape_width_m must equal the width-direction pack extent",
                )?;
                // The map's own structural + hull validation: grid
                // well-formedness, path confinement, and containment of
                // the declared extents. A Cartesian map on a racetrack
                // case synthesizes the equivalent path from the declared
                // racetrack dims; a `path3d` case checks the helix pack's
                // own swept domain.
                let synthesized;
                let centerline = if let Some(path3d) = self.fixed_geometry.path3d.as_ref() {
                    Some(crate::coupled::Centerline::Helical(path3d))
                } else {
                    match self.fixed_geometry.path.as_ref() {
                        Some(p) => Some(crate::coupled::Centerline::Planar(p)),
                        None => match (
                            &field_map.map,
                            self.fixed_geometry.straight_half_length_m,
                            self.fixed_geometry.bend_radius_m,
                        ) {
                            (
                                crate::coupled::FieldMap::CartesianBxByBz { .. },
                                Some(half_length),
                                Some(bend),
                            ) => {
                                synthesized = crate::path::CoilPath::racetrack(half_length, bend);
                                Some(crate::coupled::Centerline::Planar(&synthesized))
                            }
                            _ => None,
                        },
                    }
                };
                field_map
                    .map
                    .validate_against_extents(centerline, width_m, height_m)?;
                // The map covers the pack only — a usable-volume field
                // requirement could not be evaluated against it, so the
                // case may not declare one.
                require(
                    self.requirement.good_field_region.is_none(),
                    "field_map cases may not declare requirement.good_field_region — the declared map covers the pack region only",
                )?;
            }
            None => {
                require(
                    self.fixed_geometry.pack_radial_width_m.is_none()
                        && self.fixed_geometry.pack_axial_height_m.is_none(),
                    "fixed_geometry fixed pack extents require a field_map (schema v13)",
                )?;
            }
        }

        validate_choice_set(
            &self.choices.turns_along_normal,
            MAX_TURNS_ALONG_NORMAL,
            "turns_along_normal",
        )?;
        validate_choice_set(
            &self.choices.tapes_along_width,
            MAX_TAPES_ALONG_WIDTH,
            "tapes_along_width",
        )?;
        if let Some(strands) = &self.choices.strands_parallel {
            validate_choice_set(strands, MAX_STRANDS_PARALLEL, "strands_parallel")?;
        }
        require(
            (1..=MAX_CANDIDATES).contains(&self.candidate_count()),
            "expected 1..=64 (turns, tapes, strands) candidates",
        )?;
        require(
            self.choices
                .turns_along_normal
                .contains(&self.baseline.turns_along_normal),
            "baseline turns_along_normal must be one of the declared choices",
        )?;
        require(
            self.choices
                .tapes_along_width
                .contains(&self.baseline.tapes_along_width),
            "baseline tapes_along_width must be one of the declared choices",
        )?;
        require(
            self.strands_choices().contains(&self.baseline_strands()),
            "baseline strands_parallel must be one of the declared choices",
        )?;

        // Schema v10 grading: `tape_specs`/`grading` come as a pair; regions
        // are sorted non-overlapping fractions whose choices resolve to a
        // spec id or the reserved `base`; the assignment product is already
        // inside `candidate_count()`'s cap.
        require(
            schema_at_least_v10(self.schema.as_str())
                || (self.tape_specs.is_none()
                    && self.grading.is_none()
                    && self.baseline.tape_spec_ids.is_none()),
            "schema v1-v9 must not declare tape_specs, grading, or baseline.tape_spec_ids",
        )?;
        require(
            self.tape_specs.is_some() == self.grading.is_some(),
            "tape_specs and grading must be declared together",
        )?;
        if let Some(grading) = &self.grading {
            let specs = self
                .tape_specs
                .as_ref()
                .expect("tape_specs presence checked above");
            require(
                !specs.is_empty() && specs.len() <= 32,
                "tape_specs must hold 1..=32 entries",
            )?;
            require(
                !specs.contains_key(BASE_TAPE_SPEC_ID),
                "\"base\" is reserved for the case-level material binding",
            )?;
            for (id, spec) in specs {
                require(
                    !id.trim().is_empty() && id.len() <= 64,
                    "tape_spec ids must be non-empty and <= 64 chars",
                )?;
                spec.material.validate()?;
                require(
                    spec.price_usd_per_m.is_finite()
                        && spec.price_usd_per_m > 0.0
                        && spec.price_usd_per_m <= 1e12,
                    "tape_specs price_usd_per_m must be finite, positive and within 1e12",
                )?;
            }
            require(
                !grading.regions.is_empty() && grading.regions.len() <= 8,
                "grading.regions must hold 1..=8 entries",
            )?;
            let mut prev_hi = 0.0_f64;
            for region in &grading.regions {
                let [lo, hi] = region.turn_range;
                require(
                    lo.is_finite() && hi.is_finite() && lo >= 0.0 && hi <= 1.0 && lo < hi,
                    "grading region turn_range must satisfy 0 <= lo < hi <= 1",
                )?;
                require(
                    lo >= prev_hi,
                    "grading regions must be sorted and non-overlapping",
                )?;
                prev_hi = hi;
                require(
                    !region.tape_spec_choices.is_empty() && region.tape_spec_choices.len() <= 32,
                    "grading region tape_spec_choices must hold 1..=32 ids",
                )?;
                let mut seen = HashSet::new();
                for choice in &region.tape_spec_choices {
                    require(
                        seen.insert(choice.as_str()),
                        "grading region tape_spec_choices must not repeat an id",
                    )?;
                    require(
                        choice == BASE_TAPE_SPEC_ID || specs.contains_key(choice),
                        "grading region tape_spec_choices must resolve to a tape_specs id or \"base\"",
                    )?;
                }
            }
            for id in specs.keys() {
                require(
                    grading
                        .regions
                        .iter()
                        .any(|r| r.tape_spec_choices.iter().any(|c| c == id)),
                    "every tape_specs entry must appear in some region's tape_spec_choices",
                )?;
            }
            let baseline_ids = self.baseline.tape_spec_ids.as_ref().ok_or_else(|| {
                ModelError::Invalid(
                    "baseline.tape_spec_ids is required when grading is declared".to_owned(),
                )
            })?;
            require(
                baseline_ids.len() == grading.regions.len(),
                "baseline.tape_spec_ids must assign one spec per grading region",
            )?;
            for (i, id) in baseline_ids.iter().enumerate() {
                require(
                    grading.regions[i].tape_spec_choices.contains(id),
                    "baseline.tape_spec_ids must choose from each region's tape_spec_choices",
                )?;
            }
        } else {
            require(
                self.baseline.tape_spec_ids.is_none(),
                "baseline.tape_spec_ids requires a grading block",
            )?;
        }

        require(
            self.operating.temperature_k.is_finite()
                && self.operating.temperature_k > 0.0
                && self.operating.temperature_k <= 400.0,
            "operating temperature_k must be finite, positive and at most 400 K",
        )?;
        require(
            self.operating.electric_field_criterion_v_per_m.is_finite()
                && self.operating.electric_field_criterion_v_per_m > 0.0,
            "electric_field_criterion_v_per_m must be finite and positive",
        )?;

        self.material.validate()?;

        require(
            (1..=16).contains(&self.sampling.stations.len()),
            "expected 1..=16 stations",
        )?;
        let mut ids = HashSet::new();
        // Station validation needs the geometry only (racetrack dims or
        // the path); the placeholder cross-section is never read. Schema
        // v21: an axis-declared dim still names the racetrack family —
        // any declared value validates identically (values are finite
        // positive by the axis gate), so the first stands in.
        let placeholder_bend = self
            .fixed_geometry
            .bend_radius_m
            .or_else(|| self.choices.bend_radius_m.as_ref()?.first().copied());
        let placeholder_straight = self.fixed_geometry.straight_half_length_m.or_else(|| {
            self.choices
                .straight_half_length_m
                .as_ref()?
                .first()
                .copied()
        });
        let placeholder_pack = Pack {
            straight_half_length_m: placeholder_straight,
            bend_radius_m: placeholder_bend,
            radial_width_m: placeholder_bend.unwrap_or(1e-3),
            axial_height_m: placeholder_bend.unwrap_or(1e-3),
            path: self.fixed_geometry.path.clone(),
            path3d: self.fixed_geometry.path3d.clone(),
        };
        for station in &self.sampling.stations {
            station.validate(&placeholder_pack)?;
            require(ids.insert(station.id().to_owned()), "duplicate station id")?;
        }
        require(
            (1..=MAX_CHOICE_VALUES).contains(&self.sampling.relative_turn_indices.len()),
            "relative_turn_indices must list 1..=64 entries",
        )?;
        for entry in &self.sampling.relative_turn_indices {
            entry.validate()?;
        }
        require(
            self.sampling.width_points == 5,
            "width_points must equal 5 (the shared Gauss-Lobatto rule)",
        )?;

        self.limits.validate()?;

        require(
            self.numerics.quadrature_orders[0] < self.numerics.quadrature_orders[1],
            "quadrature_orders must be strictly increasing",
        )?;
        require(
            self.numerics
                .quadrature_orders
                .iter()
                .all(|&o| (2..=24).contains(&o)),
            "quadrature_orders must lie within 2..=24",
        )?;
        require(
            self.numerics.field_scale_t.is_finite() && self.numerics.field_scale_t > 0.0,
            "numerics field_scale_t must be finite and positive",
        )?;
        require(
            self.numerics.max_refinement_change_fraction.is_finite()
                && self.numerics.max_refinement_change_fraction > 0.0
                && self.numerics.max_refinement_change_fraction < 1.0,
            "numerics max_refinement_change_fraction must be a finite fraction in (0, 1)",
        )?;

        if let Some(pruning) = &self.pruning {
            require(
                !pruning.coarse_stations.is_empty(),
                "pruning coarse_stations must not be empty",
            )?;
            for id in &pruning.coarse_stations {
                require(
                    self.sampling.stations.iter().any(|s| s.id() == id),
                    "pruning coarse_stations entry is not part of the sampling plan",
                )?;
            }
            require(
                !pruning.coarse_turn_fractions.is_empty(),
                "pruning coarse_turn_fractions must not be empty",
            )?;
            for entry in &pruning.coarse_turn_fractions {
                entry.validate()?;
            }
            require(
                !pruning.coarse_width_indices.is_empty() && pruning.coarse_width_indices.len() <= 5,
                "pruning coarse_width_indices must list 1..=5 entries",
            )?;
            let mut seen = HashSet::new();
            for &index in &pruning.coarse_width_indices {
                require(
                    index < 5,
                    "pruning coarse_width_indices entries must be 0..=4",
                )?;
                require(
                    seen.insert(index),
                    "duplicate pruning coarse_width_indices entry",
                )?;
            }
        }

        // Contract §9.4: 6 additional stations, ids disjoint from
        // `sampling.stations` (reusing the same `ids` set populated above)
        // and from each other, so the merged 10-station refined plan has no
        // collisions.
        require(
            self.refined_plan.additional_stations.len() == 6,
            "refined_plan.additional_stations must declare exactly 6 stations (contract §9.4: 4 arc azimuths + 2 straight positions)",
        )?;
        for station in &self.refined_plan.additional_stations {
            station.validate(&placeholder_pack)?;
            require(
                ids.insert(station.id().to_owned()),
                "refined_plan.additional_stations station id collides with sampling.stations or another refined_plan station",
            )?;
        }
        require(
            self.refined_plan
                .max_sampling_shortfall_fraction
                .is_finite()
                && self.refined_plan.max_sampling_shortfall_fraction >= 0.0
                && self.refined_plan.max_sampling_shortfall_fraction < 1.0,
            "refined_plan.max_sampling_shortfall_fraction must be a finite fraction in [0, 1)",
        )?;

        require(
            self.cost.price_usd_per_m.is_finite() && self.cost.price_usd_per_m > 0.0,
            "cost price_usd_per_m must be finite and positive",
        )?;
        require(
            self.cost.scrap_fraction.is_finite()
                && self.cost.scrap_fraction >= 0.0
                && self.cost.scrap_fraction <= 1.0,
            "cost scrap_fraction must be finite and within 0..=1",
        )?;
        require(
            self.cost.assembly_cost_per_pancake_usd.is_finite()
                && self.cost.assembly_cost_per_pancake_usd > 0.0,
            "cost assembly_cost_per_pancake_usd must be finite and positive",
        )?;
        require(
            self.cost.joint_cost_usd.is_finite() && self.cost.joint_cost_usd > 0.0,
            "cost joint_cost_usd must be finite and positive",
        )?;

        // Schema v24: piece-quantized procurement + price provenance.
        let validate_offerings = |offerings: &[PieceOffering],
                                  site: &str|
         -> Result<(), ModelError> {
            require(
                !offerings.is_empty() && offerings.len() <= 16,
                &format!("{site} piece_offerings must list 1..=16 entries"),
            )?;
            let mut lengths: Vec<u64> = offerings.iter().map(|o| o.length_m.to_bits()).collect();
            lengths.sort_unstable();
            lengths.dedup();
            require(
                lengths.len() == offerings.len(),
                &format!("{site} piece_offerings lengths must be distinct"),
            )?;
            for o in offerings {
                require(
                    o.length_m.is_finite() && o.length_m > 0.0 && o.length_m <= 1e9,
                    &format!("{site} piece_offerings length_m must be finite positive within 1e9"),
                )?;
                require(
                    o.price_usd_per_m.is_finite()
                        && o.price_usd_per_m > 0.0
                        && o.price_usd_per_m <= 1e12,
                    &format!(
                        "{site} piece_offerings price_usd_per_m must be finite positive within 1e12"
                    ),
                )?;
            }
            Ok(())
        };
        if let Some(policy) = &self.cost.piece_policy {
            require(
                schema_at_least_v24(self.schema.as_str()),
                "cost.piece_policy requires schema v24",
            )?;
            // The piece walk is defined for the radial-normal winding
            // topologies every benchmark uses; a declared field map or an
            // axial tape normal folds the tape axis differently and keeps
            // the legacy ledger instead.
            require(
                self.field_map.is_none(),
                "cost.piece_policy is not supported with a declared field_map",
            )?;
            require(
                self.fixed_geometry.tape_normal == TapeNormal::Radial,
                "cost.piece_policy requires tape_normal \"radial\"",
            )?;
            require(
                policy.splice_cost_usd.is_finite() && policy.splice_cost_usd >= 0.0,
                "piece_policy splice_cost_usd must be finite and nonnegative",
            )?;
            if let Some(p) = policy.piece_length_m {
                require(
                    p.is_finite() && p > 0.0 && p <= 1e9,
                    "piece_policy piece_length_m must be finite positive within 1e9",
                )?;
            }
            require(
                !(policy.piece_length_m.is_some() && self.cost.piece_offerings.is_some()),
                "declare either piece_policy.piece_length_m or cost.piece_offerings, not both",
            )?;
        }
        if let Some(offerings) = &self.cost.piece_offerings {
            require(
                schema_at_least_v24(self.schema.as_str()),
                "cost.piece_offerings requires schema v24",
            )?;
            require(
                self.cost.piece_policy.is_some(),
                "cost.piece_offerings requires cost.piece_policy",
            )?;
            validate_offerings(offerings, "cost")?;
        }
        require(
            self.cost.price_source.is_none() || schema_at_least_v24(self.schema.as_str()),
            "cost.price_source requires schema v24",
        )?;
        if let Some(policy) = &self.cost.piece_policy {
            require(
                policy.piece_length_m.is_some() || self.cost.piece_offerings.is_some(),
                "cost.piece_policy requires a base piece source: piece_policy.piece_length_m or cost.piece_offerings",
            )?;
        }
        if let Some(specs) = &self.tape_specs {
            for (id, spec) in specs {
                if let Some(offerings) = &spec.piece_offerings {
                    require(
                        schema_at_least_v24(self.schema.as_str()),
                        "tape_specs piece_offerings requires schema v24",
                    )?;
                    require(
                        self.cost.piece_policy.is_some(),
                        "tape_specs piece_offerings requires cost.piece_policy",
                    )?;
                    validate_offerings(offerings, &format!("tape_specs[{id}]"))?;
                }
                require(
                    spec.price_source.is_none() || schema_at_least_v24(self.schema.as_str()),
                    "tape_specs price_source requires schema v24",
                )?;
            }
        }

        require(
            (1..=8).contains(&self.execution.max_threads),
            "execution max_threads must be within 1..=8",
        )?;

        Ok(())
    }

    /// Dataset-dependent identity checks on the base `material` binding
    /// (mirrors `coupled::CoupledCase::validate_against_dataset`):
    /// dataset_id, csv_sha256 and method must match the supplied dataset
    /// exactly, the electric-field criterion must match within roundoff,
    /// and the operating temperature must be strictly interior to the
    /// dataset's nominal temperature span. For a graded (v10) case call
    /// `validate_against_dataset_map` — every referenced binding's
    /// identity must check, not only the base's.
    pub fn validate_against_dataset(&self, dataset: &MaterialDataset) -> Result<(), ModelError> {
        self.material.validate_against_dataset(
            dataset,
            self.operating.temperature_k,
            self.operating.electric_field_criterion_v_per_m,
        )
    }

    /// Every referenced binding's dataset checks (schema v10): the base
    /// `material` plus each `tape_specs` entry is validated against the
    /// dataset its own `dataset_id` resolves to. `datasets` is keyed by
    /// `MaterialDataset::metadata.id`; a declared id with no entry is an
    /// error, never a silent substitute. Ungraded cases reduce to the
    /// single base binding.
    pub fn validate_against_dataset_map(
        &self,
        datasets: &BTreeMap<String, MaterialDataset>,
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

    /// The spec id a 1-based `turn_index` resolves to for a candidate
    /// with `turns` turns under `assignment` (one choice id per
    /// `grading.regions` entry): the covering region's assigned id, or
    /// `BASE_TAPE_SPEC_ID` when no region covers the turn or the case is
    /// ungraded. A region that collapses empty on this turn count is
    /// skipped — `evaluate_one_candidate` rejects that candidate before
    /// any evaluation reaches here.
    pub fn spec_id_for_turn<'a>(
        &'a self,
        turns: u32,
        turn_index: u32,
        assignment: &'a [String],
    ) -> &'a str {
        let Some(grading) = &self.grading else {
            return BASE_TAPE_SPEC_ID;
        };
        for (region, spec_id) in grading.regions.iter().zip(assignment.iter()) {
            if let Ok((first, last)) = Self::region_turn_range(region, turns)
                && (first..=last).contains(&turn_index)
            {
                return spec_id.as_str();
            }
        }
        BASE_TAPE_SPEC_ID
    }
}

fn validate_choice_set(values: &[u32], max: u32, name: &str) -> Result<(), ModelError> {
    require(
        (1..=MAX_CHOICE_VALUES).contains(&values.len()),
        &format!("{name} must list 1..=64 values"),
    )?;
    require(
        values.windows(2).all(|w| w[0] < w[1]),
        &format!("{name} must be strictly ascending (which also implies uniqueness)"),
    )?;
    require(
        values.iter().all(|&v| v >= 1 && v <= max),
        &format!("{name} entries must be positive and within the declared numerical work limit"),
    )?;
    Ok(())
}

/// Build the equivalent `coupled::CoupledCase` for one `(turns, tapes,
/// current_a[, assignment])` candidate: the search's declared fixed
/// geometry expanded to a real pack/winding, plus an explicit sampling
/// plan supplied by the caller (the full plan or a coarse subset). This
/// is the only place OC-007 constructs geometry; both the search's own
/// screening and its acceptance re-verification call it, so the coupled
/// runner's rules (`run_coupled_case` / `evaluate_candidate_point`) are
/// reused rather than reimplemented.
///
/// `tape_spec_assignment` (schema v10) is the candidate's chosen spec per
/// `grading.regions` entry, in declaration order; a region assigned
/// `"base"` emits no `winding.regions` entry — its turns fall back to the
/// case-level `material` — and only actually referenced specs land in the
/// generated case's `tape_specs`.
#[allow(clippy::too_many_arguments)]
pub fn build_coupled_case(
    search: &CoupledSearchCase,
    turns_along_normal: u32,
    tapes_along_width: u32,
    strands_parallel: u32,
    current_a: f64,
    stations: Vec<Station>,
    turn_indices_along_normal: Vec<u32>,
    tape_indices_along_width: Vec<u32>,
    tape_spec_assignment: Option<&[String]>,
    dims: CandidateDims,
) -> Result<CoupledCase, ModelError> {
    // Schema v21: the candidate's axis-resolved dims shadow the fixed
    // declarations — the generated case evaluates the searched geometry.
    let fg = search.fixed_geometry.resolved_for(dims);
    // Under a declared field map (schema v13) the pack extents are the
    // declared fixed values — the map is valid only for the winding it
    // was computed on; the counts then set the discretization, not the
    // footprint. Otherwise the legacy `count x pitch/width` mapping.
    let (radial_width_m, axial_height_m) =
        fg.candidate_extents_m(turns_along_normal, tapes_along_width);
    let pack = Pack {
        straight_half_length_m: fg.straight_half_length_m,
        bend_radius_m: fg.bend_radius_m,
        radial_width_m,
        axial_height_m,
        path: fg.path.clone(),
        path3d: fg.path3d.clone(),
    };
    // Resolve the v10 grading plan into explicit winding regions: only
    // non-base assignments emit a region, and only the specs they name
    // are copied into the generated case's `tape_specs`.
    let mut winding_regions: Vec<crate::coupled::WindingRegion> = Vec::new();
    let mut used_specs: Vec<String> = Vec::new();
    if let (Some(grading), Some(assignment)) = (&search.grading, tape_spec_assignment) {
        require(
            assignment.len() == grading.regions.len(),
            "tape_spec_assignment must assign one spec per grading region",
        )?;
        for (region, spec_id) in grading.regions.iter().zip(assignment.iter()) {
            let (first_turn, last_turn) =
                CoupledSearchCase::region_turn_range(region, turns_along_normal)?;
            if spec_id == BASE_TAPE_SPEC_ID {
                continue;
            }
            require(
                search
                    .tape_specs
                    .as_ref()
                    .is_some_and(|specs| specs.contains_key(spec_id)),
                "tape_spec_assignment must resolve to a tape_specs id or \"base\"",
            )?;
            if !used_specs.contains(spec_id) {
                used_specs.push(spec_id.clone());
            }
            winding_regions.push(crate::coupled::WindingRegion {
                first_turn,
                last_turn,
                tape_spec: spec_id.clone(),
            });
        }
    }
    let tape_specs = if used_specs.is_empty() {
        None
    } else {
        Some(
            used_specs
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        search.tape_specs.as_ref().expect("checked")[id]
                            .material
                            .clone(),
                    )
                })
                .collect(),
        )
    };
    let winding = Winding {
        tape_width_m: search
            .fixed_geometry
            .candidate_tape_width_m(tapes_along_width),
        tape_normal: search.fixed_geometry.tape_normal,
        turns_along_normal,
        tapes_along_width,
        strands_parallel,
        regions: if winding_regions.is_empty() {
            None
        } else {
            Some(winding_regions)
        },
    };
    let reference_subset = vec![ReferenceSubsetEntry {
        station: stations
            .first()
            .map(|s| s.id().to_owned())
            .unwrap_or_default(),
        tape_index: *tape_indices_along_width.first().unwrap_or(&1),
        turn_index: *turn_indices_along_normal.first().unwrap_or(&1),
    }];
    // A graded candidate (search schema v10, any non-"base" assignment)
    // generates a coupled-conductor v6 case — the newest schema, which
    // also carries path support and every limits extension earlier
    // versions introduced. A path pack (search schema v9) generates v5.
    // Otherwise: a search case declaring limits.self_field_correction
    // (schema v5) generates a coupled v2 case; one declaring
    // limits.along_current_model (schema v6) a v3 case; one declaring the
    // OC-014 Phase-2 `critical_state_strip` correction or its layer
    // thickness (schema v8) a v4 case — the earlier coupled schemas forbid
    // those fields, so copying limits verbatim under them would fail the
    // generated case's own validation.
    let coupled_schema = if search.fixed_geometry.path3d.is_some() {
        // A non-planar helix centerline (search schema v19) generates a
        // coupled v9 case — the earliest coupled schema that can carry
        // `pack.path3d`. The declared map still rides in
        // `sampling.field_map`.
        crate::coupled::COUPLED_CASE_SCHEMA_V9
    } else if search.field_map.is_some() {
        // Search cases with a declared map generate coupled v7 cases —
        // or v8 when the map is Cartesian (search schema v14). The map
        // rides in `sampling.field_map` below.
        match search.field_map.as_ref().map(|fm| fm.map.components()) {
            Some(crate::coupled::FieldMapComponents::CartesianBxByBz) => {
                crate::coupled::COUPLED_CASE_SCHEMA_V8
            }
            _ => crate::coupled::COUPLED_CASE_SCHEMA_V7,
        }
    } else if winding.regions.is_some() {
        crate::coupled::COUPLED_CASE_SCHEMA_V6
    } else if search.fixed_geometry.path.is_some() {
        crate::coupled::COUPLED_CASE_SCHEMA_V5
    } else if search.limits.self_field_correction.as_deref() == Some("critical_state_strip")
        || search.limits.critical_state_layer_thickness_m.is_some()
    {
        crate::coupled::COUPLED_CASE_SCHEMA_V4
    } else if search.limits.along_current_model.is_some() {
        crate::coupled::COUPLED_CASE_SCHEMA_V3
    } else if search.limits.self_field_correction.is_some() {
        crate::coupled::COUPLED_CASE_SCHEMA_V2
    } else {
        crate::coupled::COUPLED_CASE_SCHEMA
    };
    let case = CoupledCase {
        schema: coupled_schema.to_owned(),
        id: format!(
            "{}-geometry-turns{turns_along_normal}-tapes{tapes_along_width}",
            search.id
        ),
        provenance: format!(
            "Generated by the coupled-search geometry helper from case '{}' for turns_along_normal={turns_along_normal}, tapes_along_width={tapes_along_width}, strands_parallel={strands_parallel}; not an independently frozen benchmark.",
            search.id
        ),
        geometry_data_class: DataClass::Synthetic,
        pack,
        winding,
        operating: Operating {
            temperature_k: search.operating.temperature_k,
            current_candidates_a: vec![current_a],
            electric_field_criterion_v_per_m: search.operating.electric_field_criterion_v_per_m,
        },
        material: search.material.clone(),
        tape_specs,
        sampling: Sampling {
            stations,
            turn_indices_along_normal,
            tape_indices_along_width,
            width_quadrature: WidthQuadrature::GaussLobatto,
            width_points: 5,
            // Schema v13: the declared map rides into every generated
            // case — the full-plan rerun then evaluates the customer's
            // field solution exactly as the coarse plan does.
            field_map: search.field_map.as_ref().map(|fm| fm.map.clone()),
        },
        limits: search.limits.clone(),
        numerics: Numerics {
            quadrature_orders: search.numerics.quadrature_orders,
            field_scale_t: search.numerics.field_scale_t,
            max_refinement_change_fraction: search.numerics.max_refinement_change_fraction,
            max_reference_refinement_fraction: 0.5,
            max_reference_field_error_fraction: 0.5,
            max_reference_angle_error_deg: 90.0,
            max_reference_capacity_relative_error: 0.5,
            reference_subset,
        },
        width_transfer: WidthTransfer {
            basis: WidthTransferBasis::NoTransfer,
        },
    };
    case.validate()?;
    Ok(case)
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

    fn minimal_case_json(dataset_block: &str, threads: u32) -> String {
        format!(
            r#"{{
  "schema": "optcoil-coupled-search/v1",
  "id": "unit-test-search-case",
  "provenance": "coupled_search.rs unit test fixture; not a frozen benchmark.",
  "requirement": {{"bore_probe_m": [0.0, 0.0, 0.0], "b_target_t": 0.9, "tolerance_fraction": 1e-9}},
  "fixed_geometry": {{
    "straight_half_length_m": 0.3,
    "bend_radius_m": 0.2,
    "radial_pitch_m": 0.0001,
    "tape_width_m": 0.012,
    "tape_normal": "radial"
  }},
  "choices": {{"turns_along_normal": [120, 200], "tapes_along_width": [2, 3]}},
  "operating": {{"temperature_k": 21.0, "electric_field_criterion_v_per_m": 0.0001}},
  "material": {dataset_block},
  "sampling": {{
    "stations": [{{"id": "s0", "kind": "straight", "x_m": 0.0}}],
    "relative_turn_indices": [
      {{"kind": "from_start", "offset": 1}},
      {{"kind": "fraction", "value": 0.5}},
      {{"kind": "from_end", "offset": 0}}
    ],
    "width_points": 5
  }},
  "limits": {{
    "max_along_current_field_fraction": 0.2,
    "max_self_field_ratio": 0.1,
    "interpolation_overprediction_budget": 0.1,
    "utilization_limit": 0.8
  }},
  "numerics": {{"quadrature_orders": [2, 4], "field_scale_t": 1.0, "max_refinement_change_fraction": 0.5}},
  "pruning": null,
  "cost": {{
    "price_usd_per_m": 30.0,
    "scrap_fraction": 0.1,
    "assembly_cost_per_pancake_usd": 500.0,
    "joint_cost_usd": 200.0
  }},
  "baseline": {{"turns_along_normal": 200, "tapes_along_width": 3}},
  "refined_plan": {{
    "additional_stations": [
      {{"id": "arc_15", "kind": "arc", "azimuth_deg": 15.0}},
      {{"id": "arc_30", "kind": "arc", "azimuth_deg": 30.0}},
      {{"id": "arc_60", "kind": "arc", "azimuth_deg": 60.0}},
      {{"id": "arc_75", "kind": "arc", "azimuth_deg": 75.0}},
      {{"id": "straight_015", "kind": "straight", "x_m": 0.15}},
      {{"id": "straight_025", "kind": "straight", "x_m": 0.25}}
    ],
    "max_sampling_shortfall_fraction": 0.02
  }},
  "execution": {{"max_threads": {threads}}}
}}
"#
        )
    }

    fn oc004_dataset_block() -> &'static str {
        r#"{
    "dataset_id": "robinson-superpower-ap-v3",
    "csv_sha256": "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
    "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
    "angle_mapping": "period_180_field_reversal",
    "mirror_policy": "minimum_of_mirror_pair",
    "field_basis_mapping": "pack_field_as_applied_field_self_field_consistent",
    "field_magnitude_policy": "total_magnitude_with_transverse_angle",
    "low_field_policy": "monotone_field_lower_bound",
    "low_field_clamp_t": 1.001,
    "monotonicity_tolerance": 0.001
  }"#
    }

    #[test]
    fn embedded_oc007_and_control_cases_parse_and_match_the_contract() {
        let case = CoupledSearchCase::embedded().unwrap();
        assert_eq!(case.requirement.b_target_t, 0.9);
        assert_eq!(case.baseline.turns_along_normal, 200);
        assert_eq!(case.baseline.tapes_along_width, 3);
        assert_eq!(case.candidate_count(), 15);
        assert_eq!(
            case.material.dataset_id,
            "robinson-superpower-ap-v3-lowfield"
        );
        assert_eq!(case.execution.max_threads, 5);

        let control = CoupledSearchCase::embedded_control().unwrap();
        assert_eq!(control.material.dataset_id, "robinson-superpower-ap-v3");
        assert_eq!(control.baseline.turns_along_normal, 200);
        assert_eq!(control.candidate_count(), 15);
    }

    #[test]
    fn relative_turn_indices_resolve_and_dedup_per_contract_d4() {
        let specs = [
            RelativeTurnIndex::FromStart { offset: 1 },
            RelativeTurnIndex::FromStart { offset: 2 },
            RelativeTurnIndex::FromStart { offset: 3 },
            RelativeTurnIndex::FromStart { offset: 5 },
            RelativeTurnIndex::FromStart { offset: 10 },
            RelativeTurnIndex::FromStart { offset: 20 },
            RelativeTurnIndex::Fraction { value: 0.25 },
            RelativeTurnIndex::Fraction { value: 0.5 },
            RelativeTurnIndex::Fraction { value: 0.75 },
            RelativeTurnIndex::FromEnd { offset: 19 },
            RelativeTurnIndex::FromEnd { offset: 9 },
            RelativeTurnIndex::FromEnd { offset: 4 },
            RelativeTurnIndex::FromEnd { offset: 1 },
            RelativeTurnIndex::FromEnd { offset: 0 },
        ];
        // n = 120: round(0.25*120)=30, round(0.5*120)=60, round(0.75*120)=90;
        // n-19=101, n-9=111, n-4=116, n-1=119, n=120.
        let expanded = expand_relative_turn_indices(&specs, 120);
        assert_eq!(
            expanded,
            vec![1, 2, 3, 5, 10, 20, 30, 60, 90, 101, 111, 116, 119, 120]
        );
        // A tiny n exercises clipping and dedup collapsing several entries.
        let small = expand_relative_turn_indices(&specs, 3);
        assert_eq!(small, vec![1, 2, 3]);
        assert!(small.iter().all(|&i| (1..=3).contains(&i)));
    }

    #[test]
    fn schema_rejects_empty_or_oversized_choice_sets_and_baseline_outside_them() {
        let mut case = CoupledSearchCase::embedded().unwrap();
        case.choices.turns_along_normal = vec![];
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.choices.turns_along_normal = (1..=65).collect();
        case.choices.tapes_along_width = vec![1];
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.choices.turns_along_normal.swap(0, 1); // no longer ascending
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.baseline.turns_along_normal = 999_999; // not in the choice set
        assert!(case.validate().is_err());
    }

    #[test]
    fn candidate_cap_of_64_is_enforced() {
        let json = minimal_case_json(oc004_dataset_block(), 5);
        let mut case: CoupledSearchCase = serde_json::from_str(&json).unwrap();
        case.validate().unwrap();
        case.choices.turns_along_normal = (1..=9).collect(); // 9 * 8 = 72 > 64
        case.choices.tapes_along_width = (1..=8).collect();
        case.baseline.turns_along_normal = 1;
        case.baseline.tapes_along_width = 1;
        assert!(case.validate().is_err());
    }

    #[test]
    fn positive_cost_fields_and_thread_bounds_are_enforced() {
        let mut case = CoupledSearchCase::embedded().unwrap();
        case.cost.price_usd_per_m = 0.0;
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.cost.assembly_cost_per_pancake_usd = -1.0;
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.execution.max_threads = 0;
        assert!(case.validate().is_err());

        let mut case = CoupledSearchCase::embedded().unwrap();
        case.execution.max_threads = 9;
        assert!(case.validate().is_err());
    }

    #[test]
    fn requirement_probe_must_be_finite() {
        let mut case = CoupledSearchCase::embedded().unwrap();
        case.requirement.bore_probe_m[0] = f64::NAN;
        assert!(case.validate().is_err());
        case.requirement.bore_probe_m[0] = f64::INFINITY;
        assert!(case.validate().is_err());
    }

    #[test]
    fn dataset_identity_is_checked_against_the_supplied_dataset() {
        let case = CoupledSearchCase::embedded().unwrap();
        let dataset =
            MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
        case.validate_against_dataset(&dataset).unwrap();

        let wrong = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        assert!(case.validate_against_dataset(&wrong).is_err());

        let mut mutated = case.clone();
        mutated.material.csv_sha256 = "0".repeat(64);
        assert!(mutated.validate_against_dataset(&dataset).is_err());

        let mut boundary = case;
        boundary.operating.temperature_k = 20.0; // exactly the nominal minimum
        assert!(boundary.validate_against_dataset(&dataset).is_err());
    }

    #[test]
    fn pruning_declares_a_subset_of_the_full_sampling_plan() {
        let case = CoupledSearchCase::embedded().unwrap();
        let pruning = case.pruning.as_ref().expect("oc-007 declares pruning");
        for id in &pruning.coarse_stations {
            assert!(case.sampling.stations.iter().any(|s| s.id() == id));
        }
        assert!(pruning.coarse_width_indices.iter().all(|&i| i < 5));
    }

    #[test]
    fn build_coupled_case_produces_the_expected_pack_and_winding() {
        let search = CoupledSearchCase::embedded().unwrap();
        let stations = search.sampling.stations.clone();
        let turns = expand_relative_turn_indices(&search.sampling.relative_turn_indices, 200);
        let case = build_coupled_case(
            &search,
            200,
            3,
            1,
            123.0,
            stations,
            turns,
            vec![1, 2, 3],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(case.winding.turns_along_normal, 200);
        assert_eq!(case.winding.tapes_along_width, 3);
        assert!((case.pack.radial_width_m - 200.0 * 0.0001).abs() < 1e-15);
        assert!((case.pack.axial_height_m - 3.0 * 0.012).abs() < 1e-15);
        assert_eq!(case.operating.current_candidates_a, vec![123.0]);
        case.validate().unwrap();
    }

    // ---------------------------------------------------------------
    // OC-008 contract §2 B2: schema v2 / `refinement` block validation.
    // ---------------------------------------------------------------

    fn minimal_case_json_v2(dataset_block: &str, threads: u32, refinement_block: &str) -> String {
        let v1 = minimal_case_json(dataset_block, threads);
        let with_schema = v1.replace(
            "\"schema\": \"optcoil-coupled-search/v1\"",
            "\"schema\": \"optcoil-coupled-search/v2\"",
        );
        with_schema.replacen(
            "\"execution\":",
            &format!("{refinement_block},\n  \"execution\":"),
            1,
        )
    }

    fn valid_refinement_block() -> &'static str {
        r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 200},
    "brackets": [
      {"tapes": 2, "fail_turns": 10, "pass_turns": 50},
      {"tapes": 3, "fail_turns": 10, "pass_turns": 50}
    ],
    "monotonicity_check": true
  }"#
    }

    #[test]
    fn v1_case_without_refinement_still_parses_and_validates() {
        let json = minimal_case_json(oc004_dataset_block(), 5);
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V1);
        assert!(case.refinement.is_none());
        // The two still-current v1 frozen cases keep parsing unchanged too.
        assert!(CoupledSearchCase::embedded().unwrap().refinement.is_none());
        assert!(
            CoupledSearchCase::embedded_control()
                .unwrap()
                .refinement
                .is_none()
        );
    }

    #[test]
    fn v2_case_with_a_well_formed_refinement_block_parses_and_validates() {
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, valid_refinement_block());
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V2);
        let refinement = case.refinement.as_ref().unwrap();
        assert_eq!(refinement.pancake_counts, vec![2, 3]);
        assert_eq!(refinement.turn_resolution, 2);
        assert_eq!(refinement.brackets.len(), 2);
    }

    #[test]
    fn v2_schema_requires_a_refinement_block() {
        let json = minimal_case_json(oc004_dataset_block(), 5).replace(
            "\"optcoil-coupled-search/v1\"",
            "\"optcoil-coupled-search/v2\"",
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v1_schema_must_not_declare_a_refinement_block() {
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, valid_refinement_block())
            .replace(
                "\"optcoil-coupled-search/v2\"",
                "\"optcoil-coupled-search/v1\"",
            );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    fn minimal_case_json_v3(dataset_block: &str, threads: u32, region_block: &str) -> String {
        let v2 = minimal_case_json_v2(dataset_block, threads, valid_refinement_block());
        v2.replacen(
            "\"optcoil-coupled-search/v2\"",
            "\"optcoil-coupled-search/v3\"",
            1,
        )
        .replacen(
            "\"tolerance_fraction\": 1e-9}",
            &format!("\"tolerance_fraction\": 1e-9, {region_block}}}"),
            1,
        )
    }

    fn valid_region_block() -> &'static str {
        r#""good_field_region": {"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 3}"#
    }

    #[test]
    fn v3_case_with_a_good_field_region_parses_and_validates() {
        let json = minimal_case_json_v3(oc004_dataset_block(), 5, valid_region_block());
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V3);
        let region = case.requirement.good_field_region.as_ref().unwrap();
        assert_eq!(region.half_extents_m, [0.05, 0.05, 0.05]);
        assert_eq!(region.points_per_axis, 3);
    }

    #[test]
    fn v3_schema_requires_a_good_field_region() {
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, valid_refinement_block())
            .replacen(
                "\"optcoil-coupled-search/v2\"",
                "\"optcoil-coupled-search/v3\"",
                1,
            );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v1_and_v2_schemas_must_not_declare_a_good_field_region() {
        // v1 case, region injected: rejected.
        let v1 = minimal_case_json(oc004_dataset_block(), 5).replacen(
            "\"tolerance_fraction\": 1e-9}",
            &format!("\"tolerance_fraction\": 1e-9, {}}}", valid_region_block()),
            1,
        );
        assert!(CoupledSearchCase::from_json(&v1).is_err());
        // v2 case (refinement present), region injected: still rejected.
        let v2 = minimal_case_json_v2(oc004_dataset_block(), 5, valid_refinement_block()).replacen(
            "\"tolerance_fraction\": 1e-9}",
            &format!("\"tolerance_fraction\": 1e-9, {}}}", valid_region_block()),
            1,
        );
        assert!(CoupledSearchCase::from_json(&v2).is_err());
    }

    #[test]
    fn good_field_region_bounds_are_checked() {
        for block in [
            r#""good_field_region": {"half_extents_m": [0.0, 0.05, 0.05], "points_per_axis": 3}"#,
            r#""good_field_region": {"half_extents_m": [0.05, -1.0, 0.05], "points_per_axis": 3}"#,
            r#""good_field_region": {"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 1}"#,
            r#""good_field_region": {"half_extents_m": [0.05, 0.05, 0.05], "points_per_axis": 10}"#,
        ] {
            let json = minimal_case_json_v3(oc004_dataset_block(), 5, block);
            assert!(
                CoupledSearchCase::from_json(&json).is_err(),
                "region block must be rejected: {block}"
            );
        }
    }

    #[test]
    fn good_field_lattice_covers_the_box_corners_and_center() {
        let region = GoodFieldRegion {
            half_extents_m: [0.1, 0.2, 0.3],
            points_per_axis: 3,
            max_relative_deviation: None,
            harmonics: None,
        };
        let points = region.lattice_points([1.0, 2.0, 3.0]);
        assert_eq!(points.len(), 27);
        assert!(points.contains(&[0.9, 1.8, 2.7]));
        assert!(points.contains(&[1.1, 2.2, 3.3]));
        assert!(points.contains(&[1.0, 2.0, 3.0]));

        let corners_only = GoodFieldRegion {
            half_extents_m: [0.1, 0.2, 0.3],
            points_per_axis: 2,
            max_relative_deviation: None,
            harmonics: None,
        };
        let points = corners_only.lattice_points([0.0, 0.0, 0.0]);
        assert_eq!(points.len(), 8);
        assert!(points.contains(&[-0.1, -0.2, -0.3]));
        assert!(points.contains(&[0.1, 0.2, 0.3]));
    }

    #[test]
    fn overlaps_pack_detects_straight_and_bend_overlap() {
        let region = GoodFieldRegion {
            half_extents_m: [0.05, 0.05, 0.05],
            points_per_axis: 3,
            max_relative_deviation: None,
            harmonics: None,
        };
        // Pack 0.012 wide at bend_radius 0.095 (inner face 0.089): the
        // region's in-plane corner rho = 0.0707 stays clear.
        assert!(!region.overlaps_pack([0.0; 3], 0.075, 0.095, 0.012, 0.048));
        // A 0.25-wide pack at the same outline has an inner face of -0.03:
        // every straight lattice point at |y| <= 0.05 sits inside the band.
        assert!(region.overlaps_pack([0.0; 3], 0.075, 0.095, 0.5, 0.048));
        // The pack occupies |z| <= h/2 = 0.024: a lattice whose every
        // point sits at |z| = 0.05 is axially clear even when in-plane it
        // lies inside the band (y = +/-0.095 within [0.089, 0.101]).
        let axial_only = GoodFieldRegion {
            half_extents_m: [0.01, 0.095, 0.05],
            points_per_axis: 2,
            max_relative_deviation: None,
            harmonics: None,
        };
        assert!(!axial_only.overlaps_pack([0.0; 3], 0.075, 0.095, 0.012, 0.048));
        // With z half extent 0.01 the same in-plane band hits the pack.
        let in_band_at_midplane = GoodFieldRegion {
            half_extents_m: [0.01, 0.095, 0.01],
            points_per_axis: 2,
            max_relative_deviation: None,
            harmonics: None,
        };
        assert!(in_band_at_midplane.overlaps_pack([0.0; 3], 0.075, 0.095, 0.012, 0.048));
        // Bend side: a region whose x half extent reaches past the straight
        // into the bend annulus overlaps when the pack is wide.
        let bend_side = GoodFieldRegion {
            half_extents_m: [0.18, 0.01, 0.01],
            points_per_axis: 3,
            max_relative_deviation: None,
            harmonics: None,
        };
        assert!(bend_side.overlaps_pack([0.0; 3], 0.075, 0.095, 0.5, 0.048));
        assert!(!bend_side.overlaps_pack([0.0; 3], 0.075, 0.095, 0.012, 0.048));
    }

    #[test]
    fn v3_accepts_a_manufacturing_block_and_validates_it() {
        let block = r#""manufacturing": {"min_inner_bend_radius_m": 0.08},
  "execution":"#;
        let json = minimal_case_json_v3(oc004_dataset_block(), 5, valid_region_block()).replacen(
            "\"execution\":",
            block,
            1,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.manufacturing.unwrap().min_inner_bend_radius_m, 0.08);

        for bad in [
            r#""manufacturing": {"min_inner_bend_radius_m": 0.0},"#,
            r#""manufacturing": {"min_inner_bend_radius_m": -0.1},"#,
            r#""manufacturing": {"min_inner_bend_radius_m": 2e6},"#,
        ] {
            let json = minimal_case_json_v3(oc004_dataset_block(), 5, valid_region_block())
                .replacen("\"execution\":", &format!("{bad}\n  \"execution\":"), 1);
            assert!(
                CoupledSearchCase::from_json(&json).is_err(),
                "manufacturing block must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn v1_and_v2_schemas_must_not_declare_a_manufacturing_block() {
        let block = r#""manufacturing": {"min_inner_bend_radius_m": 0.08},
  "execution":"#;
        let v1 = minimal_case_json(oc004_dataset_block(), 5).replacen("\"execution\":", block, 1);
        assert!(CoupledSearchCase::from_json(&v1).is_err());
        let v2 = minimal_case_json_v2(oc004_dataset_block(), 5, valid_refinement_block()).replacen(
            "\"execution\":",
            block,
            1,
        );
        assert!(CoupledSearchCase::from_json(&v2).is_err());
    }

    /// Recursively stringify every number into a canonical decimal string
    /// (plain form, no exponent) -- the encoding a canonical-profile case
    /// authored for Core lineage uses.
    fn canonicalize_numbers(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Number(n) => {
                *value = serde_json::Value::String(canonical_decimal(
                    n.as_f64()
                        .unwrap_or(n.as_u64().map(|u| u as f64).unwrap_or_default()),
                ));
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(canonicalize_numbers);
            }
            serde_json::Value::Object(members) => {
                members.values_mut().for_each(canonicalize_numbers);
            }
            _ => {}
        }
    }

    fn canonical_decimal(f: f64) -> String {
        let shortest = format!("{f}");
        let Some((mantissa, exponent)) = shortest.split_once(['e', 'E']) else {
            return shortest;
        };
        let exponent: i32 = exponent.parse().unwrap();
        let negative = mantissa.starts_with('-');
        let mantissa = mantissa.trim_start_matches('-');
        let (int_part, frac_part) = mantissa
            .split_once('.')
            .map_or((mantissa.to_string(), String::new()), |(i, f)| {
                (i.to_string(), f.to_string())
            });
        let digits = format!("{int_part}{frac_part}");
        let point = int_part.len() as i32 + exponent;
        let plain = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        if negative { format!("-{plain}") } else { plain }
    }

    #[test]
    fn a_canonical_profile_case_parses_identically_to_the_native_one() {
        // Integer-looking decimals ("30") and exponent values (1e-9) both
        // normalize to the same model value as their binary-float form.
        let native = minimal_case_json_v3(oc004_dataset_block(), 5, valid_region_block());
        let mut canonical: serde_json::Value = serde_json::from_str(&native).unwrap();
        canonicalize_numbers(&mut canonical);
        // Confirm the transform actually produced decimal strings.
        assert!(canonical["requirement"]["b_target_t"].is_string());
        assert_eq!(
            canonical["requirement"]["tolerance_fraction"],
            serde_json::Value::String("0.000000001".into())
        );
        let parsed = CoupledSearchCase::from_json(&canonical.to_string()).unwrap();
        let reference = CoupledSearchCase::from_json(&native).unwrap();
        assert_eq!(
            parsed.requirement.b_target_t,
            reference.requirement.b_target_t
        );
        assert_eq!(
            parsed.fixed_geometry.bend_radius_m,
            reference.fixed_geometry.bend_radius_m
        );
        assert_eq!(
            parsed.choices.turns_along_normal,
            reference.choices.turns_along_normal
        );
        assert_eq!(
            parsed.manufacturing.is_none(),
            reference.manufacturing.is_none()
        );
        // Two-pass means previously valid input is untouched: a native case
        // whose string field merely looks numeric ("id": "200") parses on
        // pass 1 and is never normalized.
        let mut odd = serde_json::from_str::<serde_json::Value>(&native).unwrap();
        odd["id"] = serde_json::Value::String("200".into());
        let odd_parsed = CoupledSearchCase::from_json(&odd.to_string()).unwrap();
        assert_eq!(odd_parsed.id, "200");
        // The edge that cannot be represented: a *canonical* case (floats
        // as strings, so pass 1 fails) whose string field is
        // numeric-looking -- normalization turns "200" into a number and
        // the string field rejects it. Fails loudly, not silently.
        let mut odd_canonical: serde_json::Value = serde_json::from_str(&native).unwrap();
        odd_canonical["id"] = serde_json::Value::String("200".into());
        canonicalize_numbers(&mut odd_canonical);
        assert!(CoupledSearchCase::from_json(&odd_canonical.to_string()).is_err());
    }

    #[test]
    fn refinement_brackets_must_exactly_match_pancake_counts() {
        // Missing a bracket for pancake count 3.
        let missing = r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 200},
    "brackets": [{"tapes": 2, "fail_turns": 10, "pass_turns": 50}],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, missing);
        assert!(CoupledSearchCase::from_json(&json).is_err());

        // An extra bracket for an undeclared pancake count.
        let extra = r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 200},
    "brackets": [
      {"tapes": 2, "fail_turns": 10, "pass_turns": 50},
      {"tapes": 3, "fail_turns": 10, "pass_turns": 50},
      {"tapes": 4, "fail_turns": 10, "pass_turns": 50}
    ],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, extra);
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn refinement_bracket_fail_turns_must_be_strictly_less_than_pass_turns() {
        let inverted = r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 200},
    "brackets": [
      {"tapes": 2, "fail_turns": 50, "pass_turns": 10},
      {"tapes": 3, "fail_turns": 10, "pass_turns": 50}
    ],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, inverted);
        assert!(CoupledSearchCase::from_json(&json).is_err());

        let equal = r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 200},
    "brackets": [
      {"tapes": 2, "fail_turns": 10, "pass_turns": 10},
      {"tapes": 3, "fail_turns": 10, "pass_turns": 50}
    ],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, equal);
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn refinement_bracket_ends_must_lie_within_turn_bounds() {
        let outside = r#""refinement": {
    "pancake_counts": [2, 3],
    "turn_resolution": 2,
    "turn_bounds": {"min": 1, "max": 40},
    "brackets": [
      {"tapes": 2, "fail_turns": 10, "pass_turns": 50},
      {"tapes": 3, "fail_turns": 10, "pass_turns": 30}
    ],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, outside);
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn refinement_turn_bounds_must_be_a_strictly_increasing_range() {
        let reversed = r#""refinement": {
    "pancake_counts": [2],
    "turn_resolution": 2,
    "turn_bounds": {"min": 100, "max": 50},
    "brackets": [{"tapes": 2, "fail_turns": 60, "pass_turns": 80}],
    "monotonicity_check": true
  }"#;
        let json = minimal_case_json_v2(oc004_dataset_block(), 5, reversed);
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn embedded_oc008_case_parses_and_matches_the_contract() {
        // Structural parse/validate only -- never runs the search itself.
        let case = CoupledSearchCase::embedded_oc008().unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V2);
        assert_eq!(case.baseline.turns_along_normal, 120);
        assert_eq!(case.baseline.tapes_along_width, 4);
        let refinement = case.refinement.as_ref().unwrap();
        assert_eq!(refinement.pancake_counts, vec![2, 3, 4, 5, 6]);
        assert_eq!(refinement.turn_resolution, 2);
        assert_eq!(refinement.brackets.len(), 5);
    }

    #[test]
    fn build_coupled_case_rejects_a_geometry_that_fails_downstream_validation() {
        // radial_pitch_m * turns must stay within Pack::validate's numerical
        // domain; an absurdly large turn count blows the pack out of range.
        let mut search = CoupledSearchCase::embedded().unwrap();
        search.fixed_geometry.radial_pitch_m = 1e5;
        let stations = search.sampling.stations.clone();
        let turns = expand_relative_turn_indices(&search.sampling.relative_turn_indices, 200);
        assert!(
            build_coupled_case(
                &search,
                200,
                3,
                1,
                1.0,
                stations,
                turns,
                vec![1],
                None,
                CandidateDims::default()
            )
            .is_err()
        );
    }

    fn minimal_case_json_v5(dataset_block: &str, correction_field: &str) -> String {
        minimal_case_json_v3(dataset_block, 5, valid_region_block())
            .replacen(
                "\"optcoil-coupled-search/v3\"",
                "\"optcoil-coupled-search/v5\"",
                1,
            )
            .replacen(
                "\"utilization_limit\": 0.8",
                &format!("\"utilization_limit\": 0.8{correction_field}"),
                1,
            )
    }

    #[test]
    fn v5_case_with_uniform_transport_correction_parses_and_validates() {
        let json = minimal_case_json_v5(
            oc004_dataset_block(),
            r#", "self_field_correction": "uniform_transport""#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V5);
        assert_eq!(
            case.limits.self_field_correction.as_deref(),
            Some("uniform_transport")
        );
    }

    #[test]
    fn v5_rejects_an_unknown_self_field_correction_model() {
        let json = minimal_case_json_v5(
            oc004_dataset_block(),
            r#", "self_field_correction": "critical_state""#,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v4_case_must_not_declare_self_field_correction() {
        let json = minimal_case_json_v5(
            oc004_dataset_block(),
            r#", "self_field_correction": "uniform_transport""#,
        )
        .replacen(
            "\"optcoil-coupled-search/v5\"",
            "\"optcoil-coupled-search/v4\"",
            1,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v5_generated_coupled_case_uses_the_v2_conductor_schema() {
        let json = minimal_case_json_v5(
            oc004_dataset_block(),
            r#", "self_field_correction": "uniform_transport""#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        let stations = vec![crate::coupled::Station::Straight {
            id: "s0".into(),
            x_m: 0.0,
        }];
        let coupled = build_coupled_case(
            &case,
            2,
            2,
            1,
            100.0,
            stations.clone(),
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled.schema, crate::coupled::COUPLED_CASE_SCHEMA_V2);
        // The same build under an uncorrected v4 case keeps the v1 schema.
        let json_v4 = json
            .replacen(
                "\"optcoil-coupled-search/v5\"",
                "\"optcoil-coupled-search/v4\"",
                1,
            )
            .replacen(r#", "self_field_correction": "uniform_transport""#, "", 1);
        let case_v4 = CoupledSearchCase::from_json(&json_v4).unwrap();
        let coupled_v4 = build_coupled_case(
            &case_v4,
            2,
            2,
            1,
            100.0,
            stations,
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled_v4.schema, crate::coupled::COUPLED_CASE_SCHEMA);
    }

    fn minimal_case_json_v6(dataset_block: &str, along_current_field: &str) -> String {
        minimal_case_json_v5(dataset_block, along_current_field).replacen(
            "\"optcoil-coupled-search/v5\"",
            "\"optcoil-coupled-search/v6\"",
            1,
        )
    }

    #[test]
    fn v6_case_with_transverse_bound_parses_and_validates() {
        let json = minimal_case_json_v6(
            oc004_dataset_block(),
            r#", "along_current_model": "transverse_bound""#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V6);
        assert_eq!(
            case.limits.along_current_model.as_deref(),
            Some("transverse_bound")
        );
    }

    #[test]
    fn v6_rejects_an_unknown_along_current_model() {
        let json = minimal_case_json_v6(
            oc004_dataset_block(),
            r#", "along_current_model": "worst_angle""#,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v5_case_must_not_declare_along_current_model() {
        let json = minimal_case_json_v5(
            oc004_dataset_block(),
            r#", "along_current_model": "transverse_bound""#,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v6_generated_coupled_case_uses_the_v3_conductor_schema() {
        let json = minimal_case_json_v6(
            oc004_dataset_block(),
            r#", "along_current_model": "transverse_bound""#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        let stations = vec![crate::coupled::Station::Straight {
            id: "s0".into(),
            x_m: 0.0,
        }];
        let coupled = build_coupled_case(
            &case,
            2,
            2,
            1,
            100.0,
            stations,
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled.schema, crate::coupled::COUPLED_CASE_SCHEMA_V3);
        // The same build without the model (and with no self-field
        // correction declared) keeps the v1 schema.
        let json_no_model = json.replacen(r#", "along_current_model": "transverse_bound""#, "", 1);
        let case_no_model = CoupledSearchCase::from_json(&json_no_model).unwrap();
        let coupled_no_model = build_coupled_case(
            &case_no_model,
            2,
            2,
            1,
            100.0,
            vec![crate::coupled::Station::Straight {
                id: "s0".into(),
                x_m: 0.0,
            }],
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled_no_model.schema, crate::coupled::COUPLED_CASE_SCHEMA);
    }

    fn minimal_case_json_v7(mechanical_block: &str) -> String {
        minimal_case_json_v6(oc004_dataset_block(), "")
            .replacen(
                "\"optcoil-coupled-search/v6\"",
                "\"optcoil-coupled-search/v7\"",
                1,
            )
            .replacen(
                "\"execution\":",
                &format!("{mechanical_block}\n  \"execution\":"),
                1,
            )
    }

    #[test]
    fn v7_case_with_the_hoop_stress_pair_parses_and_validates() {
        let json = minimal_case_json_v7(
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "max_hoop_stress_pa": 4.5e8, "tension_section_area_m2": 1.2e-6},"#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V7);
        let mechanical = case.mechanical.as_ref().unwrap();
        assert_eq!(mechanical.max_hoop_stress_pa, Some(4.5e8));
        assert_eq!(mechanical.tension_section_area_m2, Some(1.2e-6));
    }

    #[test]
    fn v7_allows_the_lorentz_only_mechanical_block() {
        // The hoop pair is optional: a v4-style mechanical block stays
        // valid on v7.
        let json = minimal_case_json_v7(r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5},"#);
        let case = CoupledSearchCase::from_json(&json).unwrap();
        let mechanical = case.mechanical.as_ref().unwrap();
        assert_eq!(mechanical.max_hoop_stress_pa, None);
    }

    #[test]
    fn v6_case_must_not_declare_the_hoop_stress_fields() {
        let json = minimal_case_json_v7(
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "max_hoop_stress_pa": 4.5e8, "tension_section_area_m2": 1.2e-6},"#,
        )
        .replacen(
            "\"optcoil-coupled-search/v7\"",
            "\"optcoil-coupled-search/v6\"",
            1,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn hoop_stress_limit_requires_the_section_area_and_vice_versa() {
        for block in [
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "max_hoop_stress_pa": 4.5e8},"#,
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "tension_section_area_m2": 1.2e-6},"#,
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "max_hoop_stress_pa": 0.0, "tension_section_area_m2": 1.2e-6},"#,
            r#""mechanical": {"max_lorentz_load_n_per_m": 5.0e5, "max_hoop_stress_pa": 4.5e8, "tension_section_area_m2": -1.0},"#,
        ] {
            let json = minimal_case_json_v7(block);
            assert!(
                CoupledSearchCase::from_json(&json).is_err(),
                "mechanical block must be rejected: {block}"
            );
        }
    }

    // OC-014 Phase 2: the critical-state correction lands its fields in the
    // shared limits block; `critical_state_fields` carries both the model
    // value and the declared layer thickness.
    fn minimal_case_json_v8(dataset_block: &str, critical_state_fields: &str) -> String {
        minimal_case_json_v6(dataset_block, critical_state_fields).replacen(
            "\"optcoil-coupled-search/v6\"",
            "\"optcoil-coupled-search/v8\"",
            1,
        )
    }

    #[test]
    fn v8_case_with_critical_state_strip_parses_and_validates() {
        let json = minimal_case_json_v8(
            oc004_dataset_block(),
            r#", "self_field_correction": "critical_state_strip", "critical_state_layer_thickness_m": 1.0e-6"#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V8);
        assert_eq!(
            case.limits.self_field_correction.as_deref(),
            Some("critical_state_strip")
        );
        assert_eq!(case.limits.critical_state_layer_thickness_m, Some(1.0e-6));
    }

    #[test]
    fn v8_critical_state_strip_requires_the_layer_thickness() {
        let json = minimal_case_json_v8(
            oc004_dataset_block(),
            r#", "self_field_correction": "critical_state_strip""#,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v8_layer_thickness_requires_the_critical_state_model() {
        // The declared thickness with no critical-state model is a stray
        // field that must not silently change screening semantics.
        for correction in [
            r#", "critical_state_layer_thickness_m": 1.0e-6"#,
            r#", "self_field_correction": "uniform_transport", "critical_state_layer_thickness_m": 1.0e-6"#,
        ] {
            let json = minimal_case_json_v8(oc004_dataset_block(), correction);
            assert!(
                CoupledSearchCase::from_json(&json).is_err(),
                "limits block must be rejected: {correction}"
            );
        }
    }

    #[test]
    fn v7_must_not_declare_critical_state_strip() {
        // The correction value changes dominance-regime semantics, so a v7
        // case declaring it is rejected rather than run under the wrong model.
        let json = minimal_case_json_v8(
            oc004_dataset_block(),
            r#", "self_field_correction": "critical_state_strip", "critical_state_layer_thickness_m": 1.0e-6"#,
        )
        .replacen(
            "\"optcoil-coupled-search/v8\"",
            "\"optcoil-coupled-search/v7\"",
            1,
        );
        assert!(CoupledSearchCase::from_json(&json).is_err());
    }

    #[test]
    fn v8_generated_coupled_case_uses_the_v4_conductor_schema() {
        let json = minimal_case_json_v8(
            oc004_dataset_block(),
            r#", "self_field_correction": "critical_state_strip", "critical_state_layer_thickness_m": 1.0e-6"#,
        );
        let case = CoupledSearchCase::from_json(&json).unwrap();
        let stations = vec![crate::coupled::Station::Straight {
            id: "s0".into(),
            x_m: 0.0,
        }];
        let coupled = build_coupled_case(
            &case,
            2,
            2,
            1,
            100.0,
            stations,
            vec![1],
            vec![1],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled.schema, crate::coupled::COUPLED_CASE_SCHEMA_V4);
    }

    #[test]
    fn oc019_d_shape_case_parses_and_validates_as_v9() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V9);
        let path = case.fixed_geometry.path.as_ref().unwrap();
        assert_eq!(path.segments.len(), 4);
        assert!(case.fixed_geometry.straight_half_length_m.is_none());
        assert!(case.fixed_geometry.bend_radius_m.is_none());
        // D-shape: inner leg 0.7 m + corner arcs 2x(0.08 * pi/2) + outer
        // bulge 0.43 * pi => 0.7 + 0.51*pi.
        assert!(
            (path.length_m() - (0.7 + 0.51 * std::f64::consts::PI)).abs() < 1e-12,
            "path length {} m",
            path.length_m()
        );
        assert!((path.min_radius_m() - 0.08).abs() < 1e-15);
        assert!((path.max_radius_m() - 0.43).abs() < 1e-15);
        assert!(
            case.sampling
                .stations
                .iter()
                .all(|s| matches!(s, Station::Path { .. }))
        );
        assert_eq!(case.refined_plan.additional_stations.len(), 6);
        assert!(
            case.refined_plan
                .additional_stations
                .iter()
                .all(|s| matches!(s, Station::Path { .. }))
        );
    }

    #[test]
    fn v9_generated_coupled_case_uses_the_v5_conductor_schema_and_path_pack() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        let stations = case.sampling.stations.clone();
        let turns = expand_relative_turn_indices(&case.sampling.relative_turn_indices, 200);
        let coupled = build_coupled_case(
            &case,
            200,
            3,
            1,
            100.0,
            stations,
            turns,
            vec![1, 2, 3],
            None,
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled.schema, crate::coupled::COUPLED_CASE_SCHEMA_V5);
        assert!(coupled.pack.path.is_some());
        assert!(coupled.pack.straight_half_length_m.is_none());
        assert!(coupled.pack.bend_radius_m.is_none());
        // w = 200 * 0.0005 = 0.1 m; inner face offset is -w/2.
        assert!((coupled.pack.inner_rho_m().unwrap() - (-0.05)).abs() < 1e-15);
    }

    #[test]
    fn v9_geometry_declarations_are_exclusive() {
        let json = OC019_JSON;
        // Both representations declared: ambiguous — must reject.
        let both = json.replacen(
            "\"path\": {",
            "\"straight_half_length_m\": 0.3, \"bend_radius_m\": 0.2, \"path\": {",
            1,
        );
        assert!(CoupledSearchCase::from_json(&both).is_err());
        // Neither: geometry-less — must reject.
        let neither = minimal_case_json_v8(oc004_dataset_block(), "")
            .replacen(
                "\"optcoil-coupled-search/v8\"",
                "\"optcoil-coupled-search/v9\"",
                1,
            )
            .replacen("\"straight_half_length_m\": 0.3,\n    ", "", 1)
            .replacen("\"bend_radius_m\": 0.2,\n    ", "", 1);
        assert!(CoupledSearchCase::from_json(&neither).is_err());
        // Path on an earlier schema: not yet a legal declaration.
        let v8 = json.replacen(
            "\"optcoil-coupled-search/v9\"",
            "\"optcoil-coupled-search/v8\"",
            1,
        );
        assert!(CoupledSearchCase::from_json(&v8).is_err());
    }

    #[test]
    fn station_kinds_must_match_the_declared_geometry() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        let path_pack = Pack {
            straight_half_length_m: None,
            bend_radius_m: None,
            radial_width_m: 0.1,
            axial_height_m: 0.036,
            path: case.fixed_geometry.path.clone(),
            path3d: None,
        };
        let legacy_pack = Pack {
            straight_half_length_m: Some(0.3),
            bend_radius_m: Some(0.2),
            radial_width_m: 0.1,
            axial_height_m: 0.036,
            path: None,
            path3d: None,
        };
        let path_station = Station::Path {
            id: "p".into(),
            s_m: 1.0,
        };
        assert!(path_station.validate(&path_pack).is_ok());
        assert!(path_station.validate(&legacy_pack).is_err());
        let straight = Station::Straight {
            id: "s".into(),
            x_m: 0.0,
        };
        assert!(straight.validate(&legacy_pack).is_ok());
        assert!(straight.validate(&path_pack).is_err());
        let arc = Station::Arc {
            id: "a".into(),
            azimuth_deg: 45.0,
        };
        assert!(arc.validate(&legacy_pack).is_ok());
        assert!(arc.validate(&path_pack).is_err());
        // s beyond the centerline length is out of domain.
        let too_far = Station::Path {
            id: "p".into(),
            s_m: 2.4,
        };
        assert!(too_far.validate(&path_pack).is_err());
    }

    #[test]
    fn path_station_positions_are_centerline_plus_signed_offset() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        let path = case.fixed_geometry.path.as_ref().unwrap();
        let pack = Pack {
            straight_half_length_m: None,
            bend_radius_m: None,
            radial_width_m: 0.1,
            axial_height_m: 0.036,
            path: Some(path.clone()),
            path3d: None,
        };
        // leg_mid sits at s=0.35 on the inner leg: pose (0, 0) heading -y.
        // Right of -y travel is -x, so the path normal points outward from
        // the D's bore (the loop interior is +x). A tape at offset
        // rho=+0.02, z=0.01 lands at (-0.02, 0, 0.01).
        let station = Station::Path {
            id: "leg_mid".into(),
            s_m: 0.35,
        };
        let pos = station.position_m(&pack, 0.02, 0.01).unwrap();
        assert!((pos[0] - (-0.02)).abs() < 1e-12);
        assert!(pos[1].abs() < 1e-12);
        assert!((pos[2] - 0.01).abs() < 1e-12);
        let frame = station
            .frame(
                crate::coupled::TapeNormal::Radial,
                Some(crate::coupled::Centerline::Planar(path)),
            )
            .unwrap();
        assert!((frame.t[1] - (-1.0)).abs() < 1e-12);
        assert!((frame.n[0] - (-1.0)).abs() < 1e-12);
    }

    #[test]
    fn overlaps_pack_on_path_detects_the_winding_band() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        let path = case.fixed_geometry.path.as_ref().unwrap();
        let region = case.requirement.good_field_region.as_ref().unwrap();
        // The declared good-field box sits inside the D's bore — no overlap
        // for the 200-turn pack (w = 0.1 m).
        assert!(!region.overlaps_pack_on_path(case.requirement.bore_probe_m, path, 0.1, 0.036));
        // A box centered on the inner leg (x=0) does overlap it.
        let on_leg = GoodFieldRegion {
            half_extents_m: [0.005, 0.005, 0.005],
            points_per_axis: 3,
            max_relative_deviation: None,
            harmonics: None,
        };
        assert!(on_leg.overlaps_pack_on_path([0.0, 0.0, 0.0], path, 0.1, 0.036));
        // ...but not when the probe box is far above the pack's z extent.
        assert!(!on_leg.overlaps_pack_on_path([0.0, 0.0, 0.5], path, 0.1, 0.036));
        // The racetrack analogue still answers the same question on a
        // racetrack-shaped path: a probe on the outer bulge overlaps.
        assert!(on_leg.overlaps_pack_on_path([0.51, 0.0, 0.0], path, 0.1, 0.036));
    }

    #[test]
    fn v9_case_round_trips_the_path_block() {
        let case = CoupledSearchCase::embedded_oc019().unwrap();
        let json = serde_json::to_string(&case).unwrap();
        assert!(json.contains("\"path\""));
        let reparsed = CoupledSearchCase::from_json(&json).unwrap();
        let path = reparsed.fixed_geometry.path.as_ref().unwrap();
        assert_eq!(path.segments.len(), 4);
        // A legacy case must not gain a path key on reserialization.
        let legacy = CoupledSearchCase::embedded().unwrap();
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        assert!(!legacy_json.contains("\"path\""));
        assert!(legacy_json.contains("\"straight_half_length_m\""));
    }
}

#[cfg(test)]
mod v10_graded_tests {
    use super::*;

    #[test]
    fn oc020_parses_validates_and_counts_assignments() {
        let case = CoupledSearchCase::embedded_oc020().unwrap();
        let grading = case.grading.as_ref().unwrap();
        assert_eq!(grading.regions.len(), 2);
        assert_eq!(case.assignment_count(), 4);
        assert_eq!(case.candidate_count(), 60);
        assert_eq!(
            case.assignments(),
            vec![
                vec!["base".to_string(), "base".to_string()],
                vec!["base".to_string(), "hts-lowfield".to_string()],
                vec!["hts-lowfield".to_string(), "base".to_string()],
                vec!["hts-lowfield".to_string(), "hts-lowfield".to_string()],
            ]
        );
    }

    #[test]
    fn oc020_per_turn_spec_resolution_uses_the_assigned_region() {
        let case = CoupledSearchCase::embedded_oc020().unwrap();
        let assignment = vec!["hts-lowfield".to_string(), "base".to_string()];
        // turns=200: region 0 covers 1..=100, region 1 covers 101..=200.
        assert_eq!(case.spec_id_for_turn(200, 1, &assignment), "hts-lowfield");
        assert_eq!(case.spec_id_for_turn(200, 100, &assignment), "hts-lowfield");
        assert_eq!(case.spec_id_for_turn(200, 101, &assignment), "base");
        assert_eq!(case.spec_id_for_turn(200, 200, &assignment), "base");
        // The material binding follows the resolved spec.
        assert_eq!(
            case.material_for_turn(200, 50, Some(&assignment))
                .dataset_id,
            "robinson-superpower-ap-v3-lowfield"
        );
        assert_eq!(
            case.material_for_turn(200, 150, Some(&assignment))
                .dataset_id,
            "robinson-superpower-ap-v3"
        );
        // Prices follow the resolved spec, not the region.
        assert_eq!(case.spec_price_usd_per_m("hts-lowfield"), Some(18.0));
        assert_eq!(case.spec_price_usd_per_m("base"), Some(30.0));
    }

    #[test]
    fn oc020_generated_case_emits_v6_regions_and_only_referenced_specs() {
        let case = CoupledSearchCase::embedded_oc020().unwrap();
        // Assignment: inner half stays base, outer half takes hts-lowfield.
        let assignment = vec!["base".to_string(), "hts-lowfield".to_string()];
        let stations = case.sampling.stations.clone();
        let turn_indices = expand_relative_turn_indices(&case.sampling.relative_turn_indices, 200);
        let coupled = build_coupled_case(
            &case,
            200,
            3,
            1,
            100.0,
            stations,
            turn_indices,
            vec![1, 2, 3],
            Some(&assignment),
            CandidateDims::default(),
        )
        .unwrap();
        assert_eq!(coupled.schema, crate::coupled::COUPLED_CASE_SCHEMA_V6);
        let regions = coupled.winding.regions.as_ref().unwrap();
        assert_eq!(regions.len(), 1, "only the non-base region is emitted");
        assert_eq!(regions[0].first_turn, 101);
        assert_eq!(regions[0].last_turn, 200);
        assert_eq!(regions[0].tape_spec, "hts-lowfield");
        let specs = coupled.tape_specs.as_ref().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs["hts-lowfield"].dataset_id,
            "robinson-superpower-ap-v3-lowfield"
        );
        coupled.validate().unwrap();

        // An all-base assignment emits no regions; with no path and no
        // limits extensions the generated case falls back to the minimal
        // coupled schema (v1).
        let all_base = vec!["base".to_string(), "base".to_string()];
        let stations = case.sampling.stations.clone();
        let turn_indices = expand_relative_turn_indices(&case.sampling.relative_turn_indices, 200);
        let coupled_base = build_coupled_case(
            &case,
            200,
            3,
            1,
            100.0,
            stations,
            turn_indices,
            vec![1, 2, 3],
            Some(&all_base),
            CandidateDims::default(),
        )
        .unwrap();
        assert!(coupled_base.winding.regions.is_none());
        assert!(coupled_base.tape_specs.is_none());
        assert_eq!(coupled_base.schema, crate::coupled::COUPLED_CASE_SCHEMA);
    }

    #[test]
    fn oc020_dataset_map_validation_covers_every_binding() {
        let case = CoupledSearchCase::embedded_oc020().unwrap();
        let base = MaterialDataset::embedded_by_id("robinson-superpower-ap-v3").unwrap();
        let lowfield =
            MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-lowfield").unwrap();
        let mut datasets = BTreeMap::new();
        datasets.insert(base.metadata.id.clone(), base);
        datasets.insert(lowfield.metadata.id.clone(), lowfield);
        case.validate_against_dataset_map(&datasets).unwrap();
        // Dropping the spec's dataset is an error, never a substitute.
        datasets.remove("robinson-superpower-ap-v3-lowfield");
        assert!(case.validate_against_dataset_map(&datasets).is_err());
    }

    #[test]
    fn v10_rejects_refinement_and_requires_a_baseline_assignment() {
        let json = OC020_JSON.replace(
            "\"baseline\": {\n    \"turns_along_normal\": 160,\n    \"tapes_along_width\": 4,\n    \"tape_spec_ids\": [\"base\", \"base\"]\n  }",
            "\"baseline\": {\"turns_along_normal\": 160, \"tapes_along_width\": 4}",
        );
        assert!(
            CoupledSearchCase::from_json(&json).is_err(),
            "a graded case without baseline.tape_spec_ids must fail validation"
        );

        let with_refine = OC020_JSON.replace(
            "\"pruning\": null,",
            "\"pruning\": null,\n  \"refinement\": {\"pancake_counts\": [2], \"turn_resolution\": 2, \"turn_bounds\": {\"min\": 1, \"max\": 400}, \"brackets\": [{\"tapes\": 2, \"fail_turns\": 1, \"pass_turns\": 400}], \"monotonicity_check\": true},",
        );
        let err = CoupledSearchCase::from_json(&with_refine).unwrap_err();
        assert!(
            err.to_string().contains("refinement"),
            "expected a refinement rejection, got {err}"
        );
    }

    #[test]
    fn v10_rejects_bad_grading_declarations() {
        // Reserved id as a spec key.
        let bad_spec = OC020_JSON.replace("\"hts-lowfield\": {", "\"base\": {");
        assert!(CoupledSearchCase::from_json(&bad_spec).is_err());
        // Unknown choice id.
        let bad_choice = OC020_JSON.replace(
            "\"tape_spec_choices\": [\"base\", \"hts-lowfield\"]",
            "\"tape_spec_choices\": [\"base\", \"nonexistent\"]",
        );
        assert!(CoupledSearchCase::from_json(&bad_choice).is_err());
        // Overlapping regions.
        let overlap = OC020_JSON.replace(
            "{\"turn_range\": [0.5, 1.0],",
            "{\"turn_range\": [0.4, 0.9],",
        );
        assert!(CoupledSearchCase::from_json(&overlap).is_err());
        // Grading fields on a v9 schema are gated out entirely.
        let v9 = OC020_JSON.replace(
            "\"schema\": \"optcoil-coupled-search/v10\"",
            "\"schema\": \"optcoil-coupled-search/v9\"",
        );
        assert!(CoupledSearchCase::from_json(&v9).is_err());
    }

    #[test]
    fn v10_region_turn_range_resolves_inclusive_bounds_and_empty_regions_err() {
        let region = GradingRegion {
            turn_range: [0.0, 0.5],
            tape_spec_choices: vec!["base".into()],
        };
        assert_eq!(
            CoupledSearchCase::region_turn_range(&region, 200).unwrap(),
            (1, 100)
        );
        assert_eq!(
            CoupledSearchCase::region_turn_range(&region, 61).unwrap(),
            (1, 30)
        );
        // floor(0.004 * 200) = 0 turns in the region -> unrealizable.
        let empty = GradingRegion {
            turn_range: [0.0, 0.004],
            tape_spec_choices: vec!["base".into()],
        };
        assert!(CoupledSearchCase::region_turn_range(&empty, 200).is_err());
    }

    #[test]
    fn oc022_binds_two_physically_distinct_measured_datasets() {
        let case = CoupledSearchCase::embedded_oc022().unwrap();
        assert_eq!(case.assignment_count(), 4);
        assert_eq!(case.candidate_count(), 60);
        // The two bindings resolve to different embedded measured datasets —
        // the whole point of the fixture.
        assert_eq!(case.material.dataset_id, "robinson-superpower-ap-v3");
        let sh = &case.tape_specs.as_ref().unwrap()["hts-shanghai"];
        assert_eq!(sh.material.dataset_id, "robinson-shanghai-hflt-v3");
        assert_ne!(sh.material.csv_sha256, case.material.csv_sha256);
        // Both ids resolve to embedded datasets whose declared csv hashes
        // match the case's declarations exactly.
        let base =
            crate::material::MaterialDataset::embedded_by_id(&case.material.dataset_id).unwrap();
        let shanghai =
            crate::material::MaterialDataset::embedded_by_id(&sh.material.dataset_id).unwrap();
        assert_eq!(base.metadata.csv_sha256, case.material.csv_sha256);
        assert_eq!(shanghai.metadata.csv_sha256, sh.material.csv_sha256);
        assert_eq!(shanghai.points.len(), 1504);
        // Per-binding declared policies differ: the SH dataset's measured
        // 7→8 T non-monotonicity needs the looser tolerance it declares.
        assert_eq!(sh.material.monotonicity_tolerance, 0.025);
        assert_eq!(case.material.monotonicity_tolerance, 0.001);
    }

    #[test]
    fn oc023_binds_three_physically_distinct_measured_datasets() {
        let case = CoupledSearchCase::embedded_oc023().unwrap();
        assert_eq!(case.assignment_count(), 9);
        assert_eq!(case.candidate_count(), 54);
        let specs = case.tape_specs.as_ref().unwrap();
        assert_eq!(case.material.dataset_id, "robinson-superpower-ap-v3");
        assert_eq!(
            specs["hts-shanghai"].material.dataset_id,
            "robinson-shanghai-hflt-v3"
        );
        assert_eq!(
            specs["hts-theva"].material.dataset_id,
            "robinson-theva-ap-v2"
        );
        // All three bindings resolve to distinct embedded measured
        // datasets whose hashes match the case's declarations.
        let mut shas = std::collections::BTreeSet::new();
        shas.insert(case.material.csv_sha256.clone());
        for binding in specs.values() {
            let ds = crate::material::MaterialDataset::embedded_by_id(&binding.material.dataset_id)
                .unwrap();
            assert_eq!(ds.metadata.csv_sha256, binding.material.csv_sha256);
            shas.insert(binding.material.csv_sha256.clone());
        }
        let base =
            crate::material::MaterialDataset::embedded_by_id(&case.material.dataset_id).unwrap();
        assert_eq!(base.metadata.csv_sha256, case.material.csv_sha256);
        assert_eq!(shas.len(), 3);
        // THEVA's measured field sweeps are monotone: it declares the
        // strict tolerance, unlike Shanghai's instrument-edge 0.025.
        assert_eq!(specs["hts-theva"].material.monotonicity_tolerance, 0.001);
        assert_eq!(specs["hts-shanghai"].material.monotonicity_tolerance, 0.025);
    }
}

#[cfg(test)]
mod v11_graded_refine_tests {
    use super::*;

    /// OC-020's graded case re-schematized to v11, with a refinement block
    /// spliced in before `"execution"`. v11 is the first schema where a
    /// graded case may (and must, under the v2+ convention) declare one.
    fn oc020_v11(refinement_json: Option<&str>) -> String {
        let mut json = OC020_JSON.replace(
            "\"schema\": \"optcoil-coupled-search/v10\"",
            "\"schema\": \"optcoil-coupled-search/v11\"",
        );
        if let Some(refinement) = refinement_json {
            json = json.replacen(
                "\"execution\": {",
                &format!("{refinement},\n  \"execution\": {{"),
                1,
            );
        }
        json
    }

    const REFINEMENT: &str = "\"refinement\": {\"pancake_counts\": [2], \"turn_resolution\": 2, \"turn_bounds\": {\"min\": 1, \"max\": 400}, \"brackets\": [{\"tapes\": 2, \"fail_turns\": 1, \"pass_turns\": 400}], \"monotonicity_check\": true}";

    #[test]
    fn v11_graded_case_with_a_valid_refinement_block_validates() {
        let case = CoupledSearchCase::from_json(&oc020_v11(Some(REFINEMENT))).unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V11);
        assert!(case.grading.is_some());
        assert!(case.refinement.is_some());
        assert_eq!(case.assignment_count(), 4);
    }

    #[test]
    fn v11_graded_case_without_refinement_is_rejected() {
        // v11 sits under the v2+ convention: refinement is required, and
        // grading no longer exempts it (v10's carve-out ended at v10).
        let err = CoupledSearchCase::from_json(&oc020_v11(None)).unwrap_err();
        assert!(
            err.to_string().contains("refinement"),
            "expected a refinement requirement, got {err}"
        );
    }

    #[test]
    fn v11_graded_refinement_must_declare_monotonicity_check() {
        // Region boundaries are floor()-computed from n, so utilization is
        // not guaranteed monotone across a graded slice's bisection; the
        // check is mandatory rather than optional here.
        let without_check = REFINEMENT.replace(
            "\"monotonicity_check\": true",
            "\"monotonicity_check\": false",
        );
        let err = CoupledSearchCase::from_json(&oc020_v11(Some(&without_check))).unwrap_err();
        assert!(
            err.to_string().contains("monotonicity_check"),
            "expected a monotonicity_check requirement, got {err}"
        );
    }

    #[test]
    fn v10_graded_case_still_rejects_refinement() {
        // The v10 boundary must hold exactly: grading at v10 may not
        // declare refinement (assignment-aware bisection did not exist at
        // v10), while the identical block validates at v11.
        let json = OC020_JSON.replacen(
            "\"execution\": {",
            &format!("{REFINEMENT},\n  \"execution\": {{"),
            1,
        );
        let err = CoupledSearchCase::from_json(&json).unwrap_err();
        assert!(
            err.to_string().contains("refinement"),
            "expected a v10 graded refinement rejection, got {err}"
        );
    }
}

#[cfg(test)]
mod v12_transverse_pressure_tests {
    use super::*;

    /// OC-020 re-schematized to v12 with its refinement block (required
    /// on a graded case at v11+) and a mechanical block spliced in before
    /// `"execution"`.
    fn oc020_v12(mechanical_json: &str) -> String {
        OC020_JSON
            .replace(
                "\"schema\": \"optcoil-coupled-search/v10\"",
                "\"schema\": \"optcoil-coupled-search/v12\"",
            )
            .replacen(
                "\"execution\": {",
                &format!(
                    "\"refinement\": {{\"pancake_counts\": [2], \"turn_resolution\": 2, \"turn_bounds\": {{\"min\": 1, \"max\": 400}}, \"brackets\": [{{\"tapes\": 2, \"fail_turns\": 1, \"pass_turns\": 400}}], \"monotonicity_check\": true}},\n  \"mechanical\": {mechanical_json},\n  \"execution\": {{"
                ),
                1,
            )
    }

    #[test]
    fn v12_accepts_a_finite_positive_transverse_pressure_bound() {
        let case = CoupledSearchCase::from_json(&oc020_v12(
            "{\"max_lorentz_load_n_per_m\": 1e6, \"max_transverse_pressure_pa\": 4.0e7}",
        ))
        .unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V12);
        assert_eq!(
            case.mechanical.unwrap().max_transverse_pressure_pa,
            Some(4.0e7)
        );
        // Optional on v12: the bound may be absent entirely.
        CoupledSearchCase::from_json(&oc020_v12("{\"max_lorentz_load_n_per_m\": 1e6}")).unwrap();
    }

    #[test]
    fn v12_rejects_nonpositive_transverse_pressure_bounds() {
        for bad in ["0.0", "-4.0e7"] {
            let err = CoupledSearchCase::from_json(&oc020_v12(&format!(
                "{{\"max_lorentz_load_n_per_m\": 1e6, \"max_transverse_pressure_pa\": {bad}}}"
            )))
            .unwrap_err();
            assert!(
                err.to_string().contains("max_transverse_pressure_pa"),
                "expected a positivity rejection for {bad}, got {err}"
            );
        }
    }

    #[test]
    fn v11_rejects_the_transverse_pressure_field() {
        // The bound is a v12 semantic — a mechanical block declaring it
        // on an earlier schema must fail validation, not silently ignore
        // it.
        let json =
            oc020_v12("{\"max_lorentz_load_n_per_m\": 1e6, \"max_transverse_pressure_pa\": 4.0e7}")
                .replace(
                    "\"schema\": \"optcoil-coupled-search/v12\"",
                    "\"schema\": \"optcoil-coupled-search/v11\"",
                );
        let err = CoupledSearchCase::from_json(&json).unwrap_err();
        assert!(
            err.to_string().contains("max_transverse_pressure_pa"),
            "expected a v12 gating rejection, got {err}"
        );
    }

    #[test]
    fn oc021_parses_validates_and_declares_the_bound() {
        let case = CoupledSearchCase::embedded_oc021().unwrap();
        assert_eq!(case.schema, COUPLED_SEARCH_CASE_SCHEMA_V12);
        let mechanical = case.mechanical.as_ref().expect("v12 mechanical block");
        assert_eq!(mechanical.max_transverse_pressure_pa, Some(3.0e7));
        // The fixture keeps the full declared stack: hoop pair, path
        // geometry, refinement block, manufacturing bound.
        assert!(mechanical.max_hoop_stress_pa.is_some());
        assert!(case.fixed_geometry.path.is_some());
        assert!(case.refinement.is_some());
        assert!(case.grading.is_none());
    }
}

#[cfg(test)]
mod refined_plan_union_tests {
    use super::*;

    #[test]
    fn refined_plan_is_a_superset_of_the_coarse_plan_and_respects_the_index_cap() {
        for n in [120_u32, 160, 200, 240, 300] {
            let coarse: Vec<u32> = [1, 2, 3, 5, 10, 20]
                .into_iter()
                .chain([n / 4, n / 2, 3 * n / 4])
                .chain([n - 19, n - 9, n - 4, n - 1, n])
                .collect();
            let refined = refined_plan_turn_indices(n, &coarse);
            for c in &coarse {
                assert!(
                    refined.contains(c),
                    "n={n}: coarse turn {c} missing from the refined plan"
                );
            }
            for near in [1, 2, 3, n - 2, n - 1, n] {
                assert!(
                    refined.contains(&near),
                    "n={n}: near-face turn {near} missing"
                );
            }
            assert!(
                refined.len() <= 64,
                "n={n}: {} indices exceed the cap",
                refined.len()
            );
            assert!(refined.windows(2).all(|w| w[0] < w[1]));
            assert!(refined.contains(&5) && refined.contains(&(n / 5 * 5)));
        }
    }
}
