//! Pure metric/formatting seams extracted from `views.rs` — record
//! pricing arithmetic, USD formatting and label helpers. No egui types
//! here: everything is a free function over record data so it can be
//! unit-tested headless.

use optcoil_model::{CostBreakdown, Status, coupled_search::CoupledSearchCase};
use optcoil_search::coupled_search::{
    CoupledSearchRunRecord, SearchCandidateResult, SearchCostLedger,
};

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

/// Whether a single $/m control has an unambiguous meaning for this run.
/// Piece catalogues and per-spec prices cannot be represented by one
/// absolute scalar price.
pub(crate) fn scalar_repricing_supported(record: &CoupledSearchRunRecord) -> bool {
    record.case.cost.piece_policy.is_none()
        && record.case.grading.is_none()
        && record
            .case
            .tape_specs
            .as_ref()
            .is_none_or(std::collections::BTreeMap::is_empty)
        && record
            .candidates
            .iter()
            .all(|c| c.geometry.tape_spec_ids.is_none())
}

/// Price a ledger in its record context. For graded or piece-priced records
/// the declared price is still a valid display of the original ledger; a
/// hypothetical scalar price is not.
pub(crate) fn record_total_at_price(
    record: &CoupledSearchRunRecord,
    ledger: &SearchCostLedger,
    price_usd_per_m: f64,
) -> Option<f64> {
    if scalar_repricing_supported(record) {
        Some(repriced_total_usd(
            ledger,
            price_usd_per_m,
            record.case.cost.scrap_fraction,
        ))
    } else if (price_usd_per_m - record.case.cost.price_usd_per_m).abs() <= f64::EPSILON {
        Some(ledger.total_usd)
    } else {
        None
    }
}

/// Record-aware ledger components for the cost views. This keeps every
/// declared per-spec amount intact at the source price and declines a
/// hypothetical scalar split when prices are incompatible.
pub(crate) fn record_cost_components(
    record: &CoupledSearchRunRecord,
    ledger: &SearchCostLedger,
    price_usd_per_m: f64,
) -> Option<[(&'static str, f64); 5]> {
    if scalar_repricing_supported(record) {
        Some(search_cost_components(
            ledger,
            price_usd_per_m,
            record.case.cost.scrap_fraction,
        ))
    } else if (price_usd_per_m - record.case.cost.price_usd_per_m).abs() <= f64::EPSILON {
        Some([
            ("Conductor", ledger.conductor_usd),
            ("Scrap", ledger.scrap_usd),
            ("Assembly", ledger.assembly_usd),
            ("Joints", ledger.joints_usd),
            ("Opex", ledger.opex_usd.unwrap_or(0.0)),
        ])
    } else {
        None
    }
}

/// Lifecycle at a hypothetical conductor price: repriced capex + the
/// opex term (declared heat loads are price-independent — they pass
/// through untouched).
#[cfg(test)]
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

/// Record-aware optimum selection. Incompatible pricing keeps the source
/// optimum at the declared price and has no hypothetical scalar result.
pub(crate) fn record_best_index_at_price(
    record: &CoupledSearchRunRecord,
    price_usd_per_m: f64,
) -> Option<usize> {
    if !scalar_repricing_supported(record) {
        return ((price_usd_per_m - record.case.cost.price_usd_per_m).abs() <= f64::EPSILON)
            .then_some(record.best_index)
            .flatten();
    }
    best_index_at_price(record, price_usd_per_m, record.case.cost.scrap_fraction)
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

/// The gating reasons behind a candidate's verdict, in the order the
/// checks close — rendered under the status chips so a bare FAIL or
/// INCONCLUSIVE names its limiter. A candidate can carry several: a
/// degenerate pack that also never screened reports both.
pub(crate) fn explain_status(
    candidate: &SearchCandidateResult,
    case: &CoupledSearchCase,
) -> Vec<String> {
    let mut why = Vec::new();
    if !candidate.pack_geometry_valid {
        why.push("degenerate pack — the winding fills the bore".to_owned());
    }
    if !candidate.manufacturing_feasible {
        why.push(
            "inner bend radius or tape strain exceeds the declared manufacturing limit".to_owned(),
        );
    }
    if candidate.requirement_status == Status::Fail {
        why.push("bore-field requirement not met — no valid operating current".to_owned());
    }
    if candidate.requirement_status == Status::Inconclusive {
        why.push("requirement unresolved — the field refinement gate could not certify".to_owned());
    }
    if candidate.mechanical_feasible == Some(false) {
        why.push("a declared mechanical bound is exceeded".to_owned());
    }
    match candidate.refinement_status {
        Status::Fail => {
            why.push("field refinement diverged beyond the declared gate".to_owned());
        }
        Status::Inconclusive => {
            why.push(
                "refinement gate unresolved — sampled-field accuracy not certified".to_owned(),
            );
        }
        _ => {}
    }
    if let Some(screening) = &candidate.screening {
        if screening.status == Status::Fail
            && let Some(util) = screening.max_utilization
        {
            let at = screening
                .limiting
                .as_ref()
                .map(|l| format!(" at station {}", l.station))
                .unwrap_or_default();
            why.push(format!(
                "over the {:.2} utilization limit ({util:.3}{at})",
                case.limits.utilization_limit,
            ));
        }
        if screening.status == Status::Inconclusive {
            let counts = &screening.point_counts;
            if counts.unsupported > 0 {
                why.push(format!(
                    "{} sampled point(s) fall outside the dataset's measured domain",
                    counts.unsupported,
                ));
            }
            if counts.lower_bound > 0 {
                why.push(format!(
                    "{} point(s) sit below the dataset floor — bounded, not determined",
                    counts.lower_bound,
                ));
            }
            if counts.along_current_excluded > 0 {
                why.push(format!(
                    "{} along-current point(s) excluded by the declared policy",
                    counts.along_current_excluded,
                ));
            }
        }
    }
    if let Some(screens) = &candidate.screens {
        let declared = [
            (
                "thermal margin",
                screens.thermal_margin.as_ref().map(|s| s.status),
            ),
            ("AC loss", screens.ac_loss.as_ref().map(|s| s.status)),
            (
                "quench hotspot",
                screens.quench_hotspot.as_ref().map(|s| s.status),
            ),
            (
                "screening current",
                screens.screening_current.as_ref().map(|s| s.status),
            ),
            ("transition", screens.transition.as_ref().map(|s| s.status)),
            (
                "quench transient",
                screens.quench_transient.as_ref().map(|s| s.status),
            ),
        ];
        for (name, status) in declared {
            match status {
                Some(Status::Fail) => why.push(format!("declared {name} screen failed")),
                Some(Status::Inconclusive) => {
                    why.push(format!("declared {name} screen is inconclusive"));
                }
                _ => {}
            }
        }
    }
    if why.is_empty() && candidate.status == Status::Pass {
        if let Some(screening) = &candidate.screening {
            let bounded = screening.point_counts.along_current_bounded;
            why.push(format!(
                "all sampled points determined; max utilization {:.3} under the {:.2} limit{}",
                screening.max_utilization.unwrap_or(0.0),
                case.limits.utilization_limit,
                if bounded > 0 {
                    format!(" ({bounded} via the bounded along-current model)")
                } else {
                    String::new()
                },
            ));
        } else {
            why.push("all declared checks passed".to_owned());
        }
    }
    why
}

#[cfg(test)]
mod tests {
    use super::*;
    use optcoil_search::coupled::{CandidateResult, LimitingPoint, PointCounts};
    use optcoil_search::coupled_search::CandidateGeometry;

    fn candidate() -> SearchCandidateResult {
        SearchCandidateResult {
            index: 0,
            geometry: CandidateGeometry {
                turns_along_normal: 4,
                tapes_along_width: 2,
                strands_parallel: 1,
                total_turns: 8,
                total_conductors: 8,
                tape_spec_ids: None,
                bend_radius_m: None,
                straight_half_length_m: None,
                radial_width_m: 0.01,
                axial_height_m: 0.02,
            },
            unit_bore_bz_t_per_ampere_turn: 1e-4,
            bore_refinement_change_t: 0.0,
            good_field: None,
            pack_geometry_valid: true,
            manufacturing_feasible: true,
            inner_bend_radius_m: Some(0.05),
            bend_strain: Some(0.001),
            requirement_kernel_evaluations: 0,
            ampere_turns_a: 10_000.0,
            operating_current_a: 500.0,
            requirement_status: Status::Pass,
            refinement_status: Status::Pass,
            numerical_status: Status::Inconclusive,
            peak_sampled_field_t: Some(2.0),
            lorentz_load_n_per_m: Some(1_000.0),
            mechanical_feasible: Some(true),
            hoop_stress_pa: Some(1e6),
            transverse_pressure_pa: Some(1e5),
            transverse_pressure_location: None,
            membrane_tension_n_per_m: Some(100.0),
            screening: Some(CandidateResult {
                current_a: 500.0,
                ampere_turns_a: 10_000.0,
                status: Status::Pass,
                limiting: None,
                min_allowed_screening_a: Some(600.0),
                max_utilization: Some(0.75),
                point_counts: PointCounts {
                    estimate: 34,
                    lower_bound: 0,
                    unsupported: 0,
                    along_current_excluded: 0,
                    along_current_bounded: 0,
                },
                max_self_field_ratio: 0.1,
                limiting_self_field_ratio: None,
                max_transport_self_field_ratio: None,
                max_along_current_fraction: 0.1,
                max_refinement_change_t: 0.0,
            }),
            pruned_by: None,
            coarse_refinement_unresolved: false,
            full_plan_point_count: 34,
            coarse_points_evaluated: 8,
            coarse_kernel_evaluations: 80,
            full_points_evaluated: 34,
            full_kernel_evaluations: 340,
            field_timing_ms: 5.0,
            cost: ledger(),
            status: Status::Pass,
            screens: None,
        }
    }

    fn case() -> CoupledSearchCase {
        crate::author::CaseDraft::default()
            .build()
            .expect("default draft must build")
    }

    #[test]
    fn explain_pass_reports_coverage_and_utilization() {
        let why = explain_status(&candidate(), &case());
        assert_eq!(why.len(), 1);
        assert!(why[0].contains("all sampled points determined"));
        assert!(why[0].contains("0.750"));
    }

    #[test]
    fn explain_fail_names_the_limiter_station() {
        let mut c = candidate();
        c.status = Status::Fail;
        let s = c.screening.as_mut().unwrap();
        s.status = Status::Fail;
        s.max_utilization = Some(1.57);
        s.limiting = Some(LimitingPoint {
            station: "p2".into(),
            tape_index: 1,
            turn_index: 1,
            width_index: 0,
        });
        let why = explain_status(&c, &case());
        assert!(
            why.iter()
                .any(|r| r.contains("utilization") && r.contains("p2"))
        );
    }

    #[test]
    fn explain_inconclusive_names_each_coverage_gap() {
        let mut c = candidate();
        c.status = Status::Inconclusive;
        let s = c.screening.as_mut().unwrap();
        s.status = Status::Inconclusive;
        s.point_counts.unsupported = 3;
        s.point_counts.lower_bound = 7;
        s.point_counts.along_current_excluded = 6;
        let why = explain_status(&c, &case());
        assert!(
            why.iter()
                .any(|r| r.contains("3 sampled point") && r.contains("domain"))
        );
        assert!(
            why.iter()
                .any(|r| r.contains("7 point") && r.contains("floor"))
        );
        assert!(why.iter().any(|r| r.contains("6 along-current")));
    }

    #[test]
    fn explain_reports_requirement_and_geometry_gates() {
        let mut c = candidate();
        c.status = Status::Fail;
        c.pack_geometry_valid = false;
        c.requirement_status = Status::Fail;
        c.screening = None;
        let why = explain_status(&c, &case());
        assert!(why.iter().any(|r| r.contains("degenerate pack")));
        assert!(why.iter().any(|r| r.contains("bore-field requirement")));
    }

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
