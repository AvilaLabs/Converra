//! Synthetic reel inventory generator specification and truth file (CR-02).
//! The generator itself lives in `optcoil-search::synthetic`.

use serde::{Deserialize, Serialize};

use crate::{ModelError, reel::ProductMapRef};

pub const SYNTHETIC_SPEC_SCHEMA: &str = "optcoil-synthetic-inventory-spec/v1";
pub const SYNTHETIC_TRUTH_SCHEMA: &str = "optcoil-synthetic-truth/v1";

pub const DEFAULT_LOT_RELATIVE_SD: f64 = 0.11;
pub const DEFAULT_TRANSFER_LOG_SD: f64 = 0.15;
pub const MAX_REEL_COUNT: u32 = 10_000;
/// Total profile points an inventory may hold, so the generated document
/// stays within the readers' size limits.
pub const MAX_TOTAL_POINTS: u64 = 1_000_000;
const MAX_SPEC_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AbOffsetGroup {
    #[serde(rename = "cheng2025_F1")]
    Cheng2025F1,
    #[serde(rename = "cheng2025_H1")]
    Cheng2025H1,
    #[serde(rename = "cheng2025_HX")]
    Cheng2025Hx,
}

/// Published ab-plane offset statistics (Cheng et al. 2025, Table 2), degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbOffsetStats {
    pub mean_deg: f64,
    pub sd_deg: f64,
    pub ci95_deg: f64,
}

impl AbOffsetGroup {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cheng2025F1 => "cheng2025_F1",
            Self::Cheng2025H1 => "cheng2025_H1",
            Self::Cheng2025Hx => "cheng2025_HX",
        }
    }

    pub fn stats(self) -> AbOffsetStats {
        let (mean_deg, sd_deg, ci95_deg) = match self {
            Self::Cheng2025F1 => (-1.81, 0.53, 0.49),
            Self::Cheng2025H1 => (3.25, 0.11, 0.11),
            Self::Cheng2025Hx => (-1.09, 0.58, 0.94),
        };
        AbOffsetStats {
            mean_deg,
            sd_deg,
            ci95_deg,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticProduct {
    pub vendor: String,
    pub product: String,
    pub product_map: ProductMapRef,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReelLengthRange {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticProfile {
    pub temperature_k: f64,
    pub field_t: f64,
    pub angle_from_normal_deg: f64,
    pub electric_field_criterion_v_per_m: f64,
    pub resolution_m: f64,
    pub map_reference_row: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticSpec {
    pub schema: String,
    pub inventory_id: String,
    pub seed: u64,
    pub product: SyntheticProduct,
    pub reel_count: u32,
    pub reel_length_m: ReelLengthRange,
    pub width_m: f64,
    pub profile: SyntheticProfile,
    #[serde(default)]
    pub lot_relative_sd: Option<f64>,
    pub length_relative_sd: f64,
    pub length_correlation_m: f64,
    #[serde(default)]
    pub transfer_log_sd: Option<f64>,
    #[serde(default)]
    pub ab_offset_group: Option<AbOffsetGroup>,
}

impl SyntheticSpec {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        if json.len() > MAX_SPEC_BYTES {
            return Err(invalid("synthetic inventory spec exceeds 1 MiB"));
        }
        let spec: Self = serde_json::from_str(json)?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn lot_sd(&self) -> f64 {
        self.lot_relative_sd.unwrap_or(DEFAULT_LOT_RELATIVE_SD)
    }

    pub fn transfer_sd(&self) -> f64 {
        self.transfer_log_sd.unwrap_or(DEFAULT_TRANSFER_LOG_SD)
    }

    /// Shape validation; the product map and row are checked at generation.
    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == SYNTHETIC_SPEC_SCHEMA,
            &format!("unsupported synthetic spec schema (expected {SYNTHETIC_SPEC_SCHEMA})"),
        )?;
        require(
            !self.inventory_id.trim().is_empty(),
            "inventory_id must be non-empty",
        )?;
        require(
            !self.product.vendor.trim().is_empty() && !self.product.product.trim().is_empty(),
            "product vendor and product must be non-empty",
        )?;
        let map = &self.product.product_map;
        require(
            !map.dataset_id.trim().is_empty()
                && map.csv_sha256.len() == 64
                && map
                    .csv_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "product_map needs a dataset_id and a lowercase hex csv_sha256",
        )?;
        require(
            (1..=MAX_REEL_COUNT).contains(&self.reel_count),
            "reel_count must be within 1..=10000",
        )?;
        let range = self.reel_length_m;
        require(
            range.min.is_finite()
                && range.max.is_finite()
                && range.min > 0.0
                && range.min <= range.max,
            "reel_length_m needs 0 < min <= max",
        )?;
        positive(self.width_m, "width_m")?;
        let p = self.profile;
        positive(p.temperature_k, "profile.temperature_k")?;
        require(
            p.field_t.is_finite() && p.field_t >= 0.0,
            "profile.field_t must be finite and nonnegative",
        )?;
        require(
            p.angle_from_normal_deg.is_finite(),
            "profile.angle_from_normal_deg must be finite",
        )?;
        positive(
            p.electric_field_criterion_v_per_m,
            "profile.electric_field_criterion_v_per_m",
        )?;
        positive(p.resolution_m, "profile.resolution_m")?;
        require(
            p.map_reference_row >= 1,
            "profile.map_reference_row must be at least 1",
        )?;
        let points_per_reel = (range.max / p.resolution_m).floor() + 2.0;
        require(
            points_per_reel * f64::from(self.reel_count) <= MAX_TOTAL_POINTS as f64,
            "reel_count x profile points would exceed 1000000 total points; raise resolution_m or lower reel_count",
        )?;
        for (value, name) in [
            (self.lot_relative_sd, "lot_relative_sd"),
            (Some(self.length_relative_sd), "length_relative_sd"),
            (self.transfer_log_sd, "transfer_log_sd"),
        ] {
            if let Some(v) = value {
                require(
                    v.is_finite() && v >= 0.0,
                    &format!("{name} must be finite and nonnegative"),
                )?;
            }
        }
        positive(self.length_correlation_m, "length_correlation_m")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TruthReel {
    pub reel_id: String,
    pub lot_factor: f64,
    /// Hidden factor between the scaled rating and the true operating-point
    /// Ic: true Ic(x) = scaled rating(x) x transfer_factor.
    pub transfer_factor: f64,
    pub ab_offset_deg: Option<f64>,
}

/// What the generator drew and the rating must never see.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticTruth {
    pub schema: String,
    pub inventory_id: String,
    /// SHA-256 of the exact generated inventory bytes.
    pub inventory_sha256: String,
    pub spec_sha256: String,
    pub seed: u64,
    pub reels: Vec<TruthReel>,
    pub limitations: Vec<String>,
}

fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

fn require(condition: bool, message: &str) -> Result<(), ModelError> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn positive(value: f64, name: &str) -> Result<(), ModelError> {
    require(
        value.is_finite() && value > 0.0,
        &format!("{name} must be finite and positive"),
    )
}
