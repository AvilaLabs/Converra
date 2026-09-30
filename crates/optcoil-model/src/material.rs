//! Measured conductor data. Kept separate from OC-001's synthetic grade schema.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ModelError;

pub const MATERIAL_SCHEMA: &str = "optcoil-measured-material/v1";
/// v2 is format-identical to v1; the only change is that `data_class` may
/// carry classes beyond `measured` (currently `measured_with_model_extension`).
/// Schema v1 remains restricted to purely measured datasets, so a modeled
/// extension cannot be presented under the original schema identity.
pub const MATERIAL_SCHEMA_V2: &str = "optcoil-measured-material/v2";
/// Single-file dataset bundle (`MaterialDataset::from_bundle_json`):
/// `{"schema": "optcoil-material-dataset/v1", "metadata": {...},
/// "csv_data": "<raw csv text>"}`. The bundle carries the same metadata +
/// measurement pair as the embedded assets, so `metadata.csv_sha256`
/// still binds the exact measurement bytes — a bundle cannot silently
/// carry different rows than its metadata declares.
pub const MATERIAL_DATASET_BUNDLE_SCHEMA: &str = "optcoil-material-dataset/v1";
/// Bundle v2 adds an optional `attestation` member — an issuer's ed25519
/// signature over the dataset identity (`crate::attestation`). v1 bundles
/// remain valid and are simply unsigned.
pub const MATERIAL_DATASET_BUNDLE_SCHEMA_V2: &str = "optcoil-material-dataset/v2";
pub const OC003_JSON: &str = include_str!("../../../benchmarks/measured/oc-003.json");
pub const OC005_JSON: &str = include_str!("../../../benchmarks/measured/oc-005.json");
pub const SUPERPOWER_ID: &str = "robinson-superpower-ap-v3";
pub const SUPERPOWER_METADATA: &str =
    include_str!("../../../data/materials/robinson-superpower-ap-v3/material.json");
pub const SUPERPOWER_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-superpower-ap-v3/measurements.csv");
pub const SUPERPOWER_LOWFIELD_ID: &str = "robinson-superpower-ap-v3-lowfield";
pub const SUPERPOWER_LOWFIELD_METADATA: &str =
    include_str!("../../../data/materials/robinson-superpower-ap-v3-lowfield/material.json");
pub const SUPERPOWER_LOWFIELD_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-superpower-ap-v3-lowfield/measurements.csv");
pub const SUPERPOWER_MODELEXT_ID: &str = "robinson-superpower-ap-v3-modelext";
pub const SUPERPOWER_MODELEXT_METADATA: &str =
    include_str!("../../../data/materials/robinson-superpower-ap-v3-modelext/material.json");
pub const SUPERPOWER_MODELEXT_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-superpower-ap-v3-modelext/measurements.csv");
pub const SHANGHAI_HFLT_ID: &str = "robinson-shanghai-hflt-v3";
pub const BABOUCHE_SP_ID: &str = "babouche-superpower-m31477-memfit-v1";
pub const BABOUCHE_SP_METADATA: &str =
    include_str!("../../../data/materials/babouche-superpower-m31477-memfit-v1/material.json");
pub const BABOUCHE_SP_CSV: &[u8] =
    include_bytes!("../../../data/materials/babouche-superpower-m31477-memfit-v1/measurements.csv");

pub const BABOUCHE_SST_ID: &str = "babouche-sst-yp506-memfit-v1";
pub const BABOUCHE_SST_METADATA: &str =
    include_str!("../../../data/materials/babouche-sst-yp506-memfit-v1/material.json");
pub const BABOUCHE_SST_CSV: &[u8] =
    include_bytes!("../../../data/materials/babouche-sst-yp506-memfit-v1/measurements.csv");

/// Extended-grid variant of the same published MEM fit: identical
/// parameters, wider emitted (T,B) node set reaching the quad's 19 T
/// corner at 20 K.  New dataset identity; v1 stays embedded unchanged.
pub const BABOUCHE_SP_V2_ID: &str = "babouche-superpower-m31477-memfit-v2";
pub const BABOUCHE_SP_V2_METADATA: &str =
    include_str!("../../../data/materials/babouche-superpower-m31477-memfit-v2/material.json");
pub const BABOUCHE_SP_V2_CSV: &[u8] =
    include_bytes!("../../../data/materials/babouche-superpower-m31477-memfit-v2/measurements.csv");

/// Extended-grid variant; see `BABOUCHE_SP_V2_ID`.
pub const BABOUCHE_SST_V2_ID: &str = "babouche-sst-yp506-memfit-v2";
pub const BABOUCHE_SST_V2_METADATA: &str =
    include_str!("../../../data/materials/babouche-sst-yp506-memfit-v2/material.json");
pub const BABOUCHE_SST_V2_CSV: &[u8] =
    include_bytes!("../../../data/materials/babouche-sst-yp506-memfit-v2/measurements.csv");

pub const SHANGHAI_HFLT_METADATA: &str =
    include_str!("../../../data/materials/robinson-shanghai-hflt-v3/material.json");
pub const SHANGHAI_HFLT_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-shanghai-hflt-v3/measurements.csv");
pub const THEVA_AP_ID: &str = "robinson-theva-ap-v2";
pub const THEVA_AP_METADATA: &str =
    include_str!("../../../data/materials/robinson-theva-ap-v2/material.json");
pub const THEVA_AP_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-theva-ap-v2/measurements.csv");
pub const FFJ_YBCO_ID: &str = "robinson-ffj-ybco-v1";
pub const FFJ_YBCO_METADATA: &str =
    include_str!("../../../data/materials/robinson-ffj-ybco-v1/material.json");
pub const FFJ_YBCO_CSV: &[u8] =
    include_bytes!("../../../data/materials/robinson-ffj-ybco-v1/measurements.csv");
const HEADERS: [&str; 10] = [
    "source_row",
    "nominal_temperature_k",
    "nominal_field_t",
    "nominal_angle_deg",
    "temperature_k",
    "applied_field_t",
    "angle_from_normal_deg",
    "ic_a_per_m",
    "bridge_ic_a",
    "n_value",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialMetadata {
    pub schema: String,
    pub id: String,
    pub data_class: MaterialDataClass,
    pub material: String,
    pub sample_id: String,
    pub source_doi: String,
    pub source_url: String,
    pub authors: Vec<String>,
    pub license: String,
    pub license_url: String,
    pub measurement_dates: String,
    pub source_xlsx_sha256: String,
    pub csv_sha256: String,
    pub preparation_source_sha256: String,
    pub source_description_sha256: String,
    /// Present only on tabular imports. Binds the exact original upload and
    /// the explicit unit/column transformation used to produce canonical CSV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tabular_import: Option<TabularImportReceipt>,
    pub electric_field_criterion_v_per_m: f64,
    pub voltage_tap_spacing_m: f64,
    pub measured_bridge_width_m: f64,
    pub original_tape_width_m: f64,
    pub field_basis: MaterialFieldBasis,
    pub angle_convention: AngleConvention,
    pub coordinate_policy: String,
    pub normalization: String,
    pub measurement_uncertainty_fraction: Option<f64>,
    pub strain_state: String,
    pub selection: MaterialSelection,
    pub max_cell_spans: CellSpanLimits,
    pub point_count: usize,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabularImportReceipt {
    pub schema: String,
    pub source_sha256: String,
    pub source_format: crate::tabular_import::TabularFormat,
    pub worksheet: Option<String>,
    pub mapping: crate::tabular_import::ColumnMapping,
    pub canonical_csv_sha256: String,
    /// Declared Ic perturbations applied after import, from the canonical
    /// imported CSV through the currently bound dataset CSV.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ic_scalings: Vec<IcScalingReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcScalingReceipt {
    pub parent_csv_sha256: String,
    pub result_csv_sha256: String,
    pub factor: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialDataClass {
    Measured,
    /// Rows above the measured domain are model-generated (see the
    /// dataset's `limitations` and preparation audit). Any verdict that
    /// leans on such nodes is model-informed, not measured-data-verified.
    MeasuredWithModelExtension,
    /// Every row is an evaluation of a published parametrization (e.g. a
    /// fitted scaling model), not a measurement. No verdict produced with
    /// such a dataset is measured-data-verified; see the dataset's
    /// `limitations` and preparation audit.
    PublishedModelFit,
    /// Values transformed for a declared sensitivity/what-if analysis.
    /// Source attribution is retained, but these transformed values are not
    /// measurements, published fit evaluations, or covered by a source
    /// attestation.
    SyntheticSensitivity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterialFieldBasis {
    AppliedFieldIncludingSampleSelfFieldResponse,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AngleConvention {
    OrientedFromTapeNormalInMaximumLorentzGeometry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialSelection {
    pub nominal_temperature_k: Vec<f64>,
    pub nominal_field_t: Vec<f64>,
    pub nominal_angle_range_deg: [f64; 2],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSpanLimits {
    pub temperature_k: f64,
    pub field_ratio: f64,
    pub angle_deg: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialPoint {
    /// One-based worksheet row; stable provenance for every interpolation weight.
    pub source_row: u32,
    pub nominal_temperature_k: f64,
    pub nominal_field_t: f64,
    pub nominal_angle_deg: f64,
    pub temperature_k: f64,
    pub applied_field_t: f64,
    pub angle_from_normal_deg: f64,
    /// Published current per width, converted to SI A/m; not a local Jc in A/m².
    pub ic_a_per_m: f64,
    pub bridge_ic_a: f64,
    pub n_value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialDataset {
    pub metadata: MaterialMetadata,
    pub points: Vec<MaterialPoint>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialQuery {
    pub temperature_k: f64,
    pub applied_field_t: f64,
    pub angle_from_normal_deg: f64,
    pub electric_field_criterion_v_per_m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialBenchmark {
    pub schema: String,
    pub id: String,
    pub description: String,
    pub dataset_id: String,
    pub method: String,
    pub folds: Vec<MaterialFold>,
    pub holdout_policy: String,
    pub acceptance: MaterialAcceptance,
    #[serde(default)]
    pub stratum_acceptance: Option<StratumAcceptance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumAcceptance {
    pub three_axis_max_positive_relative_error: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialFold {
    pub id: String,
    pub axis: ValidationAxis,
    #[serde(default)]
    pub holdout_values: Vec<f64>,
    #[serde(default)]
    pub holdout_by_axis: Option<HoldoutByAxis>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutByAxis {
    #[serde(default)]
    pub temperature: Vec<f64>,
    #[serde(default)]
    pub field: Vec<f64>,
    #[serde(default)]
    pub angle: Vec<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationAxis {
    Temperature,
    Field,
    Angle,
    Multi,
}

impl ValidationAxis {
    /// Only meaningful for the three single-axis variants.
    pub fn index(self) -> usize {
        match self {
            Self::Temperature => 0,
            Self::Field => 1,
            Self::Angle => 2,
            Self::Multi => panic!("ValidationAxis::index has no single index for Multi"),
        }
    }
}

impl HoldoutByAxis {
    fn lists(&self) -> [&[f64]; 3] {
        [&self.temperature, &self.field, &self.angle]
    }

    fn is_empty(&self) -> bool {
        self.lists().iter().all(|l| l.is_empty())
    }
}

/// Per-axis holdout lists for any fold, single-axis or multi-axis: index 0 is
/// temperature, 1 is field, 2 is angle. Single-axis folds place their one
/// `holdout_values` list at their own axis index and leave the other two
/// empty, so callers can treat every fold uniformly.
pub fn fold_holdout_lists(fold: &MaterialFold) -> [&[f64]; 3] {
    static EMPTY: [f64; 0] = [];
    match fold.axis {
        ValidationAxis::Temperature => [&fold.holdout_values, &EMPTY, &EMPTY],
        ValidationAxis::Field => [&EMPTY, &fold.holdout_values, &EMPTY],
        ValidationAxis::Angle => [&EMPTY, &EMPTY, &fold.holdout_values],
        ValidationAxis::Multi => fold
            .holdout_by_axis
            .as_ref()
            .expect("multi-axis fold validated to carry holdout_by_axis")
            .lists(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialAcceptance {
    pub minimum_coverage_fraction: f64,
    pub max_p95_relative_error: f64,
    pub max_relative_error: f64,
    pub max_positive_relative_error: f64,
    pub max_node_relative_error: f64,
}

impl MaterialBenchmark {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let case: Self = crate::encoding::parse_case(json)?;
        case.validate()?;
        Ok(case)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let schema_v2 = self.schema == "optcoil-material-benchmark/v2";
        if (!schema_v2 && self.schema != "optcoil-material-benchmark/v1")
            || self.id.trim().is_empty()
            || self.dataset_id.trim().is_empty()
            || self.folds.is_empty()
            || self.folds.len() > 8
        {
            return Err(invalid(
                "invalid material benchmark identity, schema or fold count",
            ));
        }
        let mut identities = HashSet::new();
        for fold in &self.folds {
            if fold.id.trim().is_empty() || !identities.insert(&fold.id) {
                return Err(invalid("fold identities must be nonempty and unique"));
            }
            fold.validate(schema_v2)?;
        }
        let a = &self.acceptance;
        for tolerance in [
            a.minimum_coverage_fraction,
            a.max_p95_relative_error,
            a.max_relative_error,
            a.max_positive_relative_error,
            a.max_node_relative_error,
        ] {
            if !positive(tolerance) || tolerance > 1.0 {
                return Err(invalid("material acceptance fractions must be in (0, 1]"));
            }
        }
        if a.max_p95_relative_error > a.max_relative_error
            || a.max_positive_relative_error > a.max_relative_error
        {
            return Err(invalid("material error limits are inconsistent"));
        }
        if let Some(stratum) = &self.stratum_acceptance {
            let gate = stratum.three_axis_max_positive_relative_error;
            if !positive(gate) || gate > 1.0 || gate > a.max_positive_relative_error {
                return Err(invalid(
                    "stratum acceptance gate must be in (0, 1] and at most the fold's max_positive_relative_error",
                ));
            }
            if !self
                .folds
                .iter()
                .any(|f| matches!(f.axis, ValidationAxis::Multi))
            {
                return Err(invalid(
                    "stratum_acceptance requires at least one multi-axis fold",
                ));
            }
        }
        Ok(())
    }
}

impl MaterialFold {
    fn validate(&self, schema_v2: bool) -> Result<(), ModelError> {
        match self.axis {
            ValidationAxis::Multi => {
                if !schema_v2 {
                    return Err(invalid("multi-axis folds require schema v2"));
                }
                if !self.holdout_values.is_empty() {
                    return Err(invalid(
                        "multi-axis folds must not set holdout_values; use holdout_by_axis",
                    ));
                }
                let by_axis = self.holdout_by_axis.as_ref().ok_or_else(|| {
                    invalid("multi-axis folds require a nonempty holdout_by_axis")
                })?;
                if by_axis.is_empty() {
                    return Err(invalid(
                        "multi-axis holdout_by_axis needs at least one nonempty axis list",
                    ));
                }
                for list in by_axis.lists() {
                    validate_unique_finite(list)?;
                }
            }
            ValidationAxis::Temperature | ValidationAxis::Field | ValidationAxis::Angle => {
                if self.holdout_by_axis.is_some() {
                    return Err(invalid(
                        "single-axis folds must not set holdout_by_axis; use holdout_values",
                    ));
                }
                if self.holdout_values.is_empty() {
                    return Err(invalid("single-axis folds require nonempty holdout_values"));
                }
                validate_unique_finite(&self.holdout_values)?;
            }
        }
        Ok(())
    }
}

fn validate_unique_finite(values: &[f64]) -> Result<(), ModelError> {
    let mut seen = HashSet::new();
    if values
        .iter()
        .any(|v| !v.is_finite() || !seen.insert(if *v == 0.0 { 0 } else { v.to_bits() }))
    {
        return Err(invalid("holdout values must be finite and unique"));
    }
    Ok(())
}

impl MaterialPoint {
    pub fn position(&self) -> [f64; 3] {
        [
            self.temperature_k,
            self.applied_field_t,
            self.angle_from_normal_deg,
        ]
    }

    pub fn nominal_position(&self) -> [f64; 3] {
        [
            self.nominal_temperature_k,
            self.nominal_field_t,
            if self.nominal_angle_deg == 0.0 {
                0.0
            } else {
                self.nominal_angle_deg
            },
        ]
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        for coordinate in [self.position(), self.nominal_position()] {
            validate_coordinates(coordinate, false)?;
        }
        if self.source_row == 0
            || !positive(self.ic_a_per_m)
            || !positive(self.bridge_ic_a)
            || !self.n_value.is_finite()
            || self.n_value <= 1.0
        {
            return Err(invalid(
                "invalid measurement identity, current or fit exponent",
            ));
        }
        Ok(())
    }
}

impl CellSpanLimits {
    pub fn validate(&self) -> Result<(), ModelError> {
        if !positive(self.temperature_k)
            || !positive(self.angle_deg)
            || !self.field_ratio.is_finite()
            || self.field_ratio <= 1.0
        {
            return Err(invalid("invalid interpolation cell span limits"));
        }
        Ok(())
    }
}

impl MaterialQuery {
    pub fn position(&self) -> [f64; 3] {
        [
            self.temperature_k,
            self.applied_field_t,
            self.angle_from_normal_deg,
        ]
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        validate_coordinates(self.position(), true)?;
        if !positive(self.electric_field_criterion_v_per_m) {
            return Err(invalid(
                "electric-field criterion must be finite and positive",
            ));
        }
        Ok(())
    }
}

impl MaterialMetadata {
    pub fn validate(&self) -> Result<(), ModelError> {
        let schema_ok = match self.schema.as_str() {
            MATERIAL_SCHEMA => matches!(self.data_class, MaterialDataClass::Measured),
            MATERIAL_SCHEMA_V2 => true,
            _ => false,
        };
        if !schema_ok || self.point_count < 8 || self.point_count > 100_000 {
            return Err(invalid(
                "unsupported material schema/class combination or point count outside 8..=100000",
            ));
        }
        for text in [
            &self.id,
            &self.material,
            &self.sample_id,
            &self.source_doi,
            &self.source_url,
            &self.license,
            &self.license_url,
            &self.measurement_dates,
            &self.coordinate_policy,
            &self.normalization,
            &self.strain_state,
        ] {
            if text.trim().is_empty() {
                return Err(invalid(
                    "material provenance and conventions must be explicit",
                ));
            }
        }
        if self.authors.is_empty() || self.authors.iter().any(|a| a.trim().is_empty()) {
            return Err(invalid("material data requires author attribution"));
        }
        let receipt = self.tabular_import.as_ref();
        let source_xlsx_optional = receipt.is_some_and(|receipt| {
            receipt.source_format != crate::tabular_import::TabularFormat::Xlsx
        }) && self.source_xlsx_sha256.is_empty();
        if let Some(receipt) = receipt {
            if self.schema != MATERIAL_SCHEMA_V2
                || receipt.schema != "optcoil-tabular-import/v1"
                || !valid_sha256(&receipt.source_sha256)
                || !valid_sha256(&receipt.canonical_csv_sha256)
                || receipt.mapping.validate().is_err()
                || match receipt.source_format {
                    crate::tabular_import::TabularFormat::Xlsx => receipt
                        .worksheet
                        .as_ref()
                        .is_none_or(|worksheet| worksheet.trim().is_empty()),
                    crate::tabular_import::TabularFormat::Csv
                    | crate::tabular_import::TabularFormat::Tsv => receipt.worksheet.is_some(),
                }
                || (receipt.source_format == crate::tabular_import::TabularFormat::Xlsx
                    && receipt.source_sha256 != self.source_xlsx_sha256)
                || (receipt.source_format != crate::tabular_import::TabularFormat::Xlsx
                    && !self.source_xlsx_sha256.is_empty()
                    && !valid_sha256(&self.source_xlsx_sha256))
            {
                return Err(invalid(
                    "invalid tabular import receipt or source hash binding",
                ));
            }
            if receipt.ic_scalings.len() > 32 {
                return Err(invalid("tabular import scaling history exceeds 32 steps"));
            }
            let mut expected_parent = receipt.canonical_csv_sha256.as_str();
            for step in &receipt.ic_scalings {
                if step.parent_csv_sha256 != expected_parent
                    || !valid_sha256(&step.parent_csv_sha256)
                    || !valid_sha256(&step.result_csv_sha256)
                    || !positive(step.factor)
                {
                    return Err(invalid("invalid tabular import Ic scaling history"));
                }
                expected_parent = &step.result_csv_sha256;
            }
            if expected_parent != self.csv_sha256 {
                return Err(invalid(
                    "tabular import receipt does not bind the current canonical CSV",
                ));
            }
            let recipe = serde_json::to_vec(receipt).map_err(|e| invalid(&e.to_string()))?;
            if format!("{:x}", Sha256::digest(recipe)) != self.preparation_source_sha256 {
                return Err(invalid(
                    "tabular import preparation hash does not match its receipt",
                ));
            }
        }
        for (hash, optional) in [
            (&self.source_xlsx_sha256, source_xlsx_optional),
            (&self.csv_sha256, false),
            (&self.preparation_source_sha256, false),
            (&self.source_description_sha256, false),
        ] {
            if optional && hash.is_empty() {
                continue;
            }
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(invalid(
                    "material source identities must be lowercase SHA-256",
                ));
            }
        }
        for x in [
            self.electric_field_criterion_v_per_m,
            self.voltage_tap_spacing_m,
            self.measured_bridge_width_m,
            self.original_tape_width_m,
        ] {
            if !positive(x) {
                return Err(invalid(
                    "material dimensions and criterion must be finite and positive",
                ));
            }
        }
        if self.measured_bridge_width_m > self.original_tape_width_m {
            return Err(invalid(
                "measured bridge cannot be wider than original tape",
            ));
        }
        if self
            .measurement_uncertainty_fraction
            .is_some_and(|x| !x.is_finite() || !(0.0..1.0).contains(&x))
        {
            return Err(invalid("invalid stated measurement uncertainty"));
        }
        for axis in [
            &self.selection.nominal_temperature_k,
            &self.selection.nominal_field_t,
        ] {
            if axis.len() < 2
                || axis.iter().any(|x| !positive(*x))
                || axis.windows(2).any(|p| p[0] >= p[1])
            {
                return Err(invalid(
                    "selected temperature/field axes must be increasing and positive",
                ));
            }
        }
        let [low, high] = self.selection.nominal_angle_range_deg;
        if !low.is_finite() || !high.is_finite() || low >= high || low < -360.0 || high > 720.0 {
            return Err(invalid("invalid selected angle range"));
        }
        self.max_cell_spans.validate()
    }
}

impl MaterialDataset {
    pub fn from_csv(metadata_json: &str, csv_bytes: &[u8]) -> Result<Self, ModelError> {
        let metadata: MaterialMetadata = serde_json::from_str(metadata_json)?;
        metadata.validate()?;
        if csv_bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("material CSV exceeds 32 MiB"));
        }
        if format!("{:x}", Sha256::digest(csv_bytes)) != metadata.csv_sha256 {
            return Err(invalid("material CSV SHA-256 does not match its metadata"));
        }
        let mut reader = csv::ReaderBuilder::new().from_reader(csv_bytes);
        let headers = reader.headers().map_err(|e| invalid(&e.to_string()))?;
        let unique: HashSet<_> = headers.iter().collect();
        if headers.len() != HEADERS.len()
            || unique.len() != HEADERS.len()
            || !HEADERS.iter().all(|h| unique.contains(h))
        {
            return Err(invalid(
                "material CSV requires the exact SI column names without duplicates",
            ));
        }
        let mut points = Vec::new();
        for row in reader.deserialize::<MaterialPoint>() {
            if points.len() >= 100_000 {
                return Err(invalid("too many material measurements"));
            }
            points.push(row.map_err(|e| invalid(&format!("material CSV: {e}")))?);
        }
        let dataset = Self { metadata, points };
        dataset.validate()?;
        Ok(dataset)
    }

    /// Loads a single-file dataset bundle (v1 or v2): the metadata JSON and
    /// the raw measurement CSV text in one document. Validation is identical
    /// to `from_csv` — `metadata.csv_sha256` must match the bundle's
    /// `csv_data` bytes, so the declared dataset identity still pins the
    /// measurements exactly. A v2 bundle's `attestation` is ignored here;
    /// use `MaterialBundle::from_json` to retain it.
    pub fn from_bundle_json(json: &str) -> Result<Self, ModelError> {
        Ok(MaterialBundle::from_json(json)?.dataset)
    }

    pub fn embedded() -> Result<Self, ModelError> {
        Self::from_csv(SUPERPOWER_METADATA, SUPERPOWER_CSV)
    }

    /// Loads an embedded dataset by its declared `id`. `robinson-superpower-ap-v3`
    /// is the original characterization region `embedded()` also returns;
    /// `robinson-superpower-ap-v3-lowfield` is OC-006's low-field extension;
    /// `robinson-superpower-ap-v3-modelext` is OC-016's labeled model
    /// extension above 8 T (data_class `measured_with_model_extension`);
    /// `robinson-shanghai-hflt-v3` is a second measured dataset —
    /// Shanghai Superconductor HFLT on the same instrument over the same
    /// declared characterization window; `robinson-theva-ap-v2` is a
    /// third — THEVA Pro-Line Advanced Pinning over the same window with
    /// a denser measured 105-135 deg angle wedge.
    /// `babouche-superpower-m31477-memfit-v1` and `babouche-sst-yp506-memfit-v1`
    /// are declared model-fit datasets (data_class `published_model_fit`):
    /// evaluations of the published MEM parametrization of Babouche et al.
    /// 2026 (doi:10.1088/1361-6668/ae940e) on a declared interior grid — no
    /// row is a measurement.
    /// `robinson-ffj-ybco-v1` is a fourth measured dataset — Faraday
    /// Factory Japan YBCO over the same 20-40 K temperature window but a
    /// denser field grid (0.01-8 T), including a 0.01-0.7 T decade the
    /// other embedded datasets do not cover.
    /// Every id `embedded_by_id` accepts, in declaration order. Kept next
    /// to the match so adding a dataset updates both at once.
    pub const EMBEDDED_IDS: &[&str] = &[
        SUPERPOWER_ID,
        SUPERPOWER_LOWFIELD_ID,
        SUPERPOWER_MODELEXT_ID,
        SHANGHAI_HFLT_ID,
        THEVA_AP_ID,
        FFJ_YBCO_ID,
        BABOUCHE_SP_ID,
        BABOUCHE_SST_ID,
        BABOUCHE_SP_V2_ID,
        BABOUCHE_SST_V2_ID,
    ];

    /// An unrecognized id is rejected explicitly rather than silently
    /// falling back to any embedded dataset.
    pub fn embedded_by_id(id: &str) -> Result<Self, ModelError> {
        match id {
            SUPERPOWER_ID => Self::from_csv(SUPERPOWER_METADATA, SUPERPOWER_CSV),
            SUPERPOWER_LOWFIELD_ID => {
                Self::from_csv(SUPERPOWER_LOWFIELD_METADATA, SUPERPOWER_LOWFIELD_CSV)
            }
            BABOUCHE_SP_ID => Self::from_csv(BABOUCHE_SP_METADATA, BABOUCHE_SP_CSV),
            BABOUCHE_SST_ID => Self::from_csv(BABOUCHE_SST_METADATA, BABOUCHE_SST_CSV),
            BABOUCHE_SP_V2_ID => Self::from_csv(BABOUCHE_SP_V2_METADATA, BABOUCHE_SP_V2_CSV),
            BABOUCHE_SST_V2_ID => Self::from_csv(BABOUCHE_SST_V2_METADATA, BABOUCHE_SST_V2_CSV),
            SUPERPOWER_MODELEXT_ID => {
                Self::from_csv(SUPERPOWER_MODELEXT_METADATA, SUPERPOWER_MODELEXT_CSV)
            }
            SHANGHAI_HFLT_ID => Self::from_csv(SHANGHAI_HFLT_METADATA, SHANGHAI_HFLT_CSV),
            THEVA_AP_ID => Self::from_csv(THEVA_AP_METADATA, THEVA_AP_CSV),
            FFJ_YBCO_ID => Self::from_csv(FFJ_YBCO_METADATA, FFJ_YBCO_CSV),
            _ => Err(invalid(&format!(
                "unknown embedded material dataset id: {id}"
            ))),
        }
    }

    /// Retains an embedded dataset's source metadata and CSV as an unsigned
    /// portable v1 bundle. The CSV bytes are included verbatim, so the
    /// declared `csv_sha256` remains the exact source identity.
    pub fn embedded_bundle_json(id: &str) -> Result<String, ModelError> {
        let (metadata, csv) = match id {
            SUPERPOWER_ID => (SUPERPOWER_METADATA, SUPERPOWER_CSV),
            SUPERPOWER_LOWFIELD_ID => (SUPERPOWER_LOWFIELD_METADATA, SUPERPOWER_LOWFIELD_CSV),
            SUPERPOWER_MODELEXT_ID => (SUPERPOWER_MODELEXT_METADATA, SUPERPOWER_MODELEXT_CSV),
            SHANGHAI_HFLT_ID => (SHANGHAI_HFLT_METADATA, SHANGHAI_HFLT_CSV),
            THEVA_AP_ID => (THEVA_AP_METADATA, THEVA_AP_CSV),
            FFJ_YBCO_ID => (FFJ_YBCO_METADATA, FFJ_YBCO_CSV),
            BABOUCHE_SP_ID => (BABOUCHE_SP_METADATA, BABOUCHE_SP_CSV),
            BABOUCHE_SST_ID => (BABOUCHE_SST_METADATA, BABOUCHE_SST_CSV),
            BABOUCHE_SP_V2_ID => (BABOUCHE_SP_V2_METADATA, BABOUCHE_SP_V2_CSV),
            BABOUCHE_SST_V2_ID => (BABOUCHE_SST_V2_METADATA, BABOUCHE_SST_V2_CSV),
            _ => {
                return Err(invalid(&format!(
                    "unknown embedded material dataset id: {id}"
                )));
            }
        };
        let csv_data = std::str::from_utf8(csv).map_err(|e| invalid(&e.to_string()))?;
        let quoted_csv = serde_json::to_string(csv_data)?;
        Ok(format!(
            "{{\"schema\":\"{MATERIAL_DATASET_BUNDLE_SCHEMA}\",\"metadata\":{metadata},\"csv_data\":{quoted_csv}}}"
        ))
    }

    /// A declared perturbation for sensitivity studies: every measured Ic
    /// (`ic_a_per_m` and `bridge_ic_a`) scaled by `factor`, then
    /// re-serialized and re-hashed so the result is a *distinct* dataset
    /// identity — `id` gains an `@icx<factor>` suffix and `csv_sha256`
    /// binds the scaled bytes. The scaled data is synthetic — the base
    /// dataset under a declared multiplicative change, not a new
    /// measurement — and stays honest because a case referencing it must
    /// declare the new id and sha.
    pub fn scaled_ic(&self, factor: f64) -> Result<Self, ModelError> {
        if !(factor.is_finite() && factor > 0.0) {
            return Err(invalid("ic scale factor must be finite and positive"));
        }
        let mut scaled = self.clone();
        for p in &mut scaled.points {
            p.ic_a_per_m *= factor;
            p.bridge_ic_a *= factor;
        }
        let mut wtr = csv::Writer::from_writer(Vec::new());
        for p in &scaled.points {
            wtr.serialize(p).map_err(|e| invalid(&e.to_string()))?;
        }
        let bytes = wtr.into_inner().map_err(|e| invalid(&e.to_string()))?;
        let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
        if let Some(receipt) = scaled.metadata.tabular_import.as_mut() {
            if receipt.ic_scalings.len() >= 32 {
                return Err(invalid("tabular import scaling history exceeds 32 steps"));
            }
            receipt.ic_scalings.push(IcScalingReceipt {
                parent_csv_sha256: self.metadata.csv_sha256.clone(),
                result_csv_sha256: result_sha256.clone(),
                factor,
            });
            let recipe = serde_json::to_vec(receipt).map_err(|e| invalid(&e.to_string()))?;
            scaled.metadata.preparation_source_sha256 = format!("{:x}", Sha256::digest(recipe));
            scaled.metadata.data_class = MaterialDataClass::SyntheticSensitivity;
        }
        scaled.metadata.csv_sha256 = result_sha256;
        scaled.metadata.id = format!("{}@icx{}", self.metadata.id, factor);
        scaled.validate()?;
        Ok(scaled)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        self.metadata.validate()?;
        if self.points.len() != self.metadata.point_count {
            return Err(invalid("material point count mismatch"));
        }
        let mut identities = HashSet::new();
        let mut nominal_nodes = HashSet::new();
        let mut measured_nodes = HashSet::new();
        let selection = &self.metadata.selection;
        for p in &self.points {
            p.validate()?;
            if !identities.insert(p.source_row)
                || !nominal_nodes.insert(p.nominal_position().map(f64::to_bits))
                || !measured_nodes
                    .insert(p.position().map(|x| if x == 0.0 { 0 } else { x.to_bits() }))
            {
                return Err(invalid(
                    "duplicate material identity or coordinate; repeats require an explicit policy",
                ));
            }
            if !selection
                .nominal_temperature_k
                .contains(&p.nominal_temperature_k)
                || !selection.nominal_field_t.contains(&p.nominal_field_t)
                || !(selection.nominal_angle_range_deg[0]..=selection.nominal_angle_range_deg[1])
                    .contains(&p.nominal_angle_deg)
            {
                return Err(invalid(
                    "measurement lies outside the declared nominal selection",
                ));
            }
        }
        Ok(())
    }
}

pub fn validate_coordinates(position: [f64; 3], allow_zero_field: bool) -> Result<(), ModelError> {
    if position.iter().any(|x| !x.is_finite())
        || position[0] <= 0.0
        || position[0] > 400.0
        || position[1] < 0.0
        || position[1] > 1000.0
        || (!allow_zero_field && position[1] == 0.0)
        || !(-360.0..=720.0).contains(&position[2])
    {
        return Err(invalid(
            "invalid temperature, applied-field magnitude or oriented angle",
        ));
    }
    Ok(())
}

fn positive(x: f64) -> bool {
    x.is_finite() && x > 0.0
}
fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

/// A parsed dataset bundle: the validated dataset, the original raw CSV
/// text (preserved byte-for-byte so `to_json` never rewrites measurements),
/// and an optional issuer attestation (v2 bundles only).
pub struct MaterialBundle {
    pub dataset: MaterialDataset,
    pub csv_data: String,
    pub attestation: Option<crate::attestation::DatasetAttestation>,
}

impl MaterialBundle {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        #[derive(Deserialize)]
        struct Peek {
            schema: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct BundleV1 {
            #[allow(dead_code)]
            schema: String,
            metadata: serde_json::Value,
            csv_data: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct BundleV2 {
            #[allow(dead_code)]
            schema: String,
            metadata: serde_json::Value,
            csv_data: String,
            attestation: Option<crate::attestation::DatasetAttestation>,
        }
        let peek: Peek = serde_json::from_str(json)?;
        let (metadata, csv_data, attestation) = match peek.schema.as_str() {
            MATERIAL_DATASET_BUNDLE_SCHEMA => {
                let bundle: BundleV1 = serde_json::from_str(json)?;
                (bundle.metadata, bundle.csv_data, None)
            }
            MATERIAL_DATASET_BUNDLE_SCHEMA_V2 => {
                let bundle: BundleV2 = serde_json::from_str(json)?;
                (bundle.metadata, bundle.csv_data, bundle.attestation)
            }
            other => {
                return Err(invalid(&format!(
                    "unsupported material dataset bundle schema: {other}"
                )));
            }
        };
        let metadata_json = serde_json::to_string(&metadata)?;
        let dataset = MaterialDataset::from_csv(&metadata_json, csv_data.as_bytes())?;
        Ok(Self {
            dataset,
            csv_data,
            attestation,
        })
    }

    /// Builds a bundle from a validated dataset and its source CSV text.
    /// The CSV must be the exact bytes the dataset was parsed from.
    pub fn from_parts(
        dataset: MaterialDataset,
        csv_data: String,
        attestation: Option<crate::attestation::DatasetAttestation>,
    ) -> Result<Self, ModelError> {
        if format!("{:x}", Sha256::digest(csv_data.as_bytes())) != dataset.metadata.csv_sha256 {
            return Err(invalid(
                "material bundle CSV does not match metadata.csv_sha256",
            ));
        }
        Ok(Self {
            dataset,
            csv_data,
            attestation,
        })
    }

    /// Serializes as a v2 bundle, emitting `csv_data` verbatim and
    /// `attestation` when present.
    pub fn to_json(&self) -> Result<String, ModelError> {
        if format!("{:x}", Sha256::digest(self.csv_data.as_bytes()))
            != self.dataset.metadata.csv_sha256
        {
            return Err(invalid(
                "material bundle CSV does not match metadata.csv_sha256",
            ));
        }
        #[derive(Serialize)]
        struct BundleV2<'a> {
            schema: &'a str,
            metadata: &'a MaterialMetadata,
            csv_data: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            attestation: Option<&'a crate::attestation::DatasetAttestation>,
        }
        let bundle = BundleV2 {
            schema: MATERIAL_DATASET_BUNDLE_SCHEMA_V2,
            metadata: &self.dataset.metadata,
            csv_data: &self.csv_data,
            attestation: self.attestation.as_ref(),
        };
        serde_json::to_string_pretty(&bundle).map_err(ModelError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_source_preserves_units_coordinates_and_provenance() {
        let data = MaterialDataset::embedded().unwrap();
        assert_eq!(data.points.len(), 1505);
        assert_eq!(data.metadata.measured_bridge_width_m, 0.001);
        assert_eq!(data.metadata.electric_field_criterion_v_per_m, 1e-4);
        assert_eq!(data.metadata.measurement_uncertainty_fraction, None);
        assert!(
            data.points
                .iter()
                .any(|p| p.temperature_k != p.nominal_temperature_k)
        );
        // The source independently publishes bridge Ic rounded to 0.01 A and
        // width-normalized Ic rounded to 0.01 A/cm. This catches factor-of-100
        // mistakes without deriving expected values from the importer formula.
        assert!(data.points.iter().all(|p| {
            (p.ic_a_per_m * data.metadata.measured_bridge_width_m - p.bridge_ic_a).abs() <= 0.006
        }));
        assert!(
            data.points
                .iter()
                .any(|p| (p.angle_from_normal_deg - p.nominal_angle_deg).abs() > 1.0)
        );
    }

    #[test]
    fn corrupt_data_duplicate_rows_and_unknown_conventions_are_rejected() {
        let mut corrupt = SUPERPOWER_CSV.to_vec();
        corrupt.push(b'\n');
        assert!(MaterialDataset::from_csv(SUPERPOWER_METADATA, &corrupt).is_err());
        assert!(
            MaterialDataset::from_csv(
                &SUPERPOWER_METADATA.replace(
                    "applied_field_including_sample_self_field_response",
                    "intrinsic_local_field"
                ),
                SUPERPOWER_CSV
            )
            .is_err()
        );
        let mut data = MaterialDataset::embedded().unwrap();
        data.points[1].source_row = data.points[0].source_row;
        assert!(data.validate().is_err());
        let mut data = MaterialDataset::embedded().unwrap();
        data.points[0].ic_a_per_m = f64::NAN;
        assert!(data.validate().is_err());
        // Structural errors must still be rejected if a caller supplies a
        // matching content hash for a malformed table.
        let mut metadata = data.metadata;
        let bad_csv = String::from_utf8(SUPERPOWER_CSV.to_vec())
            .unwrap()
            .replacen("ic_a_per_m", "ic_a_per_cm", 1);
        metadata.csv_sha256 = format!("{:x}", Sha256::digest(bad_csv.as_bytes()));
        assert!(
            MaterialDataset::from_csv(
                &serde_json::to_string(&metadata).unwrap(),
                bad_csv.as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn embedded_by_id_round_trips_both_known_datasets_and_rejects_unknown_ids() {
        let original = MaterialDataset::embedded_by_id(SUPERPOWER_ID).unwrap();
        assert_eq!(original.metadata.id, SUPERPOWER_ID);
        assert_eq!(original.points.len(), 1505);
        // embedded() and embedded_by_id(SUPERPOWER_ID) must agree exactly.
        assert_eq!(
            serde_json::to_string(&original).unwrap(),
            serde_json::to_string(&MaterialDataset::embedded().unwrap()).unwrap()
        );

        let lowfield = MaterialDataset::embedded_by_id(SUPERPOWER_LOWFIELD_ID).unwrap();
        assert_eq!(lowfield.metadata.id, SUPERPOWER_LOWFIELD_ID);
        assert_eq!(lowfield.points.len(), 3225);
        assert_eq!(lowfield.metadata.selection.nominal_field_t.len(), 15);

        let modelext = MaterialDataset::embedded_by_id(SUPERPOWER_MODELEXT_ID).unwrap();
        assert_eq!(modelext.metadata.id, SUPERPOWER_MODELEXT_ID);
        assert!(matches!(
            modelext.metadata.data_class,
            MaterialDataClass::MeasuredWithModelExtension
        ));
        // 3225 measured rows + 860 modeled rows (215 curves x 4 extended
        // field levels); modeled rows are the only points above 8 T.
        assert_eq!(modelext.points.len(), 4085);
        assert_eq!(
            modelext
                .points
                .iter()
                .filter(|p| p.nominal_field_t > 8.0)
                .count(),
            860
        );
        assert!(
            modelext
                .points
                .iter()
                .filter(|p| p.nominal_field_t > 8.0)
                .all(|p| p.source_row >= 900_000)
        );

        let shanghai = MaterialDataset::embedded_by_id(SHANGHAI_HFLT_ID).unwrap();
        assert_eq!(shanghai.metadata.id, SHANGHAI_HFLT_ID);
        assert_eq!(shanghai.points.len(), 1504);
        assert!(matches!(
            shanghai.metadata.data_class,
            MaterialDataClass::Measured
        ));
        // The same declared characterization window as the AP dataset —
        // a two-tape graded case never silently compares different regions.
        assert_eq!(
            shanghai.metadata.selection.nominal_temperature_k,
            original.metadata.selection.nominal_temperature_k
        );
        assert_eq!(
            shanghai.metadata.selection.nominal_field_t,
            original.metadata.selection.nominal_field_t
        );
        assert_eq!(
            shanghai.metadata.selection.nominal_angle_range_deg,
            original.metadata.selection.nominal_angle_range_deg
        );

        let theva = MaterialDataset::embedded_by_id(THEVA_AP_ID).unwrap();
        assert_eq!(theva.metadata.id, THEVA_AP_ID);
        assert_eq!(theva.points.len(), 1785);
        assert!(matches!(
            theva.metadata.data_class,
            MaterialDataClass::Measured
        ));
        // Same declared window again — the 105-135 deg wedge makes the
        // angle mesh denser inside it, not different.
        assert_eq!(
            theva.metadata.selection.nominal_temperature_k,
            original.metadata.selection.nominal_temperature_k
        );
        assert_eq!(
            theva.metadata.selection.nominal_field_t,
            original.metadata.selection.nominal_field_t
        );
        assert_eq!(
            theva.metadata.selection.nominal_angle_range_deg,
            original.metadata.selection.nominal_angle_range_deg
        );

        let ffj = MaterialDataset::embedded_by_id(FFJ_YBCO_ID).unwrap();
        assert_eq!(ffj.metadata.id, FFJ_YBCO_ID);
        assert_eq!(ffj.points.len(), 4845);
        assert!(matches!(
            ffj.metadata.data_class,
            MaterialDataClass::Measured
        ));
        // Same temperature/angle window as the peers, but its own denser
        // field grid — including the 0.01-0.7 T decade the others lack.
        assert_eq!(
            ffj.metadata.selection.nominal_temperature_k,
            original.metadata.selection.nominal_temperature_k
        );
        assert_eq!(
            ffj.metadata.selection.nominal_angle_range_deg,
            original.metadata.selection.nominal_angle_range_deg
        );
        assert_eq!(ffj.metadata.selection.nominal_field_t.len(), 19);
        assert_eq!(ffj.metadata.selection.nominal_field_t[0], 0.01);

        for id in [BABOUCHE_SP_ID, BABOUCHE_SST_ID] {
            let fit = MaterialDataset::embedded_by_id(id).unwrap();
            assert_eq!(fit.metadata.id, id);
            assert_eq!(fit.points.len(), 2093);
            assert!(matches!(
                fit.metadata.data_class,
                MaterialDataClass::PublishedModelFit
            ));
            // Model-fit rows only; nominal grid inside the published
            // parameter quad (T <= 35 K, B <= 16 T).
            assert!(fit.points.iter().all(|p| p.nominal_temperature_k <= 35.0
                && p.nominal_field_t <= 16.0
                && p.source_row >= 800_000));
        }

        assert!(MaterialDataset::embedded_by_id("robinson-superpower-ap-v3-nonexistent").is_err());
        assert!(MaterialDataset::embedded_by_id("").is_err());
    }

    /// `EMBEDDED_IDS` is the public enumeration clients use to discover
    /// datasets — every entry must resolve and report its own id, so a
    /// dataset added to `embedded_by_id` but not the list fails loudly
    /// here instead of becoming invisible.
    #[test]
    fn embedded_ids_enumerate_every_loadable_dataset() {
        for id in MaterialDataset::EMBEDDED_IDS {
            let ds = MaterialDataset::embedded_by_id(id)
                .unwrap_or_else(|_| panic!("EMBEDDED_IDS entry {id} does not load"));
            assert_eq!(&ds.metadata.id, id);
        }
        // And the reverse direction: the constant ids are exactly the
        // declared metadata ids — no alias resolves silently.
        assert_eq!(MaterialDataset::EMBEDDED_IDS.len(), 10);
    }

    /// The bundle is the customer-data path: one file carrying the same
    /// metadata + CSV pair, with the sha binding intact. A bundle whose
    /// csv_data no longer hashes to metadata.csv_sha256 must be rejected,
    /// as must any other schema id.
    #[test]
    fn bundle_round_trips_and_rejects_tampering() {
        let original = MaterialDataset::embedded_by_id(SUPERPOWER_ID).unwrap();
        let bundle = serde_json::json!({
            "schema": MATERIAL_DATASET_BUNDLE_SCHEMA,
            "metadata": serde_json::from_str::<serde_json::Value>(SUPERPOWER_METADATA).unwrap(),
            "csv_data": String::from_utf8_lossy(SUPERPOWER_CSV),
        })
        .to_string();
        let loaded = MaterialDataset::from_bundle_json(&bundle).unwrap();
        assert_eq!(loaded.metadata.id, original.metadata.id);
        assert_eq!(loaded.points.len(), original.points.len());
        assert_eq!(
            serde_json::to_string(&loaded).unwrap(),
            serde_json::to_string(&original).unwrap()
        );

        // A single altered measurement byte breaks the sha binding.
        let mut value: serde_json::Value = serde_json::from_str(&bundle).unwrap();
        let csv = value["csv_data"]
            .as_str()
            .unwrap()
            .replacen("ic_a_per_m", "ic_a_per_x", 1);
        value["csv_data"] = serde_json::Value::String(csv);
        assert!(MaterialDataset::from_bundle_json(&value.to_string()).is_err());

        let mut wrong_schema = serde_json::from_str::<serde_json::Value>(&bundle).unwrap();
        wrong_schema["schema"] = serde_json::Value::String("other/v9".into());
        assert!(MaterialDataset::from_bundle_json(&wrong_schema.to_string()).is_err());
    }

    /// The schema/class binding is what keeps the v1 identity honest:
    /// `measured_with_model_extension` is only legal under schema v2, so a
    /// modeled dataset cannot be relabeled into the original schema.
    #[test]
    fn schema_v1_rejects_the_model_extension_class() {
        let modelext = MaterialDataset::embedded_by_id(SUPERPOWER_MODELEXT_ID).unwrap();
        let mut metadata = modelext.metadata.clone();
        metadata.schema = MATERIAL_SCHEMA.into();
        assert!(metadata.validate().is_err());
        metadata.schema = MATERIAL_SCHEMA_V2.into();
        assert!(metadata.validate().is_ok());
    }

    #[test]
    fn lowfield_dataset_preserves_units_coordinates_and_provenance() {
        let data = MaterialDataset::embedded_by_id(SUPERPOWER_LOWFIELD_ID).unwrap();
        assert_eq!(data.points.len(), 3225);
        assert_eq!(data.metadata.measured_bridge_width_m, 0.001);
        assert_eq!(data.metadata.electric_field_criterion_v_per_m, 1e-4);
        assert_eq!(
            data.metadata.selection.nominal_field_t,
            vec![
                0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5, 0.7, 1.0, 1.5, 2.0, 3.0, 5.0, 7.0, 8.0
            ]
        );
        assert!(
            data.points
                .iter()
                .any(|p| p.temperature_k != p.nominal_temperature_k)
        );
        assert!(
            data.points
                .iter()
                .any(|p| (p.angle_from_normal_deg - p.nominal_angle_deg).abs() > 1.0)
        );
        assert!(data.points.iter().any(|p| p.applied_field_t < 1.0));
    }

    /// Every low-field-dataset row at Set (nominal) field >= 1 T must be
    /// byte-for-byte identical, field by field, to its corresponding row of
    /// the original embedded dataset -- checked directly against the parsed
    /// `MaterialPoint`s here (the preparation tool itself asserts the same
    /// thing against the raw CSV before ever writing the low-field files).
    #[test]
    fn embedded_lowfield_high_field_rows_are_identical_to_the_original_dataset() {
        let original = MaterialDataset::embedded_by_id(SUPERPOWER_ID).unwrap();
        let lowfield = MaterialDataset::embedded_by_id(SUPERPOWER_LOWFIELD_ID).unwrap();
        let original_fields: HashSet<_> = original
            .metadata
            .selection
            .nominal_field_t
            .iter()
            .map(|f| f.to_bits())
            .collect();
        let by_source_row: std::collections::HashMap<u32, &MaterialPoint> =
            original.points.iter().map(|p| (p.source_row, p)).collect();

        let mut checked = 0usize;
        for point in &lowfield.points {
            if !original_fields.contains(&point.nominal_field_t.to_bits()) {
                continue;
            }
            let expected = by_source_row.get(&point.source_row).unwrap_or_else(|| {
                panic!(
                    ">= 1 T source_row {} missing from original dataset",
                    point.source_row
                )
            });
            assert_eq!(point.source_row, expected.source_row);
            assert_eq!(point.nominal_temperature_k, expected.nominal_temperature_k);
            assert_eq!(point.nominal_field_t, expected.nominal_field_t);
            assert_eq!(point.nominal_angle_deg, expected.nominal_angle_deg);
            assert_eq!(point.temperature_k, expected.temperature_k);
            assert_eq!(point.applied_field_t, expected.applied_field_t);
            assert_eq!(point.angle_from_normal_deg, expected.angle_from_normal_deg);
            assert_eq!(point.ic_a_per_m, expected.ic_a_per_m);
            assert_eq!(point.bridge_ic_a, expected.bridge_ic_a);
            assert_eq!(point.n_value, expected.n_value);
            checked += 1;
        }
        assert_eq!(checked, original.points.len());
        assert_eq!(checked, 1505);
    }
}
