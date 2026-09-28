#!/usr/bin/env python3
"""OC-010 Feather-M2 evidence bundle — hashes the full artifact chain.

Extracts the canonical embedded case from the parity cell's run record,
hashes every artifact in the evidence chain, and writes an
evidence-manifest.json binding: case hash, dataset hash, run-record hash,
Bluemira crosscheck record hash, model/checker/schema identities, and the
headline claim (verified optimum tape length vs the published estimate).

Usage: python3 tools/oc010_evidence.py [--out runs/oc010-evidence]
"""

import argparse
import hashlib
import json
import os

ROOT = os.path.join(os.path.dirname(__file__), "..")
RUN_DIR = os.path.join(ROOT, "runs/oc010-featherm2")
# The parity cell: 184.3 m verified optimum, ~3% below the ~190 m
# Feather-M2 tape estimate (docs/OC010.md).
CELL = "L0.100-R0.028"


def sha256_file(path):
    return "sha256:" + hashlib.sha256(open(path, "rb").read()).hexdigest()


def sha256_json(value):
    return "sha256:" + hashlib.sha256(
        json.dumps(value, sort_keys=True).encode()
    ).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        default=os.path.join(ROOT, "runs/oc010-evidence"),
        help="output directory (default: runs/oc010-evidence)",
    )
    args = parser.parse_args()

    record_path = os.path.join(RUN_DIR, f"run-{CELL}.json")
    crosscheck_path = os.path.join(RUN_DIR, f"crosscheck-run-{CELL}.json")
    record = json.load(open(record_path))
    best = record["candidates"][record["best_index"]]

    out = args.out
    os.makedirs(out, exist_ok=True)

    case_path = os.path.join(out, "case.json")
    with open(case_path, "w") as f:
        json.dump(record["case"], f, indent=2, sort_keys=True)
        f.write("\n")

    manifest = {
        "schema": "optcoil-evidence-manifest/v1",
        "study": "oc010-featherm2",
        "cell": CELL,
        "case": {
            "path": "case.json",
            "schema": record["case"]["schema"],
            "id": record["case"]["id"],
            "sha256": sha256_file(case_path),
        },
        "run_record": {
            "path": os.path.relpath(record_path, ROOT),
            "schema": record["schema"],
            "sha256": sha256_file(record_path),
        },
        "identities": {
            "coupled_search_model_id": record["coupled_search_model_id"],
            "coupled_search_checker_id": record["coupled_search_checker_id"],
            "dataset_id": record["dataset_id"],
            "dataset_csv_sha256": record["dataset_csv_sha256"],
        },
        "claim": {
            "verified_optimum_installed_length_m": best["cost"]["installed_length_m"],
            "featherm2_estimate_m": 190.0,
            "relative_to_estimate": best["cost"]["installed_length_m"] / 190.0 - 1.0,
            "optimum_status": best["status"],
            "optimum_refinement_status": best.get("refinement_status"),
            "search_status": record["search_status"],
            # Read this carefully before citing: the optimum verifies at
            # the declared coarse plan (screening rerun PASS, cost/field
            # agreement 0) but the contract §9.4 refined 10-station plan
            # FAILs to re-verify it — the leg failed at an arc_60
            # tape-edge node (width_index 4) that the coarse plan never
            # sampled. The refined capacity there (1249.3 A) still exceeds
            # I_op (977.8 A), so this is not a capacity shortfall — it is
            # the edge self-field regime the OC-014 work targets (under v5
            # the corrected query can resolve it to a real verdict). The
            # baseline is additionally unverifiable because its search
            # status was not PASS. So: the headline number is a
            # coarse-plan optimum, not a fully verified design.
            "acceptance_agreement_status": record["acceptance"]["agreement_status"],
            "acceptance_checks": [
                {"id": c["id"], "status": c["status"], "detail": c.get("detail")}
                for c in record["acceptance"].get("checks", [])
            ],
        },
        "crosscheck": None,
        "notes": [
            "Feather-M2 estimate is a published-geometry estimate, not a"
            " measured winding manifest (docs/OC010.md).",
            "case.json is the embedded case from the run record; its sha256"
            " is of the canonical (sorted-key) serialization, while the"
            " record's case_sha256 binds the exact original bytes.",
            "The optimum verifies at the declared coarse plan but the"
            " contract-9.4 refined 10-station plan does not re-verify it"
            " (arc_60 tape-edge node the coarse plan never sampled;"
            " refined capacity 1249.3 A still exceeds I_op 977.8 A — the"
            " edge self-field regime OC-014 targets, resolvable to a real"
            " verdict under schema v5). The headline length is therefore"
            " a coarse-plan optimum, not a fully verified design.",
        ],
    }

    if os.path.exists(crosscheck_path):
        crosscheck = json.load(open(crosscheck_path))
        verdict = crosscheck.get("verdict", {})
        manifest["crosscheck"] = {
            "path": os.path.relpath(crosscheck_path, ROOT),
            "schema": crosscheck.get("schema"),
            "sha256": sha256_file(crosscheck_path),
            "status": verdict.get("status"),
            "max_magnitude_rel_error": verdict.get("max_magnitude_rel_error"),
            "max_direction_deg": verdict.get("max_direction_deg"),
            "probes_compared": verdict.get("probes_compared"),
            "basis": verdict.get("basis"),
        }

    manifest_path = os.path.join(out, "evidence-manifest.json")
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print(f"wrote {manifest_path}")
    print(json.dumps(manifest["claim"], indent=2))
    print("manifest sha256:", sha256_file(manifest_path))


if __name__ == "__main__":
    main()
