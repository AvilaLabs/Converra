//! Declared sensitivity-sweep specifications (`optcoil-sensitivity/v1`).
//!
//! A sweep is a full-factorial set of declared perturbations applied to a
//! coupled-search case: each axis lists the values it takes, and every
//! combination is run as its own search. The sweep never claims a
//! distribution or probability — each point is one re-run of the same
//! screening model under one declared change, hash-bound like any other
//! case.

use serde::{Deserialize, Serialize};

use crate::ModelError;

pub const SENSITIVITY_SPEC_SCHEMA: &str = "optcoil-sensitivity/v1";
/// v2 adds the `requirement_b_target_t` axis; a v1 spec still parses and
/// validates under its own schema string.
pub const SENSITIVITY_SPEC_SCHEMA_V2: &str = "optcoil-sensitivity/v2";
/// v3 adds the `utilization_limit` axis — the capacity-margin frontier,
/// "what does headroom cost?" swept over the declared utilization gate.
/// v1/v2 specs still parse and validate under their own schema strings.
pub const SENSITIVITY_SPEC_SCHEMA_V3: &str = "optcoil-sensitivity/v3";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensitivitySpec {
    pub schema: String,
    pub id: String,
    /// Who declared this sweep and why — the axes are engineering
    /// questions ("does the optimum survive a −10% tape lot?"), and the
    /// provenance line is where that intent lives.
    pub provenance: String,
    /// Full-factorial axes: 1..=3 entries, kinds unique.
    pub axes: Vec<SensitivityAxis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SensitivityAxis {
    /// Multiplies every measured Ic in the case's material dataset
    /// (`ic_a_per_m` and `bridge_ic_a`) — tape-lot quality and
    /// measurement uncertainty. The scaled dataset is a distinct
    /// identity (`MaterialDataset::scaled_ic`): a declared synthetic
    /// perturbation, never presented as measured data.
    IcScale { values: Vec<f64> },
    /// Replaces `operating.temperature_k`. Each value must stay strictly
    /// interior to the dataset's nominal temperature span — checked at
    /// run time, since the spec cannot see the dataset.
    TemperatureK { values: Vec<f64> },
    /// Replaces `cost.price_usd_per_m` — price-band sensitivity.
    /// Positive finite values only.
    PriceUsdPerM { values: Vec<f64> },
    /// Replaces `requirement.b_target_t` (spec v2) — the certified-ceiling
    /// question, "is this winding verifiably holdable at this field?",
    /// asked at each declared value. The pass/fail response in target
    /// field is *not* monotone (low-field dataset floors can leave low
    /// targets INCONCLUSIVE while higher targets pass), so the answer is
    /// read off the declared grid — largest passing value — never
    /// bisected. Positive finite values only.
    RequirementBTargetT { values: Vec<f64> },
    /// Replaces `limits.utilization_limit` (spec v3) — the
    /// capacity-margin frontier: "what does headroom cost?" asked by
    /// sweeping the declared utilization gate. Each value must lie
    /// strictly within (0, 1) — the case-level bound the axis perturbs.
    /// The frontier need not be smooth: a tighter gate can remove the
    /// cheapest passing geometry outright, so cost can step rather than
    /// slope.
    UtilizationLimit { values: Vec<f64> },
}

impl SensitivityAxis {
    /// The serde tag: the record's per-point axis key.
    pub fn kind_id(&self) -> &'static str {
        match self {
            Self::IcScale { .. } => "ic_scale",
            Self::TemperatureK { .. } => "temperature_k",
            Self::PriceUsdPerM { .. } => "price_usd_per_m",
            Self::RequirementBTargetT { .. } => "requirement_b_target_t",
            Self::UtilizationLimit { .. } => "utilization_limit",
        }
    }

    pub fn values(&self) -> &[f64] {
        match self {
            Self::IcScale { values }
            | Self::TemperatureK { values }
            | Self::PriceUsdPerM { values }
            | Self::RequirementBTargetT { values }
            | Self::UtilizationLimit { values } => values,
        }
    }

    fn validate(&self) -> Result<(), ModelError> {
        require(
            (1..=16).contains(&self.values().len()),
            "sensitivity axis must list 1..=16 values",
        )?;
        require(
            self.values().iter().all(|v| v.is_finite()),
            "sensitivity axis values must be finite",
        )?;
        match self {
            Self::IcScale { values } => require(
                values.iter().all(|v| *v > 0.0 && *v <= 4.0),
                "ic_scale values must be within (0, 4]",
            ),
            Self::TemperatureK { values } => require(
                values.iter().all(|v| *v > 0.0 && *v < 500.0),
                "temperature_k values must be within (0, 500) K",
            ),
            Self::PriceUsdPerM { values } => require(
                values.iter().all(|v| *v > 0.0),
                "price_usd_per_m values must be positive",
            ),
            Self::RequirementBTargetT { values } => require(
                values.iter().all(|v| *v > 0.0 && *v < 1.0e3),
                "requirement_b_target_t values must be within (0, 1000) T",
            ),
            Self::UtilizationLimit { values } => require(
                values.iter().all(|v| *v > 0.0 && *v < 1.0),
                "utilization_limit values must lie strictly within (0, 1)",
            ),
        }
    }
}

impl SensitivitySpec {
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let spec: Self = crate::encoding::parse_case(json)?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        require(
            self.schema == SENSITIVITY_SPEC_SCHEMA
                || self.schema == SENSITIVITY_SPEC_SCHEMA_V2
                || self.schema == SENSITIVITY_SPEC_SCHEMA_V3,
            "unsupported sensitivity spec schema",
        )?;
        if self.schema == SENSITIVITY_SPEC_SCHEMA {
            require(
                self.axes
                    .iter()
                    .all(|a| !matches!(a, SensitivityAxis::RequirementBTargetT { .. })),
                "requirement_b_target_t axes require optcoil-sensitivity/v2",
            )?;
        }
        if self.schema != SENSITIVITY_SPEC_SCHEMA_V3 {
            require(
                self.axes
                    .iter()
                    .all(|a| !matches!(a, SensitivityAxis::UtilizationLimit { .. })),
                "utilization_limit axes require optcoil-sensitivity/v3",
            )?;
        }
        require(!self.id.is_empty(), "sensitivity spec id must not be empty")?;
        require(
            (1..=3).contains(&self.axes.len()),
            "sensitivity spec must declare 1..=3 axes",
        )?;
        let mut kinds: Vec<&'static str> = self.axes.iter().map(|a| a.kind_id()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        require(
            kinds.len() == self.axes.len(),
            "sensitivity axes must declare distinct kinds",
        )?;
        for axis in &self.axes {
            axis.validate()?;
        }
        Ok(())
    }

    /// Total full-factorial combinations.
    pub fn combination_count(&self) -> usize {
        self.axes.iter().map(|a| a.values().len()).product()
    }
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

    fn spec_json(axes: &str) -> String {
        format!(
            r#"{{"schema": "{SENSITIVITY_SPEC_SCHEMA}", "id": "test-sweep",
            "provenance": "unit test fixture", "axes": [{axes}]}}"#
        )
    }

    #[test]
    fn spec_validates_axes() {
        let spec = SensitivitySpec::from_json(&spec_json(
            r#"{"kind": "ic_scale", "values": [0.9, 1.0, 1.1]},
               {"kind": "price_usd_per_m", "values": [30.0, 62.5]}"#,
        ))
        .unwrap();
        assert_eq!(spec.combination_count(), 6);

        // Duplicate kinds are rejected — a second axis of the same kind
        // would make per-point results ambiguous.
        assert!(
            SensitivitySpec::from_json(&spec_json(
                r#"{"kind": "ic_scale", "values": [1.0]},
                   {"kind": "ic_scale", "values": [0.5]}"#
            ))
            .is_err()
        );
        assert!(
            SensitivitySpec::from_json(&spec_json(r#"{"kind": "ic_scale", "values": [0.0]}"#))
                .is_err()
        );
        assert!(
            SensitivitySpec::from_json(&spec_json(
                r#"{"kind": "price_usd_per_m", "values": [-30.0]}"#
            ))
            .is_err()
        );
        assert!(SensitivitySpec::from_json(&spec_json("")).is_err());
        assert!(
            SensitivitySpec::from_json(&spec_json(r#"{"kind": "ic_scale", "values": []}"#))
                .is_err()
        );
    }

    #[test]
    fn requirement_b_target_axis_requires_v2_and_validates() {
        let axis = r#"{"kind": "requirement_b_target_t", "values": [4.0, 5.8]}"#;
        // v2 accepts it; v1 rejects the kind outright.
        let v2 = SensitivitySpec::from_json(&format!(
            r#"{{"schema": "{SENSITIVITY_SPEC_SCHEMA_V2}", "id": "t",
                "provenance": "test", "axes": [{axis}]}}"#
        ))
        .unwrap();
        assert_eq!(v2.axes[0].kind_id(), "requirement_b_target_t");
        assert!(spec_json(axis).contains("v1"));
        assert!(
            SensitivitySpec::from_json(&spec_json(axis)).is_err(),
            "v1 spec must reject requirement_b_target_t"
        );
        // Non-positive and non-finite targets are rejected under v2.
        for bad in ["[0.0]", "[-1.0]", "[1.0e3]"] {
            let spec = format!(
                r#"{{"schema": "{SENSITIVITY_SPEC_SCHEMA_V2}", "id": "t",
                    "provenance": "test", "axes":
                    [{{"kind": "requirement_b_target_t", "values": {bad}}}]}}"#
            );
            assert!(SensitivitySpec::from_json(&spec).is_err(), "{bad}");
        }
    }

    #[test]
    fn utilization_limit_axis_requires_v3_and_validates() {
        let axis = r#"{"kind": "utilization_limit", "values": [0.6, 0.8, 0.95]}"#;
        // v3 accepts it; v1 and v2 reject the kind outright.
        let v3 = SensitivitySpec::from_json(&format!(
            r#"{{"schema": "{SENSITIVITY_SPEC_SCHEMA_V3}", "id": "t",
                "provenance": "test", "axes": [{axis}]}}"#
        ))
        .unwrap();
        assert_eq!(v3.axes[0].kind_id(), "utilization_limit");
        for schema in [SENSITIVITY_SPEC_SCHEMA, SENSITIVITY_SPEC_SCHEMA_V2] {
            let spec = format!(
                r#"{{"schema": "{schema}", "id": "t",
                    "provenance": "test", "axes": [{axis}]}}"#
            );
            assert!(
                SensitivitySpec::from_json(&spec).is_err(),
                "{schema} spec must reject utilization_limit"
            );
        }
        // The case-level bound applies: strictly within (0, 1).
        for bad in ["[0.0]", "[1.0]", "[-0.5]", "[1.5]"] {
            let spec = format!(
                r#"{{"schema": "{SENSITIVITY_SPEC_SCHEMA_V3}", "id": "t",
                    "provenance": "test", "axes":
                    [{{"kind": "utilization_limit", "values": {bad}}}]}}"#
            );
            assert!(SensitivitySpec::from_json(&spec).is_err(), "{bad}");
        }
    }
}
