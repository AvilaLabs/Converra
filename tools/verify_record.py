#!/usr/bin/env python3
"""verify_record.py — check a Converra run record's bindings and ledger arithmetic.

Stdlib-only. Usage:

    verify_record.py RECORD.json [CASE.json] [--manifest M.json] [--dataset B.json]

What this proves:
  * every declared SHA-256 binding that covers raw artifact bytes recomputes
    (manifest case/record hashes, dataset CSV hash);
  * a supplied case file is logically identical to the case embedded in the
    record;
  * every candidate's cost ledger is internally consistent;
  * best/baseline selection agrees with the ledger and verdicts.

What it does NOT prove: that the physics, screening, or acceptance verdicts are
correct, and it cannot recompute `case_sha256` — that hash covers the engine's
typed serialization, which requires the case contract implementation. Bindings
establish *which inputs produced a record*; engineering validation is separate
and remains the customer's responsibility.
"""
import hashlib
import json
import sys
from pathlib import Path

VERDICTS = {"PASS", "FAIL", "INCONCLUSIVE", "NOT_EVALUATED"}
RUN_SCHEMA = "optcoil-coupled-search-run/"
MANIFEST_SCHEMA = "optcoil-evidence-manifest/"
BUNDLE_SCHEMA = "optcoil-material-dataset/"
GEO_KEYS = ("turns_along_normal", "tapes_along_width", "strands_parallel")

results = []


def check(name, outcome, detail=""):
    results.append(outcome)
    print(f"{outcome:12} {name}: {detail}")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def strip_sha(value):
    return (value or "").removeprefix("sha256:")


def deep_diff(a, b, path="$", out=None, limit=10):
    """Collect structural diffs between two parsed JSON values."""
    if out is None:
        out = []
    if len(out) >= limit:
        return out
    if type(a) is not type(b) and not (
        isinstance(a, (int, float)) and isinstance(b, (int, float))
    ):
        out.append(f"{path}: type {type(a).__name__} vs {type(b).__name__}")
        return out
    if isinstance(a, dict):
        for k in a.keys() | b.keys():
            if k not in a:
                out.append(f"{path}.{k}: missing in first")
            elif k not in b:
                out.append(f"{path}.{k}: missing in second")
            else:
                deep_diff(a[k], b[k], f"{path}.{k}", out, limit)
    elif isinstance(a, list):
        if len(a) != len(b):
            out.append(f"{path}: len {len(a)} vs {len(b)}")
        else:
            for i, (x, y) in enumerate(zip(a, b)):
                deep_diff(x, y, f"{path}[{i}]", out, limit)
    elif a != b:
        out.append(f"{path}: {a!r} vs {b!r}")
    return out


def main():
    argv = sys.argv[1:]
    positional, flags = [], {}
    i = 0
    while i < len(argv):
        if argv[i].startswith("--"):
            flags[argv[i]] = argv[i + 1]
            i += 2
        else:
            positional.append(argv[i])
            i += 1
    if not positional:
        print(__doc__)
        sys.exit(2)

    record = json.loads(Path(positional[0]).read_text())
    case = record.get("case", {})
    choices = case.get("choices", {})
    cands = record.get("candidates", [])

    check("record.schema",
          "PASS" if record.get("schema", "").startswith(RUN_SCHEMA) else "FAIL",
          record.get("schema", "<missing>"))
    check("search_status",
          "PASS" if record.get("search_status") in VERDICTS else "FAIL",
          str(record.get("search_status")))

    # --- Case file binding ---
    if len(positional) > 1:
        file_case = json.loads(Path(positional[1]).read_text())
        diffs = deep_diff(file_case, case)
        check("embedded case == file",
              "PASS" if not diffs else "FAIL",
              "identical" if not diffs else "; ".join(diffs[:3]))
        check("case_sha256 (engine-serialized)", "DECLARED",
              strip_sha(record.get("case_sha256"))[:24] + "… — recomputation "
              "requires the case contract implementation")
    else:
        check("case binding", "NOT_CHECKED", "no case file supplied")

    # --- Manifest bindings (raw artifact bytes) ---
    if "--manifest" in flags:
        man = json.loads(Path(flags["--manifest"]).read_text())
        mroot = Path(flags["--manifest"]).parent
        check("manifest.schema",
              "PASS" if man.get("schema", "").startswith(MANIFEST_SCHEMA) else "FAIL",
              man.get("schema", "<missing>"))
        for key, supplied in (("case", positional[1] if len(positional) > 1 else None),
                              ("run_record", positional[0])):
            entry = man.get(key, {})
            expect = strip_sha(entry.get("sha256"))
            path = supplied or (mroot / entry.get("path", ""))
            if Path(path).exists():
                actual = sha256_file(path)
                check(f"manifest.{key}.sha256",
                      "PASS" if actual == expect else "FAIL",
                      f"{Path(path).name}: {actual[:16]}…")
            else:
                check(f"manifest.{key}.sha256", "NOT_CHECKED", f"{path} not found")
        ids = man.get("identities", {})
        ok = (ids.get("coupled_search_model_id") == record.get("coupled_search_model_id")
              and ids.get("coupled_search_checker_id") == record.get("coupled_search_checker_id")
              and ids.get("dataset_id") == record.get("dataset_id")
              and ids.get("dataset_csv_sha256") == record.get("dataset_csv_sha256"))
        check("manifest.identities", "PASS" if ok else "FAIL",
              "model/checker/dataset identities agree with record" if ok
              else "identity mismatch vs record")

    # --- Dataset bundle binding ---
    if "--dataset" in flags:
        bundle = json.loads(Path(flags["--dataset"]).read_text())
        meta = bundle.get("metadata", {})
        csv_sha = sha256_bytes(bundle.get("csv_data", "").encode())
        ok = (bundle.get("schema", "").startswith(BUNDLE_SCHEMA)
              and csv_sha == record.get("dataset_csv_sha256")
              and meta.get("id") == record.get("dataset_id")
              and meta.get("csv_sha256") == csv_sha)
        check("dataset bundle binding", "PASS" if ok else "FAIL",
              f"id={meta.get('id')} csv_sha={csv_sha[:16]}…")
    else:
        check("dataset bundle binding", "NOT_CHECKED",
              f"record declares {record.get('dataset_id')}")

    # --- Structure ---
    n_expected = 1
    for key in GEO_KEYS:
        vals = choices.get(key)
        if isinstance(vals, list) and vals:
            n_expected *= len(vals)
    check("candidate count",
          "PASS" if len(cands) == n_expected else "FAIL",
          f"{len(cands)} candidates vs {n_expected} declared grid points")

    statuses = {c.get("status") for c in cands}
    check("verdict vocabulary",
          "PASS" if statuses <= VERDICTS else "FAIL", f"seen: {sorted(statuses)}")

    # --- Ledger arithmetic (independent of physics) ---
    price = case.get("cost", {}).get("price_usd_per_m")
    bad = []
    for idx, c in enumerate(cands):
        cost = c.get("cost", {})
        cond = round(cost.get("installed_length_m", 0) * price, 2)
        total = round(sum(cost.get(k, 0) for k in
                      ("conductor_usd", "scrap_usd", "assembly_usd", "joints_usd")), 2)
        if (abs(cost.get("conductor_usd", -1) - cond) > 0.01
                or abs(cost.get("total_usd", -1) - total) > 0.01):
            bad.append(idx)
    check("ledger arithmetic", "PASS" if not bad else "FAIL",
          f"{len(cands)}/{len(cands)} candidates recompute exactly" if not bad
          else f"mismatch at candidates {bad[:5]}")

    # --- Optimum / baseline selection ---
    passed = [i for i, c in enumerate(cands) if c.get("status") == "PASS"]
    best = record.get("best_index")
    if passed:
        argmin = min(passed, key=lambda i: cands[i]["cost"]["total_usd"])
        ok = best == argmin and cands[best].get("status") == "PASS"
        check("best_index", "PASS" if ok else "FAIL",
              f"best={best} ${cands[best]['cost']['total_usd']:,.0f} "
              f"(recomputed argmin={argmin})" if best is not None else "absent")
    else:
        check("best_index", "PASS" if best is None else "FAIL", "no PASS candidates")

    base, bi = case.get("baseline", {}), record.get("baseline_index")
    if bi is not None and bi < len(cands):
        g = cands[bi].get("geometry", {})
        ok = all(g.get(k) == base.get(k) for k in GEO_KEYS if k in base)
        check("baseline_index", "PASS" if ok else "FAIL",
              f"baseline {tuple(base.get(k) for k in GEO_KEYS if k in base)} -> "
              f"candidate {tuple(g.get(k) for k in GEO_KEYS if k in g)}")
    else:
        check("baseline_index", "FAIL", f"baseline_index={bi}")

    # --- Acceptance + identities ---
    acc = record.get("acceptance", {})
    check("acceptance.agreement",
          "PASS" if acc.get("agreement_status") in VERDICTS else "FAIL",
          str(acc.get("agreement_status")))
    check("declared identities",
          "PASS" if all(record.get(k) for k in (
              "coupled_search_model_id", "coupled_search_checker_id", "input_sha256",
              "implementation_sha256", "dataset_id", "dataset_csv_sha256")) else "FAIL",
          "model/checker/input/impl/dataset identities present")

    n = {o: results.count(o) for o in ("PASS", "FAIL", "NOT_CHECKED", "DECLARED")}
    print(f"\n{n['PASS']} PASS, {n['FAIL']} FAIL, {n['NOT_CHECKED']} NOT_CHECKED, "
          f"{n['DECLARED']} DECLARED")
    print("Verified: artifact bindings, structure, and ledger arithmetic. "
          "Not verified: physics and engineering acceptance.")
    sys.exit(1 if n["FAIL"] else 0)


if __name__ == "__main__":
    main()
