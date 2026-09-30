//! Bounded, headless import of measured material tables from CSV, TSV and XLSX.
//!
//! Import is deliberately a transformation step, not a data authority: the
//! caller supplies attribution and physical declarations, while this module
//! requires explicit units and coordinate policies and runs the resulting
//! canonical CSV through `material_bundle_from_pair` before returning it.

use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
};

use calamine::{Data, Reader, Xlsx};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ModelError,
    dataset_intake::material_bundle_from_pair,
    material::{MaterialBundle, MaterialMetadata, TabularImportReceipt},
};

pub const MAX_TABULAR_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XLSX_EXPANDED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLSX_ENTRIES: usize = 256;
const MAX_ROWS: usize = 100_000;
const MAX_COLUMNS: usize = 256;
const MAX_PREVIEW_ROWS: usize = 500;
const MAX_WORKSHEET_CELLS: usize = 1_000_000;
const MAX_FIELD_BYTES: usize = 1024 * 1024;
const MAX_TABLE_CELLS: usize = 1_100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabularFormat {
    Csv,
    Tsv,
    Xlsx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreview {
    pub sheets: Vec<String>,
    pub selected_sheet: Option<String>,
    pub headers: Vec<String>,
    /// Data rows only; the header is not repeated.
    pub rows: Vec<Vec<String>>,
    pub source_rows: Vec<u32>,
    pub total_rows: usize,
    pub truncated: bool,
    pub source_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IcUnit {
    /// Bridge critical current in A. Converted to A/m using the declared
    /// `measured_bridge_width_m`.
    Ampere,
    Kiloampere,
    /// Critical current per width in A/m. Bridge current is derived using
    /// `measured_bridge_width_m`.
    AmperePerMeter,
    /// Critical current per width in A/cm.
    AmperePerCentimeter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureUnit {
    Kelvin,
    Celsius,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldUnit {
    Tesla,
    Millitesla,
    Gauss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AngleUnit {
    Degrees,
    Radians,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "value")]
pub enum NValueSource {
    Column(usize),
    /// Explicitly supplied constant fit exponent. This assumption is recorded
    /// in metadata limitations and is not represented as a measurement.
    Constant(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum NominalCoordinates {
    /// Map the nominal/commanded axes independently from the measured axes.
    RequireColumns {
        temperature: usize,
        field: usize,
        angle: usize,
    },
    /// Reuse measured coordinates as nominal axes. This must be selected
    /// explicitly when the source does not contain commanded coordinates.
    ReuseMeasuredExplicitly,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColumnMapping {
    /// Zero-based indices into the preview's header list.
    pub temperature: usize,
    pub field: usize,
    pub angle: usize,
    pub ic: usize,
    pub temperature_unit: TemperatureUnit,
    pub field_unit: FieldUnit,
    pub angle_unit: AngleUnit,
    pub ic_unit: IcUnit,
    pub n_value: NValueSource,
    pub nominal_coordinates: NominalCoordinates,
}

impl ColumnMapping {
    /// Validate all mapping fields without an input table. The serialized
    /// receipt uses this to reject impossible or malformed source columns.
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_mapping(self, MAX_COLUMNS)
    }
}

/// Read enough of a selected table to let a caller present a mapping UI.
/// XLSX worksheets are selected by exact name; an omitted name selects the
/// first visible worksheet. All row, column, byte and workbook expansion
/// limits are enforced before exposing table data.
pub fn inspect_tabular(
    bytes: &[u8],
    format: TabularFormat,
    sheet: Option<&str>,
    preview_rows: usize,
) -> Result<ImportPreview, ModelError> {
    let source_sha256 = check_input(bytes)?;
    let table = read_table(bytes, format, sheet)?;
    let preview_rows = preview_rows.min(MAX_PREVIEW_ROWS);
    let rows = table
        .rows
        .iter()
        .take(preview_rows)
        .map(|(_, row)| row.clone())
        .collect::<Vec<_>>();
    let source_rows = table
        .rows
        .iter()
        .take(preview_rows)
        .map(|(row, _)| *row)
        .collect();
    Ok(ImportPreview {
        sheets: table.sheets,
        selected_sheet: table.selected_sheet,
        headers: table.headers,
        total_rows: table.rows.len(),
        truncated: table.rows.len() > rows.len(),
        rows,
        source_rows,
        source_sha256,
    })
}

/// Convert mapped measurements to the canonical SI material CSV and validate
/// both metadata and rows through the same gate used for all material bundles.
/// Nominal coordinates are never inferred from measured coordinates.
pub fn import_material(
    bytes: &[u8],
    format: TabularFormat,
    sheet: Option<&str>,
    mapping: ColumnMapping,
    mut metadata: MaterialMetadata,
) -> Result<MaterialBundle, ModelError> {
    let source_sha256 = check_input(bytes)?;
    let table = read_table(bytes, format, sheet)?;
    validate_mapping(&mapping, table.headers.len())?;
    if table.rows.len() < 8 {
        return Err(invalid("material import requires at least 8 data rows"));
    }
    if table.rows.len() > MAX_ROWS {
        return Err(invalid("material import exceeds 100000 data rows"));
    }

    let mut csv = csv::WriterBuilder::new().from_writer(Vec::new());
    csv.write_record([
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
    ])
    .map_err(|e| invalid(&format!("cannot write canonical material header: {e}")))?;
    let mut temperatures = BTreeSet::new();
    let mut fields = BTreeSet::new();
    let mut angle_min = f64::INFINITY;
    let mut angle_max = f64::NEG_INFINITY;
    let mut identities = BTreeSet::new();
    let mut measured = BTreeSet::new();
    let mut nominal = BTreeSet::new();

    for (source_row, row) in &table.rows {
        let temperature = temperature_to_kelvin(
            value(row, mapping.temperature, "temperature")?,
            mapping.temperature_unit,
        );
        let field = field_to_tesla(value(row, mapping.field, "field")?, mapping.field_unit);
        let angle = angle_to_degrees(value(row, mapping.angle, "angle")?, mapping.angle_unit);
        let input_ic = value(row, mapping.ic, "critical current")?;
        let (bridge_ic_a, ic_a_per_m) = match mapping.ic_unit {
            IcUnit::Ampere => (input_ic, input_ic / metadata.measured_bridge_width_m),
            IcUnit::Kiloampere => (
                input_ic * 1000.0,
                input_ic * 1000.0 / metadata.measured_bridge_width_m,
            ),
            IcUnit::AmperePerMeter => (input_ic * metadata.measured_bridge_width_m, input_ic),
            IcUnit::AmperePerCentimeter => {
                let per_meter = input_ic * 100.0;
                (per_meter * metadata.measured_bridge_width_m, per_meter)
            }
        };
        let n_value = match mapping.n_value {
            NValueSource::Column(column) => value(row, column, "n-value")?,
            NValueSource::Constant(value) => value,
        };
        let (nominal_temperature, nominal_field, nominal_angle) = match mapping.nominal_coordinates
        {
            NominalCoordinates::RequireColumns {
                temperature: nominal_t,
                field: nominal_b,
                angle: nominal_a,
            } => (
                temperature_to_kelvin(
                    value(row, nominal_t, "nominal temperature")?,
                    mapping.temperature_unit,
                ),
                field_to_tesla(value(row, nominal_b, "nominal field")?, mapping.field_unit),
                angle_to_degrees(value(row, nominal_a, "nominal angle")?, mapping.angle_unit),
            ),
            NominalCoordinates::ReuseMeasuredExplicitly => (temperature, field, angle),
        };

        let point = [
            temperature,
            field,
            angle,
            ic_a_per_m,
            bridge_ic_a,
            n_value,
            nominal_temperature,
            nominal_field,
            nominal_angle,
        ];
        if point.iter().any(|x| !x.is_finite())
            || temperature <= 0.0
            || temperature > 400.0
            || field <= 0.0
            || field > 1000.0
            || !(-360.0..=720.0).contains(&angle)
            || ic_a_per_m <= 0.0
            || bridge_ic_a <= 0.0
            || n_value <= 1.0
            || nominal_temperature <= 0.0
            || nominal_temperature > 400.0
            || nominal_field <= 0.0
            || nominal_field > 1000.0
            || !(-360.0..=720.0).contains(&nominal_angle)
        {
            return Err(invalid(&format!(
                "invalid or out-of-range physical value in source row {source_row}"
            )));
        }
        let measured_key = [temperature, field, canonical_zero(angle)].map(f64::to_bits);
        let nominal_key = [
            nominal_temperature,
            nominal_field,
            canonical_zero(nominal_angle),
        ]
        .map(f64::to_bits);
        if !identities.insert(source_row)
            || !measured.insert(measured_key)
            || !nominal.insert(nominal_key)
        {
            return Err(invalid(&format!(
                "duplicate material identity or coordinates in source row {source_row}"
            )));
        }
        temperatures.insert(nominal_temperature.to_bits());
        fields.insert(nominal_field.to_bits());
        angle_min = angle_min.min(nominal_angle);
        angle_max = angle_max.max(nominal_angle);

        csv.serialize((
            source_row,
            nominal_temperature,
            nominal_field,
            nominal_angle,
            temperature,
            field,
            angle,
            ic_a_per_m,
            bridge_ic_a,
            n_value,
        ))
        .map_err(|e| invalid(&format!("cannot encode canonical material row: {e}")))?;
    }

    if temperatures.len() < 2 || fields.len() < 2 || angle_min >= angle_max {
        return Err(invalid(
            "imported nominal grid needs at least two temperatures, two fields and a nonzero angle span",
        ));
    }
    let csv_bytes = csv
        .into_inner()
        .map_err(|e| invalid(&format!("cannot finish canonical CSV: {e}")))?;
    metadata.point_count = table.rows.len();
    metadata.selection.nominal_temperature_k = sorted_floats(temperatures);
    metadata.selection.nominal_field_t = sorted_floats(fields);
    metadata.selection.nominal_angle_range_deg = [angle_min, angle_max];
    metadata.schema = crate::material::MATERIAL_SCHEMA_V2.into();
    metadata.csv_sha256 = format!("{:x}", Sha256::digest(&csv_bytes));
    let receipt = TabularImportReceipt {
        schema: "optcoil-tabular-import/v1".into(),
        source_sha256: source_sha256.clone(),
        source_format: format,
        worksheet: table.selected_sheet.clone(),
        mapping,
        canonical_csv_sha256: metadata.csv_sha256.clone(),
        ic_scalings: Vec::new(),
    };
    let recipe = serde_json::to_vec(&receipt).map_err(|e| invalid(&e.to_string()))?;
    metadata.preparation_source_sha256 = format!("{:x}", Sha256::digest(recipe));
    if format == TabularFormat::Xlsx {
        metadata.source_xlsx_sha256 = source_sha256;
    } else {
        metadata.source_xlsx_sha256.clear();
    }
    let source_description = serde_json::to_vec(&(
        (
            &metadata.id,
            &metadata.material,
            &metadata.sample_id,
            &metadata.source_doi,
            &metadata.source_url,
            &metadata.authors,
            &metadata.license,
            &metadata.license_url,
            &metadata.measurement_dates,
            &metadata.data_class,
            &metadata.field_basis,
            &metadata.angle_convention,
            &metadata.coordinate_policy,
            &metadata.normalization,
            &metadata.measurement_uncertainty_fraction,
            &metadata.strain_state,
        ),
        (
            &metadata.electric_field_criterion_v_per_m,
            &metadata.voltage_tap_spacing_m,
            &metadata.measured_bridge_width_m,
            &metadata.original_tape_width_m,
            &metadata.limitations,
        ),
    ))
    .map_err(|e| invalid(&e.to_string()))?;
    metadata.source_description_sha256 = format!("{:x}", Sha256::digest(source_description));
    metadata.tabular_import = Some(receipt);
    metadata.limitations.push("Imported values were converted to SI using the recorded column mapping and units. No extrapolation was performed. The source file SHA-256 and exact worksheet/mapping are recorded in tabular_import.".into());
    let metadata_json = serde_json::to_string(&metadata).map_err(|e| invalid(&e.to_string()))?;
    material_bundle_from_pair(&metadata_json, &csv_bytes)
}

struct Table {
    sheets: Vec<String>,
    selected_sheet: Option<String>,
    headers: Vec<String>,
    rows: Vec<(u32, Vec<String>)>,
}

fn read_table(
    bytes: &[u8],
    format: TabularFormat,
    sheet: Option<&str>,
) -> Result<Table, ModelError> {
    match format {
        TabularFormat::Csv | TabularFormat::Tsv => read_delimited(bytes, format),
        TabularFormat::Xlsx => read_xlsx(bytes, sheet),
    }
}

fn read_delimited(bytes: &[u8], format: TabularFormat) -> Result<Table, ModelError> {
    preflight_delimited(
        bytes,
        if format == TabularFormat::Csv {
            b','
        } else {
            b'\t'
        },
    )?;
    let delimiter = if format == TabularFormat::Csv {
        b','
    } else {
        b'\t'
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(false)
        .from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| invalid(&format!("tabular headers: {e}")))?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    validate_headers(&headers)?;
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| invalid(&format!("tabular row: {e}")))?;
        let source_row = record
            .position()
            .map(|position| position.record() as u32 + 1)
            .unwrap_or(rows.len() as u32 + 2);
        if record.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        if rows.len() == MAX_ROWS {
            return Err(invalid("tabular input exceeds 100000 data rows"));
        }
        rows.push((source_row, record.iter().map(str::to_owned).collect()));
    }
    Ok(Table {
        sheets: Vec::new(),
        selected_sheet: None,
        headers,
        rows,
    })
}

/// Bound CSV parser work before it constructs its per-field record buffers.
fn preflight_delimited(bytes: &[u8], delimiter: u8) -> Result<(), ModelError> {
    let mut records = 0usize;
    let mut total_cells = 0usize;
    let mut fields = 1usize;
    let mut field_bytes = 0usize;
    let mut in_quotes = false;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    field_bytes = field_bytes.saturating_add(1);
                    index += 2;
                    if field_bytes > MAX_FIELD_BYTES {
                        return Err(invalid("tabular field exceeds 1 MiB"));
                    }
                    continue;
                }
                in_quotes = false;
            } else {
                field_bytes = field_bytes.saturating_add(1);
            }
        } else if byte == b'"' {
            if field_bytes != 0 {
                return Err(invalid("malformed quoted tabular field"));
            }
            in_quotes = true;
        } else if byte == delimiter {
            fields += 1;
            if fields > MAX_COLUMNS {
                return Err(invalid("tabular record exceeds 256 columns"));
            }
            field_bytes = 0;
        } else if byte == b'\r' || byte == b'\n' {
            total_cells = total_cells.saturating_add(fields);
            if total_cells > MAX_TABLE_CELLS {
                return Err(invalid("tabular table exceeds 1.1 million cells"));
            }
            records += 1;
            if records > MAX_ROWS + 1 {
                return Err(invalid("tabular input exceeds 100000 data rows"));
            }
            fields = 1;
            field_bytes = 0;
            if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                index += 1;
            }
        } else {
            field_bytes = field_bytes.saturating_add(1);
        }
        if field_bytes > MAX_FIELD_BYTES {
            return Err(invalid("tabular field exceeds 1 MiB"));
        }
        index += 1;
    }
    if in_quotes {
        return Err(invalid("unterminated quoted tabular field"));
    }
    let total_records =
        records + usize::from(!bytes.is_empty() && !matches!(bytes.last(), Some(b'\n' | b'\r')));
    if !bytes.is_empty() && !matches!(bytes.last(), Some(b'\n' | b'\r')) {
        total_cells = total_cells.saturating_add(fields);
    }
    if total_records > MAX_ROWS + 1 {
        return Err(invalid("tabular input exceeds 100000 data rows"));
    }
    if total_cells > MAX_TABLE_CELLS {
        return Err(invalid("tabular table exceeds 1.1 million cells"));
    }
    if fields > MAX_COLUMNS {
        return Err(invalid("tabular record exceeds 256 columns"));
    }
    Ok(())
}

fn read_xlsx(bytes: &[u8], sheet: Option<&str>) -> Result<Table, ModelError> {
    check_xlsx_expansion(bytes)?;
    preflight_xlsx_cells(bytes)?;
    let mut workbook: Xlsx<_> = Xlsx::new(Cursor::new(bytes))
        .map_err(|e| invalid(&format!("cannot open XLSX workbook: {e}")))?;
    let sheets = workbook.sheet_names().to_vec();
    if sheets.iter().collect::<BTreeSet<_>>().len() != sheets.len() {
        return Err(invalid("XLSX workbook contains duplicate worksheet names"));
    }
    let selected = match sheet {
        Some(name) if sheets.iter().any(|candidate| candidate == name) => name.to_owned(),
        Some(name) => return Err(invalid(&format!("XLSX worksheet not found: {name}"))),
        None => sheets
            .first()
            .cloned()
            .ok_or_else(|| invalid("XLSX workbook has no worksheets"))?,
    };
    let range = workbook
        .worksheet_range(&selected)
        .map_err(|e| invalid(&format!("cannot read XLSX worksheet: {e}")))?;
    let formulas = workbook
        .worksheet_formula(&selected)
        .map_err(|e| invalid(&format!("cannot inspect XLSX formulas: {e}")))?;
    if formulas
        .rows()
        .flatten()
        .any(|formula| !formula.trim().is_empty())
    {
        return Err(invalid(
            "XLSX formulas are unsupported; import literal measured values",
        ));
    }
    let (height, width) = range.get_size();
    if height == 0 || width == 0 {
        return Err(invalid("selected XLSX worksheet is empty"));
    }
    if width > MAX_COLUMNS {
        return Err(invalid("worksheet exceeds 256 columns"));
    }
    if height > MAX_ROWS + 1 {
        return Err(invalid("worksheet exceeds 100000 data rows"));
    }
    let start_row = range.start().map(|(row, _)| row).unwrap_or(0);
    let mut iter = range.rows();
    let header_row = iter
        .next()
        .ok_or_else(|| invalid("selected XLSX worksheet is empty"))?;
    let headers = header_row.iter().map(cell_text).collect::<Vec<_>>();
    validate_headers(&headers)?;
    let mut rows = Vec::new();
    for (offset, row) in iter.enumerate() {
        if row.iter().all(|cell| cell_text(cell).trim().is_empty()) {
            continue;
        }
        if rows.len() == MAX_ROWS {
            return Err(invalid("worksheet exceeds 100000 data rows"));
        }
        let source_row = u32::try_from(start_row as usize + offset + 2)
            .map_err(|_| invalid("XLSX source row exceeds supported range"))?;
        rows.push((source_row, row.iter().map(cell_text).collect()));
    }
    Ok(Table {
        sheets,
        selected_sheet: Some(selected),
        headers,
        rows,
    })
}

/// Guard against tiny XML files that claim a cell at the far end of Excel's
/// grid, which could otherwise make a range reader allocate a huge rectangle.
fn preflight_xlsx_cells(bytes: &[u8]) -> Result<(), ModelError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| invalid(&format!("invalid XLSX ZIP archive: {e}")))?;
    let mut expanded_actual = 0u64;
    let mut entry_names = BTreeSet::new();
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|e| invalid(&format!("invalid XLSX ZIP entry: {e}")))?;
        let name = file.name().to_owned();
        if !entry_names.insert(name.clone()) {
            return Err(invalid("XLSX archive contains duplicate ZIP entry names"));
        }
        let is_xml = name.ends_with(".xml");
        let declared_size = file.size();
        let expected_crc32 = file.crc32();
        let remaining = MAX_XLSX_EXPANDED_BYTES.saturating_sub(expanded_actual);
        if declared_size > remaining {
            return Err(invalid("XLSX expanded contents exceed 64 MiB"));
        }
        let mut bounded = file.take(remaining.saturating_add(1));
        if is_xml {
            let mut contents = Vec::with_capacity(declared_size as usize);
            bounded
                .read_to_end(&mut contents)
                .map_err(|e| invalid(&format!("cannot read XLSX worksheet XML: {e}")))?;
            if contents.len() as u64 != declared_size
                || crc32fast::hash(&contents) != expected_crc32
            {
                return Err(invalid(
                    "XLSX ZIP entry size or checksum does not match its contents",
                ));
            }
            expanded_actual = expanded_actual.saturating_add(contents.len() as u64);
            if expanded_actual > MAX_XLSX_EXPANDED_BYTES {
                return Err(invalid("XLSX expanded contents exceed 64 MiB"));
            }
            inspect_worksheet_xml(&contents)?;
        } else {
            let mut buffer = [0; 8192];
            let mut entry_actual = 0u64;
            let mut checksum = crc32fast::Hasher::new();
            loop {
                let read = bounded
                    .read(&mut buffer)
                    .map_err(|e| invalid(&format!("cannot inspect XLSX ZIP entry: {e}")))?;
                if read == 0 {
                    break;
                }
                entry_actual = entry_actual.saturating_add(read as u64);
                checksum.update(&buffer[..read]);
                expanded_actual = expanded_actual.saturating_add(read as u64);
                if expanded_actual > MAX_XLSX_EXPANDED_BYTES {
                    return Err(invalid("XLSX expanded contents exceed 64 MiB"));
                }
            }
            if entry_actual != declared_size || checksum.finalize() != expected_crc32 {
                return Err(invalid(
                    "XLSX ZIP entry size or checksum does not match its contents",
                ));
            }
        }
    }
    Ok(())
}

fn inspect_worksheet_xml(contents: &[u8]) -> Result<(), ModelError> {
    let mut probe = quick_xml::Reader::from_reader(contents);
    let has_sheet_data = loop {
        match probe.read_event() {
            Ok(
                quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element),
            ) if element.local_name().as_ref() == b"sheetData" => break true,
            Ok(quick_xml::events::Event::Eof) => break false,
            Ok(_) => {}
            Err(error) => return Err(invalid(&format!("invalid XLSX XML: {error}"))),
        }
    };
    if !has_sheet_data {
        return Ok(());
    }
    let mut reader = quick_xml::Reader::from_reader(contents);
    let mut max_row = 0usize;
    let mut max_column = 0usize;
    let mut current_row = 0usize;
    let mut row_cell_count = 0usize;
    let mut implicit_column = 0usize;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(element))
                if element.local_name().as_ref() == b"row" =>
            {
                row_cell_count = 0;
                implicit_column = 0;
                current_row = match attribute_value(&element, b"r")? {
                    Some(value) => parse_positive_index(&value, MAX_ROWS + 1, "XLSX row")?,
                    None => current_row.saturating_add(1),
                };
                if current_row > MAX_ROWS + 1 {
                    return Err(invalid("XLSX worksheet exceeds 100000 data rows"));
                }
                max_row = max_row.max(current_row);
            }
            Ok(quick_xml::events::Event::Empty(element))
                if element.local_name().as_ref() == b"row" =>
            {
                row_cell_count = 0;
                implicit_column = 0;
                current_row = match attribute_value(&element, b"r")? {
                    Some(value) => parse_positive_index(&value, MAX_ROWS + 1, "XLSX row")?,
                    None => current_row.saturating_add(1),
                };
                if current_row > MAX_ROWS + 1 {
                    return Err(invalid("XLSX worksheet exceeds 100000 data rows"));
                }
                max_row = max_row.max(current_row);
            }
            Ok(
                quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element),
            ) if element.local_name().as_ref() == b"c" => {
                row_cell_count += 1;
                if row_cell_count > MAX_COLUMNS {
                    return Err(invalid("XLSX worksheet exceeds 256 columns"));
                }
                let mut explicit_reference = None;
                for attribute in element.attributes() {
                    let attribute = attribute
                        .map_err(|e| invalid(&format!("invalid XLSX cell attribute: {e}")))?;
                    if attribute.key.as_ref() == b"r" {
                        let coordinate = std::str::from_utf8(attribute.value.as_ref())
                            .map_err(|e| invalid(&format!("invalid XLSX cell coordinate: {e}")))?;
                        let (column, row) = check_cell_coordinate(coordinate)?;
                        if current_row != 0 && row != current_row {
                            return Err(invalid("XLSX cell row differs from its enclosing row"));
                        }
                        max_column = max_column.max(column);
                        max_row = max_row.max(row);
                        explicit_reference = Some(column);
                    }
                }
                if current_row == 0 {
                    return Err(invalid("XLSX cell has no enclosing row"));
                }
                let column = explicit_reference.unwrap_or(implicit_column + 1);
                if explicit_reference.is_some() {
                    implicit_column = column;
                } else {
                    implicit_column += 1;
                }
                if column > MAX_COLUMNS {
                    return Err(invalid("XLSX worksheet exceeds 256 columns"));
                }
                max_column = max_column.max(column);
                max_row = max_row.max(current_row);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(invalid(&format!("invalid XLSX worksheet XML: {error}"))),
        }
    }
    if max_row.saturating_mul(max_column) > MAX_WORKSHEET_CELLS {
        return Err(invalid(
            "XLSX worksheet's used cell rectangle exceeds one million cells",
        ));
    }
    Ok(())
}

fn attribute_value(
    element: &quick_xml::events::BytesStart<'_>,
    key: &[u8],
) -> Result<Option<String>, ModelError> {
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|e| invalid(&format!("invalid XLSX attribute: {e}")))?;
        if attribute.key.as_ref() == key {
            return std::str::from_utf8(attribute.value.as_ref())
                .map(|value| Some(value.to_owned()))
                .map_err(|e| invalid(&format!("invalid XLSX attribute text: {e}")));
        }
    }
    Ok(None)
}

fn parse_positive_index(value: &str, maximum: usize, label: &str) -> Result<usize, ModelError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| invalid(&format!("invalid {label} index")))?;
    if parsed == 0 || parsed > maximum {
        return Err(invalid(&format!("{label} index exceeds import limits")));
    }
    Ok(parsed)
}

fn check_cell_coordinate(coordinate: &str) -> Result<(usize, usize), ModelError> {
    let bytes = coordinate.as_bytes();
    let mut split = 0;
    let mut column = 0usize;
    while let Some(byte) = bytes.get(split).filter(|byte| byte.is_ascii_alphabetic()) {
        column = column
            .checked_mul(26)
            .and_then(|value| value.checked_add(usize::from(byte.to_ascii_uppercase() - b'A' + 1)))
            .ok_or_else(|| invalid("invalid XLSX cell column"))?;
        split += 1;
    }
    let mut row = 0usize;
    for byte in &bytes[split..] {
        if !byte.is_ascii_digit() {
            return Err(invalid("invalid XLSX cell coordinate"));
        }
        row = row
            .checked_mul(10)
            .and_then(|value| value.checked_add(usize::from(byte - b'0')))
            .ok_or_else(|| invalid("invalid XLSX cell row"))?;
    }
    if split == 0
        || split == bytes.len()
        || column == 0
        || column > MAX_COLUMNS
        || row == 0
        || row > MAX_ROWS + 1
    {
        return Err(invalid(
            "XLSX worksheet cell lies outside the 100000-row/256-column import limit",
        ));
    }
    Ok((column, row))
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(value) => value.clone(),
        Data::Float(value) => value.to_string(),
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::DateTime(value) => value.to_string(),
        Data::DateTimeIso(value) | Data::DurationIso(value) => value.to_string(),
        Data::Error(value) => value.to_string(),
    }
}

fn validate_headers(headers: &[String]) -> Result<(), ModelError> {
    if headers.is_empty() || headers.len() > MAX_COLUMNS {
        return Err(invalid("tabular input must have 1 to 256 columns"));
    }
    if headers.iter().any(|header| header.trim().is_empty()) {
        return Err(invalid("tabular header names cannot be empty"));
    }
    let unique = headers
        .iter()
        .map(|header| header.trim())
        .collect::<BTreeSet<_>>();
    if unique.len() != headers.len() {
        return Err(invalid("duplicate tabular column headers"));
    }
    Ok(())
}

fn validate_mapping(mapping: &ColumnMapping, width: usize) -> Result<(), ModelError> {
    let mut used = vec![
        mapping.temperature,
        mapping.field,
        mapping.angle,
        mapping.ic,
    ];
    if let NValueSource::Column(column) = mapping.n_value {
        used.push(column);
    }
    if let NominalCoordinates::RequireColumns {
        temperature,
        field,
        angle,
    } = mapping.nominal_coordinates
    {
        used.extend([temperature, field, angle]);
    }
    if used.iter().any(|column| *column >= width) {
        return Err(invalid(
            "column mapping refers to a column outside the selected table",
        ));
    }
    if let NValueSource::Constant(value) = mapping.n_value
        && (!value.is_finite() || value <= 1.0)
    {
        return Err(invalid(
            "constant n-value must be finite and greater than one",
        ));
    }
    Ok(())
}

fn value(row: &[String], column: usize, name: &str) -> Result<f64, ModelError> {
    let raw = row
        .get(column)
        .ok_or_else(|| invalid("row has fewer columns than its header"))?
        .trim();
    let parsed = raw
        .parse::<f64>()
        .map_err(|_| invalid(&format!("{name} is not a plain numeric value: {raw}")))?;
    if !parsed.is_finite() {
        return Err(invalid(&format!("{name} must be finite")));
    }
    Ok(parsed)
}

fn check_input(bytes: &[u8]) -> Result<String, ModelError> {
    if bytes.is_empty() {
        return Err(invalid("tabular input is empty"));
    }
    if bytes.len() > MAX_TABULAR_INPUT_BYTES {
        return Err(invalid("tabular input exceeds 32 MiB"));
    }
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Inspect XLSX central-directory sizes before asking the workbook reader to
/// inflate XML parts. Zip64 and multi-disk files are rejected so expansion
/// limits cannot be bypassed by sentinel sizes.
fn check_xlsx_expansion(bytes: &[u8]) -> Result<(), ModelError> {
    let search_start = bytes.len().saturating_sub(65_557);
    let eocd = bytes[search_start..]
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .map(|position| search_start + position)
        .ok_or_else(|| invalid("invalid XLSX ZIP directory"))?;
    if eocd + 22 > bytes.len() {
        return Err(invalid("truncated XLSX ZIP directory"));
    }
    let disk = read_u16(bytes, eocd + 4)?;
    let directory_disk = read_u16(bytes, eocd + 6)?;
    let disk_entries = read_u16(bytes, eocd + 8)?;
    let entries = read_u16(bytes, eocd + 10)? as usize;
    let directory_size = read_u32(bytes, eocd + 12)? as usize;
    let directory_offset = read_u32(bytes, eocd + 16)? as usize;
    if disk != 0
        || directory_disk != 0
        || disk_entries as usize != entries
        || entries == u16::MAX as usize
        || directory_size == u32::MAX as usize
        || directory_offset == u32::MAX as usize
    {
        return Err(invalid("multi-disk and Zip64 XLSX files are unsupported"));
    }
    if entries > MAX_XLSX_ENTRIES {
        return Err(invalid("XLSX workbook has too many ZIP entries"));
    }
    let end = directory_offset
        .checked_add(directory_size)
        .ok_or_else(|| invalid("invalid XLSX ZIP directory size"))?;
    if end > bytes.len() || directory_offset >= eocd {
        return Err(invalid("invalid XLSX ZIP directory bounds"));
    }
    let mut cursor = directory_offset;
    let mut expanded = 0u64;
    for _ in 0..entries {
        if read_u32(bytes, cursor)? != 0x0201_4b50 {
            return Err(invalid("invalid XLSX ZIP entry"));
        }
        let flags = read_u16(bytes, cursor + 8)?;
        let uncompressed = read_u32(bytes, cursor + 24)?;
        let name_len = read_u16(bytes, cursor + 28)? as usize;
        let extra_len = read_u16(bytes, cursor + 30)? as usize;
        let comment_len = read_u16(bytes, cursor + 32)? as usize;
        if flags & 1 != 0 || uncompressed == u32::MAX {
            return Err(invalid("encrypted or Zip64 XLSX entry is unsupported"));
        }
        expanded = expanded.saturating_add(uncompressed as u64);
        if expanded > MAX_XLSX_EXPANDED_BYTES {
            return Err(invalid("XLSX expanded contents exceed 64 MiB"));
        }
        cursor = cursor
            .checked_add(46 + name_len + extra_len + comment_len)
            .ok_or_else(|| invalid("invalid XLSX ZIP entry size"))?;
        if cursor > end {
            return Err(invalid("truncated XLSX ZIP directory entry"));
        }
    }
    if cursor != end {
        return Err(invalid("inconsistent XLSX ZIP directory size"));
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ModelError> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| invalid("truncated XLSX ZIP metadata"))?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ModelError> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("truncated XLSX ZIP metadata"))?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}
fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}
fn temperature_to_kelvin(value: f64, unit: TemperatureUnit) -> f64 {
    match unit {
        TemperatureUnit::Kelvin => value,
        TemperatureUnit::Celsius => value + 273.15,
    }
}
fn field_to_tesla(value: f64, unit: FieldUnit) -> f64 {
    match unit {
        FieldUnit::Tesla => value,
        FieldUnit::Millitesla => value / 1000.0,
        FieldUnit::Gauss => value / 10_000.0,
    }
}
fn angle_to_degrees(value: f64, unit: AngleUnit) -> f64 {
    match unit {
        AngleUnit::Degrees => value,
        AngleUnit::Radians => value.to_degrees(),
    }
}
fn sorted_floats(bits: BTreeSet<u64>) -> Vec<f64> {
    let mut values = bits.into_iter().map(f64::from_bits).collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    values
}
fn invalid(message: &str) -> ModelError {
    ModelError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use crate::material::{MaterialBundle, MaterialMetadata, SUPERPOWER_METADATA};

    use super::*;

    const HEADERS: &str = "temperature,field,angle,ic,n\n";

    fn grid_csv(ic: &str) -> Vec<u8> {
        let mut csv = String::from(HEADERS);
        for temperature in [20.0, 30.0] {
            for field in [1.0, 2.0] {
                for angle in [0.0, 90.0] {
                    csv.push_str(&format!("{temperature},{field},{angle},{ic},25\n"));
                }
            }
        }
        csv.into_bytes()
    }

    fn metadata() -> MaterialMetadata {
        let mut metadata: MaterialMetadata = serde_json::from_str(SUPERPOWER_METADATA).unwrap();
        metadata.id = "test-tabular-import".into();
        metadata.source_xlsx_sha256.clear();
        metadata.tabular_import = None;
        metadata
    }

    fn mapping() -> ColumnMapping {
        ColumnMapping {
            temperature: 0,
            field: 1,
            angle: 2,
            ic: 3,
            temperature_unit: TemperatureUnit::Kelvin,
            field_unit: FieldUnit::Tesla,
            angle_unit: AngleUnit::Degrees,
            ic_unit: IcUnit::Ampere,
            n_value: NValueSource::Column(4),
            nominal_coordinates: NominalCoordinates::ReuseMeasuredExplicitly,
        }
    }

    #[test]
    fn conversions_emit_canonical_si_and_valid_receipt() {
        let mut selected = mapping();
        selected.temperature_unit = TemperatureUnit::Celsius;
        selected.field_unit = FieldUnit::Gauss;
        selected.angle_unit = AngleUnit::Radians;
        selected.ic_unit = IcUnit::Kiloampere;
        let mut csv = String::from("temperature,field,angle,ic,n\n");
        for temperature in [20.0, 30.0] {
            for field in [10_000.0, 20_000.0] {
                for angle in [0.0, std::f64::consts::FRAC_PI_2] {
                    csv.push_str(&format!("{temperature},{field},{angle},0.29311,25\n"));
                }
            }
        }
        let source = csv.into_bytes();
        let bundle =
            import_material(&source, TabularFormat::Csv, None, selected, metadata()).unwrap();
        let point = &bundle.dataset.points[0];
        assert_eq!(point.temperature_k, 293.15);
        assert_eq!(point.applied_field_t, 1.0);
        assert_eq!(point.angle_from_normal_deg, 0.0);
        assert!((point.bridge_ic_a - 293.11).abs() < 1e-9);
        assert!((point.ic_a_per_m - 293_110.0).abs() < 1e-7);
        assert_eq!(
            bundle.dataset.metadata.schema,
            crate::material::MATERIAL_SCHEMA_V2
        );
        assert_eq!(
            bundle.dataset.metadata.selection.nominal_temperature_k,
            [293.15, 303.15]
        );
        assert!(bundle.dataset.metadata.source_xlsx_sha256.is_empty());
        let receipt = bundle.dataset.metadata.tabular_import.as_ref().unwrap();
        assert_eq!(
            receipt.source_sha256,
            format!("{:x}", Sha256::digest(source))
        );
        assert_eq!(receipt.source_format, TabularFormat::Csv);
        assert!(
            bundle
                .csv_data
                .starts_with("source_row,nominal_temperature_k,nominal_field_t")
        );
        let round_trip = MaterialBundle::from_json(&bundle.to_json().unwrap()).unwrap();
        assert_eq!(
            round_trip.dataset.metadata.tabular_import.unwrap().mapping,
            selected
        );
        assert_eq!(bundle.dataset.metadata.source_description_sha256.len(), 64);
        assert!(matches!(
            bundle.dataset.metadata.data_class,
            crate::material::MaterialDataClass::Measured
        ));
    }

    #[test]
    fn imported_ic_scaling_receipt_round_trips_as_synthetic_bundle() {
        let source = grid_csv("293.11");
        let bundle =
            import_material(&source, TabularFormat::Csv, None, mapping(), metadata()).unwrap();
        let scaled = bundle.dataset.scaled_ic(1.1).unwrap();
        assert!(matches!(
            scaled.metadata.data_class,
            crate::material::MaterialDataClass::SyntheticSensitivity
        ));
        let receipt = scaled.metadata.tabular_import.as_ref().unwrap();
        assert_eq!(receipt.ic_scalings.len(), 1);
        assert_eq!(
            receipt.ic_scalings[0].parent_csv_sha256,
            bundle.dataset.metadata.csv_sha256
        );
        assert_eq!(
            receipt.ic_scalings[0].result_csv_sha256,
            scaled.metadata.csv_sha256
        );

        let mut writer = csv::Writer::from_writer(Vec::new());
        for point in &scaled.points {
            writer.serialize(point).unwrap();
        }
        let csv_data = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        let scaled_bundle =
            MaterialBundle::from_parts(scaled, csv_data, None).expect("scaled bundle validates");
        let reloaded = MaterialBundle::from_json(&scaled_bundle.to_json().unwrap()).unwrap();
        assert!((reloaded.dataset.points[0].bridge_ic_a - 322.421).abs() < 1e-9);
        assert_eq!(
            reloaded
                .dataset
                .metadata
                .tabular_import
                .unwrap()
                .ic_scalings
                .len(),
            1
        );
    }

    #[test]
    fn all_ic_units_and_millitesla_use_the_declared_bridge_width() {
        for (unit, input) in [
            (IcUnit::Ampere, "293.11"),
            (IcUnit::Kiloampere, "0.29311"),
            (IcUnit::AmperePerMeter, "293110"),
            (IcUnit::AmperePerCentimeter, "2931.1"),
        ] {
            let mut selected = mapping();
            selected.ic_unit = unit;
            selected.field_unit = FieldUnit::Millitesla;
            let mut csv = String::from(HEADERS);
            for temperature in [20.0, 30.0] {
                for field in [1000.0, 2000.0] {
                    for angle in [0.0, 90.0] {
                        csv.push_str(&format!("{temperature},{field},{angle},{input},25\n"));
                    }
                }
            }
            let bundle = import_material(
                csv.as_bytes(),
                TabularFormat::Csv,
                None,
                selected,
                metadata(),
            )
            .unwrap();
            let point = &bundle.dataset.points[0];
            assert!((point.bridge_ic_a - 293.11).abs() < 1e-9);
            assert!((point.ic_a_per_m - 293_110.0).abs() < 1e-7);
            assert_eq!(point.applied_field_t, 1.0);
        }
    }

    #[test]
    fn nominal_columns_remain_distinct_from_measured_coordinates() {
        let mut csv = String::from("t,b,a,ic,n,nt,nb,na\n");
        for temperature in [20.0, 30.0] {
            for field in [1.0, 2.0] {
                for angle in [0.0, 90.0] {
                    csv.push_str(&format!(
                        "{},{},{},300,25,{temperature},{field},{angle}\n",
                        temperature + 0.1,
                        field,
                        angle - 1.0
                    ));
                }
            }
        }
        let mut selected = mapping();
        selected.nominal_coordinates = NominalCoordinates::RequireColumns {
            temperature: 5,
            field: 6,
            angle: 7,
        };
        let bundle = import_material(
            csv.as_bytes(),
            TabularFormat::Csv,
            None,
            selected,
            metadata(),
        )
        .unwrap();
        let first = &bundle.dataset.points[0];
        assert_eq!(first.temperature_k, 20.1);
        assert_eq!(first.nominal_temperature_k, 20.0);
        assert_eq!(first.angle_from_normal_deg, -1.0);
        assert_eq!(first.nominal_angle_deg, 0.0);
    }

    #[test]
    fn tsv_preview_and_import_keep_source_row_identity() {
        let tsv = String::from_utf8(grid_csv("300"))
            .unwrap()
            .replace(',', "\t");
        let preview = inspect_tabular(tsv.as_bytes(), TabularFormat::Tsv, None, 1).unwrap();
        assert_eq!(preview.source_rows, [2]);
        let bundle = import_material(
            tsv.as_bytes(),
            TabularFormat::Tsv,
            None,
            mapping(),
            metadata(),
        )
        .unwrap();
        assert_eq!(bundle.dataset.points[0].source_row, 2);
        assert_eq!(
            bundle
                .dataset
                .metadata
                .tabular_import
                .unwrap()
                .source_format,
            TabularFormat::Tsv
        );
    }

    #[test]
    fn import_preserves_a_declared_model_data_class() {
        let mut declared = metadata();
        declared.data_class = crate::material::MaterialDataClass::PublishedModelFit;
        let bundle = import_material(
            &grid_csv("300"),
            TabularFormat::Csv,
            None,
            mapping(),
            declared,
        )
        .unwrap();
        assert!(matches!(
            bundle.dataset.metadata.data_class,
            crate::material::MaterialDataClass::PublishedModelFit
        ));
    }

    #[test]
    fn rejects_bad_values_columns_and_duplicate_coordinates() {
        let mut selected = mapping();
        selected.field = 99;
        assert!(
            import_material(
                &grid_csv("300"),
                TabularFormat::Csv,
                None,
                selected,
                metadata()
            )
            .is_err()
        );

        let mut bad_numeric = grid_csv("300");
        bad_numeric = String::from_utf8(bad_numeric)
            .unwrap()
            .replacen("20,1,0,300,25", "20,1,0,NaN,25", 1)
            .into_bytes();
        assert!(
            import_material(
                &bad_numeric,
                TabularFormat::Csv,
                None,
                mapping(),
                metadata()
            )
            .is_err()
        );

        let mut duplicate = grid_csv("300");
        let line = "30,2,90,300,25\n";
        duplicate.extend_from_slice(line.as_bytes());
        assert!(
            import_material(&duplicate, TabularFormat::Csv, None, mapping(), metadata()).is_err()
        );
    }

    #[test]
    fn rejects_missing_nominal_policy_and_unsupported_domain() {
        let mut selected = mapping();
        selected.nominal_coordinates = NominalCoordinates::RequireColumns {
            temperature: 3,
            field: 4,
            angle: 2,
        };
        assert!(
            import_material(
                &grid_csv("300"),
                TabularFormat::Csv,
                None,
                selected,
                metadata()
            )
            .is_err()
        );

        let mut outside = grid_csv("300");
        outside = String::from_utf8(outside)
            .unwrap()
            .replacen("20,1,0,300,25", "20,1001,0,300,25", 1)
            .into_bytes();
        assert!(
            import_material(&outside, TabularFormat::Csv, None, mapping(), metadata()).is_err()
        );

        let mut no_angle_span = String::from(HEADERS);
        for temperature in [20.0, 30.0] {
            for field in [1.0, 2.0] {
                for _ in 0..2 {
                    no_angle_span.push_str(&format!("{temperature},{field},0,300,25\n"));
                }
            }
        }
        assert!(
            import_material(
                no_angle_span.as_bytes(),
                TabularFormat::Csv,
                None,
                mapping(),
                metadata()
            )
            .is_err()
        );
    }

    #[test]
    fn xlsx_preview_selects_sheet_and_records_physical_row_numbers() {
        let bytes = test_xlsx();
        let first = inspect_tabular(&bytes, TabularFormat::Xlsx, None, 2).unwrap();
        assert_eq!(first.sheets, ["First", "Measurements"]);
        assert_eq!(first.selected_sheet.as_deref(), Some("First"));
        assert_eq!(first.headers, ["ignored"]);
        let preview =
            inspect_tabular(&bytes, TabularFormat::Xlsx, Some("Measurements"), 2).unwrap();
        assert_eq!(
            preview.headers,
            ["temperature", "field", "angle", "ic", "n"]
        );
        assert_eq!(preview.source_rows, [2, 3]);
        assert_eq!(preview.total_rows, 8);
        assert!(preview.truncated);
        let bundle = import_material(
            &bytes,
            TabularFormat::Xlsx,
            Some("Measurements"),
            mapping(),
            metadata(),
        )
        .unwrap();
        assert_eq!(bundle.dataset.points[0].source_row, 2);
        let receipt = bundle.dataset.metadata.tabular_import.as_ref().unwrap();
        assert_eq!(
            bundle.dataset.metadata.source_xlsx_sha256,
            receipt.source_sha256
        );
        assert_eq!(receipt.worksheet.as_deref(), Some("Measurements"));
        assert!(inspect_tabular(&bytes, TabularFormat::Xlsx, Some("missing"), 2).is_err());
        assert!(
            import_material(
                &test_xlsx_with_formula(),
                TabularFormat::Xlsx,
                Some("Measurements"),
                mapping(),
                metadata()
            )
            .is_err()
        );
        assert!(
            inspect_tabular(
                &test_xlsx_with_duplicate_sheets(),
                TabularFormat::Xlsx,
                None,
                2
            )
            .is_err()
        );
    }

    #[test]
    fn workbook_expansion_and_input_bytes_are_bounded() {
        assert!(check_input(&vec![b'x'; MAX_TABULAR_INPUT_BYTES + 1]).is_err());
        assert!(check_xlsx_expansion(b"not a workbook").is_err());
        assert_eq!(check_cell_coordinate("IV100001").unwrap(), (256, 100_001));
        assert!(check_cell_coordinate("XFD1").is_err());
        assert!(check_cell_coordinate("A100002").is_err());
        assert!(check_cell_coordinate("A0").is_err());
        assert!(inspect_worksheet_xml(b"<worksheet><dimension ref='A1:A1'/><sheetData><row r='1'><c r='XFD1'/></row></sheetData></worksheet>").is_err());
        assert!(
            inspect_worksheet_xml(
                b"<worksheet><sheetData><row r='100002'/></sheetData></worksheet>"
            )
            .is_err()
        );
        let many_columns = format!(
            "<worksheet><sheetData><row r='1'>{}</row></sheetData></worksheet>",
            "<c/>".repeat(MAX_COLUMNS + 1)
        );
        assert!(inspect_worksheet_xml(many_columns.as_bytes()).is_err());
        assert!(
            preflight_delimited(format!("{}\n", ",".repeat(MAX_COLUMNS)).as_bytes(), b',').is_err()
        );
        assert!(preflight_delimited(&vec![b'x'; MAX_FIELD_BYTES + 1], b',').is_err());
        let too_many_records = format!("header\n{}", "x\n".repeat(MAX_ROWS + 1));
        assert!(preflight_delimited(too_many_records.as_bytes(), b',').is_err());
        let too_many_cr_records = format!("header\r{}", "x\r".repeat(MAX_ROWS + 1));
        assert!(preflight_delimited(too_many_cr_records.as_bytes(), b',').is_err());
        let too_many_cells = format!("{}\n", ",".repeat(199)).repeat(6000);
        assert!(preflight_delimited(too_many_cells.as_bytes(), b',').is_err());
        assert!(preflight_xlsx_cells(&corrupt_declared_sheet_size(test_xlsx())).is_err());
    }

    #[test]
    fn receipt_cannot_be_detached_from_canonical_csv_or_preparation_hash() {
        let bundle = import_material(
            &grid_csv("300"),
            TabularFormat::Csv,
            None,
            mapping(),
            metadata(),
        )
        .unwrap();
        let mut metadata = bundle.dataset.metadata.clone();
        metadata.csv_sha256 = "0".repeat(64);
        assert!(metadata.validate().is_err());
        metadata = bundle.dataset.metadata.clone();
        metadata.preparation_source_sha256 = "0".repeat(64);
        assert!(metadata.validate().is_err());
        metadata = bundle.dataset.metadata.clone();
        metadata
            .tabular_import
            .as_mut()
            .unwrap()
            .mapping
            .temperature = MAX_COLUMNS;
        assert!(metadata.validate().is_err());
        metadata = bundle.dataset.metadata.clone();
        metadata.tabular_import.as_mut().unwrap().worksheet = Some("unexpected".into());
        assert!(metadata.validate().is_err());
    }

    fn test_xlsx() -> Vec<u8> {
        test_xlsx_variant(false, false)
    }

    fn test_xlsx_with_formula() -> Vec<u8> {
        test_xlsx_variant(true, false)
    }

    fn test_xlsx_with_duplicate_sheets() -> Vec<u8> {
        test_xlsx_variant(false, true)
    }

    fn test_xlsx_variant(with_formula: bool, duplicate_sheets: bool) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let workbook_xml = if duplicate_sheets {
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="First" sheetId="1" r:id="rId1"/><sheet name="First" sheetId="2" r:id="rId2"/></sheets></workbook>"#
        } else {
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="First" sheetId="1" r:id="rId1"/><sheet name="Measurements" sheetId="2" r:id="rId2"/></sheets></workbook>"#
        };
        let files = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            ("xl/workbook.xml", workbook_xml),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>ignored</t></is></c></row><row r="2"><c r="A2"><v>1</v></c></row></sheetData></worksheet>"#,
            ),
        ];
        for (name, contents) in files {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        let mut sheet = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\">",
        );
        for (index, name) in ["temperature", "field", "angle", "ic", "n"]
            .iter()
            .enumerate()
        {
            let col = (b'A' + index as u8) as char;
            sheet.push_str(&format!(
                "<c r=\"{col}1\" t=\"inlineStr\"><is><t>{name}</t></is></c>"
            ));
        }
        sheet.push_str("</row>");
        let mut row_number = 2;
        for temperature in [20.0, 30.0] {
            for field in [1.0, 2.0] {
                for angle in [0.0, 90.0] {
                    sheet.push_str(&format!("<row r=\"{row_number}\">"));
                    for (index, value) in
                        [temperature, field, angle, 300.0, 25.0].iter().enumerate()
                    {
                        let col = (b'A' + index as u8) as char;
                        if with_formula && row_number == 2 && index == 0 {
                            sheet.push_str(&format!(
                                "<c r=\"{col}{row_number}\"><f>10+10</f><v>{value}</v></c>"
                            ));
                        } else {
                            sheet.push_str(&format!(
                                "<c r=\"{col}{row_number}\"><v>{value}</v></c>"
                            ));
                        }
                    }
                    sheet.push_str("</row>");
                    row_number += 1;
                }
            }
        }
        sheet.push_str("</sheetData></worksheet>");
        writer
            .start_file("xl/worksheets/sheet2.xml", options)
            .unwrap();
        writer.write_all(sheet.as_bytes()).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn corrupt_declared_sheet_size(mut bytes: Vec<u8>) -> Vec<u8> {
        let mut offset = 0;
        while offset + 46 <= bytes.len() {
            if bytes.get(offset..offset + 4) == Some(b"PK\x01\x02") {
                let name_len = read_u16(&bytes, offset + 28).unwrap() as usize;
                let extra_len = read_u16(&bytes, offset + 30).unwrap() as usize;
                let comment_len = read_u16(&bytes, offset + 32).unwrap() as usize;
                let name =
                    std::str::from_utf8(&bytes[offset + 46..offset + 46 + name_len]).unwrap();
                if name == "xl/worksheets/sheet2.xml" {
                    let local = read_u32(&bytes, offset + 42).unwrap() as usize;
                    bytes[offset + 24..offset + 28].copy_from_slice(&1u32.to_le_bytes());
                    bytes[local + 22..local + 26].copy_from_slice(&1u32.to_le_bytes());
                    return bytes;
                }
                offset += 46 + name_len + extra_len + comment_len;
            } else {
                offset += 1;
            }
        }
        panic!("worksheet ZIP entry not found")
    }
}
