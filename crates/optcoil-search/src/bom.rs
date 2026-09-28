//! Bill of materials (`optcoil-bom/v1`): the purchasable artifact a
//! completed coupled search implies — conductor meters per spec and per
//! contiguous turn range, installed vs purchased length under the declared
//! scrap fraction, joint and pancake counts, and the same cost figures
//! the record's ledger carries, partitioned per spec.
//!
//! The BOM is derived from the record's own optimum: per-turn installed
//! lengths come from `turn_ledger_rows` — the same walk the production
//! cost ledger sums — so a BOM total that disagreed with the record's
//! ledger is a defect, not a view difference; `totals_agree` reports the
//! check at 1e-9 relative and the record's own figures are stored beside
//! the derived ones.
//!
//! This is a modeled procurement document, not a purchase order: prices
//! are the case's declared (often synthetic) figures, lengths are the
//! winding model's, and joints/pancakes are the ledger's own counts —
//! splices from spool length limits, lead lengths, insulation and
//! tooling are all unmodeled.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use optcoil_model::coupled_search::BASE_TAPE_SPEC_ID;

use crate::{
    RunError,
    coupled_search::{CandidateGeometry, CoupledSearchRunRecord, turn_ledger_rows},
};

pub const BOM_SCHEMA: &str = "optcoil-bom/v1";

/// One conductor spec's procurement row: every turn range it covers,
/// the meters to buy, and its share of the ledger cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomSpecRow {
    pub spec_id: String,
    /// The dataset the spec's binding actually resolved to (record's
    /// `dataset_id` for `base`, `spec_datasets[id]` otherwise).
    pub dataset_id: String,
    pub price_usd_per_m: f64,
    /// Contiguous turn ranges (1-based, inclusive) this spec covers, in
    /// winding order — a spec can recur in separated regions.
    pub turn_ranges: Vec<[u32; 2]>,
    pub turn_count: u32,
    /// Installed conductor metres across all conductor layers (tapes ×
    /// strands, or strands under the axial-normal v13 fold).
    pub installed_length_m: f64,
    /// Metres to buy — `installed × (1 + scrap_fraction)` on legacy
    /// records, the pieces actually bought under a v24 piece policy.
    pub purchased_length_m: f64,
    pub conductor_usd: f64,
    /// Scrap priced at this spec's own price — scrapped premium tape is
    /// scrapped at the premium price.
    pub scrap_usd: f64,
    /// v24 piece plan: the offering's piece length, pieces, splices and
    /// remnant — `None` on records without `piece_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_length_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_splices: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remnant_length_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomRecord {
    pub schema: String,
    pub optcoil_version: String,
    /// SHA-256 of the source record file bytes — the BOM's evidence basis.
    pub source_record_sha256: String,
    /// The run record's own `case_sha256`.
    pub case_sha256: String,
    /// The selected optimum's geometry — turns/tapes/strands/assignment.
    pub geometry: CandidateGeometry,
    /// Conductor layers multiplying each turn row's strand-length (tapes ×
    /// strands, or strands under the axial-normal v13 fold).
    pub conductor_layers: u32,
    pub spec_rows: Vec<BomSpecRow>,
    /// Total physical joints — `tapes − 1` module interfaces on legacy
    /// records; module + piece + spec splices under a v24 piece policy.
    pub joint_count: u32,
    /// v24 joint breakdown — `None` on records without `piece_policy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_joints: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piece_splices: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_splices: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pieces_bought: Option<u32>,
    /// v24: tape-metres bought beyond installed-plus-attrition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remnant_length_m: Option<f64>,
    pub joints_usd: f64,
    /// `tapes` — pancake/assembly count.
    pub pancake_count: u32,
    pub assembly_usd: f64,
    pub total_installed_length_m: f64,
    pub total_purchased_length_m: f64,
    /// Σ spec conductor + scrap + assembly + joints — the BOM's own
    /// recomputation of the record's `total_usd`.
    pub total_usd: f64,
    /// The record ledger's own figures, stored for comparison.
    pub record_installed_length_m: f64,
    pub record_total_usd: f64,
    /// True iff the derived totals match the record's ledger at 1e-9
    /// relative — a disagreement is a defect, not a view difference.
    pub totals_agree: bool,
    pub limitations: Vec<String>,
}

impl BomRecord {
    pub fn write_new(&self, path: impl AsRef<std::path::Path>) -> Result<(), RunError> {
        crate::write_json_new(self, path)
    }
}

/// Build the BOM from a serialized coupled-search run record: the optimum
/// candidate's geometry and spec assignment drive the same per-turn ledger
/// walk the search priced. Errors when the record has no PASS optimum —
/// there is no design to buy.
pub fn bom_from_record(record_json: &str) -> Result<BomRecord, RunError> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("run record does not parse: {e}")))?;
    let best = record
        .best_index
        .map(|i| &record.candidates[i])
        .ok_or_else(|| RunError::Invalid("record has no PASS optimum — nothing to buy".into()))?;

    let search = &record.case;
    let geometry = &best.geometry;
    let assignment: Vec<String> = geometry.tape_spec_ids.clone().unwrap_or_default();
    let strands = geometry.strands_parallel;

    // Schema v21: the optimum's axis-resolved racetrack dims ride the
    // candidate record — absent fields mean the case's fixed geometry.
    let dims = geometry.dims();
    let (rows, tapes_per_turn) = turn_ledger_rows(
        search,
        geometry.turns_along_normal,
        geometry.tapes_along_width,
        &assignment,
        dims,
    );
    let conductor_layers = if tapes_per_turn > 1.0 {
        strands
    } else {
        geometry.tapes_along_width * strands
    };

    let dataset_of = |spec_id: &str| -> String {
        if spec_id == BASE_TAPE_SPEC_ID {
            record.dataset_id.clone()
        } else {
            record
                .spec_datasets
                .get(spec_id)
                .map(|d| d.id.clone())
                .unwrap_or_else(|| spec_id.to_string())
        }
    };

    // Group turns per spec (preserving first-appearance order), then
    // collapse each spec's turns into contiguous ranges.
    let mut order: Vec<String> = Vec::new();
    let mut by_spec: BTreeMap<String, Vec<(u32, f64)>> = BTreeMap::new();
    for (k, (spec_id, length_k)) in rows.iter().enumerate() {
        let turn = k as u32 + 1;
        if !by_spec.contains_key(spec_id) {
            order.push(spec_id.clone());
        }
        by_spec
            .entry(spec_id.clone())
            .or_default()
            .push((turn, *length_k));
    }

    let piece_plan = best.cost.piece_plan.as_deref();
    let mut spec_rows = Vec::with_capacity(order.len());
    for spec_id in &order {
        let turns = &by_spec[spec_id];
        let mut ranges: Vec<[u32; 2]> = Vec::new();
        for &(turn, _) in turns {
            match ranges.last_mut() {
                Some(last) if turn == last[1] + 1 => last[1] = turn,
                _ => ranges.push([turn, turn]),
            }
        }
        let length_sum: f64 = turns.iter().map(|(_, l)| l).sum();
        let installed = length_sum * f64::from(conductor_layers);
        let plan = piece_plan.and_then(|plans| plans.iter().find(|p| &p.spec_id == spec_id));
        let price = plan.map(|p| p.price_usd_per_m).unwrap_or_else(|| {
            search
                .spec_price_usd_per_m(spec_id)
                .expect("turn spec resolves on a validated case")
        });
        let conductor_usd = installed * price;
        let (purchased, scrap_usd) = match plan {
            Some(p) => (
                p.purchased_length_m,
                (p.purchased_length_m - installed) * price,
            ),
            None => (
                installed * (1.0 + search.cost.scrap_fraction),
                conductor_usd * search.cost.scrap_fraction,
            ),
        };
        spec_rows.push(BomSpecRow {
            spec_id: spec_id.clone(),
            dataset_id: dataset_of(spec_id),
            price_usd_per_m: price,
            turn_ranges: ranges,
            turn_count: turns.len() as u32,
            installed_length_m: installed,
            purchased_length_m: purchased,
            conductor_usd,
            scrap_usd,
            piece_length_m: plan.map(|p| p.piece_length_m),
            pieces: plan.map(|p| p.pieces),
            piece_splices: plan.map(|p| p.piece_splices),
            remnant_length_m: plan.map(|p| p.remnant_length_m),
        });
    }

    let total_installed: f64 = spec_rows.iter().map(|r| r.installed_length_m).sum();
    let total_purchased: f64 = spec_rows.iter().map(|r| r.purchased_length_m).sum();
    let conductor_total: f64 = spec_rows.iter().map(|r| r.conductor_usd).sum();
    let scrap_total: f64 = spec_rows.iter().map(|r| r.scrap_usd).sum();
    let (module_joints, piece_splices, spec_splices) = if piece_plan.is_some() {
        (
            best.cost.module_joints.unwrap_or(0),
            best.cost.piece_splices.unwrap_or(0),
            best.cost.spec_splices.unwrap_or(0),
        )
    } else {
        (geometry.tapes_along_width.saturating_sub(1), 0, 0)
    };
    let joint_count = module_joints + piece_splices + spec_splices;
    let joints_usd = search.cost.joint_cost_usd * f64::from(module_joints)
        + search
            .cost
            .piece_policy
            .as_ref()
            .map(|p| p.splice_cost_usd * f64::from(piece_splices + spec_splices))
            .unwrap_or(0.0);
    let pancake_count = geometry.tapes_along_width;
    let assembly_usd = search.cost.assembly_cost_per_pancake_usd * f64::from(pancake_count);
    let total_usd = conductor_total + scrap_total + assembly_usd + joints_usd;

    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0);
    let totals_agree = close(total_installed, best.cost.installed_length_m)
        && close(total_usd, best.cost.total_usd);

    Ok(BomRecord {
        schema: BOM_SCHEMA.into(),
        optcoil_version: env!("CARGO_PKG_VERSION").into(),
        source_record_sha256: format!("{:x}", Sha256::digest(record_json.as_bytes())),
        case_sha256: record.case_sha256.clone(),
        geometry: geometry.clone(),
        conductor_layers,
        spec_rows,
        joint_count,
        module_joints: piece_plan.map(|_| module_joints),
        piece_splices: piece_plan.map(|_| piece_splices),
        spec_splices: piece_plan.map(|_| spec_splices),
        pieces_bought: best.cost.pieces_bought,
        remnant_length_m: piece_plan.map(|_| best.cost.remnant_length_m.unwrap_or(0.0)),
        joints_usd,
        pancake_count,
        assembly_usd,
        total_installed_length_m: total_installed,
        total_purchased_length_m: total_purchased,
        total_usd,
        record_installed_length_m: best.cost.installed_length_m,
        record_total_usd: best.cost.total_usd,
        totals_agree,
        limitations: vec![
            if piece_plan.is_some() {
                "A modeled procurement document, not a purchase order: prices are the case's declared figures (see each price_source — synthetic stays synthetic), and piece/splice counts follow the declared piece_policy's boundary and unit semantics. Lead lengths, insulation, tooling, splice resistance/QA yield and freight are unmodeled.".into()
            } else {
                "A modeled procurement document, not a purchase order: prices are the case's declared (often synthetic) figures, lengths are the winding model's installed metres plus the declared scrap fraction. Spool-length splices, lead lengths, insulation, tooling and freight are all unmodeled.".into()
            },
            "Derived from the record's optimum through the same per-turn ledger walk the search priced; totals_agree reports the 1e-9-relative reconciliation against the record's own ledger. A disagreement is a defect in the derivation, not a difference of opinion.".into(),
        ],
    })
}

/// Render the procurement-facing document — markdown a buyer can paste
/// into an RFQ package. Built from the same `bom_from_record` derivation
/// (so its figures reconcile with the record's ledger at 1e-9) plus the
/// case's declared price provenance. This is a modeled schedule, not a
/// purchase order — the limitations text says so on the document itself.
pub fn rfq_markdown_from_record(record_json: &str) -> Result<String, RunError> {
    let record: CoupledSearchRunRecord = serde_json::from_str(record_json)
        .map_err(|e| RunError::Invalid(format!("run record does not parse: {e}")))?;
    let bom = bom_from_record(record_json)?;
    let case = &record.case;
    let source_of = |spec_id: &str| -> String {
        let src = if spec_id == BASE_TAPE_SPEC_ID {
            case.cost.price_source
        } else {
            case.tape_specs
                .as_ref()
                .and_then(|m| m.get(spec_id))
                .and_then(|s| s.price_source)
        };
        match src {
            Some(optcoil_model::coupled_search::PriceSource::Synthetic) => "synthetic".into(),
            Some(optcoil_model::coupled_search::PriceSource::Estimated) => "estimated".into(),
            Some(optcoil_model::coupled_search::PriceSource::Published) => "published".into(),
            Some(optcoil_model::coupled_search::PriceSource::Quoted) => "quoted".into(),
            None => "undeclared".into(),
        }
    };
    let g = &bom.geometry;
    let mut out = format!(
        "# Request for quotation — HTS conductor\n\nCase `{}` · record `{}` · case `{}`\n\nWinding: {} turns × {} tapes × {} strands",
        case.id,
        &bom.source_record_sha256[..16],
        &bom.case_sha256[..16],
        g.turns_along_normal,
        g.tapes_along_width,
        g.strands_parallel,
    );
    if let Some(ids) = &g.tape_spec_ids {
        out.push_str(&format!(" — spec assignment {}", ids.join(", ")));
    }
    out.push_str("\n\n## Conductor schedule\n\n| Spec | Dataset | Piece m | Pieces | Purchase m | $/m | Source |\n|---|---|---|---|---|---|---|\n");
    for r in &bom.spec_rows {
        let (piece, count) = match (r.piece_length_m, r.pieces) {
            (Some(l), Some(n)) => (format!("{l:.1}"), n.to_string()),
            _ => ("—".into(), "—".into()),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.1} | {:.2} | {} |\n",
            r.spec_id,
            r.dataset_id,
            piece,
            count,
            r.purchased_length_m,
            r.price_usd_per_m,
            source_of(&r.spec_id),
        ));
    }
    out.push_str("\n## Joints & assembly\n\n");
    match (bom.module_joints, bom.piece_splices, bom.spec_splices) {
        (Some(m), Some(p), Some(s)) => out.push_str(&format!(
            "- Module-interface joints: {m}\n- In-winding splices: {p} piece-exhaustion + {s} spec-change\n- Pancakes/assemblies: {}\n",
            bom.pancake_count
        )),
        _ => out.push_str(&format!(
            "- Module-interface joints: {}\n- Pancakes/assemblies: {}\n",
            bom.joint_count, bom.pancake_count
        )),
    }
    out.push_str(&format!(
        "\n## Totals\n\n- Conductor + scrap/remnant: ${:.0}\n- Joints & splices: ${:.0}\n- Assembly: ${:.0}\n- **Total: ${:.0}**\n\nInstalled {:.1} m · purchased {:.1} m",
        bom.spec_rows
            .iter()
            .map(|r| r.conductor_usd + r.scrap_usd)
            .sum::<f64>(),
        bom.joints_usd,
        bom.assembly_usd,
        bom.total_usd,
        bom.total_installed_length_m,
        bom.total_purchased_length_m,
    ));
    if let Some(remnant) = bom.remnant_length_m {
        out.push_str(&format!(" · remnant {remnant:.1} m"));
    }
    out.push_str(&format!(
        "\n\nLedger reconciliation vs record: {}\n\n## Basis & limitations\n\n",
        if bom.totals_agree {
            "**agrees** (1e-9 relative)"
        } else {
            "**DISAGREES — defect in the derivation, do not send**"
        }
    ));
    for lim in &bom.limitations {
        out.push_str(&format!("- {lim}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::coupled_search::{
        CoupledSearchOptions, run_coupled_search_case, run_coupled_search_case_with_dataset,
        tests::{reduced_case_json, reduced_graded_spec_case_json},
    };

    fn run_record(case_json: &str) -> String {
        let record =
            run_coupled_search_case(case_json, &CoupledSearchOptions { threads: None }).unwrap();
        serde_json::to_string(&record).unwrap()
    }

    #[test]
    fn bom_partitions_lengths_and_reconciles_totals() {
        // A genuinely graded optimum: the weak spec (scaled_ic 0.725 —
        // declared synthetic) serves the low-field outer half only, so
        // the search mixes base inside + weak outside. The BOM must split
        // installed metres and dollars per spec and still land on the
        // record's own ledger totals.
        let base_ds =
            optcoil_model::material::MaterialDataset::embedded_by_id("robinson-superpower-ap-v3")
                .unwrap();
        let weak = base_ds.scaled_ic(0.725).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&reduced_graded_spec_case_json(
            "[300]",
            300,
            10.0,
            &weak.metadata.id,
            &weak.metadata.csv_sha256,
        ))
        .unwrap();
        v["requirement"]["b_target_t"] = 1.5.into();
        v["fixed_geometry"]["radial_pitch_m"] = 0.0005.into();
        let json = serde_json::to_string(&v).unwrap();
        let record = run_coupled_search_case_with_dataset(
            &json,
            &CoupledSearchOptions::default(),
            Some(weak),
            &AtomicBool::new(false),
        )
        .unwrap();
        let record = serde_json::to_string(&record).unwrap();
        let bom = bom_from_record(&record).unwrap();

        assert!(
            bom.totals_agree,
            "BOM totals must reconcile with the ledger"
        );
        // A genuinely mixed optimum yields two spec rows.
        assert_eq!(bom.spec_rows.len(), 2);
        for row in &bom.spec_rows {
            assert!(row.installed_length_m > 0.0);
            assert!(row.purchased_length_m > row.installed_length_m);
            assert!(!row.turn_ranges.is_empty());
            assert_eq!(
                row.turn_count,
                row.turn_ranges.iter().map(|r| r[1] - r[0] + 1).sum::<u32>()
            );
        }
        // Turn ranges tile the winding without overlap.
        let covered: u32 = bom.spec_rows.iter().map(|r| r.turn_count).sum();
        assert_eq!(covered, bom.geometry.turns_along_normal);
        // Per-spec installed metres sum to the ledger's installed length.
        let installed_sum: f64 = bom.spec_rows.iter().map(|r| r.installed_length_m).sum();
        assert!((installed_sum - bom.record_installed_length_m).abs() < 1e-6);
        // Joint and pancake counts are the ledger's own derivations.
        assert_eq!(bom.pancake_count, bom.geometry.tapes_along_width);
        assert_eq!(bom.joint_count, bom.geometry.tapes_along_width - 1);
    }

    #[test]
    fn bom_rejects_a_record_with_no_optimum() {
        // turns=4 fails screening everywhere — no PASS optimum, nothing
        // to buy.
        let record = run_record(&reduced_case_json("[3, 4]", 30.0));
        let parsed: CoupledSearchRunRecord = serde_json::from_str(&record).unwrap();
        if parsed.best_index.is_none() {
            assert!(bom_from_record(&record).is_err());
        }
    }

    #[test]
    fn bom_ungraded_case_reports_a_single_base_row() {
        let record = run_record(&reduced_case_json("[3, 200]", 30.0));
        let bom = bom_from_record(&record).unwrap();
        assert_eq!(bom.spec_rows.len(), 1);
        assert_eq!(bom.spec_rows[0].spec_id, "base");
        assert!(bom.totals_agree);
    }

    #[test]
    fn rfq_markdown_carries_schedule_totals_and_provenance() {
        let record = run_record(&reduced_case_json("[3, 200]", 30.0));
        let doc = rfq_markdown_from_record(&record).unwrap();
        // The procurement-facing content: schedule, totals, the
        // reconciliation check, and the limitations that keep the
        // document honest.
        for marker in [
            "Request for quotation",
            "Conductor schedule",
            "| base |",
            "Module-interface joints",
            "**Total: $",
            "Installed",
            "purchased",
            "Ledger reconciliation vs record: **agrees**",
            "not a purchase order",
        ] {
            assert!(doc.contains(marker), "RFQ missing '{marker}'");
        }
    }
}
