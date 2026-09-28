//! Prescribed-current magnetostatics cases, separate from allocation schema v1.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{DataClass, ModelError};

pub const FIELD_CASE_SCHEMA: &str = "optcoil-magnetostatics/v1";
pub const OC002_JSON: &str = include_str!("../../../benchmarks/synthetic/oc-002.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldCase {
    pub schema: String,
    pub id: String,
    pub data_class: DataClass,
    pub provenance: String,
    pub geometry: Racetrack,
    pub probes: Vec<FieldProbe>,
    pub acceptance: FieldAcceptance,
}

/// Canonical XY racetrack, centered at the origin. Straight runs are parallel
/// to X; the two semicircle centers are (+/- straight_half_length_m, 0, 0).
/// The cross section spans radial_width_m in the local outward direction and
/// axial_height_m along Z. Positive ampere-turns circulate CCW viewed from +Z.
/// This is a homogeneous winding pack, not a model of individual HTS tapes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Racetrack {
    pub straight_half_length_m: f64,
    pub bend_radius_m: f64,
    pub radial_width_m: f64,
    pub axial_height_m: f64,
    pub ampere_turns_a: f64,
    pub current_model: CurrentModel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentModel {
    UniformWindingPack,
}

/// Point value of the continuous volume-current field, including interior and
/// boundary locations. It is neither a tape-volume average nor a global peak.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldProbe {
    pub id: String,
    pub position_m: [f64; 3],
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldAcceptance {
    /// Fixed case scale: all errors are vector norms divided by this value.
    pub field_scale_t: f64,
    pub max_reference_error_fraction: f64,
    pub max_refinement_change_fraction: f64,
    pub max_reference_refinement_fraction: f64,
}

impl FieldCase {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let case: Self = crate::encoding::parse_case(json)?;
        case.validate()?;
        Ok(case)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let invalid = |message: &str| ModelError::Invalid(message.into());
        if self.schema != FIELD_CASE_SCHEMA || self.id.trim().is_empty() {
            return Err(invalid(
                "unsupported magnetic case schema or empty identity",
            ));
        }
        if self.provenance.trim().is_empty() {
            return Err(invalid("magnetic case requires provenance"));
        }
        self.geometry.validate()?;
        if self.probes.is_empty() || self.probes.len() > 256 {
            return Err(invalid("magnetic case requires 1..=256 probes"));
        }
        let mut ids = HashSet::new();
        for probe in &self.probes {
            if probe.id.trim().is_empty() || !ids.insert(&probe.id) {
                return Err(invalid("probe identities must be nonempty and unique"));
            }
            validate_position(probe.position_m)?;
        }
        let a = &self.acceptance;
        if !a.field_scale_t.is_finite() || a.field_scale_t <= 0.0 {
            return Err(invalid("field scale must be finite and positive"));
        }
        for tolerance in [
            a.max_reference_error_fraction,
            a.max_refinement_change_fraction,
            a.max_reference_refinement_fraction,
        ] {
            if !tolerance.is_finite() || !(0.0..1.0).contains(&tolerance) || tolerance == 0.0 {
                return Err(invalid(
                    "field tolerances must lie strictly between 0 and 1",
                ));
            }
            let absolute_tolerance = a.field_scale_t * tolerance;
            if !absolute_tolerance.is_finite() || absolute_tolerance <= 0.0 {
                return Err(invalid(
                    "absolute field tolerances must be representable and positive",
                ));
            }
        }
        if a.max_reference_refinement_fraction >= a.max_reference_error_fraction {
            return Err(invalid(
                "reference refinement tolerance must be tighter than comparison tolerance",
            ));
        }
        Ok(())
    }
}

impl Racetrack {
    pub fn validate(&self) -> Result<(), ModelError> {
        // Numerical support bounds, not engineering or material design limits.
        let positive = [self.bend_radius_m, self.radial_width_m, self.axial_height_m];
        if positive
            .iter()
            .any(|x| !x.is_finite() || !(1e-9..=1e6).contains(x))
            || !self.straight_half_length_m.is_finite()
            || !(0.0..=1e6).contains(&self.straight_half_length_m)
            || !self.ampere_turns_a.is_finite()
            || self.ampere_turns_a.abs() > 1e12
        {
            return Err(ModelError::Invalid(
                "racetrack dimensions/current exceed the documented numerical domain".into(),
            ));
        }
        if self.bend_radius_m - self.radial_width_m / 2.0 < 1e-9 {
            return Err(ModelError::Invalid(
                "racetrack inner bend radius must be at least 1e-9 m".into(),
            ));
        }
        Ok(())
    }
}

pub fn validate_position(position_m: [f64; 3]) -> Result<(), ModelError> {
    if position_m.iter().any(|x| !x.is_finite() || x.abs() > 1e6) {
        return Err(ModelError::Invalid(
            "probe coordinates must be finite and within +/- 1e6 m".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_degenerate_geometry_ambiguous_probes_and_bad_gates() {
        let mut case = FieldCase::from_json(OC002_JSON).unwrap();
        case.geometry.bend_radius_m = case.geometry.radial_width_m / 2.0;
        assert!(case.validate().is_err());
        let mut case = FieldCase::from_json(OC002_JSON).unwrap();
        case.probes[1].id = case.probes[0].id.clone();
        assert!(case.validate().is_err());
        let mut case = FieldCase::from_json(OC002_JSON).unwrap();
        case.acceptance.field_scale_t = 0.0;
        assert!(case.validate().is_err());
        let mut case = FieldCase::from_json(OC002_JSON).unwrap();
        case.geometry.ampere_turns_a = f64::NAN;
        assert!(case.validate().is_err());
        assert!(
            FieldCase::from_json(&OC002_JSON.replace("uniform_winding_pack", "hts_redistribution"))
                .is_err()
        );
    }
}
