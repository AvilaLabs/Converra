//! Pure metric/formatting seams extracted from `views.rs` — record
//! pricing arithmetic, USD formatting and label helpers. No egui types
//! here: everything is a free function over record data so it can be
//! unit-tested headless.

use optcoil_model::{CostBreakdown, Status};
use optcoil_search::coupled_search::{CoupledSearchRunRecord, SearchCostLedger};

pub(super) fn strands_suffix(strands: u32) -> String {
    if strands > 1 {
        format!(" × {strands} strands")
    } else {
        String::new()
    }
}

/// Exact closed-form repricing of one cost ledger — mirrors
/// `optcoil-search::reprice`: conductor and scrap scale with $/m, assembly
/// and joints are price-independent. Verdicts never move; only dollars do.
/// v24 piece-policy ledgers price per-spec catalogues, not a scalar $/m —
/// the slider cannot reprice them; the record's own `total_usd` stands.
pub(crate) fn repriced_total_usd(
    ledger: &SearchCostLedger,
    price_usd_per_m: f64,
    scrap_fraction: f64,
) -> f64 {
    if ledger.piece_plan.is_some() {
        return ledger.total_usd;
    }
    ledger.installed_length_m * price_usd_per_m * (1.0 + scrap_fraction)
        + ledger.assembly_usd
        + ledger.joints_usd
}

/// Lifecycle at a hypothetical conductor price: repriced capex + the
/// opex term (declared heat loads are price-independent — they pass
/// through untouched).
pub(super) fn lifecycle_at_price(
    ledger: &SearchCostLedger,
    price_usd_per_m: f64,
    scrap_fraction: f64,
) -> f64 {
    repriced_total_usd(ledger, price_usd_per_m, scrap_fraction) + ledger.opex_usd.unwrap_or(0.0)
}

/// Cheapest PASS candidate at `price` — the record's own `best_index` when
/// the price equals the declared case price.
pub(super) fn best_index_at_price(
    record: &CoupledSearchRunRecord,
    price_usd_per_m: f64,
    scrap_fraction: f64,
) -> Option<usize> {
    if (price_usd_per_m - record.case.cost.price_usd_per_m).abs() <= f64::EPSILON {
        return record.best_index;
    }
    record
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.status == Status::Pass)
        .min_by(|(_, a), (_, b)| {
            repriced_total_usd(&a.cost, price_usd_per_m, scrap_fraction)
                .total_cmp(&repriced_total_usd(
                    &b.cost,
                    price_usd_per_m,
                    scrap_fraction,
                ))
                .then(a.geometry.total_turns.cmp(&b.geometry.total_turns))
                .then(a.index.cmp(&b.index))
        })
        .map(|(i, _)| i)
}

/// Compact USD for map cells: `$201k` / `$8.7k` / `$640`.
pub(super) fn usd_k(value: f64) -> String {
    if value.abs() >= 100_000.0 {
        format!("${:.0}k", value / 1000.0)
    } else if value.abs() >= 1_000.0 {
        format!("${:.1}k", value / 1000.0)
    } else {
        format!("${:.0}", value)
    }
}

/// Thousands-grouped USD, e.g. `$377,426.18`.
pub(super) fn usd(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let absolute = value.abs();
    let mut whole = absolute.trunc() as u64;
    let mut frac = ((absolute - whole as f64) * 100.0).round() as u64;
    if frac == 100 {
        whole += 1;
        frac = 0;
    }
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}${grouped}.{frac:02}")
}

pub(super) fn cost_values(cost: &CostBreakdown) -> [f64; 4] {
    [
        cost.installed_conductor_usd,
        cost.scrap_usd,
        cost.assembly_usd,
        cost.joints_usd,
    ]
}

pub(super) fn search_cost_components(
    ledger: &SearchCostLedger,
    price_usd_per_m: f64,
    scrap_fraction: f64,
) -> [(&'static str, f64); 5] {
    if ledger.piece_plan.is_some() {
        // v24: conductor/scrap are already priced per spec by the piece
        // plan — report the ledger's own columns, not a re-scaling.
        return [
            ("Conductor", ledger.conductor_usd),
            ("Scrap", ledger.scrap_usd),
            ("Assembly", ledger.assembly_usd),
            ("Joints", ledger.joints_usd),
            ("Opex", ledger.opex_usd.unwrap_or(0.0)),
        ];
    }
    [
        ("Conductor", ledger.installed_length_m * price_usd_per_m),
        (
            "Scrap",
            ledger.installed_length_m * price_usd_per_m * scrap_fraction,
        ),
        ("Assembly", ledger.assembly_usd),
        ("Joints", ledger.joints_usd),
        ("Opex", ledger.opex_usd.unwrap_or(0.0)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> SearchCostLedger {
        SearchCostLedger {
            installed_length_m: 100.0,
            purchased_length_m: 110.0,
            conductor_usd: 3000.0,
            scrap_usd: 300.0,
            assembly_usd: 500.0,
            joints_usd: 200.0,
            total_usd: 4000.0,
            ..Default::default()
        }
    }

    #[test]
    fn usd_groups_thousands_and_carries_cents() {
        assert_eq!(usd(377_426.18), "$377,426.18");
        assert_eq!(usd(1_234_567.891), "$1,234,567.89");
        assert_eq!(usd(0.0), "$0.00");
        assert_eq!(usd(42.5), "$42.50");
        // cents round-up carries into the whole part
        assert_eq!(usd(0.996), "$1.00");
        assert_eq!(usd(999.999), "$1,000.00");
        assert_eq!(usd(-1500.25), "-$1,500.25");
    }

    #[test]
    fn usd_k_picks_compact_scales() {
        assert_eq!(usd_k(640.0), "$640");
        assert_eq!(usd_k(8_700.0), "$8.7k");
        assert_eq!(usd_k(201_000.0), "$201k");
        assert_eq!(usd_k(-8_700.0), "$-8.7k");
        assert_eq!(usd_k(999.9), "$1000");
        assert_eq!(usd_k(1_000.0), "$1.0k");
        assert_eq!(usd_k(99_999.0), "$100.0k");
    }

    #[test]
    fn strands_suffix_only_for_multi_strand() {
        assert_eq!(strands_suffix(0), "");
        assert_eq!(strands_suffix(1), "");
        assert_eq!(strands_suffix(4), " × 4 strands");
    }

    #[test]
    fn repriced_scales_conductor_and_scrap_only() {
        let l = ledger();
        // 100 m × $50/m × 1.1 + 500 + 200
        let total = repriced_total_usd(&l, 50.0, 0.1);
        assert_eq!(total, 100.0 * 50.0 * 1.1 + 700.0);
        // piece-plan ledgers are catalogue-priced — the scalar slider
        // must not move them
        let mut piece = ledger();
        piece.piece_plan = Some(vec![]);
        piece.total_usd = 9_999.0;
        assert_eq!(repriced_total_usd(&piece, 50.0, 0.1), 9_999.0);
    }

    #[test]
    fn lifecycle_passes_opex_through_unchanged() {
        let mut l = ledger();
        l.opex_usd = Some(1234.0);
        let capex = repriced_total_usd(&l, 50.0, 0.1);
        assert_eq!(lifecycle_at_price(&l, 50.0, 0.1), capex + 1234.0);
        let mut none = ledger();
        none.opex_usd = None;
        assert_eq!(lifecycle_at_price(&none, 50.0, 0.1), capex);
    }

    #[test]
    fn search_cost_components_scale_or_report_priced_plan() {
        let l = ledger();
        let scaled = search_cost_components(&l, 50.0, 0.1);
        assert_eq!(scaled[0], ("Conductor", 5000.0));
        assert_eq!(scaled[1], ("Scrap", 500.0));
        assert_eq!(scaled[4], ("Opex", 0.0));

        let mut piece = ledger();
        piece.piece_plan = Some(vec![]);
        piece.conductor_usd = 111.0;
        piece.scrap_usd = 22.0;
        piece.opex_usd = Some(7.0);
        let priced = search_cost_components(&piece, 50.0, 0.1);
        assert_eq!(priced[0], ("Conductor", 111.0));
        assert_eq!(priced[1], ("Scrap", 22.0));
        assert_eq!(priced[4], ("Opex", 7.0));
    }
}
