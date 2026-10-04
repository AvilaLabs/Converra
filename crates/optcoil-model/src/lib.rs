//! Canonical schema. Every dimensional scalar names its unit; money is USD.

pub mod attestation;
pub mod bakeoff;
pub mod coupled;
pub mod coupled_search;
pub mod dataset_intake;
mod encoding;
pub mod magnetics;
pub mod material;
pub mod path;
pub mod path3d;
pub mod product;
pub mod reel;
pub mod sensitivity;
pub mod synthetic;
pub mod tabular_import;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEMO_JSON: &str = include_str!("../../../benchmarks/synthetic/oc-001.json");

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("invalid case: {0}")]
    Invalid(String),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    Synthetic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub schema_version: u32,
    pub id: String,
    pub description: String,
    pub data_class: DataClass,
    pub provenance: String,
    pub circuit_current_a: f64,
    pub utilization_limit: f64,
    /// Extra purchased length / installed length, rather than a yield fraction.
    pub scrap_fraction: f64,
    pub assembly_cost_per_module_usd: f64,
    pub grades: Vec<Grade>,
    /// Ordered, separately wound modules in one series circuit.
    pub modules: Vec<Module>,
    pub interfaces: Vec<Interface>,
    pub baseline: Candidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    pub id: String,
    pub price_usd_per_m: f64,
    pub ic_at_zero_field_a: f64,
    pub field_derating_per_t: f64,
    pub domain: MaterialDomain,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDomain {
    pub min_temperature_k: f64,
    pub max_temperature_k: f64,
    pub max_field_t: f64,
    /// Angle between the prescribed field and the tape surface, in degrees.
    pub min_angle_deg: f64,
    pub max_angle_deg: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatingPoint {
    pub temperature_k: f64,
    pub field_t: f64,
    pub field_angle_deg: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub id: String,
    pub turns: u32,
    pub mean_turn_length_m: f64,
    pub max_tapes: u32,
    pub allowed_grade_ids: Vec<String>,
    /// Prescribed input; this scaffold does not solve the coil's field.
    pub operating_point: OperatingPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub left_module_id: String,
    pub right_module_id: String,
    pub allow_allocation_change: bool,
    pub fixed_cost_usd: f64,
    /// Additional manufacturing charge if tape count OR grade changes.
    pub change_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub module_id: String,
    pub grade_id: String,
    pub tapes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Same order and identities as Case::modules, with one entry per module.
    pub allocations: Vec<Allocation>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Fail,
    Inconclusive,
    NotEvaluated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CostBreakdown {
    pub installed_conductor_usd: f64,
    pub scrap_usd: f64,
    pub assembly_usd: f64,
    pub joints_usd: f64,
    pub total_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleResult {
    pub module_id: String,
    pub installed_length_m: f64,
    pub purchased_length_m: f64,
    pub allowed_current_a: Option<f64>,
    pub current_margin_a: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub candidate: Candidate,
    pub screening_status: Status,
    pub engineering_status: Status,
    pub cost: CostBreakdown,
    pub modules: Vec<ModuleResult>,
    pub checks: Vec<Check>,
}

impl Case {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let case: Self = encoding::parse_case(json)?;
        case.validate()?;
        Ok(case)
    }

    pub fn demo() -> Result<Self, ModelError> {
        Self::from_json(DEMO_JSON)
    }

    pub fn grade(&self, id: &str) -> Option<&Grade> {
        self.grades.iter().find(|grade| grade.id == id)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema_version == SCHEMA_VERSION,
            "unsupported schema version",
        )?;
        require(!self.id.trim().is_empty(), "case id is empty")?;
        require(
            !self.provenance.trim().is_empty(),
            "case provenance is empty",
        )?;
        positive(self.circuit_current_a, "circuit_current_a")?;
        positive(self.utilization_limit, "utilization_limit")?;
        require(
            self.utilization_limit <= 1.0,
            "utilization_limit must be <= 1",
        )?;
        nonnegative(self.scrap_fraction, "scrap_fraction")?;
        require(self.scrap_fraction <= 1.0, "scrap_fraction must be <= 1")?;
        nonnegative(self.assembly_cost_per_module_usd, "assembly cost")?;
        require(
            (1..=32).contains(&self.modules.len()),
            "expected 1..=32 modules",
        )?;
        require(
            (1..=32).contains(&self.grades.len()),
            "expected 1..=32 grades",
        )?;
        unique_ids(self.grades.iter().map(|g| g.id.as_str()), "grade")?;
        unique_ids(self.modules.iter().map(|m| m.id.as_str()), "module")?;

        for grade in &self.grades {
            positive(grade.price_usd_per_m, "grade price")?;
            positive(grade.ic_at_zero_field_a, "grade current")?;
            nonnegative(grade.field_derating_per_t, "field derating")?;
            require(
                !grade.provenance.trim().is_empty(),
                "grade provenance is empty",
            )?;
            let d = &grade.domain;
            positive(d.min_temperature_k, "minimum temperature")?;
            positive(d.max_temperature_k, "maximum temperature")?;
            require(
                d.min_temperature_k <= d.max_temperature_k,
                "reversed temperature domain",
            )?;
            nonnegative(d.max_field_t, "maximum field")?;
            require(
                (0.0..=180.0).contains(&d.min_angle_deg)
                    && (d.min_angle_deg..=180.0).contains(&d.max_angle_deg),
                "invalid angle domain",
            )?;
            require(
                grade.field_derating_per_t * d.max_field_t < 1.0,
                "synthetic current envelope must remain positive throughout its domain",
            )?;
            require(
                (grade.ic_at_zero_field_a * 64.0).is_finite(),
                "current envelope overflow",
            )?;
        }

        let mut max_cost = self.assembly_cost_per_module_usd * self.modules.len() as f64;
        for module in &self.modules {
            require(module.turns > 0, "turn count must be positive")?;
            positive(module.mean_turn_length_m, "mean turn length")?;
            require(
                (1..=64).contains(&module.max_tapes),
                "expected 1..=64 tapes per module",
            )?;
            require(
                !module.allowed_grade_ids.is_empty(),
                "module must allow at least one grade",
            )?;
            unique_ids(
                module.allowed_grade_ids.iter().map(String::as_str),
                "allowed grade",
            )?;
            let mut max_price: f64 = 0.0;
            for id in &module.allowed_grade_ids {
                let grade = self
                    .grade(id)
                    .ok_or_else(|| ModelError::Invalid(format!("unknown grade {id}")))?;
                max_price = max_price.max(grade.price_usd_per_m);
            }
            let point = &module.operating_point;
            positive(point.temperature_k, "operating temperature")?;
            nonnegative(point.field_t, "operating field")?;
            require(
                (0.0..=180.0).contains(&point.field_angle_deg),
                "invalid operating angle",
            )?;
            let max_length = f64::from(module.turns)
                * module.mean_turn_length_m
                * f64::from(module.max_tapes)
                * (1.0 + self.scrap_fraction);
            positive(max_length, "purchased tape length")?;
            max_cost += max_length * max_price;
        }
        require(
            self.interfaces.len() == self.modules.len() - 1,
            "one interface required between each adjacent pair of modules",
        )?;
        for (i, interface) in self.interfaces.iter().enumerate() {
            require(
                interface.left_module_id == self.modules[i].id
                    && interface.right_module_id == self.modules[i + 1].id,
                "interface identities/order must match the series circuit",
            )?;
            nonnegative(interface.fixed_cost_usd, "fixed interface cost")?;
            nonnegative(interface.change_cost_usd, "interface change cost")?;
            max_cost += interface.fixed_cost_usd + interface.change_cost_usd;
        }
        positive(max_cost, "maximum total cost")?;
        self.validate_candidate(&self.baseline)
    }

    /// Structural validation; electrical and interface constraints are assessed separately.
    pub fn validate_candidate(&self, candidate: &Candidate) -> Result<(), ModelError> {
        require(
            candidate.allocations.len() == self.modules.len(),
            "allocation count must match module count",
        )?;
        for (module, allocation) in self.modules.iter().zip(&candidate.allocations) {
            require(
                allocation.module_id == module.id,
                "allocation identities/order must match modules",
            )?;
            require(
                module.allowed_grade_ids.contains(&allocation.grade_id),
                "allocation uses a disallowed grade",
            )?;
            require(
                (1..=module.max_tapes).contains(&allocation.tapes),
                "allocation tape count outside manufacturing bounds",
            )?;
        }
        Ok(())
    }
}

pub fn allocation_changed(left: &Allocation, right: &Allocation) -> bool {
    left.grade_id != right.grade_id || left.tapes != right.tapes
}

fn require(ok: bool, detail: &str) -> Result<(), ModelError> {
    if ok {
        Ok(())
    } else {
        Err(ModelError::Invalid(detail.to_owned()))
    }
}

fn positive(value: f64, name: &str) -> Result<(), ModelError> {
    require(
        value.is_finite() && value > 0.0,
        &format!("{name} must be finite and positive"),
    )
}

fn nonnegative(value: f64, name: &str) -> Result<(), ModelError> {
    require(
        value.is_finite() && value >= 0.0,
        &format!("{name} must be finite and nonnegative"),
    )
}

fn unique_ids<'a>(ids: impl Iterator<Item = &'a str>, kind: &str) -> Result<(), ModelError> {
    let mut seen = HashSet::new();
    for id in ids {
        require(
            !id.trim().is_empty() && seen.insert(id),
            &format!("empty or duplicate {kind} identity"),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_or_nonfinite_inputs() {
        let mut case = Case::demo().unwrap();
        case.modules[1].id = case.modules[0].id.clone();
        assert!(case.validate().is_err());
        let mut case = Case::demo().unwrap();
        case.grades[0].price_usd_per_m = f64::NAN;
        assert!(case.validate().is_err());
        assert!(
            Case::from_json(&DEMO_JSON.replace("\"schema_version\": 1", "\"schema_version\": 99"))
                .is_err()
        );
        assert!(
            Case::from_json(&DEMO_JSON.replace("\"scrap_fraction\"", "\"scrap_fractoin\""))
                .is_err()
        );
    }

    #[test]
    fn rejects_reordered_allocations_and_invalid_connectivity() {
        let mut case = Case::demo().unwrap();
        case.baseline.allocations.swap(0, 1);
        assert!(case.validate().is_err());
        let mut case = Case::demo().unwrap();
        case.interfaces[0].right_module_id = "high-field".into();
        assert!(case.validate().is_err());
    }
}
