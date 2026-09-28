//! Integration seam only. No commercial solver connector is implemented yet.
//! The OC-011 kernel cross-check (`kernel_crosscheck`) is the exception:
//! it asks an external implementation for fields at frozen probes, never
//! for a candidate verdict.

pub mod field_map;
pub mod kernel_crosscheck;

use optcoil_model::{Candidate, Case, Check};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Serialize)]
pub struct AdapterDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub integration_surface: &'static str,
    pub availability: Availability,
    pub supports_derivatives: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Planned,
}

/// The request identity must include candidate, case, solver model, settings and version.
/// A concrete connector must also validate geometry/material/current-direction mappings.
pub struct SolverRequest<'a> {
    pub case: &'a Case,
    pub candidate: &'a Candidate,
    pub request_sha256: &'a str,
}

#[derive(Debug, Serialize)]
pub struct SolverEvidence {
    pub request_sha256: String,
    pub solver_name: String,
    pub solver_version: String,
    pub model_identity: String,
    pub checks: Vec<Check>,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("{0} adapter is planned, not implemented; no solver was run")]
    NotImplemented(&'static str),
    /// The external implementation or its runner is absent — callers map
    /// this to NOT_EVALUATED, never a silent skip.
    #[error("external implementation not available: {0}")]
    NotAvailable(String),
    /// The implementation ran but failed.
    #[error("external solver failed: {0}")]
    SolverFailed(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Blocking execution belongs on a worker/process, never the egui event thread.
pub trait SolverAdapter: Send + Sync {
    fn descriptor(&self) -> AdapterDescriptor;
    fn evaluate(&self, request: &SolverRequest<'_>) -> Result<SolverEvidence, AdapterError>;
}

#[derive(Debug, Clone, Copy)]
pub enum PlannedAdapter {
    Allsolve,
    Comsol,
    Ansys,
}

pub const PLANNED_ADAPTERS: [PlannedAdapter; 3] = [
    PlannedAdapter::Allsolve,
    PlannedAdapter::Comsol,
    PlannedAdapter::Ansys,
];

impl SolverAdapter for PlannedAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        let (id, name, integration_surface) = match self {
            Self::Allsolve => ("allsolve", "Quanscient Allsolve", "Python SDK / HTTP API"),
            Self::Comsol => ("comsol", "COMSOL Multiphysics", "Java API / batch model"),
            Self::Ansys => (
                "ansys",
                "Ansys Mechanical",
                "PyMechanical; physics coverage to be selected",
            ),
        };
        AdapterDescriptor {
            id,
            name,
            integration_surface,
            availability: Availability::Planned,
            supports_derivatives: false,
        }
    }

    fn evaluate(&self, _request: &SolverRequest<'_>) -> Result<SolverEvidence, AdapterError> {
        Err(AdapterError::NotImplemented(self.descriptor().name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_connectors_cannot_return_successful_evidence() {
        let case = Case::demo().unwrap();
        let request = SolverRequest {
            case: &case,
            candidate: &case.baseline,
            request_sha256: "fixture",
        };
        for adapter in PLANNED_ADAPTERS {
            assert!(matches!(
                adapter.evaluate(&request),
                Err(AdapterError::NotImplemented(_))
            ));
        }
    }
}
