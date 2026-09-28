use crate::{JobKind, Workbench, brand, status_color, status_label};
use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Bar, BarChart, Legend, Line, Plot, Points, VLine};
use optcoil_adapters::{PLANNED_ADAPTERS, SolverAdapter};
use optcoil_model::{
    Check, CostBreakdown, Status, coupled::Station, coupled_search::CoupledSearchCase,
};
use optcoil_physics::tape_capacity_a;
use optcoil_search::coupled_search::{CoupledSearchRunRecord, SearchCostLedger};
use optcoil_search::verify;

const COST_LABELS: [&str; 4] = ["Conductor", "Scrap", "Assembly", "Joints"];

impl Workbench {
    pub(super) fn overview(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Compare coil allocations",
            "See where the cost changes and which module limits the design.",
        );
        let reveal = self.reveal_progress();
        ui.columns(3, |columns| {
            metric(
                &mut columns[0],
                "BASELINE",
                format!("${:.2}", self.baseline.cost.total_usd),
                "Reference allocation",
                1.0,
                false,
            );
            metric(
                &mut columns[1],
                "CANDIDATE",
                self.record
                    .as_ref()
                    .map_or("Ready to optimize".into(), |r| {
                        format!("${:.2}", r.best.cost.total_usd * reveal as f64)
                    }),
                "Checked model cost",
                reveal,
                false,
            );
            metric(
                &mut columns[2],
                "MODELED SAVING",
                self.record.as_ref().map_or("—".into(), |r| {
                    format!("{:.2}%", r.savings_percent * reveal as f64)
                }),
                &self
                    .record
                    .as_ref()
                    .map_or("Run the synthetic example".into(), |r| {
                        format!("${:.2} per modeled assembly", r.savings_usd * reveal as f64)
                    }),
                reveal,
                true,
            );
        });
        if self.record.is_none() {
            ui.add_space(10.0);
            egui::Frame::group(ui.style())
                .fill(brand::status_tint(brand::BLUE))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(
                        RichText::new(
                            "Run ▶ Optimize to compare this case against its baseline — \
                             every allocation is checked against the same gates.",
                        )
                        .color(brand::BLUE),
                    );
                });
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Manufacturing cost / USD");
            ui.colored_label(
                brand::MUTED,
                "Hover for values · drag to pan · scroll to zoom",
            );
        });
        let y_headroom = cost_values(&self.baseline.cost)
            .into_iter()
            .chain(self.record.iter().flat_map(|r| cost_values(&r.best.cost)))
            .fold(0.0_f64, f64::max)
            * 1.15;
        Plot::new(("cost-comparison", self.plot_revision))
            .height(230.0)
            .legend(Legend::default())
            .include_x(-0.75)
            .include_x(3.75)
            .include_y(0.0)
            .include_y(y_headroom)
            .allow_scroll(false)
            .x_axis_formatter(|mark, _| {
                let index = mark.value.round() as isize;
                if (mark.value - index as f64).abs() < 0.01 && (0..4).contains(&index) {
                    COST_LABELS[index as usize].into()
                } else {
                    String::new()
                }
            })
            .show(ui, |plot| {
                if self.show_baseline || self.record.is_none() {
                    plot.bar_chart(cost_bars(
                        "Baseline",
                        &self.baseline.cost,
                        -0.18,
                        brand::BASELINE,
                        1.0,
                    ));
                }
                if let Some(record) = &self.record {
                    // Bars grow with the reveal animation.
                    plot.bar_chart(cost_bars(
                        "Candidate",
                        &record.best.cost,
                        0.18,
                        brand::BLUE,
                        reveal,
                    ));
                }
            });
        ui.add_space(12.0);
        ui.heading("Module allocation");
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.module_filter)
                    .hint_text("Filter modules…")
                    .desired_width(210.0),
            );
            ui.checkbox(&mut self.sort_by_field, "Sort by field");
            ui.small("Select a row to inspect its design.");
        });
        let filter = self.module_filter.to_lowercase();
        let mut indices: Vec<usize> = (0..self.case.modules.len())
            .filter(|&i| self.case.modules[i].id.to_lowercase().contains(&filter))
            .collect();
        if self.sort_by_field {
            indices.sort_by(|&a, &b| {
                self.case.modules[a]
                    .operating_point
                    .field_t
                    .total_cmp(&self.case.modules[b].operating_point.field_t)
            });
        }
        if indices.is_empty() {
            ui.label("No modules match this filter.");
        }
        TableBuilder::new(ui)
            .id_salt("module-table")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(110.0).clip(true))
            .column(Column::initial(65.0).at_least(55.0))
            .column(Column::initial(130.0).at_least(80.0).clip(true))
            .column(Column::initial(130.0).at_least(80.0).clip(true))
            .column(Column::initial(95.0).at_least(80.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(210.0)
            .header(25.0, |mut header| {
                for label in ["Module", "Field / T", "Baseline", "Candidate", "Margin / A"] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(32.0, indices.len(), |mut row| {
                    let i = indices[row.index()];
                    let module = &self.case.modules[i];
                    row.set_selected(self.selected_module == i);
                    row.col(|ui| {
                        ui.label(&module.id);
                    });
                    row.col(|ui| {
                        ui.label(format!("{:.1}", module.operating_point.field_t));
                    });
                    let baseline = &self.case.baseline.allocations[i];
                    row.col(|ui| {
                        ui.label(format!("{} × {}", baseline.tapes, baseline.grade_id));
                    });
                    row.col(|ui| {
                        ui.set_opacity(reveal.max(0.15));
                        ui.label(self.record.as_ref().map_or("—".into(), |r| {
                            let a = &r.best.candidate.allocations[i];
                            format!("{} × {}", a.tapes, a.grade_id)
                        }));
                    });
                    row.col(|ui| {
                        ui.set_opacity(reveal.max(0.15));
                        ui.label(
                            self.record
                                .as_ref()
                                .and_then(|r| r.best.modules[i].current_margin_a)
                                .map_or("—".into(), |v| format!("{v:.1}")),
                        );
                    });
                    if row.response().clicked() {
                        self.selected_module = i;
                    }
                });
            });
        ui.add_space(14.0);
        egui::CollapsingHeader::new("Cost ledger and search outcome")
            .default_open(true)
            .show(ui, |ui| {
                ui.set_opacity(if self.record.is_some() {
                    reveal.max(0.15)
                } else {
                    1.0
                });
                egui::Grid::new("cost-ledger")
                    .striped(true)
                    .min_col_width(135.0)
                    .show(ui, |ui| {
                        ui.strong("Cost / USD");
                        ui.strong("Baseline");
                        ui.strong("Candidate");
                        ui.end_row();
                        for (i, label) in COST_LABELS
                            .iter()
                            .chain(std::iter::once(&"Total"))
                            .enumerate()
                        {
                            ui.label(*label);
                            let original = if i == 4 {
                                self.baseline.cost.total_usd
                            } else {
                                cost_values(&self.baseline.cost)[i]
                            };
                            ui.label(format!("{original:.2}"));
                            ui.label(self.record.as_ref().map_or("—".into(), |r| {
                                format!(
                                    "{:.2}",
                                    if i == 4 {
                                        r.best.cost.total_usd
                                    } else {
                                        cost_values(&r.best.cost)[i]
                                    }
                                )
                            }));
                            ui.end_row();
                        }
                    });
                if let Some(record) = &self.record {
                    ui.add_space(8.0);
                    ui.label(format!(
                        "{:?} · {} / {} allocations · {} ms · optimum within screening model: {}",
                        record.termination,
                        record.evaluated_candidates,
                        record.search_space_size,
                        record.elapsed_ms,
                        record.proven_optimal_in_screening_model
                    ));
                    if record.inconclusive_candidates > 0 {
                        ui.colored_label(
                            status_color(Status::Inconclusive),
                            format!(
                                "{} allocations have unresolved material coverage.",
                                record.inconclusive_candidates
                            ),
                        );
                    }
                }
            });
        ui.add_space(8.0);
        ui.colored_label(
            brand::MUTED,
            "Synthetic allocation benchmark. Full engineering acceptance has not been established.",
        );
    }

    pub(super) fn inspector(&mut self, ui: &mut egui::Ui) {
        if self.search_case.is_some() {
            self.search_inspector(ui);
            return;
        }
        // Brief fade when the selection changes — directs the eye to what
        // the table row / module picker just swapped in.
        ui.set_opacity(egui::emath::easing::cubic_out(
            self.anim_progress(self.inspector_fade_start, 180),
        ));
        let i = self.selected_module;
        let module = &self.case.modules[i];
        ui.add_space(10.0);
        ui.label(RichText::new("MODULE INSPECTOR").color(brand::BLUE).small());
        ui.heading(&module.id);
        ui.small("Selected from the allocation table");
        ui.add_space(12.0);
        ui.strong("Fixed operating point");
        for text in [
            format!("Field: {:.2} T", module.operating_point.field_t),
            format!("Temperature: {:.2} K", module.operating_point.temperature_k),
            format!(
                "Tape / field angle: {:.1}°",
                module.operating_point.field_angle_deg
            ),
            format!("Circuit current: {:.0} A", self.case.circuit_current_a),
            format!(
                "Utilization ceiling: {:.0}%",
                self.case.utilization_limit * 100.0
            ),
        ] {
            ui.label(text);
        }
        ui.separator();
        ui.strong("Winding inputs");
        ui.label(format!(
            "{} turns · {:.2} m / turn",
            module.turns, module.mean_turn_length_m
        ));
        ui.label(format!("At most {} parallel tapes", module.max_tapes));
        ui.add_space(10.0);
        let (label, assessment) = self
            .record
            .as_ref()
            .map_or(("Baseline", &self.baseline), |r| ("Candidate", &r.best));
        let allocation = &assessment.candidate.allocations[i];
        let result = &assessment.modules[i];
        ui.strong(label);
        ui.label(format!(
            "{} × {} tape",
            allocation.tapes, allocation.grade_id
        ));
        ui.label(format!("Installed: {:.1} m", result.installed_length_m));
        ui.label(format!("Purchased: {:.1} m", result.purchased_length_m));
        ui.label(
            result
                .current_margin_a
                .map_or("Current margin: unknown".into(), |m| {
                    format!("Current margin: {m:.1} A")
                }),
        );
        ui.separator();
        ui.strong("Engineering status");
        status_chip(ui, assessment.engineering_status);
        ui.small("Prescribed fields and ideal current sharing. Stress, thermal behavior and quench still require analysis.");
    }

    pub(super) fn materials(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Material envelope",
            "The declared single-tape capacity per grade — a case input, not a result. Within a grade's domain the synthetic model derates with field only; temperature and angle set where a grade is supported at all.",
        );
        egui::ComboBox::from_id_salt("material-module")
            .selected_text(&self.case.modules[self.selected_module].id)
            .show_ui(ui, |ui| {
                for (i, module) in self.case.modules.iter().enumerate() {
                    ui.selectable_value(&mut self.selected_module, i, &module.id);
                }
            });
        let module = &self.case.modules[self.selected_module];
        ui.label(format!(
            "Module operating point: {:.1} K · {:.1}° · prescribed field {:.1} T",
            module.operating_point.temperature_k,
            module.operating_point.field_angle_deg,
            module.operating_point.field_t
        ));
        Plot::new((
            "material-envelope",
            self.selected_module,
            self.plot_revision,
        ))
        .height(310.0)
        .legend(Legend::default())
        .include_y(0.0)
        .allow_scroll(false)
        .x_axis_label("Magnetic field / T")
        .y_axis_label("Single-tape critical current / A")
        .show(ui, |plot| {
            for (i, grade) in self
                .case
                .grades
                .iter()
                .enumerate()
                .filter(|(_, g)| module.allowed_grade_ids.contains(&g.id))
            {
                let points: Vec<[f64; 2]> = (0..=100)
                    .filter_map(|n| {
                        let mut point = module.operating_point.clone();
                        point.field_t = grade.domain.max_field_t * f64::from(n) / 100.0;
                        tape_capacity_a(grade, &point).map(|capacity| [point.field_t, capacity])
                    })
                    .collect();
                if !points.is_empty() {
                    let color = if i % 2 == 0 {
                        brand::BLUE
                    } else {
                        Color32::from_rgb(0, 118, 140)
                    };
                    plot.line(Line::new(&grade.id, points).width(2.5).color(color));
                    if let Some(capacity) = tape_capacity_a(grade, &module.operating_point) {
                        plot.points(
                            Points::new(
                                format!("{} @ operating point", grade.id),
                                vec![[module.operating_point.field_t, capacity]],
                            )
                            .radius(5.0)
                            .color(color),
                        );
                    }
                }
            }
            plot.vline(
                VLine::new("Module field", module.operating_point.field_t).color(brand::BASELINE),
            );
        });
        let capacities: Vec<String> = self
            .case
            .grades
            .iter()
            .filter(|g| module.allowed_grade_ids.contains(&g.id))
            .map(|g| match tape_capacity_a(g, &module.operating_point) {
                Some(ic) => format!("{}: {:.1} A/tape", g.id, ic),
                None => format!("{}: outside declared domain", g.id),
            })
            .collect();
        ui.label(format!(
            "At the module's field — {}",
            capacities.join("  ·  ")
        ));
        ui.colored_label(brand::MUTED, "Synthetic lower-bound envelopes; no extrapolation beyond the specified domain. Curves are single-tape Ic, before the stack utilization ceiling.");
        for grade in &self.case.grades {
            ui.add_space(10.0);
            egui::CollapsingHeader::new(format!(
                "{} · ${:.2} / m",
                grade.id, grade.price_usd_per_m
            ))
            .default_open(true)
            .show(ui, |ui| {
                ui.label(format!(
                    "Supported: {}–{} K · 0–{} T · {}–{}°",
                    grade.domain.min_temperature_k,
                    grade.domain.max_temperature_k,
                    grade.domain.max_field_t,
                    grade.domain.min_angle_deg,
                    grade.domain.max_angle_deg
                ));
                ui.label(&grade.provenance);
            });
        }
        ui.collapsing("Advanced: original case data", |ui| {
            if let Ok(json) = serde_json::to_string_pretty(&self.case) {
                ui.monospace(json);
            }
        });
    }

    pub(super) fn checks(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Checks & evidence",
            "Keep successful screening calculations separate from missing engineering evidence.",
        );
        let assessment = self.record.as_ref().map_or(&self.baseline, |r| &r.best);
        ui.horizontal_wrapped(|ui| {
            ui.strong(if self.record.is_some() {
                "Candidate"
            } else {
                "Baseline"
            });
            ui.label(RichText::new("Screening").color(brand::MUTED).small());
            status_chip(ui, assessment.screening_status);
            ui.add_space(6.0);
            ui.label(RichText::new("Engineering").color(brand::MUTED).small());
            status_chip(ui, assessment.engineering_status);
        });
        ui.checkbox(
            &mut self.unresolved_only,
            "Show only failed or unresolved checks",
        );
        ui.add_space(10.0);
        for check in assessment
            .checks
            .iter()
            .filter(|c| !self.unresolved_only || c.status != Status::Pass)
        {
            // A left-edge stripe in the verdict color turns the list into
            // an evidence ledger — status reads before the text does.
            let frame = egui::Frame::group(ui.style())
                .fill(Color32::WHITE)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        status_chip(ui, check.status);
                        ui.strong(&check.id);
                    });
                    ui.label(&check.detail);
                });
            let rect = frame.response.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top() + 7.0),
                    egui::pos2(rect.left() + 3.0, rect.bottom() - 7.0),
                ),
                1.5,
                status_color(check.status),
            );
        }
        if let Some(record) = &self.record {
            ui.add_space(10.0);
            ui.collapsing("Run identity and model assumptions", |ui| {
                ui.label(format!("Case SHA-256: {}", record.case_sha256));
                ui.label(format!("Input SHA-256: {}", record.input_sha256));
                for limitation in &record.limitations {
                    ui.label(limitation);
                }
            });
        }
    }

    pub(super) fn integrations(&self, ui: &mut egui::Ui) {
        title(
            ui,
            "Imports & solver connections",
            "A project combines geometry, material data, requirements and analysis settings.",
        );
        ui.heading("Available now");
        ui.label("Open or drop a Converra case (.json). The file contains the complete synthetic allocation problem. Export run records with their inputs and results from the toolbar.");
        ui.add_space(12.0);
        ui.heading("Planned engineering imports");
        for (name, purpose) in [
            (
                "STEP / STP",
                "CAD solids and assemblies. Imported parts will need coil, orientation and current assignments.",
            ),
            (
                "CSV, then Excel",
                "Material measurements, costs and operating tables, with column mapping, units and provenance.",
            ),
            (
                "Native solver models / APIs",
                "Reuse existing geometry, materials and analysis definitions through a compatible solver connection.",
            ),
            (
                "MAKEGRID coil files",
                "A later option for stellarator centerlines and currents; not a complete manufacturing model.",
            ),
        ] {
            ui.group(|ui| {
                ui.strong(name);
                ui.label(purpose);
                ui.colored_label(brand::MUTED, "Planned — import not implemented");
            });
        }
        ui.add_space(12.0);
        ui.heading("External solvers");
        for adapter in PLANNED_ADAPTERS {
            let descriptor = adapter.descriptor();
            ui.group(|ui| {
                ui.strong(descriptor.name);
                ui.label(descriptor.integration_surface);
                ui.colored_label(brand::MUTED, "Planned — no solver is executed");
            });
        }
        ui.add_space(8.0);
        ui.small("Planned inputs: measured-material CSV, then Excel tables, then STEP CAD and solver connections.");
    }

    // ---- Coupled-search project pages ------------------------------------

    /// Overview for a coupled-search project: the requirement and candidate
    /// space before a run, the verdict ledger and optimum after one.
    pub(super) fn search_overview(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Coupled conductor search",
            "Every declared geometry is screened against the magnet requirement; the optimum is the cheapest PASS.",
        );
        let reveal = self.reveal_progress();
        let record = self.search_record.as_ref();
        ui.columns(3, |columns| {
            metric(
                &mut columns[0],
                "BASELINE",
                record.map_or_else(
                    || {
                        self.search_case.as_ref().map_or("—".into(), |c| {
                            format!(
                                "{} × {}{}",
                                c.baseline.turns_along_normal,
                                c.baseline.tapes_along_width,
                                strands_suffix(c.baseline_strands())
                            )
                        })
                    },
                    |r| {
                        let price = self
                            .reprice_usd_per_m
                            .unwrap_or(r.case.cost.price_usd_per_m);
                        usd(repriced_total_usd(
                            &r.candidates[r.baseline_index].cost,
                            price,
                            r.case.cost.scrap_fraction,
                        ) * reveal as f64)
                    },
                ),
                "Declared reference geometry",
                if record.is_some() { reveal } else { 1.0 },
                false,
            );
            metric(
                &mut columns[1],
                "OPTIMUM",
                record.map_or("Not run".into(), |r| {
                    let price = self
                        .reprice_usd_per_m
                        .unwrap_or(r.case.cost.price_usd_per_m);
                    best_index_at_price(r, price, r.case.cost.scrap_fraction).map_or(
                        "No PASS".into(),
                        |i| {
                            let g = &r.candidates[i].geometry;
                            format!(
                                "{} × {}{}",
                                g.turns_along_normal,
                                g.tapes_along_width,
                                strands_suffix(g.strands_parallel)
                            )
                        },
                    )
                }),
                record
                    .map_or("Run the search".to_string(), |r| {
                        let price = self
                            .reprice_usd_per_m
                            .unwrap_or(r.case.cost.price_usd_per_m);
                        best_index_at_price(r, price, r.case.cost.scrap_fraction).map_or(
                            "No candidate passed".to_string(),
                            |i| {
                                format!(
                                    "{} checked cost",
                                    usd(repriced_total_usd(
                                        &r.candidates[i].cost,
                                        price,
                                        r.case.cost.scrap_fraction
                                    ))
                                )
                            },
                        )
                    })
                    .as_str(),
                reveal,
                false,
            );
            metric(
                &mut columns[2],
                "MODELED SAVING",
                record.map_or("—".into(), |r| {
                    if r.candidates.len() == 1 {
                        // One candidate: baseline IS the optimum — a bare
                        // "0.00%" reads like a failure.
                        "—".into()
                    } else {
                        let price = self
                            .reprice_usd_per_m
                            .unwrap_or(r.case.cost.price_usd_per_m);
                        let scrap = r.case.cost.scrap_fraction;
                        best_index_at_price(r, price, scrap)
                            .map(|i| {
                                if i == r.baseline_index {
                                    "0.00%".into()
                                } else {
                                    let base = repriced_total_usd(
                                        &r.candidates[r.baseline_index].cost,
                                        price,
                                        scrap,
                                    );
                                    let best =
                                        repriced_total_usd(&r.candidates[i].cost, price, scrap);
                                    if base > 0.0 {
                                        format!(
                                            "{:.2}%",
                                            (base - best) / base * 100.0 * reveal as f64
                                        )
                                    } else {
                                        "—".into()
                                    }
                                }
                            })
                            .unwrap_or_else(|| "—".into())
                    }
                }),
                &record.map_or("vs the declared baseline".to_string(), |r| {
                    if r.candidates.len() == 1 {
                        return "single candidate — it is both baseline and optimum".to_string();
                    }
                    let price = self
                        .reprice_usd_per_m
                        .unwrap_or(r.case.cost.price_usd_per_m);
                    let scrap = r.case.cost.scrap_fraction;
                    best_index_at_price(r, price, scrap).map_or("—".to_string(), |i| {
                        if i == r.baseline_index {
                            return "baseline already optimal — no cheaper candidate passed"
                                .to_string();
                        }
                        let base =
                            repriced_total_usd(&r.candidates[r.baseline_index].cost, price, scrap);
                        let best = repriced_total_usd(&r.candidates[i].cost, price, scrap);
                        format!(
                            "{} vs the declared baseline",
                            usd((base - best) * reveal as f64)
                        )
                    })
                }),
                reveal,
                true,
            );
        });
        if let Some(record) = self.search_record.as_ref() {
            self.recommendation_card(ui, record);
        }
        let Some(case) = self.search_case.as_ref() else {
            return;
        };
        if self.search_record.is_none() {
            ui.add_space(10.0);
            egui::Frame::group(ui.style())
                .fill(brand::status_tint(brand::BLUE))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(
                        RichText::new(if self.search_json.is_empty() {
                            "This is a saved run record opened read-only — open the case file itself to re-run the search."
                        } else {
                            "Run ▶ Run search to evaluate every candidate geometry against the requirement and the screening limits."
                        })
                        .color(brand::BLUE),
                    );
                });
            ui.add_space(12.0);
            search_case_summary(ui, case);
            return;
        }
        // A running search is a real state, not a dead window — show the
        // work while the worker thread grinds.
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.kind == JobKind::Search)
        {
            egui::Frame::group(ui.style())
                .fill(brand::status_tint(brand::BLUE))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Evaluating candidate geometries…")
                                    .color(brand::BLUE)
                                    .strong(),
                            );
                            ui.small(
                                "field solves run on a background thread — the verdict tally lands when the last candidate is screened",
                            );
                        });
                    });
                });
            ui.add_space(10.0);
        }
        let record = self.search_record.as_ref().unwrap();
        // Staged reveal: sections fade in, in reading order, when a fresh
        // record lands — verdicts first, detail last.
        let p0 = self.stage(0);
        let p1 = self.stage(80);
        let p2 = self.stage(160);
        let p3 = self.stage(240);
        let p4 = self.stage(320);
        let p5 = self.stage(400);
        ui.add_space(10.0);
        ui.set_opacity(p0);
        // Verdict tally: the four-status vocabulary is preserved verbatim.
        ui.horizontal_wrapped(|ui| {
            for status in [
                Status::Pass,
                Status::Fail,
                Status::Inconclusive,
                Status::NotEvaluated,
            ] {
                let n = record
                    .candidates
                    .iter()
                    .filter(|c| c.status == status)
                    .count();
                if n > 0 {
                    status_chip(ui, status);
                    ui.label(format!("× {n}"));
                }
            }
            ui.separator();
            ui.label(RichText::new("Search").color(brand::MUTED).small());
            status_chip(ui, record.search_status);
            ui.label(RichText::new("Acceptance").color(brand::MUTED).small());
            status_chip(ui, record.acceptance.agreement_status);
        });
        ui.add_space(6.0);
        ui.set_opacity(p1);
        // Live repricing — the same closed-form model as `optcoil reprice`.
        // Dollars move; verdicts and margins cannot (price is not a
        // physics input), which is exactly the point of the demo.
        {
            let case_price = record.case.cost.price_usd_per_m;
            let mut price = self.reprice_usd_per_m.unwrap_or(case_price);
            let mut moved = false;
            let mut reset = false;
            // v24 piece-catalogue pricing is per-spec — a single $/m
            // slider cannot reprice it. Show the ledger's own totals.
            let piece_priced = record
                .candidates
                .iter()
                .any(|c| c.cost.piece_plan.is_some());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Conductor $/m").color(brand::MUTED).small());
                if piece_priced {
                    ui.colored_label(
                        brand::MUTED,
                        "piece-catalogue pricing — slider not applicable",
                    );
                } else {
                    if ui
                        .add(
                            egui::Slider::new(
                                &mut price,
                                (case_price * 0.25).max(1.0)..=case_price * 3.0,
                            )
                            .custom_formatter(|v, _| format!("${v:.2}"))
                            .fixed_decimals(2),
                        )
                        .changed()
                    {
                        moved = true;
                    }
                    if self.reprice_usd_per_m.is_some() {
                        ui.colored_label(brand::BLUE, "repriced — verdicts unchanged");
                        if ui.small_button("reset").clicked() {
                            reset = true;
                        }
                    }
                }
            });
            if moved {
                self.reprice_usd_per_m = ((price - case_price).abs() > 1e-9).then_some(price);
            }
            if reset {
                self.reprice_usd_per_m = None;
            }
        }
        ui.add_space(10.0);
        let shown_price = self
            .reprice_usd_per_m
            .unwrap_or(record.case.cost.price_usd_per_m);
        let scrap_fraction = record.case.cost.scrap_fraction;
        let best = best_index_at_price(record, shown_price, scrap_fraction);
        if let Some(best) = best {
            let candidate = &record.candidates[best];
            ui.horizontal_wrapped(|ui| {
                ui.strong("Selected optimum:");
                ui.label(format!(
                    "{} × {}{} · I_op {:.1} A · {}",
                    candidate.geometry.turns_along_normal,
                    candidate.geometry.tapes_along_width,
                    strands_suffix(candidate.geometry.strands_parallel),
                    candidate.operating_current_a,
                    usd(repriced_total_usd(
                        &candidate.cost,
                        shown_price,
                        scrap_fraction
                    )),
                ));
                if let (Some(_), Some(opex)) =
                    (candidate.cost.lifecycle_usd, candidate.cost.opex_usd)
                {
                    // Reprice the capex part under the slider; opex is
                    // conductor-price-independent, so it passes through.
                    let lifecycle =
                        repriced_total_usd(&candidate.cost, shown_price, scrap_fraction) + opex;
                    ui.colored_label(
                        brand::BLUE,
                        format!("· lifecycle {} (incl. declared opex)", usd(lifecycle)),
                    );
                }
                {
                    let base = repriced_total_usd(
                        &record.candidates[record.baseline_index].cost,
                        shown_price,
                        scrap_fraction,
                    );
                    let best_total =
                        repriced_total_usd(&candidate.cost, shown_price, scrap_fraction);
                    if base > 0.0 {
                        ui.colored_label(
                            brand::BLUE,
                            format!(
                                "{} ({:.1}%) below the declared baseline",
                                usd(base - best_total),
                                (base - best_total) / base * 100.0
                            ),
                        );
                    }
                }
            });
        } else {
            ui.colored_label(
                status_color(Status::Fail),
                "No candidate passed — no feasible geometry in the declared choice set.",
            );
        }
        // Cost breakdown: the declared baseline vs the inspected (or
        // cheapest-PASS) candidate across every ledger component —
        // conductor, scrap, assembly, joints and declared opex.
        ui.set_opacity(p2);
        {
            let baseline = &record.candidates[record.baseline_index].cost;
            let shown = record
                .candidates
                .get(self.selected_candidate)
                .or(best.and_then(|i| record.candidates.get(i)))
                .map(|c| &c.cost);
            if let Some(shown) = shown {
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    ui.strong("Cost breakdown / USD");
                    ui.colored_label(
                        brand::MUTED,
                        "declared baseline vs inspected candidate · hover for values",
                    );
                });
                let headroom = search_cost_components(baseline, shown_price, scrap_fraction)
                    .into_iter()
                    .chain(search_cost_components(shown, shown_price, scrap_fraction))
                    .map(|(_, v)| v)
                    .fold(0.0_f64, f64::max)
                    * 1.15;
                Plot::new("search-cost-breakdown")
                    .height(190.0)
                    .legend(Legend::default())
                    .include_x(-0.75)
                    .include_x(4.75)
                    .include_y(0.0)
                    .include_y(headroom)
                    .allow_scroll(false)
                    .x_axis_formatter(|mark, _| {
                        let index = mark.value.round() as isize;
                        if (mark.value - index as f64).abs() < 0.01 && (0..5).contains(&index) {
                            SEARCH_COST_LABELS[index as usize].into()
                        } else {
                            String::new()
                        }
                    })
                    .show(ui, |plot| {
                        plot.bar_chart(search_cost_bars(
                            "Baseline",
                            &search_cost_components(baseline, shown_price, scrap_fraction),
                            -0.18,
                            brand::BASELINE,
                            1.0,
                        ));
                        plot.bar_chart(search_cost_bars(
                            "Candidate",
                            &search_cost_components(shown, shown_price, scrap_fraction),
                            0.18,
                            brand::BLUE,
                            1.0,
                        ));
                    });
            }
        }
        // Cost landscape: every evaluated candidate as a status-colored
        // point — cost against peak sampled field. Click a point to
        // inspect that candidate; the ring tracks the cheapest PASS.
        ui.set_opacity(p3);
        {
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong("Cost landscape");
                ui.colored_label(
                    brand::MUTED,
                    "every candidate · cost vs peak sampled field · click a point to inspect",
                );
            });
            // A landscape needs at least two plotted points — a lone
            // candidate renders as a dot on a degenerate auto-scaled
            // axis, which reads as a broken plot rather than a single
            // data point.
            let plotted = record
                .candidates
                .iter()
                .filter(|c| c.peak_sampled_field_t.is_some())
                .count();
            if plotted < 2 {
                ui.colored_label(
                    brand::MUTED,
                    format!(
                        "{} candidate{} with a sampled peak field — a landscape needs at least \
                         two points to compare",
                        plotted,
                        if plotted == 1 { "" } else { "s" }
                    ),
                );
            } else {
                let mut clicked = None;
                let plot_resp = Plot::new("search-cost-landscape")
                    .height(200.0)
                    .x_axis_label("lifecycle cost / USD")
                    .y_axis_label("peak field / T")
                    .show(ui, |plot| {
                        for status in [
                            Status::Pass,
                            Status::Fail,
                            Status::Inconclusive,
                            Status::NotEvaluated,
                        ] {
                            let pts: Vec<[f64; 2]> = record
                                .candidates
                                .iter()
                                .filter(|c| c.status == status)
                                .filter_map(|c| {
                                    c.peak_sampled_field_t.map(|b| {
                                        [c.cost.lifecycle_usd.unwrap_or(c.cost.total_usd), b]
                                    })
                                })
                                .collect();
                            if pts.is_empty() {
                                continue;
                            }
                            plot.points(
                                Points::new(status_label(status), pts)
                                    .color(status_color(status))
                                    .radius(4.0)
                                    .filled(true),
                            );
                        }
                        if let Some(best_i) = best {
                            let c = &record.candidates[best_i];
                            if let Some(b) = c.peak_sampled_field_t {
                                plot.points(
                                    Points::new(
                                        "optimum",
                                        vec![[c.cost.lifecycle_usd.unwrap_or(c.cost.total_usd), b]],
                                    )
                                    .color(brand::BLUE)
                                    .radius(7.0)
                                    .filled(false),
                                );
                            }
                        }
                        if plot.response().clicked()
                            && let Some(coord) = plot.pointer_coordinate()
                        {
                            // Nearest candidate in normalized axes so a
                            // dollar and a tesla weigh comparably.
                            let xs = record
                                .candidates
                                .iter()
                                .map(|c| c.cost.lifecycle_usd.unwrap_or(c.cost.total_usd));
                            let ys = record
                                .candidates
                                .iter()
                                .filter_map(|c| c.peak_sampled_field_t);
                            let span = |it: &mut dyn Iterator<Item = f64>| {
                                let (lo, hi) = it
                                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| {
                                        (a.min(v), b.max(v))
                                    });
                                (hi - lo).max(1e-9)
                            };
                            let sx = span(&mut xs.clone());
                            let sy = span(&mut ys.clone());
                            let mut best_d = f64::INFINITY;
                            for (i, c) in record.candidates.iter().enumerate() {
                                let Some(b) = c.peak_sampled_field_t else {
                                    continue;
                                };
                                let x = c.cost.lifecycle_usd.unwrap_or(c.cost.total_usd);
                                let d = ((x - coord.x) / sx).powi(2) + ((b - coord.y) / sy).powi(2);
                                if d < best_d {
                                    best_d = d;
                                    clicked = Some(i);
                                }
                            }
                        }
                    });
                plot_resp.response.context_menu(|ui| {
                    ui.label("click a point to inspect its candidate");
                });
                if let Some(i) = clicked {
                    self.selected_candidate = i;
                }
            }
        }
        ui.add_space(12.0);
        // Search-space map: the declared choice grid as verdict-colored
        // cells — the whole feasible region at a glance. Click a cell to
        // inspect it; the blue ring tracks the cheapest PASS at the shown
        // price, the grey ring marks the declared baseline.
        ui.set_opacity(p4);
        {
            let turns = &case.choices.turns_along_normal;
            let tapes = &case.choices.tapes_along_width;
            let mut strands = case.choices.strands_parallel.clone().unwrap_or_default();
            if strands.is_empty() {
                strands.push(1);
            }
            let mut strand_sel = self.map_strand.min(strands.len() - 1);
            let strand = strands[strand_sel];
            // v21 geometry axes: when a dimension is a search axis the map
            // needs a slice selector per axis; resolved candidate dims key
            // the lookup so every declared geometry lands in a cell.
            let bends: Vec<f64> = case
                .choices
                .bend_radius_m
                .clone()
                .unwrap_or_else(|| case.fixed_geometry.bend_radius_m.into_iter().collect());
            let straights: Vec<f64> =
                case.choices
                    .straight_half_length_m
                    .clone()
                    .unwrap_or_else(|| {
                        case.fixed_geometry
                            .straight_half_length_m
                            .into_iter()
                            .collect()
                    });
            let mut bend_sel = self.map_bend.min(bends.len().saturating_sub(1));
            let mut straight_sel = self.map_straight.min(straights.len().saturating_sub(1));
            let bend = bends.get(bend_sel).copied();
            let straight = straights.get(straight_sel).copied();
            let best_i = best_index_at_price(record, shown_price, scrap_fraction);
            let baseline_i = record.baseline_index;
            let selected = self.selected_candidate;

            let mut lookup = std::collections::HashMap::new();
            for (i, c) in record.candidates.iter().enumerate() {
                lookup.insert(
                    (
                        c.geometry.turns_along_normal,
                        c.geometry.tapes_along_width,
                        c.geometry.strands_parallel,
                        c.geometry.bend_radius_m.map(f64::to_bits),
                        c.geometry.straight_half_length_m.map(f64::to_bits),
                    ),
                    i,
                );
            }

            ui.horizontal_wrapped(|ui| {
                ui.strong("Search space");
                ui.colored_label(
                    brand::MUTED,
                    "every declared geometry · click to inspect · blue ring = cheapest PASS at shown price · grey ring = baseline",
                );
                if strands.len() > 1 {
                    egui::ComboBox::from_id_salt("map-strand")
                        .selected_text(format!("{strand} strands"))
                        .show_ui(ui, |ui| {
                            for (i, s) in strands.iter().enumerate() {
                                ui.selectable_value(&mut strand_sel, i, format!("{s}"));
                            }
                        });
                }
                if case.choices.bend_radius_m.is_some() && bends.len() > 1 {
                    egui::ComboBox::from_id_salt("map-bend")
                        .selected_text(format!("R {:.3} m", bends[bend_sel]))
                        .show_ui(ui, |ui| {
                            for (i, r) in bends.iter().enumerate() {
                                ui.selectable_value(&mut bend_sel, i, format!("{r:.3}"));
                            }
                        });
                }
                if case.choices.straight_half_length_m.is_some() && straights.len() > 1 {
                    egui::ComboBox::from_id_salt("map-straight")
                        .selected_text(format!("±L {:.3} m", straights[straight_sel]))
                        .show_ui(ui, |ui| {
                            for (i, l) in straights.iter().enumerate() {
                                ui.selectable_value(&mut straight_sel, i, format!("{l:.3}"));
                            }
                        });
                }
            });
            self.map_strand = strand_sel;
            self.map_bend = bend_sel;
            self.map_straight = straight_sel;

            let mut clicked = None;
            egui::Grid::new(("search-space-map", strand))
                .spacing([4.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("tapes × turns").small().color(brand::MUTED));
                    for t in turns {
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(format!("{t}")).small().strong());
                        });
                    }
                    ui.end_row();
                    for w in tapes {
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(format!("{w}")).small().strong());
                        });
                        for t in turns {
                            let Some(&ci) = lookup.get(&(
                                *t,
                                *w,
                                strand,
                                bend.map(f64::to_bits),
                                straight.map(f64::to_bits),
                            )) else {
                                ui.label(RichText::new("·").color(brand::MUTED));
                                continue;
                            };
                            let cand = &record.candidates[ci];
                            let is_best = Some(ci) == best_i;
                            let is_base = ci == baseline_i;
                            let is_sel = ci == selected;
                            let fill = brand::status_tint(status_color(cand.status));
                            let stroke = if is_best {
                                egui::Stroke::new(2.0, brand::BLUE)
                            } else if is_sel {
                                egui::Stroke::new(2.0, Color32::BLACK)
                            } else if is_base {
                                egui::Stroke::new(1.5, brand::BASELINE)
                            } else {
                                egui::Stroke::new(0.5, brand::BASELINE.gamma_multiply(0.6))
                            };
                            let status = cand.status;
                            let total = repriced_total_usd(&cand.cost, shown_price, scrap_fraction);
                            let iop = cand.operating_current_a;
                            let resp = egui::Frame::new()
                                .fill(fill)
                                .stroke(stroke)
                                .inner_margin(egui::Margin::same(4))
                                .corner_radius(4.0)
                                .show(ui, |ui| {
                                    ui.set_min_size(egui::vec2(74.0, 38.0));
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new(format!("{t}×{w}")).small().strong(),
                                        );
                                        ui.label(RichText::new(usd_k(total)).small());
                                    });
                                })
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_ui(|ui| {
                                    ui.label(format!(
                                        "{t}×{w} · {strand} strands — {}",
                                        status_label(status)
                                    ));
                                    ui.label(format!("I_op {iop:.1} A · {}", usd(total)));
                                    if is_base {
                                        ui.label("declared baseline");
                                    }
                                    if is_best {
                                        ui.label("optimum at shown price");
                                    }
                                });
                            if resp.clicked() {
                                clicked = Some(ci);
                            }
                        }
                        ui.end_row();
                    }
                });
            if let Some(i) = clicked {
                self.selected_candidate = i;
            }
        }
        ui.add_space(12.0);
        ui.set_opacity(p5);
        ui.horizontal_wrapped(|ui| {
            ui.strong("Candidate geometries");
            ui.colored_label(
                brand::MUTED,
                "sorted by modeled cost · click a row to inspect",
            );
        });
        let baseline_cost = repriced_total_usd(
            &record.candidates[record.baseline_index].cost,
            shown_price,
            scrap_fraction,
        );
        let has_axis =
            case.choices.bend_radius_m.is_some() || case.choices.straight_half_length_m.is_some();
        let indices = self.sorted_candidates();
        TableBuilder::new(ui)
            .id_salt("search-candidate-table")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(40.0).at_least(34.0))
            .column(Column::initial(105.0).at_least(80.0))
            .column(Column::initial(85.0).at_least(60.0))
            .column(Column::initial(115.0).at_least(80.0))
            .column(Column::initial(95.0).at_least(70.0))
            .column(Column::remainder().at_least(90.0))
            .column(Column::initial(36.0).at_least(30.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(320.0)
            .header(25.0, |mut header| {
                for label in [
                    "#",
                    "Geometry",
                    "I_op / A",
                    "Cost / USD",
                    "vs baseline",
                    "Status",
                    "⇄",
                ] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, indices.len(), |mut row| {
                    let i = indices[row.index()];
                    let candidate = &self.search_record.as_ref().unwrap().candidates[i];
                    row.set_selected(self.selected_candidate == i);
                    row.col(|ui| {
                        ui.label(format!("{}", candidate.index));
                    });
                    row.col(|ui| {
                        let dims = match (
                            candidate.geometry.bend_radius_m,
                            candidate.geometry.straight_half_length_m,
                        ) {
                            (Some(r), Some(l)) if has_axis => {
                                format!(" · R{r:.2}·±{l:.2}")
                            }
                            _ => String::new(),
                        };
                        ui.label(format!(
                            "{} × {}{}{}",
                            candidate.geometry.turns_along_normal,
                            candidate.geometry.tapes_along_width,
                            strands_suffix(candidate.geometry.strands_parallel),
                            dims,
                        ));
                    });
                    row.col(|ui| {
                        ui.label(format!("{:.1}", candidate.operating_current_a));
                    });
                    row.col(|ui| {
                        ui.label(usd(repriced_total_usd(
                            &candidate.cost,
                            shown_price,
                            scrap_fraction,
                        )));
                    });
                    row.col(|ui| {
                        ui.label(format!(
                            "{:+.1}%",
                            (repriced_total_usd(&candidate.cost, shown_price, scrap_fraction)
                                - baseline_cost)
                                / baseline_cost
                                * 100.0
                        ));
                    });
                    row.col(|ui| {
                        status_chip(ui, candidate.status);
                    });
                    row.col(|ui| {
                        if ui
                            .selectable_label(self.compare_pin == Some(i), "⇄")
                            .on_hover_text(
                                "Pin for A/B compare — then select another row to see the diff",
                            )
                            .clicked()
                        {
                            self.compare_pin = if self.compare_pin == Some(i) {
                                None
                            } else {
                                Some(i)
                            };
                        }
                    });
                    if row.response().clicked() {
                        self.selected_candidate = i;
                    }
                });
            });
        ui.add_space(14.0);
        egui::CollapsingHeader::new("Independent acceptance")
            .default_open(true)
            .show(ui, |ui| {
                ui.set_opacity(reveal.max(0.15));
                for check in &record.acceptance.checks {
                    check_card(ui, check);
                }
            });
        ui.add_space(8.0);
        // Run stats — the record's own accounting, at the foot of the page.
        ui.colored_label(
            brand::MUTED,
            format!(
                "ran in {:.1} s · {} candidates · model {} · checker {}",
                record.elapsed_ms / 1000.0,
                record.candidates.len(),
                record.coupled_search_model_id,
                record.coupled_search_checker_id,
            ),
        );
        ui.add_space(8.0);
        ui.colored_label(
            brand::MUTED,
            "Screening model verdicts — structural, thermal and quench behavior are not established by a PASS.",
        );
    }

    /// The customer-facing summary: what to buy, what it costs, the
    /// limiting margin, and — above all — what the record does *not*
    /// substantiate. Rendered entirely from the record, so it can never
    /// claim more than the run did.
    fn recommendation_card(&self, ui: &mut egui::Ui, record: &CoupledSearchRunRecord) {
        let price = self
            .reprice_usd_per_m
            .unwrap_or(record.case.cost.price_usd_per_m);
        let scrap = record.case.cost.scrap_fraction;
        let best = best_index_at_price(record, price, scrap);
        egui::Frame::group(ui.style())
            .fill(brand::status_tint(brand::BLUE))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.strong("Recommendation");
                    ui.colored_label(brand::MUTED, "search");
                    status_chip(ui, record.search_status);
                    ui.colored_label(brand::MUTED, "acceptance");
                    status_chip(ui, record.acceptance.agreement_status);
                });
                let Some(i) = best else {
                    ui.colored_label(
                        status_color(Status::Fail),
                        "No candidate passed screening — nothing to recommend. The Checks page \
                         lists the failing gates.",
                    );
                    return;
                };
                let cand = &record.candidates[i];
                let g = &cand.geometry;
                let spec_note = g
                    .tape_spec_ids
                    .as_ref()
                    .map(|ids| format!(" — spec assignment: {}", ids.join(" / ")))
                    .unwrap_or_default();
                let total = repriced_total_usd(&cand.cost, price, scrap);
                let base = repriced_total_usd(
                    &record.candidates[record.baseline_index].cost,
                    price,
                    scrap,
                );
                ui.label(format!(
                    "{} × {}{}{} at {}",
                    g.turns_along_normal,
                    g.tapes_along_width,
                    strands_suffix(g.strands_parallel),
                    spec_note,
                    usd(total)
                ));
                if i != record.baseline_index && base > 0.0 {
                    ui.colored_label(
                        brand::MUTED,
                        format!(
                            "{} ({:.1}%) below the declared baseline",
                            usd(base - total),
                            (base - total) / base * 100.0
                        ),
                    );
                }
                if let Some(screening) = &cand.screening
                    && let Some(u) = screening.max_utilization
                {
                    let limit = record.case.limits.utilization_limit;
                    let loc = screening
                        .limiting
                        .as_ref()
                        .map(|p| {
                            format!(
                                " at {} (turn {}, tape {})",
                                p.station, p.turn_index, p.tape_index
                            )
                        })
                        .unwrap_or_default();
                    let tight = u >= limit * 0.95;
                    ui.colored_label(
                        if tight {
                            status_color(Status::Inconclusive)
                        } else {
                            brand::MUTED
                        },
                        format!(
                            "limiting margin: utilization {u:.4} of the declared {limit} \
                             limit{loc}{}",
                            if tight { " — thin margin" } else { "" }
                        ),
                    );
                }
                // What the run did not establish — named, not hidden.
                let mut unchecked: Vec<String> = Vec::new();
                if cand.mechanical_feasible.is_none() {
                    unchecked.push("mechanical bound undeclared".into());
                }
                match &cand.screens {
                    None => unchecked.push("no optional screens declared".into()),
                    Some(screens) => {
                        let rows: [(&str, Option<Status>); 6] = [
                            (
                                "thermal margin",
                                screens.thermal_margin.as_ref().map(|s| s.status),
                            ),
                            ("ac loss", screens.ac_loss.as_ref().map(|s| s.status)),
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
                        for (name, st) in rows {
                            match st {
                                Some(Status::Pass) => {}
                                Some(st) => unchecked.push(format!("{name}: {}", status_label(st))),
                                None => unchecked.push(format!("{name}: not declared")),
                            }
                        }
                    }
                }
                if !unchecked.is_empty() {
                    ui.colored_label(
                        status_color(Status::Inconclusive),
                        format!("Not established: {}", unchecked.join("; ")),
                    );
                }
                ui.small(format!(
                    "{} recorded limitations — the Checks page carries them verbatim. A screening \
                     pass is not manufacturing acceptance.",
                    record.limitations.len()
                ));
            });
        ui.add_space(8.0);
    }

    /// Checks page for a coupled-search project: the embedded acceptance
    /// recomputation plus the run's own checks, rendered as one ledger.
    pub(super) fn search_checks(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Checks & evidence",
            "The run record's checks and the independent acceptance recomputation, kept separate from missing engineering evidence.",
        );
        let Some(record) = self.search_record.as_ref() else {
            ui.colored_label(
                brand::MUTED,
                "No run record yet — run the search or open a saved run record to see its check ledger.",
            );
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Search").color(brand::MUTED).small());
            status_chip(ui, record.search_status);
            ui.add_space(6.0);
            ui.label(RichText::new("Acceptance").color(brand::MUTED).small());
            status_chip(ui, record.acceptance.agreement_status);
        });
        // Status strip: every recorded check as a status-proportioned
        // segmented bar — the evidence picture at a glance.
        {
            let all: Vec<Status> = record
                .acceptance
                .checks
                .iter()
                .chain(record.checks.iter())
                .map(|c| c.status)
                .collect();
            if !all.is_empty() {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 14.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                let total = all.len() as f32;
                let mut x = rect.min.x;
                for status in [
                    Status::Pass,
                    Status::Fail,
                    Status::Inconclusive,
                    Status::NotEvaluated,
                ] {
                    let n = all.iter().filter(|&&s| s == status).count() as f32;
                    if n == 0.0 {
                        continue;
                    }
                    let w = rect.width() * n / total;
                    painter.rect_filled(
                        egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(w, 14.0)),
                        2.0,
                        status_color(status),
                    );
                    x += w;
                }
                ui.add_space(4.0);
            }
        }
        // Independent acceptance: the checker's own recomputation of the
        // headline numbers — surfaced as deltas, not just a verdict word.
        {
            let acc = &record.acceptance;
            egui::Frame::group(ui.style())
                .fill(Color32::WHITE)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        status_chip(ui, acc.agreement_status);
                        ui.strong("Independent acceptance recomputation");
                        ui.colored_label(brand::MUTED, &acc.checker_id);
                    });
                    for c in acc.best.iter().chain([&acc.baseline]) {
                        ui.small(format!(
                            "{} #{}: cost within {:.2}% · bore field within {:.4} T",
                            c.label,
                            c.index,
                            c.cost_relative_error * 100.0,
                            c.field_absolute_error_t
                        ));
                    }
                });
            ui.add_space(8.0);
        }
        ui.checkbox(
            &mut self.unresolved_only,
            "Show only failed or unresolved checks",
        );
        ui.add_space(6.0);
        ui.strong("Acceptance checks");
        for check in record
            .acceptance
            .checks
            .iter()
            .filter(|c| !self.unresolved_only || c.status != Status::Pass)
        {
            check_card(ui, check);
        }
        ui.add_space(6.0);
        ui.strong("Run checks");
        for check in record
            .checks
            .iter()
            .filter(|c| !self.unresolved_only || c.status != Status::Pass)
        {
            check_card(ui, check);
        }
        if self.unresolved_only
            && record
                .acceptance
                .checks
                .iter()
                .chain(record.checks.iter())
                .all(|c| c.status == Status::Pass)
        {
            ui.colored_label(status_color(Status::Pass), "Every recorded check passed.");
        }
        // Declared screens of the selected candidate — the v15–v18 blocks
        // the case author bound, each with its honest status.
        if let Some(candidate) = record.candidates.get(self.selected_candidate)
            && let Some(screens) = &candidate.screens
        {
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong("Declared screens");
                ui.colored_label(
                    brand::MUTED,
                    format!(
                        "candidate #{} — case-author bounds, not engineering acceptance",
                        candidate.index
                    ),
                );
            });
            let mut any = false;
            macro_rules! screen_row {
                ($rec:expr, $name:literal, $metric:expr) => {
                    if let Some(s) = $rec {
                        any = true;
                        ui.horizontal_wrapped(|ui| {
                            status_chip(ui, s.status);
                            ui.label(RichText::new(format!("{} — {}", $name, $metric(s))).small());
                        });
                    }
                };
            }
            screen_row!(
                screens.thermal_margin.as_ref(),
                "thermal margin",
                |s: &optcoil_search::coupled_search::ThermalMarginScreenRecord| {
                    s.min_margin_k
                        .map(|m| format!("min {m:.1} K"))
                        .unwrap_or_else(|| "no margin evaluated".into())
                }
            );
            screen_row!(
                screens.ac_loss.as_ref(),
                "AC loss",
                |s: &optcoil_search::coupled_search::AcLossScreenRecord| {
                    s.total_loss_w
                        .map(|w| format!("{w:.1} W aggregate"))
                        .unwrap_or_else(|| "no loss evaluated".into())
                }
            );
            screen_row!(
                screens.quench_hotspot.as_ref(),
                "quench hotspot",
                |s: &optcoil_search::coupled_search::QuenchHotspotScreenRecord| {
                    s.hotspot_temperature_k
                        .map(|t| format!("{t:.0} K"))
                        .unwrap_or_else(|| "inconclusive".into())
                }
            );
            screen_row!(
                screens.screening_current.as_ref(),
                "screening current",
                |s: &optcoil_search::coupled_search::ScreeningCurrentScreenRecord| {
                    s.max_penetrated_width_fraction
                        .map(|f| format!("{:.0}% penetrated", f * 100.0))
                        .unwrap_or_else(|| "no fraction evaluated".into())
                }
            );
            screen_row!(
                screens.transition.as_ref(),
                "transition depth",
                |s: &optcoil_search::coupled_search::TransitionScreenRecord| {
                    s.max_e_over_ec
                        .map(|e| format!("max E/Ec {e:.2}"))
                        .unwrap_or_else(|| "no depth evaluated".into())
                }
            );
            screen_row!(
                screens.quench_transient.as_ref(),
                "quench transient",
                |s: &optcoil_search::coupled_search::QuenchTransientScreenRecord| {
                    s.peak_temperature_k
                        .map(|t| format!("peak {t:.0} K"))
                        .unwrap_or_else(|| "inconclusive".into())
                }
            );
            if !any {
                ui.colored_label(
                    brand::MUTED,
                    "No declared screens were evaluated for this candidate.",
                );
            }
        }
        ui.add_space(10.0);
        ui.collapsing("Run identity and model assumptions", |ui| {
            for text in [
                format!("Record schema: {}", record.schema),
                format!("Model: {}", record.coupled_search_model_id),
                format!("Checker: {}", record.coupled_search_checker_id),
                format!("Case SHA-256: {}", record.case_sha256),
                format!("Input SHA-256: {}", record.input_sha256),
                format!(
                    "{:.0} ms · {} kernel evaluations · {} sampling points",
                    record.elapsed_ms, record.kernel_evaluations, record.points_evaluated
                ),
                format!(
                    "Pruning skipped {} full-plan points (≈{:.0} kernel evaluations saved)",
                    record.points_saved_by_pruning, record.estimated_kernel_evaluations_saved
                ),
            ] {
                ui.label(text);
            }
            ui.add_space(6.0);
            ui.strong("Limitations");
            for limitation in &record.limitations {
                ui.label(limitation);
            }
        });
    }

    /// Candidates in the table's cost order — the same ordering ↑/↓
    /// key navigation walks.
    pub(super) fn sorted_candidates(&self) -> Vec<usize> {
        let Some(record) = &self.search_record else {
            return Vec::new();
        };
        let price = self
            .reprice_usd_per_m
            .unwrap_or(record.case.cost.price_usd_per_m);
        let scrap = record.case.cost.scrap_fraction;
        let mut indices: Vec<usize> = (0..record.candidates.len()).collect();
        indices.sort_by(|&a, &b| {
            lifecycle_at_price(&record.candidates[a].cost, price, scrap).total_cmp(
                &lifecycle_at_price(&record.candidates[b].cost, price, scrap),
            )
        });
        indices
    }

    /// Reports page — the decision artifacts computed from the loaded
    /// coupled-search record: BOM, margin frontier, vendor bake-off,
    /// grading delta, and the integrity verify ledger. Everything here
    /// derives from the record artifact itself, never recomputation.
    pub(super) fn reports(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Reports & artifacts",
            "Procurement-facing outputs computed from the loaded run record — the same artifacts the CLI emits, rendered.",
        );
        let Some(record) = self.search_record.clone() else {
            ui.colored_label(
                brand::MUTED,
                "No coupled-search record loaded — run a search or open a saved record to see its artifacts.",
            );
            return;
        };

        // --- Record compare ---------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            ui.strong("Compare records");
            ui.colored_label(
                brand::MUTED,
                "a second run record read-only — same case + different dataset is an instant bake-off pair",
            );
        });
        if ui
            .add_enabled(
                self.worker.is_none(),
                egui::Button::new("Compare against record…"),
            )
            .clicked()
        {
            self.pick_compare_record();
        }
        if let Some(other) = &self.compare_record {
            let cost_of = |r: &CoupledSearchRunRecord| -> Option<f64> {
                r.best_index.and_then(|i| {
                    r.candidates
                        .get(i)
                        .map(|c| c.cost.lifecycle_usd.unwrap_or(c.cost.total_usd))
                })
            };
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .fill(Color32::WHITE)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("this record").color(brand::MUTED).small());
                        status_chip(ui, record.search_status);
                        ui.label(
                            cost_of(&record)
                                .map(usd)
                                .unwrap_or_else(|| "no optimum".into()),
                        );
                        ui.label(RichText::new("vs").color(brand::MUTED).small());
                        ui.label(RichText::new("other record").color(brand::MUTED).small());
                        status_chip(ui, other.search_status);
                        ui.label(
                            cost_of(other)
                                .map(usd)
                                .unwrap_or_else(|| "no optimum".into()),
                        );
                    });
                    if let (Some(a), Some(b)) = (cost_of(&record), cost_of(other)) {
                        ui.small(format!(
                            "Δ optimum {:+} ({:+.1}%)",
                            usd(a - b),
                            (a - b) / b.abs().max(1e-9) * 100.0
                        ));
                    }
                    ui.small(format!(
                        "other record: dataset {} · started {}",
                        other.dataset_id, other.started_unix_ms
                    ));
                });
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);
        // --- Integrity verify -------------------------------------------------
        ui.strong("Integrity — verify this record");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.worker.is_none(),
                    egui::Button::new("Verify record"),
                )
                .on_hover_text(
                    "Re-checks artifact bindings, structure and ledger arithmetic against the record — the same ledger `optcoil verify` prints",
                )
                .clicked()
            {
                self.verify_now();
            }
            ui.small("artifact bindings + ledger arithmetic — physics and engineering acceptance are separate");
        });
        if let Some(checks) = &self.verify_checks {
            ui.add_space(4.0);
            for check in checks {
                ui.horizontal_wrapped(|ui| {
                    let (label, color) = match check.outcome {
                        verify::Outcome::Pass => ("PASS", status_color(Status::Pass)),
                        verify::Outcome::Fail => ("FAIL", status_color(Status::Fail)),
                        verify::Outcome::NotChecked => {
                            ("NOT_CHECKED", status_color(Status::NotEvaluated))
                        }
                    };
                    ui.colored_label(color, label);
                    ui.label(RichText::new(format!("{} — {}", check.name, check.detail)).small());
                });
            }
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);

        // --- Margin frontier --------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            ui.strong("Cost–margin frontier");
            ui.colored_label(
                brand::MUTED,
                "cheapest-PASS cost against limits.utilization_limit — runs a v3 sensitivity sweep",
            );
        });
        if ui
            .add_enabled(
                self.worker.is_none() && self.search_dataset.is_some(),
                egui::Button::new("Run margin sweep"),
            )
            .on_hover_text("Sweeps the declared utilization limit over a fixed grid — the capacity-margin price curve")
            .clicked()
        {
            self.run_frontier();
        }
        if self.search_dataset.is_none() {
            ui.small("Load the declared dataset on the Materials page first.");
        }
        if let Some(sweep) = &self.sweep_record {
            let mut frontier: Vec<[f64; 2]> = Vec::new();
            let mut inconclusive: Vec<[f64; 2]> = Vec::new();
            for point in &sweep.points {
                let Some(axis) = point.values.first() else {
                    continue;
                };
                let Some(cost) = point.optimum_total_usd else {
                    continue;
                };
                let xy = [axis.value, cost];
                if point.search_status == Status::Pass {
                    frontier.push(xy);
                } else {
                    inconclusive.push(xy);
                }
            }
            frontier.sort_by(|a, b| a[0].total_cmp(&b[0]));
            Plot::new("margin-frontier")
                .height(220.0)
                .legend(Legend::default())
                .x_axis_label("utilization limit")
                .y_axis_label("optimum cost / USD")
                .show(ui, |plot| {
                    plot.line(
                        Line::new("cheapest PASS", frontier.clone())
                            .color(brand::BLUE)
                            .width(2.0),
                    );
                    plot.points(
                        Points::new("no PASS", inconclusive)
                            .color(status_color(Status::Fail))
                            .radius(4.0),
                    );
                    if let Some(base) = sweep.points.iter().find_map(|p| p.baseline_total_usd) {
                        plot.line(
                            Line::new("declared baseline", vec![[0.0, base], [1.0, base]])
                                .color(brand::BASELINE)
                                .style(egui_plot::LineStyle::Dashed { length: 8.0 }),
                        );
                    }
                });
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);

        // --- Bill of materials ------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bill of materials");
            ui.colored_label(
                brand::MUTED,
                "purchasable spec per tape-spec — the optimum's ledger walked to meters",
            );
        });
        match &self.bom_record {
            Some(bom) => {
                TableBuilder::new(ui)
                    .id_salt("bom-table")
                    .striped(true)
                    .column(Column::remainder().at_least(90.0))
                    .column(Column::initial(70.0).at_least(60.0))
                    .column(Column::initial(80.0).at_least(60.0))
                    .column(Column::initial(90.0).at_least(70.0))
                    .column(Column::initial(90.0).at_least(70.0))
                    .column(Column::initial(90.0).at_least(70.0))
                    .header(24.0, |mut header| {
                        for label in [
                            "Spec",
                            "$/m",
                            "Turns",
                            "Installed / m",
                            "Purchase / m",
                            "Conductor $",
                        ] {
                            header.col(|ui| {
                                ui.strong(label);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(24.0, bom.spec_rows.len(), |mut row| {
                            let r = &bom.spec_rows[row.index()];
                            row.col(|ui| {
                                ui.label(&r.spec_id);
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.2}", r.price_usd_per_m));
                            });
                            row.col(|ui| {
                                ui.label(
                                    r.turn_ranges
                                        .iter()
                                        .map(|t| format!("{}–{}", t[0], t[1]))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                );
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1}", r.installed_length_m));
                            });
                            row.col(|ui| {
                                ui.label(format!("{:.1}", r.purchased_length_m));
                            });
                            row.col(|ui| {
                                ui.label(usd(r.conductor_usd));
                            });
                        });
                    });
                ui.small(format!(
                    "{} conductor layers · {} joints ({}) · {} pancakes ({})",
                    bom.conductor_layers,
                    bom.joint_count,
                    usd(bom.joints_usd),
                    bom.pancake_count,
                    usd(bom.assembly_usd),
                ));
                ui.colored_label(
                    brand::MUTED,
                    "File → Export RFQ… writes the procurement-facing schedule — piece lengths, \
                     spec assignments, joints and totals — with the record's provenance hashes.",
                );
            }
            None => {
                ui.colored_label(
                    brand::MUTED,
                    "No PASS optimum in this record — nothing to buy.",
                );
            }
        }
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(10.0);

        // --- Grading delta ----------------------------------------------------
        if let Some(report) = &self.grade_report {
            ui.strong("Graded-vs-uniform");
            if report.graded {
                match (report.grading_delta_usd, report.grading_delta_percent) {
                    (Some(delta), Some(pct)) => {
                        ui.label(format!(
                            "Grading the optimum saves {} ({:.1}%) vs the best uniform spec{}",
                            usd(delta),
                            pct,
                            report
                                .best_uniform_spec_id
                                .as_ref()
                                .map(|id| format!(" ({id})"))
                                .unwrap_or_default(),
                        ));
                    }
                    _ => {
                        ui.label("Graded optimum found; no uniform comparator passed.");
                    }
                }
            } else {
                ui.colored_label(
                    brand::MUTED,
                    "This record is ungraded — nothing to compare.",
                );
            }
            ui.add_space(14.0);
            ui.separator();
            ui.add_space(10.0);
        }

        // --- Vendor bake-off ---------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            ui.strong("Vendor bake-off");
            ui.colored_label(
                brand::MUTED,
                "the same case run under every dataset bundle in a folder",
            );
        });
        if ui
            .add_enabled(
                self.worker.is_none(),
                egui::Button::new("Choose bundle folder…"),
            )
            .on_hover_text(
                "Every optcoil-material-dataset bundle in the folder is run against this case",
            )
            .clicked()
        {
            self.pick_bakeoff_dir();
        }
        if let Some(bakeoff) = &self.bakeoff_record {
            ui.add_space(4.0);
            ui.small(&bakeoff.ranking_basis);
            TableBuilder::new(ui)
                .id_salt("bakeoff-table")
                .striped(true)
                .column(Column::initial(40.0))
                .column(Column::remainder().at_least(110.0))
                .column(Column::initial(80.0).at_least(70.0))
                .column(Column::initial(95.0).at_least(70.0))
                .column(Column::initial(80.0).at_least(60.0))
                .header(24.0, |mut header| {
                    for label in ["Rank", "Dataset", "Optimum $", "Savings", "Status"] {
                        header.col(|ui| {
                            ui.strong(label);
                        });
                    }
                })
                .body(|body| {
                    body.rows(24.0, bakeoff.entries.len(), |mut row| {
                        let e = &bakeoff.entries[row.index()];
                        let rank = bakeoff
                            .ranking
                            .iter()
                            .position(|id| id == &e.dataset_id)
                            .map(|i| i + 1);
                        row.col(|ui| {
                            ui.label(rank.map(|r| format!("{r}")).unwrap_or_else(|| "—".into()));
                        });
                        row.col(|ui| {
                            ui.label(&e.dataset_id);
                        });
                        row.col(|ui| {
                            ui.label(e.optimum_total_usd.map(usd).unwrap_or_else(|| "—".into()));
                        });
                        row.col(|ui| {
                            ui.label(
                                e.savings_percent
                                    .map(|p| format!("{p:.1}%"))
                                    .unwrap_or_else(|| "—".into()),
                            );
                        });
                        row.col(|ui| {
                            status_chip(ui, e.search_status);
                        });
                    });
                });
        }
        let _ = record;
    }

    /// Materials page for a coupled-search project: the declared material
    /// dataset, policies, sampling plan and screening limits.
    pub(super) fn search_materials(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Material & operating point",
            "The coupled case's declared dataset and screening settings — all case-author assumptions, versioned in the run record.",
        );
        let mut wants_dataset = false;
        {
            let Some(case) = self.search_case.as_ref() else {
                return;
            };
            egui::Grid::new("search-material-grid")
            .num_columns(2)
            .spacing([20.0, 8.0])
            .show(ui, |ui| {
                ui.strong("Material dataset");
                ui.vertical(|ui| {
                    ui.label(&case.material.dataset_id);
                    ui.small(format!("CSV SHA-256: {}", case.material.csv_sha256));
                    match self.search_dataset.as_ref().map(|s| &s.origin) {
                        Some(crate::DatasetOrigin::Embedded) => {
                            ui.small("Source: embedded dataset store");
                        }
                        Some(crate::DatasetOrigin::File(path)) => {
                            ui.small(format!(
                                "Source: external bundle — {}",
                                path.display()
                            ));
                        }
                        None => {
                            ui.colored_label(
                                status_color(Status::Inconclusive),
                                "Not embedded — load a dataset bundle to run this case",
                            );
                        }
                    }
                    if let Some(source) = self.search_dataset.as_ref()
                        && matches!(source.origin, crate::DatasetOrigin::File(_))
                    {
                        match &source.attestation {
                            Some(a) => ui.small(format!(
                                "Signed by {} (key {}) — verify with `optcoil dataset verify`",
                                a.issuer, a.key_id
                            )),
                            None => ui.small(
                                "Unsigned bundle — no issuer attestation",
                            ),
                        };
                    }
                    if ui
                        .button("Load dataset bundle…")
                        .on_hover_text(
                            "Open an optcoil-material-dataset bundle (v1 or signed v2) whose id and CSV SHA-256 match this case's declared material",
                        )
                        .clicked()
                    {
                        wants_dataset = true;
                    }
                });
                ui.end_row();
                ui.strong("Operating point");
                ui.label(format!(
                    "{:.1} K · E criterion {:.0e} V/m",
                    case.operating.temperature_k, case.operating.electric_field_criterion_v_per_m
                ));
                ui.end_row();
                ui.strong("Ic method");
                ui.label(RichText::new(&case.material.method).monospace().small());
                ui.end_row();
                ui.strong("Field policies");
                ui.vertical(|ui| {
                    for (name, value) in [
                        ("angle", serde_json::to_string(&case.material.angle_mapping)),
                        (
                            "mirror",
                            serde_json::to_string(&case.material.mirror_policy),
                        ),
                        (
                            "basis",
                            serde_json::to_string(&case.material.field_basis_mapping),
                        ),
                        (
                            "magnitude",
                            serde_json::to_string(&case.material.field_magnitude_policy),
                        ),
                        (
                            "low-field",
                            serde_json::to_string(&case.material.low_field_policy),
                        ),
                    ] {
                        ui.label(format!(
                            "{name}: {}",
                            value.unwrap_or_default().trim_matches('"')
                        ));
                    }
                    ui.label(format!(
                        "low-field clamp: {:.3} T · monotonicity tol {:.3}",
                        case.material.low_field_clamp_t, case.material.monotonicity_tolerance
                    ));
                });
                ui.end_row();
                ui.strong("Sampling plan");
                ui.vertical(|ui| {
                    ui.label(format!(
                        "{} stations · {} turn indices · {} width points",
                        case.sampling.stations.len(),
                        case.sampling.relative_turn_indices.len(),
                        case.sampling.width_points
                    ));
                    ui.label(format!(
                        "refined: +{} stations · shortfall ≤ {:.2}%",
                        case.refined_plan.additional_stations.len(),
                        case.refined_plan.max_sampling_shortfall_fraction * 100.0
                    ));
                    if let Some(pruning) = &case.pruning {
                        ui.label(format!(
                            "coarse pruning: {} stations · {} turn fractions · {} width indices",
                            pruning.coarse_stations.len(),
                            pruning.coarse_turn_fractions.len(),
                            pruning.coarse_width_indices.len()
                        ));
                    }
                });
                ui.end_row();
                ui.strong("Screening limits");
                ui.vertical(|ui| {
                    ui.label(format!(
                        "utilization ≤ {:.0}% · along-current ≤ {:.0}% · self-field ratio ≤ {:.2}",
                        case.limits.utilization_limit * 100.0,
                        case.limits.max_along_current_field_fraction * 100.0,
                        case.limits.max_self_field_ratio
                    ));
                    ui.label(format!(
                        "self-field correction: {}",
                        case.limits
                            .self_field_correction
                            .as_deref()
                            .unwrap_or("none")
                    ));
                    if let Some(d) = case.limits.critical_state_layer_thickness_m {
                        ui.label(format!("critical-state layer thickness: {d:.3e} m"));
                    }
                    ui.label(format!(
                        "interpolation over-prediction budget: {:.3}",
                        case.limits.interpolation_overprediction_budget
                    ));
                });
                ui.end_row();
                if let Some(mechanical) = &case.mechanical {
                    ui.strong("Mechanical screen");
                    ui.vertical(|ui| {
                        ui.label(format!(
                            "Lorentz load ≤ {:.0} kN/m",
                            mechanical.max_lorentz_load_n_per_m / 1000.0
                        ));
                        if let Some(hoop) = mechanical.max_hoop_stress_pa {
                            ui.label(format!("hoop stress ≤ {:.0} MPa", hoop / 1e6));
                        }
                        ui.colored_label(brand::MUTED, "First-order bound, not stress analysis.");
                    });
                    ui.end_row();
                }
                if let Some(manufacturing) = &case.manufacturing {
                    ui.strong("Manufacturing");
                    ui.label(format!(
                        "inner bend radius ≥ {:.3} m",
                        manufacturing.min_inner_bend_radius_m
                    ));
                    ui.end_row();
                }
                ui.strong("Quadrature");
                ui.label(format!(
                    "orders {} / {} · field scale {:.2} T",
                    case.numerics.quadrature_orders[0],
                    case.numerics.quadrature_orders[1],
                    case.numerics.field_scale_t
                ));
                ui.end_row();
            });
        }
        if wants_dataset {
            self.load_dataset(ui.ctx());
        }
        ui.add_space(8.0);
        ui.colored_label(
            brand::MUTED,
            "Measured data and labeled model extensions are distinct domains — the dataset id and method above say which this case used.",
        );
    }

    /// Inspector for a coupled-search project: the selected candidate's
    /// verdict detail, or the fixed geometry before a run.
    pub(super) fn search_inspector(&mut self, ui: &mut egui::Ui) {
        let mut profile_for = None;
        ui.set_opacity(egui::emath::easing::cubic_out(
            self.anim_progress(self.inspector_fade_start, 180),
        ));
        ui.add_space(10.0);
        ui.label(
            RichText::new("CANDIDATE INSPECTOR")
                .color(brand::BLUE)
                .small(),
        );
        let Some(record) = &self.search_record else {
            let Some(case) = &self.search_case else {
                return;
            };
            ui.heading(&case.id);
            ui.small("Fixed geometry & requirement");
            ui.add_space(12.0);
            for text in [
                match &case.fixed_geometry.path {
                    Some(path) => format!(
                        "Planar path · {} segments · L {:.3} m",
                        path.segments.len(),
                        path.length_m()
                    ),
                    None => format!(
                        "Straight {} · bend R {}",
                        case.fixed_geometry
                            .straight_half_length_m
                            .map(|l| format!("±{l:.3} m"))
                            .unwrap_or_else(|| "searched".into()),
                        case.fixed_geometry
                            .bend_radius_m
                            .map(|r| format!("{r:.3} m"))
                            .unwrap_or_else(|| "searched".into())
                    ),
                },
                format!(
                    "Radial pitch {:.4} m · tape {:.3} m wide",
                    case.fixed_geometry.radial_pitch_m, case.fixed_geometry.tape_width_m
                ),
                format!("Bore target: {:.2} T", case.requirement.b_target_t),
                format!("{} candidate geometries", case.candidate_count()),
            ] {
                ui.label(text);
            }
            ui.separator();
            ui.small("Run the search to inspect candidates.");
            return;
        };
        let Some(candidate) = record.candidates.get(self.selected_candidate) else {
            ui.small("Select a candidate row.");
            return;
        };
        ui.heading(format!(
            "{} × {}{}",
            candidate.geometry.turns_along_normal,
            candidate.geometry.tapes_along_width,
            strands_suffix(candidate.geometry.strands_parallel)
        ));
        ui.small(format!(
            "candidate #{} of {}",
            candidate.index,
            record.candidates.len()
        ));
        ui.add_space(12.0);
        ui.strong("Verdicts");
        ui.horizontal_wrapped(|ui| {
            status_chip(ui, candidate.status);
            ui.label(RichText::new("overall").color(brand::MUTED).small());
        });
        for (name, status) in [
            ("requirement", candidate.requirement_status),
            ("refinement", candidate.refinement_status),
            (
                "screening",
                candidate
                    .screening
                    .as_ref()
                    .map_or(Status::NotEvaluated, |s| s.status),
            ),
        ] {
            ui.horizontal_wrapped(|ui| {
                status_chip(ui, status);
                ui.label(RichText::new(name).color(brand::MUTED).small());
            });
        }
        if let Some(feasible) = candidate.mechanical_feasible {
            ui.horizontal_wrapped(|ui| {
                status_chip(ui, if feasible { Status::Pass } else { Status::Fail });
                ui.label(RichText::new("mechanical").color(brand::MUTED).small());
            });
        }
        // A/B compare: the pinned candidate against the current
        // selection — the deltas that actually drive the choice.
        if let Some(pin_i) = self.compare_pin.filter(|&p| p != self.selected_candidate)
            && let Some(other) = record.candidates.get(pin_i)
        {
            ui.add_space(8.0);
            egui::Frame::group(ui.style())
                .fill(brand::status_tint(brand::BLUE))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(format!("A/B — pinned #{}", other.index));
                        status_chip(ui, other.status);
                    });
                    let dg = &other.geometry;
                    let cg = &candidate.geometry;
                    let mut rows = vec![
                        (
                            "turns × tapes",
                            format!("{} × {}", dg.turns_along_normal, dg.tapes_along_width),
                            format!("{} × {}", cg.turns_along_normal, cg.tapes_along_width),
                        ),
                        (
                            "I_op",
                            format!("{:.1} A", other.operating_current_a),
                            format!("{:.1} A", candidate.operating_current_a),
                        ),
                    ];
                    if dg.bend_radius_m.is_some() || cg.bend_radius_m.is_some() {
                        rows.push((
                            "bend / straight",
                            format!(
                                "R{} · ±{}",
                                dg.bend_radius_m
                                    .map(|r| format!("{r:.2}"))
                                    .unwrap_or("—".into()),
                                dg.straight_half_length_m
                                    .map(|l| format!("{l:.2}"))
                                    .unwrap_or("—".into()),
                            ),
                            format!(
                                "R{} · ±{}",
                                cg.bend_radius_m
                                    .map(|r| format!("{r:.2}"))
                                    .unwrap_or("—".into()),
                                cg.straight_half_length_m
                                    .map(|l| format!("{l:.2}"))
                                    .unwrap_or("—".into()),
                            ),
                        ));
                    }
                    rows.push((
                        "cost",
                        usd(other.cost.lifecycle_usd.unwrap_or(other.cost.total_usd)),
                        usd(candidate
                            .cost
                            .lifecycle_usd
                            .unwrap_or(candidate.cost.total_usd)),
                    ));
                    for (label, a, b) in rows {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(label).color(brand::MUTED).small());
                            ui.label(RichText::new(a).small());
                            ui.label(RichText::new("→").color(brand::MUTED).small());
                            ui.label(RichText::new(b).small().strong());
                        });
                    }
                    let dc = candidate
                        .cost
                        .lifecycle_usd
                        .unwrap_or(candidate.cost.total_usd)
                        - other.cost.lifecycle_usd.unwrap_or(other.cost.total_usd);
                    ui.small(format!("Δ lifecycle {:+}", usd(dc)));
                });
        }
        ui.add_space(10.0);
        ui.strong("Operating point");
        for text in [
            format!(
                "I_op: {:.1} A ({:.0} A·turns)",
                candidate.operating_current_a, candidate.ampere_turns_a
            ),
            format!(
                "pack: {:.3} m radial × {:.3} m axial",
                candidate.geometry.radial_width_m, candidate.geometry.axial_height_m
            ),
        ] {
            ui.label(text);
        }
        // Resolved winding shape — candidate dims are always populated,
        // including under v21 geometry-axis cases where the case-level
        // fixed fields are absent.
        if let (Some(straight), Some(bend)) = (
            candidate.geometry.straight_half_length_m,
            candidate.geometry.bend_radius_m,
        ) {
            ui.label(format!("racetrack: ±{straight:.3} m · R {bend:.3} m"));
        } else {
            ui.label("winding: declared path geometry");
        }
        if let Some(spec_ids) = &candidate.geometry.tape_spec_ids {
            let assignment = spec_ids
                .iter()
                .enumerate()
                .map(|(i, id)| format!("region {} → {id}", i + 1))
                .collect::<Vec<_>>()
                .join(" · ");
            ui.label(format!("spec grading: {assignment}"));
        }
        if let (Some(straight), Some(bend)) = (
            candidate.geometry.straight_half_length_m,
            candidate.geometry.bend_radius_m,
        ) {
            ui.add_space(8.0);
            ui.strong("Winding plan");
            racetrack_plan(
                ui,
                bend,
                straight,
                self.search_case
                    .as_ref()
                    .map(|c| c.requirement.bore_probe_m),
                self.search_case
                    .as_ref()
                    .and_then(|c| c.requirement.good_field_region.as_ref())
                    .map(|r| r.half_extents_m),
            );
            if ui
                .add_enabled(
                    self.worker.is_none(),
                    egui::Button::new("Bore Bz(z) profile"),
                )
                .on_hover_text("Physics evaluator runs on a worker thread — samples the axial field through the bore probe plane")
                .clicked()
            {
                profile_for = Some(candidate.index);
            }
            if let Some((idx, points)) = &self.field_profile
                && *idx == candidate.index
            {
                Plot::new("bore-profile")
                    .height(150.0)
                    .x_axis_label("z / m")
                    .y_axis_label("Bz / T")
                    .show(ui, |plot| {
                        plot.line(
                            Line::new("Bz", points.clone())
                                .color(brand::BLUE)
                                .width(2.0),
                        );
                    });
            }
        }
        // Winding-pack cross-section: tapes along width × turns along the
        // normal, colored by spec assignment (base conductor blue,
        // declared specs cycling a fixed palette). Under grading this is
        // the "where did the premium tape go" answer at a glance.
        {
            let turns = candidate.geometry.turns_along_normal as usize;
            let tapes = candidate.geometry.tapes_along_width as usize;
            if turns > 0 && tapes > 0 {
                ui.add_space(8.0);
                ui.strong("Winding pack (cross-section)");
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(250.0, 140.0), egui::Sense::hover());
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 4.0, brand::BACKGROUND);
                let cw = rect.width() / turns as f32;
                let ch = rect.height() / tapes as f32;
                let spec_palette = [
                    Color32::from_rgb(0xC8, 0x54, 0x1E), // burnt orange
                    Color32::from_rgb(0x2E, 0x7D, 0x5B), // green
                    Color32::from_rgb(0x8E, 0x5B, 0xB5), // purple
                    Color32::from_rgb(0xB5, 0x8A, 0x2E), // ochre
                ];
                let spec_ids = candidate.geometry.tape_spec_ids.as_deref();
                let regions = self
                    .search_case
                    .as_ref()
                    .and_then(|c| c.grading.as_ref())
                    .map(|g| g.regions.as_slice())
                    .unwrap_or(&[]);
                for k in 0..turns {
                    // Region coverage: floor(lo*N) < turn <= floor(hi*N),
                    // turns counted 1-based from the inner face outward.
                    let turn_1b = k + 1;
                    let region_i = regions.iter().position(|r| {
                        let lo = (r.turn_range[0] * turns as f64).floor() as usize;
                        let hi = (r.turn_range[1] * turns as f64).floor() as usize;
                        turn_1b > lo && turn_1b <= hi
                    });
                    let spec_i = region_i.and_then(|r| {
                        spec_ids.and_then(|ids| ids.get(r)).and_then(|id| {
                            self.search_case
                                .as_ref()
                                .and_then(|c| c.tape_specs.as_ref())
                                .and_then(|s| s.keys().position(|k| k == id))
                        })
                    });
                    let fill = match spec_i {
                        Some(i) => spec_palette[i % spec_palette.len()],
                        None => brand::BLUE,
                    };
                    for j in 0..tapes {
                        let cell = egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + k as f32 * cw, rect.min.y + j as f32 * ch),
                            egui::vec2(cw - 1.0, ch - 1.0),
                        );
                        painter.rect_filled(cell, 1.5, fill);
                    }
                }
                resp.on_hover_ui(|ui| {
                    ui.label(format!(
                        "{turns} turns × {tapes} tapes — inner face left, outer right"
                    ));
                    if !regions.is_empty() {
                        ui.label("colored by assigned spec");
                    }
                });
            }
        }
        if let Some(peak) = candidate.peak_sampled_field_t {
            ui.label(format!("peak sampled field: {peak:.3} T"));
        }
        if let Some(load) = candidate.lorentz_load_n_per_m {
            ui.label(format!("Lorentz load: {:.0} kN/m", load / 1000.0));
        }
        if let Some(hoop) = candidate.hoop_stress_pa {
            ui.label(format!("hoop stress: {:.0} MPa", hoop / 1e6));
        }
        if let Some(screening) = &candidate.screening {
            ui.add_space(10.0);
            ui.strong("Screening detail");
            if let Some(allowed) = screening.min_allowed_screening_a {
                ui.label(format!("allowed current: {allowed:.1} A"));
            }
            if let Some(utilization) = screening.max_utilization {
                ui.label(format!("max utilization: {:.1}%", utilization * 100.0));
            }
            if let Some(limiting) = &screening.limiting {
                ui.label(format!(
                    "limiting: {} · tape {} · turn {} · width {}",
                    limiting.station,
                    limiting.tape_index,
                    limiting.turn_index,
                    limiting.width_index
                ));
            }
            let counts = &screening.point_counts;
            ui.small(format!(
                "{} estimates · {} lower bounds · {} unsupported",
                counts.estimate, counts.lower_bound, counts.unsupported
            ));
        }
        ui.add_space(10.0);
        ui.strong("Constraint headroom");
        ui.small("share of each declared limit in use — what binds, and by how much");
        let case = &record.case;
        let mut rows: Vec<(String, f64, String)> = Vec::new();
        if let Some(screening) = &candidate.screening {
            if let Some(u) = screening.max_utilization {
                rows.push((
                    "Ic screen".into(),
                    u,
                    format!(
                        "I_op {:.0} A at {:.0}% of allowed",
                        candidate.operating_current_a,
                        u * 100.0
                    ),
                ));
            }
            let counts = &screening.point_counts;
            let total_pts = (counts.estimate + counts.lower_bound + counts.unsupported) as f64;
            if total_pts > 0.0 {
                let share = counts.unsupported as f64 / total_pts;
                rows.push((
                    "Coverage".into(),
                    share,
                    format!(
                        "{} of {} sampled points unsupported",
                        counts.unsupported, total_pts as u64
                    ),
                ));
            }
        }
        if let (Some(load), Some(mechanical)) =
            (candidate.lorentz_load_n_per_m, case.mechanical.as_ref())
        {
            rows.push((
                "Lorentz load".into(),
                load / mechanical.max_lorentz_load_n_per_m,
                format!(
                    "{:.0} of {:.0} kN/m",
                    load / 1000.0,
                    mechanical.max_lorentz_load_n_per_m / 1000.0
                ),
            ));
            if let (Some(hoop), Some(limit)) =
                (candidate.hoop_stress_pa, mechanical.max_hoop_stress_pa)
            {
                rows.push((
                    "Hoop stress".into(),
                    hoop / limit,
                    format!("{:.0} of {:.0} MPa", hoop / 1e6, limit / 1e6),
                ));
            }
        }
        if let Some(manufacturing) = case.manufacturing.as_ref() {
            let inner = case
                .fixed_geometry
                .min_inner_radius_m(candidate.geometry.radial_width_m)
                .unwrap_or(f64::NEG_INFINITY);
            if inner > 0.0 {
                rows.push((
                    "Bend radius".into(),
                    manufacturing.min_inner_bend_radius_m / inner,
                    format!(
                        "{:.3} m inner vs {:.3} m minimum",
                        inner, manufacturing.min_inner_bend_radius_m
                    ),
                ));
            }
        }
        if rows.is_empty() {
            ui.small("No evaluated constraints on this candidate.");
        }
        for (label, usage, detail) in rows {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).small());
                ui.add(
                    egui::ProgressBar::new(usage.clamp(0.0, 1.0) as f32)
                        .desired_width(90.0)
                        .fill(headroom_color(usage))
                        .corner_radius(2.0)
                        .text(format!("{:.0}%", usage * 100.0)),
                );
                ui.small(detail);
            });
        }
        limiting_anatomy(ui, &record.case, candidate);
        if let Some(pruned) = &candidate.pruned_by {
            ui.colored_label(
                brand::MUTED,
                format!(
                    "Coarse-pruned at {} · turn {} · width {}",
                    pruned.station, pruned.turn_index, pruned.width_index
                ),
            );
        }
        if let Some(gf) = &candidate.good_field {
            ui.add_space(10.0);
            ui.strong("Field quality (usable-volume region)");
            ui.label(format!(
                "B_z range {:.4}–{:.4} T/A·turns",
                gf.min_unit_bz_t_per_ampere_turn,
                gf.max_unit_bz_t_per_ampere_turn.unwrap_or(f64::NAN)
            ));
            if let Some(dev) = gf.relative_deviation {
                ui.label(format!(
                    "peak-to-peak deviation: {:.1} ppm of center",
                    dev * 1e6
                ));
            }
            if gf.pack_overlap {
                ui.colored_label(
                    status_color(Status::Fail),
                    "region overlaps the winding pack",
                );
            }
            for (label, units) in [
                ("normal b_n", &gf.harmonic_normal_units),
                ("skew a_n", &gf.harmonic_skew_units),
            ] {
                if let Some(units) = units {
                    let mut ranked: Vec<(usize, f64)> =
                        units.iter().enumerate().map(|(i, &v)| (i + 1, v)).collect();
                    ranked.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
                    let top: Vec<String> = ranked
                        .iter()
                        .take(3)
                        .map(|(n, v)| {
                            format!(
                                "{}_{} {:+.1}",
                                label.split(' ').next().unwrap_or(""),
                                n,
                                v * 1e4
                            )
                        })
                        .collect();
                    ui.label(format!(
                        "{label} top |units|: {} (×10⁻⁴ of B₀)",
                        top.join("  ")
                    ));
                }
            }
        }
        if let Some(screens) = &candidate.screens {
            ui.add_space(10.0);
            ui.strong("Declared screens");
            let rows: [(Option<Status>, String); 6] = [
                (
                    screens.thermal_margin.as_ref().map(|s| s.status),
                    screens
                        .thermal_margin
                        .as_ref()
                        .map(|s| {
                            format!(
                                "thermal margin — min {}{}",
                                s.min_margin_k
                                    .map(|m| format!("{m:.1} K"))
                                    .unwrap_or_else(|| "—".into()),
                                if s.min_margin_is_lower_bound {
                                    " (span-bound)"
                                } else {
                                    ""
                                }
                            )
                        })
                        .unwrap_or_default(),
                ),
                (
                    screens.ac_loss.as_ref().map(|s| s.status),
                    screens
                        .ac_loss
                        .as_ref()
                        .map(|s| {
                            format!(
                                "AC loss — total {}",
                                s.total_loss_w
                                    .map(|w| format!("{w:.1} W"))
                                    .unwrap_or_else(|| "—".into())
                            )
                        })
                        .unwrap_or_default(),
                ),
                (
                    screens.quench_hotspot.as_ref().map(|s| s.status),
                    screens
                        .quench_hotspot
                        .as_ref()
                        .map(|s| {
                            format!(
                                "quench hotspot — {}{}",
                                s.hotspot_temperature_k
                                    .map(|t| format!("{t:.0} K"))
                                    .unwrap_or_else(|| "—".into()),
                                if s.table_exhausted {
                                    " (table exhausted)"
                                } else {
                                    ""
                                }
                            )
                        })
                        .unwrap_or_default(),
                ),
                (
                    screens.screening_current.as_ref().map(|s| s.status),
                    screens
                        .screening_current
                        .as_ref()
                        .map(|s| {
                            format!(
                                "screening current — penetrated {}",
                                s.max_penetrated_width_fraction
                                    .map(|f| format!("{f:.0}%", f = f * 100.0))
                                    .unwrap_or_else(|| "—".into())
                            )
                        })
                        .unwrap_or_default(),
                ),
                (
                    screens.transition.as_ref().map(|s| s.status),
                    screens
                        .transition
                        .as_ref()
                        .map(|s| {
                            format!(
                                "transition — max E/Ec {}",
                                s.max_e_over_ec
                                    .map(|e| format!("{e:.2}"))
                                    .unwrap_or_else(|| "—".into())
                            )
                        })
                        .unwrap_or_default(),
                ),
                (
                    screens.quench_transient.as_ref().map(|s| s.status),
                    screens
                        .quench_transient
                        .as_ref()
                        .map(|s| {
                            format!(
                                "quench transient — peak {}",
                                s.peak_temperature_k
                                    .map(|t| format!("{t:.0} K"))
                                    .unwrap_or_else(|| "—".into())
                            )
                        })
                        .unwrap_or_default(),
                ),
            ];
            for (status, text) in rows {
                if let (Some(status), text) = (status, text) {
                    ui.horizontal_wrapped(|ui| {
                        status_chip(ui, status);
                        ui.label(RichText::new(text).small());
                    });
                }
            }
            // Drill-downs: every recorded field per screen — the record's
            // own detail, expanded on demand.
            let detail = |ui: &mut egui::Ui, title: &str, lines: Vec<String>| {
                egui::CollapsingHeader::new(RichText::new(title).small())
                    .id_salt(("screen-drill", title))
                    .default_open(false)
                    .show(ui, |ui| {
                        for line in lines {
                            ui.label(RichText::new(line).small());
                        }
                    });
            };
            if let Some(s) = &screens.thermal_margin {
                detail(
                    ui,
                    "thermal margin detail",
                    vec![
                        format!("model: {}", s.model),
                        format!(
                            "min margin: {}{}",
                            s.min_margin_k
                                .map(|m| format!("{m:.2} K"))
                                .unwrap_or_else(|| "—".into()),
                            if s.min_margin_is_lower_bound {
                                " (lower bound — span edge)"
                            } else {
                                ""
                            }
                        ),
                        format!(
                            "evaluated: {} · inconclusive: {}",
                            s.points_evaluated, s.points_inconclusive
                        ),
                        s.limiting
                            .as_ref()
                            .map(|l| {
                                format!(
                                    "limiting: station {} · turn {} · tape {} · width {}",
                                    l.station, l.turn_index, l.tape_index, l.width_index
                                )
                            })
                            .unwrap_or_default(),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
            if let Some(s) = &screens.ac_loss {
                detail(
                    ui,
                    "AC loss detail",
                    vec![
                        format!("model: {}", s.model),
                        s.transport_loss_w
                            .map(|w| format!("transport: {w:.2} W"))
                            .unwrap_or_default(),
                        s.parallel_slab_loss_w
                            .map(|w| format!("parallel slab: {w:.2} W"))
                            .unwrap_or_default(),
                        s.perpendicular_bound_w
                            .map(|w| format!("perpendicular bound: {w:.2} W"))
                            .unwrap_or_default(),
                        s.total_loss_w
                            .map(|w| format!("total: {w:.2} W"))
                            .unwrap_or_default(),
                        s.peak_loss_j_per_m_per_cycle
                            .map(|j| format!("peak: {j:.2} J/m/cycle"))
                            .unwrap_or_default(),
                        format!(
                            "evaluated: {} · inconclusive: {}",
                            s.points_evaluated, s.points_inconclusive
                        ),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
            if let Some(s) = &screens.quench_hotspot {
                detail(
                    ui,
                    "quench hotspot detail",
                    vec![
                        format!("model: {}", s.model),
                        s.hotspot_temperature_k
                            .map(|t| format!("hotspot: {t:.0} K"))
                            .unwrap_or_default(),
                        format!("table exhausted: {}", s.table_exhausted),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
            if let Some(s) = &screens.screening_current {
                detail(
                    ui,
                    "screening current detail",
                    vec![
                        format!("model: {}", s.model),
                        s.max_penetrated_width_fraction
                            .map(|f| format!("penetrated width: {:.0}%", f * 100.0))
                            .unwrap_or_default(),
                        s.max_b_perp_over_bc
                            .map(|b| format!("max B⊥/Bc: {b:.2}"))
                            .unwrap_or_default(),
                        format!(
                            "evaluated: {} · inconclusive: {}",
                            s.points_evaluated, s.points_inconclusive
                        ),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
            if let Some(s) = &screens.transition {
                detail(
                    ui,
                    "transition detail",
                    vec![
                        format!("model: {}", s.model),
                        s.max_e_over_ec
                            .map(|e| format!("max E/Ec: {e:.3}"))
                            .unwrap_or_default(),
                        s.terminal_voltage_v
                            .map(|v| format!("terminal: {v:.1} V"))
                            .unwrap_or_default(),
                        s.worst_utilization
                            .map(|u| format!("worst utilization: {:.0}%", u * 100.0))
                            .unwrap_or_default(),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
            if let Some(s) = &screens.quench_transient {
                detail(
                    ui,
                    "quench transient detail",
                    vec![
                        format!("model: {}", s.model),
                        s.peak_temperature_k
                            .map(|t| {
                                format!(
                                    "peak: {t:.0} K{}",
                                    if s.peak_temperature_is_lower_bound {
                                        " (lower bound)"
                                    } else {
                                        ""
                                    }
                                )
                            })
                            .unwrap_or_default(),
                        s.detection_time_s
                            .map(|t| format!("detection: {t:.3} s"))
                            .unwrap_or_default(),
                        s.peak_series_voltage_v
                            .map(|v| format!("peak series: {v:.0} V"))
                            .unwrap_or_default(),
                        format!(
                            "evaluated: {} · sharing: {} · inconclusive: {}",
                            s.points_evaluated, s.points_sharing, s.points_inconclusive
                        ),
                        format!("coverage exhausted: {}", s.coverage_exhausted),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                );
            }
        }
        ui.add_space(10.0);
        ui.strong("Cost ledger");
        for text in [
            format!(
                "installed {:.0} m · purchased {:.0} m",
                candidate.cost.installed_length_m, candidate.cost.purchased_length_m
            ),
            format!("conductor {}", usd(candidate.cost.conductor_usd)),
            format!("scrap {}", usd(candidate.cost.scrap_usd)),
            format!("assembly {}", usd(candidate.cost.assembly_usd)),
            format!("joints {}", usd(candidate.cost.joints_usd)),
            format!("capex total {}", usd(candidate.cost.total_usd)),
        ] {
            ui.label(text);
        }
        if let Some(opex) = candidate.cost.opex_usd {
            ui.label(format!("opex (declared loads, lifetime) {}", usd(opex)));
        }
        if let Some(lifecycle) = candidate.cost.lifecycle_usd {
            ui.label(RichText::new(format!("lifecycle {}", usd(lifecycle))).strong());
        }
        ui.separator();
        ui.small("Screening verdicts only — structural, thermal and quench evidence are separate.");
        if let Some(index) = profile_for {
            self.run_profile(index);
        }
    }
}

/// Pre-run summary card for a coupled-search case: requirement, fixed
/// geometry, choice sets, baseline and cost basis.
fn search_case_summary(ui: &mut egui::Ui, case: &CoupledSearchCase) {
    egui::Frame::group(ui.style())
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0, brand::LINE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Magnet requirement");
            ui.label(format!(
                "{:.2} T at bore probe ({:.3}, {:.3}, {:.3}) m",
                case.requirement.b_target_t,
                case.requirement.bore_probe_m[0],
                case.requirement.bore_probe_m[1],
                case.requirement.bore_probe_m[2]
            ));
            if let Some(region) = &case.requirement.good_field_region {
                ui.label(format!(
                    "usable volume ±{:.3} × ±{:.3} × ±{:.3} m · {}³ lattice",
                    region.half_extents_m[0],
                    region.half_extents_m[1],
                    region.half_extents_m[2],
                    region.points_per_axis
                ));
            }
            ui.add_space(8.0);
            ui.strong("Fixed geometry");
            ui.label(match &case.fixed_geometry.path {
                Some(path) => format!(
                    "planar path · {} segments · L {:.3} m · radial pitch {:.4} m · tape {:.3} m ({:?})",
                    path.segments.len(),
                    path.length_m(),
                    case.fixed_geometry.radial_pitch_m,
                    case.fixed_geometry.tape_width_m,
                    case.fixed_geometry.tape_normal
                ),
                None => format!(
                    "straight {} · bend radius {} · radial pitch {:.4} m · tape {:.3} m ({:?})",
                    case.fixed_geometry
                        .straight_half_length_m
                        .map(|l| format!("±{l:.3} m"))
                        .unwrap_or_else(|| "searched".into()),
                    case.fixed_geometry
                        .bend_radius_m
                        .map(|r| format!("{r:.3} m"))
                        .unwrap_or_else(|| "searched".into()),
                    case.fixed_geometry.radial_pitch_m,
                    case.fixed_geometry.tape_width_m,
                    case.fixed_geometry.tape_normal
                ),
            });
            ui.add_space(8.0);
            ui.strong("Search space");
            ui.label(format!("turns: {:?}", case.choices.turns_along_normal));
            ui.label(format!("tapes across: {:?}", case.choices.tapes_along_width));
            if let Some(strands) = &case.choices.strands_parallel {
                ui.label(format!("parallel strands: {strands:?}"));
            }
            if let Some(bends) = &case.choices.bend_radius_m {
                ui.label(format!("bend radii (m): {bends:?}"));
            }
            if let Some(straights) = &case.choices.straight_half_length_m {
                ui.label(format!("straight halves (m): {straights:?}"));
            }
            if let Some(fm) = &case.field_map {
                ui.label(format!(
                    "field map: {:?} · source sha {}",
                    fm.map.components(),
                    fm.map.source_sha256(),
                ));
                field_map_preview(ui, &fm.map);
            }
            ui.label(format!(
                "baseline: {} × {}{}",
                case.baseline.turns_along_normal,
                case.baseline.tapes_along_width,
                strands_suffix(case.baseline_strands())
            ));
            ui.add_space(8.0);
            ui.strong("Cost basis (declared)");
            ui.label(format!(
                "{} / m conductor · {:.0}% scrap · {} / pancake assembly · {} / joint",
                usd(case.cost.price_usd_per_m),
                case.cost.scrap_fraction * 100.0,
                usd(case.cost.assembly_cost_per_pancake_usd),
                usd(case.cost.joint_cost_usd)
            ));
            if let Some(policy) = &case.cost.piece_policy {
                let boundary = serde_json::to_string(&policy.boundary)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                let unit = serde_json::to_string(&policy.piece_unit)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string();
                let offerings = case
                    .cost
                    .piece_offerings
                    .as_ref()
                    .map(|o| {
                        o.iter()
                            .map(|p| format!("{:.0} m @ {}", p.length_m, usd(p.price_usd_per_m)))
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })
                    .or_else(|| {
                        policy
                            .piece_length_m
                            .map(|l| format!("{:.0} m pieces", l))
                    })
                    .unwrap_or_default();
                ui.colored_label(
                    brand::MUTED,
                    format!(
                        "pieces: {boundary} · {unit} · {offerings} · {} / splice",
                        usd(policy.splice_cost_usd)
                    ),
                );
            }
            if let Some(opex) = &case.opex {
                ui.add_space(8.0);
                ui.strong("Opex (declared)");
                ui.label(format!(
                    "{:.0} W cold load · COP {:.0}% Carnot · {:.0} K sink · ${:.3}/kWh · {:.0} h/yr × {:.1} yr",
                    opex.heat_loads_w.iter().map(|t| t.power_w).sum::<f64>(),
                    opex.cop_fraction_of_carnot * 100.0,
                    opex.sink_temperature_k,
                    opex.electricity_usd_per_kwh,
                    opex.operating_hours_per_year,
                    opex.operating_years,
                ));
            }
            ui.add_space(6.0);
            ui.colored_label(brand::MUTED, format!("Schema {} · {}", case.schema, case.provenance));
        });
}

/// A check row with the verdict-color left stripe — the evidence-ledger
/// pattern shared by both project kinds.
fn check_card(ui: &mut egui::Ui, check: &Check) {
    let frame = egui::Frame::group(ui.style())
        .fill(Color32::WHITE)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                status_chip(ui, check.status);
                ui.strong(&check.id);
            });
            ui.label(&check.detail);
        });
    let rect = frame.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top() + 7.0),
            egui::pos2(rect.left() + 3.0, rect.bottom() - 7.0),
        ),
        1.5,
        status_color(check.status),
    );
}

/// `" × 2 strands"` for multi-strand candidates, `""` for single-strand.
fn strands_suffix(strands: u32) -> String {
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
fn lifecycle_at_price(ledger: &SearchCostLedger, price_usd_per_m: f64, scrap_fraction: f64) -> f64 {
    repriced_total_usd(ledger, price_usd_per_m, scrap_fraction) + ledger.opex_usd.unwrap_or(0.0)
}

/// Cheapest PASS candidate at `price` — the record's own `best_index` when
/// the price equals the declared case price.
fn best_index_at_price(
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
fn usd_k(value: f64) -> String {
    if value.abs() >= 100_000.0 {
        format!("${:.0}k", value / 1000.0)
    } else if value.abs() >= 1_000.0 {
        format!("${:.1}k", value / 1000.0)
    } else {
        format!("${:.0}", value)
    }
}

/// Headroom bar color: blue with margin to spare, amber near the limit,
/// red at/over it.
fn headroom_color(usage: f64) -> Color32 {
    if usage < 0.7 {
        brand::BLUE
    } else if usage < 1.0 {
        status_color(Status::Inconclusive)
    } else {
        status_color(Status::Fail)
    }
}

/// "Where it binds" — the candidate's limiting point drawn twice: on the
/// winding path (exact station positions from the case's sampling plan) and
/// in the pack cross-section (turn index x tape width index).
fn limiting_anatomy(
    ui: &mut egui::Ui,
    case: &CoupledSearchCase,
    candidate: &optcoil_search::coupled_search::SearchCandidateResult,
) {
    let Some(screening) = &candidate.screening else {
        return;
    };
    let Some(limiting) = &screening.limiting else {
        return;
    };
    let geom = &candidate.geometry;
    // Plan view: centerline outline, station dots, limiting station in red.
    // Racetrack cases draw the legacy analytic outline; v9 path cases
    // sample poses along the declared CoilPath (identical outline on a
    // racetrack path — `CoilPath::racetrack` reproduces it exactly).
    let mut outline: Vec<[f64; 2]> = Vec::new();
    let mut station_xy: Vec<(&Station, [f64; 2])> = Vec::new();
    match &case.fixed_geometry.path {
        Some(path) => {
            let n = 64usize;
            let total = path.length_m();
            for i in 0..=n {
                let Ok(pose) = path.pose_at(total * i as f64 / n as f64) else {
                    return;
                };
                outline.push([pose.position_m[0], pose.position_m[1]]);
            }
            for st in &case.sampling.stations {
                if let Station::Path { s_m, .. } = st
                    && let Ok(pose) = path.pose_at(*s_m)
                {
                    station_xy.push((st, [pose.position_m[0], pose.position_m[1]]));
                }
            }
        }
        None => {
            let (Some(l), Some(r)) = (
                case.fixed_geometry.straight_half_length_m,
                case.fixed_geometry.bend_radius_m,
            ) else {
                return;
            };
            if !(l.is_finite() && r.is_finite() && r > 0.0) {
                return;
            }
            let n = 24usize;
            let nf = n as f64;
            for i in 0..=n {
                outline.push([-l + 2.0 * l * i as f64 / nf, r]);
            }
            for i in 1..=n {
                let a = std::f64::consts::FRAC_PI_2 - std::f64::consts::PI * i as f64 / nf;
                outline.push([l + r * a.cos(), r * a.sin()]);
            }
            for i in 1..=n {
                outline.push([l - 2.0 * l * i as f64 / nf, -r]);
            }
            for i in 1..n {
                let a = -std::f64::consts::FRAC_PI_2 - std::f64::consts::PI * i as f64 / nf;
                outline.push([-l + r * a.cos(), r * a.sin()]);
            }
            for st in &case.sampling.stations {
                let xy = match st {
                    Station::Straight { x_m, .. } => [*x_m, r],
                    Station::Arc { azimuth_deg, .. } => {
                        let a = azimuth_deg.to_radians();
                        [l + r * a.cos(), r * a.sin()]
                    }
                    Station::Path { .. } => continue,
                };
                station_xy.push((st, xy));
            }
        }
    }
    let (mut x0, mut x1, mut y0, mut y1) = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for p in &outline {
        x0 = x0.min(p[0]);
        x1 = x1.max(p[0]);
        y0 = y0.min(p[1]);
        y1 = y1.max(p[1]);
    }
    let span_x = (x1 - x0).max(1e-9);
    let span_y = (y1 - y0).max(1e-9);

    ui.add_space(10.0);
    ui.strong("Where it binds");
    ui.small(format!(
        "{} · turn {} of {} · tape {} of {}",
        limiting.station,
        limiting.turn_index,
        geom.turns_along_normal,
        limiting.width_index,
        geom.tapes_along_width
    ));

    let width = ui.available_width().min(260.0);
    let height = (width * (span_y / span_x) as f32 + 18.0).clamp(48.0, 130.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let scale = ((rect.width() as f64 * 0.86) / span_x).min((rect.height() as f64 * 0.86) / span_y);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let to_screen = |x: f64, y: f64| {
        egui::pos2(
            rect.center().x + ((x - cx) * scale) as f32,
            rect.center().y - ((y - cy) * scale) as f32,
        )
    };
    painter.add(egui::Shape::line(
        outline.iter().map(|p| to_screen(p[0], p[1])).collect(),
        egui::Stroke::new(1.5, brand::MUTED),
    ));
    let fail = status_color(Status::Fail);
    for (st, xy) in &station_xy {
        let pos = to_screen(xy[0], xy[1]);
        let is_limiting = st.id() == limiting.station;
        painter.circle_filled(
            pos,
            if is_limiting { 4.0 } else { 2.5 },
            if is_limiting { fail } else { brand::MUTED },
        );
        if is_limiting {
            painter.circle_stroke(pos, 8.0, egui::Stroke::new(1.5, fail));
        }
    }
    ui.small("winding path — stations per the case's sampling plan");

    // Pack cross-section: x = turn index (normal direction), y = width index.
    let turns = geom.turns_along_normal.max(1);
    let tapes = geom.tapes_along_width.max(1);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 52.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect.shrink(2.0), 2.0, brand::status_tint(brand::BLUE));
    let cw = rect.width() / turns as f32;
    let ch = rect.height() / tapes as f32;
    let cell = egui::Rect::from_min_size(
        rect.min
            + egui::vec2(
                (limiting.turn_index.min(turns - 1)) as f32 * cw,
                (limiting.width_index.min(tapes - 1)) as f32 * ch,
            ),
        egui::vec2(cw.max(4.0), ch.max(4.0)),
    );
    painter.rect_filled(cell.shrink(0.5), 1.0, fail);
    painter.rect_stroke(
        cell.expand(2.0),
        1.0,
        egui::Stroke::new(1.5, fail),
        egui::StrokeKind::Inside,
    );
    ui.small(format!(
        "pack section — turn → {} (inner→outer), tape ↕ axial · {:.1} × {:.1} mm",
        match case.fixed_geometry.tape_normal {
            optcoil_model::coupled::TapeNormal::Radial => "radial",
            optcoil_model::coupled::TapeNormal::Axial => "axial",
        },
        geom.radial_width_m * 1000.0,
        geom.axial_height_m * 1000.0
    ));
}

/// Thousands-grouped USD, e.g. `$377,426.18`.
fn usd(value: f64) -> String {
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

fn cost_values(cost: &CostBreakdown) -> [f64; 4] {
    [
        cost.installed_conductor_usd,
        cost.scrap_usd,
        cost.assembly_usd,
        cost.joints_usd,
    ]
}

fn cost_bars(
    name: &str,
    cost: &CostBreakdown,
    offset: f64,
    color: Color32,
    scale: f32,
) -> BarChart {
    BarChart::new(
        name,
        cost_values(cost)
            .into_iter()
            .enumerate()
            .map(|(i, value)| {
                Bar::new(i as f64 + offset, value * scale as f64)
                    .width(0.32)
                    .name(COST_LABELS[i])
            })
            .collect(),
    )
    .color(color)
    .element_formatter(Box::new(|bar, _| {
        format!("{}: ${:.2}", bar.name, bar.value)
    }))
}

fn title(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).size(27.0));
    ui.colored_label(brand::MUTED, description);
    ui.add_space(15.0);
}

fn metric(ui: &mut egui::Ui, label: &str, value: String, caption: &str, reveal: f32, accent: bool) {
    let frame = egui::Frame::group(ui.style())
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0, brand::LINE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(label).size(11.0).color(brand::MUTED));
            ui.label(
                RichText::new(value)
                    .size(25.0)
                    .color(brand::BLUE.gamma_multiply(0.35 + 0.65 * reveal)),
            );
            ui.small(caption);
        });
    if accent {
        // The saving figure is the product's whole point — a thin brand
        // strip across the card's top edge, quiet but unmistakable.
        let rect = frame.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.left() + 8.0, rect.top()),
                egui::pos2(rect.right() - 8.0, rect.top() + 3.0),
            ),
            1.5,
            brand::BLUE.gamma_multiply(reveal.max(0.25)),
        );
    }
}

/// A tinted status pill — the four-verdict vocabulary rendered as a label,
/// matching the chips on the public page.
/// Search-ledger cost components at the shown conductor price —
/// conductor and scrap scale with price; assembly, joints and declared
/// opex are price-independent and pass through.
fn search_cost_components(
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

fn search_cost_bars(
    name: &str,
    components: &[(&'static str, f64)],
    offset: f64,
    color: Color32,
    scale: f32,
) -> BarChart {
    BarChart::new(
        name,
        components
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                Bar::new(i as f64 + offset, *value * scale as f64)
                    .width(0.32)
                    .name(*label)
            })
            .collect(),
    )
    .color(color)
    .element_formatter(Box::new(|bar, _| {
        format!("{}: ${:.2}", bar.name, bar.value)
    }))
}

const SEARCH_COST_LABELS: [&str; 5] = ["Conductor", "Scrap", "Assembly", "Joints", "Opex"];

/// Racetrack plan view (xy, z=0): two straights of half-length `straight`
/// joined by 180° arcs of radius `bend`. The declared good-field region
/// projects onto the plane as a dashed rect; the bore probe is a dot.
fn racetrack_plan(
    ui: &mut egui::Ui,
    bend: f64,
    straight: f64,
    probe: Option<[f64; 3]>,
    region_half_extents: Option<[f64; 3]>,
) {
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(280.0, 160.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, brand::BACKGROUND);
    let span_x = (straight + bend) as f32;
    let span_y = bend as f32;
    let scale = ((rect.width() - 24.0) / (2.0 * span_x).max(1e-6))
        .min((rect.height() - 24.0) / (2.0 * span_y).max(1e-6));
    let cx = rect.center().x;
    let cy = rect.center().y;
    let to_px = |x: f64, y: f64| {
        egui::pos2(
            cx + (x * scale as f64) as f32,
            cy - (y * scale as f64) as f32,
        )
    };
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(64);
    let l = straight;
    let r = bend;
    // top straight → right arc → bottom straight → left arc
    pts.push(to_px(-l, r));
    pts.push(to_px(l, r));
    for i in 0..=16 {
        let t = std::f64::consts::FRAC_PI_2 - (i as f64 / 16.0) * std::f64::consts::PI;
        pts.push(to_px(l + r * t.cos(), r * t.sin()));
    }
    pts.push(to_px(-l, -r));
    for i in 0..=16 {
        let t = -std::f64::consts::FRAC_PI_2 - (i as f64 / 16.0) * std::f64::consts::PI;
        pts.push(to_px(-l + r * t.cos(), r * t.sin()));
    }
    painter.add(egui::Shape::closed_line(
        pts,
        egui::Stroke::new(2.0, brand::BLUE),
    ));
    if let Some(he) = region_half_extents {
        let p0 = probe.unwrap_or([0.0, 0.0, 0.0]);
        let a = to_px(p0[0] - he[0], p0[1] - he[1]);
        let b = to_px(p0[0] + he[0], p0[1] + he[1]);
        painter.rect_stroke(
            egui::Rect::from_two_pos(a, b),
            0.0,
            egui::Stroke::new(1.0, brand::MUTED),
            egui::StrokeKind::Outside,
        );
        painter.rect_filled(
            egui::Rect::from_two_pos(a, b),
            0.0,
            brand::MUTED.gamma_multiply(0.08),
        );
    }
    if let Some(p) = probe {
        painter.circle_filled(to_px(p[0], p[1]), 3.0, Color32::BLACK);
    }
    ui.small(format!(
        "plan view — ±L {:.3} m straights · R {:.3} m bends · dashed box = good-field region (midplane)",
        straight, bend
    ));
}

/// Field-map preview — the customer's declared map drawn as a |B|
/// heatmap. Cartesian maps render the midplane (z nearest 0) x-y slice;
/// cylindrical maps render the rho-z plane. Read-only visualization of
/// the declared artifact — no re-interpolation.
#[allow(clippy::type_complexity)]
fn field_map_preview(ui: &mut egui::Ui, map: &optcoil_model::coupled::FieldMap) {
    use optcoil_model::coupled::FieldMap;
    // (u,v axes labels, cells: (u_idx, v_idx, |B|), u levels, v levels)
    let (ulabel, vlabel, cells, ulevels, vlevels): (
        &str,
        &str,
        Vec<(u32, u32, f64)>,
        Vec<f64>,
        Vec<f64>,
    ) = match map {
        FieldMap::CartesianBxByBz {
            x_levels_m,
            y_levels_m,
            z_levels_m,
            entries,
            ..
        } => {
            // midplane slice — the z level nearest zero
            let z_idx = z_levels_m
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
                .map(|(i, _)| i as u32)
                .unwrap_or(0);
            let cells = entries
                .iter()
                .filter(|e| e.z_index == z_idx)
                .map(|e| {
                    (
                        e.x_index,
                        e.y_index,
                        (e.bx_t * e.bx_t + e.by_t * e.by_t + e.bz_t * e.bz_t).sqrt(),
                    )
                })
                .collect();
            (
                "x / m",
                "y / m",
                cells,
                x_levels_m.clone(),
                y_levels_m.clone(),
            )
        }
        FieldMap::CylindricalBrBz {
            rho_levels_m,
            z_levels_m,
            entries,
            ..
        } => {
            let cells = entries
                .iter()
                .map(|e| {
                    (
                        e.rho_index,
                        e.z_index,
                        (e.br_t * e.br_t + e.bz_t * e.bz_t).sqrt(),
                    )
                })
                .collect();
            (
                "ρ / m",
                "z / m",
                cells,
                rho_levels_m.clone(),
                z_levels_m.clone(),
            )
        }
    };
    if cells.is_empty() || ulevels.len() < 2 || vlevels.len() < 2 {
        return;
    }
    let bmax = cells.iter().map(|c| c.2).fold(0.0f64, f64::max).max(1e-9);
    let (u0, u1) = (
        ulevels.iter().copied().fold(f64::INFINITY, f64::min),
        ulevels.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    );
    let (v0, v1) = (
        vlevels.iter().copied().fold(f64::INFINITY, f64::min),
        vlevels.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    );
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(360.0), 170.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::BLACK);
    let uspan = (u1 - u0).max(1e-9);
    let vspan = (v1 - v0).max(1e-9);
    let ustep = uspan / (ulevels.len() - 1).max(1) as f64;
    let vstep = vspan / (vlevels.len() - 1).max(1) as f64;
    for (ui_idx, vi_idx, b) in &cells {
        let (u, v) = (ulevels[*ui_idx as usize], vlevels[*vi_idx as usize]);
        let x0 = rect.min.x + ((u - u0) / uspan) as f32 * rect.width();
        let x1 = rect.min.x + ((u - u0 + ustep) / uspan) as f32 * rect.width();
        let y1 = rect.max.y - ((v - v0) / vspan) as f32 * rect.height();
        let y0 = rect.max.y - ((v - v0 + vstep) / vspan) as f32 * rect.height();
        let t = (b / bmax) as f32;
        // dark → blue → white ramp
        let color = if t < 0.5 {
            brand::BLUE.gamma_multiply(t * 2.0)
        } else {
            Color32::from_rgb(
                (24.0 + (t - 0.5) * 2.0 * 231.0) as u8,
                ((t - 0.5) * 2.0 * 255.0) as u8,
                255,
            )
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, y0),
                egui::pos2(x1.max(x0 + 1.0), y1.max(y0 + 1.0)),
            ),
            0.0,
            color,
        );
    }
    ui.small(format!(
        "declared field map — |B| on the {ulabel}–{vlabel} plane · peak {bmax:.2} T",
    ));
}

pub(super) fn status_chip(ui: &mut egui::Ui, status: Status) -> egui::Response {
    let color = status_color(status);
    egui::Frame::group(ui.style())
        .fill(brand::status_tint(color))
        .stroke(egui::Stroke::new(
            1.0,
            brand::mix(color, Color32::WHITE, 0.35),
        ))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(status_label(status))
                    .size(10.0)
                    .color(color)
                    .strong(),
            );
        })
        .response
}
