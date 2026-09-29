//! Self-contained HTML evidence digest for a coupled-search run record —
//! the readable half of the hash-bound JSON record: the same verdicts,
//! costs, identities and limitations rendered for review rather than
//! diffing. No external assets; one file, printable.

use optcoil_model::Status;

use crate::RunError;
use crate::coupled_search::CoupledSearchRunRecord;

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn verdict(status: Status) -> (&'static str, &'static str) {
    match status {
        Status::Pass => ("PASS", "#1a7f37"),
        Status::Fail => ("FAIL", "#c0392b"),
        Status::Inconclusive => ("INCONCLUSIVE", "#b7791f"),
        Status::NotEvaluated => ("NOT EVALUATED", "#5f697c"),
    }
}

fn badge(status: Status) -> String {
    let (label, color) = verdict(status);
    format!("<span class=\"badge\" style=\"background:{color}\">{label}</span>")
}

fn usd(v: Option<f64>) -> String {
    v.map(|v| format!("${}", grouped(v)))
        .unwrap_or_else(|| "—".into())
}

/// `$123,456` — integer dollars with thousands separators.
fn grouped(v: f64) -> String {
    let digits = format!("{:.0}", v);
    let (sign, digits) = digits
        .strip_prefix('-')
        .map(|d| ("-", d))
        .unwrap_or(("", digits.as_str()));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

fn geometry_label(g: &crate::coupled_search::CandidateGeometry) -> String {
    let base = format!("{}×{}", g.turns_along_normal, g.tapes_along_width);
    if g.strands_parallel > 1 {
        format!("{base}×{}s", g.strands_parallel)
    } else {
        base
    }
}

/// Render a coupled-search run record as a standalone HTML report.
/// Everything shown comes straight out of the record — the report adds
/// no new engineering claims, only formatting. A record whose declared
/// baseline or optimum index is out of range is structurally corrupt;
/// it errors rather than rendering a misleading digest or panicking.
pub fn render_search_report_html(record: &CoupledSearchRunRecord) -> Result<String, RunError> {
    let case = &record.case;
    let best = record
        .best_index
        .map(|i| {
            record.candidates.get(i).ok_or_else(|| {
                RunError::Invalid(format!(
                    "report: record declares best_index {i} but has {} candidates",
                    record.candidates.len()
                ))
            })
        })
        .transpose()?;
    let baseline = record
        .candidates
        .get(record.baseline_index)
        .ok_or_else(|| {
            RunError::Invalid(format!(
                "report: record declares baseline_index {} but has {} candidates",
                record.baseline_index,
                record.candidates.len()
            ))
        })?;
    let pass = record
        .candidates
        .iter()
        .filter(|c| c.status == Status::Pass)
        .count();
    let fail = record
        .candidates
        .iter()
        .filter(|c| c.status == Status::Fail)
        .count();
    let inconclusive = record
        .candidates
        .iter()
        .filter(|c| c.status == Status::Inconclusive)
        .count();
    let not_evaluated = record
        .candidates
        .iter()
        .filter(|c| c.status == Status::NotEvaluated)
        .count();

    let summary = crate::review::decision_summary(&serde_json::to_string(record)?)?;
    let mut decision = format!(
        "<div class=\"card\"><p>Selected option: {}. Baseline: {}.</p>",
        summary
            .selected_candidate_index
            .map(|i| format!("candidate {i}, {}", usd(summary.selected_total_usd)))
            .unwrap_or_else(|| "no resolved screening recommendation".into()),
        usd(Some(summary.baseline_total_usd))
    );
    if let Some(utilization) = summary.current_utilization {
        decision.push_str(&format!(
            "<p>Current utilization {utilization:.4} / limit {:.4}; {}</p>",
            summary.utilization_limit,
            esc(summary
                .limiting_location
                .as_deref()
                .unwrap_or("location unavailable"))
        ));
    }
    if let Some(cost) = &summary.cost_components {
        decision.push_str(&format!(
            "<p>Conductor {} · scrap {} · assembly {} · joints {} · total {}</p>",
            usd(Some(cost.conductor_usd)),
            usd(Some(cost.scrap_usd)),
            usd(Some(cost.assembly_usd)),
            usd(Some(cost.joints_usd)),
            usd(Some(cost.total_usd))
        ));
    }
    decision.push_str("<h3>Price basis</h3><ul>");
    for basis in &summary.price_basis {
        decision.push_str(&format!("<li>{}</li>", esc(basis)));
    }
    decision.push_str("</ul><h3>Unresolved work</h3><ul>");
    for gate in &summary.unresolved_gates {
        decision.push_str(&format!("<li>{}</li>", esc(gate)));
    }
    decision.push_str("</ul><h3>Next actions</h3><ul>");
    for action in &summary.next_actions {
        decision.push_str(&format!("<li>{}</li>", esc(action)));
    }
    decision.push_str("</ul><p>Separate cost recomputation uses shared physics; agreement does not establish independent physical validation or production acceptance.</p></div>");
    let mut rows = String::new();
    for (i, c) in record.candidates.iter().enumerate() {
        let (label, color) = verdict(c.status);
        // best_index/baseline_index are positions into `candidates`, so
        // tag rows by position — not the index field, which a forged
        // record could set inconsistently.
        let is_best = record.best_index == Some(i);
        let is_baseline = i == record.baseline_index;
        let tags = [
            is_best.then_some("optimum"),
            is_baseline.then_some("baseline"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class=\"num\">{:.1}</td>\
             <td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td class=\"num\">${}</td>\
             <td><span class=\"badge\" style=\"background:{color}\">{label}</span> {tags}</td></tr>",
            c.index,
            geometry_label(&c.geometry),
            c.operating_current_a,
            c.peak_sampled_field_t
                .map(|f| format!("{f:.2}"))
                .unwrap_or_else(|| "—".into()),
            c.screening
                .as_ref()
                .and_then(|s| s.max_utilization)
                .map(|u| format!("{u:.3}"))
                .unwrap_or_else(|| "—".into()),
            grouped(c.cost.total_usd),
        ));
    }

    let mut check_rows = String::new();
    for c in record.checks.iter().chain(record.acceptance.checks.iter()) {
        let (label, color) = verdict(c.status);
        check_rows.push_str(&format!(
            "<tr><td class=\"mono\">{}</td><td>{}</td>\
             <td><span class=\"badge\" style=\"background:{color}\">{label}</span></td></tr>",
            esc(&c.id),
            esc(&c.detail),
        ));
    }

    let limitations: String = record
        .limitations
        .iter()
        .map(|l| format!("<li>{}</li>", esc(l)))
        .collect();

    let savings_line = if record.candidates.len() == 1 {
        "<div class=\"hero-num\">—</div><div class=\"muted\">single candidate — it is both baseline \
         and optimum; no alternative was compared</div>"
            .to_owned()
    } else {
        match (record.savings_usd, record.savings_percent) {
            (Some(_), Some(_)) if record.best_index == Some(record.baseline_index) => {
                format!(
                    "<div class=\"hero-num\">0.0%</div><div class=\"muted\">baseline already optimal \
                     — no cheaper candidate passed (baseline {})</div>",
                    usd(Some(baseline.cost.total_usd)),
                )
            }
            (Some(usd_v), Some(pct)) => format!(
                "<div class=\"hero-num\">−{:.1}%</div><div class=\"muted\">modeled conductor-cost \
                 saving vs the declared baseline — {} on a {} baseline</div>",
                pct,
                usd(Some(usd_v)),
                usd(Some(baseline.cost.total_usd)),
            ),
            _ => "<div class=\"hero-num\">—</div><div class=\"muted\">no PASS optimum \
                 selected</div>"
                .into(),
        }
    };

    Ok(format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Converra coupled-search report — {id}</title>
<style>
body {{ font-family: -apple-system, "Segoe UI", Helvetica, Arial, sans-serif;
  background: #f7f8fc; color: #1c2333; margin: 0; padding: 40px;
  max-width: 1000px; margin: 0 auto; }}
h1 {{ font-size: 22px; border-top: 4px solid #1800ad; padding-top: 14px; }}
h2 {{ font-size: 14px; text-transform: uppercase; letter-spacing: .08em;
  color: #1800ad; margin-top: 32px; }}
.card {{ background: #fff; border: 1px solid #dcdce2; border-radius: 8px;
  padding: 16px 20px; margin: 12px 0; }}
table {{ border-collapse: collapse; width: 100%; font-size: 13px; }}
th {{ text-align: left; color: #5f697c; font-weight: 600; font-size: 11px;
  text-transform: uppercase; letter-spacing: .05em;
  border-bottom: 1px solid #dcdce2; padding: 6px 8px; }}
td {{ padding: 6px 8px; border-bottom: 1px solid #eee;
  vertical-align: top; }}
td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
.badge {{ color: #fff; border-radius: 4px; padding: 1px 7px;
  font-size: 11px; font-weight: 700; }}
.hero-num {{ font-size: 42px; font-weight: 800; color: #1800ad; }}
.muted {{ color: #5f697c; font-size: 12px; }}
.mono {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11px; word-break: break-all; }}
.disclaimer {{ border-left: 3px solid #b7791f; background: #fdf6e8;
  padding: 10px 14px; font-size: 12px; color: #6b5320; }}
.grid2 {{ display: grid; grid-template-columns: 1fr 1fr; gap: 0 40px; }}
.kv {{ display: grid; grid-template-columns: 200px 1fr; gap: 4px 16px;
  font-size: 13px; }}
.kv div:nth-child(odd) {{ color: #5f697c; }}
@media print {{ body {{ padding: 0; }} }}
</style></head><body>
<h1>Coupled-search evidence report</h1>
<div class="muted">Converra · generated by optcoil {ver} · {id}</div>

<div class="card">{savings_line}
<div style="margin-top:8px">{s_badge} <b>search</b> &nbsp; {a_badge} <b>independent acceptance</b></div></div>

<h2>Requirement &amp; geometry</h2>
<div class="card"><div class="kv">
<div>Bore target</div><div>{b_target:.3} T at ({px:.3}, {py:.3}, {pz:.3}) m</div>
<div>Fixed geometry</div><div>{geometry} · tape {tw:.1} mm {normal}</div>
<div>Operating point</div><div>{temp:.1} K · E criterion {ecrit:.0e} V/m</div>
<div>Cost basis</div><div>${price:.2}/m · scrap {scrap:.0}% · assembly ${assembly:.0}/pancake · joint ${joint:.0}{piece_line}</div>
</div></div>

<h2>Result</h2>
<div class="card"><div class="kv">
<div>Optimum</div><div>{optimum}</div>
<div>Baseline</div><div>{baseline}</div>
<div>Optimum cost</div><div>{opt_cost}</div>
<div>Baseline cost</div><div>{base_cost}</div>
<div>Candidate verdicts</div><div>{pass} PASS · {fail} FAIL · {inc} INCONCLUSIVE · {nev} NOT_EVALUATED</div>
<div>Sampling work</div><div>{k_evals} kernel evaluations · {pts} points</div>
</div></div>

<h2>Decision at declared inputs and prices</h2>
{decision}

<h2>Acceptance recomputation</h2>
<div class="card"><div class="kv">
<div>Agreement</div><div>{a_badge2}</div>
<div>Baseline recompute</div><div>cost {bcost:?} · field {bfield:?} · region {bregion:?}</div>
<div>Optimum recompute</div><div>{best_line}</div>
<div>Refined plan</div><div>{refined_line}</div>
</div></div>

<h2>Candidate ledger</h2>
<div class="card"><table>
<tr><th>#</th><th>turns×tapes</th><th class="num">I_op A</th>
<th class="num">peak B T</th><th class="num">util</th><th class="num">cost</th><th>verdict</th></tr>
{rows}
</table></div>

<h2>Check ledger</h2>
<div class="card"><table>
<tr><th>check</th><th>detail</th><th>verdict</th></tr>
{check_rows}
</table></div>

<h2>Run identity</h2>
<div class="card"><div class="kv">
<div>Case SHA-256</div><div class="mono">{case_sha}</div>
<div>Input SHA-256</div><div class="mono">{input_sha}</div>
<div>Implementation SHA-256</div><div class="mono">{impl_sha}</div>
<div>Material dataset</div><div>{ds_id}<br><span class="mono muted">csv sha {ds_sha}</span></div>
<div>Model</div><div class="mono">{model_id}</div>
<div>Checker</div><div class="mono">{checker_id}</div>
<div>Acceptance checker</div><div class="mono">{achecker_id}</div>
<div>Elapsed</div><div>{elapsed:.1} s</div>
</div></div>

<h2>Limitations</h2>
<div class="card"><ul>{limitations}</ul>
<div class="disclaimer">Screening-model results — a modeled saving under declared
assumptions, not a production design qualification. INCONCLUSIVE means the model
could not resolve the point, not that it is physically infeasible.</div></div>
</body></html>"#,
        id = esc(&case.id),
        ver = esc(&record.optcoil_version),
        savings_line = savings_line,
        s_badge = badge(record.search_status),
        a_badge = badge(record.acceptance.agreement_status),
        b_target = case.requirement.b_target_t,
        px = case.requirement.bore_probe_m[0],
        py = case.requirement.bore_probe_m[1],
        pz = case.requirement.bore_probe_m[2],
        geometry = match &case.fixed_geometry.path {
            Some(path) => {
                let min_r = path.min_radius_m();
                format!(
                    "planar path — {} segments · L {:.3} m · min bend R {} m",
                    path.segments.len(),
                    path.length_m(),
                    if min_r.is_finite() {
                        format!("{min_r:.3}")
                    } else {
                        "—".to_owned()
                    },
                )
            }
            None => format!(
                "racetrack — straight ±{:.3} m · bend R {:.3} m",
                case.fixed_geometry
                    .straight_half_length_m
                    .unwrap_or(f64::NAN),
                case.fixed_geometry.bend_radius_m.unwrap_or(f64::NAN),
            ),
        },
        tw = case.fixed_geometry.tape_width_m * 1000.0,
        normal = serde_json::to_string(&case.fixed_geometry.tape_normal)
            .unwrap_or_default()
            .trim_matches('"'),
        temp = case.operating.temperature_k,
        ecrit = case.operating.electric_field_criterion_v_per_m,
        price = case.cost.price_usd_per_m,
        piece_line = case.cost.piece_policy.as_ref().map_or(String::new(), |p| {
            let boundary = serde_json::to_string(&p.boundary)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let unit = serde_json::to_string(&p.piece_unit)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            let offering = case
                .cost
                .piece_offerings
                .as_ref()
                .map(|o| {
                    o.iter()
                        .map(|e| format!("{:.0}m@${:.2}", e.length_m, e.price_usd_per_m))
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .or_else(|| p.piece_length_m.map(|l| format!("{l:.0}m")))
                .unwrap_or_default();
            format!(
                " · pieces {boundary}/{unit} [{offering}] splice ${:.0}",
                p.splice_cost_usd
            )
        }),
        scrap = case.cost.scrap_fraction * 100.0,
        assembly = case.cost.assembly_cost_per_pancake_usd,
        joint = case.cost.joint_cost_usd,
        optimum = best
            .map(|c| geometry_label(&c.geometry))
            .unwrap_or_else(|| "none (no PASS candidate)".into()),
        baseline = geometry_label(&baseline.geometry),
        opt_cost = best
            .map(|c| usd(Some(c.cost.total_usd)))
            .unwrap_or_else(|| "—".into()),
        base_cost = usd(Some(baseline.cost.total_usd)),
        pass = pass,
        fail = fail,
        inc = inconclusive,
        nev = not_evaluated,
        k_evals = record.kernel_evaluations,
        pts = record.points_evaluated,
        a_badge2 = badge(record.acceptance.agreement_status),
        bcost = record.acceptance.baseline.cost_agreement_status,
        bfield = record.acceptance.baseline.field_agreement_status,
        bregion = record.acceptance.baseline.region_agreement_status,
        best_line = record
            .acceptance
            .best
            .as_ref()
            .map(|b| format!(
                "cost {:?} · field {:?} · agreement {:?}",
                b.cost_agreement_status, b.field_agreement_status, b.agreement_status
            ))
            .unwrap_or_else(|| "—".into()),
        refined_line = record
            .acceptance
            .best
            .as_ref()
            .map(|b| format!("{:?}", b.sampling_refinement_status))
            .unwrap_or_else(|| "—".into()),
        decision = decision,
        rows = rows,
        check_rows = check_rows,
        case_sha = esc(&record.case_sha256),
        input_sha = esc(&record.input_sha256),
        impl_sha = esc(&record.implementation_sha256),
        ds_id = esc(&record.dataset_id),
        ds_sha = esc(&record.dataset_csv_sha256),
        model_id = esc(&record.coupled_search_model_id),
        checker_id = esc(&record.coupled_search_checker_id),
        achecker_id = esc(&record.acceptance.checker_id),
        elapsed = record.elapsed_ms / 1000.0,
        limitations = limitations,
    ))
}
