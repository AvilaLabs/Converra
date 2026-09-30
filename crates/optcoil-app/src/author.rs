//! Guided coupled-search case builder: a form that emits a valid
//! `optcoil-coupled-search/v24` case JSON. Single-variant material
//! policies are fixed to their only implemented values; everything
//! else is editable. "Save" round-trips through
//! `CoupledSearchCase::from_json`, so what lands on disk is exactly
//! what the runner will parse — never a hand-built JSON string that
//! only looks right.
//!
//! Authored surface: the racetrack pair, a general line+arc `path`,
//! or a helical `path3d` (all v9), `tape_specs`/`grading` (v10+),
//! declared field maps (v13+), v21 geometry search axes, and the
//! declared-screen blocks (thermal margin, AC loss, quench
//! hotspot/transient, screening current, transition — v15–v18).

use eframe::egui::{self, RichText};
use optcoil_model::{
    coupled::{
        AngleMapping, FieldBasisMapping, FieldMagnitudePolicy, FieldMap, LowFieldPolicy,
        MirrorPolicy, Station, TapeNormal,
    },
    coupled_search::COUPLED_SEARCH_CASE_SCHEMA_V24,
    coupled_search::{
        AcLossScreen, Baseline, Bracket, Choices, Cost, CoupledSearchCase, Execution,
        FixedGeometry, GoodFieldRegion, Grading, GradingRegion, HeatLoadTerm, Manufacturing,
        Mechanical, Opex, PieceBoundary, PiecePolicy, PieceUnit, QuenchHotspotScreen,
        QuenchTransientScreen, RefinedPlan, RefinementPlan, RegionHarmonics, Requirement,
        ScreeningCurrentScreen, SearchFieldMap, SearchNumerics, SearchOperating, SearchSampling,
        TapeSpec, ThermalMarginScreen, TransitionScreen, TurnBounds,
    },
    dataset_intake::material_bundle_from_pair,
    material::{MaterialBundle, MaterialDataset},
    path::{CoilPath, PathSegment},
    path3d::{CoilPath3D, PathSegment3D},
    product::{ProductRegistry, RegistryProduct},
};
use sha2::{Digest, Sha256};

mod parse;

use crate::brand;
use parse::{
    parse_f64_list, parse_f64_pairs, parse_offerings, parse_turn_indices, parse_u32_list,
    price_source,
};

/// Centerline family: the racetrack pair, a planar `path` (schema v9),
/// or a non-planar helix `path3d` (schema v9 — requires a declared
/// field map and a radial tape normal).
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CenterlineKind {
    Racetrack,
    Path,
    Path3D,
}

/// Station addressing: racetrack cases sample straights by x and arcs
/// by azimuth; `path`/`path3d` cases (schema v9) address the centerline
/// by arc length `s_m`.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StationKind {
    Straight,
    Arc,
    Path,
}

/// One editable station row.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StationRow {
    id: String,
    kind: StationKind,
    /// `x_m` for straights, `azimuth_deg` for arcs, `s_m` on a path.
    param: f64,
}

impl StationRow {
    fn station(&self) -> Result<Station, String> {
        if self.id.trim().is_empty() {
            return Err("station id must not be empty".into());
        }
        Ok(match self.kind {
            StationKind::Arc => Station::Arc {
                id: self.id.trim().into(),
                azimuth_deg: self.param,
            },
            StationKind::Path => Station::Path {
                id: self.id.trim().into(),
                s_m: self.param,
            },
            StationKind::Straight => Station::Straight {
                id: self.id.trim().into(),
                x_m: self.param,
            },
        })
    }
}

/// One editable `path` segment row: `Line { length_m }` or
/// `Arc { radius_m, sweep_deg }`. Both fields stay in the row; the
/// non-applicable one is ignored on emit.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSegmentRow {
    /// 0 = line, 1 = arc.
    kind: usize,
    length_m: f64,
    radius_m: f64,
    sweep_deg: f64,
}

impl PathSegmentRow {
    fn segment(&self, index: usize) -> Result<PathSegment, String> {
        Ok(if self.kind == 1 {
            if self.radius_m <= 0.0 {
                return Err(format!(
                    "segment {}: arc radius must be positive",
                    index + 1
                ));
            }
            if self.sweep_deg == 0.0 {
                return Err(format!("segment {}: arc sweep must be nonzero", index + 1));
            }
            PathSegment::Arc {
                radius_m: self.radius_m,
                sweep_deg: self.sweep_deg,
            }
        } else {
            if self.length_m <= 0.0 {
                return Err(format!(
                    "segment {}: line length must be positive",
                    index + 1
                ));
            }
            PathSegment::Line {
                length_m: self.length_m,
            }
        })
    }
}

/// A field map arriving off the event loop — either a picked file or
/// the filament-model generator's output.
enum PendingFieldMap {
    /// Only the browser's async file pick produces this variant.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Picked(String, Result<FieldMap, String>),
    Generated(Result<optcoil_search::fieldmap::GeneratedFieldMap, String>),
}

enum PendingMaterialPair {
    Loaded(String, Result<MaterialBundle, String>),
}

/// One editable `path3d` helix segment — the CCT/CORC element.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelixSegmentRow {
    axis_origin_m: [f64; 3],
    axis_dir: [f64; 3],
    radius_m: f64,
    start_azimuth_deg: f64,
    turns: f64,
    rise_per_turn_m: f64,
}

impl HelixSegmentRow {
    /// A solenoid-layer winding like the OC-031 benchmark — the seed
    /// when switching the centerline to helix mode.
    fn solenoid() -> Self {
        Self {
            axis_origin_m: [0.0; 3],
            axis_dir: [0.0, 0.0, 1.0],
            radius_m: 0.025,
            start_azimuth_deg: 0.0,
            turns: 50.0,
            rise_per_turn_m: 0.004,
        }
    }

    fn segment(&self) -> PathSegment3D {
        PathSegment3D::Helix {
            axis_origin_m: self.axis_origin_m,
            axis_dir: self.axis_dir,
            radius_m: self.radius_m,
            start_azimuth_deg: self.start_azimuth_deg,
            turns: self.turns,
            rise_per_turn_m: self.rise_per_turn_m,
        }
    }
}

/// One editable opex heat-load term row.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HeatLoadRow {
    id: String,
    power_w: f64,
}

/// One editable tape-spec row: the dataset + price that distinguish a
/// purchasable conductor variant; screening policies inherit the
/// case-level material declaration.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TapeSpecRow {
    id: String,
    /// Index into `DATASET_CHOICES`; `DATASET_CUSTOM` means free-text id+sha.
    dataset_choice: usize,
    dataset_id: String,
    dataset_sha: String,
    price_usd_per_m: f64,
    /// Per-spec measured-floor clamp — the dataset's own domain bound.
    low_field_clamp_t: f64,
    /// Schema v24: this spec's piece catalogue — `length_m : $/m` pairs,
    /// comma-separated; empty means the unquantized `price_usd_per_m`.
    piece_offerings_text: String,
    /// 0 = undeclared, 1..=4 = synthetic/estimated/published/quoted.
    price_source: usize,
}

/// One editable grading region row.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GradingRow {
    /// Turn-range fraction bounds (inner face → outer).
    lo: f64,
    hi: f64,
    /// Comma-separated spec ids this region may take (`base` = the
    /// case-level conductor).
    specs_text: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseDraft {
    id: String,
    provenance: String,
    bore_probe: [f64; 3],
    b_target_t: f64,
    tolerance_fraction: f64,
    use_region: bool,
    region_half: [f64; 3],
    region_points: u32,
    /// Schema v22: optional uniformity bound on the region lattice.
    use_uniformity_bound: bool,
    max_relative_deviation: f64,
    /// Schema v23: optional midplane harmonic expansion.
    use_harmonics: bool,
    harmonic_radius_m: f64,
    harmonic_max_order: u32,
    harmonic_theta_samples: u32,
    use_harmonic_normal_bound: bool,
    harmonic_normal_bound: f64,
    use_harmonic_skew_bound: bool,
    harmonic_skew_bound: f64,
    straight_half_length_m: f64,
    bend_radius_m: f64,
    /// Centerline family — a `path`/`path3d` suppresses the racetrack
    /// dims and their search axes.
    centerline: CenterlineKind,
    path_segments: Vec<PathSegmentRow>,
    path_start_x_m: f64,
    path_start_y_m: f64,
    path_start_heading_deg: f64,
    /// Schema v9 helix segments — a `path3d` winding (CCT/CORC class).
    /// Open by default: a helical layer's ends are its leads.
    path3d_segments: Vec<HelixSegmentRow>,
    path3d_closed: bool,
    /// Schema v21: racetrack dims as declared search axes — when set,
    /// the fixed field is absent and the axis list + baseline apply.
    search_bend_radius: bool,
    bend_radius_text: String,
    baseline_bend_radius: f64,
    search_straight_half: bool,
    straight_half_text: String,
    baseline_straight_half: f64,
    radial_pitch_m: f64,
    tape_width_mm: f64,
    tape_normal_radial: bool,
    turns_text: String,
    tapes_text: String,
    strands_text: String,
    temperature_k: f64,
    e_criterion: f64,
    /// Index into `DATASET_CHOICES`; `DATASET_CUSTOM` means free-text id+sha.
    dataset_choice: usize,
    dataset_id: String,
    dataset_sha: String,
    /// 0 = custom dataset binding; n>0 = embedded product registry row
    /// `n-1`. Picking a product fills the dataset, tape width, price and
    /// piece-catalogue fields below.
    product_choice: usize,
    /// Index into the picked product's `dataset_variants`; 0 selects the
    /// product's own `dataset_id`.
    dataset_variant_choice: usize,
    /// Requirement preset: 0 custom · 1 solenoid bore · 2 insert geometry
    /// seed · 3 field-map driven. Presets only fill fields —
    /// every emitted value stays visible and editable.
    preset_choice: usize,
    /// The last sizing estimate and its render — kept so the search-space
    /// step can show the derivation, not just the numbers it proposed.
    #[serde(skip)]
    sizing_hint: Option<optcoil_search::sizing::SizingHint>,
    #[serde(skip)]
    sizing_error: Option<String>,
    low_field_clamp_t: f64,
    monotonicity_tolerance: f64,
    stations: Vec<StationRow>,
    turn_indices_text: String,
    max_along_fraction: f64,
    max_self_ratio: f64,
    overprediction_budget: f64,
    utilization_limit: f64,
    /// 0 none · 1 uniform_transport · 2 critical_state_strip
    self_field_correction: usize,
    /// 0 none · 1 transverse_bound
    along_current_model: usize,
    critical_state_layer_um: f64,
    quadrature_lo: u32,
    quadrature_hi: u32,
    field_scale_t: f64,
    max_refinement_change_fraction: f64,
    price_usd_per_m: f64,
    scrap_fraction: f64,
    assembly_usd: f64,
    joint_usd: f64,
    /// Schema v20: declared refrigeration economics at the operating
    /// point — off by default like the other optional declarations.
    use_opex: bool,
    heat_loads: Vec<HeatLoadRow>,
    cop_fraction: f64,
    sink_temperature_k: f64,
    electricity_usd_per_kwh: f64,
    opex_hours_per_year: f64,
    opex_years: u32,
    baseline_turns: u32,
    baseline_tapes: u32,
    baseline_strands: u32,
    refined_stations: Vec<StationRow>,
    max_sampling_shortfall: f64,
    pancake_counts_text: String,
    turn_resolution: u32,
    turn_bounds_min: u32,
    turn_bounds_max: u32,
    /// Schema v10: purchasable conductor variants beyond the base.
    tape_specs: Vec<TapeSpecRow>,
    /// Schema v10: grading regions — turn-range + allowed spec ids.
    grading: Vec<GradingRow>,
    /// Baseline spec assignment — one id per grading region, each
    /// from that region's allowed list (schema requires it when
    /// grading is declared).
    baseline_specs_text: String,
    /// Schema v14: a loaded Cartesian field map — the map bytes are the
    /// customer's artifact; only its identity is editable here.
    field_map: Option<FieldMap>,
    field_map_label: String,
    /// The CSV sidecar a generated map binds — keepable as the
    /// provenance artifact; `None` for imported maps.
    field_map_source_csv: Option<Vec<u8>>,
    /// Validated external bundle bytes retained across interrupted authoring.
    #[serde(default)]
    imported_material_bundle_json: Option<String>,
    /// Filament-model generation controls (path3d only).
    map_gen_spacing_m: f64,
    map_gen_margin_m: f64,
    map_gen_reference_ni: f64,
    /// Async field-map work — the browser's file pick and (on both
    /// targets) the filament generator's result land here and apply at
    /// the top of `show`.
    #[serde(skip)]
    field_map_pending: Option<std::sync::mpsc::Receiver<PendingFieldMap>>,
    #[serde(skip)]
    material_pair_pending: Option<std::sync::mpsc::Receiver<PendingMaterialPair>>,
    imported_material_label: Option<String>,
    field_map_bore_field_t: f64,
    /// Pack extents — emitted only under a declared map, which
    /// validates candidates against them.
    pack_radial_width_m: f64,
    pack_axial_height_m: f64,
    /// v15–v18 declared screens — each a checkbox + its declared bounds.
    use_thermal_margin: bool,
    min_margin_k: f64,
    use_ac_loss: bool,
    ac_frequency_hz: f64,
    ac_transport_fraction: f64,
    ac_sc_layer_um: f64,
    ac_max_loss_w: f64,
    use_quench_hotspot: bool,
    hotspot_dump_s: f64,
    hotspot_stabilizer_mm2: f64,
    hotspot_u_text: String,
    hotspot_max_k: f64,
    use_screening_current: bool,
    screening_max_width_fraction: f64,
    use_transition: bool,
    transition_max_e_over_ec: f64,
    use_transition_voltage: bool,
    transition_max_voltage_v: f64,
    use_quench_transient: bool,
    qt_dump_s: f64,
    use_qt_initial_t: bool,
    qt_initial_t_k: f64,
    qt_conducting_area_per_width_m: f64,
    qt_rho_text: String,
    qt_cv_text: String,
    use_qt_max_t: bool,
    qt_max_t_k: f64,
    use_qt_detection: bool,
    qt_detection_voltage_v: f64,
    qt_max_detection_s: f64,
    /// Schema v24: piece-quantized procurement — discrete piece lengths,
    /// splice pricing, and the price-provenance class.
    use_piece_policy: bool,
    /// `length_m : $/m` pairs, comma-separated — empty falls back to a
    /// single `piece_length_m`.
    piece_offerings_text: String,
    piece_length_m: f64,
    /// 0 = per_module, 1 = continuous.
    piece_boundary: usize,
    /// 0 = conductor_unit, 1 = per_strand.
    piece_unit: usize,
    splice_cost_usd: f64,
    /// 0 = undeclared, 1..=4 = synthetic/estimated/published/quoted.
    price_source: usize,
    /// Human-readable citation or estimate basis. The enum stays in the
    /// cost declaration; this text is carried in case provenance.
    price_basis_text: String,
    /// Guided mode: `Some(step)` renders the wizard pages over the same
    /// fields; `None` renders the complete field form. The wizard never
    /// hides anything permanently — "All fields…" drops to the full
    /// editor with everything the wizard set preserved.
    guided_step: Option<u8>,
    /// Loaded cases are edited as their original JSON document so every
    /// schema declaration survives revisions, including fields unknown to
    /// the guided builder.
    advanced_json: Option<String>,
    /// Parsed source document for selective, lossless structured edits.
    /// We patch only the paths exposed below, preserving every other key.
    revision_value: Option<serde_json::Value>,
    revision_turns: String,
    revision_tapes: String,
    revision_strands: String,
    revision_strands_explicit: bool,
    revision_baseline_specs: String,
    revision_json_invalid: bool,
    revision_dirty: bool,
    material_choices: Vec<(String, String)>,
    /// One runtime-checked bracket per pancake (tapes) count.
    brackets: Vec<[u32; 3]>,
    monotonicity_check: bool,
    use_manufacturing: bool,
    min_inner_bend_m: f64,
    use_mechanical: bool,
    max_lorentz_kn_per_m: f64,
    use_hoop: bool,
    max_hoop_mpa: f64,
    tension_area_mm2: f64,
    use_pressure: bool,
    max_pressure_mpa: f64,
    max_threads: u32,
    #[serde(skip)]
    error: Option<String>,
}

const DATASET_CHOICES: &[&str] = &[
    "robinson-superpower-ap-v3",
    "robinson-superpower-ap-v3-lowfield",
    "robinson-superpower-ap-v3-modelext",
    "robinson-shanghai-hflt-v3",
    "robinson-theva-ap-v2",
];
const DATASET_CUSTOM: usize = DATASET_CHOICES.len();

impl Default for CaseDraft {
    /// OC-012-flavored v8 defaults: the modern screening stack (good-field
    /// region, manufacturing and mechanical gates, critical-state
    /// self-field bound, transverse along-current bound) at a
    /// dipole-benchmark scale — change what the customer changes, keep
    /// the evidence-grade defaults.
    fn default() -> Self {
        let dataset_id = DATASET_CHOICES[2].to_string();
        let dataset_sha = MaterialDataset::embedded_by_id(&dataset_id)
            .map(|d| d.metadata.csv_sha256)
            .unwrap_or_default();
        Self {
            id: "new-coupled-search".into(),
            provenance: "Authored in the Converra case builder — describe where this requirement, geometry and material data came from.".into(),
            bore_probe: [0.0, 0.0, 0.0],
            b_target_t: 12.0,
            tolerance_fraction: 1e-9,
            use_region: true,
            region_half: [0.04, 0.04, 0.03],
            region_points: 3,
            use_uniformity_bound: false,
            max_relative_deviation: 0.001,
            use_harmonics: false,
            harmonic_radius_m: 0.02,
            harmonic_max_order: 4,
            harmonic_theta_samples: 32,
            use_harmonic_normal_bound: false,
            harmonic_normal_bound: 0.001,
            use_harmonic_skew_bound: false,
            harmonic_skew_bound: 0.001,
            straight_half_length_m: 0.15,
            bend_radius_m: 0.09,
            centerline: CenterlineKind::Racetrack,
            path_segments: Vec::new(),
            path_start_x_m: 0.0,
            path_start_y_m: 0.0,
            path_start_heading_deg: 0.0,
            path3d_segments: Vec::new(),
            path3d_closed: false,
            search_bend_radius: false,
            bend_radius_text: "0.09, 0.12, 0.15".into(),
            baseline_bend_radius: 0.09,
            search_straight_half: false,
            straight_half_text: "0.15, 0.20, 0.25".into(),
            baseline_straight_half: 0.15,
            radial_pitch_m: 0.0001,
            tape_width_mm: 12.0,
            tape_normal_radial: true,
            turns_text: "200, 320, 480, 640, 800".into(),
            tapes_text: "4, 6, 8, 12".into(),
            strands_text: "1".into(),
            temperature_k: 20.0,
            e_criterion: 1e-4,
            dataset_choice: 2,
            dataset_id,
            dataset_sha,
            product_choice: 0,
            dataset_variant_choice: 0,
            preset_choice: 0,
            sizing_hint: None,
            sizing_error: None,
            low_field_clamp_t: 0.0501,
            monotonicity_tolerance: 0.002,
            stations: vec![
                StationRow { id: "straight_center".into(), kind: StationKind::Straight, param: 0.0 },
                StationRow { id: "straight_near_junction".into(), kind: StationKind::Straight, param: 0.14 },
                StationRow { id: "arc_apex".into(), kind: StationKind::Arc, param: 0.0 },
                StationRow { id: "arc_mid".into(), kind: StationKind::Arc, param: 45.0 },
            ],
            turn_indices_text:
                "start:1,2,3,5,10,20 frac:0.25,0.5,0.75 end:19,9,4,1,0".into(),
            max_along_fraction: 0.2,
            max_self_ratio: 0.1,
            overprediction_budget: 0.1,
            utilization_limit: 0.8,
            self_field_correction: 2,
            along_current_model: 1,
            critical_state_layer_um: 2.0,
            quadrature_lo: 10,
            quadrature_hi: 14,
            field_scale_t: 1.0,
            max_refinement_change_fraction: 0.0025,
            price_usd_per_m: 62.5,
            scrap_fraction: 0.1,
            assembly_usd: 500.0,
            joint_usd: 200.0,
            use_opex: false,
            heat_loads: vec![
                HeatLoadRow { id: "static".into(), power_w: 50.0 },
                HeatLoadRow { id: "leads".into(), power_w: 20.0 },
            ],
            cop_fraction: 0.15,
            sink_temperature_k: 300.0,
            electricity_usd_per_kwh: 0.12,
            opex_hours_per_year: 4000.0,
            opex_years: 10,
            baseline_turns: 800,
            baseline_tapes: 12,
            baseline_strands: 1,
            refined_stations: vec![
                StationRow { id: "arc_15".into(), kind: StationKind::Arc, param: 15.0 },
                StationRow { id: "arc_30".into(), kind: StationKind::Arc, param: 30.0 },
                StationRow { id: "arc_60".into(), kind: StationKind::Arc, param: 60.0 },
                StationRow { id: "arc_75".into(), kind: StationKind::Arc, param: 75.0 },
                StationRow { id: "straight_005".into(), kind: StationKind::Straight, param: 0.05 },
                StationRow { id: "straight_010".into(), kind: StationKind::Straight, param: 0.10 },
            ],
            tape_specs: Vec::new(),
            grading: Vec::new(),
            baseline_specs_text: "base".into(),
            field_map: None,
            field_map_label: String::new(),
            field_map_source_csv: None,
            imported_material_bundle_json: None,
            map_gen_spacing_m: 0.005,
            map_gen_margin_m: 0.01,
            map_gen_reference_ni: 1.0e4,
            field_map_pending: None,
            material_pair_pending: None,
            imported_material_label: None,
            field_map_bore_field_t: 1.0,
            pack_radial_width_m: 0.048,
            pack_axial_height_m: 0.012,
            use_thermal_margin: false,
            min_margin_k: 5.0,
            use_ac_loss: false,
            ac_frequency_hz: 0.1,
            ac_transport_fraction: 0.5,
            ac_sc_layer_um: 2.0,
            ac_max_loss_w: 100.0,
            use_quench_hotspot: false,
            hotspot_dump_s: 0.5,
            hotspot_stabilizer_mm2: 5.0,
            hotspot_u_text: "15:1.0e16, 77:3.0e16, 150:9.0e16, 300:4.0e17".into(),
            hotspot_max_k: 400.0,
            use_screening_current: false,
            screening_max_width_fraction: 0.3,
            use_transition: false,
            transition_max_e_over_ec: 1.0,
            use_transition_voltage: false,
            transition_max_voltage_v: 10.0,
            use_quench_transient: false,
            qt_dump_s: 0.5,
            use_qt_initial_t: false,
            qt_initial_t_k: 40.0,
            qt_conducting_area_per_width_m: 4e-4,
            qt_rho_text: "20:3e-10, 77:5e-10, 150:1.5e-9, 300:1.6e-8".into(),
            qt_cv_text: "20:1e5, 77:5e5, 150:1.5e6, 300:3.5e6".into(),
            use_qt_max_t: false,
            qt_max_t_k: 400.0,
            use_qt_detection: false,
            qt_detection_voltage_v: 1.0,
            qt_max_detection_s: 0.1,
            max_sampling_shortfall: 0.02,
            pancake_counts_text: "4, 6, 8, 12".into(),
            turn_resolution: 10,
            turn_bounds_min: 1,
            turn_bounds_max: 1000,
            use_piece_policy: false,
            piece_offerings_text: String::new(),
            piece_length_m: 300.0,
            piece_boundary: 0,
            piece_unit: 0,
            splice_cost_usd: 500.0,
            // Builder prices are placeholders — mark them synthetic so
            // records that embed the case keep the dollar figures honest.
            price_source: 1,
            price_basis_text: "Synthetic placeholder from the case builder; replace with a specific price source before use.".into(),
            // OC-012-style hints: fail at 100 turns, pass near
            // ~3840/tapes — checked at run time, not trusted.
            guided_step: None,
            advanced_json: None,
            revision_value: None,
            revision_turns: String::new(),
            revision_tapes: String::new(),
            revision_strands: String::new(),
            revision_strands_explicit: false,
            revision_baseline_specs: String::new(),
            revision_json_invalid: false,
            revision_dirty: false,
            material_choices: DATASET_CHOICES
                .iter()
                .filter_map(|id| MaterialDataset::embedded_by_id(id).ok().map(|d| (id.to_string(), d.metadata.csv_sha256)))
                .collect(),
            brackets: vec![[4, 100, 800], [6, 100, 640], [8, 100, 480], [12, 100, 320]],
            monotonicity_check: true,
            use_manufacturing: true,
            min_inner_bend_m: 0.03,
            use_mechanical: true,
            max_lorentz_kn_per_m: 600.0,
            use_hoop: true,
            max_hoop_mpa: 450.0,
            tension_area_mm2: 0.6,
            // Off by default: a declared delamination bound is a
            // mechanical claim the author should make deliberately,
            // not a default that sneaks into every saved case.
            use_pressure: false,
            max_pressure_mpa: 30.0,
            max_threads: 4,
            error: None,
        }
    }
}

impl CaseDraft {
    /// Serialize the complete editable surface for crash recovery. This does
    /// not validate the case or discard invalid unsaved JSON edits.
    pub fn recovery_json(&self) -> Result<String, String> {
        let json = serde_json::to_string(self).map_err(|error| error.to_string())?;
        if json.len() > crate::recovery::MAX_AUTHOR_BYTES {
            return Err("author draft exceeds recovery size limit".into());
        }
        Ok(json)
    }

    /// Restore editable fields while leaving async work and derived estimates
    /// reset. Invalid case content is preserved exactly for further editing.
    pub fn from_recovery_json(json: &str) -> Result<Self, String> {
        if json.len() > crate::recovery::MAX_AUTHOR_BYTES {
            return Err("author draft exceeds recovery size limit".into());
        }
        let draft: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if let Some(bundle_json) = &draft.imported_material_bundle_json {
            MaterialBundle::from_json(bundle_json)
                .map_err(|error| format!("invalid retained material bundle: {error}"))?;
        }
        Ok(draft)
    }

    /// Bundle imported into this draft, if any. The workspace coordinator can
    /// install it when the draft is saved, avoiding a second file selection.
    pub fn imported_bundle_json(&self) -> Option<&str> {
        let json = self.imported_material_bundle_json.as_deref()?;
        let bundle = MaterialBundle::from_json(json).ok()?;
        (bundle.dataset.metadata.id == self.dataset_id
            && bundle.dataset.metadata.csv_sha256 == self.dataset_sha)
            .then_some(json)
    }

    /// Apply a previously validated imported bundle through the same field
    /// binding used by the metadata/CSV intake flow.
    pub fn apply_imported_material(
        &mut self,
        bundle: MaterialBundle,
        label: String,
    ) -> Result<(), String> {
        let bundle_json = bundle.to_json().map_err(|error| error.to_string())?;
        if self.advanced_json.is_some() {
            if self.revision_json_invalid {
                let message = "cannot bind imported material while advanced case JSON is invalid";
                self.error = Some(message.into());
                return Err(message.into());
            }
            let mut value = self
                .revision_value
                .clone()
                .ok_or_else(|| "revision source JSON is unavailable".to_string())?;
            let material = value
                .get_mut("material")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| "revision case has no material object to update".to_string())?;
            material.insert(
                "dataset_id".into(),
                serde_json::Value::String(bundle.dataset.metadata.id.clone()),
            );
            material.insert(
                "csv_sha256".into(),
                serde_json::Value::String(bundle.dataset.metadata.csv_sha256.clone()),
            );
            let advanced_json =
                serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
            self.dataset_id = bundle.dataset.metadata.id.clone();
            self.dataset_sha = bundle.dataset.metadata.csv_sha256.clone();
            self.dataset_choice = DATASET_CUSTOM;
            self.imported_material_bundle_json = Some(bundle_json);
            self.imported_material_label = Some(format!(
                "Validated {label}: {} — {:?}",
                bundle.dataset.metadata.id, bundle.dataset.metadata.data_class
            ));
            self.revision_value = Some(value);
            self.advanced_json = Some(advanced_json);
            self.revision_dirty = true;
            self.error = None;
            return Ok(());
        }
        self.apply_material_pair(Ok(bundle), label);
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(())
    }

    /// Open a validated loaded case in the lossless advanced JSON editor.
    pub fn revision(json: &str) -> Result<Self, String> {
        let case = CoupledSearchCase::from_json(json).map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut draft = Self {
            advanced_json: Some(json.to_owned()),
            revision_turns: array_as_text(&value, &["choices", "turns_along_normal"]),
            revision_tapes: array_as_text(&value, &["choices", "tapes_along_width"]),
            revision_strands: if json_path(&value, &["choices", "strands_parallel"]).is_some() {
                array_as_text(&value, &["choices", "strands_parallel"])
            } else {
                "1".into()
            },
            revision_strands_explicit: json_path(&value, &["choices", "strands_parallel"])
                .is_some(),
            revision_baseline_specs: string_array_as_text(&value, &["baseline", "tape_spec_ids"]),
            revision_value: Some(value),
            ..Self::default()
        };
        draft.id = case.id;
        draft.provenance = case.provenance;
        Ok(draft)
    }

    pub fn set_material_choices(&mut self, choices: Vec<(String, String)>) {
        let mut unique = std::collections::BTreeMap::new();
        for (id, sha) in self.material_choices.drain(..).chain(choices) {
            unique.insert(id, sha);
        }
        self.material_choices = unique.into_iter().collect();
    }

    /// Duplicate from the exact source document so declarations unknown to
    /// the current typed schema survive as well.
    pub fn duplicate_json(json: &str) -> Result<Self, String> {
        let mut draft = Self::revision(json)?;
        let value = draft.revision_value.as_mut().expect("revision value");
        let source_id = value["id"].as_str().unwrap_or("case").to_owned();
        value["id"] = serde_json::Value::String(format!("{source_id}-copy"));
        let provenance = value["provenance"].as_str().unwrap_or_default();
        value["provenance"] = serde_json::Value::String(format!(
            "{}\nDuplicated from case `{source_id}`; review this copy's identity and provenance.",
            provenance.trim_end()
        ));
        draft.sync_revision_json()?;
        Ok(draft)
    }

    fn sync_revision_json(&mut self) -> Result<(), String> {
        if let Some(value) = &self.revision_value {
            self.advanced_json =
                Some(serde_json::to_string_pretty(value).map_err(|e| e.to_string())?);
        }
        Ok(())
    }

    fn revision_controls(&mut self, ui: &mut egui::Ui) {
        if self.revision_json_invalid {
            ui.colored_label(status_red(), "Advanced JSON does not validate against the case schema. Correct it before using structured controls.");
            return;
        }
        let material_choices = self.material_choices.clone();
        let Some(value) = self.revision_value.as_mut() else {
            return;
        };
        ui.label(RichText::new("Decision inputs").strong().color(brand::BLUE));
        ui.small("These controls patch only the fields shown. Grading, field maps, path3d, schema and policy declarations remain in the source document.");
        let mut changed = false;
        form_section_with_open(ui, "Identity", false, |ui| {
            changed |= ui.text_edit_singleline(&mut self.id).changed();
            ui.small("Case id");
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut self.provenance)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                )
                .changed();
            ui.small("Provenance");
        });
        form_section(ui, "Requirement", |ui| {
            changed |= value_float(
                ui,
                value,
                &["requirement", "b_target_t"],
                "Target field (T)",
            );
            changed |= value_float(
                ui,
                value,
                &["requirement", "tolerance_fraction"],
                "Tolerance fraction",
            );
            if let Some(probe) = value
                .pointer_mut("/requirement/bore_probe_m")
                .and_then(serde_json::Value::as_array_mut)
            {
                ui.label("Bore probe (m)");
                for (i, label) in ["x", "y", "z"].iter().enumerate() {
                    if let Some(mut x) = probe.get(i).and_then(serde_json::Value::as_f64) {
                        let value_changed = ui
                            .horizontal(|ui| {
                                ui.label(*label);
                                ui.add(egui::DragValue::new(&mut x).speed(0.001)).changed()
                            })
                            .inner;
                        if value_changed {
                            changed = true;
                            probe[i] = serde_json::Number::from_f64(x)
                                .map(Into::into)
                                .unwrap_or(serde_json::Value::Null);
                        }
                    }
                }
            }
            changed |= value_float(
                ui,
                value,
                &["requirement", "good_field_region", "half_extents_m", "0"],
                "Region half width x (m)",
            );
            changed |= value_float(
                ui,
                value,
                &["requirement", "good_field_region", "half_extents_m", "1"],
                "Region half width y (m)",
            );
            changed |= value_float(
                ui,
                value,
                &["requirement", "good_field_region", "half_extents_m", "2"],
                "Region half width z (m)",
            );
        });
        form_section(ui, "Fixed geometry", |ui| {
            let editable_racetrack = value
                .get("field_map")
                .is_none_or(serde_json::Value::is_null)
                && value.get("path").is_none_or(serde_json::Value::is_null)
                && value.get("path3d").is_none_or(serde_json::Value::is_null)
                && value.pointer("/choices/bend_radius_m").is_none()
                && value.pointer("/choices/straight_half_length_m").is_none();
            if editable_racetrack
                && value
                    .get("fixed_geometry")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|g| g.contains_key("bend_radius_m"))
            {
                changed |= value_float(
                    ui,
                    value,
                    &["fixed_geometry", "straight_half_length_m"],
                    "Straight half length (m)",
                );
                changed |= value_float(
                    ui,
                    value,
                    &["fixed_geometry", "bend_radius_m"],
                    "Bend radius (m)",
                );
                changed |= value_float(
                    ui,
                    value,
                    &["fixed_geometry", "radial_pitch_m"],
                    "Radial pitch (m)",
                );
            } else {
                ui.small("Geometry is controlled by a path, field map or declared search axis. These dimensions are not effective inputs for this case.");
            }
        });
        form_section(ui, "Pack choices and baseline", |ui| {
            changed |= ui.text_edit_singleline(&mut self.revision_turns).changed();
            ui.label("Turns along normal (comma separated)");
            changed |= ui.text_edit_singleline(&mut self.revision_tapes).changed();
            ui.label("Tapes along width (comma separated)");
            let strands_changed = ui
                .text_edit_singleline(&mut self.revision_strands)
                .changed();
            changed |= strands_changed;
            self.revision_strands_explicit |= strands_changed;
            ui.label("Parallel strands (comma separated)");
            if json_path(value, &["baseline", "tape_spec_ids"]).is_some() {
                changed |= ui
                    .text_edit_singleline(&mut self.revision_baseline_specs)
                    .changed();
                ui.label("Baseline tape spec per grading region");
            }
            for (key, label) in [
                ("turns_along_normal", "Baseline turns"),
                ("tapes_along_width", "Baseline tapes"),
                ("strands_parallel", "Baseline strands"),
            ] {
                changed |= value_u64(ui, value, &["baseline", key], label);
            }
        });
        form_section(ui, "Material, prices and limits", |ui| {
            changed |= value_float(
                ui,
                value,
                &["operating", "temperature_k"],
                "Operating temperature (K)",
            );
            changed |= value_float(
                ui,
                value,
                &["operating", "electric_field_criterion_v_per_m"],
                "Electric field criterion (V/m)",
            );
            changed |= value_float(
                ui,
                value,
                &["fixed_geometry", "tape_width_m"],
                "Tape width (m)",
            );
            changed |= value_float(
                ui,
                value,
                &["fixed_geometry", "pack_radial_width_m"],
                "Pack radial width (m)",
            );
            changed |= value_float(
                ui,
                value,
                &["fixed_geometry", "pack_axial_height_m"],
                "Pack axial height (m)",
            );
            changed |= value_material_binding(
                ui,
                value,
                &["material"],
                "Base conductor dataset",
                &material_choices,
            );
            changed |= value_float(
                ui,
                value,
                &["cost", "price_usd_per_m"],
                "Base tape price (USD/m)",
            );
            changed |= value_float(ui, value, &["cost", "scrap_fraction"], "Scrap fraction");
            changed |= value_float(
                ui,
                value,
                &["cost", "assembly_cost_per_pancake_usd"],
                "Assembly (USD/pancake)",
            );
            changed |= value_float(ui, value, &["cost", "joint_cost_usd"], "Joint cost (USD)");
            changed |= value_choice(
                ui,
                value,
                &["cost", "price_source"],
                "Base price source",
                &["synthetic", "estimated", "published", "quoted"],
            );
            for (key, label) in [
                (
                    "max_along_current_field_fraction",
                    "Max along-current field fraction",
                ),
                ("max_self_field_ratio", "Max self-field ratio"),
                (
                    "interpolation_overprediction_budget",
                    "Interpolation overprediction budget",
                ),
                ("utilization_limit", "Utilization limit"),
            ] {
                changed |= value_float(ui, value, &["limits", key], label);
            }
            ui.small(
                "Tape bindings and their per-spec prices are shown below when grading is declared.",
            );
            if let Some(specs) = value
                .get_mut("tape_specs")
                .and_then(serde_json::Value::as_object_mut)
            {
                for (id, spec) in specs {
                    let id = id.clone();
                    ui.collapsing(format!("{id} binding and price"), |ui| {
                        changed |= value_float_in(ui, spec, "price_usd_per_m", "Price (USD/m)");
                        changed |= value_choice_in(
                            ui,
                            spec,
                            "price_source",
                            "Price source",
                            &["synthetic", "estimated", "published", "quoted"],
                        );
                        changed |= value_material_binding(
                            ui,
                            spec,
                            &["material"],
                            "Conductor dataset",
                            &material_choices,
                        );
                    });
                }
            }
        });
        form_section(ui, "Execution", |ui| {
            changed |= value_u64(ui, value, &["execution", "max_threads"], "Maximum threads");
        });
        if changed {
            self.revision_dirty = true;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("id".into(), self.id.clone().into());
                obj.insert("provenance".into(), self.provenance.clone().into());
            }
            let patch_existing = |value: &mut serde_json::Value, path: &[&str], text: &str| {
                if json_path(value, path).is_some() {
                    patch_array(value, path, text)
                } else {
                    Ok(())
                }
            };
            if let Err(error) = patch_existing(
                value,
                &["choices", "turns_along_normal"],
                &self.revision_turns,
            )
            .and_then(|_| {
                patch_existing(
                    value,
                    &["choices", "tapes_along_width"],
                    &self.revision_tapes,
                )
            })
            .and_then(|_| {
                let path = &["choices", "strands_parallel"];
                if json_path(value, path).is_some() || self.revision_strands_explicit {
                    patch_array(value, path, &self.revision_strands)
                } else {
                    Ok(())
                }
            })
            .and_then(|_| {
                if json_path(value, &["baseline", "tape_spec_ids"]).is_some() {
                    patch_string_array(
                        value,
                        &["baseline", "tape_spec_ids"],
                        &self.revision_baseline_specs,
                    )
                } else {
                    Ok(())
                }
            }) {
                self.error = Some(error);
            } else {
                self.error = None;
                let _ = self.sync_revision_json();
            }
        }
    }

    /// Guided authoring: the evidence-grade defaults plus a step-by-step
    /// walk of the fields a customer actually changes — requirement,
    /// envelope, search space, material, cost, identity. Every step's
    /// answers land on the same struct, so "All fields…" drops into the
    /// complete editor with nothing lost.
    pub fn guided() -> Self {
        Self {
            guided_step: Some(0),
            ..Self::default()
        }
    }

    /// Assemble the declared `path` centerline from the segment table —
    /// unvalidated, so the preview can draw an in-progress (open) path.
    /// `build()` re-validates for closure before emitting.
    fn coil_path(&self) -> Result<CoilPath, String> {
        if self.path_segments.is_empty() {
            return Err("a custom path needs at least one segment".into());
        }
        let mut segments = Vec::with_capacity(self.path_segments.len());
        for (i, row) in self.path_segments.iter().enumerate() {
            segments.push(row.segment(i)?);
        }
        Ok(CoilPath {
            segments,
            start_position_m: [self.path_start_x_m, self.path_start_y_m],
            start_heading_deg: self.path_start_heading_deg,
        })
    }

    /// Assemble the declared `path3d` centerline — unvalidated, like
    /// `coil_path`, so the preview can draw mid-edit. `build()` runs
    /// `CoilPath3D::validate` (continuity +, when declared, closure).
    fn coil_path3d(&self) -> Result<CoilPath3D, String> {
        if self.path3d_segments.is_empty() {
            return Err("a helix path needs at least one segment".into());
        }
        Ok(CoilPath3D {
            segments: self
                .path3d_segments
                .iter()
                .map(HelixSegmentRow::segment)
                .collect(),
            closed: self.path3d_closed,
        })
    }

    /// Build the typed case; the caller still round-trips it through
    /// `CoupledSearchCase::from_json` before writing, so the saved file is
    /// a document the runner accepts verbatim.
    pub(crate) fn build(&self) -> Result<CoupledSearchCase, String> {
        let path = if self.centerline == CenterlineKind::Path {
            let path = self.coil_path()?;
            path.validate()
                .map_err(|e| format!("fixed_geometry.path: {e}"))?;
            Some(path)
        } else {
            None
        };
        let path3d = if self.centerline == CenterlineKind::Path3D {
            let path3d = self.coil_path3d()?;
            path3d
                .validate()
                .map_err(|e| format!("fixed_geometry.path3d: {e}"))?;
            // The schema requires it too; name the missing input with
            // its section instead of surfacing a bare contract error.
            if self.field_map.is_none() {
                return Err(
                    "path3d windings cannot use the built-in field evaluator — load a field map or generate one (filament model) in the Field map section"
                        .into(),
                );
            }
            Some(path3d)
        } else {
            None
        };
        let stations: Result<Vec<Station>, String> =
            self.stations.iter().map(StationRow::station).collect();
        let refined: Result<Vec<Station>, String> = self
            .refined_stations
            .iter()
            .map(StationRow::station)
            .collect();
        let strands = parse_u32_list(&self.strands_text, "strands_parallel")?;
        if strands.contains(&0) {
            return Err("strands_parallel: values must be at least 1".into());
        }
        let mechanical = if self.use_mechanical {
            let (hoop, area) = if self.use_hoop {
                (
                    Some(self.max_hoop_mpa * 1e6),
                    Some(self.tension_area_mm2 * 1e-6),
                )
            } else {
                (None, None)
            };
            Some(Mechanical {
                max_lorentz_load_n_per_m: self.max_lorentz_kn_per_m * 1000.0,
                max_hoop_stress_pa: hoop,
                tension_section_area_m2: area,
                max_transverse_pressure_pa: self
                    .use_pressure
                    .then_some(self.max_pressure_mpa * 1e6),
                // Schema v17 fields are authored in JSON only — the
                // workbench does not yet expose them.
                max_membrane_tension_n_per_m: None,
            })
        } else {
            None
        };
        let piece_offerings = parse_offerings(&self.piece_offerings_text)?;
        if piece_offerings.is_some() && !self.use_piece_policy {
            return Err(
                "piece offerings declared but the piece policy is off — tick 'buy conductor in discrete pieces'"
                    .into(),
            );
        }
        Ok(CoupledSearchCase {
            schema: COUPLED_SEARCH_CASE_SCHEMA_V24.into(),
            id: self.id.trim().into(),
            provenance: if self.price_basis_text.trim().is_empty() {
                self.provenance.trim().into()
            } else {
                format!(
                    "{}\nPrice basis ({}): {}",
                    self.provenance.trim(),
                    price_basis_label(self.price_source),
                    self.price_basis_text.trim()
                )
            },
            requirement: Requirement {
                bore_probe_m: self.bore_probe,
                b_target_t: self.b_target_t,
                tolerance_fraction: self.tolerance_fraction,
                good_field_region: (self.use_region && self.field_map.is_none()).then_some(
                    GoodFieldRegion {
                        half_extents_m: self.region_half,
                        points_per_axis: self.region_points,
                        max_relative_deviation: self
                            .use_uniformity_bound
                            .then_some(self.max_relative_deviation),
                        harmonics: self.use_harmonics.then_some(RegionHarmonics {
                            reference_radius_m: self.harmonic_radius_m,
                            max_order: self.harmonic_max_order,
                            theta_samples: self.harmonic_theta_samples,
                            max_normal_unit_fraction: self
                                .use_harmonic_normal_bound
                                .then_some(self.harmonic_normal_bound),
                            max_skew_unit_fraction: self
                                .use_harmonic_skew_bound
                                .then_some(self.harmonic_skew_bound),
                        }),
                    },
                ),
            },
            fixed_geometry: FixedGeometry {
                // A declared v21 search axis owns its dimension — the
                // fixed field is absent, never shadowed (schema enforces
                // one declaration site per dimension).
                straight_half_length_m: (self.centerline == CenterlineKind::Racetrack
                    && (!self.search_straight_half || self.field_map.is_some()))
                .then_some(self.straight_half_length_m),
                bend_radius_m: (self.centerline == CenterlineKind::Racetrack
                    && (!self.search_bend_radius || self.field_map.is_some()))
                .then_some(self.bend_radius_m),
                path,
                path3d,
                radial_pitch_m: self.radial_pitch_m,
                tape_width_m: self.tape_width_mm / 1000.0,
                // A non-planar pack is defined on the cylinder frame —
                // the schema only admits a radial tape normal there.
                tape_normal: if self.tape_normal_radial || self.centerline == CenterlineKind::Path3D
                {
                    TapeNormal::Radial
                } else {
                    TapeNormal::Axial
                },
                // Declared only under a field map — the map validates
                // candidates against the pack's swept extents.
                pack_radial_width_m: self.field_map.is_some().then_some(self.pack_radial_width_m),
                pack_axial_height_m: self.field_map.is_some().then_some(self.pack_axial_height_m),
            },
            choices: Choices {
                turns_along_normal: parse_u32_list(&self.turns_text, "turns_along_normal")?,
                tapes_along_width: parse_u32_list(&self.tapes_text, "tapes_along_width")?,
                strands_parallel: Some(strands),
                bend_radius_m: (self.search_bend_radius
                    && self.field_map.is_none()
                    && self.centerline == CenterlineKind::Racetrack)
                    .then(|| parse_f64_list(&self.bend_radius_text, "choices.bend_radius_m"))
                    .transpose()?,
                straight_half_length_m: (self.search_straight_half
                    && self.field_map.is_none()
                    && self.centerline == CenterlineKind::Racetrack)
                    .then(|| {
                        parse_f64_list(&self.straight_half_text, "choices.straight_half_length_m")
                    })
                    .transpose()?,
            },
            operating: SearchOperating {
                temperature_k: self.temperature_k,
                electric_field_criterion_v_per_m: self.e_criterion,
            },
            material: optcoil_model::coupled::MaterialSettings {
                dataset_id: self.dataset_id.trim().into(),
                csv_sha256: self.dataset_sha.trim().into(),
                method: optcoil_model::coupled::EXPECTED_MATERIAL_METHOD_ID.into(),
                angle_mapping: AngleMapping::Period180FieldReversal,
                mirror_policy: MirrorPolicy::MinimumOfMirrorPair,
                field_basis_mapping: FieldBasisMapping::PackFieldAsAppliedFieldSelfFieldConsistent,
                field_magnitude_policy: FieldMagnitudePolicy::TotalMagnitudeWithTransverseAngle,
                low_field_policy: LowFieldPolicy::MonotoneFieldLowerBound,
                low_field_clamp_t: self.low_field_clamp_t,
                monotonicity_tolerance: self.monotonicity_tolerance,
            },
            sampling: SearchSampling {
                stations: stations?,
                relative_turn_indices: parse_turn_indices(&self.turn_indices_text)?,
                width_points: 5,
            },
            limits: optcoil_model::coupled::Limits {
                max_along_current_field_fraction: self.max_along_fraction,
                max_self_field_ratio: self.max_self_ratio,
                interpolation_overprediction_budget: self.overprediction_budget,
                utilization_limit: self.utilization_limit,
                self_field_correction: match self.self_field_correction {
                    1 => Some("uniform_transport".into()),
                    2 => Some("critical_state_strip".into()),
                    _ => None,
                },
                along_current_model: match self.along_current_model {
                    1 => Some("transverse_bound".into()),
                    _ => None,
                },
                critical_state_layer_thickness_m: (self.self_field_correction == 2)
                    .then_some(self.critical_state_layer_um * 1e-6),
            },
            numerics: SearchNumerics {
                quadrature_orders: [self.quadrature_lo, self.quadrature_hi],
                field_scale_t: self.field_scale_t,
                max_refinement_change_fraction: self.max_refinement_change_fraction,
            },
            pruning: None,
            cost: Cost {
                price_usd_per_m: self.price_usd_per_m,
                scrap_fraction: self.scrap_fraction,
                assembly_cost_per_pancake_usd: self.assembly_usd,
                joint_cost_usd: self.joint_usd,
                piece_policy: self.use_piece_policy.then_some(PiecePolicy {
                    // A catalogue supersedes the single piece length —
                    // the schema holds them mutually exclusive.
                    piece_length_m: (piece_offerings.is_none()).then_some(self.piece_length_m),
                    boundary: if self.piece_boundary == 1 {
                        PieceBoundary::Continuous
                    } else {
                        PieceBoundary::PerModule
                    },
                    piece_unit: if self.piece_unit == 1 {
                        PieceUnit::PerStrand
                    } else {
                        PieceUnit::ConductorUnit
                    },
                    splice_cost_usd: self.splice_cost_usd,
                }),
                piece_offerings,
                price_source: price_source(self.price_source),
            },
            opex: self.use_opex.then_some(Opex {
                heat_loads_w: self
                    .heat_loads
                    .iter()
                    .map(|row| HeatLoadTerm {
                        id: row.id.trim().into(),
                        power_w: row.power_w,
                    })
                    .collect(),
                cop_fraction_of_carnot: self.cop_fraction,
                sink_temperature_k: self.sink_temperature_k,
                electricity_usd_per_kwh: self.electricity_usd_per_kwh,
                operating_hours_per_year: self.opex_hours_per_year,
                operating_years: self.opex_years,
            }),
            baseline: Baseline {
                turns_along_normal: self.baseline_turns,
                tapes_along_width: self.baseline_tapes,
                strands_parallel: Some(self.baseline_strands),
                tape_spec_ids: if self.grading.is_empty() {
                    None
                } else {
                    Some(
                        self.baseline_specs_text
                            .split(',')
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect(),
                    )
                },
                bend_radius_m: (self.search_bend_radius
                    && self.field_map.is_none()
                    && self.centerline == CenterlineKind::Racetrack)
                    .then_some(self.baseline_bend_radius),
                straight_half_length_m: (self.search_straight_half
                    && self.field_map.is_none()
                    && self.centerline == CenterlineKind::Racetrack)
                    .then_some(self.baseline_straight_half),
            },
            refinement: Some(RefinementPlan {
                pancake_counts: parse_u32_list(
                    &self.pancake_counts_text,
                    "refinement.pancake_counts",
                )?,
                turn_resolution: self.turn_resolution,
                turn_bounds: TurnBounds {
                    min: self.turn_bounds_min,
                    max: self.turn_bounds_max,
                },
                brackets: self
                    .brackets
                    .iter()
                    .map(|&[tapes, fail_turns, pass_turns]| Bracket {
                        tapes,
                        fail_turns,
                        pass_turns,
                    })
                    .collect(),
                monotonicity_check: self.monotonicity_check,
            }),
            refined_plan: RefinedPlan {
                additional_stations: refined?,
                max_sampling_shortfall_fraction: self.max_sampling_shortfall,
            },
            tape_specs: if self.tape_specs.is_empty() {
                None
            } else {
                Some(
                    self.tape_specs
                        .iter()
                        .map(|spec| -> Result<(String, TapeSpec), String> {
                            Ok((
                                spec.id.trim().to_string(),
                                TapeSpec {
                                    // Per-spec: dataset identity +
                                    // measured-floor clamp + price.
                                    // Policies inherit the case-level
                                    // material declaration.
                                    material: optcoil_model::coupled::MaterialSettings {
                                        dataset_id: spec.dataset_id.trim().into(),
                                        csv_sha256: spec.dataset_sha.trim().into(),
                                        method: optcoil_model::coupled::EXPECTED_MATERIAL_METHOD_ID
                                            .into(),
                                        angle_mapping: AngleMapping::Period180FieldReversal,
                                        mirror_policy: MirrorPolicy::MinimumOfMirrorPair,
                                        field_basis_mapping: FieldBasisMapping::PackFieldAsAppliedFieldSelfFieldConsistent,
                                        field_magnitude_policy: FieldMagnitudePolicy::TotalMagnitudeWithTransverseAngle,
                                        low_field_policy: LowFieldPolicy::MonotoneFieldLowerBound,
                                        low_field_clamp_t: spec.low_field_clamp_t,
                                        monotonicity_tolerance: self.monotonicity_tolerance,
                                    },
                                    price_usd_per_m: spec.price_usd_per_m,
                                    piece_offerings: parse_offerings(
                                        &spec.piece_offerings_text,
                                    )?,
                                    price_source: price_source(spec.price_source),
                                },
                            ))
                        })
                        .collect::<Result<_, String>>()?,
                )
            },
            grading: if self.grading.is_empty() {
                None
            } else {
                Some(Grading {
                    regions: self
                        .grading
                        .iter()
                        .map(|row| GradingRegion {
                            turn_range: [row.lo, row.hi],
                            tape_spec_choices: row
                                .specs_text
                                .split(',')
                                .map(|t| t.trim().to_string())
                                .filter(|t| !t.is_empty())
                                .collect(),
                        })
                        .collect(),
                })
            },
            field_map: self.field_map.clone().map(|map| SearchFieldMap {
                map,
                bore_field_at_reference_t: self.field_map_bore_field_t,
            }),
            manufacturing: self.use_manufacturing.then_some(Manufacturing {
                min_inner_bend_radius_m: self.min_inner_bend_m,
                tape_thickness_m: None,
                max_bend_strain: None,
            }),
            mechanical,
            thermal_margin: self.use_thermal_margin.then_some(ThermalMarginScreen {
                min_margin_k: self.min_margin_k,
            }),
            ac_loss: self.use_ac_loss.then_some(AcLossScreen {
                frequency_hz: self.ac_frequency_hz,
                transport_amplitude_fraction: self.ac_transport_fraction,
                sc_layer_thickness_m: self.ac_sc_layer_um * 1e-6,
                max_loss_w: self.ac_max_loss_w,
            }),
            quench_hotspot: self
                .use_quench_hotspot
                .then(|| {
                    Ok::<_, String>(QuenchHotspotScreen {
                        dump_time_constant_s: self.hotspot_dump_s,
                        stabilizer_area_m2: self.hotspot_stabilizer_mm2 * 1e-6,
                        quench_function_a2s_per_m4: parse_f64_pairs(
                            &self.hotspot_u_text,
                            "quench_hotspot.quench_function",
                        )?,
                        max_hotspot_k: self.hotspot_max_k,
                    })
                })
                .transpose()?,
            screening_current: self
                .use_screening_current
                .then_some(ScreeningCurrentScreen {
                    max_penetrated_width_fraction: self.screening_max_width_fraction,
                }),
            transition: self.use_transition.then_some(TransitionScreen {
                max_e_over_ec: self.transition_max_e_over_ec,
                max_voltage_v: self
                    .use_transition_voltage
                    .then_some(self.transition_max_voltage_v),
            }),
            quench_transient: self
                .use_quench_transient
                .then(|| {
                    Ok::<_, String>(QuenchTransientScreen {
                        dump_time_constant_s: self.qt_dump_s,
                        initial_temperature_k: self.use_qt_initial_t.then_some(self.qt_initial_t_k),
                        conducting_area_per_width_m: self.qt_conducting_area_per_width_m,
                        resistivity_ohm_m: parse_f64_pairs(
                            &self.qt_rho_text,
                            "quench_transient.resistivity",
                        )?,
                        heat_capacity_j_per_m3k: parse_f64_pairs(
                            &self.qt_cv_text,
                            "quench_transient.heat_capacity",
                        )?,
                        max_temperature_k: self.use_qt_max_t.then_some(self.qt_max_t_k),
                        detection_voltage_v: self
                            .use_qt_detection
                            .then_some(self.qt_detection_voltage_v),
                        max_detection_time_s: self
                            .use_qt_detection
                            .then_some(self.qt_max_detection_s),
                    })
                })
                .transpose()?,
            execution: Execution {
                max_threads: self.max_threads,
            },
        })
    }

    /// The modal window. Returns `Some((case, json))` when the user asked
    /// to save — the caller writes `json` to disk and opens it.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<(CoupledSearchCase, String)> {
        if let Some(receiver) = &self.material_pair_pending {
            match receiver.try_recv() {
                Ok(PendingMaterialPair::Loaded(label, result)) => {
                    self.material_pair_pending = None;
                    self.apply_material_pair(result, label);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.material_pair_pending = None;
                }
            }
        }
        // Async field-map work — the browser pick's dialog and the
        // filament generator both resolve between frames.
        if let Some(receiver) = &self.field_map_pending {
            match receiver.try_recv() {
                Ok(PendingFieldMap::Picked(name, result)) => {
                    self.field_map_pending = None;
                    self.apply_field_map(result, name);
                }
                Ok(PendingFieldMap::Generated(result)) => {
                    self.field_map_pending = None;
                    self.apply_generated_map(result);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Keep repainting while the generator runs.
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.field_map_pending = None;
                }
            }
        }
        let mut open = true;
        let mut save: Option<(CoupledSearchCase, String)> = None;
        let title = if self.advanced_json.is_some() {
            "Revise coupled-search case"
        } else if self.guided_step.is_some() {
            "Guided coupled-search case"
        } else {
            "New coupled-search case"
        };
        egui::Window::new(title)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .default_size([720.0, 620.0])
            .min_size([380.0, 320.0])
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                // Vertical scroll keeps the content width bounded to the
                // viewport, so wrapping rows reflow as the window
                // narrows; wide tables carry their own horizontal
                // scroll areas. The bar stays visible so cut content
                // reads as scrollable, not truncated.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        // Text and spacing scale with the window width —
                        // drag the corner bigger and the form grows
                        // with it. Gentle range: text growth also grows
                        // content height.
                        let zoom = (ui.available_width() / 720.0).clamp(0.85, 1.35);
                        if (zoom - 1.0).abs() > 0.01 {
                            let style = ui.style_mut();
                            for fid in style.text_styles.values_mut() {
                                fid.size *= zoom;
                            }
                            style.spacing.item_spacing *= zoom;
                        }
                        self.body(ui, &mut save);
                    });
            });
        if !open {
            self.error = Some("__closed__".into()); // sentinel: caller clears draft
        }
        save
    }

    /// True when the window was closed this frame (sentinel set by `show`).
    pub fn closed(&self) -> bool {
        self.error.as_deref() == Some("__closed__")
    }

    /// File dialog for a customer field map: the serialized JSON map, or
    /// a lab-frame solver export of `x y z Bx By Bz` rows (CSV, TSV or
    /// whitespace). The export's bytes are hashed into `source_sha256`
    /// — the provenance binding between case and customer artifact.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_field_map_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Load customer field map")
            .add_filter("Field map", &["json", "csv", "txt", "dat", "tsv"])
            .pick_file()
        else {
            return;
        };
        let result = if path.extension().is_some_and(|e| e == "json") {
            std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<FieldMap>(&text).map_err(|e| e.to_string()))
        } else {
            std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    let sha = format!("{:x}", Sha256::digest(&bytes));
                    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                    // Newly imported maps default to a per-ampere-turn
                    // export; the field-map section exposes the drag to
                    // declare a smeared-J reference instead.
                    optcoil_adapters::field_map::cartesian_csv_to_field_map(&text, sha, 1.0)
                })
        };
        self.apply_field_map(result, path.display().to_string());
    }

    /// Browser counterpart: the async file picker resolves to bytes on
    /// the event loop, then the same `apply_field_map` handles them.
    #[cfg(target_arch = "wasm32")]
    fn load_field_map_dialog(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .add_filter("Field map", &["json", "csv", "txt", "dat", "tsv"])
                .pick_file()
                .await
            else {
                return;
            };
            let name = handle.file_name().to_owned();
            let bytes = handle.read().await;
            let result = if name.ends_with(".json") {
                String::from_utf8(bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        serde_json::from_str::<FieldMap>(&text).map_err(|e| e.to_string())
                    })
            } else {
                let sha = format!("{:x}", Sha256::digest(&bytes));
                String::from_utf8(bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|text| {
                        optcoil_adapters::field_map::cartesian_csv_to_field_map(&text, sha, 1.0)
                    })
            };
            let _ = sender.send(PendingFieldMap::Picked(name, result));
        });
        self.field_map_pending = Some(receiver);
    }

    fn apply_field_map(&mut self, result: Result<FieldMap, String>, label: String) {
        match result {
            Ok(map @ FieldMap::CartesianBxByBz { .. }) => {
                self.field_map_label = label;
                self.field_map = Some(map);
                self.field_map_source_csv = None;
            }
            Ok(FieldMap::CylindricalBrBz { .. }) => {
                self.error = Some(
                    "cylindrical maps require a circular path winding — the builder authors racetrack cases only".into(),
                );
            }
            Err(e) => self.error = Some(format!("field map: {e}")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_material_pair_dialog(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let metadata_path = rfd::FileDialog::new()
                .set_title("Choose material metadata JSON")
                .add_filter("Material metadata", &["json"])
                .pick_file();
            let Some(metadata_path) = metadata_path else {
                return;
            };
            let csv_path = rfd::FileDialog::new()
                .set_title("Choose matching measurement CSV")
                .add_filter("Material measurements", &["csv"])
                .pick_file();
            let Some(csv_path) = csv_path else {
                return;
            };
            let label = format!(
                "{} + {}",
                metadata_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                csv_path.file_name().unwrap_or_default().to_string_lossy()
            );
            let result =
                read_material_pair_files(&metadata_path, &csv_path).and_then(|(metadata, csv)| {
                    material_bundle_from_pair(&metadata, &csv).map_err(|e| e.to_string())
                });
            let _ = sender.send(PendingMaterialPair::Loaded(label, result));
        });
        self.material_pair_pending = Some(receiver);
    }

    #[cfg(target_arch = "wasm32")]
    fn load_material_pair_dialog(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let metadata = crate::web::pick_bytes_limited(&["json"], 1024 * 1024).await;
            let Ok(Some((metadata_name, metadata_bytes))) = metadata else {
                return;
            };
            let csv = crate::web::pick_bytes_limited(&["csv"], 32 * 1024 * 1024).await;
            let Ok(Some((csv_name, csv_bytes))) = csv else {
                return;
            };
            let label = format!("{metadata_name} + {csv_name}");
            let result = String::from_utf8(metadata_bytes)
                .map_err(|e| e.to_string())
                .and_then(|metadata| {
                    material_bundle_from_pair(&metadata, &csv_bytes).map_err(|e| e.to_string())
                });
            let _ = sender.send(PendingMaterialPair::Loaded(label, result));
        });
        self.material_pair_pending = Some(receiver);
    }

    fn apply_material_pair(&mut self, result: Result<MaterialBundle, String>, label: String) {
        match result {
            Ok(bundle) => {
                self.imported_material_bundle_json = match bundle.to_json() {
                    Ok(json) => Some(json),
                    Err(error) => {
                        self.error = Some(format!("material bundle serialization: {error}"));
                        return;
                    }
                };
                self.dataset_id = bundle.dataset.metadata.id.clone();
                self.dataset_sha = bundle.dataset.metadata.csv_sha256.clone();
                self.dataset_choice = DATASET_CUSTOM;
                self.imported_material_label = Some(format!(
                    "Validated {label}: {} — {:?}",
                    bundle.dataset.metadata.id, bundle.dataset.metadata.data_class
                ));
                self.error = None;
            }
            Err(error) => self.error = Some(format!("material metadata/CSV pair: {error}")),
        }
    }

    /// Inputs for the filament generator, read off the form. Errors
    /// name the owning section.
    #[allow(clippy::type_complexity)]
    fn field_map_gen_inputs(
        &self,
    ) -> Result<
        (
            CoilPath3D,
            f64,
            f64,
            [f64; 3],
            f64,
            optcoil_search::fieldmap::MapGrid,
        ),
        String,
    > {
        let path3d = self
            .coil_path3d()
            .map_err(|e| format!("Fixed geometry: {e}"))?;
        if !(self.pack_radial_width_m > 0.0 && self.pack_axial_height_m > 0.0) {
            return Err(
                "Field map: pack extents (radial × axial) must be positive to size a map".into(),
            );
        }
        if !(self.map_gen_spacing_m > 0.0 && self.map_gen_margin_m >= 0.0) {
            return Err("Field map: grid spacing must be positive".into());
        }
        if !(self.map_gen_reference_ni.is_finite() && self.map_gen_reference_ni > 0.0) {
            return Err("Field map: reference ampere-turns must be positive".into());
        }
        Ok((
            path3d,
            self.pack_radial_width_m,
            self.pack_axial_height_m,
            self.bore_probe,
            self.map_gen_reference_ni,
            optcoil_search::fieldmap::MapGrid {
                spacing_m: self.map_gen_spacing_m,
                margin_m: self.map_gen_margin_m,
                ..Default::default()
            },
        ))
    }

    /// Generate a declared map for the authored `path3d` winding with
    /// the filament Biot-Savart model — off the event loop (a real
    /// grid is millions of field evaluations).
    #[cfg(not(target_arch = "wasm32"))]
    fn generate_field_map(&mut self, ctx: &egui::Context) {
        let inputs = self.field_map_gen_inputs();
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = inputs.and_then(|(path3d, radial, width, probe, ni, grid)| {
                optcoil_search::fieldmap::generate_path3d_field_map(
                    &path3d, radial, width, probe, ni, &grid,
                )
                .map_err(|e| e.to_string())
            });
            let _ = sender.send(PendingFieldMap::Generated(result));
            ctx.request_repaint();
        });
        self.field_map_pending = Some(receiver);
    }

    /// Browser counterpart: the same generator runs on the search
    /// worker's wasm instance (it posts `{kind:"fieldmap",…}`), so a
    /// multi-second grid does not freeze the page.
    #[cfg(target_arch = "wasm32")]
    fn generate_field_map(&mut self, ctx: &egui::Context) {
        use wasm_bindgen::JsCast as _;
        let inputs = self.field_map_gen_inputs();
        let (path3d, radial, width, probe, ni, grid) = match inputs {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        let Ok(worker) = web_sys::Worker::new_with_options("./worker.js", &options) else {
            self.error = Some("could not start the search worker".into());
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let done_worker = worker.clone();
        let onmessage =
            wasm_bindgen::closure::Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                let result = event
                    .data()
                    .as_string()
                    .ok_or_else(|| "worker replied with a non-string message".to_owned())
                    .and_then(|text| {
                        let payload: serde_json::Value =
                            serde_json::from_str(&text).map_err(|e| e.to_string())?;
                        match payload.get("status").and_then(|s| s.as_str()) {
                            Some("ok") => serde_json::from_value::<
                                optcoil_search::fieldmap::GeneratedFieldMap,
                            >(
                                payload["generated"].clone()
                            )
                            .map_err(|e| e.to_string()),
                            _ => Err(payload["error"]
                                .as_str()
                                .unwrap_or("worker error")
                                .to_owned()),
                        }
                    });
                let _ = sender.send(PendingFieldMap::Generated(result));
                ctx.request_repaint();
                done_worker.terminate(); // one-shot job — retire it
            }) as Box<dyn FnMut(_)>);
        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
        let payload = serde_json::json!({
            "kind": "fieldmap",
            "path3d": path3d,
            "radial_band_m": radial,
            "width_band_m": width,
            "bore_probe_m": probe,
            "reference_ni_a": ni,
            "grid": {
                "spacing_m": grid.spacing_m,
                "margin_m": grid.margin_m,
                "line_samples": grid.line_samples,
                "n_rho": grid.n_rho,
                "n_width": grid.n_width,
            },
        });
        let _ = worker.post_message(&wasm_bindgen::JsValue::from_str(&payload.to_string()));
        self.field_map_pending = Some(receiver);
    }

    /// Write the generated map's CSV sidecar to disk — the artifact
    /// `source_sha256` binds to.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_field_map_source(&mut self, csv: Vec<u8>) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save field-map source CSV")
            .set_file_name("field-map-source.csv")
            .save_file()
            && let Err(e) = std::fs::write(&path, csv)
        {
            self.error = Some(format!("save field-map source: {e}"));
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn save_field_map_source(&mut self, csv: Vec<u8>) {
        crate::web::download_bytes("field-map-source.csv", &csv);
    }

    fn apply_generated_map(
        &mut self,
        result: Result<optcoil_search::fieldmap::GeneratedFieldMap, String>,
    ) {
        match result {
            Ok(g) => {
                self.field_map_bore_field_t = g.bore_field_at_reference_t;
                self.field_map_source_csv = Some(g.source_csv.clone());
                self.field_map_label = "generated — filament model (optcoil-filament/v1)".into();
                self.field_map = Some(g.map);
                if !self.provenance.contains("optcoil-filament") {
                    self.provenance = format!(
                        "{}\nField map: optcoil-filament/v1 — band-smeared Biot-Savart over the declared centerline (screening-grade; not an FEM export).",
                        self.provenance.trim_end()
                    );
                }
            }
            Err(e) => self.error = Some(format!("field map generation: {e}")),
        }
    }

    /// Build + round-trip + schema-validate; on success returns the case
    /// and its canonical JSON for the caller to write.
    fn try_save(&mut self) -> Option<(CoupledSearchCase, String)> {
        if self.revision_json_invalid || self.error.is_some() {
            return None;
        }
        if self.revision_dirty {
            let mut value = self.revision_value.clone()?;
            if let Some(object) = value.as_object_mut() {
                object.insert("id".into(), self.id.clone().into());
                object.insert("provenance".into(), self.provenance.clone().into());
            }
            let patch_choices =
                |value: &mut serde_json::Value, path: &[&str], text: &str| -> Result<(), String> {
                    if json_path(value, path).is_some() {
                        patch_array(value, path, text)
                    } else {
                        Ok(())
                    }
                };
            let patched = patch_choices(
                &mut value,
                &["choices", "turns_along_normal"],
                &self.revision_turns,
            )
            .and_then(|_| {
                patch_choices(
                    &mut value,
                    &["choices", "tapes_along_width"],
                    &self.revision_tapes,
                )
            })
            .and_then(|_| {
                let path = &["choices", "strands_parallel"];
                if json_path(&value, path).is_some() || self.revision_strands_explicit {
                    patch_array(&mut value, path, &self.revision_strands)
                } else {
                    Ok(())
                }
            })
            .and_then(|_| {
                if json_path(&value, &["baseline", "tape_spec_ids"]).is_some() {
                    patch_string_array(
                        &mut value,
                        &["baseline", "tape_spec_ids"],
                        &self.revision_baseline_specs,
                    )
                } else {
                    Ok(())
                }
            });
            if let Err(error) = patched {
                self.error = Some(error);
                return None;
            }
            let json = match serde_json::to_string_pretty(&value) {
                Ok(json) => json,
                Err(error) => {
                    self.error = Some(error.to_string());
                    return None;
                }
            };
            match CoupledSearchCase::from_json(&json) {
                Ok(case) => {
                    self.revision_value = Some(value);
                    self.advanced_json = Some(json.clone());
                    self.error = None;
                    return Some((case, json));
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    return None;
                }
            }
        }
        if let Some(json) = self.advanced_json.as_ref() {
            match CoupledSearchCase::from_json(json) {
                Ok(case) => {
                    self.error = None;
                    return Some((case, json.clone()));
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return None;
                }
            }
        }
        match self.build().and_then(|case| {
            let json = serde_json::to_string_pretty(&case).map_err(|e| e.to_string())?;
            // Round-trip: save only a document the runner itself accepts.
            CoupledSearchCase::from_json(&json)
                .map(|case| (case, json))
                .map_err(|e| e.to_string())
        }) {
            Ok(done) => {
                self.error = None;
                Some(done)
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    /// The material-dataset combo + sha binding, shared between the
    /// guided material page and the full form's dataset section.
    /// Apply a registry product's declared defaults to the draft —
    /// dataset binding, tape width, price and piece catalogue. The
    /// fields stay editable afterward; the product is a starting point,
    /// not a lock.
    fn apply_product(&mut self, product: &RegistryProduct, dataset_id: &str) {
        self.dataset_id = dataset_id.to_string();
        self.dataset_sha = MaterialDataset::embedded_by_id(dataset_id)
            .map(|d| d.metadata.csv_sha256)
            .unwrap_or_default();
        self.dataset_choice = DATASET_CHOICES
            .iter()
            .position(|id| *id == dataset_id)
            .unwrap_or(DATASET_CUSTOM);
        self.tape_width_mm = product.tape_width_m * 1000.0;
        self.price_usd_per_m = product.price_usd_per_m;
        match &product.piece_offerings {
            Some(offerings) if !offerings.is_empty() => {
                self.use_piece_policy = true;
                self.piece_offerings_text = offerings
                    .iter()
                    .map(|o| format!("{}:{}", o.length_m, o.price_usd_per_m))
                    .collect::<Vec<_>>()
                    .join(", ");
            }
            _ => {}
        }
    }

    /// Stations derived from the declared racetrack envelope: three
    /// straight stations (centre, mid, near-junction) and two arc
    /// stations (apex, mid-arc). Called by presets and the derive
    /// button — the rows are ordinary stations afterwards.
    fn derived_stations(&self) -> Vec<StationRow> {
        let l = self.straight_half_length_m.max(1e-3);
        vec![
            StationRow {
                id: "straight_center".into(),
                kind: StationKind::Straight,
                param: 0.0,
            },
            StationRow {
                id: "straight_mid".into(),
                kind: StationKind::Straight,
                param: 0.5 * l,
            },
            StationRow {
                id: "straight_near_junction".into(),
                kind: StationKind::Straight,
                param: 0.85 * l,
            },
            StationRow {
                id: "arc_apex".into(),
                kind: StationKind::Arc,
                param: 0.0,
            },
            StationRow {
                id: "arc_mid".into(),
                kind: StationKind::Arc,
                param: 45.0,
            },
        ]
    }

    /// Apply a requirement preset — fills only existing draft fields so
    /// the emitted case is ordinary v24 JSON; every value stays visible.
    /// Field-map (3) writes nothing: it routes the user to the import.
    fn apply_preset(&mut self, kind: usize) {
        match kind {
            1 | 2 => {
                self.bore_probe = [0.0, 0.0, 0.0];
                self.use_region = true;
                self.region_half = [0.04, 0.04, 0.03];
                self.region_points = 3;
                self.stations = self.derived_stations();
            }
            _ => {}
        }
    }

    /// The product picker: a purchasable-conductor dropdown whose rows
    /// fill the dataset binding, tape width, price and piece catalogue.
    /// "Custom" keeps the raw dataset binding below. The picked product's
    /// notes and the bound dataset's measured-domain applicability are
    /// shown verbatim — the customer sees the evidence basis, not a
    /// marketing row.
    fn product_picker(&mut self, ui: &mut egui::Ui) {
        let registry = ProductRegistry::embedded();
        let selected = if self.product_choice == 0 {
            "custom — bind a dataset directly".to_string()
        } else {
            registry
                .products
                .get(self.product_choice - 1)
                .map(|p| format!("{} — {}", p.vendor, p.product_name))
                .unwrap_or_default()
        };
        let pick = egui::ComboBox::from_label("Conductor product")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.product_choice,
                    0,
                    "custom — bind a dataset directly",
                );
                for (i, p) in registry.products.iter().enumerate() {
                    ui.selectable_value(
                        &mut self.product_choice,
                        i + 1,
                        format!(
                            "{} — {} · ${}/m ({:?})",
                            p.vendor, p.product_name, p.price_usd_per_m, p.price_source
                        ),
                    );
                }
            });
        if pick.inner.is_some() && self.product_choice > 0 {
            self.dataset_variant_choice = 0;
            let product = registry.products[self.product_choice - 1].clone();
            self.apply_product(&product, &product.dataset_id.clone());
        }
        let Some(product) = self
            .product_choice
            .checked_sub(1)
            .and_then(|i| registry.products.get(i))
        else {
            return;
        };
        ui.colored_label(brand::MUTED, &product.notes);
        if product.material_policy.is_some() {
            ui.colored_label(
                brand::MUTED,
                "This product declares material-policy overrides — apply them in the full editor.",
            );
        }
        if !product.dataset_variants.is_empty() {
            let labels: Vec<String> = std::iter::once(product.dataset_id.clone())
                .chain(product.dataset_variants.iter().cloned())
                .collect();
            let current = self
                .dataset_variant_choice
                .min(labels.len().saturating_sub(1));
            let variant = egui::ComboBox::from_label("Evidence variant")
                .selected_text(&labels[current])
                .show_ui(ui, |ui| {
                    for (i, id) in labels.iter().enumerate() {
                        ui.selectable_value(&mut self.dataset_variant_choice, i, id);
                    }
                });
            if variant.inner.is_some() && self.dataset_variant_choice < labels.len() {
                let id = labels[self.dataset_variant_choice].clone();
                self.apply_product(product, &id);
            }
            ui.colored_label(
                brand::MUTED,
                "Variants are the same specimen family under different evidence — pick for coverage, read the data class before trusting it.",
            );
        }
        if let Ok(dataset) = MaterialDataset::embedded_by_id(&self.dataset_id) {
            let m = &dataset.metadata;
            let sel = &m.selection;
            ui.small(format!(
                "Evidence: {:?} — {:.2} mm measured bridge from a {:.0} mm tape · {:.1}–{:.1} T · {:.0}–{:.0} K",
                m.data_class,
                m.measured_bridge_width_m * 1000.0,
                m.original_tape_width_m * 1000.0,
                sel.nominal_field_t.first().copied().unwrap_or(0.0),
                sel.nominal_field_t.last().copied().unwrap_or(0.0),
                sel.nominal_temperature_k.first().copied().unwrap_or(0.0),
                sel.nominal_temperature_k.last().copied().unwrap_or(0.0),
            ));
            if (product.tape_width_m - m.original_tape_width_m).abs()
                > m.original_tape_width_m * 1e-9
            {
                ui.colored_label(
                    status_red(),
                    format!(
                        "Width note: this product is sold at {:.0} mm but the bound dataset measured a {:.0} mm tape — cross-product-width transfer is recorded as a limitation on the run.",
                        product.tape_width_m * 1000.0,
                        m.original_tape_width_m * 1000.0,
                    ),
                );
            }
        }
    }

    fn dataset_picker(&mut self, ui: &mut egui::Ui) {
        let choice = egui::ComboBox::from_label("Dataset")
            .selected_text(if self.dataset_choice < DATASET_CHOICES.len() {
                DATASET_CHOICES[self.dataset_choice]
            } else {
                "external dataset…"
            })
            .show_ui(ui, |ui| {
                for (i, id) in DATASET_CHOICES.iter().enumerate() {
                    ui.selectable_value(&mut self.dataset_choice, i, *id);
                }
                ui.selectable_value(
                    &mut self.dataset_choice,
                    DATASET_CUSTOM,
                    "external dataset…",
                );
            });
        if choice.inner.is_some() && self.dataset_choice < DATASET_CHOICES.len() {
            self.dataset_id = DATASET_CHOICES[self.dataset_choice].into();
            self.dataset_sha = MaterialDataset::embedded_by_id(&self.dataset_id)
                .map(|d| d.metadata.csv_sha256)
                .unwrap_or_default();
        }
        if self.dataset_choice == DATASET_CUSTOM {
            let pending = self.material_pair_pending.is_some();
            if ui
                .add_enabled(!pending, egui::Button::new("Load metadata + CSV pair…"))
                .clicked()
            {
                self.load_material_pair_dialog();
            }
            if let Some(label) = &self.imported_material_label {
                ui.small(label);
                ui.colored_label(
                    brand::MUTED,
                    "The saved case binds this dataset id and CSV hash. Keep the attributed metadata and CSV together; load them on the Materials page to run with this external dataset.",
                );
            }
            field(ui, "Dataset id", |ui| {
                ui.text_edit_singleline(&mut self.dataset_id)
            });
            field(ui, "CSV SHA-256", |ui| {
                ui.text_edit_singleline(&mut self.dataset_sha)
            });
            ui.colored_label(
                brand::MUTED,
                "The sha pins your measured CSV — `optcoil dataset-bundle` reports and validates it. At run time, load the bundle on the Materials page.",
            );
        } else {
            ui.small(format!("CSV SHA-256: {}", self.dataset_sha));
            if self.dataset_id == "robinson-superpower-ap-v3-modelext" {
                ui.colored_label(
                    brand::MUTED,
                    "This dataset contains modeled nodes above 8 T; those nodes are not measurements. Choose the measured low-field dataset when its measured envelope covers your operating field.",
                );
            } else if self.dataset_id == "robinson-superpower-ap-v3-lowfield" {
                ui.colored_label(
                    brand::MUTED,
                    "Measured dataset; its declared field envelope ends at 8 T, so unsupported higher-field queries remain inconclusive.",
                );
            }
        }
    }

    /// Guided authoring pages — a curated walk over the fields a
    /// customer changes, mutating the same draft the full form edits.
    /// Nothing is hidden permanently: "All fields…" opens the complete
    /// editor with every answer preserved.
    fn guided_body(
        &mut self,
        ui: &mut egui::Ui,
        save: &mut Option<(CoupledSearchCase, String)>,
        step: u8,
    ) {
        const STEPS: &[&str] = &[
            "Requirement",
            "Winding envelope",
            "Search space",
            "Material & operating point",
            "Cost basis",
            "Identity & screening",
        ];
        ui.horizontal(|ui| {
            ui.strong(format!("Guided case — {} of {}", step + 1, STEPS.len()));
            ui.colored_label(brand::MUTED, STEPS[step as usize]);
        });
        ui.colored_label(
            brand::MUTED,
            "Defaults carry the evidence-grade screening stack (good-field region, manufacturing and \
             mechanical gates, critical-state self-field bound). Change what your magnet changes.",
        );
        ui.add_space(8.0);

        match step {
            0 => {
                section(ui, "What field must the bore deliver?", |ui| {
                    let preset = egui::ComboBox::from_label("Requirement preset")
                        .selected_text(
                            [
                                "custom",
                                "solenoid bore field",
                                "insert geometry seed",
                                "field-map driven",
                            ][self.preset_choice],
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.preset_choice, 0, "custom");
                            ui.selectable_value(&mut self.preset_choice, 1, "solenoid bore field");
                            ui.selectable_value(&mut self.preset_choice, 2, "insert geometry seed");
                            ui.selectable_value(&mut self.preset_choice, 3, "field-map driven");
                        });
                    if preset.inner.is_some() {
                        self.apply_preset(self.preset_choice);
                    }
                    match self.preset_choice {
                        2 => {
                            ui.colored_label(
                                brand::MUTED,
                                "This seeds the insert's geometry and bore requirement. The case models only its declared winding geometry and field source; it does not model or add a background outsert field. Use a declared field map when the required field includes external coils.",
                            );
                        }
                        3 => {
                            ui.colored_label(
                                brand::MUTED,
                                "Import your solver's field map on the next step — it replaces the field solve at sampled points; pack extents live in the full editor.",
                            );
                        }
                        _ => {}
                    }
                    field(ui, "Bore field target (T)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.b_target_t).speed(0.1))
                    });
                    field(ui, "Tolerance fraction", |ui| {
                        ui.add(egui::DragValue::new(&mut self.tolerance_fraction).speed(1e-10))
                    });
                    grid3(ui, "Bore probe (m)", &mut self.bore_probe);
                    ui.checkbox(
                        &mut self.use_region,
                        "Good-field region — screen uniformity over a usable volume",
                    );
                    if self.use_region {
                        ui.indent("g_region", |ui| {
                            grid3(ui, "Half extents (m)", &mut self.region_half);
                            field(ui, "Points per axis (2–9)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.region_points).range(2..=9))
                            });
                        });
                    }
                });
            }
            1 => {
                section(ui, "Winding envelope (racetrack)", |ui| {
                    field(ui, "Straight half-length (m)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.straight_half_length_m).speed(0.01))
                    });
                    field(ui, "Bend radius (m)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.bend_radius_m).speed(0.005))
                    });
                    field(ui, "Radial pitch (m)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.radial_pitch_m).speed(0.001))
                    });
                    field(ui, "Tape width (mm)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.tape_width_mm).speed(0.1))
                    });
                    if ui.button("Derive stations from envelope").clicked() {
                        self.stations = self.derived_stations();
                    }
                    ui.colored_label(
                        brand::MUTED,
                        "Tape normal fixed radial; envelope-dimension search axes live in the full editor.",
                    );
                    ui.add_space(6.0);
                    ui.separator();
                    if self.field_map.is_some() {
                        ui.colored_label(
                            brand::BLUE,
                            format!("Field map loaded — {}", self.field_map_label),
                        );
                        ui.colored_label(
                            brand::MUTED,
                            "A declared map fixes the winding shape — envelope dims above are ignored, and pack extents are declared in the full editor.",
                        );
                        if ui.button("Clear map").clicked() {
                            self.field_map = None;
                            self.field_map_label.clear();
                        }
                    } else {
                        if ui
                            .button("Or import a field map (solver CSV or JSON)…")
                            .clicked()
                        {
                            self.load_field_map_dialog();
                        }
                        ui.colored_label(
                            brand::MUTED,
                            "Your own solver export replaces the engine's field solve at sampled points.",
                        );
                    }
                });
            }
            2 => {
                section(ui, "Which pack geometries to try", |ui| {
                    field(ui, "Turns along normal (list)", |ui| {
                        ui.text_edit_singleline(&mut self.turns_text)
                    });
                    field(ui, "Tapes along width (list)", |ui| {
                        ui.text_edit_singleline(&mut self.tapes_text)
                    });
                    field(ui, "Strands in parallel (list)", |ui| {
                        ui.text_edit_singleline(&mut self.strands_text)
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "Comma lists — e.g. \"12, 16, 20\". Every combination is evaluated and screened.",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Baseline");
                        ui.add(egui::DragValue::new(&mut self.baseline_turns).range(1..=10_000));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.baseline_tapes).range(1..=10_000));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.baseline_strands).range(1..=10_000));
                        ui.colored_label(brand::MUTED, "turns × tapes × strands — the reference the saving is measured against");
                    });
                    ui.add_space(6.0);
                    if ui
                        .button("Suggest a search space from the requirement…")
                        .on_hover_text(
                            "Estimates the ampere-turns the target needs — one field evaluation \
                             on the baseline geometry, or the declared map's own anchor under a \
                             field map — and proposes a turns × tapes grid. The lists stay \
                             editable; nothing runs until you save and search.",
                        )
                        .clicked()
                    {
                        match self.build() {
                            Ok(case) => match optcoil_search::sizing::sizing_estimate(&case) {
                                Ok(hint) => {
                                    self.turns_text = hint
                                        .suggested_turns
                                        .iter()
                                        .map(u32::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    self.tapes_text = hint
                                        .suggested_tapes
                                        .iter()
                                        .map(u32::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    self.strands_text = hint
                                        .suggested_strands
                                        .iter()
                                        .map(u32::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    self.sizing_hint = Some(hint);
                                    self.sizing_error = None;
                                }
                                Err(e) => {
                                    self.sizing_hint = None;
                                    self.sizing_error = Some(e);
                                }
                            },
                            Err(e) => {
                                self.sizing_hint = None;
                                self.sizing_error = Some(format!("case does not build yet: {e}"));
                            }
                        }
                    }
                    if let Some(hint) = &self.sizing_hint {
                        ui.small(format!(
                            "unit bore field {:.3e} T/A·turn → NI needed ≈ {:.0} A·turns; \
                             per-tape Ic floor {:.0}–{:.0} A over {:.2}–{:.1} T field proxies",
                            hint.unit_bore_field_t_per_at,
                            hint.ni_needed_a,
                            hint.ic_floor_pessimistic_a.unwrap_or(f64::NAN),
                            hint.ic_floor_optimistic_a.unwrap_or(f64::NAN),
                            hint.field_proxies_t.0,
                            hint.field_proxies_t.2,
                        ));
                        let mut table =
                            String::from("min turns (tapes → optimistic/mid/pessimistic): ");
                        for (t, lo, mid, hi) in &hint.min_turns_table {
                            table.push_str(&format!(
                                "{t}→{}/{}/{}   ",
                                lo.map_or("—".into(), |v| v.to_string()),
                                mid.map_or("—".into(), |v| v.to_string()),
                                hi.map_or("—".into(), |v| v.to_string()),
                            ));
                        }
                        ui.small(table);
                        for note in &hint.notes {
                            ui.colored_label(brand::MUTED, note);
                        }
                    }
                    if let Some(err) = &self.sizing_error {
                        ui.colored_label(status_red(), err);
                    }
                });
            }
            3 => {
                section(ui, "Material & operating point", |ui| {
                    self.product_picker(ui);
                    self.dataset_picker(ui);
                    field(ui, "Operating temperature (K)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.temperature_k).speed(0.5))
                    });
                });
            }
            4 => {
                section(ui, "Cost basis", |ui| {
                    field(ui, "Conductor price ($/m)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.price_usd_per_m).speed(1.0))
                    });
                    field(ui, "Scrap fraction", |ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.scrap_fraction)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                    });
                    field(ui, "Assembly cost ($)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.assembly_usd).speed(100.0))
                    });
                    field(ui, "Joint cost ($/joint)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.joint_usd).speed(10.0))
                    });
                    field(ui, "Price source", |ui| {
                        price_source_combo(ui, "guided-cost-price-src", &mut self.price_source)
                    });
                    field(ui, "Price basis detail", |ui| {
                        ui.text_edit_multiline(&mut self.price_basis_text)
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "The category is a typed cost declaration; this note is carried in case provenance.",
                    );
                    ui.checkbox(
                        &mut self.use_piece_policy,
                        "Buy conductor in discrete pieces",
                    );
                    if self.use_piece_policy {
                        field(ui, "Piece catalogue (len_m : $/m, list)", |ui| {
                            ui.text_edit_singleline(&mut self.piece_offerings_text)
                        });
                        field(ui, "Splice cost ($/splice)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.splice_cost_usd).speed(10.0))
                        });
                        ui.colored_label(
                            brand::MUTED,
                            "e.g. \"70:32.0, 300:30.0\" — the ledger picks the offering minimizing spend plus splices. Boundary/unit policy and per-spec catalogues live in the full editor.",
                        );
                    }
                });
            }
            _ => {
                section(ui, "Identity", |ui| {
                    field(ui, "Case id", |ui| ui.text_edit_singleline(&mut self.id));
                    field(ui, "Provenance", |ui| {
                        ui.text_edit_multiline(&mut self.provenance)
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "Required — the run record binds this text. Say where the requirement, geometry and dataset came from.",
                    );
                });
                section(ui, "Screening gates", |ui| {
                    ui.checkbox(&mut self.use_manufacturing, "Manufacturing limits");
                    if self.use_manufacturing {
                        ui.indent("g_mfg", |ui| {
                            field(ui, "Min inner bend radius (m)", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.min_inner_bend_m).speed(0.005),
                                )
                            });
                        });
                    }
                    ui.checkbox(&mut self.use_mechanical, "Mechanical screen");
                    if self.use_mechanical {
                        ui.indent("g_mech", |ui| {
                            field(ui, "Max Lorentz load (kN/m)", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.max_lorentz_kn_per_m)
                                        .speed(10.0),
                                )
                            });
                            ui.checkbox(&mut self.use_hoop, "Hoop-stress bound");
                            if self.use_hoop {
                                ui.indent("g_hoop", |ui| {
                                    field(ui, "Max hoop stress (MPa)", |ui| {
                                        ui.add(
                                            egui::DragValue::new(&mut self.max_hoop_mpa)
                                                .speed(10.0),
                                        )
                                    });
                                    field(ui, "Tension section area (mm²)", |ui| {
                                        ui.add(
                                            egui::DragValue::new(&mut self.tension_area_mm2)
                                                .speed(0.05),
                                        )
                                    });
                                });
                            }
                            ui.checkbox(&mut self.use_pressure, "Transverse-pressure bound");
                            if self.use_pressure {
                                ui.indent("g_press", |ui| {
                                    field(ui, "Max transverse pressure (MPa)", |ui| {
                                        ui.add(
                                            egui::DragValue::new(&mut self.max_pressure_mpa)
                                                .speed(1.0),
                                        )
                                    });
                                });
                            }
                        });
                    }
                    egui::ComboBox::from_label("Self-field correction")
                        .selected_text(
                            ["none", "uniform_transport", "critical_state_strip"]
                                [self.self_field_correction],
                        )
                        .show_ui(ui, |ui| {
                            for (i, name) in ["none", "uniform_transport", "critical_state_strip"]
                                .iter()
                                .enumerate()
                            {
                                ui.selectable_value(&mut self.self_field_correction, i, *name);
                            }
                        });
                });
                ui.add_space(6.0);
                if let Some(error) = &self.error
                    && *error != "__closed__"
                {
                    ui.colored_label(status_red(), error.to_string());
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    if ui.button("Validate & save…").clicked()
                        && let Some(done) = self.try_save()
                    {
                        *save = Some(done);
                    }
                    ui.colored_label(
                        brand::MUTED,
                        "Validated against the v24 schema — including its version gates — before writing.",
                    );
                });
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            if step > 0 && ui.button("← Back").clicked() {
                self.guided_step = Some(step - 1);
            }
            if step + 1 < STEPS.len() as u8 && ui.button("Next →").clicked() {
                self.guided_step = Some(step + 1);
            }
            if ui
                .button("All fields…")
                .on_hover_text(
                    "Open the complete editor — every schema field, with your answers kept",
                )
                .clicked()
            {
                self.guided_step = None;
            }
        });
    }

    fn body(&mut self, ui: &mut egui::Ui, save: &mut Option<(CoupledSearchCase, String)>) {
        if self.advanced_json.is_some() {
            self.revision_controls(ui);
            ui.add_space(8.0);
            ui.colored_label(
                brand::MUTED,
                "Advanced JSON remains available for every schema declaration.",
            );
            let mut accept = false;
            let mut cancel = false;
            egui::CollapsingHeader::new("Advanced JSON")
                .default_open(false)
                .show(ui, |ui| {
                    let json = self.advanced_json.as_mut().expect("checked above");
                    let text_changed = egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(json)
                                    .font(egui::TextStyle::Monospace)
                                    .code_editor()
                                    .desired_rows(20)
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        })
                        .inner;
                    if text_changed {
                        self.revision_dirty = true;
                        match CoupledSearchCase::from_json(json) {
                            Ok(case) => {
                                if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
                                    self.revision_turns =
                                        array_as_text(&value, &["choices", "turns_along_normal"]);
                                    self.revision_tapes =
                                        array_as_text(&value, &["choices", "tapes_along_width"]);
                                    self.revision_strands =
                                        array_as_text(&value, &["choices", "strands_parallel"]);
                                    self.revision_strands_explicit =
                                        json_path(&value, &["choices", "strands_parallel"])
                                            .is_some();
                                    self.revision_baseline_specs = string_array_as_text(
                                        &value,
                                        &["baseline", "tape_spec_ids"],
                                    );
                                    self.revision_value = Some(value);
                                }
                                self.id = case.id;
                                self.provenance = case.provenance;
                                self.revision_json_invalid = false;
                                self.error = None;
                            }
                            Err(error) => {
                                self.revision_json_invalid = true;
                                self.error = Some(error.to_string());
                            }
                        }
                    }
                });
            if let Some(error) = self.error.as_deref()
                && error != "__closed__"
            {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Validate and accept revision").clicked() {
                    accept = true;
                }
                if ui.button("Cancel revision").clicked() {
                    cancel = true;
                }
            });
            if accept {
                *save = self.try_save();
            }
            if cancel {
                self.error = Some("__closed__".into());
            }
            return;
        }
        if let Some(step) = self.guided_step {
            self.guided_body(ui, save, step);
        } else {
            self.form_body(ui, save);
        }
    }

    fn form_body(&mut self, ui: &mut egui::Ui, save: &mut Option<(CoupledSearchCase, String)>) {
        {
            ui.colored_label(
                brand::MUTED,
                "Emits optcoil-coupled-search/v24 — the current schema. Fields map one-to-one onto the case JSON; the saved file is validated exactly as the runner will parse it.",
            );
            ui.add_space(8.0);

            form_section(ui, "Identity", |ui| {
                field(ui, "Case id", |ui| ui.text_edit_singleline(&mut self.id));
                field(ui, "Provenance", |ui| {
                    ui.text_edit_multiline(&mut self.provenance)
                });
                ui.colored_label(
                    brand::MUTED,
                    "Required — the run record binds this text. Say where the requirement, geometry and dataset came from.",
                );
            });

            form_section(ui, "Requirement", |ui| {
                grid3(ui, "Bore probe (m)", &mut self.bore_probe);
                field(ui, "B target (T)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.b_target_t).speed(0.1))
                });
                field(ui, "Tolerance fraction", |ui| {
                    ui.add(egui::DragValue::new(&mut self.tolerance_fraction).speed(1e-10))
                });
                ui.add_enabled(
                    self.field_map.is_none(),
                    egui::Checkbox::new(&mut self.use_region, "Usable-volume region (schema v3+)"),
                );
                if self.field_map.is_some() {
                    ui.colored_label(
                        brand::MUTED,
                        "Suppressed — a declared field map forbids the usable-volume region.",
                    );
                }
                if self.use_region {
                    ui.indent("region", |ui| {
                        grid3(ui, "Half extents (m)", &mut self.region_half);
                        field(ui, "Points per axis (2–9)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.region_points).range(2..=9))
                        });
                        ui.checkbox(
                            &mut self.use_uniformity_bound,
                            "Uniformity bound (schema v22)",
                        );
                        if self.use_uniformity_bound {
                            field(ui, "Max relative deviation", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.max_relative_deviation)
                                        .speed(1e-4)
                                        .range(0.0..=1.0),
                                )
                            });
                            ui.colored_label(
                                brand::MUTED,
                                "(Bmax − Bmin) / B_center over the lattice — ppm homogeneity ÷ 1e6.",
                            );
                        }
                        ui.checkbox(
                            &mut self.use_harmonics,
                            "Multipole expansion (schema v23)",
                        );
                        if self.use_harmonics {
                            ui.indent("harmonics", |ui| {
                                field(ui, "Reference radius (m)", |ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut self.harmonic_radius_m)
                                            .speed(0.005),
                                    )
                                });
                                field(ui, "Max order (1–16)", |ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut self.harmonic_max_order)
                                            .range(1..=16),
                                    )
                                });
                                field(ui, "Theta samples", |ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut self.harmonic_theta_samples)
                                            .range(3..=512),
                                    )
                                });
                                ui.checkbox(
                                    &mut self.use_harmonic_normal_bound,
                                    "Bound normal b_n",
                                );
                                if self.use_harmonic_normal_bound {
                                    field(ui, "Max |b_n| / b0", |ui| {
                                        ui.add(
                                            egui::DragValue::new(&mut self.harmonic_normal_bound)
                                                .speed(1e-4)
                                                .range(0.0..=1.0),
                                        )
                                    });
                                }
                                ui.checkbox(&mut self.use_harmonic_skew_bound, "Bound skew a_n");
                                if self.use_harmonic_skew_bound {
                                    field(ui, "Max |a_n| / b0", |ui| {
                                        ui.add(
                                            egui::DragValue::new(&mut self.harmonic_skew_bound)
                                                .speed(1e-4)
                                                .range(0.0..=1.0),
                                        )
                                    });
                                }
                                ui.colored_label(
                                    brand::MUTED,
                                    "B_z on a midplane circle → b_n/a_n in units of the circle mean (accelerator units ÷ 1e4). Samples must exceed 2×order (Nyquist).",
                                );
                            });
                        }
                    });
                }
            });

            form_section(ui, "Field map (schema v14)", |ui| {
                if let Some(map) = &self.field_map {
                    let (grid, hull) = match map {
                        FieldMap::CartesianBxByBz {
                            x_levels_m,
                            y_levels_m,
                            z_levels_m,
                            source_sha256,
                            ..
                        } => (
                            format!(
                                "cartesian {}×{}×{}",
                                x_levels_m.len(),
                                y_levels_m.len(),
                                z_levels_m.len()
                            ),
                            format!("source sha {}", source_sha256),
                        ),
                        FieldMap::CylindricalBrBz {
                            rho_levels_m,
                            z_levels_m,
                            source_sha256,
                            ..
                        } => (
                            format!("cylindrical {}×{}", rho_levels_m.len(), z_levels_m.len()),
                            format!("source sha {}", source_sha256),
                        ),
                    };
                    ui.label(format!("{} — {}", self.field_map_label, grid));
                    ui.colored_label(brand::MUTED, hull);
                    field(ui, "Bore field at reference (T)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.field_map_bore_field_t).speed(0.01))
                    });
                    if let Some(FieldMap::CartesianBxByBz {
                        reference_ampere_turns_a,
                        ..
                    }) = self.field_map.as_mut()
                    {
                        field(ui, "Reference ampere-turns (A)", |ui| {
                            ui.add(egui::DragValue::new(reference_ampere_turns_a).speed(10.0))
                        });
                        ui.colored_label(
                            brand::MUTED,
                            "1.0 for a per-ampere-turn export; J·A_pack for a smeared-J solve.",
                        );
                    }
                    field(ui, "Pack extents — radial × axial (m)", |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.pack_radial_width_m).speed(0.005),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.pack_axial_height_m).speed(0.005),
                            );
                        })
                        .inner
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "A declared map fixes the winding shape — the usable-volume region and geometry axes are suppressed, and the map's own grid is the field resolution (refinement evidence reports zero by construction). Baseline turns×pitch and tapes×width must equal the pack extents.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Clear map").clicked() {
                            self.field_map = None;
                            self.field_map_label.clear();
                            self.field_map_source_csv = None;
                        }
                        if let Some(csv) = self.field_map_source_csv.clone()
                            && ui.button("Save source CSV…").clicked()
                        {
                            self.save_field_map_source(csv);
                        }
                    });
                    if self.field_map_source_csv.is_some() {
                        ui.colored_label(
                            brand::MUTED,
                            "Generated map — the CSV is the provenance artifact its sha256 binds; keep it next to the case.",
                        );
                    }
                } else {
                    let map_pending = self.field_map_pending.is_some();
                    ui.add_enabled_ui(!map_pending, |ui| {
                        if ui
                            .button("Load field map — JSON or solver CSV export…")
                            .clicked()
                        {
                            self.load_field_map_dialog();
                        }
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "Optional — a customer-declared field solution (their FEA export) replaces the engine's solve at sampled points. Accepts the serialized JSON map or a lab-frame export of x y z Bx By Bz rows. Cartesian maps only; cylindrical maps pair with circular path geometry this builder does not author.",
                    );
                    if self.centerline == CenterlineKind::Path3D {
                        ui.separator();
                        ui.label("Or generate one from the winding:");
                        field(ui, "Grid pitch × margin (m)", |ui| {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.map_gen_spacing_m).speed(0.001),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut self.map_gen_margin_m).speed(0.001),
                                );
                            })
                            .inner
                        });
                        field(ui, "Reference ampere-turns", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.map_gen_reference_ni).speed(100.0),
                            )
                        });
                        ui.add_enabled_ui(!map_pending, |ui| {
                            if ui.button("Generate field map — filament model").clicked() {
                                self.generate_field_map(ui.ctx());
                            }
                        });
                        ui.colored_label(
                            brand::MUTED,
                            "Evaluates the winding as band-smeared Biot-Savart filaments over its own declared (radial × binormal) band — screening-grade, not FEM. The bore anchor is computed with the same model so the NI solve stays self-consistent. Provenance is labeled optcoil-filament/v1 in the emitted case.",
                        );
                        if map_pending {
                            ui.spinner();
                        }
                    }
                }
            });

            form_section(ui, "Fixed geometry", |ui| {
                egui::ComboBox::from_label("Centerline")
                    .selected_text(match self.centerline {
                        CenterlineKind::Racetrack => "racetrack",
                        CenterlineKind::Path => "custom path — lines + arcs (schema v9)",
                        CenterlineKind::Path3D => "helix path — path3d (schema v9)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.centerline,
                            CenterlineKind::Racetrack,
                            "racetrack",
                        );
                        ui.selectable_value(
                            &mut self.centerline,
                            CenterlineKind::Path,
                            "custom path (v9)",
                        );
                        ui.selectable_value(
                            &mut self.centerline,
                            CenterlineKind::Path3D,
                            "helix path — path3d (v9, needs a field map)",
                        );
                        // Switching to path mode seeds the current
                        // racetrack as equivalent segments — the
                        // preview then opens on a valid loop.
                        if self.centerline == CenterlineKind::Path && self.path_segments.is_empty()
                        {
                            let seeded = CoilPath::racetrack(
                                self.straight_half_length_m,
                                self.bend_radius_m,
                            );
                            self.path_segments = seeded
                                .segments
                                .iter()
                                .map(|s| match *s {
                                    PathSegment::Line { length_m } => PathSegmentRow {
                                        kind: 0,
                                        length_m,
                                        radius_m: self.bend_radius_m,
                                        sweep_deg: 90.0,
                                    },
                                    PathSegment::Arc {
                                        radius_m,
                                        sweep_deg,
                                    } => PathSegmentRow {
                                        kind: 1,
                                        length_m: self.straight_half_length_m * 2.0,
                                        radius_m,
                                        sweep_deg,
                                    },
                                })
                                .collect();
                            self.path_start_x_m = seeded.start_position_m[0];
                            self.path_start_y_m = seeded.start_position_m[1];
                            self.path_start_heading_deg = seeded.start_heading_deg;
                        }
                        // Helix mode seeds a solenoid-layer winding —
                        // OC-031's shape — edit from there.
                        if self.centerline == CenterlineKind::Path3D
                            && self.path3d_segments.is_empty()
                        {
                            self.path3d_segments.push(HelixSegmentRow::solenoid());
                        }
                    });
                match self.centerline {
                    CenterlineKind::Racetrack => self.racetrack_fields(ui),
                    CenterlineKind::Path => self.path_editor(ui),
                    CenterlineKind::Path3D => self.path3d_editor(ui),
                }
                field(ui, "Radial pitch (m)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.radial_pitch_m).speed(1e-5))
                });
                field(ui, "Tape width (mm)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.tape_width_mm).speed(0.5))
                });
                ui.add_enabled_ui(self.centerline != CenterlineKind::Path3D, |ui| {
                    egui::ComboBox::from_label("Tape normal")
                        .selected_text(
                            if self.centerline == CenterlineKind::Path3D || self.tape_normal_radial
                            {
                                "radial"
                            } else {
                                "axial"
                            },
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.tape_normal_radial, true, "radial");
                            ui.selectable_value(&mut self.tape_normal_radial, false, "axial");
                        });
                });
                if self.centerline == CenterlineKind::Path3D {
                    ui.colored_label(
                        brand::MUTED,
                        "path3d windings ride the cylinder frame — tape normal is radial.",
                    );
                }
            });

            form_section(ui, "Search choices", |ui| {
                field(ui, "Turns along normal", |ui| {
                    ui.text_edit_singleline(&mut self.turns_text)
                });
                field(ui, "Tapes along width", |ui| {
                    ui.text_edit_singleline(&mut self.tapes_text)
                });
                field(ui, "Strands in parallel", |ui| {
                    ui.text_edit_singleline(&mut self.strands_text)
                });
                ui.colored_label(
                    brand::MUTED,
                    "Comma-separated lists; the grid is their product (≤ 64 candidates). The baseline's turns/tapes/strands must each appear in these lists — the baseline is itself a candidate.",
                );
            });

            form_section(ui, "Operating point", |ui| {
                field(ui, "Temperature (K)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.temperature_k).speed(0.5))
                });
                field(ui, "E-field criterion (V/m)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.e_criterion).speed(1e-5))
                });
            });

            form_section(ui, "Material dataset", |ui| {
                self.dataset_picker(ui);
                field(ui, "Low-field clamp (T)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.low_field_clamp_t).speed(0.001))
                });
                field(ui, "Monotonicity tolerance", |ui| {
                    ui.add(egui::DragValue::new(&mut self.monotonicity_tolerance).speed(1e-4))
                });
            });

            form_section(ui, "Conductor specs & grading (schema v10)", |ui| {
                ui.colored_label(
                    brand::MUTED,
                    "Purchasable conductor variants beyond the base dataset — each a dataset + price. Grading regions assign which specs may wind which turn ranges; uncovered turns take the base conductor.",
                );
                let mut remove_spec = None;
                for (i, spec) in self.tape_specs.iter_mut().enumerate() {
                    ui.indent(format!("spec-{i}"), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Spec id");
                            ui.text_edit_singleline(&mut spec.id);
                            if ui.button("×").clicked() {
                                remove_spec = Some(i);
                            }
                        });
                        let choice = egui::ComboBox::from_id_salt(format!("spec-ds-{i}"))
                            .selected_text(if spec.dataset_choice < DATASET_CHOICES.len() {
                                DATASET_CHOICES[spec.dataset_choice]
                            } else {
                                "external dataset…"
                            })
                            .show_ui(ui, |ui| {
                                for (j, id) in DATASET_CHOICES.iter().enumerate() {
                                    ui.selectable_value(&mut spec.dataset_choice, j, *id);
                                }
                                ui.selectable_value(
                                    &mut spec.dataset_choice,
                                    DATASET_CUSTOM,
                                    "external dataset…",
                                );
                            });
                        if choice.inner.is_some() && spec.dataset_choice < DATASET_CHOICES.len() {
                            spec.dataset_id = DATASET_CHOICES[spec.dataset_choice].into();
                            spec.dataset_sha = MaterialDataset::embedded_by_id(&spec.dataset_id)
                                .map(|d| d.metadata.csv_sha256)
                                .unwrap_or_default();
                        }
                        if spec.dataset_choice == DATASET_CUSTOM {
                            field(ui, "Dataset id", |ui| {
                                ui.text_edit_singleline(&mut spec.dataset_id)
                            });
                            field(ui, "CSV SHA-256", |ui| {
                                ui.text_edit_singleline(&mut spec.dataset_sha)
                            });
                        }
                        field(ui, "Price ($/m)", |ui| {
                            ui.add(egui::DragValue::new(&mut spec.price_usd_per_m).speed(1.0))
                        });
                        field(ui, "Piece catalogue (len : $/m, list)", |ui| {
                            ui.text_edit_singleline(&mut spec.piece_offerings_text)
                        });
                        field(ui, "Price source", |ui| {
                            price_source_combo(ui, &format!("spec-src-{i}"), &mut spec.price_source)
                        });
                        field(ui, "Low-field clamp (T)", |ui| {
                            ui.add(egui::DragValue::new(&mut spec.low_field_clamp_t).speed(0.001))
                        });
                    });
                }
                if let Some(i) = remove_spec {
                    self.tape_specs.remove(i);
                }
                if ui.button("+ conductor spec").clicked() {
                    self.tape_specs.push(TapeSpecRow {
                        id: "premium".into(),
                        dataset_choice: 0,
                        dataset_id: DATASET_CHOICES[0].into(),
                        dataset_sha: MaterialDataset::embedded_by_id(DATASET_CHOICES[0])
                            .map(|d| d.metadata.csv_sha256)
                            .unwrap_or_default(),
                        price_usd_per_m: 120.0,
                        low_field_clamp_t: self.low_field_clamp_t,
                        piece_offerings_text: String::new(),
                        price_source: 1,
                    });
                }
                if !self.grading.is_empty() || !self.tape_specs.is_empty() {
                    ui.add_space(4.0);
                    ui.colored_label(
                        brand::MUTED,
                        "Grading regions — turn-range fractions plus the spec ids each may take (`base` = the case-level conductor).",
                    );
                    let mut remove_region = None;
                    wide_table(ui, "grading-h", |ui| {
                        egui::Grid::new("grading").num_columns(4).show(ui, |ui| {
                            ui.label("turn lo");
                            ui.label("turn hi");
                            ui.label("spec ids");
                            ui.label("");
                            ui.end_row();
                            for (i, row) in self.grading.iter_mut().enumerate() {
                                ui.add(
                                    egui::DragValue::new(&mut row.lo)
                                        .speed(0.05)
                                        .range(0.0..=1.0),
                                );
                                ui.add(
                                    egui::DragValue::new(&mut row.hi)
                                        .speed(0.05)
                                        .range(0.0..=1.0),
                                );
                                ui.text_edit_singleline(&mut row.specs_text);
                                if ui.button("×").clicked() {
                                    remove_region = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    if let Some(i) = remove_region {
                        self.grading.remove(i);
                    }
                }
                if ui.button("+ grading region").clicked() {
                    self.grading.push(GradingRow {
                        lo: 0.0,
                        hi: 0.5,
                        specs_text: "base".into(),
                    });
                }
                if !self.grading.is_empty() {
                    field(ui, "Baseline spec per region", |ui| {
                        ui.text_edit_singleline(&mut self.baseline_specs_text)
                    });
                }
            });

            form_section(ui, "Sampling plan", |ui| {
                wide_table(ui, "stations-h", |ui| {
                    station_table(
                        ui,
                        "sampling-stations",
                        &mut self.stations,
                        self.centerline != CenterlineKind::Racetrack,
                    )
                });
                field(ui, "Turn indices", |ui| {
                    ui.text_edit_singleline(&mut self.turn_indices_text)
                });
                ui.colored_label(
                    brand::MUTED,
                    "Groups: start:offsets · frac:0..1 · end:offsets — resolved per candidate.",
                );
            });

            form_section(ui, "Screening limits & models", |ui| {
                field(ui, "Utilization limit", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.utilization_limit)
                            .speed(0.01)
                            .range(0.01..=1.0),
                    )
                });
                field(ui, "Max along-current fraction", |ui| {
                    ui.add(egui::DragValue::new(&mut self.max_along_fraction).speed(0.01))
                });
                field(ui, "Max self-field ratio", |ui| {
                    ui.add(egui::DragValue::new(&mut self.max_self_ratio).speed(0.01))
                });
                field(ui, "Over-prediction budget", |ui| {
                    ui.add(egui::DragValue::new(&mut self.overprediction_budget).speed(0.01))
                });
                egui::ComboBox::from_label("Self-field correction")
                    .selected_text(
                        ["none", "uniform_transport", "critical_state_strip"]
                            [self.self_field_correction],
                    )
                    .show_ui(ui, |ui| {
                        for (i, name) in ["none", "uniform_transport", "critical_state_strip"]
                            .iter()
                            .enumerate()
                        {
                            ui.selectable_value(&mut self.self_field_correction, i, *name);
                        }
                    });
                if self.self_field_correction == 2 {
                    field(ui, "Critical-state layer thickness (µm)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.critical_state_layer_um).speed(0.1))
                    });
                }
                egui::ComboBox::from_label("Along-current model")
                    .selected_text(["none", "transverse_bound"][self.along_current_model])
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.along_current_model, 0, "none");
                        ui.selectable_value(&mut self.along_current_model, 1, "transverse_bound");
                    });
            });

            form_section(ui, "Numerics", |ui| {
                field(ui, "Quadrature orders (coarse, final)", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.quadrature_lo).range(2..=24));
                        ui.add(egui::DragValue::new(&mut self.quadrature_hi).range(2..=24));
                    })
                    .inner
                });
                field(ui, "Field scale (T)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.field_scale_t).speed(0.05))
                });
                field(ui, "Max refinement change fraction", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.max_refinement_change_fraction).speed(1e-4),
                    )
                });
            });

            form_section(ui, "Cost basis", |ui| {
                field(ui, "Price ($/m)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.price_usd_per_m).speed(1.0))
                });
                field(ui, "Scrap fraction", |ui| {
                    ui.add(egui::DragValue::new(&mut self.scrap_fraction).speed(0.01))
                });
                field(ui, "Assembly ($/pancake)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.assembly_usd).speed(10.0))
                });
                field(ui, "Joint ($/interface)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.joint_usd).speed(10.0))
                });
                field(ui, "Price source", |ui| {
                    price_source_combo(ui, "cost-price-src", &mut self.price_source)
                });
                field(ui, "Price basis detail", |ui| {
                    ui.text_edit_multiline(&mut self.price_basis_text)
                });
                ui.colored_label(
                    brand::MUTED,
                    "This note is appended to case provenance; the price source category above remains a separate typed cost declaration. Describe a quote, publication, estimate, or synthetic basis.",
                );
            });

            form_section(ui, "Piece procurement (schema v24)", |ui| {
                ui.checkbox(
                    &mut self.use_piece_policy,
                    "Buy conductor in discrete pieces",
                );
                if self.use_piece_policy {
                    field(ui, "Piece catalogue (len_m : $/m, list)", |ui| {
                        ui.text_edit_singleline(&mut self.piece_offerings_text)
                    });
                    if self.piece_offerings_text.trim().is_empty() {
                        field(ui, "Piece length (m)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.piece_length_m).speed(1.0))
                        });
                    } else {
                        ui.colored_label(
                            brand::MUTED,
                            "e.g. \"70:32.0, 300:30.0, 600:28.0\" — the ledger picks the offering minimizing spend plus splices.",
                        );
                    }
                    field(ui, "Pieces span module boundaries", |ui| {
                        egui::ComboBox::from_id_salt("piece-boundary")
                            .selected_text(["per_module", "continuous"][self.piece_boundary])
                            .show_ui(ui, |ui| {
                                for (i, name) in ["per_module", "continuous"].iter().enumerate() {
                                    ui.selectable_value(&mut self.piece_boundary, i, *name);
                                }
                            });
                    });
                    field(ui, "A piece covers", |ui| {
                        egui::ComboBox::from_id_salt("piece-unit")
                            .selected_text(["conductor_unit", "per_strand"][self.piece_unit])
                            .show_ui(ui, |ui| {
                                for (i, name) in ["conductor_unit", "per_strand"].iter().enumerate()
                                {
                                    ui.selectable_value(&mut self.piece_unit, i, *name);
                                }
                            });
                    });
                    field(ui, "Splice cost ($/splice)", |ui| {
                        ui.add(egui::DragValue::new(&mut self.splice_cost_usd).speed(10.0))
                    });
                    ui.colored_label(
                        brand::MUTED,
                        "Splices price piece-exhaustion and spec-change joins; module-interface joints stay on 'Joint' above. Per-spec catalogues sit on each spec row.",
                    );
                } else {
                    ui.colored_label(
                        brand::MUTED,
                        "Off preserves the legacy installed×(1+scrap) ledger bit-identically.",
                    );
                }
            });

            form_section(ui, "Opex — lifecycle economics (schema v20)", |ui| {
                ui.checkbox(&mut self.use_opex, "Declare refrigeration economics");
                if self.use_opex {
                    ui.indent("opex", |ui| {
                        ui.colored_label(
                            brand::MUTED,
                            "Cold-side load is the declared sum below — your own estimate (static, conduction, AC loss, leads). Input power is Carnot-scaled at the declared efficiency fraction; lifetime opex is undiscounted.",
                        );
                        let mut remove = None;
                        wide_table(ui, "heat-loads-h", |ui| {
                            egui::Grid::new("heat-loads").num_columns(3).show(ui, |ui| {
                                ui.label("term");
                                ui.label("W @ cold stage");
                                ui.label("");
                                ui.end_row();
                                for (i, load) in self.heat_loads.iter_mut().enumerate() {
                                    ui.text_edit_singleline(&mut load.id);
                                    ui.add(egui::DragValue::new(&mut load.power_w).speed(1.0));
                                    if ui.button("×").clicked() {
                                        remove = Some(i);
                                    }
                                    ui.end_row();
                                }
                            });
                        });
                        if let Some(i) = remove {
                            self.heat_loads.remove(i);
                        }
                        if ui.button("+ heat load").clicked() {
                            self.heat_loads.push(HeatLoadRow {
                                id: "term".into(),
                                power_w: 10.0,
                            });
                        }
                        field(ui, "COP fraction of Carnot (0–1]", |ui| {
                            ui.add(egui::DragValue::new(&mut self.cop_fraction).speed(0.01))
                        });
                        field(ui, "Sink temperature (K)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.sink_temperature_k).speed(5.0))
                        });
                        field(ui, "Electricity ($/kWh)", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.electricity_usd_per_kwh).speed(0.01),
                            )
                        });
                        field(ui, "Duty hours / year", |ui| {
                            ui.add(egui::DragValue::new(&mut self.opex_hours_per_year).speed(100.0))
                        });
                        field(ui, "Operating years", |ui| {
                            ui.add(egui::DragValue::new(&mut self.opex_years).range(1..=100))
                        });
                    });
                }
            });

            form_section(ui, "Baseline", |ui| {
                field(ui, "Turns along normal", |ui| {
                    ui.add(egui::DragValue::new(&mut self.baseline_turns).speed(10))
                });
                field(ui, "Tapes along width", |ui| {
                    ui.add(egui::DragValue::new(&mut self.baseline_tapes).speed(1))
                });
                field(ui, "Strands in parallel", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.baseline_strands)
                            .speed(1)
                            .range(1..=64),
                    )
                });
            });

            form_section(ui, "Refined acceptance plan", |ui| {
                wide_table(ui, "refined-h", |ui| {
                    station_table(
                        ui,
                        "refined-stations",
                        &mut self.refined_stations,
                        self.centerline != CenterlineKind::Racetrack,
                    )
                });
                field(ui, "Max sampling shortfall", |ui| {
                    ui.add(egui::DragValue::new(&mut self.max_sampling_shortfall).speed(0.005))
                });
            });

            form_section(ui, "Refinement plan (bracketing bisection)", |ui| {
                field(ui, "Pancake counts", |ui| {
                    ui.text_edit_singleline(&mut self.pancake_counts_text)
                });
                field(ui, "Turn resolution", |ui| {
                    ui.add(egui::DragValue::new(&mut self.turn_resolution).range(1..=100))
                });
                field(ui, "Turn bounds (min, max)", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.turn_bounds_min).range(1..=10000));
                        ui.add(egui::DragValue::new(&mut self.turn_bounds_max).range(1..=10000));
                    })
                    .inner
                });
                ui.colored_label(
                    brand::MUTED,
                    "Brackets are hints — each is checked at run time: tapes · expected-FAIL turns · expected-PASS turns.",
                );
                let mut remove = None;
                wide_table(ui, "brackets-h", |ui| {
                    egui::Grid::new("brackets").num_columns(4).show(ui, |ui| {
                        ui.label("tapes");
                        ui.label("fail turns");
                        ui.label("pass turns");
                        ui.label("");
                        ui.end_row();
                        for (i, b) in self.brackets.iter_mut().enumerate() {
                            ui.add(egui::DragValue::new(&mut b[0]).range(1..=64));
                            ui.add(egui::DragValue::new(&mut b[1]).range(0..=100000));
                            ui.add(egui::DragValue::new(&mut b[2]).range(0..=100000));
                            if ui.button("×").clicked() {
                                remove = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                });
                if let Some(i) = remove {
                    self.brackets.remove(i);
                }
                if ui.button("+ bracket").clicked() {
                    self.brackets.push([4, 100, 800]);
                }
                ui.checkbox(
                    &mut self.monotonicity_check,
                    "Runtime utilization-monotonicity check",
                );
            });

            form_section(ui, "Manufacturing & mechanical screens", |ui| {
                ui.checkbox(
                    &mut self.use_manufacturing,
                    "Manufacturing limits (schema v3+)",
                );
                if self.use_manufacturing {
                    ui.indent("mfg", |ui| {
                        field(ui, "Min inner bend radius (m)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.min_inner_bend_m).speed(0.005))
                        });
                    });
                }
                ui.checkbox(&mut self.use_mechanical, "Mechanical screen (schema v4+)");
                if self.use_mechanical {
                    ui.indent("mech", |ui| {
                        field(ui, "Max Lorentz load (kN/m)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.max_lorentz_kn_per_m).speed(10.0))
                        });
                        ui.checkbox(&mut self.use_hoop, "Hoop-stress bound (schema v7+)");
                        if self.use_hoop {
                            field(ui, "Max hoop stress (MPa)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.max_hoop_mpa).speed(10.0))
                            });
                            field(ui, "Tension section per strand (mm²)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.tension_area_mm2).speed(0.05))
                            });
                        }
                        ui.checkbox(
                            &mut self.use_pressure,
                            "Transverse-pressure bound (schema v12)",
                        );
                        if self.use_pressure {
                            field(ui, "Max transverse pressure (MPa)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.max_pressure_mpa).speed(1.0))
                            });
                            ui.colored_label(
                                brand::MUTED,
                                "Declared bound on accumulated broad-face Lorentz load through the pack — a first-order screen, not a strain model. ~20–50 MPa is the literature's delamination range; a measured per-tape allowable is the dataset's job.",
                            );
                        }
                    });
                }
            });

            form_section(ui, "Declared screens (schemas v15–v18)", |ui| {
                ui.colored_label(
                    brand::MUTED,
                    "Customer-declared bounds — each declares a physical limit the screening records and gates against; INCONCLUSIVE where the declared tables don't reach the answer.",
                );
                ui.checkbox(&mut self.use_thermal_margin, "Thermal margin (v15)");
                if self.use_thermal_margin {
                    ui.indent("tm", |ui| {
                        field(ui, "Min T_cs − T_op (K)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.min_margin_k).speed(0.5))
                        });
                    });
                }
                ui.checkbox(&mut self.use_ac_loss, "AC loss (v15)");
                if self.use_ac_loss {
                    ui.indent("ac", |ui| {
                        field(ui, "Swing frequency (Hz)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.ac_frequency_hz).speed(0.01))
                        });
                        field(ui, "Transport amplitude fraction", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.ac_transport_fraction)
                                    .speed(0.01)
                                    .range(0.01..=1.0),
                            )
                        });
                        field(ui, "SC layer thickness (µm)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.ac_sc_layer_um).speed(0.1))
                        });
                        field(ui, "Loss budget (W)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.ac_max_loss_w).speed(5.0))
                        });
                    });
                }
                ui.checkbox(&mut self.use_quench_hotspot, "Quench hotspot (v15)");
                if self.use_quench_hotspot {
                    ui.indent("qh", |ui| {
                        field(ui, "Dump time constant (s)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.hotspot_dump_s).speed(0.05))
                        });
                        field(ui, "Stabilizer area (mm²)", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.hotspot_stabilizer_mm2).speed(0.5),
                            )
                        });
                        field(ui, "U(T) table — T:U A²s/m⁴", |ui| {
                            ui.text_edit_singleline(&mut self.hotspot_u_text)
                        });
                        field(ui, "Max hotspot (K)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.hotspot_max_k).speed(10.0))
                        });
                    });
                }
                ui.checkbox(&mut self.use_screening_current, "Screening current (v16)");
                if self.use_screening_current {
                    ui.indent("sc", |ui| {
                        field(ui, "Max penetrated-width fraction", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.screening_max_width_fraction)
                                    .speed(0.01)
                                    .range(0.01..=1.0),
                            )
                        });
                    });
                }
                ui.checkbox(&mut self.use_transition, "Transition depth (v16)");
                if self.use_transition {
                    ui.indent("tr", |ui| {
                        field(ui, "Max E/Ec (uⁿ)", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.transition_max_e_over_ec)
                                    .speed(0.05),
                            )
                        });
                        ui.checkbox(&mut self.use_transition_voltage, "Terminal-voltage bound");
                        if self.use_transition_voltage {
                            field(ui, "Max voltage (V)", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.transition_max_voltage_v)
                                        .speed(0.5),
                                )
                            });
                        }
                    });
                }
                ui.checkbox(&mut self.use_quench_transient, "Quench transient (v18)");
                if self.use_quench_transient {
                    ui.indent("qt", |ui| {
                        field(ui, "Dump time constant (s)", |ui| {
                            ui.add(egui::DragValue::new(&mut self.qt_dump_s).speed(0.05))
                        });
                        ui.checkbox(&mut self.use_qt_initial_t, "Initial temperature (K)");
                        if self.use_qt_initial_t {
                            field(ui, "T₀ (K)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.qt_initial_t_k).speed(1.0))
                            });
                        }
                        field(ui, "Conducting area / width (m)", |ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.qt_conducting_area_per_width_m)
                                    .speed(1e-5),
                            )
                        });
                        field(ui, "ρ(T) table — T:ρ Ω·m", |ui| {
                            ui.text_edit_singleline(&mut self.qt_rho_text)
                        });
                        field(ui, "C_v(T) table — T:C J/m³K", |ui| {
                            ui.text_edit_singleline(&mut self.qt_cv_text)
                        });
                        ui.checkbox(&mut self.use_qt_max_t, "Max-temperature bound");
                        if self.use_qt_max_t {
                            field(ui, "Max temperature (K)", |ui| {
                                ui.add(egui::DragValue::new(&mut self.qt_max_t_k).speed(10.0))
                            });
                        }
                        ui.checkbox(&mut self.use_qt_detection, "Detection bound");
                        if self.use_qt_detection {
                            field(ui, "Detection voltage (V)", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.qt_detection_voltage_v)
                                        .speed(0.1),
                                )
                            });
                            field(ui, "Max detection time (s)", |ui| {
                                ui.add(
                                    egui::DragValue::new(&mut self.qt_max_detection_s).speed(0.01),
                                )
                            });
                        }
                    });
                }
            });

            form_section(ui, "Execution", |ui| {
                field(ui, "Max threads", |ui| {
                    ui.add(egui::DragValue::new(&mut self.max_threads).range(1..=64))
                });
            });

            ui.add_space(10.0);
            if let Some(error) = &self.error
                && *error != "__closed__"
            {
                // Point at the owning section — a bare "contract §9.4"
                // citation leaves a newcomer hunting a 20-section form.
                let hint = section_hint(error)
                    .map(|s| format!("{s}: "))
                    .unwrap_or_default();
                ui.colored_label(status_red(), format!("{hint}{error}"));
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                if ui.button("Validate & save…").clicked()
                    && let Some(done) = self.try_save()
                {
                    *save = Some(done);
                }
                ui.colored_label(
                    brand::MUTED,
                    "Validated against the v24 schema — including its version gates — before writing.",
                );
            });
        }
    }

    /// The racetrack-dim fields (legacy fixed geometry) — hidden when a
    /// custom `path` is authored; v21 search axes stay available.
    fn racetrack_fields(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled(
            self.field_map.is_none(),
            egui::Checkbox::new(
                &mut self.search_straight_half,
                "Search straight half length (schema v21)",
            ),
        );
        if self.search_straight_half {
            ui.indent("straight-axis", |ui| {
                field(ui, "Choices (m)", |ui| {
                    ui.text_edit_singleline(&mut self.straight_half_text)
                });
                field(ui, "Baseline value (m)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.baseline_straight_half).speed(0.01))
                });
            });
        } else {
            field(ui, "Straight half length (m)", |ui| {
                ui.add(egui::DragValue::new(&mut self.straight_half_length_m).speed(0.01))
            });
        }
        ui.add_enabled(
            self.field_map.is_none(),
            egui::Checkbox::new(
                &mut self.search_bend_radius,
                "Search bend radius (schema v21)",
            ),
        );
        if self.search_bend_radius {
            ui.indent("bend-axis", |ui| {
                field(ui, "Choices (m)", |ui| {
                    ui.text_edit_singleline(&mut self.bend_radius_text)
                });
                field(ui, "Baseline value (m)", |ui| {
                    ui.add(egui::DragValue::new(&mut self.baseline_bend_radius).speed(0.005))
                });
            });
        } else {
            field(ui, "Bend radius (m)", |ui| {
                ui.add(egui::DragValue::new(&mut self.bend_radius_m).speed(0.005))
            });
        }
        if self.search_bend_radius || self.search_straight_half {
            ui.colored_label(
                brand::MUTED,
                "Axis baseline must appear in its choices list. The bracketing refinement runner (coupled-refine) rejects geometry-axis cases — the grid search runs them normally.",
            );
        }
    }

    /// The custom `path` editor: segment table, loop placement, and a
    /// live outline drawn by walking `pose_at`. Validation is
    /// `CoilPath::validate` itself — the same closure check the runner
    /// applies — so what passes here is what builds.
    fn path_editor(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            brand::MUTED,
            "Ordered centerline segments; the loop must close in position and tangent. Switching from racetrack seeds the equivalent segment list — edit from there. Stations on a path case address the centerline by arc length (s, m).",
        );
        let mut remove = None;
        egui::Grid::new("path-segments")
            .num_columns(5)
            .show(ui, |ui| {
                for (i, seg) in self.path_segments.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("seg-kind", i))
                        .selected_text(if seg.kind == 1 { "arc" } else { "line" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut seg.kind, 0, "line");
                            ui.selectable_value(&mut seg.kind, 1, "arc");
                        });
                    if seg.kind == 0 {
                        ui.label("length (m)");
                        ui.add(egui::DragValue::new(&mut seg.length_m).speed(0.01));
                    } else {
                        ui.label("radius (m) · sweep (°)");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut seg.radius_m).speed(0.005));
                            ui.add(
                                egui::DragValue::new(&mut seg.sweep_deg)
                                    .speed(1.0)
                                    .range(-360.0..=360.0),
                            );
                        });
                    }
                    if ui.button("×").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            self.path_segments.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.button("+ line").clicked() {
                self.path_segments.push(PathSegmentRow {
                    kind: 0,
                    length_m: 0.2,
                    radius_m: 0.09,
                    sweep_deg: 90.0,
                });
            }
            if ui.button("+ arc").clicked() {
                self.path_segments.push(PathSegmentRow {
                    kind: 1,
                    length_m: 0.2,
                    radius_m: 0.09,
                    sweep_deg: 90.0,
                });
            }
        });
        field(ui, "Start x / y (m) · heading (°)", |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.path_start_x_m).speed(0.01));
                ui.add(egui::DragValue::new(&mut self.path_start_y_m).speed(0.01));
                ui.add(egui::DragValue::new(&mut self.path_start_heading_deg).speed(1.0));
            })
            .inner
        });
        match self.coil_path() {
            Ok(path) => {
                let closed = path.validate();
                draw_path_preview(ui, &path);
                match closed {
                    Ok(()) => ui.colored_label(
                        brand::BLUE,
                        format!("closed loop — {:.3} m centerline", path.length_m()),
                    ),
                    Err(e) => ui.colored_label(status_red(), format!("open loop — {e}")),
                };
            }
            Err(e) => {
                ui.colored_label(status_red(), e);
            }
        }
    }

    /// The `path3d` helix editor: segment table (each row is one
    /// helical conductor element on a declared axis), the open/closed
    /// declaration, and an isometric preview drawn by walking
    /// `pose_at`. A declared field map is required — non-planar packs
    /// cannot use the built-in evaluator (schema rule).
    fn path3d_editor(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            brand::MUTED,
            "Helix segments join position- and tangent-continuously on their declared axes. A field map is required — the built-in evaluator is planar-only (load one in the Field map section). Stations address the centerline by arc length (s, m).",
        );
        let mut remove = None;
        egui::Grid::new("path3d-segments")
            .num_columns(3)
            .show(ui, |ui| {
                for (i, seg) in self.path3d_segments.iter_mut().enumerate() {
                    ui.push_id(i, |ui| {
                        ui.vertical(|ui| {
                            ui.label(format!("helix {}", i + 1));
                            ui.horizontal(|ui| {
                                ui.label("origin");
                                for axis in &mut seg.axis_origin_m {
                                    ui.add(egui::DragValue::new(axis).speed(0.005).max_decimals(4));
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("axis dir");
                                for axis in &mut seg.axis_dir {
                                    ui.add(egui::DragValue::new(axis).speed(0.1).max_decimals(3));
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("radius (m)");
                                ui.add(
                                    egui::DragValue::new(&mut seg.radius_m)
                                        .speed(0.001)
                                        .range(0.001..=10.0)
                                        .max_decimals(4),
                                );
                                ui.label("turns");
                                ui.add(
                                    egui::DragValue::new(&mut seg.turns)
                                        .speed(0.5)
                                        .max_decimals(3),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("rise/turn (m)");
                                ui.add(
                                    egui::DragValue::new(&mut seg.rise_per_turn_m)
                                        .speed(0.0005)
                                        .max_decimals(4),
                                );
                                ui.label("start azimuth (°)");
                                ui.add(egui::DragValue::new(&mut seg.start_azimuth_deg).speed(1.0));
                            });
                        });
                        if ui.button("×").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            self.path3d_segments.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.button("+ helix segment").clicked() {
                // Continue the winding from the last segment's end.
                self.path3d_segments.push(
                    self.path3d_segments
                        .last()
                        .map(|prev| HelixSegmentRow {
                            start_azimuth_deg: (prev.start_azimuth_deg + prev.turns * 360.0)
                                .rem_euclid(360.0),
                            axis_origin_m: [
                                prev.axis_origin_m[0],
                                prev.axis_origin_m[1],
                                prev.axis_origin_m[2] + prev.rise_per_turn_m * prev.turns,
                            ],
                            ..*prev
                        })
                        .unwrap_or_else(HelixSegmentRow::solenoid),
                );
            }
            ui.checkbox(
                &mut self.path3d_closed,
                "closed loop (rare — ends return to the start pose)",
            );
        });
        match self.coil_path3d() {
            Ok(path3d) => match path3d.validate() {
                Ok(()) => {
                    draw_path3d_preview(ui, &path3d);
                    ui.colored_label(
                        brand::BLUE,
                        format!("{:.3} m centerline", path3d.length_m()),
                    );
                }
                Err(e) => {
                    ui.colored_label(status_red(), format!("path3d: {e}"));
                }
            },
            Err(e) => {
                ui.colored_label(status_red(), e);
            }
        }
    }
}

/// Isometric projection of the 3D centerline — 30°/150° cabinet
/// projection drawn with `pose_at` samples, the same evaluation the
/// kernel walks.
fn draw_path3d_preview(ui: &mut egui::Ui, path3d: &CoilPath3D) {
    const SAMPLES: usize = 400;
    let total = path3d.length_m();
    if !(total.is_finite() && total > 0.0) {
        return;
    }
    let mut pts = Vec::with_capacity(SAMPLES + 1);
    for i in 0..=SAMPLES {
        if let Ok(pose) = path3d.pose_at(total * i as f64 / SAMPLES as f64) {
            let [x, y, z] = pose.position_m;
            // Cabinet-ish: depth axis (y) at half scale, 30° off-axis.
            let u = x + 0.5 * y * std::f64::consts::FRAC_1_SQRT_2;
            let v = z + 0.5 * y * std::f64::consts::FRAC_1_SQRT_2;
            pts.push([u, v]);
        }
    }
    if pts.len() < 2 {
        return;
    }
    egui_plot::Plot::new("path3d-preview")
        .height(180.0)
        .data_aspect(1.0)
        .show_axes([false, false])
        .allow_drag(false)
        .allow_zoom(false)
        .show(ui, |plot_ui| {
            plot_ui.line(
                egui_plot::Line::new("centerline", egui_plot::PlotPoints::from(pts))
                    .color(brand::BLUE),
            );
        });
}

/// A scale-to-fit xy outline of the path drawn with `pose_at` samples —
/// the same evaluation the kernel walks, so the preview is faithful
/// even for an open in-progress path.
fn draw_path_preview(ui: &mut egui::Ui, path: &CoilPath) {
    const SAMPLES: usize = 240;
    let total = path.length_m();
    if !(total.is_finite() && total > 0.0) {
        return;
    }
    let mut pts = Vec::with_capacity(SAMPLES + 1);
    for i in 0..=SAMPLES {
        if let Ok(pose) = path.pose_at(total * i as f64 / SAMPLES as f64) {
            pts.push([pose.position_m[0], pose.position_m[1]]);
        }
    }
    if pts.len() < 2 {
        return;
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &[x, y] in &pts {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(320.0), 150.0),
        egui::Sense::hover(),
    );
    let scale = (rect.width() as f64 * 0.8 / (x1 - x0).max(1e-9))
        .min(rect.height() as f64 * 0.8 / (y1 - y0).max(1e-9));
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let to_screen = |[x, y]: [f64; 2]| {
        rect.center() + egui::vec2(((x - cx) * scale) as f32, ((cy - y) * scale) as f32)
    };
    let painter = ui.painter_at(rect);
    painter.rect_stroke(
        rect.shrink(2.0),
        4.0,
        egui::Stroke::new(1.0, brand::MUTED.gamma_multiply(0.4)),
        egui::StrokeKind::Inside,
    );
    let polyline: Vec<egui::Pos2> = pts.iter().map(|&p| to_screen(p)).collect();
    painter.add(egui::Shape::line(
        polyline,
        egui::Stroke::new(1.8, brand::BLUE),
    ));
    // Segment-0 start marker — the loop's declared origin.
    if let Some(&first) = pts.first() {
        painter.circle_filled(to_screen(first), 3.0, status_red());
    }
}

fn status_red() -> egui::Color32 {
    egui::Color32::from_rgb(0xc0, 0x39, 0x2b)
}

/// Map a build/schema error to the form section owning the field, so
/// the save banner reads "Refined acceptance plan: <error>" instead of
/// a bare contract citation. Most-specific needles first — the error
/// text is the schema's own (e.g. `refined_plan.additional_stations…`).
fn section_hint(error: &str) -> Option<&'static str> {
    const MAP: &[(&str, &str)] = &[
        ("refined_plan", "Refined acceptance plan"),
        ("refinement", "Refinement plan (bracketing bisection)"),
        ("pruning", "Numerics"),
        ("numerics", "Numerics"),
        ("mechanical", "Manufacturing & mechanical screens"),
        ("manufacturing", "Manufacturing & mechanical screens"),
        ("thermal_margin", "Declared screens (schemas v15–v18)"),
        ("ac_loss", "Declared screens (schemas v15–v18)"),
        ("quench", "Declared screens (schemas v15–v18)"),
        ("screening_current", "Declared screens (schemas v15–v18)"),
        ("transition", "Declared screens (schemas v15–v18)"),
        ("tape_spec", "Conductor specs & grading (schema v10)"),
        ("grading", "Conductor specs & grading (schema v10)"),
        ("field_map", "Field map (schema v14)"),
        ("piece_policy", "Piece procurement (schema v24)"),
        ("opex", "Opex — lifecycle economics (schema v20)"),
        ("sampling", "Sampling plan"),
        ("station", "Sampling plan"),
        ("choices", "Search choices"),
        ("baseline", "Baseline"),
        ("path", "Fixed geometry"),
        ("fixed_geometry", "Fixed geometry"),
        ("bend_radius", "Fixed geometry"),
        ("straight_half", "Fixed geometry"),
        ("material", "Material dataset"),
        ("dataset", "Material dataset"),
        ("operating", "Operating point"),
        ("temperature", "Operating point"),
        ("cost", "Cost basis"),
        ("requirement", "Requirement"),
        ("good_field", "Requirement"),
        ("bore", "Requirement"),
        ("execution", "Execution"),
        ("threads", "Execution"),
    ];
    MAP.iter()
        .find(|(needle, _)| error.contains(needle))
        .map(|(_, section)| *section)
}

fn section(ui: &mut egui::Ui, name: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(name).strong().color(brand::BLUE));
    add(ui);
}

/// The full case form is twenty sections deep — collapsible headers
/// keep the wall readable. Core blocks (the path a new case walks:
/// identity → requirement → geometry → material → cost → baseline →
/// run) stay open; advanced blocks arrive collapsed. egui remembers
/// the open state per name, so a user's arrangement persists.
fn form_section(ui: &mut egui::Ui, name: &str, add: impl FnOnce(&mut egui::Ui)) {
    form_section_with_open(
        ui,
        name,
        [
            "Identity",
            "Requirement",
            "Fixed geometry",
            "Search choices",
            "Operating point",
            "Material dataset",
            "Cost basis",
            "Baseline",
            "Execution",
        ]
        .contains(&name),
        add,
    );
}

fn form_section_with_open(
    ui: &mut egui::Ui,
    name: &str,
    default_open: bool,
    add: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(RichText::new(name).strong().color(brand::BLUE))
        .id_salt(name)
        .default_open(default_open)
        .show(ui, |ui| {
            ui.add_space(2.0);
            add(ui);
            ui.add_space(10.0);
        });
}

fn json_path_mut<'a>(
    value: &'a mut serde_json::Value,
    path: &[&str],
) -> Option<&'a mut serde_json::Value> {
    let mut current = value;
    for part in path {
        current = if let Ok(index) = part.parse::<usize>() {
            current.as_array_mut()?.get_mut(index)?
        } else {
            current.as_object_mut()?.get_mut(*part)?
        };
    }
    Some(current)
}

fn value_float(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    path: &[&str],
    label: &str,
) -> bool {
    let Some(mut number) = json_path(value, path).and_then(serde_json::Value::as_f64) else {
        return false;
    };
    let speed = (number.abs() * 0.01).max(1e-12);
    let changed = ui
        .horizontal_wrapped(|ui| {
            ui.add_sized([170.0, 16.0], egui::Label::new(label));
            ui.add(
                egui::DragValue::new(&mut number)
                    .speed(speed)
                    .custom_formatter(|value, _| format_revision_number(value)),
            )
            .changed()
        })
        .inner;
    if changed && let Some(slot) = json_path_mut(value, path) {
        *slot = serde_json::Number::from_f64(number)
            .map(Into::into)
            .unwrap_or(serde_json::Value::Null);
    }
    changed
}

fn value_float_in(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    key: &str,
    label: &str,
) -> bool {
    let Some(mut number) = value.get(key).and_then(serde_json::Value::as_f64) else {
        return false;
    };
    let speed = (number.abs() * 0.01).max(1e-12);
    let changed = ui
        .horizontal_wrapped(|ui| {
            ui.add_sized([170.0, 16.0], egui::Label::new(label));
            ui.add(
                egui::DragValue::new(&mut number)
                    .speed(speed)
                    .custom_formatter(|value, _| format_revision_number(value)),
            )
            .changed()
        })
        .inner;
    if changed {
        value[key] = serde_json::Number::from_f64(number)
            .map(Into::into)
            .unwrap_or(serde_json::Value::Null);
    }
    changed
}

fn format_revision_number(value: f64) -> String {
    if value != 0.0 && (value.abs() < 1e-3 || value.abs() >= 1e6) {
        format!("{value:.4e}")
    } else {
        let mut text = format!("{value:.6}");
        while text.contains('.') && text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

fn value_u64(ui: &mut egui::Ui, value: &mut serde_json::Value, path: &[&str], label: &str) -> bool {
    let Some(mut number) = json_path(value, path).and_then(serde_json::Value::as_u64) else {
        return false;
    };
    let changed = ui
        .horizontal_wrapped(|ui| {
            ui.add_sized([170.0, 16.0], egui::Label::new(label));
            ui.add(egui::DragValue::new(&mut number).range(1..=u32::MAX as u64))
                .changed()
        })
        .inner;
    if changed && let Some(slot) = json_path_mut(value, path) {
        *slot = number.into();
    }
    changed
}

fn value_material_binding(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    path: &[&str],
    label: &str,
    choices: &[(String, String)],
) -> bool {
    let Some(binding) = json_path_mut(value, path).and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    let current = binding
        .get("dataset_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown dataset")
        .to_owned();
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([170.0, 16.0], egui::Label::new(label));
        egui::ComboBox::from_id_salt(("material-binding", path.join("."), label))
            .selected_text(current.clone())
            .show_ui(ui, |ui| {
                for (id, sha) in choices {
                    if ui.selectable_label(current == *id, id).clicked() {
                        binding.insert("dataset_id".into(), id.clone().into());
                        binding.insert("csv_sha256".into(), sha.clone().into());
                        changed = true;
                    }
                }
            });
        if let Some((_, sha)) = choices.iter().find(|(id, _)| id == &current) {
            ui.weak(format!("CSV {}…", sha.chars().take(12).collect::<String>()));
        }
    });
    changed
}

fn value_choice(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    path: &[&str],
    label: &str,
    choices: &[&str],
) -> bool {
    let Some(slot) = json_path_mut(value, path) else {
        return false;
    };
    let current = slot.as_str().unwrap_or("unspecified").to_owned();
    let mut selected = current.clone();
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([170.0, 16.0], egui::Label::new(label));
        egui::ComboBox::from_id_salt(("json-choice", path.join("."), label))
            .selected_text(&selected)
            .show_ui(ui, |ui| {
                for choice in choices {
                    if ui
                        .selectable_value(&mut selected, (*choice).to_owned(), *choice)
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });
    if changed {
        *slot = selected.into();
    }
    changed
}

fn value_choice_in(
    ui: &mut egui::Ui,
    value: &mut serde_json::Value,
    key: &str,
    label: &str,
    choices: &[&str],
) -> bool {
    value_choice(ui, value, &[key], label, choices)
}

fn array_as_text(value: &serde_json::Value, path: &[&str]) -> String {
    json_path(value, path)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn string_array_as_text(value: &serde_json::Value, path: &[&str]) -> String {
    json_path(value, path)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn json_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for part in path {
        current = if let Ok(index) = part.parse::<usize>() {
            current.as_array()?.get(index)?
        } else {
            current.get(*part)?
        };
    }
    Some(current)
}

fn patch_array(value: &mut serde_json::Value, path: &[&str], source: &str) -> Result<(), String> {
    let mut values = Vec::new();
    for token in source.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = token.parse::<u64>().map_err(|_| {
            format!("Expected comma-separated whole numbers; `{token}` is invalid.")
        })?;
        values.push(serde_json::Value::from(parsed));
    }
    if values.is_empty() {
        return Err("Enter at least one value in each pack choice list.".into());
    }
    let Some(target) = json_path_mut(value, path) else {
        if let Some((leaf, parent_path)) = path.split_last()
            && let Some(parent) = json_path_mut(value, parent_path)
            && let Some(object) = parent.as_object_mut()
        {
            object.insert((*leaf).to_owned(), serde_json::Value::Array(values));
            return Ok(());
        }
        return Err(format!("Source document has no {} field.", path.join(".")));
    };
    *target = serde_json::Value::Array(values);
    Ok(())
}

fn patch_string_array(
    value: &mut serde_json::Value,
    path: &[&str],
    source: &str,
) -> Result<(), String> {
    let items = source
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::Value::String(s.to_owned()))
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Err("Enter one baseline tape spec per grading region.".into());
    }
    let Some(target) = json_path_mut(value, path) else {
        return Err(format!("Source document has no {} field.", path.join(".")));
    };
    *target = serde_json::Value::Array(items);
    Ok(())
}

fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) {
    // Wrapped, not rigid: when the window is narrower than label +
    // widget, the widget drops to the next line instead of pinning the
    // window's minimum width.
    ui.horizontal_wrapped(|ui| {
        ui.add_sized([170.0, 16.0], egui::Label::new(label));
        add(ui);
    });
}

fn grid3(ui: &mut egui::Ui, label: &str, values: &mut [f64; 3]) {
    field(ui, label, |ui| {
        for v in values.iter_mut() {
            ui.add(egui::DragValue::new(v).speed(0.005));
        }
    });
}

/// The shared price-provenance combo — same labels as `price_source`.
fn price_source_combo(ui: &mut egui::Ui, salt: &str, idx: &mut usize) {
    const SOURCES: [&str; 5] = [
        "undeclared",
        "synthetic",
        "estimated",
        "published",
        "quoted",
    ];
    egui::ComboBox::from_id_salt(ui.id().with(salt))
        .selected_text(SOURCES[*idx])
        .show_ui(ui, |ui| {
            for (i, s) in SOURCES.iter().enumerate() {
                ui.selectable_value(idx, i, *s);
            }
        });
}

fn price_basis_label(index: usize) -> &'static str {
    match index {
        1 => "synthetic",
        2 => "estimated",
        3 => "published",
        4 => "quoted",
        _ => "undeclared",
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_material_pair_files(
    metadata_path: &std::path::Path,
    csv_path: &std::path::Path,
) -> Result<(String, Vec<u8>), String> {
    use std::io::Read;
    fn bounded(path: &std::path::Path, max: u64, label: &str) -> Result<Vec<u8>, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() > max {
            return Err(format!("{label} exceeds the supported size limit"));
        }
        let mut bytes = Vec::new();
        file.take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > max {
            return Err(format!("{label} exceeds the supported size limit"));
        }
        Ok(bytes)
    }
    let metadata = String::from_utf8(bounded(metadata_path, 1024 * 1024, "metadata JSON")?)
        .map_err(|e| e.to_string())?;
    let csv = bounded(csv_path, 32 * 1024 * 1024, "measurement CSV")?;
    Ok((metadata, csv))
}

/// Wrap a wide table in a horizontal scroll area so a narrow window
/// scrolls it sideways instead of pinning the window open.
fn wide_table(ui: &mut egui::Ui, salt: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::horizontal()
        .id_salt(salt)
        .auto_shrink([false, true])
        .show(ui, add);
}

fn station_table(ui: &mut egui::Ui, id: &str, stations: &mut Vec<StationRow>, path_case: bool) {
    // A row's addressing must match the declared geometry: a path case
    // rejects straight/arc stations and vice versa — flag mismatches
    // rather than letting save-time validation surprise the author.
    let mismatched = |kind: StationKind| (kind == StationKind::Path) != path_case;
    let mut remove = None;
    egui::Grid::new(id).num_columns(4).show(ui, |ui| {
        for (i, s) in stations.iter_mut().enumerate() {
            ui.add(egui::TextEdit::singleline(&mut s.id).desired_width(140.0));
            let label = match s.kind {
                StationKind::Arc => "arc",
                StationKind::Path => "path (s)",
                StationKind::Straight => "straight",
            };
            egui::ComboBox::from_id_salt((id, i))
                .selected_text(if mismatched(s.kind) {
                    RichText::new(format!("{label} ⚠")).color(status_red())
                } else {
                    RichText::new(label)
                })
                .show_ui(ui, |ui| {
                    // Straight/arc stations address racetrack geometry;
                    // a declared `path` case samples by arc length.
                    if !path_case {
                        ui.selectable_value(&mut s.kind, StationKind::Straight, "straight");
                        ui.selectable_value(&mut s.kind, StationKind::Arc, "arc");
                    }
                    ui.selectable_value(&mut s.kind, StationKind::Path, "path (s)");
                });
            ui.add(egui::DragValue::new(&mut s.param).speed(0.01).suffix(
                if s.kind == StationKind::Arc {
                    " °"
                } else {
                    " m"
                },
            ));
            if ui.button("×").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        stations.remove(i);
    }
    if ui.button("+ station").clicked() {
        stations.push(StationRow {
            id: format!("station_{}", stations.len() + 1),
            kind: if path_case {
                StationKind::Path
            } else {
                StationKind::Straight
            },
            param: 0.0,
        });
    }
    let stale = stations.iter().filter(|s| mismatched(s.kind)).count();
    if stale > 0 {
        ui.colored_label(
            status_red(),
            if path_case {
                format!(
                    "{stale} station(s) still address racetrack geometry — a path case needs path (s) stations"
                )
            } else {
                format!(
                    "{stale} path (s) station(s) on a racetrack case — switch them to straight or arc"
                )
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_model::coupled_search::{PriceSource, RelativeTurnIndex};

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_pair_reader_rejects_oversized_file_before_reading_it() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!(
            "converra-pair-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let metadata = dir.join("metadata.json");
        let csv = dir.join("measurements.csv");
        let file = std::fs::File::create(&metadata).unwrap();
        file.set_len((1024 * 1024 + 1) as u64).unwrap();
        std::fs::File::create(&csv)
            .unwrap()
            .write_all(b"temperature_k,field_t\n4.2,0.1\n")
            .unwrap();

        let error = read_material_pair_files(&metadata, &csv).unwrap_err();
        assert!(error.contains("metadata JSON exceeds"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn benchmark(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../benchmarks/coupled")
                .join(name),
        )
        .expect("benchmark fixture")
    }

    #[test]
    fn loaded_cases_round_trip_without_losing_declarations() {
        for name in ["oc-007.json", "oc-020.json", "oc-031-helix-layer.json"] {
            let source = benchmark(name);
            let original: serde_json::Value = serde_json::from_str(&source).unwrap();
            let mut draft = CaseDraft::revision(&source).expect("valid fixture");
            let (_, saved) = draft.try_save().expect("unchanged revision validates");
            let round_trip: serde_json::Value = serde_json::from_str(&saved).unwrap();
            assert_eq!(round_trip, original, "fixture changed: {name}");
        }
    }

    #[test]
    fn structured_revision_patches_only_selected_paths_for_graded_and_path3d_cases() {
        let graded_source = benchmark("oc-020.json");
        let mut graded = CaseDraft::revision(&graded_source).expect("graded case");
        graded.revision_value.as_mut().unwrap()["operating"]["temperature_k"] = 29.0.into();
        graded.revision_value.as_mut().unwrap()["cost"]["price_usd_per_m"] = 31.0.into();
        graded.revision_value.as_mut().unwrap()["baseline"]["tapes_along_width"] = 3.into();
        graded.revision_tapes = "3, 6".into();
        graded.revision_dirty = true;
        let (graded_case, graded_json) = graded.try_save().expect("valid structured revision");
        let graded_saved: serde_json::Value = serde_json::from_str(&graded_json).unwrap();
        let graded_original: serde_json::Value = serde_json::from_str(&graded_source).unwrap();
        assert_eq!(graded_case.operating.temperature_k, 29.0);
        assert_eq!(
            graded_saved["choices"]["tapes_along_width"],
            serde_json::json!([3, 6])
        );
        assert_eq!(graded_saved["grading"], graded_original["grading"]);
        assert_eq!(graded_saved["tape_specs"], graded_original["tape_specs"]);
        assert_eq!(graded_saved["material"], graded_original["material"]);

        let helix_source = benchmark("oc-031-helix-layer.json");
        let mut helix = CaseDraft::revision(&helix_source).expect("path3d case");
        if json_path(
            helix.revision_value.as_ref().unwrap(),
            &["choices", "strands_parallel"],
        )
        .is_none()
        {
            assert_eq!(helix.revision_strands, "1");
            assert!(!helix.revision_strands_explicit);
        }
        helix.revision_value.as_mut().unwrap()["operating"]["temperature_k"] = 18.0.into();
        helix.revision_dirty = true;
        let (_, helix_json) = helix.try_save().expect("valid path3d revision");
        let helix_saved: serde_json::Value = serde_json::from_str(&helix_json).unwrap();
        let helix_original: serde_json::Value = serde_json::from_str(&helix_source).unwrap();
        assert_eq!(
            helix_saved["fixed_geometry"]["path3d"],
            helix_original["fixed_geometry"]["path3d"]
        );
        assert_eq!(helix_saved["field_map"], helix_original["field_map"]);
        assert_eq!(helix_saved["grading"], helix_original["grading"]);
    }

    #[test]
    fn revision_numeric_display_keeps_tiny_values_visible() {
        assert_eq!(format_revision_number(1e-9), "1.0000e-9");
        assert_eq!(format_revision_number(0.0001), "1.0000e-4");
        assert_eq!(format_revision_number(0.01), "0.01");
        assert_eq!(format_revision_number(0.0), "0");
    }

    #[test]
    fn malformed_structured_pack_list_cannot_silently_save_stale_json() {
        let source = benchmark("oc-020.json");
        let mut draft = CaseDraft::revision(&source).unwrap();
        draft.revision_tapes = "2, invalid".into();
        draft.revision_dirty = true;
        assert!(draft.try_save().is_none());
        assert!(
            draft
                .error
                .as_deref()
                .is_some_and(|e| e.contains("whole numbers"))
        );
    }

    #[test]
    fn invalid_revision_is_rejected_and_duplicate_retains_declarations() {
        let source = benchmark("oc-031-helix-layer.json");
        let case = CoupledSearchCase::from_json(&source).expect("valid fixture");
        let mut duplicate = CaseDraft::duplicate_json(&source).expect("duplicate editor");
        let (copy, json) = duplicate.try_save().expect("duplicate validates");
        assert_ne!(copy.id, case.id);
        assert_eq!(copy.schema, case.schema);
        let source_value: serde_json::Value = serde_json::from_str(&source).unwrap();
        let copy_value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            copy_value["fixed_geometry"]["path3d"],
            source_value["fixed_geometry"]["path3d"]
        );
        assert_eq!(copy_value["field_map"], source_value["field_map"]);
        assert!(copy.provenance.contains(&case.id));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["schema"],
            case.schema
        );

        let invalid = source.replace("optcoil-coupled-search/v19", "optcoil-coupled-search/v999");
        let mut revision = CaseDraft::revision(&source).unwrap();
        revision.advanced_json = Some(invalid);
        assert!(
            revision.try_save().is_none(),
            "invalid edit must be rejected"
        );
        assert!(revision.error.is_some());
    }

    #[test]
    fn builder_accepts_only_a_validated_material_pair_binding() {
        let bundle = material_bundle_from_pair(
            optcoil_model::material::SUPERPOWER_METADATA,
            optcoil_model::material::SUPERPOWER_CSV,
        )
        .unwrap();
        let expected_id = bundle.dataset.metadata.id.clone();
        let expected_sha = bundle.dataset.metadata.csv_sha256.clone();
        let mut draft = CaseDraft::default();
        draft.apply_material_pair(Ok(bundle), "metadata.json + measurements.csv".into());
        assert_eq!(draft.dataset_choice, DATASET_CUSTOM);
        assert_eq!(draft.dataset_id, expected_id);
        assert_eq!(draft.dataset_sha, expected_sha);
        assert!(draft.imported_material_label.is_some());
    }

    #[test]
    fn recovery_round_trips_invalid_and_unsaved_author_fields() {
        let draft = CaseDraft {
            id: "unfinished draft".into(),
            price_usd_per_m: -12.5,
            advanced_json: Some("{ invalid edit".into()),
            revision_json_invalid: true,
            ..CaseDraft::default()
        };
        let encoded = draft.recovery_json().unwrap();
        let restored = CaseDraft::from_recovery_json(&encoded).unwrap();
        assert_eq!(restored.id, "unfinished draft");
        assert_eq!(restored.price_usd_per_m, -12.5);
        assert_eq!(restored.advanced_json.as_deref(), Some("{ invalid edit"));
        assert!(restored.revision_json_invalid);
        assert!(restored.sizing_hint.is_none());
        assert!(restored.field_map_pending.is_none());
    }

    #[test]
    fn imported_material_bundle_survives_draft_recovery() {
        let bundle = material_bundle_from_pair(
            optcoil_model::material::SUPERPOWER_METADATA,
            optcoil_model::material::SUPERPOWER_CSV,
        )
        .unwrap();
        let mut draft = CaseDraft::default();
        draft
            .apply_imported_material(bundle, "metadata + csv".into())
            .unwrap();
        let expected = draft.imported_bundle_json().unwrap().to_owned();
        let restored = CaseDraft::from_recovery_json(&draft.recovery_json().unwrap()).unwrap();
        assert_eq!(restored.imported_bundle_json(), Some(expected.as_str()));
    }

    #[test]
    fn recovery_rejects_malformed_retained_material_bundle() {
        let draft = CaseDraft {
            imported_material_bundle_json: Some("{ malformed bundle".into()),
            ..CaseDraft::default()
        };
        assert!(CaseDraft::from_recovery_json(&draft.recovery_json().unwrap()).is_err());
    }

    #[test]
    fn imported_material_updates_revision_binding_and_preserves_other_declarations() {
        let mut draft = CaseDraft::revision(optcoil_model::coupled_search::OC008_JSON).unwrap();
        let old_provenance = draft.provenance.clone();
        let old_cost = draft.revision_value.as_ref().unwrap()["cost"].clone();
        let bundle = material_bundle_from_pair(
            optcoil_model::material::SUPERPOWER_METADATA,
            optcoil_model::material::SUPERPOWER_CSV,
        )
        .unwrap();
        let expected_id = bundle.dataset.metadata.id.clone();
        let expected_hash = bundle.dataset.metadata.csv_sha256.clone();
        draft
            .apply_imported_material(bundle, "validated pair".into())
            .unwrap();
        let case = CoupledSearchCase::from_json(draft.advanced_json.as_deref().unwrap()).unwrap();
        assert_eq!(case.material.dataset_id, expected_id);
        assert_eq!(case.material.csv_sha256, expected_hash);
        assert_eq!(draft.revision_value.as_ref().unwrap()["cost"], old_cost);
        assert_eq!(draft.provenance, old_provenance);
        assert!(draft.revision_dirty);
    }

    #[test]
    fn imported_material_does_not_overwrite_invalid_revision_json() {
        let mut draft = CaseDraft::revision(optcoil_model::coupled_search::OC008_JSON).unwrap();
        draft.advanced_json = Some("{ invalid unsaved case".into());
        draft.revision_json_invalid = true;
        let original = draft.advanced_json.clone();
        let bundle = material_bundle_from_pair(
            optcoil_model::material::SUPERPOWER_METADATA,
            optcoil_model::material::SUPERPOWER_CSV,
        )
        .unwrap();
        assert!(
            draft
                .apply_imported_material(bundle, "validated pair".into())
                .is_err()
        );
        assert_eq!(draft.advanced_json, original);
        assert!(draft.imported_bundle_json().is_none());
    }

    /// The shipped defaults must produce a case the runner accepts —
    /// catches schema drift between the form and `from_json`'s gates.
    #[test]
    fn default_draft_builds_a_valid_v24_case() {
        let draft = CaseDraft::default();
        let case = draft.build().expect("default draft must build");
        let json = serde_json::to_string_pretty(&case).unwrap();
        let parsed = CoupledSearchCase::from_json(&json).expect("round-trip must validate");
        assert_eq!(parsed.schema, COUPLED_SEARCH_CASE_SCHEMA_V24);
        assert_eq!(parsed.choices.turns_along_normal.len(), 5);
        assert!(parsed.provenance.contains("Price basis (synthetic):"));
        assert!(
            parsed
                .provenance
                .contains("replace with a specific price source")
        );
        assert!(parsed.requirement.good_field_region.is_some());
        let mechanical = parsed.mechanical.as_ref().expect("mechanical block");
        assert!(mechanical.max_hoop_stress_pa.is_some());
        // The pressure bound is opt-in; defaults leave it undeclared.
        assert_eq!(mechanical.max_transverse_pressure_pa, None);
    }

    #[test]
    fn piece_policy_emits_a_v24_catalogue() {
        let parsed = build_parsed(CaseDraft {
            use_piece_policy: true,
            piece_offerings_text: "70:32.0, 300:30.0".into(),
            piece_unit: 1,
            splice_cost_usd: 500.0,
            ..Default::default()
        });
        let policy = parsed.cost.piece_policy.expect("piece policy");
        assert_eq!(policy.piece_unit, PieceUnit::PerStrand);
        assert_eq!(policy.boundary, PieceBoundary::PerModule);
        // A declared catalogue supersedes the single piece length.
        assert_eq!(policy.piece_length_m, None);
        let offerings = parsed.cost.piece_offerings.expect("base catalogue");
        assert_eq!(offerings.len(), 2);
        assert_eq!(offerings[0].length_m, 70.0);
        assert_eq!(parsed.cost.price_source, Some(PriceSource::Synthetic));
    }

    /// A closed custom path must emit `fixed_geometry.path` (dims
    /// absent), pass the engine's own closure check, and survive the
    /// v24 round-trip the runner applies.
    #[test]
    fn path_geometry_builds_a_valid_case() {
        let seeded = CoilPath::racetrack(0.15, 0.09);
        let draft = CaseDraft {
            centerline: CenterlineKind::Path,
            path_segments: seeded
                .segments
                .iter()
                .map(|s| match *s {
                    PathSegment::Line { length_m } => PathSegmentRow {
                        kind: 0,
                        length_m,
                        radius_m: 0.09,
                        sweep_deg: 90.0,
                    },
                    PathSegment::Arc {
                        radius_m,
                        sweep_deg,
                    } => PathSegmentRow {
                        kind: 1,
                        length_m: 0.3,
                        radius_m,
                        sweep_deg,
                    },
                })
                .collect(),
            path_start_x_m: seeded.start_position_m[0],
            path_start_y_m: seeded.start_position_m[1],
            path_start_heading_deg: seeded.start_heading_deg,
            // Stations on a path case address the centerline by s; the
            // §9.4 refined plan needs 6 additional ones.
            stations: vec![
                StationRow {
                    id: "s0".into(),
                    kind: StationKind::Path,
                    param: 0.0,
                },
                StationRow {
                    id: "s1".into(),
                    kind: StationKind::Path,
                    param: 0.5,
                },
            ],
            refined_stations: (1..=6)
                .map(|i| StationRow {
                    id: format!("refined_s{i}"),
                    kind: StationKind::Path,
                    param: i as f64 * 0.15,
                })
                .collect(),
            ..Default::default()
        };
        let case = draft.build().expect("closed path must build");
        let geometry = &case.fixed_geometry;
        assert!(geometry.path.is_some());
        assert_eq!(geometry.straight_half_length_m, None);
        assert_eq!(geometry.bend_radius_m, None);
        let json = serde_json::to_string_pretty(&case).unwrap();
        CoupledSearchCase::from_json(&json).expect("path case must validate as v24");
    }

    /// A path that does not return to its start must fail `build` —
    /// `CoilPath::validate` is the engine's own closure check.
    #[test]
    fn open_path_is_rejected() {
        let draft = CaseDraft {
            centerline: CenterlineKind::Path,
            path_segments: vec![PathSegmentRow {
                kind: 0,
                length_m: 0.5,
                radius_m: 0.09,
                sweep_deg: 90.0,
            }],
            ..Default::default()
        };
        let err = draft.build().expect_err("open path must not build");
        assert!(err.contains("path"), "unexpected error: {err}");
    }

    /// A complete 2×2×2 cartesian map — the smallest grid the schema's
    /// product-grid check accepts. Field values are a uniform 1 T Bz.
    fn fixture_field_map() -> FieldMap {
        let mut entries = Vec::new();
        for x_index in 0..2 {
            for y_index in 0..2 {
                for z_index in 0..2 {
                    entries.push(optcoil_model::coupled::FieldMapCartesianEntry {
                        x_index,
                        y_index,
                        z_index,
                        bx_t: 0.0,
                        by_t: 0.0,
                        bz_t: 1.0,
                    });
                }
            }
        }
        FieldMap::CartesianBxByBz {
            source_sha256: "0".repeat(64),
            reference_ampere_turns_a: 1.0,
            // Levels sized to contain a solenoid-layer sweep (r≈0.03,
            // z∈[0,0.2]) — the hull extends a half-spacing past each level.
            x_levels_m: vec![-0.05, 0.05],
            y_levels_m: vec![-0.05, 0.05],
            z_levels_m: vec![0.05, 0.20],
            entries,
        }
    }

    /// A helix winding emits `fixed_geometry.path3d` (racetrack dims and
    /// `path` absent), forces a radial tape normal even when the form
    /// still says axial, and round-trips as v24.
    #[test]
    fn path3d_geometry_builds_a_valid_case() {
        let draft = CaseDraft {
            centerline: CenterlineKind::Path3D,
            tape_normal_radial: false,
            path3d_segments: vec![HelixSegmentRow::solenoid()],
            // Under a declared map the pack extents must equal the
            // baseline's swept dimensions — OC-031's invariants.
            turns_text: "4, 8".into(),
            tapes_text: "1, 2".into(),
            baseline_turns: 8,
            baseline_tapes: 1,
            radial_pitch_m: 0.0005,
            tape_width_mm: 8.0,
            pack_radial_width_m: 0.004,
            pack_axial_height_m: 0.008,
            field_map: Some(fixture_field_map()),
            field_map_label: "map.json".into(),
            stations: vec![
                StationRow {
                    id: "s0".into(),
                    kind: StationKind::Path,
                    param: 0.0,
                },
                StationRow {
                    id: "s1".into(),
                    kind: StationKind::Path,
                    param: 0.5,
                },
            ],
            refined_stations: (1..=6)
                .map(|i| StationRow {
                    id: format!("refined_s{i}"),
                    kind: StationKind::Path,
                    param: i as f64 * 0.15,
                })
                .collect(),
            ..Default::default()
        };
        let case = draft.build().expect("helix draft must build");
        let path3d = case.fixed_geometry.path3d.as_ref().expect("path3d emitted");
        assert_eq!(path3d.segments.len(), 1);
        assert_eq!(case.fixed_geometry.tape_normal, TapeNormal::Radial);
        assert!(case.fixed_geometry.path.is_none());
        assert!(case.fixed_geometry.straight_half_length_m.is_none());
        let json = serde_json::to_string_pretty(&case).unwrap();
        CoupledSearchCase::from_json(&json).expect("path3d case must validate as v24");
    }

    /// Non-planar packs cannot use the built-in evaluator — build must
    /// name the missing field map instead of surfacing a schema error.
    #[test]
    fn path3d_without_field_map_is_rejected() {
        let draft = CaseDraft {
            centerline: CenterlineKind::Path3D,
            path3d_segments: vec![HelixSegmentRow::solenoid()],
            ..Default::default()
        };
        let err = draft
            .build()
            .expect_err("helix without a field map must not build");
        assert!(err.contains("field map"), "unexpected error: {err}");
    }

    /// Two helix segments whose start doesn't continue the previous
    /// end must fail `CoilPath3D::validate` inside `build`.
    #[test]
    fn discontinuous_path3d_is_rejected() {
        let mut second = HelixSegmentRow::solenoid();
        // Jump the axis origin — the segment's start no longer lands on
        // the previous segment's end pose.
        second.axis_origin_m[2] = 1.0;
        let draft = CaseDraft {
            centerline: CenterlineKind::Path3D,
            path3d_segments: vec![HelixSegmentRow::solenoid(), second],
            field_map: Some(fixture_field_map()),
            ..Default::default()
        };
        let err = draft
            .build()
            .expect_err("discontinuous helix path must not build");
        assert!(err.contains("path3d"), "unexpected error: {err}");
    }

    /// A product pick must land on the draft's actual fields — dataset
    /// binding, tape width and price — and the resulting case must still
    /// validate as v24.
    #[test]
    fn product_pick_fills_the_case_fields() {
        let registry = ProductRegistry::embedded();
        let product = registry.product("theva-proline-class-6mm").unwrap();
        let mut draft = CaseDraft::default();
        draft.apply_product(product, &product.dataset_id.clone());
        assert_eq!(draft.dataset_id, "robinson-theva-ap-v2");
        assert_eq!(draft.dataset_choice, 4);
        assert_eq!(draft.tape_width_mm, 6.0);
        assert_eq!(draft.price_usd_per_m, 38.0);
        let case = draft.build().expect("product-filled draft builds");
        let parsed = CoupledSearchCase::from_json(&serde_json::to_string(&case).unwrap()).unwrap();
        assert_eq!(parsed.material.dataset_id, "robinson-theva-ap-v2");
        assert_eq!(parsed.fixed_geometry.tape_width_m, 0.006);
        // A variant pick rebinds the dataset, not just the label.
        let sp = registry.product("superpower-scs-class-12mm").unwrap();
        let mut draft = CaseDraft::default();
        draft.apply_product(sp, "robinson-superpower-ap-v3-modelext");
        assert_eq!(draft.dataset_id, "robinson-superpower-ap-v3-modelext");
        assert_eq!(draft.dataset_choice, 2);
        assert_eq!(draft.tape_width_mm, 12.0);
    }

    /// A solenoid preset must only fill existing draft fields — the
    /// emitted case validates as ordinary v24 and the stations it
    /// derives come from the declared envelope.
    #[test]
    fn solenoid_preset_derives_a_valid_case() {
        let mut draft = CaseDraft {
            straight_half_length_m: 0.5,
            ..Default::default()
        };
        draft.apply_preset(1);
        assert_eq!(draft.bore_probe, [0.0, 0.0, 0.0]);
        assert!(draft.use_region);
        assert_eq!(draft.stations.len(), 5);
        assert!(draft.stations.iter().any(|s| s.kind == StationKind::Arc));
        // Station params derive from the declared envelope.
        let mid = draft
            .stations
            .iter()
            .find(|s| s.id == "straight_mid")
            .unwrap();
        assert!((mid.param - 0.25).abs() < 1e-9);
        let case = draft.build().expect("preset draft must build");
        CoupledSearchCase::from_json(&serde_json::to_string(&case).unwrap())
            .expect("preset case must validate");
    }

    /// The insert preset seeds supported geometry and a requirement; it
    /// makes no outsert field claim.
    #[test]
    fn insert_preset_only_seeds_supported_geometry() {
        let mut draft = CaseDraft {
            b_target_t: 3.0,
            ..Default::default()
        };
        draft.apply_preset(2);
        let case = draft.build().expect("insert preset must build");
        assert_eq!(case.requirement.b_target_t, 3.0);
        assert!(case.fixed_geometry.path.is_none());
        CoupledSearchCase::from_json(&serde_json::to_string(&case).unwrap())
            .expect("insert preset case must validate");
    }

    /// The milestone's done-when: the guided wizard alone reproduces a
    /// benchmark case. Drive `CaseDraft` exactly as the wizard flow does
    /// — solenoid preset, product pick, then the field values — and the
    /// emitted case must be semantically identical to oc-029-vendor:
    /// same schema, every gate-relevant field equal; only `provenance`
    /// (authorship metadata, not search semantics) may differ. Equality
    /// of the serialized case implies equality of the record the engine
    /// would produce — the benchmark's own record is the optimum proof.
    #[test]
    fn wizard_reproduces_the_oc029_vendor_case() {
        let mut draft = CaseDraft::guided();
        draft.apply_preset(1); // solenoid bore field
        let registry = ProductRegistry::embedded();
        let product = registry.product("superpower-scs-class-12mm").unwrap();
        draft.apply_product(product, &product.dataset_id.clone());
        // The product's piece catalogue would add a piece policy the
        // benchmark doesn't declare — a real user targeting this case
        // clears it; the price provenance stays honest at undeclared.
        draft.piece_offerings_text.clear();
        draft.price_source = 0;
        draft.price_usd_per_m = 30.0;
        // The guided defaults are *stricter* than this benchmark — they
        // declare the mechanical/manufacturing bounds and self-field
        // corrections. A user targeting the benchmark's plain screening
        // switches them off; all are visible toggles in the wizard.
        draft.use_mechanical = false;
        draft.use_manufacturing = false;
        draft.self_field_correction = 0;
        draft.along_current_model = 0;

        draft.id = "oc-029-vendor".into();
        draft.b_target_t = 0.9;
        draft.tolerance_fraction = 0.02;
        draft.region_half = [0.05, 0.05, 0.05];
        draft.region_points = 3;
        draft.use_uniformity_bound = false;
        draft.straight_half_length_m = 0.3;
        draft.bend_radius_m = 0.2;
        draft.radial_pitch_m = 0.0001;
        draft.tape_normal_radial = true;
        draft.stations = vec![
            StationRow {
                id: "s15".into(),
                kind: StationKind::Straight,
                param: 0.15,
            },
            StationRow {
                id: "a45".into(),
                kind: StationKind::Arc,
                param: 45.0,
            },
            StationRow {
                id: "a90".into(),
                kind: StationKind::Arc,
                param: 90.0,
            },
            StationRow {
                id: "a0".into(),
                kind: StationKind::Arc,
                param: 0.0,
            },
        ];
        draft.turn_indices_text = "start:1 frac:0.125,0.5,0.85 end:0".into();
        draft.turns_text = "200, 240, 300".into();
        draft.tapes_text = "2, 3".into();
        draft.strands_text = "1".into();
        draft.temperature_k = 25.0;
        draft.e_criterion = 0.0001;
        draft.low_field_clamp_t = 1.001;
        draft.monotonicity_tolerance = 0.001;
        draft.max_along_fraction = 0.2;
        draft.max_self_ratio = 0.1;
        draft.overprediction_budget = 0.1;
        draft.utilization_limit = 0.8;
        draft.quadrature_lo = 10;
        draft.quadrature_hi = 14;
        draft.field_scale_t = 1.0;
        draft.max_refinement_change_fraction = 0.0025;
        draft.scrap_fraction = 0.1;
        draft.assembly_usd = 500.0;
        draft.joint_usd = 200.0;
        draft.baseline_turns = 240;
        draft.baseline_tapes = 3;
        draft.baseline_strands = 1;
        draft.pancake_counts_text = "2, 3".into();
        draft.turn_resolution = 10;
        draft.turn_bounds_min = 1;
        draft.turn_bounds_max = 400;
        draft.brackets = vec![[2, 100, 300], [3, 100, 300]];
        draft.monotonicity_check = true;
        draft.refined_stations = vec![
            StationRow {
                id: "arc_15".into(),
                kind: StationKind::Arc,
                param: 15.0,
            },
            StationRow {
                id: "arc_30".into(),
                kind: StationKind::Arc,
                param: 30.0,
            },
            StationRow {
                id: "arc_60".into(),
                kind: StationKind::Arc,
                param: 60.0,
            },
            StationRow {
                id: "arc_75".into(),
                kind: StationKind::Arc,
                param: 75.0,
            },
            StationRow {
                id: "straight_015".into(),
                kind: StationKind::Straight,
                param: 0.15,
            },
            StationRow {
                id: "straight_025".into(),
                kind: StationKind::Straight,
                param: 0.25,
            },
        ];
        draft.max_sampling_shortfall = 0.02;
        draft.max_threads = 8;

        let case = draft.build().expect("wizard draft must build");
        let mut emitted = serde_json::to_value(&case).unwrap();
        let mut expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string("../../benchmarks/coupled/oc-029-vendor.json").unwrap(),
        )
        .unwrap();
        // provenance is authorship metadata, not search semantics, and
        // `None` fields serialize as explicit nulls where the benchmark
        // omits the key — serde treats both as absent.
        emitted["provenance"] = expected["provenance"].clone();
        strip_nulls(&mut emitted);
        strip_nulls(&mut expected);
        assert_eq!(emitted, expected, "wizard-emitted case diverges");
    }

    /// Recursively drop explicit `null` values — serde reads a missing
    /// key and an explicit null identically for `Option` fields, so
    /// both spellings count as absent for equivalence.
    fn strip_nulls(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                m.retain(|_, val| !val.is_null());
                for val in m.values_mut() {
                    strip_nulls(val);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(strip_nulls),
            _ => {}
        }
    }

    #[test]
    fn guided_draft_builds_the_same_valid_case() {
        let draft = CaseDraft::guided();
        assert_eq!(draft.guided_step, Some(0), "guided starts at step 0");
        // The wizard mutates the same fields — a fresh guided draft must
        // produce exactly the default case.
        let case = draft.build().expect("guided draft must build");
        let json = serde_json::to_string_pretty(&case).unwrap();
        CoupledSearchCase::from_json(&json).expect("round-trip must validate");
        // Escaping to the full editor keeps the draft — same struct,
        // just a different render path.
        let mut draft = draft;
        draft.guided_step = None;
        draft.build().expect("escaped draft still builds");
    }

    #[test]
    fn enabling_the_pressure_bound_emits_a_v24_declaration() {
        let draft = CaseDraft {
            use_pressure: true,
            max_pressure_mpa: 30.0,
            ..Default::default()
        };
        let case = draft.build().expect("draft must build");
        let parsed =
            CoupledSearchCase::from_json(&serde_json::to_string_pretty(&case).unwrap()).unwrap();
        assert_eq!(
            parsed.mechanical.and_then(|m| m.max_transverse_pressure_pa),
            Some(30.0e6)
        );
    }

    #[test]
    fn turn_index_shorthand_parses_all_groups() {
        let idx = parse_turn_indices("start:1,2 frac:0.5 end:0").unwrap();
        assert!(matches!(
            idx.as_slice(),
            [
                RelativeTurnIndex::FromStart { offset: 1 },
                RelativeTurnIndex::FromStart { offset: 2 },
                RelativeTurnIndex::Fraction { value } ,
                RelativeTurnIndex::FromEnd { offset: 0 },
            ] if *value == 0.5
        ));
        assert!(parse_turn_indices("bogus:1").is_err());
        assert!(parse_turn_indices("").is_err());
    }

    #[test]
    fn bad_choice_list_is_rejected_not_dropped() {
        let draft = CaseDraft {
            turns_text: "100, abc".into(),
            ..Default::default()
        };
        assert!(draft.build().is_err());
    }

    /// Helper: build + round-trip a modified draft, as save does.
    fn build_parsed(draft: CaseDraft) -> CoupledSearchCase {
        let case = draft.build().expect("draft must build");
        CoupledSearchCase::from_json(&serde_json::to_string_pretty(&case).unwrap())
            .expect("round-trip must validate")
    }

    #[test]
    fn uniformity_bound_emits_inside_the_region() {
        let parsed = build_parsed(CaseDraft {
            use_uniformity_bound: true,
            max_relative_deviation: 0.0005,
            ..Default::default()
        });
        assert_eq!(
            parsed
                .requirement
                .good_field_region
                .and_then(|r| r.max_relative_deviation),
            Some(0.0005)
        );
    }

    #[test]
    fn harmonics_declaration_emits_with_bounds() {
        let parsed = build_parsed(CaseDraft {
            use_harmonics: true,
            harmonic_radius_m: 0.02,
            use_harmonic_normal_bound: true,
            harmonic_normal_bound: 0.001,
            ..Default::default()
        });
        let h = parsed
            .requirement
            .good_field_region
            .and_then(|r| r.harmonics)
            .expect("harmonics block");
        assert_eq!(h.reference_radius_m, 0.02);
        assert_eq!(h.max_normal_unit_fraction, Some(0.001));
        assert_eq!(h.max_skew_unit_fraction, None);
    }

    #[test]
    fn geometry_axis_replaces_the_fixed_dimension() {
        let parsed = build_parsed(CaseDraft {
            search_bend_radius: true,
            bend_radius_text: "0.09, 0.12".into(),
            baseline_bend_radius: 0.12,
            ..Default::default()
        });
        assert_eq!(parsed.fixed_geometry.bend_radius_m, None);
        assert_eq!(
            parsed.choices.bend_radius_m,
            Some(vec![0.09, 0.12]),
            "the axis list emits as declared"
        );
        assert_eq!(parsed.baseline.bend_radius_m, Some(0.12));
        // The sibling dimension still resolves — through its fixed
        // field here.
        assert_eq!(parsed.fixed_geometry.straight_half_length_m, Some(0.15));
    }

    #[test]
    fn opex_block_emits_the_declared_terms() {
        let parsed = build_parsed(CaseDraft {
            use_opex: true,
            cop_fraction: 0.2,
            sink_temperature_k: 300.0,
            opex_years: 15,
            ..Default::default()
        });
        let opex = parsed.opex.expect("opex block");
        assert_eq!(opex.heat_loads_w.len(), 2);
        assert_eq!(opex.cop_fraction_of_carnot, 0.2);
        assert_eq!(opex.operating_years, 15);
        // An empty heat-load table is a declaration bug — the schema
        // requires at least one term.
        let draft = CaseDraft {
            use_opex: true,
            heat_loads: vec![],
            ..Default::default()
        };
        assert!(
            CoupledSearchCase::from_json(
                &serde_json::to_string_pretty(&draft.build().unwrap()).unwrap()
            )
            .is_err(),
            "an opex declaration with no heat loads must not round-trip"
        );
    }

    #[test]
    fn tape_specs_and_grading_emit() {
        let parsed = build_parsed(CaseDraft {
            tape_specs: vec![TapeSpecRow {
                id: "premium".into(),
                dataset_choice: 0,
                dataset_id: DATASET_CHOICES[0].into(),
                dataset_sha: MaterialDataset::embedded_by_id(DATASET_CHOICES[0])
                    .map(|d| d.metadata.csv_sha256)
                    .unwrap_or_default(),
                price_usd_per_m: 120.0,
                low_field_clamp_t: 0.05,
                piece_offerings_text: String::new(),
                price_source: 1,
            }],
            grading: vec![GradingRow {
                lo: 0.0,
                hi: 0.5,
                specs_text: "base, premium".into(),
            }],
            ..Default::default()
        });
        let spec = &parsed.tape_specs.as_ref().unwrap()["premium"];
        assert_eq!(spec.price_usd_per_m, 120.0);
        assert_eq!(spec.material.dataset_id, DATASET_CHOICES[0]);
        let regions = &parsed.grading.as_ref().unwrap().regions;
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].turn_range, [0.0, 0.5]);
        assert_eq!(
            regions[0].tape_spec_choices,
            vec!["base".to_string(), "premium".to_string()]
        );
    }

    #[test]
    fn declared_screens_emit() {
        let parsed = build_parsed(CaseDraft {
            use_thermal_margin: true,
            use_ac_loss: true,
            use_quench_hotspot: true,
            use_screening_current: true,
            use_transition: true,
            use_transition_voltage: true,
            use_quench_transient: true,
            use_qt_max_t: true,
            use_qt_detection: true,
            ..Default::default()
        });
        assert_eq!(parsed.thermal_margin.unwrap().min_margin_k, 5.0);
        assert_eq!(parsed.ac_loss.unwrap().sc_layer_thickness_m, 2.0e-6);
        let hotspot = parsed.quench_hotspot.unwrap();
        assert!((hotspot.stabilizer_area_m2 - 5.0e-6).abs() < 1e-18);
        assert_eq!(hotspot.quench_function_a2s_per_m4.len(), 4);
        assert_eq!(
            parsed
                .screening_current
                .unwrap()
                .max_penetrated_width_fraction,
            0.3
        );
        assert_eq!(parsed.transition.unwrap().max_voltage_v, Some(10.0));
        let qt = parsed.quench_transient.unwrap();
        assert_eq!(qt.resistivity_ohm_m.len(), 4);
        assert_eq!(qt.max_detection_time_s, Some(0.1));
    }

    #[test]
    fn field_map_suppresses_region_and_geometry_axes() {
        let draft = CaseDraft {
            field_map: Some(FieldMap::CartesianBxByBz {
                source_sha256: "0".repeat(64),
                reference_ampere_turns_a: 1.0,
                x_levels_m: vec![0.0, 0.1],
                y_levels_m: vec![0.0, 0.1],
                z_levels_m: vec![0.0, 0.1],
                entries: Vec::new(),
            }),
            field_map_label: "map.json".into(),
            use_region: true,
            search_bend_radius: true,
            ..Default::default()
        };
        // build() only — the empty grid is not a valid map, and deep
        // map-vs-pack validation is the schema's job; this test checks
        // the builder's emission wiring.
        let case = draft.build().expect("draft must build");
        assert!(case.field_map.is_some());
        assert!(
            case.requirement.good_field_region.is_none(),
            "a declared map suppresses the region"
        );
        assert_eq!(case.choices.bend_radius_m, None);
        assert_eq!(case.baseline.bend_radius_m, None);
        assert_eq!(case.fixed_geometry.pack_radial_width_m, Some(0.048));
        assert_eq!(case.fixed_geometry.pack_axial_height_m, Some(0.012));
    }
}
