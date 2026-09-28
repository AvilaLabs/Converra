#!/usr/bin/env python3
"""Reproduce the OC-006 low-field extension from the same archived XLSX source.

Reuses tools/prepare_oc003.py's parser and transformation functions
unmodified (imported, never copied or edited); only the selected Set field
range and the resulting metadata/limitations/README differ. Every row at
Set field >= 1 T must reproduce the corresponding robinson-superpower-ap-v3
row byte for byte -- this tool asserts that, and the total row count, before
writing anything. Python's standard library is sufficient; no network access.
"""

import argparse
import csv
import hashlib
import io
import json
import math
import statistics
from pathlib import Path

import prepare_oc003 as base

NEW_ID = "robinson-superpower-ap-v3-lowfield"
ORIGINAL_MEASUREMENTS_CSV = base.DATA / "measurements.csv"
# Captured before base.FIELDS is overridden below.
ORIGINAL_FIELDS = set(base.FIELDS)
LOWFIELD_FIELDS = [0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5, 0.7, 1.0, 1.5, 2.0, 3.0, 5.0, 7.0, 8.0]
EXPECTED_ROWS = len(base.TEMPERATURES) * len(LOWFIELD_FIELDS) * 43  # 5 x 15 x 43 = 3225
EXPECTED_HIGH_FIELD_ROWS = len(base.TEMPERATURES) * len(ORIGINAL_FIELDS) * 43  # 5 x 7 x 43 = 1505
IDENTITY_FIELDS = [
    "source_row",
    "nominal_temperature_k",
    "nominal_field_t",
    "nominal_angle_deg",
    "temperature_k",
    "applied_field_t",
    "angle_from_normal_deg",
    "ic_a_per_m",
    "bridge_ic_a",
    "n_value",
]

README = """# Robinson Research Institute / SuperPower AP, version 3 -- low-field extension

Third-party measurement data, **CC BY 4.0**. The project's MIT license covers source code only and does not replace the license of these data.

Attribution: Stuart Wimbush, Nick Strickland and Andres Pantoja, *Critical current characterisation of SuperPower Advanced Pinning 2G HTS superconducting wire*, Robinson Research Institute, Victoria University of Wellington. [Dataset DOI, version 3](https://doi.org/10.6084/m9.figshare.4256624.v3). [CC BY 4.0 license](https://creativecommons.org/licenses/by/4.0/). Same archived workbook as `data/materials/robinson-superpower-ap-v3/`, downloaded September 9, 2026.

## What this is

This directory extends `robinson-superpower-ap-v3` downward in applied field, for OC-006. The original dataset directory is untouched and stays byte-identical; it remains the dataset pinned by the frozen OC-003/OC-004/OC-005 benchmarks. This is a separate, independently identified dataset (`robinson-superpower-ap-v3-lowfield`) produced by a new tool, `tools/prepare_oc006.py`, which imports `tools/prepare_oc003.py`'s parser and transformation functions unmodified and changes only which Set field levels are selected. See `tools/prepare_oc006.py` for the exact transformation.

## What differs from the original dataset

- Selection adds nominal Set field 0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5 and 0.7 T to the original 1, 1.5, 2, 3, 5, 7 and 8 T, for 15 nominal field levels total (nominal temperature 20-40 K and angle 0-180 deg are unchanged). 3,225 rows (5 x 15 x 43), versus the original's 1,505 (5 x 7 x 43).
- Every row at Set field >= 1 T is byte-identical (source row, all coordinates, `ic_a_per_m`, `bridge_ic_a`, `n_value`) to the corresponding row of `robinson-superpower-ap-v3/measurements.csv`; `tools/prepare_oc006.py` asserts this before writing any output, and records the check's result in `preparation-audit.json`.
- `max_cell_spans` is unchanged (10 K, field ratio 2.5, 10 deg). Adjacent-level ratios across the full 15-level nominal grid stay well under that ceiling; the ceiling was already set to cover the wider gaps that appear once individual field planes are withheld for validation, which is unaffected by this extension.
- `limitations` gains the two entries described under Exclusion rule below; every original bridge/self-field/measurement-geometry/width caveat is preserved unchanged.
- `preparation-audit.json` additionally records the unwrapped Hall-angle-minus-set-angle statistics (mean, standard deviation, maximum absolute value) per Set field level for the 20-40 K rows, at every workbook field level from 0 T through 8 T -- including the levels this dataset excludes -- and the result of the >= 1 T identity check.

## Exclusion rule

Set field 0.01, 0.015, 0.02 and 0.03 T are excluded: below 0.05 T the unwrapped Hall angle drifts away from the commanded angle enough to risk inverting measured-coordinate tetrahedra at the dataset's 2 deg nominal angle spacing (the maximum deviation grows from about 2-3 deg near 0.05-1 T to 9.03 deg at 0.01 T; see the per-field table in `preparation-audit.json`). Set field 0 T is excluded outright: the Hall angle is meaningless there (tens of degrees of scatter), and zero applied field has no image under the logarithmic (ln B) interpolation model this dataset is meant to support. These exclusions are recorded in `preparation-audit.json` alongside every other excluded source row.

No manufacturer endorsement, lot guarantee, quantified uncertainty, strain coverage or engineering acceptance is implied. This dataset is a characterization artifact for OC-006; see `benchmarks/measured/oc-006.json` for the frozen validation benchmark and its pre-declared consequences.
"""


def hall_angle_offsets_by_set_field():
    """Unwrapped Hall-angle-minus-set-angle statistics for the 20-40 K,
    0-180 deg rows, grouped by every Set field level present in the archived
    workbook -- including levels this dataset excludes. Reuses
    prepare_oc003's own reader; nothing is written here."""
    rows = list(base.read_source())
    if rows[0][1] != base.HEADERS:
        raise ValueError("published workbook header changed")
    groups = {}
    for _row_number, v in rows[1:]:
        t_set, f_set, a_set, hall_angle = v[10], v[11], v[12], v[9]
        if not isinstance(t_set, float) or t_set not in base.TEMPERATURES:
            continue
        if not isinstance(f_set, float):
            continue
        if not isinstance(a_set, float) or not 0.0 <= a_set <= 180.0:
            continue
        if not isinstance(hall_angle, float) or not math.isfinite(hall_angle):
            continue
        unwrapped = hall_angle + 360 * round((a_set - hall_angle) / 360)
        groups.setdefault(f_set, []).append(unwrapped - a_set)
    table = {}
    for f_set in sorted(groups):
        deviations = groups[f_set]
        table[f"{f_set:g}"] = {
            "set_field_t": f_set,
            "count": len(deviations),
            "mean_deg": statistics.fmean(deviations),
            "stdev_deg": statistics.pstdev(deviations) if len(deviations) > 1 else 0.0,
            "max_abs_deg": max(abs(d) for d in deviations),
        }
    return table


def assert_high_field_rows_match_original(csv_bytes):
    """Every row at Set field >= 1 T must be byte-identical to the
    corresponding row of the original robinson-superpower-ap-v3 CSV. Reads
    that original CSV; never writes to it."""
    with ORIGINAL_MEASUREMENTS_CSV.open(newline="") as f:
        original_by_row = {int(r["source_row"]): r for r in csv.DictReader(f)}
    selected = list(csv.DictReader(io.StringIO(csv_bytes.decode())))
    if len(selected) != EXPECTED_ROWS:
        raise AssertionError(f"expected {EXPECTED_ROWS} rows (5 x 15 x 43), got {len(selected)}")
    checked = 0
    for row in selected:
        if float(row["nominal_field_t"]) not in ORIGINAL_FIELDS:
            continue
        source_row = int(row["source_row"])
        original = original_by_row.get(source_row)
        if original is None:
            raise AssertionError(
                f">= 1 T row source_row={source_row} is absent from the original dataset"
            )
        mismatched = [k for k in IDENTITY_FIELDS if row[k] != original[k]]
        if mismatched:
            raise AssertionError(
                f"source_row={source_row} differs from the original dataset in {mismatched}"
            )
        checked += 1
    if checked != EXPECTED_HIGH_FIELD_ROWS:
        raise AssertionError(
            f"expected {EXPECTED_HIGH_FIELD_ROWS} rows at Set field >= 1 T, checked {checked}"
        )
    return {
        "checked_rows": checked,
        "identical": True,
        "compared_against": "data/materials/robinson-superpower-ap-v3/measurements.csv",
    }


def prepare():
    base.FIELDS = LOWFIELD_FIELDS
    artifacts = base.prepare()
    csv_bytes = artifacts["measurements.csv"]

    identity_check = assert_high_field_rows_match_original(csv_bytes)

    metadata = json.loads(artifacts["material.json"])
    metadata["id"] = NEW_ID
    metadata["preparation_source_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    metadata["limitations"] = metadata["limitations"] + [
        "0-0.03 T rows excluded: the Hall-angle coordinate is unreliable below 0.05 T (unwrapped Hall-angle-minus-set-angle deviation grows to a maximum of 9.03 deg at 0.01 T; see preparation-audit.json for the full per-field table).",
        "Zero applied field is excluded: it is not representable in the logarithmic (ln B) interpolation model, and the Hall angle is meaningless there.",
    ]
    artifacts["material.json"] = (json.dumps(metadata, indent=2) + "\n").encode()

    audit = json.loads(artifacts["preparation-audit.json"])
    audit["hall_angle_offset_by_set_field_deg"] = hall_angle_offsets_by_set_field()
    audit["identity_check"] = identity_check
    artifacts["preparation-audit.json"] = (json.dumps(audit, indent=2) + "\n").encode()

    artifacts["README.md"] = README.encode()
    return artifacts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    artifacts = prepare()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for name in artifacts:
        if (args.output_dir / name).exists():
            raise FileExistsError(args.output_dir / name)
    for name, content in artifacts.items():
        with (args.output_dir / name).open("xb") as output:
            output.write(content)
    metadata = json.loads(artifacts["material.json"])
    audit = json.loads(artifacts["preparation-audit.json"])
    print(json.dumps({
        "points": metadata["point_count"],
        "csv_sha256": metadata["csv_sha256"],
        **{k: v for k, v in audit.items() if k != "exclusions"},
    }, indent=2))


if __name__ == "__main__":
    main()
