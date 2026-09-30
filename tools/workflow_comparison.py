#!/usr/bin/env python3
"""Run matched explicit-file CLI and shared-study workflow comparisons.

This harness records commands, software operations, artifacts, input hashes and
process timings. Action counts and script timings are not measured human effort.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
from datetime import datetime, timezone
import subprocess
import time
from pathlib import Path
from typing import Any


REPO = Path(__file__).resolve().parents[1]
MANIFEST = REPO / "benchmarks/workflow-comparison/fixtures.json"
VOLATILE_PATHS = {("elapsed_ms",), ("started_unix_ms",)}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def strip_volatile(value: Any, path: tuple[Any, ...] = ()) -> Any:
    """Drop only known timing fields at their declared run-record paths."""
    if isinstance(value, dict):
        return {
            k: strip_volatile(v, path + (k,))
            for k, v in value.items()
            if path + (k,) not in VOLATILE_PATHS
            and not (
                len(path) == 2
                and path[0] == "candidates"
                and isinstance(path[1], int)
                and k == "field_timing_ms"
            )
        }
    if isinstance(value, list):
        return [strip_volatile(v, path + (index,)) for index, v in enumerate(value)]
    return value


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def at_pointer(value: Any, pointer: str) -> Any:
    target = value
    for encoded in pointer.strip("/").split("/"):
        key = encoded.replace("~1", "/").replace("~0", "~")
        target = target[key]
    return target


def set_pointer(value: Any, pointer: str, replacement: Any) -> None:
    parts = pointer.strip("/").split("/")
    target = value
    for encoded in parts[:-1]:
        key = encoded.replace("~1", "/").replace("~0", "~")
        target = target[key]
    key = parts[-1].replace("~1", "/").replace("~0", "~")
    target[key] = replacement


def freeze_fixture(spec: dict[str, Any]) -> tuple[bytes, list[bytes], bytes]:
    case_path = REPO / spec["case"]
    case_bytes = case_path.read_bytes()
    bundle_paths = [REPO / p for p in spec["bundles"]]
    bundle_bytes = [p.read_bytes() for p in bundle_paths]
    expected = spec["sha256"]
    actual = {"case": sha256(case_bytes), "bundles": [sha256(b) for b in bundle_bytes]}
    if actual != expected:
        raise RuntimeError(f"frozen fixture hash mismatch for {spec['id']}: expected {expected}, got {actual}")
    return case_bytes, bundle_bytes, case_path.read_bytes()


def make_revision(case_bytes: bytes, spec: dict[str, Any]) -> bytes:
    case = json.loads(case_bytes)
    pointer = spec["revision"]["pointer"]
    old = at_pointer(case, pointer)
    new = old * spec["revision"].get("multiplier", 1.05)
    set_pointer(case, pointer, new)
    return (json.dumps(case, indent=2, ensure_ascii=False, allow_nan=False) + "\n").encode()


def run_command(argv: list[str], cwd: Path, log_dir: Path, name: str, *, allow_failure: bool = False) -> dict[str, Any]:
    started = time.perf_counter()
    result = subprocess.run(argv, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    elapsed = (time.perf_counter() - started) * 1000.0
    (log_dir / f"{name}.stdout.txt").write_text(result.stdout, encoding="utf-8")
    (log_dir / f"{name}.stderr.txt").write_text(result.stderr, encoding="utf-8")
    receipt = {"name": name, "argv": argv, "exit_code": result.returncode, "wall_ms": elapsed}
    if result.returncode and not allow_failure:
        raise RuntimeError(f"{name} failed with exit {result.returncode}; see {log_dir / (name + '.stderr.txt')}\n{result.stderr[-2000:]}")
    return receipt


def validate_search_exit(receipt: dict[str, Any], output: Path) -> None:
    if not output.exists():
        raise RuntimeError(f"{receipt['name']} did not write a run record")
    record = load_json(output)
    passed = record.get("search_status") == "PASS" and record.get("acceptance", {}).get("agreement_status") == "PASS"
    expected = 0 if passed else 1
    if receipt["exit_code"] != expected:
        raise RuntimeError(f"{receipt['name']} exit {receipt['exit_code']} differs from expected {expected} for its declared verdicts")


def cli_prefix(cli: str, subcommand: str, case: Path, bundles: list[Path]) -> list[str]:
    args = [cli, subcommand, str(case), "--threads", "1"]
    for bundle in bundles:
        args.extend(["--dataset-bundle", str(bundle)])
    return args


def record_semantics(path: Path) -> dict[str, Any]:
    record = load_json(path)
    candidates = record.get("candidates", [])
    selected = record.get("best_index")
    selected_candidate = next((c for c in candidates if c.get("index") == selected), None)
    case = record.get("case", {})
    dataset_identities = []
    base_binding = case.get("material", {})
    if base_binding.get("dataset_id"):
        dataset_identities.append((base_binding.get("dataset_id"), base_binding.get("csv_sha256")))
    for spec in (case.get("tape_specs") or {}).values():
        binding = spec.get("material", {})
        if binding.get("dataset_id"):
            dataset_identities.append((binding.get("dataset_id"), binding.get("csv_sha256")))
    return {
        "case_sha256": record.get("case_sha256"),
        "input_sha256": record.get("input_sha256"),
        "search_status": record.get("search_status"),
        "acceptance_agreement_status": record.get("acceptance", {}).get("agreement_status"),
        "best_index": selected,
        "candidate_count": len(candidates),
        "candidate_statuses": [c.get("status") for c in candidates],
        "selected_geometry": selected_candidate.get("geometry") if selected_candidate else None,
        "selected_cost": selected_candidate.get("cost") if selected_candidate else None,
        "dataset_identities": sorted(set(dataset_identities)),
    }


def run_fixture(spec: dict[str, Any], cli: str, workspace_exe: str, run_root: Path) -> dict[str, Any]:
    fixture_dir = run_root / spec["id"]
    fixture_dir.mkdir()
    cli_dir = fixture_dir / "explicit-cli"
    shared_dir = fixture_dir / "shared-workspace"
    inputs_dir = fixture_dir / "frozen-inputs"
    cli_dir.mkdir()
    shared_dir.mkdir()
    inputs_dir.mkdir()

    base_bytes, bundle_bytes, _ = freeze_fixture(spec)
    revised_bytes = make_revision(base_bytes, spec)
    base_path = inputs_dir / "case-base.json"
    revised_path = inputs_dir / "case-revised.json"
    base_path.write_bytes(base_bytes)
    revised_path.write_bytes(revised_bytes)
    bundle_paths = []
    for index, (source, data) in enumerate(zip(spec["bundles"], bundle_bytes, strict=True)):
        path = inputs_dir / f"bundle-{index + 1}.json"
        path.write_bytes(data)
        if sha256(path.read_bytes()) != sha256((REPO / source).read_bytes()):
            raise RuntimeError("bundle snapshot differs from frozen source")
        bundle_paths.append(path)
    hashes = {
        "base_case_sha256": sha256(base_bytes),
        "revised_case_sha256": sha256(revised_bytes),
        "bundle_sha256": [sha256(b) for b in bundle_bytes],
        "revision": spec["revision"],
        "execution_options": {"threads": 1},
    }
    (inputs_dir / "frozen-inputs.json").write_text(json.dumps(hashes, indent=2) + "\n", encoding="utf-8")

    command_receipts = []
    # Explicit files and CLI commands: preflight/search, revise by creating
    # an exact revised case file, search it, verify records, compare, repeat,
    # and create + verify a portable package.
    for variant, case in [("base", base_path), ("revised", revised_path)]:
        prefix = cli_prefix(cli, "preflight", case, bundle_paths)
        prefix.append("--json")
        command_receipts.append(run_command(prefix, REPO, cli_dir, f"{variant}-preflight"))
        output = cli_dir / f"{variant}-record.json"
        search = cli_prefix(cli, "coupled-search", case, bundle_paths)
        search.extend(["--output", str(output)])
        receipt = run_command(search, REPO, cli_dir, f"{variant}-search", allow_failure=True)
        validate_search_exit(receipt, output)
        command_receipts.append(receipt)
        verify = [cli, "verify", str(output), str(case)]
        for bundle in bundle_paths:
            verify.extend(["--dataset", str(bundle)])
        command_receipts.append(run_command(verify, REPO, cli_dir, f"{variant}-verify"))

    cli_records = {name: cli_dir / f"{name}-record.json" for name in ("base", "revised")}
    cli_index = {
        "schema": "converra-explicit-file-study-index/v1",
        "variants": [
            {"name": "Measured baseline", "case": str(base_path), "record": str(cli_records["base"])},
            {"name": "Named price revision +5%", "case": str(revised_path), "record": str(cli_records["revised"])},
        ],
        "dataset_bundles": [str(path) for path in bundle_paths],
        "execution_options": {"threads": 1},
    }
    cli_index_path = cli_dir / "study-index.json"
    cli_index_path.write_text(json.dumps(cli_index, indent=2) + "\n", encoding="utf-8")
    reopened_cli_index = load_json(cli_index_path)
    if reopened_cli_index != cli_index:
        raise RuntimeError("saved explicit-file study index changed when reopened")
    cli_diff = record_semantics(cli_records["revised"])
    cli_diff["base"] = record_semantics(cli_records["base"])
    cli_diff["revision"] = "case files compared by the harness; no native CLI study diff operation"
    command_receipts.append(run_command(
        cli_prefix(cli, "coupled-search", base_path, bundle_paths)
        + ["--output", str(cli_dir / "exact-repeat-record.json")],
        REPO, cli_dir, "exact-repeat-search", allow_failure=True,
    ))
    validate_search_exit(command_receipts[-1], cli_dir / "exact-repeat-record.json")
    command_receipts.append(run_command([
        cli, "review-package", str(cli_records["revised"]), str(revised_path),
        "--output", str(cli_dir / "review-package"),
        *sum((["--dataset-bundle", str(p)] for p in bundle_paths), []),
    ], REPO, cli_dir, "review-package"))
    command_receipts.append(run_command([cli, "verify-package", str(cli_dir / "review-package")], REPO, cli_dir, "verify-package"))

    started = time.perf_counter()
    workspace_receipt = run_command([
        workspace_exe, str(base_path), str(revised_path), str(shared_dir / "workspace-output"),
        *map(str, bundle_paths),
    ], REPO, shared_dir, "shared-workspace", allow_failure=False)
    command_receipts.append(workspace_receipt)
    workspace_json = load_json(shared_dir / "workspace-output/workspace-receipt.json")
    records = {
        "base": shared_dir / "workspace-output/base-record.json",
        "revised": shared_dir / "workspace-output/revised-record.json",
    }
    for variant, case in (("base", base_path), ("revised", revised_path)):
        verify = [cli, "verify", str(records[variant]), str(case)]
        for bundle in bundle_paths:
            verify.extend(["--dataset", str(bundle)])
        command_receipts.append(run_command(verify, REPO, shared_dir, f"verify-shared-{variant}"))
    # The shared path creates the portable package through its workspace API;
    # this external verifier reopens it from disk, matching the CLI package gate.
    command_receipts.append(run_command([
        cli, "verify-package", str(shared_dir / "workspace-output/review-package")
    ], REPO, shared_dir, "verify-shared-review-package"))
    shared_wall_ms = (time.perf_counter() - started) * 1000.0

    equivalence = {}
    for variant in ("base", "revised"):
        a, b = load_json(cli_records[variant]), load_json(records[variant])
        equivalence[variant] = {
            "case_hash_equal": a.get("case_sha256") == b.get("case_sha256"),
            "input_hash_equal": a.get("input_sha256") == b.get("input_sha256"),
            "semantic_record_equal_excluding_runtime": strip_volatile(a) == strip_volatile(b),
            "cli_summary": record_semantics(cli_records[variant]),
            "workspace_summary": record_semantics(records[variant]),
        }
    comparison = {
        "variant_names": [item["name"] for item in reopened_cli_index["variants"]],
        "base": record_semantics(cli_records["base"]),
        "revised": record_semantics(cli_records["revised"]),
        "changed_case_inputs": spec["revision"],
    }
    (cli_dir / "comparison.json").write_text(json.dumps(comparison, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    cli_exact = strip_volatile(load_json(cli_records["base"])) == strip_volatile(load_json(cli_dir / "exact-repeat-record.json"))
    artifact_paths = sorted(str(p.relative_to(fixture_dir)) for p in fixture_dir.rglob("*") if p.is_file())
    artifact_paths.append("receipt.json")
    artifact_paths.sort()
    cli_file_operations = ["write/reopen named study index", "compare base and revised record summaries"]
    receipt = {
        "schema": "converra-workflow-comparison/v1",
        "fixture_id": spec["id"],
        "fixture_description": spec["description"],
        "input_hashes": hashes,
        "matched_execution_options": {"threads": 1},
        "workflow_paths": {
            "explicit_cli": "CLI commands over case, bundle, run-record and review-package files",
            "shared_workspace": "StudyWorkspace/StudyEngineSession typed operations in a single process",
        },
        "cli_commands": command_receipts,
        "explicit_cli_command_count": sum(1 for c in command_receipts if c["name"] not in {"shared-workspace", "verify-shared-base", "verify-shared-revised", "verify-shared-review-package"}),
        "all_cli_subprocess_invocation_count": sum(1 for c in command_receipts if c["name"] != "shared-workspace"),
        "cli_file_operations": cli_file_operations,
        "cli_file_operation_count": len(cli_file_operations),
        "workspace_operations": workspace_json["operations"],
        "workspace_operation_count": workspace_json["operation_count"],
        "produced_artifacts": artifact_paths,
        "produced_artifact_count": len(artifact_paths),
        "comparison": {
            "record_equivalence": equivalence,
            "cli_exact_repeat_semantically_equal": cli_exact,
            "workspace_exact_repeat_reused_result_id": workspace_json["exact_repeat_reused_result_id"],
            "workspace_reopened_cache_empty": workspace_json["reopened_execution_cache_empty"],
        },
        "timing_ms": {
            "cli_fresh_search_base_process_wall": next(c["wall_ms"] for c in command_receipts if c["name"] == "base-search"),
            "cli_fresh_search_revised_process_wall": next(c["wall_ms"] for c in command_receipts if c["name"] == "revised-search"),
            "cli_exact_repeat_fresh_process_wall": next(c["wall_ms"] for c in command_receipts if c["name"] == "exact-repeat-search"),
            "shared_workspace_total_process_wall": workspace_receipt["wall_ms"],
            "shared_workspace_plus_external_verification_wall": shared_wall_ms,
            "shared_workspace_internal_workflow_wall": workspace_json["timing_ms"]["workflow_total_including_solves"],
            "explicit_cli_command_process_wall_sum": sum(c["wall_ms"] for c in command_receipts if c["name"] not in {"shared-workspace", "verify-shared-base", "verify-shared-revised", "verify-shared-review-package"}),
            "shared_workspace_base_record_solve": workspace_json["timing_ms"]["base_search_elapsed_from_record"],
            "shared_workspace_revised_record_solve": workspace_json["timing_ms"]["revised_search_elapsed_from_record"],
            "cli_base_record_search_elapsed": load_json(cli_records["base"]).get("elapsed_ms"),
            "cli_revised_record_search_elapsed": load_json(cli_records["revised"]).get("elapsed_ms"),
            "shared_workspace_exact_repeat_operation": workspace_json["timing_ms"]["exact_repeat_operation_wall"],
            "shared_workspace_orchestration_excluding_run_calls": workspace_json["timing_ms"]["orchestration_excluding_run_variant_calls"],
            "interpretation": "process and engine measurements only; not human setup or review time",
        },
        "memory_measurement": "not captured; subprocess peak memory is platform-specific and not attributed reliably by this harness",
        "cache_provenance": {
            "workspace": "cold StudyEngineSession created by process; exact repeat in same process; reopening uses a new empty session",
            "cli": "each CLI command is a fresh process; exact repeat reruns from frozen files",
        },
        "decision_scope": "computed software evidence; FAIL or INCONCLUSIVE outcomes are not successful engineering decisions",
    }
    (fixture_dir / "receipt.json").write_text(json.dumps(receipt, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--optcoil", required=True, help="path to an existing release optcoil CLI executable")
    parser.add_argument("--workspace-comparison", required=True, help="path to a built workflow_comparison example executable")
    parser.add_argument("--run-dir", required=True, help="new ignored runs directory for receipts and artifacts")
    args = parser.parse_args()
    cli = str(Path(args.optcoil).resolve())
    workspace_exe = str(Path(args.workspace_comparison).resolve())
    for exe in (cli, workspace_exe):
        if not Path(exe).is_file() or not os.access(exe, os.X_OK):
            parser.error(f"executable not found or not executable: {exe}")
    run_root = Path(args.run_dir).resolve()
    try:
        run_root.relative_to((REPO / "runs").resolve())
    except ValueError:
        parser.error("--run-dir must be a new directory under the ignored repository runs/ directory")
    if run_root.exists():
        parser.error(f"run directory already exists: {run_root}")
    run_root.mkdir(parents=True)
    manifest = load_json(MANIFEST)
    receipts = [run_fixture(spec, cli, workspace_exe, run_root) for spec in manifest["fixtures"]]
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, text=True, capture_output=True, check=True).stdout.strip()
    worktree = subprocess.run(["git", "status", "--short"], cwd=REPO, text=True, capture_output=True, check=True).stdout.splitlines()
    source_hashes = {str(path.relative_to(REPO)): sha256(path.read_bytes())
                     for path in sorted((REPO / "crates").rglob("*.rs"))
                     if "target" not in path.parts}
    rustc = subprocess.run(["rustc", "--version"], cwd=REPO, text=True, capture_output=True, check=False)
    cpu_model = None
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        cpu_model = next((line.split(":", 1)[1].strip() for line in cpuinfo.read_text().splitlines()
                          if line.startswith("model name") and ":" in line), None)
    summary = {
        "schema": "converra-workflow-comparison-suite/v1",
        "fixture_count": len(receipts),
        "environment": {"observed_utc": datetime.now(timezone.utc).isoformat(),
                        "platform": platform.platform(), "machine": platform.machine(),
                        "cpu_model": cpu_model, "logical_cpu_count": os.cpu_count(),
                        "rustc_version": rustc.stdout.strip(), "python_version": platform.python_version(),
                        "execution_threads": 1, "run_order": "sequential CLI then workspace for each fixture"},
        "all_input_hashes_frozen": True,
        "all_semantic_records_equivalent": all(
            row["comparison"]["record_equivalence"][v]["semantic_record_equal_excluding_runtime"]
            for row in receipts for v in ("base", "revised")
        ),
        "all_exact_repeats_semantically_equal": all(
            row["comparison"]["cli_exact_repeat_semantically_equal"] for row in receipts
        ),
        "all_receipts": [row["fixture_id"] for row in receipts],
        "run_directory": str(run_root),
        "source": {
            "git_revision": revision,
            "rust_source_sha256": source_hashes,
            "harness_sha256": sha256(Path(__file__).read_bytes()),
            "build_input_sha256": {str(path.relative_to(REPO)): sha256(path.read_bytes())
                                   for path in [REPO / "Cargo.toml", REPO / "rust-toolchain.toml", *sorted((REPO / "crates").glob("*/Cargo.toml")), *sorted((REPO / ".cargo").glob("*.toml"))]},
            "fixture_manifest_sha256": sha256(MANIFEST.read_bytes()),
            "worktree_status": worktree,
            "cargo_lock_sha256": sha256((REPO / "Cargo.lock").read_bytes()),
            "optcoil_binary_sha256": sha256(Path(cli).read_bytes()),
            "workspace_example_binary_sha256": sha256(Path(workspace_exe).read_bytes()),
        },
        "human_effort_measurement": False,
        "customer_validation": False,
    }
    (run_root / "suite-receipt.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0 if (summary["all_semantic_records_equivalent"]
                 and summary["all_exact_repeats_semantically_equal"]) else 2


if __name__ == "__main__":
    raise SystemExit(main())
