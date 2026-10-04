"""Smoke tests for the reel bindings; run with pytest after `maturin develop`."""

import hashlib
import json
import pathlib

import pytest

import converra

EXAMPLES = pathlib.Path(__file__).resolve().parents[3] / "examples" / "reels"
PASSPORT = (EXAMPLES / "passport-illustrative.json").read_text()
INVENTORY = (EXAMPLES / "inventory-illustrative.json").read_text()


def test_validate_passport_hashes_the_exact_text():
    summary = json.loads(converra.validate_reel_passport(PASSPORT))
    assert summary["reel_id"] == "ILLUSTRATIVE-REEL-A"
    assert summary["passport_sha256"] == hashlib.sha256(PASSPORT.encode()).hexdigest()
    assert summary["usable_length_m"] == 45.0


def test_validate_inventory_reports_totals():
    summary = json.loads(converra.validate_reel_inventory(INVENTORY))
    assert summary["summary"]["reel_count"] == 3
    assert summary["evidence_class"] == "synthetic"


def test_invalid_documents_raise_runtime_error():
    with pytest.raises(RuntimeError):
        converra.validate_reel_passport("{}")
    with pytest.raises(RuntimeError):
        converra.rate_reel_inventory(INVENTORY, -1.0, 2.0, 0.0)


def test_rate_inventory_and_dataset_identity():
    record = json.loads(converra.rate_reel_inventory(INVENTORY, 25.0, 2.0, 0.0))
    assert record["schema"] == "optcoil-reel-rating/v1"
    assert [r["status"] for r in record["reels"]] == ["rated", "no_product_map", "rated"]
    assert record["datasets"][0]["source"] == "embedded"

    outside = json.loads(converra.rate_reel_inventory(INVENTORY, 25.0, 12.0, 0.0))
    assert outside["reels"][0]["status"] == "outside_map_domain"

    data = EXAMPLES.parents[1] / "data" / "materials" / "robinson-superpower-ap-v3"
    bundle = {
        "schema": "optcoil-material-dataset/v1",
        "metadata": json.loads((data / "material.json").read_text()),
        "csv_data": (data / "measurements.csv").read_text(),
    }
    supplied = json.loads(
        converra.rate_reel_inventory(
            INVENTORY, 25.0, 2.0, 0.0, dataset_jsons=[json.dumps(bundle)]
        )
    )
    assert supplied["datasets"][0]["source"] == "supplied"

    # Same id, different measurements and a self-consistent hash: not the
    # dataset the passport pins.
    csv = bundle["csv_data"].replace("293111.0", "293112.0")
    bundle["csv_data"] = csv
    bundle["metadata"]["csv_sha256"] = hashlib.sha256(csv.encode()).hexdigest()
    mismatched = json.loads(
        converra.rate_reel_inventory(
            INVENTORY, 25.0, 2.0, 0.0, dataset_jsons=[json.dumps(bundle)]
        )
    )
    assert mismatched["reels"][0]["status"] == "map_unavailable"


def test_synthesize_is_deterministic_and_rateable():
    spec = (EXAMPLES / "synthetic-spec.json").read_text()
    inventory_json, truth_json = converra.synthesize_reel_inventory(spec)
    assert (inventory_json, truth_json) == converra.synthesize_reel_inventory(spec)
    truth = json.loads(truth_json)
    assert truth["inventory_sha256"] == hashlib.sha256(inventory_json.encode()).hexdigest()
    assert len(truth["reels"]) == 20
    summary = json.loads(converra.validate_reel_inventory(inventory_json))
    assert summary["summary"]["evidence_class_counts"]["synthetic"] == 20
    record = json.loads(converra.rate_reel_inventory(inventory_json, 25.0, 2.0, 0.0))
    assert record["status_counts"] == {"rated": 20}

    other = json.loads(spec)
    other["seed"] += 1
    assert converra.synthesize_reel_inventory(json.dumps(other))[0] != inventory_json

    other["profile"]["map_reference_row"] = 999999
    with pytest.raises(RuntimeError):
        converra.synthesize_reel_inventory(json.dumps(other))
