#!/usr/bin/env python3
"""Prepare the Robinson/THEVA Pro-Line Advanced Pinning dataset from its archived XLSX source.

Adapted from `prepare_oc003.py`; this workbook shares the original
`sheet.xml` layout, so only the dataset identity and metadata differ. The
selection window is identical to the other embedded Robinson datasets.
Only Python standard-library modules are needed. No network access.
"""

import argparse
import csv
import hashlib
import io
import json
import math
import zipfile
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "data/materials/robinson-theva-ap-v2"
SOURCE_SHA256 = "1ff61a69539e3ed942ee98729daee6f045ea8dc2a486f95dce456e9719d21fc2"
HEADERS = ["Temperature (K)", "Field (T)", "Angle (°)", "Ic/w (A/cm)", "Ic (A)", "n", "V0 (µV)", "V1 (µV/A)", "Hall field (T)", "Hall angle (°)", "Set temperature (K)", "Set field (T)", "Set angle (°)"]
# Same declared characterization window as robinson-superpower-ap-v3 and
# robinson-shanghai-hflt-v3 so all three datasets cover the identical
# operating region. The THEVA workbook measures a denser 105-135 deg
# angle wedge inside 0-180; those nodes are kept (the interpolator's
# cell spans handle non-uniform angle spacing, as they already do for
# the non-uniform field grid).
TEMPERATURES = [20.0, 25.0, 30.0, 35.0, 40.0]
FIELDS = [1.0, 1.5, 2.0, 3.0, 5.0, 7.0, 8.0]


def read_source():
    raw = (DATA / "source/All data.xlsx").read_bytes()
    if hashlib.sha256(raw).hexdigest() != SOURCE_SHA256:
        raise ValueError("published workbook hash mismatch")
    ns = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        strings = ["".join(element.itertext()) for element in ET.fromstring(archive.read("xl/sharedStrings.xml")).findall("s:si", ns)]
        sheet = ET.fromstring(archive.read("xl/worksheets/sheet.xml"))
    for row in sheet.findall("s:sheetData/s:row", ns):
        values = [None] * 13
        for cell in row:
            # The published workbook has exactly columns A..M.
            column = cell.get("r").rstrip("0123456789")
            if len(column) != 1 or not "A" <= column <= "M":
                raise ValueError("unexpected source column")
            value = cell.find("s:v", ns)
            if value is not None:
                values[ord(column) - ord("A")] = strings[int(value.text)] if cell.get("t") == "s" else float(value.text)
        yield int(row.get("r")), values


def prepare():
    rows = list(read_source())
    if rows[0][1] != HEADERS:
        raise ValueError("published workbook header changed")
    selected, excluded = [], []
    identities = set()
    for row_number, v in rows[1:]:
        if v[10] not in TEMPERATURES or v[11] not in FIELDS or not 0 <= v[12] <= 180:
            excluded.append({"source_row": row_number, "reason": "outside_frozen_characterization_region"})
            continue
        if any(not isinstance(v[k], float) or not math.isfinite(v[k]) for k in [0, 1, 3, 4, 5, 9, 10, 11, 12]):
            excluded.append({"source_row": row_number, "reason": "missing_or_nonfinite_required_measurement"})
            continue
        if v[1] <= 0 or v[3] <= 0 or v[4] <= 0 or v[5] <= 1:
            excluded.append({"source_row": row_number, "reason": "invalid_positive_measurement_or_fit_exponent"})
            continue
        key = tuple(v[10:13])
        if key in identities:
            raise ValueError(f"duplicate nominal node requires an explicit repeat policy: {key}")
        identities.add(key)
        # Unwrap the oriented Hall angle to the turn nearest its commanded
        # angle. This is a 360-degree coordinate equivalence, not Ic symmetry.
        angle = v[9] + 360 * round((v[12] - v[9]) / 360)
        selected.append([row_number, v[10], v[11], v[12], v[0], v[1], angle, v[3] * 100, v[4], v[5]])
    selected.sort(key=lambda r: tuple(r[1:4]))
    buffer = io.StringIO(newline="")
    writer = csv.writer(buffer, lineterminator="\n")
    writer.writerow(["source_row", "nominal_temperature_k", "nominal_field_t", "nominal_angle_deg", "temperature_k", "applied_field_t", "angle_from_normal_deg", "ic_a_per_m", "bridge_ic_a", "n_value"])
    writer.writerows(selected)
    csv_bytes = buffer.getvalue().encode()
    metadata = {
        "schema": "optcoil-measured-material/v1",
        "id": "robinson-theva-ap-v2",
        "data_class": "measured",
        "material": "THEVA Pro-Line Advanced Pinning 2G HTS; TPL4622-AP ID 230044",
        "sample_id": "THV202",
        "source_doi": "10.6084/m9.figshare.25893079.v2",
        "source_url": "https://figshare.com/articles/dataset/Critical_current_characterisation_of_THEVA_Pro-Line_Advanced_Pinning_2G_HTS_superconducting_wire/25893079/2",
        "authors": ["Stuart Wimbush", "Nick Strickland", "Andres Pantoja"],
        "license": "CC-BY-4.0",
        "license_url": "https://creativecommons.org/licenses/by/4.0/",
        "measurement_dates": "2024-04-16 to 2024-05-15",
        "source_xlsx_sha256": SOURCE_SHA256,
        "csv_sha256": hashlib.sha256(csv_bytes).hexdigest(),
        "preparation_source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_description_sha256": hashlib.sha256((DATA / "source/Description.txt").read_bytes()).hexdigest(),
        "electric_field_criterion_v_per_m": 0.0001,
        "voltage_tap_spacing_m": 0.005,
        "measured_bridge_width_m": 0.0005,
        "original_tape_width_m": 0.006,
        "field_basis": "applied_field_including_sample_self_field_response",
        "angle_convention": "oriented_from_tape_normal_in_maximum_lorentz_geometry",
        "coordinate_policy": "Measured temperature, source Field, and Hall angle; commanded coordinates define connectivity only. Hall angle unwrapped by full turns toward commanded angle.",
        "normalization": "Published Ic/w (A/cm) multiplied by 100 to A/m; no scaling to a commercial full-width tape capacity.",
        "measurement_uncertainty_fraction": None,
        "strain_state": "as_measured_not_quantified",
        "selection": {"nominal_temperature_k": TEMPERATURES, "nominal_field_t": FIELDS, "nominal_angle_range_deg": [0.0, 180.0]},
        "max_cell_spans": {"temperature_k": 10.0, "field_ratio": 2.5, "angle_deg": 10.0},
        "point_count": len(selected),
        "limitations": [
            "One patterned 0.5 mm bridge from a 6 mm tape, not a manufacturer lot guarantee or measured full-width tape capacity.",
            "Applied-field transport data includes sample self-field effects; not an intrinsic local Jc(B) law.",
            "Maximum Lorentz-force geometry only; no arbitrary field azimuth, strain model or quantified measurement uncertainty.",
            "No angle reflection/folding or out-of-domain extrapolation is assumed.",
            "Selection is a declared characterization region, not coverage of all fusion magnet operating conditions.",
            "Measured on a 6 mm tape; per-width capacity does not account for width-dependent slitting or substrate differences in other widths.",
            "Angle mesh is denser at 105-135 deg (2 deg source setpoints) than elsewhere (5 deg); the declared 0-180 deg window matches the other Robinson datasets."
        ]
    }
    audit = {
        "source_rows": len(rows) - 1,
        "selected_rows": len(selected),
        "excluded_rows": len(excluded),
        "exclusions": excluded,
        "max_temperature_offset_from_command_k": max(abs(r[4] - r[1]) for r in selected),
        "max_field_offset_from_command_t": max(abs(r[5] - r[2]) for r in selected),
        "max_angle_offset_from_command_deg": max(abs(r[6] - r[3]) for r in selected)
    }
    return {"measurements.csv": csv_bytes, "material.json": (json.dumps(metadata, indent=2) + "\n").encode(), "preparation-audit.json": (json.dumps(audit, indent=2) + "\n").encode()}


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
    print(json.dumps({"points": metadata["point_count"], "csv_sha256": metadata["csv_sha256"], **{k: v for k,v in audit.items() if k != "exclusions"}}, indent=2))


if __name__ == "__main__":
    main()
