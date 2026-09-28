//! Cost-semantics guard: the ledger arithmetic must live in exactly the
//! sanctioned places. Consumers read `SearchCostLedger` columns — they
//! never re-derive dollars or lengths from `installed_length_m`,
//! `scrap_fraction`, `price_usd_per_m` or `joint_cost_usd`.
//!
//! The oc-027 incident is the motivating defect: view-layer and reprice
//! code rebuilt totals with the legacy closed form on a v24
//! piece-catalogue record and produced a hybrid total that was neither
//! ledger. This test scans every crate's `src/` for the derivation
//! patterns and fails if they appear outside the allowlist below.
//!
//! If a new producer legitimately needs the arithmetic (a new schema
//! family, a new checker), add its file here with a reason — the point
//! is that the addition is a deliberate, reviewed act, not drift.

use std::fs;
use std::path::{Path, PathBuf};

/// Files allowed to contain ledger-derivation arithmetic, with the
/// reason each is sanctioned.
const ALLOWLIST: &[(&str, &str)] = &[
    (
        "optcoil-search/src/coupled_search.rs",
        "the ledger producer — compute_cost_ledger + piece_plan_ledger",
    ),
    (
        "optcoil-search/src/search_acceptance.rs",
        "deliberately independent recomputation — the audit value is that it does not call the producer",
    ),
    (
        "optcoil-search/src/bom.rs",
        "BOM derivation — reads the piece plan; tapes-1 fallback is the legacy joint semantics",
    ),
    (
        "optcoil-search/src/reprice.rs",
        "legacy closed-form reprice — v24 records are refused before it is reached",
    ),
    (
        "optcoil-app/src/views/metrics.rs",
        "legacy $/m slider repricing — gated by piece_plan checks added after oc-027",
    ),
    (
        "optcoil-search/src/acceptance.rs",
        "v1 coupled-case ledger — flat scrap is that schema's declared semantics",
    ),
    (
        "optcoil-search/src/lib.rs",
        "v1 coupled-case ledger producer (older schema family)",
    ),
    (
        "optcoil-model/src/lib.rs",
        "v1 coupled-case validation bound (max purchasable length)",
    ),
];

/// Line patterns that constitute cost re-derivation.
const PATTERNS: &[&str] = &[
    "installed_length_m *",
    "* installed_length_m",
    "scrap_fraction *",
    "* scrap_fraction",
    "1.0 + scrap",
    "joint_cost_usd *",
    "* joint_cost_usd",
];

fn workspace_src_files() -> Vec<PathBuf> {
    // CARGO_MANIFEST_DIR is <workspace>/crates/optcoil-search — the
    // crates directory is its parent.
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a workspace parent")
        .to_path_buf();
    let mut files = Vec::new();
    for crate_dir in fs::read_dir(&crates_dir).expect("crates dir") {
        let src = crate_dir.unwrap().path().join("src");
        if src.is_dir() {
            collect_rs(&src, &mut files);
        }
    }
    files
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn cost_derivation_lives_only_in_sanctioned_files() {
    let allowed: Vec<&str> = ALLOWLIST.iter().map(|(p, _)| *p).collect();
    let mut violations = Vec::new();
    for file in workspace_src_files() {
        let rel = file
            .to_string_lossy()
            .split("crates/")
            .nth(1)
            .unwrap_or("")
            .to_string();
        if allowed.iter().any(|a| rel == *a) {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (ln, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // `x * 100.0` / `100.0 * x` is percent conversion for display,
            // not ledger arithmetic — skip those lines.
            if line.contains("* 100") || line.contains("100.0 *") {
                continue;
            }
            for pat in PATTERNS {
                if line.contains(pat) {
                    violations.push(format!("{rel}:{} — `{pat}` in `{trimmed}`", ln + 1));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "cost re-derivation outside the sanctioned producers \
         (see tests/cost_semantics_guard.rs allowlist):\n{}",
        violations.join("\n")
    );
}
