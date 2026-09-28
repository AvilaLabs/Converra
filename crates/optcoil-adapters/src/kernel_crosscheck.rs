//! OC-011: kernel cross-check seam — independent magnetostatics evidence.
//!
//! `SolverRequest`/`SolverAdapter` answer "does this candidate pass"; this
//! module answers a different question: "does an independently developed
//! magnetostatics implementation agree with our kernel on the same frozen
//! inputs". The request is a serializable *deck* of geometry + probe
//! points — no `Case`/`Candidate` — so the two contracts stay clean.
//!
//! Implemented today: `BluemiraCrosscheck`, which shells out to
//! `tools/bluemira_probes.py`. Bluemira is an optional dev-side
//! dependency; when it or the runner is absent the adapter returns
//! `AdapterError::NotAvailable` and callers must surface NOT_EVALUATED —
//! never silently skipped.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::AdapterError;

/// Predeclared comparison tolerances (docs/OC011.md §Tolerance):
/// magnitude relative difference, absolute floor for near-zero probes,
/// and direction agreement in degrees.
pub const MAGNITUDE_REL_TOL: f64 = 1e-3;
pub const MAGNITUDE_ABS_FLOOR_T: f64 = 1e-4;
pub const DIRECTION_TOL_DEG: f64 = 0.5;

/// A frozen probe deck: everything an external implementation needs to
/// reproduce our kernel's field answer, and nothing else. Serialized as
/// canonical JSON (sorted keys, round-trip floats) so `request_sha256`
/// is stable across languages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelProbeRequest {
    pub schema: String,
    pub geometry: ProbeGeometry,
    /// Which model both sides evaluate. `thin_filament`: the centerline
    /// carries `ampere_turns_a` and the pack cross-section is collapsed
    /// (our side evaluates the declared geometry with the cross-section
    /// shrunk to a filament; Bluemira runs BiotSavartFilament).
    /// `finite_cross_section`: both sides model the real pack XSC
    /// (Phase B, `PolyhedralPrismCurrentSource` — not yet implemented).
    pub evaluation_model: String,
    /// Total ampere-turns the conductor carries (NI, not per-strand
    /// current — parallel strands share current but add no turns, and
    /// the field is set by NI either way).
    pub ampere_turns_a: f64,
    /// Probe points in metres, our coordinate convention: straights
    /// along x, bends around z, B_z is the bore field.
    pub probes_m: Vec<[f64; 3]>,
}

/// Thin-filament racetrack geometry as seen by the kernel: centerline
/// path = two straights joined by two semicircles at the pack's
/// centroidal radius.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeGeometry {
    pub straight_half_length_m: f64,
    /// Radius of the centerline the filament follows (pack centroid
    /// bend radius, not the inner bend radius).
    pub centerline_bend_radius_m: f64,
    pub radial_width_m: f64,
    pub axial_height_m: f64,
    /// `radial` | `axial` — recorded for Phase B finite-cross-section
    /// mapping; Phase A thin-filament ignores it.
    pub tape_normal: String,
}

pub const KERNEL_PROBE_DECK_SCHEMA: &str = "optcoil-kernel-probe-deck/v1";

/// The external implementation's answer: a field vector per probe plus
/// identity/version, so evidence binds to a named implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelProbeResponse {
    pub solver_name: String,
    pub solver_version: String,
    /// One `[bx, by, bz]` per request probe, tesla, same order.
    pub b_t: Vec<[f64; 3]>,
}

/// The evidence record: the frozen request hash, the response hash, and
/// the decoded response. Same role `SolverEvidence` plays for candidate
/// evaluation — bindable to a Core claim later.
#[derive(Debug, Clone, Serialize)]
pub struct KernelEvidence {
    pub request_sha256: String,
    pub response_sha256: String,
    pub response: KernelProbeResponse,
}

/// Per-probe comparison outcome.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeComparison {
    pub probe_m: [f64; 3],
    pub b_rust_t: [f64; 3],
    pub b_external_t: [f64; 3],
    pub magnitude_rel_error: f64,
    pub direction_deg: f64,
    /// Whether the probe carried enough field to compare (|B_rust| above
    /// the absolute floor). Below-floor probes are recorded but cannot
    /// fail the verdict.
    pub comparable: bool,
    pub within_tolerance: bool,
}

/// The cross-check verdict. Statuses are OptCoil's four, spelled out as
/// strings so the JSON record stays self-describing.
#[derive(Debug, Clone, Serialize)]
pub struct KernelCrosscheckVerdict {
    /// "pass" | "fail" | "inconclusive" | "not_evaluated".
    pub status: String,
    pub max_magnitude_rel_error: Option<f64>,
    pub max_direction_deg: Option<f64>,
    pub probes_compared: usize,
    pub probes_below_floor: usize,
    pub comparisons: Vec<ProbeComparison>,
    /// Human-readable basis line for the record.
    pub basis: String,
}

/// Canonical JSON for a request: serde_json with sorted map keys is
/// already deterministic for this flat shape (struct field order is
/// declaration order — stable by construction).
pub fn canonical_request_json(request: &KernelProbeRequest) -> Result<String, AdapterError> {
    serde_json::to_string(request).map_err(|e| AdapterError::Serialization(e.to_string()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The cross-check contract. Implementations must never fabricate a
/// response — absence is `AdapterError::NotAvailable`, and callers map
/// it to NOT_EVALUATED.
pub trait KernelCrosscheck: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate_deck(&self, request: &KernelProbeRequest) -> Result<KernelEvidence, AdapterError>;
}

/// The Bluemira-backed implementation: writes the deck to a temp file,
/// runs `tools/bluemira_probes.py deck.json out.json`, reads back the
/// response. No Python is linked; the runner is a plain subprocess, the
/// same shape as the Core pilot's tools.
pub struct BluemiraCrosscheck {
    /// Path to the runner script (default `tools/bluemira_probes.py`
    /// relative to the workspace root).
    pub runner: PathBuf,
    /// Python interpreter (default `python3`).
    pub python: String,
}

impl Default for BluemiraCrosscheck {
    fn default() -> Self {
        Self {
            runner: PathBuf::from("tools/bluemira_probes.py"),
            python: "python3".to_string(),
        }
    }
}

impl KernelCrosscheck for BluemiraCrosscheck {
    fn name(&self) -> &'static str {
        "bluemira"
    }

    fn evaluate_deck(&self, request: &KernelProbeRequest) -> Result<KernelEvidence, AdapterError> {
        if !Path::new(&self.runner).exists() {
            return Err(AdapterError::NotAvailable(format!(
                "runner {} not found",
                self.runner.display()
            )));
        }
        let request_json = canonical_request_json(request)?;
        let request_sha256 = sha256_hex(request_json.as_bytes());

        let dir = std::env::temp_dir();
        let tag = format!(
            "{:x}",
            request_sha256
                .as_bytes()
                .iter()
                .fold(0u64, |a, &b| a.wrapping_mul(31).wrapping_add(b as u64))
        );
        let deck_path = dir.join(format!("optcoil-kernel-deck-{tag}.json"));
        let out_path = dir.join(format!("optcoil-kernel-response-{tag}.json"));
        std::fs::write(&deck_path, &request_json)
            .map_err(|e| AdapterError::Io(format!("write deck: {e}")))?;

        let output = std::process::Command::new(&self.python)
            .arg(&self.runner)
            .arg(&deck_path)
            .arg(&out_path)
            .output()
            .map_err(|e| {
                AdapterError::NotAvailable(format!("could not spawn {}: {e}", self.python))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(
                if stderr.contains("ModuleNotFoundError") || stderr.contains("No module named") {
                    AdapterError::NotAvailable(format!("bluemira not installed: {}", stderr.trim()))
                } else {
                    AdapterError::SolverFailed(format!(
                        "runner exited {}: {}",
                        output.status,
                        stderr.trim()
                    ))
                },
            );
        }
        let response_json = std::fs::read_to_string(&out_path)
            .map_err(|e| AdapterError::Io(format!("read response: {e}")))?;
        let response_sha256 = sha256_hex(response_json.as_bytes());
        let response: KernelProbeResponse = serde_json::from_str(&response_json)
            .map_err(|e| AdapterError::Serialization(format!("response parse: {e}")))?;
        if response.b_t.len() != request.probes_m.len() {
            return Err(AdapterError::SolverFailed(format!(
                "response has {} fields for {} probes",
                response.b_t.len(),
                request.probes_m.len()
            )));
        }
        let _ = std::fs::remove_file(&deck_path);
        let _ = std::fs::remove_file(&out_path);
        Ok(KernelEvidence {
            request_sha256,
            response_sha256,
            response,
        })
    }
}

/// Compare our kernel's answers against the external evidence under the
/// predeclared tolerance. `b_rust` is the caller's own evaluation on the
/// same deck — this module never computes fields itself.
pub fn compare(
    request: &KernelProbeRequest,
    b_rust: &[[f64; 3]],
    evidence: &KernelEvidence,
) -> KernelCrosscheckVerdict {
    let mut comparisons = Vec::with_capacity(request.probes_m.len());
    let mut below_floor = 0usize;
    let mut max_rel = 0.0_f64;
    let mut max_dir = 0.0_f64;
    for (probe, (&b_r, &b_e)) in request
        .probes_m
        .iter()
        .zip(b_rust.iter().zip(evidence.response.b_t.iter()))
    {
        let mag_r = norm(b_r);
        let mag_e = norm(b_e);
        let rel = (mag_r - mag_e).abs() / mag_r.max(MAGNITUDE_ABS_FLOOR_T);
        let comparable = mag_r > MAGNITUDE_ABS_FLOOR_T;
        // Angle between the vectors; undefined (0) when either is ~zero.
        let dir_deg = if mag_r > MAGNITUDE_ABS_FLOOR_T && mag_e > MAGNITUDE_ABS_FLOOR_T {
            let dot = (b_r[0] * b_e[0] + b_r[1] * b_e[1] + b_r[2] * b_e[2]) / (mag_r * mag_e);
            dot.clamp(-1.0, 1.0).acos().to_degrees()
        } else {
            0.0
        };
        if !comparable {
            below_floor += 1;
        } else {
            max_rel = max_rel.max(rel);
            max_dir = max_dir.max(dir_deg);
        }
        let within = !comparable || (rel <= MAGNITUDE_REL_TOL && dir_deg <= DIRECTION_TOL_DEG);
        comparisons.push(ProbeComparison {
            probe_m: *probe,
            b_rust_t: b_r,
            b_external_t: b_e,
            magnitude_rel_error: rel,
            direction_deg: dir_deg,
            comparable,
            within_tolerance: within,
        });
    }
    let compared = comparisons.iter().filter(|c| c.comparable).count();
    let all_within = comparisons.iter().all(|c| c.within_tolerance);
    let (status, basis) = if compared == 0 {
        (
            "inconclusive",
            format!("every probe below the {MAGNITUDE_ABS_FLOOR_T} T floor — no signal to check"),
        )
    } else if all_within {
        (
            "pass",
            format!(
                "{} vs {}: {compared} probes within rel {} / {} deg",
                "RacetrackEvaluator",
                evidence.response.solver_name,
                MAGNITUDE_REL_TOL,
                DIRECTION_TOL_DEG
            ),
        )
    } else {
        (
            "fail",
            format!(
                "{compared} probes compared; at least one outside rel {} / {} deg",
                MAGNITUDE_REL_TOL, DIRECTION_TOL_DEG
            ),
        )
    };
    KernelCrosscheckVerdict {
        status: status.into(),
        max_magnitude_rel_error: (compared > 0).then_some(max_rel),
        max_direction_deg: (compared > 0).then_some(max_dir),
        probes_compared: compared,
        probes_below_floor: below_floor,
        comparisons,
        basis,
    }
}

/// NOT_EVALUATED verdict for when the adapter is unavailable — the
/// honest "we could not check" record, never a silent skip.
pub fn not_evaluated(reason: &str) -> KernelCrosscheckVerdict {
    KernelCrosscheckVerdict {
        status: "not_evaluated".into(),
        max_magnitude_rel_error: None,
        max_direction_deg: None,
        probes_compared: 0,
        probes_below_floor: 0,
        comparisons: Vec::new(),
        basis: format!("cross-check not run: {reason}"),
    }
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
