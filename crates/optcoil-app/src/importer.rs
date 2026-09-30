//! The measurement intake form. Parsing and validation are dispatched by the
//! workbench to a native thread or Web Worker, rather than performed here.

use eframe::egui::{self, RichText};
use optcoil_model::{
    material::{
        AngleConvention, CellSpanLimits, MaterialBundle, MaterialDataClass, MaterialFieldBasis,
        MaterialMetadata, MaterialSelection,
    },
    tabular_import::{
        AngleUnit, ColumnMapping, FieldUnit, IcUnit, ImportPreview, NValueSource,
        NominalCoordinates, TabularFormat, TemperatureUnit,
    },
};
use serde::{Deserialize, Serialize};

pub(crate) enum ImportAction {
    PickTable,
    PickMetadata,
    PreviewSheet,
    Validate,
    Apply,
    Export,
    Close,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportDraft {
    pub name: String,
    #[serde(skip)]
    pub bytes: Vec<u8>,
    pub format: TabularFormat,
    pub sheet: Option<String>,
    pub preview: Option<ImportPreview>,
    #[serde(skip)]
    pub validated: Option<MaterialBundle>,
    pub error: Option<String>,
    step: usize,
    columns: [Option<usize>; 4],
    nominal_columns: [Option<usize>; 3],
    reuse_measured: bool,
    n_column: Option<usize>,
    n_constant: String,
    temperature_unit: TemperatureUnit,
    field_unit: FieldUnit,
    angle_unit: AngleUnit,
    ic_unit: IcUnit,
    id: String,
    material: String,
    sample_id: String,
    source_reference: String,
    source_url: String,
    authors: String,
    license: String,
    license_url: String,
    dates: String,
    upstream_xlsx_sha256: String,
    data_class: usize,
    bridge_width_mm: f64,
    tape_width_mm: f64,
    tap_spacing_mm: f64,
    criterion_v_per_m: f64,
    uncertainty: String,
    strain_state: String,
    cell_spans: CellSpanLimits,
    conventions_confirmed: bool,
}

impl Default for ImportDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            bytes: Vec::new(),
            format: TabularFormat::Csv,
            sheet: None,
            preview: None,
            validated: None,
            error: None,
            step: 0,
            columns: [None; 4],
            nominal_columns: [None; 3],
            reuse_measured: false,
            n_column: None,
            n_constant: String::new(),
            temperature_unit: TemperatureUnit::Kelvin,
            field_unit: FieldUnit::Tesla,
            angle_unit: AngleUnit::Degrees,
            ic_unit: IcUnit::Ampere,
            id: String::new(),
            material: String::new(),
            sample_id: String::new(),
            source_reference: String::new(),
            source_url: String::new(),
            authors: String::new(),
            license: String::new(),
            license_url: String::new(),
            dates: String::new(),
            upstream_xlsx_sha256: String::new(),
            data_class: 0,
            bridge_width_mm: 1.0,
            tape_width_mm: 12.0,
            tap_spacing_mm: 5.0,
            criterion_v_per_m: 0.0001,
            uncertainty: String::new(),
            strain_state: "not quantified".into(),
            cell_spans: CellSpanLimits {
                temperature_k: 10.0,
                field_ratio: 2.5,
                angle_deg: 10.0,
            },
            conventions_confirmed: false,
        }
    }
}

impl ImportDraft {
    pub fn recovery_json(&self) -> Result<String, String> {
        use base64::Engine;
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        value["file_base64"] = base64::engine::general_purpose::STANDARD
            .encode(&self.bytes)
            .into();
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }

    pub fn from_recovery_json(json: &str) -> Result<Self, String> {
        use base64::Engine;
        if json.len() > crate::recovery::MAX_IMPORTER_BYTES {
            return Err("Saved importer exceeds the recovery size limit.".into());
        }
        let mut value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let encoded = value
            .as_object_mut()
            .and_then(|v| v.remove("file_base64"))
            .ok_or("Saved importer has no source bytes.")?;
        let encoded = encoded
            .as_str()
            .ok_or("Saved importer source bytes are invalid.")?;
        let limit = optcoil_model::tabular_import::MAX_TABULAR_INPUT_BYTES;
        if encoded.len() > limit.div_ceil(3) * 4 {
            return Err("Saved measurement file exceeds the import size limit.".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| e.to_string())?;
        if bytes.len() > limit {
            return Err("Saved measurement file exceeds the import size limit.".into());
        }
        let mut draft: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if draft.step > 2 || draft.data_class > 3 {
            return Err("Saved importer has an invalid editor page or data class.".into());
        }
        if let Some(preview) = &draft.preview {
            use sha2::Digest;
            if preview.source_sha256 != format!("{:x}", sha2::Sha256::digest(&bytes)) {
                return Err("Saved import preview does not match its source file.".into());
            }
        }
        draft.bytes = bytes;
        // Validation is repeated from the retained source before applying it.
        draft.validated = None;
        Ok(draft)
    }

    pub fn state_signature(&self) -> Result<String, String> {
        // File bytes are immutable while mapping and declarations are edited.
        // They are serialized only when a changed draft is committed.
        serde_json::to_string(self).map_err(|e| e.to_string())
    }

    pub fn set_file(&mut self, name: String, bytes: Vec<u8>) -> Result<(), String> {
        self.format = match name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "csv" => TabularFormat::Csv,
            "tsv" => TabularFormat::Tsv,
            "xlsx" => TabularFormat::Xlsx,
            _ => return Err("Choose a UTF-8 .csv, .tsv, or .xlsx measurement table.".into()),
        };
        if self.id.is_empty() {
            self.id = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(stem, _)| stem)
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .take(96)
                .collect();
        }
        self.name = name;
        self.bytes = bytes;
        self.sheet = None;
        self.preview = None;
        self.validated = None;
        self.error = None;
        Ok(())
    }

    pub fn set_preview(&mut self, preview: ImportPreview) {
        self.sheet = preview.selected_sheet.clone();
        let locate = |aliases: &[&str]| {
            preview
                .headers
                .iter()
                .position(|h| aliases.iter().any(|a| h.trim().eq_ignore_ascii_case(a)))
        };
        self.columns = [
            locate(&["temperature_k", "temperature", "t_k", "t"]),
            locate(&["applied_field_t", "field_t", "field", "b"]),
            locate(&[
                "angle_from_normal_deg",
                "angle_deg",
                "theta_deg",
                "angle",
                "theta",
            ]),
            locate(&["ic_a_per_m", "bridge_ic_a", "ic", "critical_current"]),
        ];
        self.nominal_columns = [
            locate(&["nominal_temperature_k"]),
            locate(&["nominal_field_t"]),
            locate(&["nominal_angle_deg"]),
        ];
        self.n_column = locate(&["n_value", "n-value", "n"]);
        if self.columns[3].is_some_and(|i| preview.headers[i].eq_ignore_ascii_case("ic_a_per_m")) {
            self.ic_unit = IcUnit::AmperePerMeter;
        }
        self.reuse_measured = false;
        self.preview = Some(preview);
        self.validated = None;
        self.error = None;
    }

    /// Optional existing metadata can fill the form; it never confirms the
    /// current file's mapping or author declarations on the user's behalf.
    pub fn load_metadata(&mut self, json: &str) -> Result<(), String> {
        let m: MaterialMetadata =
            serde_json::from_str(json).map_err(|e| format!("Source metadata: {e}"))?;
        self.id = format!("{}-imported", m.id);
        self.material = m.material;
        self.sample_id = m.sample_id;
        self.source_reference = m.source_doi;
        self.source_url = m.source_url;
        self.authors = m.authors.join("; ");
        self.license = m.license;
        self.license_url = m.license_url;
        self.dates = m.measurement_dates;
        self.upstream_xlsx_sha256 = m.source_xlsx_sha256;
        self.bridge_width_mm = m.measured_bridge_width_m * 1000.0;
        self.tape_width_mm = m.original_tape_width_m * 1000.0;
        self.tap_spacing_mm = m.voltage_tap_spacing_m * 1000.0;
        self.criterion_v_per_m = m.electric_field_criterion_v_per_m;
        self.uncertainty = m
            .measurement_uncertainty_fraction
            .map_or(String::new(), |v| v.to_string());
        self.strain_state = m.strain_state;
        self.cell_spans = m.max_cell_spans;
        self.data_class = match m.data_class {
            MaterialDataClass::Measured => 0,
            MaterialDataClass::MeasuredWithModelExtension => 1,
            MaterialDataClass::PublishedModelFit => 2,
            MaterialDataClass::SyntheticSensitivity => 3,
        };
        self.conventions_confirmed = false;
        self.validated = None;
        self.error = None;
        Ok(())
    }

    pub fn declarations(&self) -> Result<(ColumnMapping, MaterialMetadata), String> {
        let column = |index: usize, name: &str| {
            self.columns[index].ok_or_else(|| format!("Choose the {name} column."))
        };
        if !self.conventions_confirmed {
            return Err("Confirm the column units, measurement conventions and source declarations before validation.".into());
        }
        let nominal_coordinates = if self.reuse_measured {
            NominalCoordinates::ReuseMeasuredExplicitly
        } else {
            NominalCoordinates::RequireColumns {
                temperature: self.nominal_columns[0].ok_or(
                    "Choose nominal temperature or explicitly reuse measured coordinates.",
                )?,
                field: self.nominal_columns[1]
                    .ok_or("Choose nominal field or explicitly reuse measured coordinates.")?,
                angle: self.nominal_columns[2]
                    .ok_or("Choose nominal angle or explicitly reuse measured coordinates.")?,
            }
        };
        let n_value = match self.n_column {
            Some(column) => NValueSource::Column(column),
            None => NValueSource::Constant(self.n_constant.trim().parse().map_err(
                |_| "Choose an n-value column or explicitly supply a constant greater than 1.",
            )?),
        };
        let uncertainty = if self.uncertainty.trim().is_empty() {
            None
        } else {
            Some(self.uncertainty.trim().parse::<f64>().map_err(
                |_| "Measurement uncertainty must be a fraction, or blank when unknown.",
            )?)
        };
        let metadata = MaterialMetadata {
            schema: "optcoil-measured-material/v2".into(), id: self.id.trim().into(),
            data_class: match self.data_class { 1 => MaterialDataClass::MeasuredWithModelExtension, 2 => MaterialDataClass::PublishedModelFit, 3 => MaterialDataClass::SyntheticSensitivity, _ => MaterialDataClass::Measured },
            material: self.material.trim().into(), sample_id: self.sample_id.trim().into(),
            source_doi: self.source_reference.trim().into(), source_url: self.source_url.trim().into(),
            authors: self.authors.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            license: self.license.trim().into(), license_url: self.license_url.trim().into(),
            measurement_dates: self.dates.trim().into(), source_xlsx_sha256: self.upstream_xlsx_sha256.clone(),
            csv_sha256: String::new(), preparation_source_sha256: String::new(), source_description_sha256: String::new(),
            electric_field_criterion_v_per_m: self.criterion_v_per_m,
            voltage_tap_spacing_m: self.tap_spacing_mm / 1000.0,
            measured_bridge_width_m: self.bridge_width_mm / 1000.0,
            original_tape_width_m: self.tape_width_mm / 1000.0,
            field_basis: MaterialFieldBasis::AppliedFieldIncludingSampleSelfFieldResponse,
            angle_convention: AngleConvention::OrientedFromTapeNormalInMaximumLorentzGeometry,
            coordinate_policy: "Explicit tabular import mapping; measured and commanded coordinate policies are recorded in the import receipt.".into(),
            normalization: "Critical current converted using the selected units and declared measured bridge width; no full-width capacity is inferred.".into(),
            measurement_uncertainty_fraction: uncertainty, strain_state: self.strain_state.trim().into(),
            selection: MaterialSelection { nominal_temperature_k: Vec::new(), nominal_field_t: Vec::new(), nominal_angle_range_deg: [0.0, 0.0] },
            max_cell_spans: self.cell_spans, point_count: 0,
            limitations: vec!["User-imported source declarations are recorded as supplied; import validation checks structure and supported data semantics.".into()],
            tabular_import: None,
        };
        Ok((
            ColumnMapping {
                temperature: column(0, "temperature")?,
                field: column(1, "field")?,
                angle: column(2, "angle")?,
                ic: column(3, "critical current")?,
                temperature_unit: self.temperature_unit,
                field_unit: self.field_unit,
                angle_unit: self.angle_unit,
                ic_unit: self.ic_unit,
                n_value,
                nominal_coordinates,
            },
            metadata,
        ))
    }

    pub fn show(&mut self, ctx: &egui::Context, busy: bool) -> Option<ImportAction> {
        let mut open = true;
        let mut action = None;
        let mut changed = false;
        egui::Window::new("Import measurements")
            .open(&mut open).collapsible(false).default_width(820.0).default_height(620.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (i, label) in ["1. File & mapping", "2. Source & measurement", "3. Validate & use"].iter().enumerate() {
                        if ui.add_enabled(!busy, egui::Button::new(*label).selected(self.step == i)).clicked() { self.step = i; }
                    }
                });
                ui.separator();
                if busy { ui.horizontal(|ui| { ui.spinner(); ui.label("Reading or validating the selected table… You can cancel from the toolbar."); }); }
                egui::ScrollArea::vertical().max_height(550.0).show(ui, |ui| {
                    ui.add_enabled_ui(!busy, |ui| match self.step {
                        0 => {
                            ui.label("Choose your measurement file, then review every column and its units.");
                            if ui.button("Choose CSV / TSV / Excel…").clicked() { action = Some(ImportAction::PickTable); }
                            if let Some(preview) = &self.preview {
                                ui.label(format!("{} · {} rows · {} columns", self.name, preview.total_rows, preview.headers.len()));
                                ui.small(format!("Source SHA-256: {}", preview.source_sha256));
                                if !preview.sheets.is_empty() {
                                    let previous = self.sheet.clone();
                                    egui::ComboBox::from_id_salt("import-sheet").selected_text(self.sheet.as_deref().unwrap_or("Choose sheet")).show_ui(ui, |ui| {
                                        for sheet in &preview.sheets { ui.selectable_value(&mut self.sheet, Some(sheet.clone()), sheet); }
                                    });
                                    if previous != self.sheet { action = Some(ImportAction::PreviewSheet); }
                                }
                                ui.label("Source preview (unconverted)");
                                egui::ScrollArea::horizontal().id_salt("import-preview").show(ui, |ui| {
                                    egui::Grid::new("import-rows").striped(true).show(ui, |ui| {
                                        for header in &preview.headers { ui.strong(header); } ui.end_row();
                                        for row in &preview.rows { for cell in row { ui.label(cell); } ui.end_row(); }
                                    });
                                });
                                ui.separator();
                                egui::Grid::new("import-mapping").spacing([18.0, 8.0]).show(ui, |ui| {
                                    for (i, label) in ["Temperature", "Applied field", "Angle from tape normal", "Critical current"].iter().enumerate() {
                                        ui.label(*label); changed |= column_picker(ui, format!("import-col-{i}"), &preview.headers, &mut self.columns[i]);
                                        match i {
                                            0 => { changed |= units(ui, "temperature-unit", &mut self.temperature_unit, &[(TemperatureUnit::Kelvin, "K"), (TemperatureUnit::Celsius, "°C")]); }
                                            1 => { changed |= units(ui, "field-unit", &mut self.field_unit, &[(FieldUnit::Tesla, "T"), (FieldUnit::Millitesla, "mT"), (FieldUnit::Gauss, "G")]); }
                                            2 => { changed |= units(ui, "angle-unit", &mut self.angle_unit, &[(AngleUnit::Degrees, "degrees"), (AngleUnit::Radians, "radians")]); }
                                            _ => { changed |= units(ui, "ic-unit", &mut self.ic_unit, &[(IcUnit::Ampere, "A (bridge)"), (IcUnit::Kiloampere, "kA (bridge)"), (IcUnit::AmperePerMeter, "A/m"), (IcUnit::AmperePerCentimeter, "A/cm")]); }
                                        } ui.end_row();
                                    }
                                });
                                changed |= ui.checkbox(&mut self.reuse_measured, "Explicitly use measured coordinates as the nominal grid (no commanded coordinates)").changed();
                                if !self.reuse_measured {
                                    ui.small("Nominal columns use the same units selected above.");
                                    for (i, label) in ["Nominal temperature", "Nominal field", "Nominal angle"].iter().enumerate() {
                                        ui.horizontal(|ui| { ui.label(*label); changed |= column_picker(ui, format!("nominal-col-{i}"), &preview.headers, &mut self.nominal_columns[i]); });
                                    }
                                }
                                ui.horizontal(|ui| {
                                    ui.label("n-value"); changed |= column_picker(ui, "n-value-col", &preview.headers, &mut self.n_column);
                                    if self.n_column.is_none() { ui.label("Explicit constant"); changed |= ui.text_edit_singleline(&mut self.n_constant).changed(); }
                                });
                                if ui.button("Next: source & measurement").clicked() { self.step = 1; }
                            }
                        }
                        1 => {
                            ui.label("Identify the source and what was actually measured. Internal references and proprietary datasets are supported.");
                            if ui.button("Load source metadata…").clicked() { action = Some(ImportAction::PickMetadata); }
                            egui::Grid::new("import-source").spacing([18.0, 7.0]).show(ui, |ui| {
                                for (label, value) in [("Dataset id", &mut self.id), ("Material / product", &mut self.material), ("Sample id", &mut self.sample_id), ("DOI / source reference", &mut self.source_reference), ("Source URL / internal reference", &mut self.source_url), ("Authors (separate with ;)", &mut self.authors), ("License / usage terms", &mut self.license), ("License URL / internal terms", &mut self.license_url), ("Measurement dates", &mut self.dates)] {
                                    ui.label(label); changed |= ui.add(egui::TextEdit::singleline(value).desired_width(430.0)).changed(); ui.end_row();
                                }
                                ui.label("Data class"); changed |= units(ui, "import-data-class", &mut self.data_class, &[(0, "Measured"), (1, "Measured with model extension"), (2, "Published model fit"), (3, "Synthetic sensitivity")]); ui.end_row();
                                for (label, value) in [("Measured bridge width (mm)", &mut self.bridge_width_mm), ("Original tape width (mm)", &mut self.tape_width_mm), ("Voltage tap spacing (mm)", &mut self.tap_spacing_mm), ("Electric-field criterion (V/m)", &mut self.criterion_v_per_m), ("Maximum cell span: temperature (K)", &mut self.cell_spans.temperature_k), ("Maximum cell span: field ratio", &mut self.cell_spans.field_ratio), ("Maximum cell span: angle (degrees)", &mut self.cell_spans.angle_deg)] {
                                    ui.label(label); changed |= ui.add(egui::DragValue::new(value).speed(0.001).max_decimals(6)).changed(); ui.end_row();
                                }
                                ui.label("Uncertainty fraction (blank = unknown)"); changed |= ui.text_edit_singleline(&mut self.uncertainty).changed(); ui.end_row();
                                ui.label("Strain state"); changed |= ui.text_edit_singleline(&mut self.strain_state).changed(); ui.end_row();
                            });
                            ui.small("Supported convention: applied-field transport data, including sample self-field response; angles oriented from tape normal in maximum Lorentz geometry. Other conventions require a supported conversion before intake.");
                            ui.small("The measured bridge width normalizes bridge current to A/m. Original tape width does not imply measured full-width tape capacity.");
                            if ui.button("Next: validate & use").clicked() { self.step = 2; }
                        }
                        _ => {
                            ui.heading("Validate the mapped dataset");
                            ui.label("The canonical dataset must satisfy the existing material validation gates. Mapping does not extend the measured domain.");
                            changed |= ui.checkbox(&mut self.conventions_confirmed, "I confirm the selected units, coordinate policy, source declarations and measurement conventions.").changed();
                            if ui.add_enabled(self.preview.is_some() && self.conventions_confirmed, egui::Button::new("Validate mapped data")).clicked() { action = Some(ImportAction::Validate); }
                            if let Some(bundle) = &self.validated {
                                let m = &bundle.dataset.metadata;
                                ui.colored_label(crate::brand::BLUE, format!("Validated: {} · {} rows · {:?}", m.id, m.point_count, m.data_class));
                                ui.label(format!("Nominal domain: {:.3}–{:.3} K · {:.4}–{:.4} T · {:.1}–{:.1}°", m.selection.nominal_temperature_k.first().unwrap_or(&0.0), m.selection.nominal_temperature_k.last().unwrap_or(&0.0), m.selection.nominal_field_t.first().unwrap_or(&0.0), m.selection.nominal_field_t.last().unwrap_or(&0.0), m.selection.nominal_angle_range_deg[0], m.selection.nominal_angle_range_deg[1]));
                                ui.small("These extrema describe the table; sparse or missing cells can still leave operating queries unsupported.");
                                ui.horizontal(|ui| {
                                    if ui.button("Use in case builder").clicked() { action = Some(ImportAction::Apply); }
                                    if ui.button("Export dataset bundle").clicked() { action = Some(ImportAction::Export); }
                                });
                            }
                        }
                    });
                });
                if let Some(error) = &self.error { ui.colored_label(egui::Color32::from_rgb(166, 35, 41), error); }
                ui.separator(); ui.label(RichText::new("Source rows, selected units and transformation identity are retained with the canonical data.").small());
            });
        if changed {
            self.validated = None;
            if self.step != 2 {
                self.conventions_confirmed = false;
            }
        }
        if !open {
            action = Some(ImportAction::Close);
        }
        action
    }
}

fn column_picker(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    headers: &[String],
    selected: &mut Option<usize>,
) -> bool {
    let previous = *selected;
    egui::ComboBox::from_id_salt(id)
        .selected_text(
            selected
                .and_then(|i| headers.get(i))
                .map_or("Choose column / none", String::as_str),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "Choose column / none");
            for (index, header) in headers.iter().enumerate() {
                ui.selectable_value(selected, Some(index), format!("{}: {header}", index + 1));
            }
        });
    previous != *selected
}

fn units<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut T,
    options: &[(T, &str)],
) -> bool {
    let previous = *selected;
    let label = options
        .iter()
        .find(|(value, _)| value == selected)
        .map_or("Choose units", |(_, label)| *label);
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .show_ui(ui, |ui| {
            for (value, label) in options {
                ui.selectable_value(selected, *value, *label);
            }
        });
    previous != *selected
}

impl crate::Workbench {
    pub(crate) fn open_material_import(&mut self) {
        if self.worker.is_none() {
            self.material_import = Some(ImportDraft::default());
        }
    }

    pub(crate) fn apply_imported_material(
        &mut self,
        bundle: MaterialBundle,
        label: String,
    ) -> Result<(), String> {
        let draft = self
            .author
            .get_or_insert_with(crate::author::CaseDraft::guided);
        draft.apply_imported_material(bundle, label)
    }

    fn pick_import_file(&mut self, ctx: &egui::Context, metadata: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, crate::JobKind::Open, move |_| {
            use std::io::Read;
            let (title, extensions, limit): (&str, &[&str], usize) = if metadata {
                ("Load existing source metadata", &["json"], 1024 * 1024)
            } else {
                (
                    "Import measurement table",
                    &["csv", "tsv", "xlsx"],
                    optcoil_model::tabular_import::MAX_TABULAR_INPUT_BYTES,
                )
            };
            let Some(path) = rfd::FileDialog::new()
                .set_title(title)
                .add_filter("Measurement data", extensions)
                .pick_file()
            else {
                return Ok(crate::JobResult::Dismissed);
            };
            let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.take((limit + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > limit {
                return Err(format!("File exceeds the {limit}-byte import limit."));
            }
            if metadata {
                Ok(crate::JobResult::ImportMetadataLoaded(
                    String::from_utf8(bytes).map_err(|e| e.to_string())?,
                ))
            } else {
                Ok(crate::JobResult::TablePicked(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into(),
                    bytes,
                ))
            }
        });
        #[cfg(target_arch = "wasm32")]
        self.launch_wasm(ctx, crate::JobKind::Open, async move {
            let (extensions, limit): (&[&str], usize) = if metadata {
                (&["json"], 1024 * 1024)
            } else {
                (
                    &["csv", "tsv", "xlsx"],
                    optcoil_model::tabular_import::MAX_TABULAR_INPUT_BYTES,
                )
            };
            let Some((name, bytes)) = crate::web::pick_bytes_limited(extensions, limit).await?
            else {
                return Ok(crate::JobResult::Dismissed);
            };
            if metadata {
                Ok(crate::JobResult::ImportMetadataLoaded(
                    String::from_utf8(bytes).map_err(|e| e.to_string())?,
                ))
            } else {
                Ok(crate::JobResult::TablePicked(name, bytes))
            }
        });
    }

    pub(crate) fn preview_import_table(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.material_import.as_ref() else {
            return;
        };
        let bytes = draft.bytes.clone();
        let format = draft.format;
        let sheet = draft.sheet.clone();
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, crate::JobKind::Import, move |_| {
            optcoil_model::tabular_import::inspect_tabular(&bytes, format, sheet.as_deref(), 6)
                .map(|preview| crate::JobResult::TablePreview(Box::new(preview)))
                .map_err(|e| e.to_string())
        });
        #[cfg(target_arch = "wasm32")]
        self.launch_analysis_worker_with_bytes(
            ctx,
            crate::JobKind::Import,
            "Table preview",
            serde_json::json!({"kind":"table-preview", "format":format, "sheet":sheet}),
            Some(bytes),
        );
    }

    fn validate_import(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.material_import.as_mut() else {
            return;
        };
        let (mapping, metadata) = match draft.declarations() {
            Ok(value) => value,
            Err(error) => {
                draft.error = Some(error);
                return;
            }
        };
        draft.error = None;
        draft.validated = None;
        let bytes = draft.bytes.clone();
        let format = draft.format;
        let sheet = draft.sheet.clone();
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, crate::JobKind::Import, move |_| {
            optcoil_model::tabular_import::import_material(
                &bytes,
                format,
                sheet.as_deref(),
                mapping,
                metadata,
            )
            .map(|bundle| crate::JobResult::MaterialImported(Box::new(bundle)))
            .map_err(|e| e.to_string())
        });
        #[cfg(target_arch = "wasm32")]
        self.launch_analysis_worker_with_bytes(ctx, crate::JobKind::Import, "Material validation", serde_json::json!({"kind":"material-import", "format":format, "sheet":sheet, "mapping":mapping, "metadata":metadata}), Some(bytes));
    }

    pub(crate) fn import_window(&mut self, ctx: &egui::Context) {
        let busy = self.worker.is_some();
        let action = self
            .material_import
            .as_mut()
            .and_then(|draft| draft.show(ctx, busy));
        match action {
            Some(ImportAction::PickTable) => self.pick_import_file(ctx, false),
            Some(ImportAction::PickMetadata) => self.pick_import_file(ctx, true),
            Some(ImportAction::PreviewSheet) => self.preview_import_table(ctx),
            Some(ImportAction::Validate) => self.validate_import(ctx),
            Some(ImportAction::Apply) => {
                let payload = self.material_import.as_ref().and_then(|draft| {
                    draft.validated.as_ref().map(|bundle| {
                        (
                            MaterialBundle {
                                dataset: bundle.dataset.clone(),
                                csv_data: bundle.csv_data.clone(),
                                attestation: bundle.attestation.clone(),
                            },
                            draft.name.clone(),
                        )
                    })
                });
                if let Some((bundle, label)) = payload {
                    match self.apply_imported_material(bundle, label) {
                        Ok(()) => self.material_import = None,
                        Err(error) => {
                            if let Some(draft) = &mut self.material_import {
                                draft.error = Some(error);
                            }
                        }
                    }
                }
            }
            Some(ImportAction::Export) => {
                let payload = self
                    .material_import
                    .as_ref()
                    .and_then(|draft| draft.validated.as_ref())
                    .map(MaterialBundle::to_json);
                if let Some(Err(error)) = &payload
                    && let Some(draft) = &mut self.material_import
                {
                    draft.error = Some(error.to_string());
                }
                if let Some(Ok(json)) = payload {
                    #[cfg(target_arch = "wasm32")]
                    crate::web::download_bytes("material-bundle.json", json.as_bytes());
                    #[cfg(not(target_arch = "wasm32"))]
                    self.launch(ctx, crate::JobKind::Export, move |_| {
                        use std::io::Write;
                        let Some(path) = rfd::FileDialog::new()
                            .set_title("Save imported dataset bundle")
                            .set_file_name("material-bundle.json")
                            .save_file()
                        else {
                            return Ok(crate::JobResult::Dismissed);
                        };
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                            .map_err(|e| format!("Choose a new filename: {e}"))?;
                        file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
                        Ok(crate::JobResult::Exported(path))
                    });
                }
            }
            Some(ImportAction::Close) => {
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|w| w.kind == crate::JobKind::Import)
                {
                    self.cancel_search();
                }
                self.material_import = None;
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_keeps_file_mapping_and_unfinished_declarations() {
        let mut draft = ImportDraft::default();
        draft
            .set_file(
                "measurements.csv".into(),
                b"temperature,field,angle,ic\n21,1,0,2\n".to_vec(),
            )
            .unwrap();
        draft.set_preview(
            optcoil_model::tabular_import::inspect_tabular(&draft.bytes, draft.format, None, 6)
                .unwrap(),
        );
        draft.uncertainty = "unfinished value".into();
        draft.n_constant = "not yet supplied".into();
        draft.step = 2;
        let restored = ImportDraft::from_recovery_json(&draft.recovery_json().unwrap()).unwrap();
        assert_eq!(restored.bytes, draft.bytes);
        assert_eq!(restored.columns, draft.columns);
        assert_eq!(restored.uncertainty, "unfinished value");
        assert_eq!(restored.n_constant, "not yet supplied");
        assert_eq!(restored.step, 2);
        assert!(restored.validated.is_none());
        assert!(restored.declarations().is_err());
    }

    #[test]
    fn recovery_rejects_preview_bound_to_different_file() {
        let mut draft = ImportDraft::default();
        draft
            .set_file(
                "measurements.csv".into(),
                b"temperature,field\n21,1\n".to_vec(),
            )
            .unwrap();
        draft.set_preview(
            optcoil_model::tabular_import::inspect_tabular(&draft.bytes, draft.format, None, 6)
                .unwrap(),
        );
        let mut value: serde_json::Value =
            serde_json::from_str(&draft.recovery_json().unwrap()).unwrap();
        value["preview"]["source_sha256"] = "0".repeat(64).into();
        assert!(
            ImportDraft::from_recovery_json(&value.to_string())
                .err()
                .unwrap()
                .contains("does not match")
        );
    }
}
