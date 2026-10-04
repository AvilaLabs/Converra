#!/usr/bin/env python3
"""Convert the Robinson Research Institute REBCO "All data.xlsx" workbooks to canonical CSVs.

CR-01 data acquisition: unlike `prepare_ffj_ybco.py`, `prepare_theva_ap.py`
and `prepare_shanghai_hflt.py`, nothing is filtered by temperature or field
(65-85 K and 0 T / self-field rows are kept). The column conventions are the
same: measured temperature, source Field, and the Hall angle unwrapped by full
turns toward the commanded angle; Ic/w (A/cm) is multiplied by 100 to A/m.

Column positions are never assumed: the header row is matched by name and any
unrecognised header, unheaded column carrying data, cell type or text value
aborts the run. Rows that cannot be converted are skipped and logged with a
reason; converted + skipped always equals the source row count.

The coverage inventory contains coordinates and row counts only. No critical
current statistic is computed. Only Python standard-library modules are
needed. No network access.

Usage:
    cr01_prepare_robinson.py [--root .local/research/cr01/robinson]
"""

import argparse
import collections
import csv
import hashlib
import io
import json
import math
import re
import zipfile
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ROOT = ROOT / ".local/research/cr01/robinson"
NS = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
CSV_COLUMNS = ["source_row", "nominal_temperature_k", "nominal_field_t", "nominal_angle_deg", "temperature_k", "applied_field_t", "angle_from_normal_deg", "ic_a_per_m", "bridge_ic_a", "n_value"]

# Normalised header text -> role. Matching ignores case and whitespace and
# folds both micro signs; nothing else is tolerated.
BASE_HEADERS = {
    "temperature (k)": "temperature",
    "field (t)": "field",
    "angle (°)": "angle",
    "ic/w (a/cm)": "icw",
    "ic (a)": "ic",
    "n": "n",
    "v0 (uv)": "v0",
    "v1 (uv/a)": "v1",
    "hall field (t)": "hall_field",
    "hall angle (°)": "hall_angle",
}
SET_HEADERS = {
    "set temperature (k)": "set_temperature",
    "set field (t)": "set_field",
    "set angle (°)": "set_angle",
}
# Unheaded columns that carry data in a specific workbook. Each is ignored
# only because it is listed here with its reason; any other unheaded column
# with values raises.
UNHEADED_ALLOWED = {
    "sunam-han04200": {"cols": {"K", "L", "M"}, "reason": "unheaded columns K-M hold constant placeholder values; this 2015 workbook has no setpoint columns"},
    "sunam-san04200": {"cols": {"N"}, "reason": "unheaded column N holds a numeric flag (0 or 1025) with no documented meaning"},
}
MISSING_TEXT = {"--", "NaN"}
# Instrument zero offset on the field reading (e.g. -0.001 T at nominal 0 T).
FIELD_ZERO_OFFSET_T = 0.01


def norm_header(text):
    return re.sub(r"\s+", " ", text.replace("µ", "u").replace("μ", "u").strip().lower())


def column_of(ref):
    return ref.rstrip("0123456789")


def read_workbook(path):
    """Yield (row_number, {column_letter: value}) with value float, 'MISSING' or str (header row only)."""
    with zipfile.ZipFile(path) as archive:
        sheets = [n for n in archive.namelist() if n.startswith("xl/worksheets/") and n.endswith(".xml")]
        if len(sheets) != 1:
            raise ValueError(f"{path}: expected exactly one worksheet, found {sheets}")
        strings = ["".join(e.itertext()) for e in ET.fromstring(archive.read("xl/sharedStrings.xml")).findall("s:si", NS)]
        sheet = ET.fromstring(archive.read(sheets[0]))
    for index, row in enumerate(sheet.findall("s:sheetData/s:row", NS)):
        number = int(row.get("r"))
        values = {}
        for cell in row:
            v = cell.find("s:v", NS)
            if v is None:
                continue
            kind = cell.get("t")
            col = column_of(cell.get("r"))
            if kind == "s":
                text = strings[int(v.text)]
                if index == 0:
                    values[col] = text
                elif text in MISSING_TEXT:
                    values[col] = "MISSING"
                else:
                    try:
                        values[col] = float(text)
                    except ValueError:
                        raise ValueError(f"{path}: row {number} column {col}: unrecognised text {text!r}")
            elif kind in (None, "n"):
                values[col] = float(v.text)
            else:
                raise ValueError(f"{path}: row {number} column {col}: unsupported cell type {kind!r}")
        yield number, values


def map_headers(slug, header):
    roles, set_roles, unheaded = {}, {}, []
    for col, text in header.items():
        key = norm_header(text)
        if key in BASE_HEADERS:
            role = BASE_HEADERS[key]
        elif key in SET_HEADERS:
            role = SET_HEADERS[key]
            set_roles[role] = col
            continue
        else:
            raise ValueError(f"{slug}: unrecognised header {text!r} in column {col}")
        if role in roles:
            raise ValueError(f"{slug}: duplicate header for {role}")
        roles[role] = col
    missing = set(BASE_HEADERS.values()) - set(roles)
    if missing:
        raise ValueError(f"{slug}: missing headers {sorted(missing)}")
    if set_roles and len(set_roles) != 3:
        raise ValueError(f"{slug}: partial setpoint columns {sorted(set_roles)}")
    return roles, set_roles


def convert(slug, workbook):
    """Return (csv_rows, log). csv_rows are lists matching CSV_COLUMNS (nominal may be None)."""
    rows = list(read_workbook(workbook))
    header_number, header = rows[0]
    if header_number != 1:
        raise ValueError(f"{slug}: header is not row 1")
    roles, set_roles = map_headers(slug, header)
    named = set(roles.values()) | set(set_roles.values())
    allowed = UNHEADED_ALLOWED.get(slug, {"cols": set()})["cols"]
    unheaded_counts = collections.defaultdict(collections.Counter)
    out, skipped, clamped = [], [], []
    reasons = collections.Counter()
    for number, values in rows[1:]:
        extra = set(values) - named
        if extra - allowed:
            raise ValueError(f"{slug}: row {number}: data in unheaded column(s) {sorted(extra - allowed)}")
        for col in extra:
            unheaded_counts[col][values[col]] += 1
        required = ["temperature", "field", "hall_angle", "icw", "ic", "n"]
        coords = required[:3]
        fit = required[3:]
        get = lambda role: values.get(roles[role], "MISSING")
        nominal = [values.get(set_roles[r], "MISSING") for r in ("set_temperature", "set_field", "set_angle")] if set_roles else None
        bad = [r for r in coords if not isinstance(get(r), float) or not math.isfinite(get(r))]
        if bad:
            reason = "measured_coordinates_missing_or_nan"
        elif any(not isinstance(get(r), float) or not math.isfinite(get(r)) for r in fit):
            reason = "ic_fit_missing"
        elif nominal is not None and any(not isinstance(x, float) or not math.isfinite(x) for x in nominal):
            reason = "setpoint_missing_or_nan"
        else:
            reason = None
        if reason is None:
            t, b, ha = get("temperature"), get("field"), get("hall_angle")
            icw, ic, n = get("icw"), get("ic"), get("n")
            if t <= 0:
                reason = "nonpositive_temperature"
            elif icw <= 0 or ic <= 0:
                reason = "nonpositive_ic"
            elif n <= 1:
                reason = "fit_exponent_not_above_1"
        if reason is not None:
            reasons[reason] += 1
            skipped.append({"source_row": number, "reason": reason})
            continue
        if b < 0:
            if b < -FIELD_ZERO_OFFSET_T:
                raise ValueError(f"{slug}: row {number}: field {b} below the zero-offset tolerance")
            clamped.append(number)
            b = 0.0
        if nominal is None:
            # No setpoint columns: nominal coordinates are left empty; the
            # source Angle column is the angle used only to pick the turn.
            ref_angle = get("angle")
            if not math.isfinite(ref_angle):
                raise ValueError(f"{slug}: row {number}: no usable angle reference")
            nominal_cells = ["", "", ""]
        else:
            ref_angle = nominal[2]
            nominal_cells = nominal
        # Unwrap the oriented Hall angle to the turn nearest its commanded
        # angle. This is a 360-degree coordinate equivalence, not Ic symmetry.
        angle = ha + 360 * round((ref_angle - ha) / 360)
        out.append([number, *nominal_cells, t, b, angle, icw * 100, ic, n])
    source_rows = len(rows) - 1
    if len(out) + len(skipped) != source_rows:
        raise ValueError(f"{slug}: row accounting mismatch")
    log = {
        "slug": slug,
        "source_rows": source_rows,
        "converted_rows": len(out),
        "skipped_rows": len(skipped),
        "skip_reason_counts": dict(reasons),
        "has_setpoint_columns": bool(set_roles),
        "header_column_map": {**roles, **set_roles},
        "field_clamped_to_zero_rows": clamped,
        "field_zero_offset_tolerance_t": FIELD_ZERO_OFFSET_T,
        "ignored_unheaded_columns": {col: {"values": {str(k): v for k, v in c.most_common(5)}, "reason": UNHEADED_ALLOWED[slug]["reason"]} for col, c in unheaded_counts.items()},
        "skipped": skipped,
    }
    return out, log


def write_csv(path, rows):
    buffer = io.StringIO(newline="")
    writer = csv.writer(buffer, lineterminator="\n")
    writer.writerow(CSV_COLUMNS)
    writer.writerows(rows)
    path.write_bytes(buffer.getvalue().encode())


def describe(description_path):
    text = description_path.read_bytes().decode("cp1252")
    lines = [l.strip() for l in text.splitlines()]
    pick = lambda prefix: next((l.split(":", 1)[1].strip() for l in lines if l.startswith(prefix)), None)
    sample = pick("Sample description")
    dates = pick("Data collection date")
    internal = pick("Internal sample designation")
    vc = re.search(r"voltage criterion Vc = ([0-9.]+) [µu]V for a voltage tap spacing of ([0-9.]+) mm", text)
    bridge = re.search(r"([0-9.]+) mm x ([0-9.]+) mm bridge", sample or "")
    # Original tape width: the first "<n> mm width" / "<n> mm laminated width" in the sample line.
    width = re.search(r"([0-9.]+) mm (?:laminated )?width", sample or "")
    insert = re.search(r"([0-9.]+) mm wide HTS insert", sample or "")
    criterion = None
    if vc:
        criterion = float(vc.group(1)) * 1e-6 / (float(vc.group(2)) * 1e-3)
    return {
        "sample_description": sample,
        "sample_id": internal.split(".")[0].strip() if internal else None,
        "internal_and_manufacturer_designation": internal,
        "measurement_dates": dates,
        "datafile_format_version": pick("Datafile format version"),
        "bridge_width_mm": float(bridge.group(1)) if bridge else None,
        "bridge_length_mm": float(bridge.group(2)) if bridge else None,
        "bridge_width_note": None if bridge else "no bridge width stated; the sample width is the measured width",
        "original_tape_width_mm": float(width.group(1)) if width else None,
        "hts_insert_width_mm": float(insert.group(1)) if insert else None,
        "voltage_criterion_uv": float(vc.group(1)) if vc else None,
        "voltage_tap_spacing_mm": float(vc.group(2)) if vc else None,
        "electric_field_criterion_v_per_m": criterion,
        "has_corrections_file": "Corrections.txt" in text,
    }


def modal_step(values):
    steps = [round(b - a, 6) for a, b in zip(values, values[1:])]
    if not steps:
        return None
    count = collections.Counter(steps)
    return {"min": min(steps), "max": max(steps), "modal": count.most_common(1)[0][0], "distinct_steps": len(count)}


def inventory(slug, source, rows, log, desc):
    has_nom = log["has_setpoint_columns"]
    # Without setpoints the coordinates below are derived from the measured values and labelled as such.
    tkey = lambda r: r[1] if has_nom else round(r[4] * 2) / 2
    bkey = lambda r: r[2] if has_nom else round(r[5], 3)
    akey = lambda r: r[3] if has_nom else round(r[6] / 5) * 5
    temps = sorted({tkey(r) for r in rows})
    fields = sorted({bkey(r) for r in rows})
    angles = sorted({akey(r) for r in rows})
    per_temp = collections.Counter(tkey(r) for r in rows)
    nodes = collections.Counter((tkey(r), bkey(r), akey(r)) for r in rows)
    in77 = [r for r in rows if 77 <= tkey(r) <= 77.5]
    selffield77 = [r for r in in77 if bkey(r) == 0]
    low = [r for r in rows if 65 <= tkey(r) <= 77.5 and bkey(r) <= 4]
    low_pos = [r for r in low if bkey(r) > 0]
    cold = [r for r in rows if 20 <= tkey(r) <= 40 and bkey(r) >= 5]
    angle_gap = sum(1 for r in rows if abs(r[6] - (r[3] if has_nom else r[6])) > 10)
    return {
        "slug": slug,
        "figshare_id": source["figshare_id"],
        "figshare_version": source["version"],
        "doi": source["doi"],
        "title": source["title"],
        "not_a_tape": source["not_a_tape"],
        "manufacturer_product": re.sub(r" 2G.*$| superconducting.*$", "", source["title"].replace("Critical current characterisation of ", "")),
        **desc,
        "coordinate_basis": "setpoint_columns" if has_nom else "derived_from_measured_(temperature_to_0.5K, field_to_0.001T, angle_to_5deg)_no_setpoint_columns",
        "source_rows": log["source_rows"],
        "converted_rows": log["converted_rows"],
        "skipped_rows": log["skipped_rows"],
        "skip_reason_counts": log["skip_reason_counts"],
        "temperatures_k": temps,
        "measured_temperature_range_k": [min(r[4] for r in rows), max(r[4] for r in rows)],
        "fields_t": fields,
        "measured_field_range_t": [min(r[5] for r in rows), max(r[5] for r in rows)],
        "max_field_t": max(bkey(r) for r in rows),
        "angle_span_deg": [min(akey(r) for r in rows), max(akey(r) for r in rows)],
        "distinct_angles": len(angles),
        "angle_step_deg": modal_step(angles),
        "measured_angle_range_deg": [min(r[6] for r in rows), max(r[6] for r in rows)],
        "rows_per_temperature_k": {str(k): per_temp[k] for k in temps},
        "repeated_nodes_count": sum(1 for c in nodes.values() if c > 1),
        "rows_in_repeated_nodes": sum(c for c in nodes.values() if c > 1),
        "temperatures_in_77_to_77.5_k": [t for t in temps if 77 <= t <= 77.5],
        "has_self_field_0t_rows_at_77_to_77.5_k": bool(selffield77),
        "self_field_0t_rows_at_77_to_77.5_k": len(selffield77),
        "rows_at_65_to_77.5_k_field_le_4t": len(low),
        "rows_at_65_to_77.5_k_field_gt_0_le_4t": len(low_pos),
        "has_data_65_to_77.5_k_field_le_4t": bool(low),
        "rows_at_20_to_40_k_field_ge_5t": len(cold),
        "has_data_20_to_40_k_field_ge_5t": bool(cold),
        "rows_with_zero_field": sum(1 for r in rows if r[5] == 0),
        "rows_with_hall_angle_more_than_10deg_from_setpoint": angle_gap if has_nom else None,
        "quirks": quirks(slug, log, has_nom, angle_gap, desc),
    }


def quirks(slug, log, has_nom, angle_gap, desc):
    q = []
    if not has_nom:
        q.append("no setpoint columns in the source workbook; nominal_* CSV columns are empty and inventory coordinates are derived from measured values")
    if log["ignored_unheaded_columns"]:
        for col, info in log["ignored_unheaded_columns"].items():
            q.append(f"unheaded column {col} ignored: {info['reason']}")
    if log["field_clamped_to_zero_rows"]:
        q.append(f"{len(log['field_clamped_to_zero_rows'])} rows with measured field in [-{FIELD_ZERO_OFFSET_T}, 0) T set to 0.0 T (instrument offset)")
    if log["skip_reason_counts"]:
        q.append("skipped rows: " + ", ".join(f"{k}={v}" for k, v in sorted(log["skip_reason_counts"].items())))
    if desc["bridge_width_mm"] is None:
        q.append("no patterned bridge stated in the description; the full sample width was measured")
    if desc["has_corrections_file"]:
        q.append("description lists a Corrections.txt (manual fit corrections) that was not downloaded")
    if has_nom and angle_gap:
        q.append(f"{angle_gap} converted rows have a Hall angle more than 10 deg from the setpoint turn")
    return q


def cross_check(root, results):
    """Compare converted rows with the embedded data/materials CSVs (identity by source_row)."""
    pairs = {
        "faraday-factory-japan-ybco": "robinson-ffj-ybco-v1",
        "shanghai-superconductor-high-field-low-temperature": "robinson-shanghai-hflt-v3",
        "superpower-advanced-pinning": "robinson-superpower-ap-v3",
        "theva-pro-line-advanced-pinning": "robinson-theva-ap-v2",
    }
    report = {}
    for slug, material in pairs.items():
        meta = json.loads((ROOT / "data/materials" / material / "material.json").read_text())
        embedded = list(csv.DictReader(open(ROOT / "data/materials" / material / "measurements.csv")))
        mine = {r[0]: r for r in results[slug]["rows"]}
        mine_text = {}
        for r in results[slug]["rows"]:
            mine_text[r[0]] = [str(x) for x in r]
        mismatches, missing = [], []
        for e in embedded:
            n = int(e["source_row"])
            if n not in mine:
                missing.append(n)
                continue
            theirs = [e[c] for c in CSV_COLUMNS]
            if [x for x in mine_text[n][1:]] != theirs[1:]:
                numeric_equal = all(float(a) == float(b) for a, b in zip(mine_text[n][1:], theirs[1:]))
                mismatches.append({"source_row": n, "equal_as_floats": numeric_equal, "converted": mine_text[n][1:], "embedded": theirs[1:]})
        ids = {int(e["source_row"]) for e in embedded}
        windows = [e for e in embedded]
        temps = sorted({float(e["nominal_temperature_k"]) for e in embedded})
        fields = sorted({float(e["nominal_field_t"]) for e in embedded})
        # Converted rows inside the embedded selection window that the embedded CSV does not contain.
        extra = [r[0] for r in results[slug]["rows"] if r[1] in temps and r[2] in fields and 0 <= r[3] <= 180 and r[0] not in ids]
        report[slug] = {
            "embedded_material": material,
            "embedded_rows": len(embedded),
            "source_sha256_matches_embedded": meta["source_xlsx_sha256"] == results[slug]["xlsx_sha256"],
            "embedded_rows_not_in_converted": missing,
            "mismatching_rows": mismatches,
            "converted_rows_inside_embedded_window_but_absent_from_embedded": extra,
            "window_nominal_temperatures_k": temps,
            "window_nominal_fields_t": fields,
        }
    return report


def validate(slug, rows, log):
    problems = []
    if len(rows) + log["skipped_rows"] != log["source_rows"]:
        problems.append("row accounting mismatch")
    if any(r[7] <= 0 or r[8] <= 0 for r in rows):
        problems.append("nonpositive Ic per width")
    if any(not 4 <= r[4] <= 95 for r in rows):
        problems.append(f"measured temperature outside 4-95 K: min {min(r[4] for r in rows)}, max {max(r[4] for r in rows)}")
    if any(r[5] < 0 for r in rows):
        problems.append("negative field")
    if any(not all(math.isfinite(x) for x in (r[4], r[5], r[6], r[7], r[8], r[9])) for r in rows):
        problems.append("nonfinite value")
    if len({r[0] for r in rows}) != len(rows):
        problems.append("duplicate source_row")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    args = parser.parse_args()
    root = args.root
    sources = json.loads((root / "sources.json").read_text())["sources"]
    results, inventories, validation, total = {}, [], {}, 0
    for source in sources:
        slug = source["slug"]
        workbook = root / slug / "source/All data.xlsx"
        raw = workbook.read_bytes()
        digest = hashlib.sha256(raw).hexdigest()
        recorded = next(f["sha256"] for f in source["files"] if f["name"] == "All data.xlsx")
        if digest != recorded:
            raise ValueError(f"{slug}: workbook hash differs from sources.json")
        rows, log = convert(slug, workbook)
        write_csv(root / slug / "measurements.csv", rows)
        log["xlsx_sha256"] = digest
        (root / slug / "conversion-log.json").write_text(json.dumps(log, indent=1) + "\n")
        results[slug] = {"rows": rows, "xlsx_sha256": digest}
        validation[slug] = validate(slug, rows, log)
        inventories.append(inventory(slug, source, rows, log, describe(root / slug / "source/Description.txt")))
        total += len(rows)
    cross = cross_check(root, results)
    (root / "inventory.json").write_text(json.dumps({"datasets": inventories, "total_converted_rows": total, "validation_problems": validation, "cross_check": cross}, indent=1) + "\n")
    (root / "inventory.md").write_text(render_markdown(inventories, total, validation, cross))
    print(json.dumps({"datasets": len(inventories), "total_converted_rows": total, "validation_problems": {k: v for k, v in validation.items() if v}, "cross_check": {k: {"embedded_rows": v["embedded_rows"], "mismatches": len(v["mismatching_rows"]), "missing": len(v["embedded_rows_not_in_converted"]), "extra_in_window": len(v["converted_rows_inside_embedded_window_but_absent_from_embedded"]), "hash_match": v["source_sha256_matches_embedded"]} for k, v in cross.items()}}, indent=1))


def render_markdown(inventories, total, validation, cross):
    out = ["# CR-01 Robinson REBCO coverage inventory", "", "Coordinates and row counts only; no critical-current statistics. Generated by `tools/cr01_prepare_robinson.py`.", "", f"Datasets: {len(inventories)}; total converted rows: {total}.", ""]
    out += ["| dataset | rows (src/conv) | T range K | max B T | 0 T @77-77.5K | <=4 T @65-77.5K | >=5 T @20-40K | bridge/tape mm |", "|---|---|---|---|---|---|---|---|"]
    for i in inventories:
        flag = " (NOT A TAPE)" if i["not_a_tape"] else ""
        out.append(f"| {i['slug']}{flag} | {i['source_rows']}/{i['converted_rows']} | {i['temperatures_k'][0]}-{i['temperatures_k'][-1]} | {i['max_field_t']} | {'yes' if i['has_self_field_0t_rows_at_77_to_77.5_k'] else 'no'} | {i['rows_at_65_to_77.5_k_field_le_4t']} | {i['rows_at_20_to_40_k_field_ge_5t']} | {i['bridge_width_mm']}/{i['original_tape_width_mm']} |")
    for i in inventories:
        step = i["angle_step_deg"]
        out += ["", f"## {i['slug']}" + (" (not a tape: film on sapphire)" if i["not_a_tape"] else ""), "",
                f"- {i['manufacturer_product']}; figshare {i['figshare_id']} v{i['figshare_version']}; DOI {i['doi']}",
                f"- sample {i['sample_id']}; measured {i['measurement_dates']}; format version {i['datafile_format_version']}",
                f"- bridge width {i['bridge_width_mm']} mm, original tape width {i['original_tape_width_mm']} mm; electric-field criterion {i['electric_field_criterion_v_per_m']} V/m ({i['voltage_criterion_uv']} uV over {i['voltage_tap_spacing_mm']} mm)",
                f"- coordinate basis: {i['coordinate_basis']}",
                f"- temperatures K: {i['temperatures_k']}",
                f"- fields T: {i['fields_t']}",
                f"- angle span {i['angle_span_deg']} deg, {i['distinct_angles']} distinct, step {step}",
                f"- rows per temperature: {i['rows_per_temperature_k']}",
                f"- repeated nodes: {i['repeated_nodes_count']} ({i['rows_in_repeated_nodes']} rows); zero-field rows: {i['rows_with_zero_field']}",
                f"- 0 T at 77-77.5 K: {i['has_self_field_0t_rows_at_77_to_77.5_k']} ({i['self_field_0t_rows_at_77_to_77.5_k']} rows; temperatures there {i['temperatures_in_77_to_77.5_k']})",
                f"- 65-77.5 K, field <= 4 T: {i['rows_at_65_to_77.5_k_field_le_4t']} rows ({i['rows_at_65_to_77.5_k_field_gt_0_le_4t']} with field > 0)",
                f"- 20-40 K, field >= 5 T: {i['rows_at_20_to_40_k_field_ge_5t']} rows",
                f"- max field {i['max_field_t']} T",
                f"- quirks: {'; '.join(i['quirks']) if i['quirks'] else 'none'}"]
    out += ["", "## Validation", ""]
    out += [f"- {k}: {'ok' if not v else '; '.join(v)}" for k, v in validation.items()]
    out += ["", "## Cross-check against embedded data/materials", ""]
    for k, v in cross.items():
        out.append(f"- {k} vs {v['embedded_material']}: {v['embedded_rows']} embedded rows, source hash match {v['source_sha256_matches_embedded']}, mismatches {len(v['mismatching_rows'])}, embedded rows missing {len(v['embedded_rows_not_in_converted'])}, converted-in-window-but-absent {len(v['converted_rows_inside_embedded_window_but_absent_from_embedded'])}")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    main()
