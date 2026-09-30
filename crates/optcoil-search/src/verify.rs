//! `optcoil verify` — re-check a coupled-search run record's bindings and
//! ledger arithmetic without trusting the producing engine.
//!
//! What this proves: declared SHA-256 bindings recompute (the record's
//! `case_sha256` covers the raw case bytes as supplied at run time), manifest
//! artifact hashes match supplied files, a dataset bundle binds to the
//! declared id and CSV hash, and every candidate's cost ledger is
//! internally consistent.
//!
//! What it does NOT prove: that the physics, screening, or acceptance
//! verdicts are correct. Bindings establish *which inputs produced a
//! record*; engineering validation is separate and remains the customer's
//! responsibility.

use crate::RunError;
use crate::coupled_search::{CoupledSearchRunRecord, compute_cost_ledger};
use optcoil_model::Status;
use optcoil_model::attestation::{DatasetRegistry, attestation_sha256, parse_verifying_key};
use optcoil_model::coupled_search::CoupledSearchCase;
use optcoil_model::material::MaterialBundle;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// One verdict in the verify ledger — public so non-CLI consumers (the
/// workbench) render the same checks the CLI prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    NotChecked,
}

#[derive(Debug)]
pub struct CheckLine {
    pub name: &'static str,
    pub outcome: Outcome,
    pub detail: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn strip_sha(value: &str) -> &str {
    value.strip_prefix("sha256:").unwrap_or(value)
}

/// Structural equality modulo (a) map keys whose value is null — the
/// engine's typed re-serialization materializes absent options as explicit
/// nulls a hand-written case file omits — and (b) JSON number
/// representation (a file's integer literal `30` and the embedded float
/// `30.0` are the same input). Nulls inside arrays are structural and
/// compare exactly.
pub(crate) fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let keys: std::collections::BTreeSet<&String> = ma
                .iter()
                .chain(mb.iter())
                .filter(|(_, v)| !v.is_null())
                .map(|(k, _)| k)
                .collect();
            keys.iter().all(|k| match (ma.get(*k), mb.get(*k)) {
                (Some(x), Some(y)) => json_eq(x, y),
                // Present on only one side with a non-null value.
                _ => false,
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| json_eq(x, y))
        }
        (Value::Number(x), Value::Number(y)) => {
            if x.is_f64() || y.is_f64() {
                x.as_f64() == y.as_f64()
            } else {
                x == y
            }
        }
        _ => a == b,
    }
}

/// Verify a run record against optionally supplied case file, evidence
/// manifest, and dataset bundles. Prints a per-check ledger to stdout;
/// returns Err when any check fails. Each supplied bundle must bind a
/// dataset identity on the record — the base binding or, on run schema
/// v13+, a `spec_datasets` entry.
/// The check ledger without the print — the workbench renders it.
pub fn verify_record_checks(
    record_json: &str,
    case_json: Option<&str>,
    manifest_json: Option<&str>,
    manifest_dir: Option<&std::path::Path>,
    dataset_jsons: &[&str],
) -> Result<Vec<CheckLine>, RunError> {
    let mut checks: Vec<CheckLine> = Vec::new();
    let mut push = |name: &'static str, outcome: Outcome, detail: String| {
        checks.push(CheckLine {
            name,
            outcome,
            detail,
        });
    };

    let record: CoupledSearchRunRecord = serde_json::from_str(record_json).map_err(|e| {
        RunError::Invalid(format!(
            "verify: run record does not parse as the declared schema: {e}"
        ))
    })?;
    push(
        "record.schema",
        if record.schema.starts_with("optcoil-coupled-search-run/") {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        record.schema.clone(),
    );
    push(
        "search_status",
        Outcome::Pass,
        format!("{:?}", record.search_status),
    );

    // --- Case binding ---
    if let Some(case_json) = case_json {
        // The supplied file must still parse as a case under the current
        // schema — catches a non-case JSON passed positionally.
        let supplied: Value = serde_json::from_str(case_json)
            .map_err(|e| RunError::Invalid(format!("verify: case file does not parse: {e}")))?;
        let case_valid = serde_json::from_str::<CoupledSearchCase>(case_json).is_ok();
        push(
            "case parses under contract",
            if case_valid {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            "schema-valid coupled-search case".into(),
        );
        // Content equality uses raw JSON modulo representation: the
        // embedded case was serialized by the engine version that produced
        // the record, which may differ in byte form from both the file and
        // the current schema. Null-valued keys and int/float literal forms
        // are tolerated on both sides.
        let embedded: Value = serde_json::from_str(record_json)
            .ok()
            .and_then(|v: Value| v.get("case").cloned())
            .unwrap_or(Value::Null);
        let content_equal = json_eq(&embedded, &supplied);
        push(
            "embedded case == file",
            if content_equal {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            if content_equal {
                "identical (engine-null defaults tolerated)".into()
            } else {
                "structural mismatch between file and embedded case".into()
            },
        );
        // case_sha256 binds the run-time input bytes (coupled_search.rs).
        // A case republished in canonicalized form has identical content
        // but different bytes — reported as explained, not a failure.
        let recomputed = sha256_hex(case_json.as_bytes());
        let matches = recomputed == strip_sha(&record.case_sha256);
        push(
            "case_sha256 (run input bytes)",
            if matches {
                Outcome::Pass
            } else if content_equal {
                Outcome::NotChecked
            } else {
                Outcome::Fail
            },
            if matches {
                format!("{}… recomputed", &recomputed[..16])
            } else if content_equal {
                "bytes differ — file reserialized after the run; content identical".into()
            } else {
                let declared = strip_sha(&record.case_sha256);
                format!(
                    "recomputed {}… vs record {}…",
                    &recomputed[..16],
                    &declared[..16.min(declared.len())]
                )
            },
        );
    } else {
        push(
            "case binding",
            Outcome::NotChecked,
            "no case file supplied".into(),
        );
    }

    // --- Manifest bindings (raw artifact bytes) ---
    if let Some(manifest_json) = manifest_json {
        let man: Value = serde_json::from_str(manifest_json)
            .map_err(|e| RunError::Invalid(format!("verify: manifest does not parse: {e}")))?;
        push(
            "manifest.schema",
            if man
                .get("schema")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("optcoil-evidence-manifest/"))
            {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            man.get("schema")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_string(),
        );
        for (key, supplied) in [
            ("case", case_json.map(|s| s.as_bytes())),
            ("run_record", Some(record_json.as_bytes())),
        ] {
            let expected = strip_sha(
                man.get(key)
                    .and_then(|e| e.get("sha256"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            .to_string();
            let bytes = match supplied {
                Some(b) => Some(b.to_vec()),
                None => manifest_dir
                    .and_then(|d| {
                        man.get(key)
                            .and_then(|e| e.get("path"))
                            .and_then(Value::as_str)
                            .map(|p| d.join(p))
                    })
                    .and_then(|p| std::fs::read(p).ok()),
            };
            match bytes {
                Some(b) => {
                    let actual = sha256_hex(&b);
                    push(
                        match key {
                            "case" => "manifest.case.sha256",
                            _ => "manifest.run_record.sha256",
                        },
                        if actual == expected {
                            Outcome::Pass
                        } else {
                            Outcome::Fail
                        },
                        format!("{}…", &actual[..16.min(actual.len())]),
                    );
                }
                None => push(
                    match key {
                        "case" => "manifest.case.sha256",
                        _ => "manifest.run_record.sha256",
                    },
                    Outcome::NotChecked,
                    "artifact bytes not supplied and manifest-relative path unreadable".into(),
                ),
            }
        }
        let ids = man.get("identities");
        let agree = ids.is_some_and(|ids| {
            ids.get("coupled_search_model_id").and_then(Value::as_str)
                == Some(record.coupled_search_model_id.as_str())
                && ids.get("coupled_search_checker_id").and_then(Value::as_str)
                    == Some(record.coupled_search_checker_id.as_str())
                && ids.get("dataset_id").and_then(Value::as_str) == Some(record.dataset_id.as_str())
                && ids
                    .get("dataset_csv_sha256")
                    .and_then(Value::as_str)
                    .map(strip_sha)
                    == Some(strip_sha(&record.dataset_csv_sha256))
        });
        push(
            "manifest.identities",
            if agree { Outcome::Pass } else { Outcome::Fail },
            if agree {
                "model/checker/dataset identities agree with record".into()
            } else {
                "identity mismatch vs record".into()
            },
        );
    }

    // --- Dataset bundle bindings ---
    // Each supplied bundle must bind a distinct declared identity: the base
    // binding or (run schema v13+) a spec_datasets entry. A graded record
    // verified against only the base bundle reports partial coverage, not a
    // pass over the full dataset set.
    if !dataset_jsons.is_empty() {
        let mut n_bound = 0usize;
        for (i, dj) in dataset_jsons.iter().enumerate() {
            let bundle = MaterialBundle::from_json(dj)
                .map_err(|e| RunError::Invalid(format!("verify: dataset bundle rejected: {e}")))?;
            let csv_sha = dataset_csv_sha256(dj)?;
            let binds_base = csv_sha == strip_sha(&record.dataset_csv_sha256)
                && bundle.dataset.metadata.id == record.dataset_id
                && strip_sha(&bundle.dataset.metadata.csv_sha256) == csv_sha;
            let spec_match = record.spec_datasets.iter().find(|(_, d)| {
                d.id == bundle.dataset.metadata.id && strip_sha(&d.csv_sha256) == csv_sha
            });
            let role = if binds_base {
                Some(format!("base '{}'", record.dataset_id))
            } else {
                spec_match.map(|(spec, _)| format!("spec '{spec}'"))
            };
            match role {
                Some(role) => {
                    n_bound += 1;
                    push(
                        "dataset bundle binding",
                        Outcome::Pass,
                        format!("bundle #{i} id={} binds {role}", bundle.dataset.metadata.id),
                    );
                }
                None => push(
                    "dataset bundle binding",
                    Outcome::Fail,
                    format!(
                        "bundle #{i} id={} csv_sha={}… binds no declared dataset identity",
                        bundle.dataset.metadata.id,
                        &csv_sha[..16]
                    ),
                ),
            }
            match &bundle.attestation {
                Some(a) => push(
                    "dataset attestation",
                    Outcome::NotChecked,
                    format!(
                        "bundle #{i} signed by {} (key {}); check the signature with `optcoil dataset verify`",
                        a.issuer, a.key_id
                    ),
                ),
                None => push(
                    "dataset attestation",
                    Outcome::NotChecked,
                    format!("bundle #{i} is unsigned"),
                ),
            }
        }
        let n_identities = 1 + record.spec_datasets.len();
        if n_bound < n_identities {
            push(
                "dataset coverage",
                Outcome::NotChecked,
                format!(
                    "{n_bound}/{n_identities} declared dataset identities bound; \
                     remaining identities unverified"
                ),
            );
        }
    } else {
        push(
            "dataset bundle binding",
            Outcome::NotChecked,
            format!("record declares {}", record.dataset_id),
        );
    }

    // --- Structure ---
    // On graded cases the grid is geometry × per-region tape-spec
    // assignment (schema v10+); candidate_count covers both.
    let expected_n = record.case.candidate_count();
    push(
        "candidate count",
        if record.candidates.len() == expected_n {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        format!(
            "{} candidates vs {} declared grid points",
            record.candidates.len(),
            expected_n
        ),
    );

    // Undefined field/current diagnostics are retained as null on candidates
    // whose requirement failed before screening. They are never numerical
    // evidence and must not appear on an evaluated or passing candidate.
    let quantities_ok = record.candidates.iter().all(|candidate| {
        let values = [
            candidate.unit_bore_bz_t_per_ampere_turn,
            candidate.bore_refinement_change_t,
            candidate.ampere_turns_a,
            candidate.operating_current_a,
        ];
        values.iter().all(|value| value.is_finite())
            || (candidate.status == Status::Fail
                && candidate.requirement_status == Status::Fail
                && candidate.screening.is_none())
    });
    push(
        "candidate.quantity_availability",
        if quantities_ok { Outcome::Pass } else { Outcome::Fail },
        "Undefined quantities are permitted only on failed requirements without evaluated screening; they establish no field or current value.".into(),
    );

    // --- Ledger arithmetic (independent of physics) ---
    // Recompute each candidate's full ledger through the same cost function
    // the producer used: geometry from the record, per-region spec pricing
    // from the case, comparing all fields. On graded cases the ledger is
    // the per-turn price-weighted sum, so this also verifies the recorded
    // tape_spec_ids assignment drove the recorded cost.
    let mut bad = Vec::new();
    for (i, c) in record.candidates.iter().enumerate() {
        let g = &c.geometry;
        // A recorded assignment naming an unknown spec would panic inside
        // compute_cost_ledger; mark it a mismatch instead.
        if g.tape_spec_ids
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .any(|s| record.case.spec_price_usd_per_m(s).is_none())
        {
            bad.push(i);
            continue;
        }
        // Schema v21: the candidate's axis-resolved racetrack dims ride
        // the record — absent fields mean the case's fixed geometry.
        let dims = g.dims();
        let recomputed = compute_cost_ledger(
            &record.case,
            g.turns_along_normal,
            g.tapes_along_width,
            g.strands_parallel,
            g.tape_spec_ids.as_deref().unwrap_or(&[]),
            dims,
        );
        let fields = [
            (
                "installed_length_m",
                c.cost.installed_length_m,
                recomputed.installed_length_m,
            ),
            (
                "purchased_length_m",
                c.cost.purchased_length_m,
                recomputed.purchased_length_m,
            ),
            (
                "conductor_usd",
                c.cost.conductor_usd,
                recomputed.conductor_usd,
            ),
            ("scrap_usd", c.cost.scrap_usd, recomputed.scrap_usd),
            ("assembly_usd", c.cost.assembly_usd, recomputed.assembly_usd),
            ("joints_usd", c.cost.joints_usd, recomputed.joints_usd),
            ("total_usd", c.cost.total_usd, recomputed.total_usd),
        ];
        if fields.iter().any(|(_, a, b)| (a - b).abs() > 0.01) {
            bad.push(i);
        }
    }
    push(
        "ledger arithmetic",
        if bad.is_empty() {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        if bad.is_empty() {
            format!(
                "{}/{} candidates recompute exactly",
                record.candidates.len(),
                record.candidates.len()
            )
        } else {
            format!("mismatch at candidates {:?}", &bad[..bad.len().min(5)])
        },
    );

    // --- Optimum / baseline selection ---
    let passed: Vec<usize> = record
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status == Status::Pass)
        .map(|(i, _)| i)
        .collect();
    if passed.is_empty() {
        push(
            "best_index",
            if record.best_index.is_none() {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            "no PASS candidates".into(),
        );
    } else {
        // Same comparator as the engine: cost, then fewer total turns,
        // then index (coupled_search.rs best_index selection).
        let argmin = passed
            .iter()
            .min_by(|&&a, &&b| {
                let (x, y) = (&record.candidates[a], &record.candidates[b]);
                x.cost
                    .total_usd
                    .total_cmp(&y.cost.total_usd)
                    .then(x.geometry.total_turns.cmp(&y.geometry.total_turns))
                    .then(x.index.cmp(&y.index))
            })
            .copied();
        let ok = record.best_index == argmin
            && argmin.is_some_and(|i| record.candidates[i].status == Status::Pass);
        push(
            "best_index",
            if ok { Outcome::Pass } else { Outcome::Fail },
            format!(
                "best={:?} recomputed argmin={:?}",
                record.best_index, argmin
            ),
        );
    }

    let base = &record.case.baseline;
    let baseline_ok = record.baseline_index < record.candidates.len() && {
        let g = &record.candidates[record.baseline_index].geometry;
        let assignment_ok = match (&g.tape_spec_ids, &base.tape_spec_ids) {
            // Graded cases: the baseline's recorded assignment must equal
            // the declared one.
            (Some(a), Some(b)) => a == b,
            // Ungraded cases carry no assignment on either side.
            (None, None) => true,
            _ => false,
        };
        g.turns_along_normal == base.turns_along_normal
            && g.tapes_along_width == base.tapes_along_width
            && g.strands_parallel == base.strands_parallel.unwrap_or(1)
            && assignment_ok
    };
    push(
        "baseline_index",
        if baseline_ok {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        format!(
            "baseline {}x{} -> candidate geometry match={}",
            base.turns_along_normal, base.tapes_along_width, baseline_ok
        ),
    );

    // --- Acceptance + identities ---
    push(
        "acceptance.agreement",
        Outcome::Pass,
        format!("{:?}", record.acceptance.agreement_status),
    );
    let identities = [
        &record.coupled_search_model_id,
        &record.coupled_search_checker_id,
        &record.input_sha256,
        &record.implementation_sha256,
        &record.dataset_id,
        &record.dataset_csv_sha256,
    ];
    push(
        "declared identities",
        if identities.iter().all(|s| !s.is_empty()) {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        "model/checker/input/impl/dataset identities present".into(),
    );

    Ok(checks)
}

/// Verify a run record against optionally supplied case file, evidence
/// manifest, and dataset bundles. Prints a per-check ledger to stdout;
/// returns Err when any check fails. Each supplied bundle must bind a
/// dataset identity on the record — the base binding or, on run schema
/// v13+, a `spec_datasets` entry.
pub fn verify_record(
    record_json: &str,
    case_json: Option<&str>,
    manifest_json: Option<&str>,
    manifest_dir: Option<&std::path::Path>,
    dataset_jsons: &[&str],
) -> Result<(), RunError> {
    let checks = verify_record_checks(
        record_json,
        case_json,
        manifest_json,
        manifest_dir,
        dataset_jsons,
    )?;
    let mut n_pass = 0;
    let mut n_fail = 0;
    let mut n_nc = 0;
    for c in &checks {
        let label = match c.outcome {
            Outcome::Pass => {
                n_pass += 1;
                "PASS"
            }
            Outcome::Fail => {
                n_fail += 1;
                "FAIL"
            }
            Outcome::NotChecked => {
                n_nc += 1;
                "NOT_CHECKED"
            }
        };
        println!("{label:12} {}: {}", c.name, c.detail);
    }
    println!("\n{n_pass} PASS, {n_fail} FAIL, {n_nc} NOT_CHECKED");
    println!(
        "Verified: artifact bindings, structure, and ledger arithmetic. \
         Not verified: physics and engineering acceptance."
    );
    if n_fail > 0 {
        Err(RunError::Invalid(format!(
            "verify: {n_fail} check(s) failed"
        )))
    } else {
        Ok(())
    }
}

/// SHA-256 over the bundle's raw `csv_data` text — the identity binding is
/// over the CSV bytes exactly as supplied.
fn dataset_csv_sha256(bundle_json: &str) -> Result<String, RunError> {
    let v: Value = serde_json::from_str(bundle_json)
        .map_err(|e| RunError::Invalid(format!("verify: dataset bundle parse: {e}")))?;
    let csv = v
        .get("csv_data")
        .and_then(Value::as_str)
        .ok_or_else(|| RunError::Invalid("verify: bundle missing csv_data".into()))?;
    Ok(sha256_hex(csv.as_bytes()))
}

/// A single `optcoil dataset verify` check line.
pub struct DatasetCheck {
    pub name: &'static str,
    pub verdict: &'static str,
    pub detail: String,
}

/// Verify a dataset bundle's schema, dataset binding and attestation, plus
/// its registry status when `registry_json` is supplied. `pubkey_override`
/// (an `ed25519:<hex>` string the caller vouches for) allows signature
/// verification without a registry. Bundle bytes are hashed as supplied —
/// the registry binds exact artifact bytes, matching run-record conventions.
///
/// Returns one line per check; callers exit nonzero when any verdict is
/// FAIL. NOT_CHECKED always names what was missing (unsigned bundle, no
/// registry, unknown issuer key).
pub fn verify_dataset_bundle(
    bundle_text: &str,
    registry_json: Option<&str>,
    pubkey_override: Option<&str>,
) -> Result<Vec<DatasetCheck>, RunError> {
    let mut checks = Vec::new();
    let mut push = |name: &'static str, verdict: &'static str, detail: String| {
        checks.push(DatasetCheck {
            name,
            verdict,
            detail,
        });
    };

    let bundle = match MaterialBundle::from_json(bundle_text) {
        Ok(b) => b,
        Err(e) => {
            push("bundle.schema", "FAIL", format!("rejected: {e}"));
            return Ok(checks);
        }
    };
    push(
        "bundle.schema",
        "PASS",
        format!(
            "v{} · dataset {} · {} measurements · csv_sha256 bound",
            if bundle.attestation.is_some() { 2 } else { 1 },
            bundle.dataset.metadata.id,
            bundle.dataset.points.len()
        ),
    );
    let dataset_id = &bundle.dataset.metadata.id;
    let csv_sha = &bundle.dataset.metadata.csv_sha256;

    // --- Registry (load once: keys and entries both come from it) ---
    let registry = match registry_json {
        Some(text) => match DatasetRegistry::from_json(text) {
            Ok(r) => {
                push(
                    "registry.schema",
                    "PASS",
                    format!("issuer {} (key {})", r.registry_issuer, r.registry_key_id),
                );
                Some(r)
            }
            Err(e) => {
                push("registry.schema", "FAIL", format!("rejected: {e}"));
                return Ok(checks);
            }
        },
        None => None,
    };

    // --- Attestation signature ---
    match &bundle.attestation {
        None => push(
            "attestation.signature",
            "NOT_CHECKED",
            "bundle is unsigned".into(),
        ),
        Some(att) => {
            let key = if let Some(registry) = &registry {
                match registry.find_key(&att.issuer, &att.key_id) {
                    Some(k) if k.status == "revoked" => {
                        push(
                            "attestation.signature",
                            "FAIL",
                            format!("issuer key {}/{} is revoked", att.issuer, att.key_id),
                        );
                        None
                    }
                    Some(k) => match parse_verifying_key(&k.public_key) {
                        Ok(vk) => Some(vk),
                        Err(e) => {
                            push(
                                "attestation.signature",
                                "FAIL",
                                format!("registry key parses invalid: {e}"),
                            );
                            None
                        }
                    },
                    None => {
                        push(
                            "attestation.signature",
                            "NOT_CHECKED",
                            format!("issuer key {}/{} not in registry", att.issuer, att.key_id),
                        );
                        None
                    }
                }
            } else if let Some(hex) = pubkey_override {
                match parse_verifying_key(hex) {
                    Ok(vk) => Some(vk),
                    Err(e) => {
                        push(
                            "attestation.signature",
                            "FAIL",
                            format!("--pubkey parses invalid: {e}"),
                        );
                        None
                    }
                }
            } else {
                push(
                    "attestation.signature",
                    "NOT_CHECKED",
                    "no --registry or --pubkey supplied".into(),
                );
                None
            };
            if let Some(vk) = key {
                match att.verify(dataset_id, csv_sha, &vk) {
                    Ok(()) => push(
                        "attestation.signature",
                        "PASS",
                        format!("signed by {} (key {})", att.issuer, att.key_id),
                    ),
                    Err(e) => push("attestation.signature", "FAIL", e.to_string()),
                }
            }
        }
    }

    // --- Registry entry ---
    match &registry {
        None => push(
            "registry.entry",
            "NOT_CHECKED",
            "no --registry supplied".into(),
        ),
        Some(registry) => match registry.find_entry(dataset_id) {
            None => push(
                "registry.entry",
                "NOT_CHECKED",
                format!("no entry for {dataset_id}"),
            ),
            Some(entry) => {
                let bundle_sha = sha256_hex(bundle_text.as_bytes());
                push(
                    "registry.bundle_sha256",
                    if bundle_sha == strip_sha(&entry.bundle_sha256) {
                        "PASS"
                    } else {
                        "FAIL"
                    },
                    format!("bundle_sha={}…", &bundle_sha[..16.min(bundle_sha.len())]),
                );
                let att_sha_ok = bundle
                    .attestation
                    .as_ref()
                    .map(|a| attestation_sha256(a) == strip_sha(&entry.attestation_sha256))
                    .unwrap_or(false);
                push(
                    "registry.attestation_sha256",
                    if att_sha_ok { "PASS" } else { "FAIL" },
                    if bundle.attestation.is_some() {
                        "entry binds this attestation".into()
                    } else {
                        "bundle unsigned but registry entry binds an attestation".into()
                    },
                );
                let registry_key = registry.verifying_key();
                match registry_key.map_err(RunError::from).and_then(|vk| {
                    entry
                        .verify_countersignature(&registry.registry_issuer, &vk)
                        .map_err(|e| RunError::Invalid(e.to_string()))
                }) {
                    Ok(()) => push(
                        "registry.countersignature",
                        "PASS",
                        format!("countersigned by {}", registry.registry_issuer),
                    ),
                    Err(e) => push("registry.countersignature", "FAIL", e.to_string()),
                }
                push(
                    "registry.status",
                    match entry.status.as_str() {
                        "revoked" => "FAIL",
                        _ => "PASS",
                    },
                    if entry.note.is_empty() {
                        entry.status.clone()
                    } else {
                        format!("{} — {}", entry.status, entry.note)
                    },
                );
            }
        },
    }
    Ok(checks)
}
