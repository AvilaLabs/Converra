//! OC-011 kernel cross-check: evaluate a frozen probe deck through our
//! own `RacetrackEvaluator` *and* an independently implemented
//! magnetostatics (today: Bluemira via `optcoil-adapters`), then compare
//! under the deck's predeclared tolerance. The verdict is OptCoil's own
//! PASS/FAIL/INCONCLUSIVE/NOT_EVALUATED — agreement is evidence, not
//! acceptance.
//!
//! Headless operation, per the crate's role: callers get a record; the
//! CLI prints it; nothing here prints or exits.

use optcoil_adapters::AdapterError;
use optcoil_adapters::kernel_crosscheck::{
    self, KERNEL_PROBE_DECK_SCHEMA, KernelCrosscheck, KernelCrosscheckVerdict, KernelProbeRequest,
};
use optcoil_model::magnetics::{CurrentModel, Racetrack};
use optcoil_physics::racetrack::RacetrackEvaluator;
use serde::Serialize;

use crate::RunError;

/// Thin-filament mode evaluates the pack's centerline filament through
/// `optcoil_physics::filament` — the honest thin-filament model, not a
/// shrunken finite cross-section (the cell evaluator's 4096-cell budget
/// forbids the epsilon trick, and correctly so).
///
/// The full cross-check record: the frozen request, both sides'
/// answers, the verdict, and (when it ran) the external evidence
/// binding hashes.
#[derive(Debug, Serialize)]
pub struct KernelCrosscheckRunRecord {
    pub schema: &'static str,
    pub request: KernelProbeRequest,
    /// Our kernel's answers at the same probes (always computed — the
    /// NOT_EVALUATED path still records what *would* be checked).
    pub b_rust_t: Vec<[f64; 3]>,
    pub verdict: KernelCrosscheckVerdict,
    /// Present iff the external implementation actually ran.
    pub evidence: Option<kernel_crosscheck::KernelEvidence>,
}

pub const KERNEL_CROSSCHECK_RECORD_SCHEMA: &str = "optcoil-kernel-crosscheck-run/v1";

/// Run a deck end to end: our evaluator on the declared model, then the
/// external crosscheck. `crosscheck` is injected so tests can use a
/// stub; production callers pass `&BluemiraCrosscheck::default()`.
pub fn run_kernel_crosscheck(
    deck_json: &str,
    crosscheck: &dyn KernelCrosscheck,
    quadrature_order: u32,
) -> Result<KernelCrosscheckRunRecord, RunError> {
    let request: KernelProbeRequest = serde_json::from_str(deck_json)
        .map_err(|e| RunError::Invalid(format!("kernel probe deck parse: {e}")))?;
    if request.schema != KERNEL_PROBE_DECK_SCHEMA {
        return Err(RunError::Invalid(format!(
            "unsupported kernel probe deck schema '{}'",
            request.schema
        )));
    }
    if !matches!(
        request.evaluation_model.as_str(),
        "thin_filament" | "finite_cross_section"
    ) {
        return Err(RunError::Invalid(format!(
            "unsupported evaluation_model '{}' (implemented: thin_filament, finite_cross_section)",
            request.evaluation_model
        )));
    }
    if request.probes_m.is_empty() {
        return Err(RunError::Invalid("kernel probe deck has no probes".into()));
    }

    // Our side evaluates the declared model: thin_filament through the
    // centerline filament path; finite_cross_section through the cell
    // evaluator at the real pack cross-section (the model the search
    // verdicts actually run on — Phase B).
    let racetrack = Racetrack {
        straight_half_length_m: request.geometry.straight_half_length_m,
        bend_radius_m: request.geometry.centerline_bend_radius_m,
        radial_width_m: request.geometry.radial_width_m,
        axial_height_m: request.geometry.axial_height_m,
        ampere_turns_a: request.ampere_turns_a,
        current_model: CurrentModel::UniformWindingPack,
    };
    let mut b_rust = Vec::with_capacity(request.probes_m.len());
    match request.evaluation_model.as_str() {
        "thin_filament" => {
            for &probe in &request.probes_m {
                let b = optcoil_physics::filament::racetrack_filament_field(&racetrack, probe)
                    .ok_or_else(|| {
                        RunError::Invalid(format!("probe {probe:?} lies on the filament"))
                    })?;
                b_rust.push(b);
            }
        }
        _ => {
            let evaluator =
                RacetrackEvaluator::new(&racetrack, quadrature_order).map_err(RunError::Field)?;
            for &probe in &request.probes_m {
                b_rust.push(evaluator.evaluate(probe)?.field_t);
            }
        }
    }

    let (verdict, evidence) = match crosscheck.evaluate_deck(&request) {
        Ok(evidence) => (
            kernel_crosscheck::compare(&request, &b_rust, &evidence),
            Some(evidence),
        ),
        Err(AdapterError::NotAvailable(reason)) => {
            (kernel_crosscheck::not_evaluated(&reason), None)
        }
        Err(e) => return Err(RunError::Invalid(format!("cross-check failed: {e}"))),
    };

    Ok(KernelCrosscheckRunRecord {
        schema: KERNEL_CROSSCHECK_RECORD_SCHEMA,
        request,
        b_rust_t: b_rust,
        verdict,
        evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_adapters::AdapterError;
    use optcoil_adapters::kernel_crosscheck::{KernelEvidence, KernelProbeResponse, sha256_hex};
    use optcoil_physics::circular_loop_axis_bz_t;

    /// A stub crosscheck that is never available — exercises the
    /// NOT_EVALUATED plumbing without Bluemira installed.
    struct Stub;
    impl KernelCrosscheck for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn evaluate_deck(
            &self,
            _request: &KernelProbeRequest,
        ) -> Result<KernelEvidence, AdapterError> {
            Err(AdapterError::NotAvailable("stub".into()))
        }
    }

    /// A stub that returns our own answers scaled by `scale`.
    struct EchoStub {
        scale: f64,
    }
    impl KernelCrosscheck for EchoStub {
        fn name(&self) -> &'static str {
            "echo-stub"
        }
        fn evaluate_deck(
            &self,
            request: &KernelProbeRequest,
        ) -> Result<KernelEvidence, AdapterError> {
            // Compute "our" answer independently in the test (the stub
            // re-implements the same evaluator; for the echo it is the
            // same code — the point is the comparison plumbing, not
            // independence).
            let mut b_t = Vec::with_capacity(request.probes_m.len());
            for &p in &request.probes_m {
                let bz = circular_loop_axis_bz_t(
                    request.geometry.centerline_bend_radius_m,
                    p[2],
                    request.ampere_turns_a,
                )
                .unwrap_or(0.0);
                b_t.push([0.0, 0.0, bz * self.scale]);
            }
            Ok(KernelEvidence {
                request_sha256: sha256_hex(b"req"),
                response_sha256: sha256_hex(b"resp"),
                response: KernelProbeResponse {
                    solver_name: "echo-stub".into(),
                    solver_version: "test".into(),
                    b_t,
                },
            })
        }
    }

    fn loop_deck_json() -> String {
        // L=0 collapses the racetrack to a circular loop of radius R in
        // the z=0 plane; on-axis probes give the closed-form B_z.
        let deck = serde_json::json!({
            "schema": "optcoil-kernel-probe-deck/v1",
            "geometry": {
                "straight_half_length_m": 0.0,
                "centerline_bend_radius_m": 0.03,
                "radial_width_m": 0.01,
                "axial_height_m": 0.01,
                "tape_normal": "radial"
            },
            "evaluation_model": "thin_filament",
            "ampere_turns_a": 100000.0,
            "probes_m": [[0.0, 0.0, 0.0], [0.0, 0.0, 0.02], [0.0, 0.0, -0.05]]
        });
        deck.to_string()
    }

    #[test]
    fn thin_filament_loop_matches_the_closed_form_on_axis() {
        // Our own evaluator collapsed to a filament must reproduce the
        // in-tree closed form — the convention check that has to hold
        // before an external agreement means anything.
        let record = run_kernel_crosscheck(&loop_deck_json(), &Stub, 24).unwrap();
        for (b_rust, probe) in record.b_rust_t.iter().zip(record.request.probes_m.iter()) {
            let expected =
                circular_loop_axis_bz_t(0.03, probe[2], 100_000.0).expect("finite field");
            assert!(
                (b_rust[2] - expected).abs() < 1e-4 * expected.max(1e-6),
                "probe {:?}: rust {:.6e} vs closed form {:.6e}",
                probe,
                b_rust[2],
                expected
            );
            assert!(b_rust[0].abs() < 1e-9 && b_rust[1].abs() < 1e-9);
        }
    }

    #[test]
    fn finite_cross_section_uses_the_volume_evaluator_not_the_filament() {
        // Phase B: the deck's declared model selects our cell evaluator
        // at the real pack XSC. On this geometry the volume answer at
        // the bore differs from the centerline filament by ~1% — axial
        // spread lowers, radial spread raises the field — so matching
        // the filament to ppm would mean the wrong model ran.
        let deck = serde_json::json!({
            "schema": "optcoil-kernel-probe-deck/v1",
            "geometry": {
                "straight_half_length_m": 0.0,
                "centerline_bend_radius_m": 0.03,
                "radial_width_m": 0.01,
                "axial_height_m": 0.01,
                "tape_normal": "radial"
            },
            "evaluation_model": "finite_cross_section",
            "ampere_turns_a": 100000.0,
            "probes_m": [[0.0, 0.0, 0.0]]
        });
        let record = run_kernel_crosscheck(&deck.to_string(), &Stub, 24).unwrap();
        let filament = circular_loop_axis_bz_t(0.03, 0.0, 100_000.0).expect("finite field");
        let b = record.b_rust_t[0][2];
        assert!(b.is_finite() && b > 0.0);
        let rel = (b - filament).abs() / filament;
        assert!(
            rel > 1e-3 && rel < 0.1,
            "finite-XSC field {b} vs filament {filament}: rel {rel}"
        );
        // The stub keeps the verdict honest: the model ran our side,
        // but nothing external answered.
        assert_eq!(record.verdict.status, "not_evaluated");
    }

    #[test]
    fn unknown_evaluation_model_is_rejected() {
        let deck = serde_json::json!({
            "schema": "optcoil-kernel-probe-deck/v1",
            "geometry": {
                "straight_half_length_m": 0.0,
                "centerline_bend_radius_m": 0.03,
                "radial_width_m": 0.01,
                "axial_height_m": 0.01,
                "tape_normal": "radial"
            },
            "evaluation_model": "mystery",
            "ampere_turns_a": 100000.0,
            "probes_m": [[0.0, 0.0, 0.0]]
        });
        assert!(run_kernel_crosscheck(&deck.to_string(), &Stub, 24).is_err());
    }

    #[test]
    fn absent_external_implementation_yields_not_evaluated() {
        let record = run_kernel_crosscheck(&loop_deck_json(), &Stub, 24).unwrap();
        assert_eq!(record.verdict.status, "not_evaluated");
        assert!(record.evidence.is_none());
        assert_eq!(record.b_rust_t.len(), 3); // our side still computed
    }

    #[test]
    fn agreement_passes_and_perturbation_fails() {
        let pass = run_kernel_crosscheck(&loop_deck_json(), &EchoStub { scale: 1.0 }, 24).unwrap();
        assert_eq!(pass.verdict.status, "pass");
        assert!(pass.evidence.is_some());
        let fail = run_kernel_crosscheck(&loop_deck_json(), &EchoStub { scale: 1.01 }, 24).unwrap();
        assert_eq!(fail.verdict.status, "fail");
    }
}
