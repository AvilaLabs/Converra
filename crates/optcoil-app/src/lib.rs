mod author;
mod brand;
mod capture;
mod importer;
mod recovery;
#[cfg(not(target_arch = "wasm32"))]
mod recovery_close;
mod recovery_coordinator;
mod views;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(test)]
mod workflow_tests;

use eframe::egui::{self, Color32, RichText};
use optcoil_model::{
    Assessment, Case, Status,
    coupled_search::CoupledSearchCase,
    material::{MaterialBundle, MaterialDataset},
};
use optcoil_search::{
    RunRecord, acceptance,
    bakeoff::{BakeoffRecord, run_bakeoff},
    bom::{BomRecord, bom_from_record},
    coupled_search::{CoupledSearchOptions, CoupledSearchRunRecord, SearchProgress},
    gradereport::{GradingReport, grade_report_from_record},
    sensitivity::SensitivitySweepRecord,
    study::{StudyEngineSession, StudyWorkspace},
    verify::{self, CheckLine},
};
// Searches launch only on native builds — the browser refuses them
// before these are referenced.
#[cfg(not(target_arch = "wasm32"))]
use optcoil_search::{SearchOptions, run_cancellable};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, thread, time::Instant};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

/// Avila Labs sign-in controls: device sign-in on the desktop, the account
/// service's session cookie in the browser.
#[cfg(target_arch = "wasm32")]
type Suite = avila_account::ui_web::WebSuite;
#[cfg(not(target_arch = "wasm32"))]
type Suite = avila_account::ui_desktop::DesktopSuite;

/// Native entry point — the browser build enters through `web::start_web`.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(brand::ICON).expect("embedded Converra icon");
    // Renderer escape hatch: wgpu is the default, but on weak integrated
    // graphics (or a software-rasterizer fallback like llvmpipe) Glow's
    // OpenGL path is noticeably lighter for a UI this simple.
    // `CONVERRA_RENDERER=glow` selects it.
    let renderer = match std::env::var("CONVERRA_RENDERER").as_deref() {
        Ok("glow") => eframe::Renderer::Glow,
        Ok("wgpu") | Err(_) => eframe::Renderer::Wgpu,
        Ok(other) => {
            eprintln!("CONVERRA_RENDERER: unknown renderer '{other}' — using wgpu");
            eframe::Renderer::Wgpu
        }
    };
    eframe::run_native(
        "Converra — Avila Labs",
        eframe::NativeOptions {
            renderer,
            viewport: egui::ViewportBuilder::default()
                .with_icon(icon)
                .with_inner_size([1440.0, 960.0])
                .with_min_inner_size([960.0, 680.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(Workbench::new(&cc.egui_ctx)?))),
    )
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
enum Page {
    Overview,
    Materials,
    Checks,
    /// Decision artifacts computed from a coupled-search record —
    /// BOM, margin frontier, vendor bake-off, integrity verify.
    Reports,
    Integrations,
    Study,
}
#[derive(Clone, Copy, PartialEq)]
// Some variants exist only for the native build — searches, queue and
// folder jobs are refused before they can be constructed in the browser.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum JobKind {
    Open,
    Search,
    Export,
    Verify,
    Sweep,
    Bakeoff,
    Profile,
    Queue,
    Workspace,
    Robustness,
    Import,
}
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum JobResult {
    Loaded(Box<(Case, Assessment)>, PathBuf),
    /// A coupled-search case plus its raw file bytes: the run hashes the
    /// exact input text into `case_sha256`, so it is kept verbatim rather
    /// than re-serialized.
    LoadedSearch(Box<(CoupledSearchCase, String)>, PathBuf),
    /// A saved coupled-search run record opened read-only — the embedded
    /// case is shown, but there is no source JSON to re-run.
    LoadedSearchRun(Box<CoupledSearchRunRecord>, PathBuf),
    SourceCaseLoaded(String, PathBuf),
    Calculated(Box<RunRecord>),
    SearchCompleted(Box<CoupledSearchRunRecord>),
    StudySearchCompleted(
        Box<(
            StudyWorkspace,
            StudyEngineSession,
            bool,
            Result<CoupledSearchRunRecord, String>,
        )>,
    ),
    #[cfg(target_arch = "wasm32")]
    BrowserStudySearchCompleted(Box<(StudyWorkspace, CoupledSearchRunRecord, bool)>),
    #[cfg(target_arch = "wasm32")]
    BrowserRobustnessCompleted(Box<optcoil_search::robustness::RobustnessStudyRecord>),
    /// A material dataset bundle the user picked explicitly (coupled mode).
    DatasetLoaded(Box<MaterialBundle>, PathBuf, Option<String>),
    TablePicked(String, Vec<u8>),
    TablePreview(Box<optcoil_model::tabular_import::ImportPreview>),
    ImportMetadataLoaded(String),
    MaterialImported(Box<MaterialBundle>),
    Exported(PathBuf),
    /// A newly authored coupled-search case file — opened after writing.
    CaseWritten(PathBuf),
    /// The user picked a case-library folder.
    LibraryPicked(PathBuf),
    /// The user picked a folder of dataset bundles for a bake-off.
    BakeoffDirPicked(PathBuf),
    /// Acceptance verify ledger — kept even when checks FAIL, so the
    /// report page can render which lines held and which did not.
    Verified(Vec<CheckLine>),
    SweepDone(Box<SensitivitySweepRecord>),
    BakeoffDone(Box<BakeoffRecord>),
    /// Bore B_z(z) samples for a candidate — (candidate_index, points).
    ProfileDone(usize, Vec<[f64; 2]>),
    /// A second run record loaded for side-by-side compare.
    CompareLoaded(Box<CoupledSearchRunRecord>),
    /// Case files picked for the sequential run queue.
    Queued(Vec<PathBuf>),
    /// A save path chosen for the pending viewport screenshot.
    ScreenshotPathPicked(PathBuf),
    WorkspaceLoaded(Box<StudyWorkspace>, PathBuf),
    WorkspaceWritten(PathBuf),
    StudySummaryReady(Box<optcoil_search::study::StudySummary>),
    StudyCompareReady(
        Box<(
            optcoil_search::study::StudyDiff,
            optcoil_search::study::StudySummary,
            optcoil_search::study::StudySummary,
        )>,
    ),
    RobustnessCompleted(Box<StudyWorkspace>),
    RobustnessExported(PathBuf),
    StudyVariantLoaded(Box<(String, String, Vec<String>)>),
    Dismissed,
}

/// Load the persisted workbench state (recent files + library folder).
/// There is no config directory in the browser — persistence starts
/// empty every visit.
#[cfg(target_arch = "wasm32")]
fn load_persisted() -> (Vec<PathBuf>, Option<PathBuf>) {
    (Vec::new(), None)
}

#[cfg(not(target_arch = "wasm32"))]
fn load_persisted() -> (Vec<PathBuf>, Option<PathBuf>) {
    let Some(path) = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .map(|d| d.join("converra").join("workbench.json"))
    else {
        return (Vec::new(), None);
    };
    let Ok(text) = fs::read_to_string(path) else {
        return (Vec::new(), None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (Vec::new(), None);
    };
    let recent = value["recent_files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default();
    let library = value["library_dir"]
        .as_str()
        .map(PathBuf::from)
        .filter(|p| p.is_dir());
    (recent, library)
}

/// Where a coupled-search case's declared material dataset came from.
#[derive(Clone)]
enum DatasetOrigin {
    Embedded,
    File(PathBuf),
}

#[derive(Clone)]
struct DatasetSource {
    dataset: MaterialDataset,
    origin: DatasetOrigin,
    /// Issuer signature carried by a v2 bundle — `None` for embedded
    /// datasets and unsigned bundles. Verified only via `optcoil dataset
    /// verify`; shown here as provenance, not as a verified claim.
    attestation: Option<optcoil_model::attestation::DatasetAttestation>,
    bundle_json: Option<String>,
}

/// Resolve a coupled-search case's declared dataset: the embedded store
/// first, then a bundle beside the case file (`datasets/<id>.json` or
/// `<id>.json` in the case's directory). `None` means the declared
/// dataset is not available locally — the run would fail until the user
/// loads a bundle from the Materials page.
fn resolve_dataset(case: &CoupledSearchCase, case_path: &Path) -> Option<DatasetSource> {
    #[cfg(target_arch = "wasm32")]
    let _ = case_path; // no sibling-directory lookup in the browser
    if let Ok(dataset) = MaterialDataset::embedded_by_id(&case.material.dataset_id) {
        return Some(DatasetSource {
            dataset,
            origin: DatasetOrigin::Embedded,
            attestation: None,
            bundle_json: None,
        });
    }
    // Sibling-file lookup exists only natively — the browser receives a
    // case as picker/drop bytes with no surrounding directory.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = case_path.parent()?;
        for candidate in [
            dir.join("datasets")
                .join(format!("{}.json", case.material.dataset_id)),
            dir.join(format!("{}.json", case.material.dataset_id)),
        ] {
            if let Ok(text) = fs::read_to_string(&candidate)
                && let Ok(bundle) = MaterialBundle::from_json(&text)
                && case.validate_against_dataset(&bundle.dataset).is_ok()
            {
                return Some(DatasetSource {
                    dataset: bundle.dataset,
                    origin: DatasetOrigin::File(candidate),
                    attestation: bundle.attestation,
                    bundle_json: Some(text),
                });
            }
        }
    }
    None
}
struct Worker {
    receiver: Receiver<Result<JobResult, String>>,
    cancel: Arc<AtomicBool>,
    kind: JobKind,
    /// The browser `Worker` a wasm search runs on — terminate() is the
    /// only cancel it can honor (a blocked wasm thread cannot poll a
    /// flag). Unused on native.
    #[cfg(target_arch = "wasm32")]
    web_worker: Option<web_sys::Worker>,
    #[cfg(target_arch = "wasm32")]
    _web_callbacks: Option<BrowserWorkerCallbacks>,
}
#[cfg(target_arch = "wasm32")]
struct BrowserWorkerCallbacks {
    _message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _error: wasm_bindgen::closure::Closure<dyn FnMut(wasm_bindgen::JsValue)>,
}
#[cfg(target_arch = "wasm32")]
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(worker) = &self.web_worker {
            worker.set_onmessage(None);
            worker.set_onerror(None);
            worker.terminate();
        }
    }
}
struct Workbench {
    case: Case,
    baseline: Assessment,
    record: Option<RunRecord>,
    /// When `Some`, the workbench is in coupled-search mode: every page
    /// renders the coupled case/record instead of the allocation project.
    search_case: Option<CoupledSearchCase>,
    /// Raw bytes of the opened coupled-search case (hashed by the runner).
    /// Empty for a record-only view, which disables re-running.
    search_json: String,
    /// When set, a completed coupled search chains straight into the
    /// pilot-bundle export prompt — the author → run → package flow as
    /// one guided job.
    package_after_search: bool,
    /// The case's declared material dataset, resolved at open (embedded or
    /// a sibling bundle) or picked explicitly from the Materials page.
    search_dataset: Option<DatasetSource>,
    /// Additional external sources used by graded tape-spec bindings,
    /// keyed by their actual metadata dataset id. Embedded sources are
    /// resolved by the search runner and need not be duplicated here.
    search_spec_datasets: std::collections::BTreeMap<String, DatasetSource>,
    preflight: Option<optcoil_search::preflight::StudyPreflight>,
    /// The coupled-search case builder window, while open.
    author: Option<author::CaseDraft>,
    material_import: Option<importer::ImportDraft>,
    pending_import_bundle: Option<String>,
    recovery: recovery_coordinator::RecoveryRuntime,
    edit_variant: Option<String>,
    importing_workspace: bool,
    launch_after_variant: bool,
    search_record: Option<CoupledSearchRunRecord>,
    selected_candidate: usize,
    worker: Option<Worker>,
    #[cfg(target_arch = "wasm32")]
    browser_search_worker: Option<web_sys::Worker>,
    /// Live field-evaluation progress for a running coupled search —
    /// shared with the worker; cleared when the job lands.
    search_progress: Option<Arc<SearchProgress>>,
    path: Option<PathBuf>,
    logo: egui::TextureHandle,
    max_evaluations: u64,
    page: Page,
    selected_module: usize,
    module_filter: String,
    sort_by_field: bool,
    show_baseline: bool,
    unresolved_only: bool,
    show_inspector: bool,
    plot_revision: u64,
    help: bool,
    /// Current guided-tour step; `None` when the tour is closed.
    tour: Option<usize>,
    /// Display-side conductor price override (reprice slider). `None` shows
    /// the record's declared price; a set value reprices every shown cost —
    /// verdicts and margins are price-independent so only dollars move.
    reprice_usd_per_m: Option<f64>,
    /// Strand slice shown on the search-space map when the case's
    /// `strands_parallel` choice list has more than one value.
    map_strand: usize,
    /// Geometry-axis slices on the search-space map — v21 cases declare
    /// bend-radius / straight-length choice lists, so the map needs one
    /// selector per declared axis to key cells unambiguously.
    map_bend: usize,
    map_straight: usize,
    /// Most-recently-opened case/record paths (MRU-first, capped) —
    /// persisted to the user config dir so the next launch starts with
    /// the library already warm.
    recent_files: Vec<PathBuf>,
    /// Opt-in case-library folder — shallow-scanned for optcoil-schema
    /// JSON. Bounded by construction: the user picks the directory, the
    /// app never crawls the filesystem.
    library_dir: Option<PathBuf>,
    library_entries: Vec<PathBuf>,
    /// Pinned candidate for A/B comparison against the current selection.
    compare_pin: Option<usize>,
    /// Watch mode — reload the open case file when its on-disk mtime
    /// moves. The fast loop for editing a case JSON alongside the GUI.
    watch_enabled: bool,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    watch_mtime: Option<std::time::SystemTime>,
    /// Sequential run queue — picked case files run one after another;
    /// each record is written under runs/ before the next case loads.
    run_queue: Vec<PathBuf>,
    queue_active: bool,
    /// A second run record opened read-only for the Reports compare card.
    compare_record: Option<CoupledSearchRunRecord>,
    /// On-demand bore B_z profile for the selected candidate — computed
    /// by the physics evaluator on a worker, keyed by candidate index.
    field_profile: Option<(usize, Vec<[f64; 2]>)>,
    /// Screenshot target — set by the save dialog, consumed when the
    /// viewport Screenshot event delivers the frame image.
    screenshot_target: Option<PathBuf>,
    /// egui context clone — artifact jobs (verify/sweep/bakeoff) launch
    /// from apply_result, which has no Ui to borrow a context from.
    ctx: egui::Context,
    /// Serialized JSON of the loaded coupled-search record — BOM,
    /// gradereport and verify all re-parse the record artifact itself.
    search_record_json: String,
    study_workspace: StudyWorkspace,
    study_session: StudyEngineSession,
    study_variant_id: Option<String>,
    study_compare_left: Option<String>,
    study_compare_right: Option<String>,
    study_follow_up: Option<optcoil_search::study::FollowUpExperiment>,
    study_summary: Option<optcoil_search::study::StudySummary>,
    study_diff: Option<optcoil_search::study::StudyDiff>,
    study_compare_summary: Option<(
        optcoil_search::study::StudySummary,
        optcoil_search::study::StudySummary,
    )>,
    robustness_variant_ids: Vec<String>,
    robustness_scenarios: Vec<optcoil_search::robustness::RobustnessScenario>,
    robustness_preflight: Option<optcoil_search::robustness::RobustnessPreflight>,
    robustness_history_current: Vec<(String, bool)>,
    /// Decision artifacts derived from the loaded record — computed once
    /// on arrival, not per frame.
    bom_record: Option<BomRecord>,
    decision_summary: Option<optcoil_search::review::DecisionSummary>,
    grade_report: Option<GradingReport>,
    verify_checks: Option<Vec<CheckLine>>,
    sweep_record: Option<SensitivitySweepRecord>,
    bakeoff_record: Option<BakeoffRecord>,
    message: (bool, String),
    capture: Option<capture::Capture>,
    suite: Suite,
    page_fade_start: Instant,
    last_page: Page,
    results_reveal_start: Option<Instant>,
    inspector_fade_start: Instant,
    last_selected_module: usize,
    last_selected_candidate: usize,
    message_fade_start: Instant,
    drop_hint_start: Option<Instant>,
}

impl Workbench {
    fn resolved_search_datasets(&self) -> std::collections::BTreeMap<String, MaterialDataset> {
        let mut datasets = std::collections::BTreeMap::new();
        if let Some(source) = &self.search_dataset
            && matches!(source.origin, DatasetOrigin::File(_))
        {
            datasets.insert(source.dataset.metadata.id.clone(), source.dataset.clone());
        }
        for (id, source) in &self.search_spec_datasets {
            datasets.insert(id.clone(), source.dataset.clone());
        }
        datasets
    }

    fn dataset_source(&self, dataset_id: &str) -> Option<&DatasetSource> {
        self.search_spec_datasets.get(dataset_id).or_else(|| {
            self.search_dataset
                .as_ref()
                .filter(|source| source.dataset.metadata.id == dataset_id)
        })
    }

    fn robustness_dataset_ids(&self) -> Vec<String> {
        let mut ids = std::collections::BTreeSet::new();
        for id in &self.robustness_variant_ids {
            if let Ok(variant) = self.study_workspace.variant(id)
                && let Ok(case) = CoupledSearchCase::from_json(&variant.case_json)
            {
                ids.extend(
                    case.material_bindings()
                        .into_iter()
                        .map(|(_, binding)| binding.dataset_id.clone()),
                );
            }
        }
        ids.into_iter().collect()
    }

    fn sync_robustness_editor(&mut self) {
        if self.robustness_variant_ids.is_empty()
            && self.robustness_scenarios.is_empty()
            && let Some(id) = self.study_variant_id.clone()
            && self.study_workspace.variant(&id).is_ok()
        {
            self.robustness_variant_ids.push(id);
        }
        let dataset_ids = self.robustness_dataset_ids();
        if self.robustness_scenarios.is_empty() {
            let mut price_multipliers = std::collections::BTreeMap::new();
            let mut ic_multipliers = std::collections::BTreeMap::new();
            for dataset_id in &dataset_ids {
                price_multipliers.insert(dataset_id.clone(), 1.1);
                ic_multipliers.insert(dataset_id.clone(), 1.0);
            }
            self.robustness_scenarios
                .push(optcoil_search::robustness::RobustnessScenario {
                    id: "scenario-1".into(),
                    name: "Supplier price +10%".into(),
                    price_multipliers,
                    ic_multipliers,
                    temperature_offset_k: 0.0,
                });
        }
        for scenario in &mut self.robustness_scenarios {
            scenario
                .price_multipliers
                .retain(|dataset_id, _| dataset_ids.contains(dataset_id));
            scenario
                .ic_multipliers
                .retain(|dataset_id, _| dataset_ids.contains(dataset_id));
            for dataset_id in &dataset_ids {
                scenario
                    .price_multipliers
                    .entry(dataset_id.clone())
                    .or_insert(1.1);
                scenario
                    .ic_multipliers
                    .entry(dataset_id.clone())
                    .or_insert(1.0);
            }
        }
    }

    fn robustness_spec(&self) -> optcoil_search::robustness::RobustnessSpec {
        let mut scenarios = vec![optcoil_search::robustness::RobustnessScenario::nominal()];
        scenarios.extend(self.robustness_scenarios.iter().cloned());
        optcoil_search::robustness::RobustnessSpec {
            schema: optcoil_search::robustness::ROBUSTNESS_SPEC_SCHEMA.into(),
            scenarios,
        }
    }

    fn refresh_robustness_history_current(&mut self) {
        self.robustness_history_current = self
            .study_workspace
            .robustness_results
            .iter()
            .map(|record| {
                let current = optcoil_search::robustness::robustness_record_binding_is_current(
                    &self.study_workspace,
                    record,
                )
                .unwrap_or(false);
                (record.input_fingerprint.clone(), current)
            })
            .collect();
    }

    fn preview_robustness(&mut self) {
        self.sync_robustness_editor();
        let spec = self.robustness_spec();
        match optcoil_search::robustness::robustness_preflight(
            &self.study_workspace,
            &self.robustness_variant_ids,
            &spec,
        ) {
            Ok(preflight) => self.robustness_preflight = Some(preflight),
            Err(error) => {
                self.robustness_preflight = None;
                self.message = (true, error.to_string());
            }
        }
    }

    fn start_robustness(&mut self, ctx: &egui::Context) {
        #[cfg(target_arch = "wasm32")]
        if self.robustness_variant_ids.iter().any(|id| {
            self.study_workspace
                .variant(id)
                .is_ok_and(|variant| variant.options.threads != Some(1))
        }) {
            self.message = (true, "Browser scenarios use one thread. Review the explicit browser override for each selected variant before running.".into());
            return;
        }
        let Some(preflight) = self.robustness_preflight.as_ref() else {
            self.message = (
                true,
                "Preview the what-if workload before running it.".into(),
            );
            return;
        };
        if !preflight.ready_to_run {
            self.message = (
                true,
                "Resolve every scenario preflight issue before running.".into(),
            );
            return;
        }
        let workspace = self.study_workspace.clone();
        let ids = self.robustness_variant_ids.clone();
        let spec = self.robustness_spec();
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.launch(ctx, JobKind::Robustness, move |cancel| {
                let mut workspace = workspace;
                let record = optcoil_search::robustness::run_study_robustness(
                    &workspace, &ids, &spec, &cancel, None,
                )
                .map_err(|error| error.to_string())?;
                workspace
                    .attach_robustness_result(record)
                    .map_err(|error| error.to_string())?;
                Ok(JobResult::RobustnessCompleted(Box::new(workspace)))
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let workspace_json = match workspace.to_json() {
                Ok(json) => json,
                Err(error) => {
                    self.message = (true, error.to_string());
                    return;
                }
            };
            self.launch_robustness_worker(ctx, workspace_json, ids, spec);
        }
    }

    fn export_robustness_record(
        &mut self,
        ctx: &egui::Context,
        record: optcoil_search::robustness::RobustnessStudyRecord,
    ) {
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, JobKind::Export, move |_| {
            let json = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
            let Some(path) = rfd::FileDialog::new()
                .set_title("Export robustness study JSON")
                .add_filter("Robustness study", &["json"])
                .set_file_name("converra-robustness-study.json")
                .save_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("Export failed: {e}. Choose a new filename."))?;
            file.write_all(json.as_bytes())
                .map_err(|e| format!("Export failed: {e}"))?;
            Ok(JobResult::RobustnessExported(path))
        });
        #[cfg(target_arch = "wasm32")]
        {
            let json = serde_json::to_string_pretty(&record);
            self.launch_wasm(ctx, JobKind::Export, async move {
                let json = json.map_err(|e| e.to_string())?;
                web::download_bytes("converra-robustness-study.json", json.as_bytes());
                Ok(JobResult::RobustnessExported(PathBuf::from(
                    "converra-robustness-study.json",
                )))
            });
        }
    }

    fn install_workspace_bundles(&mut self, bundles: &[String]) {
        let base_id = self
            .search_case
            .as_ref()
            .map(|case| case.material.dataset_id.clone());
        self.search_dataset = None;
        self.search_spec_datasets.clear();
        for json in bundles {
            let Ok(bundle) = MaterialBundle::from_json(json) else {
                continue;
            };
            let id = bundle.dataset.metadata.id.clone();
            let source = DatasetSource {
                dataset: bundle.dataset,
                origin: DatasetOrigin::File(PathBuf::from(format!("workspace/{id}.json"))),
                attestation: bundle.attestation,
                bundle_json: Some(json.clone()),
            };
            if base_id.as_deref() == Some(id.as_str()) {
                self.search_dataset = Some(source);
            } else {
                self.search_spec_datasets.insert(id, source);
            }
        }
        self.refresh_preflight();
    }

    /// Retain only external sources whose dataset id and CSV hash still
    /// satisfy a material binding in the accepted case, then discover
    /// matching sibling bundles on native builds.
    fn resolve_case_datasets(&mut self, case: &CoupledSearchCase, path: &Path) {
        let bindings = case.material_bindings();
        let is_matching = |source: &DatasetSource| {
            let id = &source.dataset.metadata.id;
            bindings.iter().any(|(_, material)| {
                material.dataset_id == *id
                    && material.csv_sha256 == source.dataset.metadata.csv_sha256
            })
        };
        let mut candidates = std::mem::take(&mut self.search_spec_datasets);
        if let Some(base) = self.search_dataset.take() {
            let id = base.dataset.metadata.id.clone();
            let preserve_external = candidates
                .get(&id)
                .is_some_and(|source| matches!(source.origin, DatasetOrigin::File(_)))
                && matches!(base.origin, DatasetOrigin::Embedded);
            if !preserve_external {
                candidates.insert(id, base);
            }
        }
        candidates.retain(|_, source| is_matching(source));

        let mut sources = std::collections::BTreeMap::new();
        let base_id = case.material.dataset_id.as_str();
        let base = candidates
            .remove(base_id)
            .or_else(|| resolve_dataset(case, path));
        for (_, material) in bindings {
            let id = material.dataset_id.as_str();
            if id == base_id || sources.contains_key(id) {
                continue;
            }
            if let Some(source) = candidates.remove(id) {
                sources.insert(id.to_owned(), source);
                continue;
            }
            if MaterialDataset::embedded_by_id(id).is_ok() {
                continue;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(dir) = path.parent() {
                let mut found = None;
                for candidate in [
                    dir.join("datasets").join(format!("{id}.json")),
                    dir.join(format!("{id}.json")),
                ] {
                    if let Ok(text) = fs::read_to_string(&candidate)
                        && let Ok(bundle) = MaterialBundle::from_json(&text)
                        && bundle.dataset.metadata.id == id
                        && bundle.dataset.metadata.csv_sha256 == material.csv_sha256
                    {
                        found = Some(DatasetSource {
                            dataset: bundle.dataset,
                            origin: DatasetOrigin::File(candidate),
                            attestation: bundle.attestation,
                            bundle_json: Some(text),
                        });
                        break;
                    }
                }
                if let Some(source) = found {
                    sources.insert(id.to_owned(), source);
                }
            }
        }
        self.search_dataset = base;
        self.search_spec_datasets = sources;
    }

    fn unresolved_dataset_bindings(&self) -> Vec<String> {
        let Some(case) = &self.search_case else {
            return Vec::new();
        };
        case.material_bindings()
            .into_iter()
            .filter_map(|(binding, material)| {
                let cached = self.preflight.as_ref().and_then(|preflight| {
                    preflight
                        .datasets
                        .iter()
                        .find(|dataset| dataset.binding_id == binding)
                });
                let available = cached.map_or_else(
                    || {
                        self.dataset_source(&material.dataset_id)
                            .is_some_and(|source| {
                                source.dataset.metadata.csv_sha256 == material.csv_sha256
                            })
                            || MaterialDataset::embedded_by_id(&material.dataset_id).is_ok_and(
                                |dataset| dataset.metadata.csv_sha256 == material.csv_sha256,
                            )
                    },
                    |dataset| dataset.available && dataset.identity_compatible == Some(true),
                );
                if available {
                    None
                } else {
                    Some(format!("{binding} → {}", material.dataset_id))
                }
            })
            .collect()
    }

    fn new(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        brand::configure(ctx);
        let case = Case::demo()?;
        let baseline = acceptance::assess(&case, &case.baseline)?;
        let mut app = Self {
            case,
            baseline,
            logo: brand::logo(ctx)?,
            record: None,
            search_case: None,
            search_json: String::new(),
            package_after_search: false,
            search_dataset: None,
            search_spec_datasets: std::collections::BTreeMap::new(),
            preflight: None,
            author: None,
            material_import: None,
            pending_import_bundle: None,
            recovery: recovery_coordinator::RecoveryRuntime::new(),
            edit_variant: None,
            importing_workspace: false,
            launch_after_variant: false,
            search_record: None,
            selected_candidate: 0,
            worker: None,
            #[cfg(target_arch = "wasm32")]
            browser_search_worker: None,
            search_progress: None,
            path: None,
            max_evaluations: 100_000,
            page: Page::Overview,
            selected_module: 0,
            module_filter: String::new(),
            sort_by_field: true,
            show_baseline: true,
            unresolved_only: false,
            show_inspector: true,
            plot_revision: 0,
            help: false,
            tour: None,
            reprice_usd_per_m: None,
            map_strand: 0,
            map_bend: 0,
            map_straight: 0,
            recent_files: Vec::new(),
            library_dir: None,
            library_entries: Vec::new(),
            compare_pin: None,
            watch_enabled: false,
            watch_mtime: None,
            run_queue: Vec::new(),
            queue_active: false,
            compare_record: None,
            field_profile: None,
            screenshot_target: None,
            ctx: ctx.clone(),
            search_record_json: String::new(),
            study_workspace: StudyWorkspace::new("Engineering study"),
            study_session: StudyEngineSession::default(),
            study_variant_id: None,
            study_compare_left: None,
            study_compare_right: None,
            study_follow_up: None,
            study_summary: None,
            study_diff: None,
            study_compare_summary: None,
            robustness_variant_ids: Vec::new(),
            robustness_scenarios: Vec::new(),
            robustness_preflight: None,
            robustness_history_current: Vec::new(),
            bom_record: None,
            decision_summary: None,
            grade_report: None,
            verify_checks: None,
            sweep_record: None,
            bakeoff_record: None,
            message: (
                false,
                "Open a project, drop a case file here, or run the bundled example.".into(),
            ),
            capture: capture::Capture::from_env(),
            suite: Suite::new(ctx, "converra"),
            page_fade_start: Instant::now(),
            last_page: Page::Overview,
            results_reveal_start: None,
            inspector_fade_start: Instant::now(),
            last_selected_module: 0,
            last_selected_candidate: 0,
            message_fade_start: Instant::now(),
            drop_hint_start: None,
        };
        app.open_example(ctx, true);
        let (recent_files, library_dir) = load_persisted();
        app.recent_files = recent_files;
        app.library_dir = library_dir;
        app.rescan_library();
        // OPTCOIL_PROJECT opens a case or run record at launch — used by
        // demos and the capture harness instead of a file dialog.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = std::env::var_os("OPTCOIL_PROJECT") {
            let result = load_project(PathBuf::from(path));
            app.apply_result(ctx, result);
        }
        if app.capture.is_some() {
            app.start(ctx);
        }
        app.initialize_recovery();
        Ok(app)
    }

    fn open_example(&mut self, ctx: &egui::Context, measured: bool) {
        let json = if measured {
            include_str!("../../../benchmarks/coupled/first-study.json").to_owned()
        } else {
            match serde_json::to_string_pretty(
                &Case::demo().expect("embedded synthetic case is valid"),
            ) {
                Ok(json) => json,
                Err(error) => {
                    self.message = (true, error.to_string());
                    return;
                }
            }
        };
        let name = if measured {
            "first-study.json"
        } else {
            "synthetic-allocation.json"
        };
        self.apply_result(ctx, load_project_json(json, PathBuf::from(name)));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn attach_source_case(&mut self, ctx: &egui::Context) {
        self.launch(ctx, JobKind::Open, move |_| {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Attach the original case used by this run")
                .add_filter("Case JSON", &["json"])
                .pick_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            let json = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            Ok(JobResult::SourceCaseLoaded(json, path))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn attach_source_case(&mut self, ctx: &egui::Context) {
        self.launch_wasm(ctx, JobKind::Open, async {
            let Some((name, bytes)) = web::pick_bytes(&["json"]).await? else {
                return Ok(JobResult::Dismissed);
            };
            let json = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            Ok(JobResult::SourceCaseLoaded(json, PathBuf::from(name)))
        });
    }

    fn refresh_preflight(&mut self) {
        self.preflight = self.search_case.as_ref().map(|case| {
            let datasets = self.resolved_search_datasets();
            optcoil_search::preflight::preflight_coupled_search(
                case,
                Some(&datasets),
                &optcoil_search::preflight::PreflightOptions {
                    threads: cfg!(target_arch = "wasm32").then_some(1),
                },
            )
        });
    }

    fn edit_case(&mut self, duplicate: bool) {
        let Some(_) = self.search_case.as_ref() else {
            return;
        };
        let draft = if duplicate {
            self.edit_variant = None;
            author::CaseDraft::duplicate_json(&self.search_json)
        } else {
            self.edit_variant = self.study_variant_id.clone();
            let json = self.case_json().unwrap_or_default();
            author::CaseDraft::revision(&json)
        };
        match draft {
            Ok(mut draft) => {
                let cached_materials = self
                    .all_dataset_sources()
                    .into_values()
                    .map(|source| {
                        (
                            source.dataset.metadata.id.clone(),
                            source.dataset.metadata.csv_sha256.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                draft.set_material_choices(cached_materials);
                self.author = Some(draft);
            }
            Err(error) => self.message = (true, error),
        }
    }

    /// Bind the current loaded case and its exact external bundle bytes to
    /// the workspace. A revision updates its named variant while retaining
    /// historical results; a new or duplicate case gets a new variant.
    fn register_active_variant(&mut self) {
        if self.importing_workspace {
            return;
        }
        let Some(case_json) = self.case_json() else {
            return;
        };
        let bundles = self
            .all_dataset_sources()
            .into_values()
            .filter_map(|source| source.bundle_json)
            .collect::<Vec<_>>();
        let selected = self.edit_variant.take().or_else(|| {
            self.study_workspace
                .variants
                .iter()
                .find(|v| v.case_json == case_json && v.dataset_bundles == bundles)
                .map(|v| v.id.clone())
        });
        if let Some(id) = selected
            && self.study_workspace.variant(&id).is_ok()
        {
            let options = self
                .study_workspace
                .variant(&id)
                .map(|v| v.options)
                .unwrap_or_default();
            if self
                .study_workspace
                .revise_variant(&id, case_json.clone(), bundles.clone(), options)
                .is_ok()
            {
                self.study_variant_id = Some(id.clone());
                let _ = self.study_workspace.select_variant(&id);
                self.robustness_preflight = None;
                self.refresh_robustness_history_current();
                return;
            }
        }
        let name = self
            .search_case
            .as_ref()
            .map_or_else(|| "Study variant".into(), |case| case.id.clone());
        if let Ok(id) = self.study_workspace.add_variant(
            name,
            case_json,
            bundles.clone(),
            CoupledSearchOptions {
                threads: cfg!(target_arch = "wasm32").then_some(1),
            },
        ) {
            self.study_variant_id = Some(id);
            self.robustness_preflight = None;
            self.refresh_robustness_history_current();
        }
    }

    fn launch(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<JobResult, String> + Send + 'static,
    ) {
        if self.worker.is_some() {
            return;
        }
        // Numerical jobs must use a Web Worker in the browser. Refuse an
        // accidental inline dispatch before it can block the page's event loop.
        #[cfg(target_arch = "wasm32")]
        if matches!(
            kind,
            JobKind::Search
                | JobKind::Sweep
                | JobKind::Bakeoff
                | JobKind::Profile
                | JobKind::Robustness
                | JobKind::Import
        ) {
            self.message = (true, "This calculation requires a browser worker.".into());
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        thread::spawn(move || {
            let _ = sender.send(work(worker_cancel));
            ctx.request_repaint();
        });
        // Cheap browser jobs (open, verify, export) run inline. Numerical
        // jobs dispatch through worker.js instead.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = sender.send(work(worker_cancel));
            ctx.request_repaint();
        }
        self.worker = Some(Worker {
            receiver,
            cancel,
            kind,
            #[cfg(target_arch = "wasm32")]
            web_worker: None,
            #[cfg(target_arch = "wasm32")]
            _web_callbacks: None,
        });
    }

    /// The browser search path: spawn `worker.js` — a second wasm
    /// instance inside a Web Worker — hand it the case (and any loaded
    /// dataset) over postMessage, and deliver its reply through the
    /// same channel a native thread uses. Cancel = `terminate()`.
    #[cfg(target_arch = "wasm32")]
    fn launch_search_worker(
        &mut self,
        ctx: &egui::Context,
        workspace_json: String,
        variant_id: String,
    ) {
        use wasm_bindgen::JsCast as _;
        if self.worker.is_some() {
            return;
        }
        let web_worker = if let Some(worker) = self.browser_search_worker.take() {
            worker
        } else {
            let options = web_sys::WorkerOptions::new();
            options.set_type(web_sys::WorkerType::Module);
            match web_sys::Worker::new_with_options("./worker.js", &options) {
                Ok(w) => w,
                Err(e) => {
                    self.message = (
                        true,
                        format!(
                            "could not start the search worker ({:?}) — serve the built bundle, or use the desktop build",
                            e
                        ),
                    );
                    self.package_after_search = false;
                    return;
                }
            }
        };
        let (sender, receiver) = mpsc::channel();
        let error_sender = sender.clone();
        let error_ctx = ctx.clone();
        let reply_ctx = ctx.clone();
        let onmessage =
            wasm_bindgen::closure::Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                let result = event
                    .data()
                    .as_string()
                    .ok_or_else(|| "worker replied with a non-string message".to_owned())
                    .and_then(|text| {
                        let payload: serde_json::Value =
                            serde_json::from_str(&text).map_err(|e| e.to_string())?;
                        match payload.get("status").and_then(|s| s.as_str()) {
                            Some("ok") => {
                                let record: CoupledSearchRunRecord = serde_json::from_str(
                                    payload["record"].as_str().unwrap_or_default(),
                                )
                                .map_err(|e| e.to_string())?;
                                Ok(JobResult::SearchCompleted(Box::new(record)))
                            }
                            Some("study_ok") => {
                                let workspace = optcoil_search::study::StudyWorkspace::from_json(
                                    payload["workspace_json"].as_str().unwrap_or_default(),
                                )
                                .map_err(|e| e.to_string())?;
                                let record = serde_json::from_str(
                                    payload["record"].as_str().unwrap_or_default(),
                                )
                                .map_err(|e| e.to_string())?;
                                let cache_hit = payload["cache_hit"].as_bool().unwrap_or(false);
                                Ok(JobResult::BrowserStudySearchCompleted(Box::new((
                                    workspace, record, cache_hit,
                                ))))
                            }
                            _ => Err(payload["error"]
                                .as_str()
                                .unwrap_or("worker error")
                                .to_owned()),
                        }
                    });
                let _ = sender.send(result);
                reply_ctx.request_repaint();
            }) as Box<dyn FnMut(_)>);
        web_worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        let onerror = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |_event: wasm_bindgen::JsValue| {
                let _ = error_sender.send(Err("Search worker failed. Check the browser console and served worker bundle, then retry; the previous result is retained.".into()));
                error_ctx.request_repaint();
            },
        ) as Box<dyn FnMut(_)>);
        web_worker.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        let payload = serde_json::json!({
            "kind": "study-search",
            "workspace_json": workspace_json,
            "variant_id": variant_id,
        });
        if let Err(error) =
            web_worker.post_message(&wasm_bindgen::JsValue::from_str(&payload.to_string()))
        {
            web_worker.set_onmessage(None);
            web_worker.set_onerror(None);
            web_worker.terminate();
            self.package_after_search = false;
            self.message = (
                true,
                format!("Could not send the case to the search worker: {error:?}"),
            );
            return;
        }
        self.worker = Some(Worker {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            kind: JobKind::Search,
            web_worker: Some(web_worker),
            _web_callbacks: Some(BrowserWorkerCallbacks {
                _message: onmessage,
                _error: onerror,
            }),
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn launch_robustness_worker(
        &mut self,
        ctx: &egui::Context,
        workspace_json: String,
        variant_ids: Vec<String>,
        spec: optcoil_search::robustness::RobustnessSpec,
    ) {
        self.launch_analysis_worker(
            ctx,
            JobKind::Robustness,
            "What-if",
            serde_json::json!({
                "kind": "study-robustness",
                "workspace_json": workspace_json,
                "variant_ids": variant_ids,
                "spec": spec,
            }),
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn launch_analysis_worker(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        label: &'static str,
        payload: serde_json::Value,
    ) {
        self.launch_analysis_worker_with_bytes(ctx, kind, label, payload, None);
    }

    #[cfg(target_arch = "wasm32")]
    fn launch_analysis_worker_with_bytes(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        label: &'static str,
        payload: serde_json::Value,
        bytes: Option<Vec<u8>>,
    ) {
        use wasm_bindgen::JsCast as _;
        if self.worker.is_some() {
            return;
        }
        let web_worker = if let Some(worker) = self.browser_search_worker.take() {
            worker
        } else {
            let options = web_sys::WorkerOptions::new();
            options.set_type(web_sys::WorkerType::Module);
            match web_sys::Worker::new_with_options("./worker.js", &options) {
                Ok(worker) => worker,
                Err(error) => {
                    self.message = (
                        true,
                        format!(
                            "Could not start the {label} worker ({error:?}). Serve the built bundle, then retry."
                        ),
                    );
                    return;
                }
            }
        };
        let (sender, receiver) = mpsc::channel();
        let error_sender = sender.clone();
        let error_ctx = ctx.clone();
        let reply_ctx = ctx.clone();
        let onmessage =
            wasm_bindgen::closure::Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                let result = event
                    .data()
                    .as_string()
                    .ok_or_else(|| "worker replied with a non-string message".to_owned())
                    .and_then(|text| {
                        let payload: serde_json::Value =
                            serde_json::from_str(&text).map_err(|e| e.to_string())?;
                        match (kind, payload.get("status").and_then(|v| v.as_str())) {
                            (JobKind::Robustness, Some("robustness_ok")) => {
                                serde_json::from_str(payload["record"].as_str().unwrap_or_default())
                                    .map(|record| {
                                        JobResult::BrowserRobustnessCompleted(Box::new(record))
                                    })
                                    .map_err(|e| e.to_string())
                            }
                            (JobKind::Sweep, Some("sweep_ok")) => {
                                serde_json::from_str(payload["record"].as_str().unwrap_or_default())
                                    .map(|record| JobResult::SweepDone(Box::new(record)))
                                    .map_err(|e| e.to_string())
                            }
                            (JobKind::Import, Some("table_preview_ok")) => {
                                serde_json::from_value(payload["preview"].clone())
                                    .map(|preview| JobResult::TablePreview(Box::new(preview)))
                                    .map_err(|e| e.to_string())
                            }
                            (JobKind::Import, Some("material_import_ok")) => {
                                MaterialBundle::from_json(
                                    payload["bundle_json"].as_str().unwrap_or_default(),
                                )
                                .map(|bundle| JobResult::MaterialImported(Box::new(bundle)))
                                .map_err(|e| e.to_string())
                            }
                            _ => Err(payload["error"]
                                .as_str()
                                .unwrap_or("analysis worker error")
                                .to_owned()),
                        }
                    });
                let _ = sender.send(result);
                reply_ctx.request_repaint();
            }) as Box<dyn FnMut(_)>);
        web_worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        let onerror = wasm_bindgen::closure::Closure::wrap(Box::new(
            move |_event: wasm_bindgen::JsValue| {
                let _ = error_sender.send(Err(
                    format!("{label} worker failed. Check the browser console and served worker bundle; prior results are retained."),
                ));
                error_ctx.request_repaint();
            },
        ) as Box<dyn FnMut(_)>);
        web_worker.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        let dispatch = if let Some(bytes) = bytes {
            // Transfer table bytes directly; formatting millions of byte
            // values as JSON would stall the main browser thread.
            let message = js_sys::Object::new();
            let buffer = js_sys::Uint8Array::from(bytes.as_slice());
            let transfer = js_sys::Array::new();
            transfer.push(&buffer.buffer());
            js_sys::Reflect::set(&message, &"payload".into(), &payload.to_string().into())
                .and_then(|_| js_sys::Reflect::set(&message, &"bytes".into(), &buffer))
                .and_then(|_| web_worker.post_message_with_transfer(&message, &transfer))
        } else {
            web_worker.post_message(&wasm_bindgen::JsValue::from_str(&payload.to_string()))
        };
        if let Err(error) = dispatch {
            web_worker.set_onmessage(None);
            web_worker.set_onerror(None);
            web_worker.terminate();
            self.message = (true, format!("Could not send the {label} run: {error:?}"));
            return;
        }
        self.worker = Some(Worker {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            kind,
            web_worker: Some(web_worker),
            _web_callbacks: Some(BrowserWorkerCallbacks {
                _message: onmessage,
                _error: onerror,
            }),
        });
    }

    /// WASM-only counterpart for file dialogs: `rfd::AsyncFileDialog`
    /// is a future on the browser's event loop, not a blocking call —
    /// the JobResult arrives through the same channel a worker uses.
    #[cfg(target_arch = "wasm32")]
    fn launch_wasm(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        future: impl std::future::Future<Output = Result<JobResult, String>> + 'static,
    ) {
        if self.worker.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = sender.send(future.await);
            ctx.request_repaint();
        });
        self.worker = Some(Worker {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            kind,
            web_worker: None,
            _web_callbacks: None,
        });
    }

    fn cancel_search(&mut self) {
        let Some(worker) = self.worker.as_ref().filter(|w| {
            matches!(
                w.kind,
                JobKind::Search | JobKind::Robustness | JobKind::Sweep | JobKind::Import
            )
        }) else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        let kind = worker.kind;
        worker.cancel.store(true, Ordering::Relaxed);
        self.package_after_search = false;
        self.queue_active = false;
        #[cfg(target_arch = "wasm32")]
        {
            // Termination cannot deliver a reply. Drop the owned callbacks and
            // channel explicitly so cancellation makes the workbench usable.
            self.worker = None;
            self.search_progress = None;
            self.message = (
                false,
                match kind {
                    JobKind::Robustness => {
                        "What-if run cancelled. Earlier robustness results are retained.".into()
                    }
                    JobKind::Sweep => {
                        "Margin sweep cancelled. The previous completed sweep is retained.".into()
                    }
                    JobKind::Import => {
                        "Import cancelled. The active case and completed evidence are retained."
                            .into()
                    }
                    _ => "Search cancelled. The previous completed result is retained.".into(),
                },
            );
            self.message_fade_start = Instant::now();
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.worker.is_some() {
            return;
        }
        if let Some(preflight) = &self.preflight
            && !preflight.ready_to_run
        {
            self.message = (
                true,
                preflight
                    .errors
                    .iter()
                    .map(|item| {
                        format!(
                            "{} {}",
                            item.message,
                            item.correction.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            return;
        }
        // The browser build runs the search on a Web Worker holding a
        // second copy of the engine — the page thread stays free and
        // Cancel terminates the worker. Single-threaded wasm is ~2×
        // slower than the desktop build; fine for real cases.
        #[cfg(target_arch = "wasm32")]
        {
            if self.search_case.is_some() {
                if let Some(id) = self.study_variant_id.as_ref()
                    && self
                        .study_workspace
                        .variant(id)
                        .is_ok_and(|variant| variant.options.threads != Some(1))
                {
                    self.message = (true, "Browser worker uses one thread. Review the explicit override on the Engineering study page before running this imported variant.".into());
                    return;
                }
                if self.search_json.is_empty() {
                    return;
                }
                let missing = self.unresolved_dataset_bindings();
                if !missing.is_empty() {
                    self.message = (
                        true,
                        format!(
                            "Load the declared material dataset(s) before running: {}.",
                            missing.join(", ")
                        ),
                    );
                    return;
                }
                let Some(variant_id) = self.study_variant_id.clone() else {
                    self.message = (
                        true,
                        "The current case could not be bound to a study variant.".into(),
                    );
                    return;
                };
                let workspace_json = match self.study_workspace.to_json() {
                    Ok(json) => json,
                    Err(error) => {
                        self.message = (true, format!("Could not serialize study inputs: {error}"));
                        return;
                    }
                };
                self.message = (
                    false,
                    "Searching in a browser worker — single-threaded, so a real grid takes a while; the page stays responsive…".into(),
                );
                self.launch_search_worker(ctx, workspace_json, variant_id);
            }
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.search_case.is_some() {
            if self.search_json.is_empty() {
                // A record view has no source case bytes to hash and run.
                return;
            }
            let missing = self.unresolved_dataset_bindings();
            if !missing.is_empty() {
                self.message = (
                    true,
                    format!(
                        "Load the declared material dataset(s) before running: {}.",
                        missing.join(", ")
                    ),
                );
                return;
            }
            let Some(variant_id) = self.study_variant_id.clone() else {
                self.message = (
                    true,
                    "The current case could not be bound to a study variant.".into(),
                );
                return;
            };
            let mut workspace = self.study_workspace.clone();
            let mut session = std::mem::take(&mut self.study_session);
            self.message = (
                false,
                "Running coupled candidate search — every geometry is screened against the requirement…".into(),
            );
            let progress = Arc::new(SearchProgress::new());
            self.search_progress = Some(progress.clone());
            self.launch(ctx, JobKind::Search, move |cancel| {
                let cache_hit = workspace
                    .variant(&variant_id)
                    .ok()
                    .and_then(|variant| session.cached_result(variant).ok())
                    .flatten()
                    .is_some();
                let outcome = session
                    .run_variant_with(&mut workspace, &variant_id, &cancel, Some(&progress))
                    .map_err(|e| e.to_string())
                    .and_then(|result_id| {
                        workspace
                            .variant(&variant_id)
                            .map_err(|e| e.to_string())
                            .and_then(|variant| {
                                variant
                                    .results
                                    .iter()
                                    .find(|result| result.id == result_id)
                                    .ok_or_else(|| {
                                        "Study runner returned no attached result.".to_owned()
                                    })
                            })
                    })
                    .and_then(|result| {
                        serde_json::from_str(&result.record_json).map_err(|e| e.to_string())
                    });
                Ok(JobResult::StudySearchCompleted(Box::new((
                    workspace, session, cache_hit, outcome,
                ))))
            });
            return;
        }
        // Same refusal above covers the legacy allocation search too —
        // everything from here on is unreachable in the browser build.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let case = self.case.clone();
            let options = SearchOptions {
                max_evaluations: self.max_evaluations,
            };
            self.record = None;
            self.message = (false, "Searching grade and tape-count allocations…".into());
            self.launch(ctx, JobKind::Search, move |cancel| {
                run_cancellable(&case, &options, &cancel)
                    .map(|r| JobResult::Calculated(Box::new(r)))
                    .map_err(|e| e.to_string())
            });
        }
    }

    /// Where the workbench's small persisted state lives — recents and
    /// the library folder, not project data.
    #[cfg(not(target_arch = "wasm32"))]
    fn persisted_path() -> Option<PathBuf> {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(dir.join("converra").join("workbench.json"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn persist(&self) {
        let Some(path) = Self::persisted_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let state = serde_json::json!({
            "recent_files": self.recent_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "library_dir": self.library_dir.as_ref().map(|p| p.display().to_string()),
        });
        let _ = fs::write(path, state.to_string());
    }

    /// No config directory exists in the browser build.
    #[cfg(target_arch = "wasm32")]
    fn persist(&self) {}

    fn push_recent(&mut self, path: &Path) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(12);
        self.persist();
    }

    /// Shallow scan of the user's chosen library folder — `.json` files
    /// whose head declares an `optcoil-` schema marker. One directory
    /// level, opt-in, never a filesystem crawl. No folders exist in the
    /// browser build — the library stays empty there.
    #[cfg(not(target_arch = "wasm32"))]
    fn rescan_library(&mut self) {
        self.library_entries = self
            .library_dir
            .as_ref()
            .map(|dir| {
                let mut found: Vec<PathBuf> = fs::read_dir(dir)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.path())
                            .filter(|p| {
                                p.extension().is_some_and(|e| e == "json")
                                    && fs::read(p)
                                        .ok()
                                        .map(|bytes| {
                                            bytes[..bytes.len().min(8192)]
                                                .windows(9)
                                                .any(|w| w == b"\"optcoil-")
                                        })
                                        .unwrap_or(false)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                found.sort();
                found
            })
            .unwrap_or_default();
    }

    #[cfg(target_arch = "wasm32")]
    fn rescan_library(&mut self) {}

    #[cfg(not(target_arch = "wasm32"))]
    fn pick_library(&mut self, ctx: &egui::Context) {
        self.launch(ctx, JobKind::Open, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Choose a case-library folder")
                .pick_folder();
            match path {
                Some(path) => Ok(JobResult::LibraryPicked(path)),
                None => Ok(JobResult::Dismissed),
            }
        });
    }

    /// Browsers cannot open folders — the library is native-only.
    #[cfg(target_arch = "wasm32")]
    fn pick_library(&mut self, _ctx: &egui::Context) {
        self.message = (
            true,
            "Folder libraries need the desktop build — open a case by file pick or drag & drop."
                .into(),
        );
    }

    /// Derive the record-level artifacts once per record load — the
    /// record JSON serializes once; BOM and the grading delta parse it
    /// immediately (cheap), verify/sweep/bakeoff stay user-triggered.
    fn load_record_artifacts(&mut self, record: &CoupledSearchRunRecord) {
        self.search_record_json = serde_json::to_string(record).unwrap_or_default();
        self.bom_record = bom_from_record(&self.search_record_json).ok();
        self.decision_summary =
            optcoil_search::review::decision_summary(&self.search_record_json).ok();
        self.grade_report = grade_report_from_record(&self.search_record_json).ok();
        self.verify_checks = None;
        self.sweep_record = None;
        self.bakeoff_record = None;
    }

    /// The case's JSON — the raw file bytes when a case was opened,
    /// re-serialized from the record's embedded case otherwise.
    fn case_json(&self) -> Option<String> {
        if !self.search_json.is_empty() {
            Some(self.search_json.clone())
        } else {
            self.search_case
                .as_ref()
                .and_then(|c| serde_json::to_string(c).ok())
        }
    }

    fn verify_now(&mut self) {
        let Some(record_json) =
            (!self.search_record_json.is_empty()).then(|| self.search_record_json.clone())
        else {
            return;
        };
        let case_json = self.case_json();
        let ctx = self.ctx.clone();
        let bundles = self
            .all_dataset_sources()
            .into_values()
            .filter_map(|source| source.bundle_json)
            .collect::<Vec<_>>();
        self.launch(&ctx, JobKind::Verify, move |_| {
            let bundle_refs = bundles.iter().map(String::as_str).collect::<Vec<_>>();
            verify::verify_record_checks(
                &record_json,
                case_json.as_deref(),
                None,
                None,
                &bundle_refs,
            )
            .map(JobResult::Verified)
            .map_err(|e| e.to_string())
        });
    }

    /// Utilization-limit frontier — the workbench synthesizes a v3
    /// sensitivity spec over a default margin grid and runs the sweep.
    fn run_frontier(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let Some(case_json) = self.case_json() else {
            return;
        };
        let datasets = self.resolved_search_datasets();
        let spec = serde_json::json!({
            "schema": "optcoil-sensitivity/v3",
            "id": "workbench-margin-frontier",
            "provenance": "Workbench-declared sweep — cheapest-PASS cost against limits.utilization_limit",
            "axes": [{
                "kind": "utilization_limit",
                "values": [0.5, 0.55, 0.6, 0.65, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95],
            }],
        });
        let ctx = self.ctx.clone();
        self.message = (
            false,
            "Running the margin sweep in the background; you can cancel it above.".into(),
        );
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(id) = &self.study_variant_id
                && self
                    .study_workspace
                    .variant(id)
                    .is_ok_and(|variant| variant.options.threads != Some(1))
            {
                self.message = (true, "Browser margin sweeps use one thread. Review the explicit override on the Engineering study page before running this imported variant.".into());
                return;
            }
            let datasets_json = match serde_json::to_string(&datasets) {
                Ok(json) => json,
                Err(error) => {
                    self.message = (
                        true,
                        format!("Could not serialize margin sweep datasets: {error}"),
                    );
                    return;
                }
            };
            self.launch_analysis_worker(
                &ctx,
                JobKind::Sweep,
                "Margin sweep",
                serde_json::json!({
                    "kind": "margin-sweep",
                    "case_json": case_json,
                    "spec_json": spec.to_string(),
                    "datasets_json": datasets_json,
                }),
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(&ctx, JobKind::Sweep, move |cancel| {
            optcoil_search::sensitivity::run_sensitivity_sweep_with_datasets(
                &case_json,
                &spec.to_string(),
                &CoupledSearchOptions { threads: None },
                &datasets,
                &cancel,
            )
            .map(|r| JobResult::SweepDone(Box::new(r)))
            .map_err(|e| e.to_string())
        });
    }

    /// Folder pick for the bake-off — the chosen directory holds the
    /// dataset bundles; the spec is synthesized at run time.
    #[cfg(not(target_arch = "wasm32"))]
    fn pick_bakeoff_dir(&mut self) {
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Open, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Choose a folder of dataset bundles")
                .pick_folder();
            match path {
                Some(path) => Ok(JobResult::BakeoffDirPicked(path)),
                None => Ok(JobResult::Dismissed),
            }
        });
    }

    /// The bake-off scans a folder of bundles — native-only; in the
    /// browser, datasets arrive one at a time through the file picker.
    #[cfg(target_arch = "wasm32")]
    fn pick_bakeoff_dir(&mut self) {
        self.message = (
            true,
            "The dataset bake-off scans a folder — use the desktop build.".into(),
        );
    }

    /// Build the bake-off spec from every parseable bundle in `dir` plus
    /// the case's own declared dataset (bundle-less entry → embedded
    /// resolution), then run the sweep.
    fn run_bakeoff(&mut self, dir: PathBuf) {
        let Some(case_json) = self.case_json() else {
            return;
        };
        let mut datasets = Vec::new();
        if let Some(base) = self
            .search_case
            .as_ref()
            .map(|c| c.material.dataset_id.clone())
        {
            datasets.push(serde_json::json!({"dataset_id": base}));
        }
        // The bake-off reads a picked directory — native-only; the wasm
        // picker is refused before this can be reached.
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(rd) = fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(bundle) = MaterialBundle::from_json(&text) else {
                    continue;
                };
                datasets.push(serde_json::json!({
                    "dataset_id": bundle.dataset.metadata.id,
                    "bundle": entry.file_name().to_string_lossy(),
                }));
            }
        }
        let spec = serde_json::json!({
            "schema": "optcoil-bakeoff/v1",
            "provenance": "Workbench-declared bake-off — every bundle in the picked folder plus the case's declared dataset",
            "datasets": datasets,
        });
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Bakeoff, move |cancel| {
            run_bakeoff(
                &case_json,
                &spec.to_string(),
                &CoupledSearchOptions { threads: None },
                &dir,
                &cancel,
            )
            .map(|r| JobResult::BakeoffDone(Box::new(r)))
            .map_err(|e| e.to_string())
        });
    }

    /// Bore B_z(z) profile for the selected candidate — the physics
    /// evaluator runs on a worker thread; the inspector plots the
    /// returned samples.
    fn run_profile(&mut self, candidate_index: usize) {
        let Some(record) = &self.search_record else {
            return;
        };
        let Some(candidate) = record.candidates.get(candidate_index) else {
            return;
        };
        let (Some(straight), Some(bend)) = (
            candidate.geometry.straight_half_length_m,
            candidate.geometry.bend_radius_m,
        ) else {
            self.message = (
                true,
                "Bore profile needs a racetrack geometry — path-defined cases can't profile here."
                    .into(),
            );
            return;
        };
        let g = candidate.geometry.clone();
        let ampere_turns = candidate.ampere_turns_a;
        let probe = record.case.requirement.bore_probe_m;
        let order = record
            .case
            .numerics
            .quadrature_orders
            .first()
            .copied()
            .unwrap_or(8);
        let z_span = (straight + bend) * 1.4;
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Profile, move |_| {
            let path = optcoil_model::path::CoilPath::racetrack(straight, bend);
            let eval = optcoil_physics::racetrack::RacetrackEvaluator::from_path(
                &path,
                g.radial_width_m,
                g.axial_height_m,
                1.0,
                order,
            )
            .map_err(|e| e.to_string())?;
            let mut points = Vec::with_capacity(65);
            for i in 0..65 {
                let z = -z_span + (i as f64 / 64.0) * 2.0 * z_span;
                let b = eval
                    .evaluate([probe[0], probe[1], z])
                    .map_err(|e| e.to_string())?;
                points.push([z, b.field_t[2] * ampere_turns]);
            }
            Ok(JobResult::ProfileDone(candidate_index, points))
        });
    }

    /// Pick cases to run back-to-back — each record writes under runs/
    /// before the next case loads.
    #[cfg(not(target_arch = "wasm32"))]
    fn queue_cases(&mut self) {
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Queue, move |_| {
            let paths = rfd::FileDialog::new()
                .set_title("Queue coupled-search cases")
                .add_filter("Converra case", &["json"])
                .pick_files();
            match paths {
                Some(paths) if !paths.is_empty() => Ok(JobResult::Queued(paths)),
                _ => Ok(JobResult::Dismissed),
            }
        });
    }

    /// The run queue exists to chain searches — which the browser build
    /// refuses; and it writes records under runs/, which needs a fs.
    #[cfg(target_arch = "wasm32")]
    fn queue_cases(&mut self) {
        self.message = (
            true,
            "The run queue needs the desktop build — it writes records under runs/.".into(),
        );
    }

    /// A second run record for the side-by-side compare card on Reports.
    #[cfg(not(target_arch = "wasm32"))]
    fn pick_compare_record(&mut self) {
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Open, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Compare against a saved run record")
                .add_filter("Converra run record", &["json"])
                .pick_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let record: CoupledSearchRunRecord =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            Ok(JobResult::CompareLoaded(Box::new(record)))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn pick_compare_record(&mut self) {
        let ctx = self.ctx.clone();
        self.launch_wasm(&ctx, JobKind::Open, async move {
            let Some((_name, bytes)) = web::pick_bytes(&["json"]).await? else {
                return Ok(JobResult::Dismissed);
            };
            let record: CoupledSearchRunRecord =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            Ok(JobResult::CompareLoaded(Box::new(record)))
        });
    }

    /// Save a PNG of the app frame: pick the path on a worker, send the
    /// viewport screenshot command, write when the image lands in poll.
    #[cfg(not(target_arch = "wasm32"))]
    fn pick_screenshot(&mut self) {
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Open, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Save screenshot")
                .add_filter("PNG image", &["png"])
                .set_file_name("converra.png")
                .save_file();
            match path {
                Some(path) => Ok(JobResult::ScreenshotPathPicked(path)),
                None => Ok(JobResult::Dismissed),
            }
        });
    }

    /// No save dialog on the web — the filename marks the pending frame;
    /// poll downloads the PNG when the Screenshot event lands.
    #[cfg(target_arch = "wasm32")]
    fn pick_screenshot(&mut self) {
        self.screenshot_target = Some(PathBuf::from("converra.png"));
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(())));
    }

    /// The next queued case: write the finished record under runs/,
    /// then open the next case — the LoadedSearch arm auto-starts it
    /// while `queue_active` is set.
    fn advance_queue(&mut self) {
        let Some(next) = self.run_queue.first().cloned() else {
            self.queue_active = false;
            return;
        };
        self.run_queue.remove(0);
        // Records land under runs/ between queued cases — native-only;
        // the queue itself is refused in the browser build.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(record) = &self.search_record {
            let dir = PathBuf::from("runs");
            let _ = fs::create_dir_all(&dir);
            let path = dir.join(format!(
                "optcoil-coupled-search-{}.json",
                record.started_unix_ms
            ));
            let _ = record.write_new(&path);
        }
        let ctx = self.ctx.clone();
        self.open(&ctx, Some(next));
    }

    /// `path` is always `None` in the browser — picked files resolve to
    /// bytes; `open` is kept so every call site stays target-agnostic.
    #[cfg(target_arch = "wasm32")]
    fn open(&mut self, ctx: &egui::Context, _path: Option<PathBuf>) {
        self.open_bytes(ctx, None);
    }

    /// `bytes` is the picked/dropped file contents in the browser — on
    /// native, the open path is read instead.
    #[cfg(target_arch = "wasm32")]
    fn open_bytes(&mut self, ctx: &egui::Context, bytes: Option<(String, Vec<u8>)>) {
        self.launch_wasm(ctx, JobKind::Open, async move {
            let (name, bytes) = match bytes {
                Some(pair) => pair,
                None => {
                    let Some(pair) = web::pick_bytes(&["json"]).await? else {
                        return Ok(JobResult::Dismissed);
                    };
                    pair
                }
            };
            let json = String::from_utf8(bytes).map_err(|e| format!("Cannot read {name}: {e}"))?;
            load_project_json(json, PathBuf::from(name))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        self.launch(ctx, JobKind::Open, move |_| {
            let path = path.or_else(|| {
                rfd::FileDialog::new()
                    .set_title("Open Converra project")
                    .add_filter("Converra case", &["json"])
                    .pick_file()
            });
            match path {
                Some(path) => load_project(path),
                None => Ok(JobResult::Dismissed),
            }
        });
    }

    /// Pick an `optcoil-material-dataset/v1` bundle for the open coupled
    /// case. Identity is enforced in `apply_result` against the case's
    /// declared `dataset_id`/`csv_sha256`.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_dataset(&mut self, ctx: &egui::Context) {
        self.launch(ctx, JobKind::Open, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Load material dataset bundle")
                .add_filter("Material dataset bundle", &["json"])
                .pick_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let bundle = MaterialBundle::from_json(&text).map_err(|e| e.to_string())?;
            Ok(JobResult::DatasetLoaded(Box::new(bundle), path, Some(text)))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_dataset_pair(&mut self, ctx: &egui::Context) {
        self.launch(ctx, JobKind::Open, move |_| {
            let Some(metadata) = rfd::FileDialog::new()
                .set_title("Select attributed material metadata JSON")
                .add_filter("Metadata", &["json"])
                .pick_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            let Some(csv) = rfd::FileDialog::new()
                .set_title("Select canonical measurement CSV")
                .add_filter("Measurements", &["csv"])
                .pick_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            let (metadata_json, csv_bytes) = author::read_material_pair_files(&metadata, &csv)?;
            let bundle = optcoil_model::dataset_intake::material_bundle_from_pair(
                &metadata_json,
                &csv_bytes,
            )
            .map_err(|e| e.to_string())?;
            let bundle_json = bundle.to_json().ok();
            Ok(JobResult::DatasetLoaded(
                Box::new(bundle),
                metadata,
                bundle_json,
            ))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn load_dataset_pair(&mut self, ctx: &egui::Context) {
        self.launch_wasm(ctx, JobKind::Open, async {
            let Some((name, metadata)) = web::pick_bytes_limited(&["json"], 1024 * 1024).await?
            else {
                return Ok(JobResult::Dismissed);
            };
            let Some((_name, csv)) = web::pick_bytes_limited(&["csv"], 32 * 1024 * 1024).await?
            else {
                return Ok(JobResult::Dismissed);
            };
            let metadata = String::from_utf8(metadata).map_err(|e| e.to_string())?;
            let bundle = optcoil_model::dataset_intake::material_bundle_from_pair(&metadata, &csv)
                .map_err(|e| e.to_string())?;
            let bundle_json = bundle.to_json().ok();
            Ok(JobResult::DatasetLoaded(
                Box::new(bundle),
                PathBuf::from(name),
                bundle_json,
            ))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn load_dataset(&mut self, ctx: &egui::Context) {
        self.launch_wasm(ctx, JobKind::Open, async move {
            let Some((name, bytes)) = web::pick_bytes(&["json"]).await? else {
                return Ok(JobResult::Dismissed);
            };
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            let bundle = MaterialBundle::from_json(&text).map_err(|e| e.to_string())?;
            Ok(JobResult::DatasetLoaded(
                Box::new(bundle),
                PathBuf::from(name),
                Some(text),
            ))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn export(&mut self, _ctx: &egui::Context) {
        // The record serializes in memory and arrives as a download.
        if let Some(record) = self.search_record.clone() {
            let name = format!("optcoil-coupled-search-{}.json", record.started_unix_ms);
            match serde_json::to_string_pretty(&record) {
                Ok(json) => {
                    web::download_bytes(&name, json.as_bytes());
                    self.message = (false, format!("Downloaded {name}."));
                }
                Err(e) => self.message = (true, format!("Export failed: {e}")),
            }
            return;
        }
        let Some(record) = self.record.clone() else {
            return;
        };
        let name = format!("optcoil-{}.json", record.started_unix_ms);
        match serde_json::to_string_pretty(&record) {
            Ok(json) => {
                web::download_bytes(&name, json.as_bytes());
                self.message = (false, format!("Downloaded {name}."));
            }
            Err(e) => self.message = (true, format!("Export failed: {e}")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export(&mut self, ctx: &egui::Context) {
        if let Some(record) = self.search_record.clone() {
            self.launch(ctx, JobKind::Export, move |_| {
                let name = format!("optcoil-coupled-search-{}.json", record.started_unix_ms);
                let path = rfd::FileDialog::new()
                    .set_title("Export coupled-search run record")
                    .add_filter("Converra run record", &["json"])
                    .set_file_name(name)
                    .save_file();
                let Some(path) = path else {
                    return Ok(JobResult::Dismissed);
                };
                record.write_new(&path).map_err(|e| {
                    format!("Export failed: {e}. Use a new filename for each record.")
                })?;
                Ok(JobResult::Exported(path))
            });
            return;
        }
        let Some(record) = self.record.clone() else {
            return;
        };
        self.launch(ctx, JobKind::Export, move |_| {
            let name = format!("optcoil-{}.json", record.started_unix_ms);
            let path = rfd::FileDialog::new()
                .set_title("Export complete Converra run record")
                .add_filter("Converra run record", &["json"])
                .set_file_name(name)
                .save_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            record
                .write_new(&path)
                .map_err(|e| format!("Export failed: {e}. Use a new filename for each record."))?;
            Ok(JobResult::Exported(path))
        });
    }

    /// Export the coupled-search record as a self-contained HTML evidence
    /// report — the readable counterpart to the JSON record.
    #[cfg(target_arch = "wasm32")]
    fn export_report(&mut self, _ctx: &egui::Context) {
        let Some(record) = self.search_record.clone() else {
            return;
        };
        let name = format!("converra-report-{}.html", record.started_unix_ms);
        match optcoil_search::report::render_search_report_html(&record) {
            Ok(html) => {
                web::download_bytes(&name, html.as_bytes());
                self.message = (false, format!("Downloaded {name}."));
            }
            Err(e) => self.message = (true, format!("Export failed: {e}")),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export_report(&mut self, ctx: &egui::Context) {
        let Some(record) = self.search_record.clone() else {
            return;
        };
        self.launch(ctx, JobKind::Export, move |_| {
            let name = format!("converra-report-{}.html", record.started_unix_ms);
            let path = rfd::FileDialog::new()
                .set_title("Export evidence report")
                .add_filter("HTML report", &["html"])
                .set_file_name(name)
                .save_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            let html = optcoil_search::report::render_search_report_html(&record)
                .map_err(|e| format!("Export failed: {e}"))?;
            let mut file = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            use std::io::Write as _;
            file.write_all(html.as_bytes())
                .map_err(|e| format!("Export failed: {e}"))?;
            Ok(JobResult::Exported(path))
        });
    }

    /// Export the coupled-search record's RFQ document — the
    /// procurement-facing markdown derived from the same BOM ledger walk.
    #[cfg(target_arch = "wasm32")]
    fn export_rfq(&mut self, _ctx: &egui::Context) {
        let Some(record) = self.search_record.clone() else {
            return;
        };
        let name = format!("converra-rfq-{}.md", record.started_unix_ms);
        let result = serde_json::to_string(&record)
            .map_err(|e| format!("Export failed: {e}"))
            .and_then(|json| {
                optcoil_search::bom::rfq_markdown_from_record(&json)
                    .map_err(|e| format!("Export failed: {e}"))
            });
        match result {
            Ok(doc) => {
                web::download_bytes(&name, doc.as_bytes());
                self.message = (false, format!("Downloaded {name}."));
            }
            Err(e) => self.message = (true, e),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export_rfq(&mut self, ctx: &egui::Context) {
        let Some(record) = self.search_record.clone() else {
            return;
        };
        self.launch(ctx, JobKind::Export, move |_| {
            let name = format!("converra-rfq-{}.md", record.started_unix_ms);
            let path = rfd::FileDialog::new()
                .set_title("Export RFQ document")
                .add_filter("Markdown", &["md"])
                .set_file_name(name)
                .save_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            let record_json =
                serde_json::to_string(&record).map_err(|e| format!("Export failed: {e}"))?;
            let doc = optcoil_search::bom::rfq_markdown_from_record(&record_json)
                .map_err(|e| format!("Export failed: {e}"))?;
            let mut file = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
            use std::io::Write as _;
            file.write_all(doc.as_bytes())
                .map_err(|e| format!("Export failed: {e}"))?;
            Ok(JobResult::Exported(path))
        });
    }

    fn export_previous_record(&mut self, ctx: &egui::Context) {
        let Some(previous) = &self.compare_record else {
            return;
        };
        let json = match serde_json::to_string_pretty(previous) {
            Ok(json) => json,
            Err(error) => {
                self.message = (true, error.to_string());
                return;
            }
        };
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            web::download_bytes("previous-completed-record.json", json.as_bytes());
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, JobKind::Export, move |_| {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save previous completed record")
                .set_file_name("previous-completed-record.json")
                .save_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            Ok(JobResult::Exported(path))
        });
    }

    fn review_artifacts(&self) -> Result<Vec<(String, String)>, String> {
        if self.search_record.is_none() {
            return Err("Run a study before exporting a review package.".into());
        }
        let bundles = self
            .all_dataset_sources()
            .into_values()
            .filter_map(|source| source.bundle_json)
            .collect::<Vec<_>>();
        let bundle_refs = bundles.iter().map(String::as_str).collect::<Vec<_>>();
        optcoil_search::review::review_package_with_bundle_json(
            &self.search_record_json,
            (!self.search_json.is_empty()).then_some(self.search_json.as_str()),
            &bundle_refs,
        )
        .map_err(|e| e.to_string())
    }

    fn all_dataset_sources(&self) -> std::collections::BTreeMap<String, DatasetSource> {
        let mut sources = self.search_spec_datasets.clone();
        if let Some(source) = &self.search_dataset {
            let id = source.dataset.metadata.id.clone();
            let keep_loaded = sources
                .get(&id)
                .is_some_and(|current| matches!(current.origin, DatasetOrigin::File(_)))
                && matches!(source.origin, DatasetOrigin::Embedded);
            if !keep_loaded {
                sources.insert(id, source.clone());
            }
        }
        sources
    }

    #[cfg(target_arch = "wasm32")]
    fn export_pilot_bundle(&mut self, _ctx: &egui::Context) {
        match self.review_artifacts().and_then(|files| {
            optcoil_search::review::review_package_tar(&files).map_err(|e| e.to_string())
        }) {
            Ok(bytes) => {
                web::download_bytes("converra-review.tar", &bytes);
                self.message = (false, "Review package downloaded. Extract it and run the verification command in README.txt.".into());
            }
            Err(error) => self.message = (true, error),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn export_pilot_bundle(&mut self, ctx: &egui::Context) {
        let files = match self.review_artifacts() {
            Ok(files) => files,
            Err(error) => {
                self.message = (true, error);
                return;
            }
        };
        self.launch(ctx, JobKind::Export, move |_| {
            let Some(parent) = rfd::FileDialog::new()
                .set_title("Choose parent directory for the review package")
                .pick_folder()
            else {
                return Ok(JobResult::Dismissed);
            };
            let name = format!(
                "converra-review-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_millis()
            );
            let directory = parent.join(name);
            fs::create_dir(&directory).map_err(|e| e.to_string())?;
            use std::io::Write;
            for (path, contents) in files {
                let destination = directory.join(path);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|e| e.to_string())?;
                file.write_all(contents.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            Ok(JobResult::Exported(directory))
        });
    }

    /// Write a freshly authored coupled-search case, then open it.
    #[cfg(not(target_arch = "wasm32"))]
    fn save_case(&mut self, ctx: &egui::Context, json: String) {
        self.launch(ctx, JobKind::Export, move |_| {
            let path = rfd::FileDialog::new()
                .set_title("Save coupled-search case")
                .add_filter("Converra coupled-search case", &["json"])
                .set_file_name("coupled-search-case.json")
                .save_file();
            let Some(path) = path else {
                return Ok(JobResult::Dismissed);
            };
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| {
                    format!(
                        "Save failed: {e}. Choose a new filename; the source is never overwritten."
                    )
                })?;
            file.write_all(json.as_bytes())
                .map_err(|e| format!("Save failed: {e}"))?;
            Ok(JobResult::CaseWritten(path))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_study_workspace(&mut self, ctx: &egui::Context) {
        self.launch(ctx, JobKind::Workspace, move |_| {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Open engineering study workspace")
                .add_filter("Converra study workspace", &["json"])
                .pick_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            let json = fs::read_to_string(&path)
                .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
            let workspace = StudyWorkspace::from_json(&json).map_err(|e| e.to_string())?;
            Ok(JobResult::WorkspaceLoaded(Box::new(workspace), path))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn open_study_workspace(&mut self, ctx: &egui::Context) {
        self.launch_wasm(ctx, JobKind::Workspace, async move {
            let Some((name, bytes)) = web::pick_bytes(&["json"]).await? else {
                return Ok(JobResult::Dismissed);
            };
            let json = String::from_utf8(bytes).map_err(|e| format!("Cannot read {name}: {e}"))?;
            let workspace = StudyWorkspace::from_json(&json).map_err(|e| e.to_string())?;
            Ok(JobResult::WorkspaceLoaded(
                Box::new(workspace),
                PathBuf::from(name),
            ))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_study_workspace(&mut self, ctx: &egui::Context) {
        let workspace = self.study_workspace.clone();
        self.launch(ctx, JobKind::Workspace, move |_| {
            let json = workspace.to_json().map_err(|e| e.to_string())?;
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save engineering study workspace")
                .add_filter("Converra study workspace", &["json"])
                .set_file_name("converra-study.json")
                .save_file()
            else {
                return Ok(JobResult::Dismissed);
            };
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| {
                    format!(
                        "Save failed: {e}. Choose a new filename; the source is never overwritten."
                    )
                })?;
            file.write_all(json.as_bytes())
                .map_err(|e| format!("Save failed: {e}"))?;
            Ok(JobResult::WorkspaceWritten(path))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn save_study_workspace(&mut self, ctx: &egui::Context) {
        let workspace = self.study_workspace.clone();
        self.launch_wasm(ctx, JobKind::Workspace, async move {
            let json = workspace.to_json().map_err(|e| e.to_string())?;
            web::download_bytes("converra-study.json", json.as_bytes());
            Ok(JobResult::WorkspaceWritten(PathBuf::from(
                "converra-study.json",
            )))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_study_summary(&mut self) {
        let Some(id) = self.study_variant_id.clone() else {
            self.study_summary = None;
            return;
        };
        let workspace = self.study_workspace.clone();
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Workspace, move |_| {
            workspace
                .compact_summary(&id)
                .map(|summary| JobResult::StudySummaryReady(Box::new(summary)))
                .map_err(|e| e.to_string())
        });
    }

    /// Show the latest retained result only when it belongs to this exact
    /// variant input. Loading it into the UI does not populate the live
    /// engine session cache; only a completed run can do that.
    fn restore_current_study_result(&mut self, ctx: &egui::Context, id: &str) -> bool {
        use sha2::{Digest, Sha256};

        let Some(record) = self.study_workspace.variant(id).ok().and_then(|variant| {
            let exact_key = optcoil_search::study::exact_input_key(variant).ok()?;
            let case_sha = format!("{:x}", Sha256::digest(variant.case_json.as_bytes()));
            variant
                .results
                .iter()
                .enumerate()
                .filter(|(_, result)| {
                    result.exact_input_key == exact_key && result.case_sha256 == case_sha
                })
                .max_by_key(|(index, result)| (result.attached_unix_ms, *index))
                .and_then(|(_, result)| serde_json::from_str(&result.record_json).ok())
        }) else {
            return false;
        };

        self.importing_workspace = true;
        self.apply_result(ctx, Ok(JobResult::SearchCompleted(Box::new(record))));
        self.importing_workspace = false;
        true
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_study_diff(&mut self, left: String, right: String) {
        let workspace = self.study_workspace.clone();
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Workspace, move |_| {
            let diff = workspace
                .case_diff(&left, &right)
                .map_err(|e| e.to_string())?;
            let a = workspace
                .compact_summary(&left)
                .map_err(|e| e.to_string())?;
            let b = workspace
                .compact_summary(&right)
                .map_err(|e| e.to_string())?;
            Ok(JobResult::StudyCompareReady(Box::new((diff, a, b))))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn request_study_diff(&mut self, left: String, right: String) {
        let workspace = self.study_workspace.clone();
        let ctx = self.ctx.clone();
        self.launch_wasm(&ctx, JobKind::Workspace, async move {
            let diff = workspace
                .case_diff(&left, &right)
                .map_err(|e| e.to_string())?;
            let a = workspace
                .compact_summary(&left)
                .map_err(|e| e.to_string())?;
            let b = workspace
                .compact_summary(&right)
                .map_err(|e| e.to_string())?;
            Ok(JobResult::StudyCompareReady(Box::new((diff, a, b))))
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn activate_study_variant(&mut self, id: String) {
        if self.worker.is_some() {
            return;
        }
        let Ok(variant) = self.study_workspace.variant(&id) else {
            return;
        };
        let case_json = variant.case_json.clone();
        let bundles = variant.dataset_bundles.clone();
        let ctx = self.ctx.clone();
        self.launch(&ctx, JobKind::Workspace, move |_| {
            CoupledSearchCase::from_json(&case_json).map_err(|e| e.to_string())?;
            Ok(JobResult::StudyVariantLoaded(Box::new((
                id, case_json, bundles,
            ))))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn activate_study_variant(&mut self, id: String) {
        if self.worker.is_some() {
            return;
        }
        let Ok(variant) = self.study_workspace.variant(&id) else {
            return;
        };
        let case_json = variant.case_json.clone();
        let bundles = variant.dataset_bundles.clone();
        let ctx = self.ctx.clone();
        self.launch_wasm(&ctx, JobKind::Workspace, async move {
            CoupledSearchCase::from_json(&case_json).map_err(|e| e.to_string())?;
            Ok(JobResult::StudyVariantLoaded(Box::new((
                id, case_json, bundles,
            ))))
        });
    }

    fn create_study_follow_up(&mut self, experiment: optcoil_search::study::FollowUpExperiment) {
        let Ok(source) = self
            .study_workspace
            .variant(&experiment.source_variant_id)
            .cloned()
        else {
            return;
        };
        let name = format!("{} follow-up", source.name);
        match self.study_workspace.add_variant(
            name,
            experiment.proposed_case_json.clone(),
            source.dataset_bundles,
            source.options,
        ) {
            Ok(id) => {
                self.study_follow_up = None;
                self.study_summary = None;
                self.study_diff = None;
                self.study_compare_summary = None;
                self.study_variant_id = Some(id.clone());
                let _ = self.study_workspace.select_variant(&id);
                self.activate_study_variant(id);
                self.launch_after_variant = true;
                self.refresh_study_summary();
            }
            Err(error) => self.message = (true, error.to_string()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_browser_thread_override(&mut self) {
        let Some(id) = self.study_variant_id.clone() else {
            return;
        };
        let Ok(variant) = self.study_workspace.variant(&id).map(Clone::clone) else {
            return;
        };
        let mut options = variant.options;
        if options.threads == Some(1) {
            return;
        }
        options.threads = Some(1);
        match self.study_workspace.revise_variant(
            &id,
            variant.case_json,
            variant.dataset_bundles,
            options,
        ) {
            Ok(()) => {
                self.message = (false, "Browser execution override applied: one thread. The changed options create a new exact-input identity; saved results remain historical.".into());
                self.robustness_preflight = None;
                self.refresh_robustness_history_current();
                self.refresh_study_summary();
            }
            Err(error) => self.message = (true, error.to_string()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_study_summary(&mut self) {
        let Some(id) = self.study_variant_id.clone() else {
            self.study_summary = None;
            return;
        };
        let workspace = self.study_workspace.clone();
        let ctx = self.ctx.clone();
        self.launch_wasm(&ctx, JobKind::Workspace, async move {
            workspace
                .compact_summary(&id)
                .map(|summary| JobResult::StudySummaryReady(Box::new(summary)))
                .map_err(|e| e.to_string())
        });
    }

    /// Browser: the authored case downloads; it opens in-place too, the
    /// same as a saved-then-reopened file.
    #[cfg(target_arch = "wasm32")]
    fn save_case(&mut self, ctx: &egui::Context, json: String) {
        web::download_bytes("coupled-search-case.json", json.as_bytes());
        self.author = None;
        self.open_bytes(
            ctx,
            Some(("coupled-search-case.json".into(), json.into_bytes())),
        );
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(worker) = &self.worker else {
            return;
        };
        let mut result = match worker.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Background task stopped unexpectedly.".into()),
        };
        if worker.kind == JobKind::Import && worker.cancel.load(Ordering::Relaxed) {
            result = Err(
                "Import cancelled. The active case and completed evidence are retained.".into(),
            );
        }
        #[cfg(target_arch = "wasm32")]
        let keep_browser_worker = matches!(
            &result,
            Ok(JobResult::BrowserStudySearchCompleted(_) | JobResult::RobustnessCompleted(_))
        );
        #[cfg(target_arch = "wasm32")]
        if keep_browser_worker {
            if let Some(worker) = self
                .worker
                .as_mut()
                .and_then(|worker| worker.web_worker.take())
            {
                worker.set_onmessage(None);
                worker.set_onerror(None);
                self.browser_search_worker = Some(worker);
            }
        }
        self.worker = None;
        self.search_progress = None;
        self.apply_result(ctx, result);
    }

    fn apply_result(&mut self, ctx: &egui::Context, result: Result<JobResult, String>) {
        if let Err(error) = &result
            && let Some(draft) = &mut self.material_import
        {
            draft.error = Some(error.clone());
        }
        match result {
            Ok(JobResult::Loaded(data, path)) => {
                (self.case, self.baseline) = *data;
                self.push_recent(&path);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.watch_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
                }
                self.path = Some(path);
                self.record = None;
                // Opening an allocation project leaves coupled-search mode.
                self.search_case = None;
                self.search_json.clear();
                self.search_dataset = None;
                self.search_spec_datasets.clear();
                self.search_record = None;
                self.selected_module = 0;
                self.module_filter.clear();
                self.reprice_usd_per_m = None;
                self.map_strand = 0;
                self.map_bend = 0;
                self.map_straight = 0;
                self.page = Page::Overview;
                self.plot_revision += 1;
                self.message = (
                    false,
                    "Project loaded. Run optimization to compare allocations.".into(),
                );
            }
            Ok(JobResult::LoadedSearch(data, path)) => {
                self.push_recent(&path);
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.watch_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
                }
                let auto_start = self.queue_active;
                let (case, json) = *data;
                self.resolve_case_datasets(&case, &path);
                self.search_case = Some(case);
                self.search_json = json;
                if let Some(bundle) = self.pending_import_bundle.take() {
                    self.install_workspace_bundles(&[bundle]);
                }
                self.study_summary = None;
                self.register_active_variant();
                if let Some(previous) = self.search_record.take() {
                    self.compare_record = Some(previous);
                }
                self.search_record_json.clear();
                self.bom_record = None;
                self.decision_summary = None;
                self.grade_report = None;
                self.verify_checks = None;
                self.sweep_record = None;
                self.bakeoff_record = None;
                self.record = None;
                self.path = Some(path);
                self.selected_candidate = 0;
                self.reprice_usd_per_m = None;
                self.map_strand = 0;
                self.map_bend = 0;
                self.map_straight = 0;
                self.page = Page::Overview;
                self.plot_revision += 1;
                let dataset_note = match &self.search_dataset {
                    Some(source) => match &source.origin {
                        DatasetOrigin::File(path) => format!(
                            " External dataset {} loaded from {}.",
                            source.dataset.metadata.id,
                            path.display()
                        ),
                        DatasetOrigin::Embedded => String::new(),
                    },
                    None => {
                        " Declared dataset is not embedded — load a dataset bundle to run.".into()
                    }
                };
                self.message = (
                    false,
                    format!(
                        "Coupled search project loaded. Run the search to evaluate every candidate geometry.{dataset_note}"
                    ),
                );
                if auto_start {
                    let ctx = self.ctx.clone();
                    self.start(&ctx);
                } else if !self.importing_workspace {
                    self.refresh_study_summary();
                }
            }
            Ok(JobResult::SourceCaseLoaded(json, path)) => {
                use sha2::{Digest, Sha256};
                let hash = format!("{:x}", Sha256::digest(json.as_bytes()));
                let matches = self
                    .search_record
                    .as_ref()
                    .is_some_and(|record| record.case_sha256.trim_start_matches("sha256:") == hash);
                if matches {
                    self.search_json = json;
                    if let Some(case) = self.search_case.clone() {
                        self.resolve_case_datasets(&case, &path);
                    }
                    self.path = Some(path);
                    self.message = (false, "Original case attached and its byte hash verified. The completed result is retained; rerun and review export are available.".into());
                } else {
                    self.message = (true, "Case bytes do not match this record's case hash. Select the original file used for the run; the current result is retained.".into());
                }
            }
            Ok(JobResult::LoadedSearchRun(record, path)) => {
                self.push_recent(&path);
                let record = *record;
                self.compare_pin = None;
                self.load_record_artifacts(&record);
                self.search_case = Some(record.case.clone());
                self.search_json.clear();
                self.search_dataset = None;
                self.search_spec_datasets.clear();
                self.record = None;
                self.path = Some(path);
                self.selected_candidate = 0;
                self.reprice_usd_per_m = None;
                self.map_strand = 0;
                self.map_bend = 0;
                self.map_straight = 0;
                self.page = Page::Overview;
                self.plot_revision += 1;
                self.results_reveal_start = Some(Instant::now());
                self.message = (
                    false,
                    "Run record opened — viewing saved results. Open the case file itself to re-run.".into(),
                );
                self.search_record = Some(record);
            }
            Ok(JobResult::SearchCompleted(record)) => {
                self.compare_pin = None;
                self.load_record_artifacts(&record);
                self.reprice_usd_per_m = None;
                self.map_strand = 0;
                self.map_bend = 0;
                self.map_straight = 0;
                let pass = record
                    .candidates
                    .iter()
                    .filter(|c| c.status == Status::Pass)
                    .count();
                self.message = (
                    false,
                    format!(
                        "Search {:?}: {} of {} candidates PASS · acceptance {:?}.",
                        record.search_status,
                        pass,
                        record.candidates.len(),
                        record.acceptance.agreement_status,
                    ),
                );
                if let Some(previous) = self.search_record.take() {
                    // A successful replacement retains the last completed result for comparison.
                    self.compare_record = Some(previous);
                }
                self.search_record = Some(*record);
                self.selected_candidate = 0;
                self.plot_revision += 1;
                self.results_reveal_start = Some(Instant::now());
                if self.queue_active {
                    self.advance_queue();
                } else if self.package_after_search {
                    self.package_after_search = false;
                    // Chained pilot flow: the record is loaded and shown;
                    // the bundle prompt is the only decision left.
                    self.export_pilot_bundle(ctx);
                }
                if !self.importing_workspace {
                    self.refresh_study_summary();
                }
            }
            #[cfg(target_arch = "wasm32")]
            Ok(JobResult::BrowserStudySearchCompleted(payload)) => {
                let (workspace, record, cache_hit) = *payload;
                self.study_workspace = workspace;
                self.apply_result(ctx, Ok(JobResult::SearchCompleted(Box::new(record))));
                if cache_hit {
                    self.message = (false, "Reused the exact-input result cached by this live browser worker; no new calculation was run.".into());
                }
            }
            #[cfg(target_arch = "wasm32")]
            Ok(JobResult::BrowserRobustnessCompleted(record)) => {
                let mut workspace = self.study_workspace.clone();
                match workspace.attach_robustness_result(*record) {
                    Ok(()) => {
                        self.study_workspace = workspace;
                        self.robustness_preflight = None;
                        self.refresh_robustness_history_current();
                        self.message = (
                            false,
                            "Named what-if runs completed. Results are retained with their exact source snapshots; they are not probabilities or engineering acceptance.".into(),
                        );
                    }
                    Err(error) => self.message = (true, error.to_string()),
                }
            }
            Ok(JobResult::StudySearchCompleted(result)) => {
                let (workspace, session, cache_hit, outcome) = *result;
                self.study_workspace = workspace;
                self.study_session = session;
                match outcome {
                    Ok(record) => {
                        self.apply_result(ctx, Ok(JobResult::SearchCompleted(Box::new(record))));
                        if cache_hit {
                            self.message = (false, "Reused the exact-input result cached by this live engine session; no new calculation was run.".into());
                        }
                    }
                    Err(error) => {
                        let is_cancel = error.contains("cancelled");
                        self.package_after_search = false;
                        self.message = (!is_cancel, error);
                    }
                }
            }
            Ok(JobResult::StudySummaryReady(summary)) => {
                self.study_summary = Some(*summary);
            }
            Ok(JobResult::RobustnessCompleted(result)) => {
                self.study_workspace = *result;
                self.robustness_preflight = None;
                self.refresh_robustness_history_current();
                self.message = (
                    false,
                    "Named what-if runs completed. Results are retained with their exact source snapshots; they are not probabilities or engineering acceptance.".into(),
                );
            }
            Ok(JobResult::StudyCompareReady(summaries)) => {
                let (diff, left, right) = *summaries;
                self.study_diff = Some(diff);
                self.study_compare_summary = Some((left, right));
            }
            Ok(JobResult::WorkspaceLoaded(workspace, path)) => {
                #[cfg(target_arch = "wasm32")]
                if let Some(worker) = self.browser_search_worker.take() {
                    worker.terminate();
                }
                self.study_workspace = *workspace;
                self.robustness_preflight = None;
                self.robustness_variant_ids.clear();
                self.robustness_scenarios.clear();
                self.refresh_robustness_history_current();
                self.study_session = StudyEngineSession::default();
                let selected = self
                    .study_workspace
                    .selected_variant_id
                    .clone()
                    .or_else(|| self.study_workspace.variants.first().map(|v| v.id.clone()));
                self.study_variant_id = selected.clone();
                if let Some(id) = selected
                    && let Ok(variant) = self.study_workspace.variant(&id)
                {
                    let case_json = variant.case_json.clone();
                    let bundles = variant.dataset_bundles.clone();
                    self.importing_workspace = true;
                    self.apply_result(ctx, load_project_json(case_json, path.clone()));
                    self.importing_workspace = false;
                    self.install_workspace_bundles(&bundles);
                    self.study_variant_id = Some(id.clone());
                    let _ = self.study_workspace.select_variant(&id);
                    self.page = Page::Study;
                    self.restore_current_study_result(ctx, &id);
                    self.message = (
                        false,
                        format!(
                            "Study workspace opened — {} variant(s); dependency bundles and results retained.",
                            self.study_workspace.variants.len()
                        ),
                    );
                    self.refresh_study_summary();
                }
            }
            Ok(JobResult::WorkspaceWritten(path)) => {
                self.mark_study_exported();
                self.message = (
                    false,
                    format!("Study workspace saved to {}.", path.display()),
                );
            }
            Ok(JobResult::RobustnessExported(path)) => {
                self.message = (
                    false,
                    format!("Robustness JSON exported to {}.", path.display()),
                );
            }
            Ok(JobResult::StudyVariantLoaded(payload)) => {
                let (id, case_json, bundles) = *payload;
                self.importing_workspace = true;
                self.apply_result(
                    ctx,
                    load_project_json(case_json, PathBuf::from(format!("{id}.json"))),
                );
                self.importing_workspace = false;
                self.install_workspace_bundles(&bundles);
                self.study_variant_id = Some(id.clone());
                let _ = self.study_workspace.select_variant(&id);
                self.page = Page::Study;
                self.study_diff = None;
                if self.launch_after_variant {
                    self.launch_after_variant = false;
                    self.start(ctx);
                } else {
                    self.restore_current_study_result(ctx, &id);
                    self.refresh_study_summary();
                }
            }
            Ok(JobResult::Verified(checks)) => {
                self.verify_checks = Some(checks);
                self.message = (false, "Verify ledger rendered on the Reports page.".into());
            }
            Ok(JobResult::SweepDone(record)) => {
                self.sweep_record = Some(*record);
                self.message = (false, "Margin frontier complete — see Reports.".into());
            }
            Ok(JobResult::BakeoffDone(record)) => {
                self.bakeoff_record = Some(*record);
                self.message = (false, "Vendor bake-off complete — see Reports.".into());
            }
            Ok(JobResult::BakeoffDirPicked(dir)) => {
                self.run_bakeoff(dir);
            }
            Ok(JobResult::ProfileDone(index, points)) => {
                self.field_profile = Some((index, points));
            }
            Ok(JobResult::CompareLoaded(record)) => {
                self.compare_record = Some(*record);
                self.message = (false, "Comparison record loaded — see Reports.".into());
            }
            Ok(JobResult::Queued(paths)) => {
                self.run_queue.extend(paths);
                self.queue_active = true;
                self.message = (
                    false,
                    format!(
                        "Run queue: {} case(s) — records write under runs/.",
                        self.run_queue.len()
                    ),
                );
                self.advance_queue();
            }
            Ok(JobResult::ScreenshotPathPicked(path)) => {
                self.screenshot_target = Some(path);
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(())));
            }
            Ok(JobResult::Calculated(record)) => {
                self.message = (
                    false,
                    format!(
                        "Search {:?}: {} of {} allocations evaluated. Engineering checks remain incomplete.",
                        record.termination, record.evaluated_candidates, record.search_space_size
                    ),
                );
                self.record = Some(*record);
                self.plot_revision += 1;
                self.results_reveal_start = Some(Instant::now());
            }
            Ok(JobResult::DatasetLoaded(bundle, path, raw_bundle_json)) => {
                let bundle = *bundle;
                let binding = self.search_case.as_ref().and_then(|case| {
                    case.material_bindings().into_iter().find(|(_, material)| {
                        material.dataset_id == bundle.dataset.metadata.id
                            && material.csv_sha256 == bundle.dataset.metadata.csv_sha256
                    })
                });
                match (self.search_case.as_ref(), binding) {
                    (Some(_), Some((binding_id, _))) => {
                        let dataset_id = bundle.dataset.metadata.id.clone();
                        if let Some(existing) =
                            self.search_spec_datasets.get(&dataset_id).or_else(|| {
                                self.search_dataset
                                    .as_ref()
                                    .filter(|source| source.dataset.metadata.id == dataset_id)
                            })
                            && (existing.dataset.metadata.csv_sha256
                                != bundle.dataset.metadata.csv_sha256)
                        {
                            self.message = (
                                true,
                                format!(
                                    "Dataset rejected — '{}' is already bound to a different CSV identity.",
                                    dataset_id
                                ),
                            );
                            return;
                        }
                        let signed = bundle
                            .attestation
                            .as_ref()
                            .map(|a| format!(" — signed by {} (key {})", a.issuer, a.key_id))
                            .unwrap_or_else(|| " — unsigned".to_owned());
                        self.message = (
                            false,
                            format!(
                                "Dataset {} loaded from {}{}.",
                                bundle.dataset.metadata.id,
                                path.display(),
                                signed
                            ),
                        );
                        let bundle_json = raw_bundle_json.or_else(|| bundle.to_json().ok());
                        let source = DatasetSource {
                            bundle_json,
                            dataset: bundle.dataset,
                            origin: DatasetOrigin::File(path),
                            attestation: bundle.attestation,
                        };
                        if binding_id == optcoil_model::coupled_search::BASE_TAPE_SPEC_ID {
                            self.search_dataset = Some(source);
                        } else {
                            self.search_spec_datasets.insert(dataset_id, source);
                        }
                        // Dependencies are executable inputs. Keep the exact
                        // loaded bundle bytes on the selected workspace variant
                        // before a later Run/Save/Compare can observe it.
                        self.edit_variant = self.study_variant_id.clone();
                        self.register_active_variant();
                        self.refresh_study_summary();
                    }
                    (Some(case), None) => {
                        let expected = case
                            .material_bindings()
                            .into_iter()
                            .map(|(id, material)| {
                                format!("{id}: {} / {}", material.dataset_id, material.csv_sha256)
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        self.message = (
                            true,
                            format!(
                                "Dataset rejected — id/hash is not declared by this case. Required: {expected}"
                            ),
                        );
                    }
                    (None, _) => {
                        self.message = (
                            true,
                            "No coupled-search case open — nothing to bind the dataset to.".into(),
                        );
                    }
                }
            }
            Ok(JobResult::TablePicked(name, bytes)) => {
                if let Some(draft) = &mut self.material_import {
                    match draft.set_file(name, bytes) {
                        Ok(()) => self.preview_import_table(ctx),
                        Err(error) => draft.error = Some(error),
                    }
                }
            }
            Ok(JobResult::TablePreview(preview)) => {
                if let Some(draft) = &mut self.material_import {
                    draft.set_preview(*preview);
                }
            }
            Ok(JobResult::ImportMetadataLoaded(json)) => {
                if let Some(draft) = &mut self.material_import
                    && let Err(error) = draft.load_metadata(&json)
                {
                    draft.error = Some(error);
                }
            }
            Ok(JobResult::MaterialImported(bundle)) => {
                if let Some(draft) = &mut self.material_import {
                    draft.validated = Some(*bundle);
                    draft.error = None;
                }
            }
            Ok(JobResult::LibraryPicked(path)) => {
                self.library_dir = Some(path);
                self.rescan_library();
                self.persist();
                self.message = (
                    false,
                    format!(
                        "Case library set — {} optcoil file(s) found.",
                        self.library_entries.len()
                    ),
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            Ok(JobResult::CaseWritten(path)) => {
                self.author = None;
                let saved = format!("Saved {}.", path.display());
                // The file just written is exactly what load_project parses.
                self.apply_result(ctx, load_project(path));
                self.message = (false, format!("{saved} Opened — review and run it."));
            }
            // save_case opens the authored case directly on the web —
            // CaseWritten is never produced there.
            #[cfg(target_arch = "wasm32")]
            Ok(JobResult::CaseWritten(_)) => {}
            Ok(JobResult::Exported(path)) => {
                self.message = (false, format!("Saved {}", path.display()))
            }
            Ok(JobResult::Dismissed) => {}
            Err(error) => {
                self.package_after_search = false;
                // A user-cancelled run is an outcome, not a failure.
                let is_cancel = error.contains("cancelled");
                self.message = (!is_cancel, error);
            }
        }
        self.refresh_preflight();
        self.message_fade_start = Instant::now();
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui
                    .add_enabled(
                        self.worker.is_none(),
                        egui::Button::new("Open project…    Ctrl+O"),
                    )
                    .clicked()
                {
                    self.open(ui.ctx(), None);
                    ui.close();
                }
                if ui.add_enabled(self.worker.is_none(), egui::Button::new("Open study workspace…")).clicked() {
                    self.open_study_workspace(ui.ctx());
                    ui.close();
                }
                if self.has_recovered_history() && ui.button("Export recovered historical evidence…").clicked() { self.export_recovered_history(ui.ctx()); ui.close(); }
                if self.recovery.error.is_some() && ui.button("Retry draft autosave").clicked() { self.retry_recovery_save(); ui.close(); }
                if ui.add_enabled(self.worker.is_none(), egui::Button::new("Import spreadsheet / CSV…")).clicked() {
                    self.open_material_import();
                    ui.close();
                }
                if ui.add_enabled(self.worker.is_none() && !self.study_workspace.variants.is_empty(), egui::Button::new("Save study workspace…"))
                    .on_hover_text("Portable workspace with named cases, exact bundle bytes, options and attached results")
                    .clicked() {
                    self.save_study_workspace(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.worker.is_none() && self.author.is_none(),
                        egui::Button::new("New search case…"),
                    )
                    .on_hover_text("Create a supported case or revise requirement, geometry, pack, material, cost, limits and execution inputs with structured controls; Advanced JSON remains available.")
                    .clicked()
                {
                    self.edit_variant = None;
                    self.author = Some(author::CaseDraft::guided());
                    ui.close();
                }
                if self.search_case.is_some() && self.worker.is_none() {
                    if ui.button("Revise current case…").clicked() { self.edit_case(false); ui.close(); }
                    if ui.button("Duplicate current case…").clicked() { self.edit_case(true); ui.close(); }
                }
                ui.menu_button("Examples", |ui| {
                    if ui.add_enabled(self.worker.is_none(), egui::Button::new("Measured conductor first study")).clicked() {
                        self.open_example(ui.ctx(), true); ui.close();
                    }
                    if ui.add_enabled(self.worker.is_none(), egui::Button::new("Synthetic allocation reference")).clicked() {
                        self.open_example(ui.ctx(), false); ui.close();
                    }
                    if ui.add_enabled(self.worker.is_none(), egui::Button::new("Measured supported comparison (2 candidates)")).on_hover_text("Measured OC-007 inputs with unchanged gates. Review its work estimate before running; duration depends on the host.").clicked() {
                        self.apply_result(ui.ctx(), load_project_json(include_str!("../../../benchmarks/self-directed/oc007-two-candidate.json").into(), PathBuf::from("oc007-two-candidate.json")));
                        ui.close();
                    }
                });
                if ui
                    .add_enabled(
                        (self.record.is_some() || self.search_record.is_some())
                            && self.worker.is_none(),
                        egui::Button::new("Export run…    Ctrl+Shift+S"),
                    )
                    .clicked()
                {
                    self.export(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.worker.is_none() && self.search_case.is_some(),
                        egui::Button::new("Queue cases…"),
                    )
                    .on_hover_text(
                        "Run several case files back-to-back — each record writes under runs/",
                    )
                    .clicked()
                {
                    self.queue_cases();
                    ui.close();
                }
                if ui
                    .add_enabled(self.worker.is_none(), egui::Button::new("Save screenshot…"))
                    .on_hover_text("PNG of the app frame")
                    .clicked()
                {
                    self.pick_screenshot();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.search_record.is_some() && self.worker.is_none(),
                        egui::Button::new("Export report…"),
                    )
                    .on_hover_text(
                        "Self-contained HTML evidence report for the coupled-search record",
                    )
                    .clicked()
                {
                    self.export_report(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.search_record.is_some()
                            && self.bom_record.is_some()
                            && self.worker.is_none(),
                        egui::Button::new("Export RFQ…"),
                    )
                    .on_hover_text(
                        "Procurement-facing markdown — piece schedule, spec assignments and \
                         totals from the BOM, with the record's provenance hashes",
                    )
                    .clicked()
                {
                    self.export_rfq(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.search_record.is_some() && self.worker.is_none(),
                        egui::Button::new("Export review package…"),
                    )
                    .on_hover_text(
                        "Includes the case, run record, reports, required dataset bundles, manifest, \
                         and offline verification instructions",
                    )
                    .clicked()
                {
                    self.export_pilot_bundle(ui.ctx());
                    ui.close();
                }
            });
            if !self.recent_files.is_empty() {
                ui.menu_button("Recent", |ui| {
                    for path in self.recent_files.clone().iter().take(8) {
                        let label = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        if ui
                            .add_enabled(self.worker.is_none(), egui::Button::new(label))
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            self.open(ui.ctx(), Some(path.clone()));
                            ui.close();
                        }
                    }
                    if ui.small_button("Clear recent files").clicked() {
                        self.recent_files.clear();
                        self.persist();
                        ui.close();
                    }
                });
            }
            ui.menu_button("Library", |ui| {
                match self.library_dir.clone() {
                    Some(dir) => {
                        ui.small(dir.display().to_string());
                        ui.separator();
                        if self.library_entries.is_empty() {
                            ui.label("No optcoil case files found in this folder.");
                        }
                        for path in self.library_entries.clone() {
                            let label = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string());
                            if ui
                                .add_enabled(
                                    self.worker.is_none(),
                                    egui::Button::new(label),
                                )
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                self.open(ui.ctx(), Some(path.clone()));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Rescan").clicked() {
                            self.rescan_library();
                        }
                        if ui.button("Change folder…").clicked() {
                            self.pick_library(ui.ctx());
                            ui.close();
                        }
                    }
                    None => {
                        ui.label(
                            "Pick a folder of case files — the app scans it shallowly for optcoil-schema JSON.",
                        );
                        if ui.button("Choose folder…").clicked() {
                            self.pick_library(ui.ctx());
                            ui.close();
                        }
                        if PathBuf::from("runs").is_dir()
                            && ui.button("Use ./runs (generated records)").clicked()
                        {
                            self.library_dir = Some(PathBuf::from("runs").canonicalize().unwrap_or_else(|_| PathBuf::from("runs")));
                            self.rescan_library();
                            self.persist();
                            ui.close();
                        }
                    }
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(
                    &mut self.show_inspector,
                    if self.search_case.is_some() {
                        "Candidate inspector"
                    } else {
                        "Module inspector"
                    },
                );
                ui.checkbox(
                    &mut self.watch_enabled,
                    "Watch case file (reload on save)",
                );
                ui.checkbox(&mut self.show_baseline, "Show baseline in plots");
                if ui.button("Reset plot views").clicked() {
                    self.plot_revision += 1;
                    ui.close();
                }
            });
            if ui.button("Tour").clicked() {
                self.tour = Some(0);
            }
            if ui.button("Help    F1").clicked() {
                self.help = !self.help;
            }
            self.suite.header_right(ui);
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(52.0, 52.0)));
            ui.vertical(|ui| {
                ui.label(RichText::new("Converra").size(29.0).color(Color32::BLACK));
                ui.label(
                    RichText::new("AVILA LABS / MAGNET CONDUCTOR OPTIMIZATION")
                        .size(11.0)
                        .color(brand::BLUE),
                );
            });
            ui.separator();
            if ui
                .add_enabled(self.worker.is_none(), egui::Button::new("Open project…"))
                .clicked()
            {
                self.open(ui.ctx(), None);
            }
            if ui
                .add_enabled(
                    self.worker.is_none() && self.author.is_none(),
                    egui::Button::new("New search case…"),
                )
                .on_hover_text("Create a supported case or revise requirement, geometry, pack, material, cost, limits and execution inputs with structured controls; Advanced JSON remains available.")
                .clicked()
            {
                self.author = Some(author::CaseDraft::guided());
            }
            if ui
                .add_enabled(
                    (self.record.is_some() || self.search_record.is_some())
                        && self.worker.is_none(),
                    egui::Button::new("Export run…"),
                )
                .clicked()
            {
                self.export(ui.ctx());
            }
            if self.optimize_button(ui).clicked() {
                self.start(ui.ctx());
            }
            if self.search_case.is_some() {
                ui.checkbox(&mut self.package_after_search, "bundle after run")
                    .on_hover_text(
                        "After completion, export the case, record, report, datasets and manifest; include procurement files when a candidate is selected. Desktop saves a new directory; browser downloads a tar archive.",
                    );
            }
            if let Some(worker) = &self.worker {
                // The browser's search runs on a Web Worker — nothing
                // else wakes egui while it runs, so keep repainting to
                // keep the spinner honest.
                #[cfg(target_arch = "wasm32")]
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                ui.add_space(4.0);
                ui.spinner();
                ui.colored_label(
                    brand::MUTED,
                    match worker.kind {
                        JobKind::Open => "Opening project…",
                        JobKind::Search => {
                            if self.search_case.is_some() {
                                "Running coupled search…"
                            } else {
                                "Searching allocations…"
                            }
                        }
                        JobKind::Export => "Writing record…",
                        JobKind::Verify => "Verifying record…",
                        JobKind::Sweep => "Sweeping the margin frontier…",
                        JobKind::Bakeoff => "Running vendor bake-off…",
                        JobKind::Profile => "Evaluating bore profile…",
                        JobKind::Queue => "Choosing queue files…",
                        JobKind::Workspace => "Loading or saving study workspace…",
                        JobKind::Robustness => "Running named what-if scenarios…",
                        JobKind::Import => "Importing measurement data…",
                    },
                );
                if worker.kind == JobKind::Search
                    && self.search_case.is_some()
                    && let Some(progress) = &self.search_progress
                {
                    let (fraction, done, planned) = progress.fraction();
                    let done_n = progress.candidates_done.load(Ordering::Relaxed);
                    let total_n = progress.candidates_total.load(Ordering::Relaxed);
                    // Bar carries no in-fill text — egui draws it
                    // white-on-light until the fill reaches it. The
                    // detail rides in the muted label below, which is
                    // always readable.
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(260.0)
                            .desired_height(10.0),
                    );
                    let detail = if planned > 0 {
                        format!(
                            "{done} / ~{planned} field evals · {done_n}/{total_n} candidates · {}",
                            progress.phase_label()
                        )
                    } else {
                        progress.phase_label()
                    };
                    ui.colored_label(brand::MUTED, detail);
                }
                if matches!(worker.kind, JobKind::Search | JobKind::Robustness | JobKind::Sweep | JobKind::Import)
                    && ui.button("Cancel").clicked()
                {
                    self.cancel_search();
                }
            }
        });
    }

    /// Primary action button with a short hover-fill animation — the one
    /// piece of motion that is always in view.
    fn optimize_button(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let coupled = self.search_case.is_some();
        // A record-only view has no source case bytes — running is disabled.
        let enabled = self.worker.is_none() && (!coupled || !self.search_json.is_empty());
        let (rect, mut response) = ui.allocate_exact_size(
            egui::vec2(118.0, 36.0),
            if enabled {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        let hover_t =
            ui.ctx()
                .animate_bool_with_time(response.id, response.hovered() && enabled, 0.12);
        let press_t = ui.ctx().animate_bool_with_time(
            response.id.with("press"),
            response.is_pointer_button_down_on(),
            0.08,
        );
        let fill = if enabled {
            brand::mix(brand::BLUE, brand::BLUE_BRIGHT, hover_t)
        } else {
            brand::mix(brand::BASELINE, brand::BLUE, 0.15)
        };
        let rect = rect.translate(egui::vec2(0.0, press_t));
        ui.painter().rect_filled(rect, 8.0, fill);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if coupled {
                "▶  Run search"
            } else {
                "▶  Optimize"
            },
            egui::FontId::proportional(14.0),
            if enabled {
                Color32::WHITE
            } else {
                brand::mix(Color32::WHITE, brand::MUTED, 0.35)
            },
        );
        if !enabled {
            response = response.on_hover_text(if coupled && self.search_json.is_empty() {
                "This is a saved run record — open the case file itself to re-run."
            } else {
                "Wait for the current task to finish."
            });
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                enabled,
                if coupled { "Run search" } else { "Optimize" },
            )
        });
        response
    }

    /// Guided tour: a stepped walkthrough whose buttons perform each step —
    /// open a case, review inputs, run, read the record, export evidence.
    fn tour_window(&mut self, ctx: &egui::Context) {
        let Some(step) = self.tour else { return };
        const STEPS: usize = 6;
        let coupled = self.search_case.is_some();
        let has_results = self.record.is_some() || self.search_record.is_some();
        let busy = self.worker.is_some();

        let mut open = true;
        let mut next = false;
        let mut back = false;
        egui::Window::new("Guided tour")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.small(format!("Step {} of {}", step + 1, STEPS));
                ui.add_space(6.0);
                match step {
                    0 => {
                        ui.heading("Welcome");
                        ui.label(if coupled {
                            "Converra searches conductor-pack and geometry choices against your fixed requirements, then shows which candidates passed, failed, or could not be certified — with the record to prove it."
                        } else {
                            "Converra allocates tape grade and count per coil module against your declared capacity margins — then shows the cost against the baseline you would have built anyway."
                        });
                        ui.label("This tour walks the workflow end to end. Each step can perform its action for you.");
                    }
                    1 => {
                        ui.heading("Get a case in");
                        ui.label("Everything starts from a case file: the requirements, choices, material identity and cost basis, versioned and hash-bound into every record it produces.");
                        ui.horizontal(|ui| {
                            if ui.button("Open project…").clicked() {
                                self.open(ctx, None);
                            }
                            if ui.button("New search case…").clicked() {
                                self.author = Some(author::CaseDraft::guided());
                            }
                        });
                        ui.small("A bundled example is already loaded — you can also drop a .json case onto the window.");
                    }
                    2 => {
                        ui.heading("Review the inputs");
                        ui.label(if coupled {
                            "The Materials page shows the declared dataset and its SHA-256 binding. If it says 'not embedded', load the matching bundle there — a mismatched dataset rejects rather than substitutes."
                        } else {
                            "The Materials page shows each tape grade's declared capacity envelope. Those curves are case inputs — the assumptions the search draws from, not results."
                        });
                        if ui.button("Go to Materials").clicked() {
                            self.page = Page::Materials;
                        }
                    }
                    3 => {
                        ui.heading("Run the search");
                        ui.label(if coupled {
                            "Every declared candidate is screened against the requirement — field, margins, manufacturing limits — and ends PASS, FAIL, INCONCLUSIVE or NOT_EVALUATED. Nothing is silently dropped."
                        } else {
                            "The optimizer searches grade and tape-count allocations within your evaluation budget; every module is checked against the same gates."
                        });
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!busy, egui::Button::new("▶ Run now")).clicked() {
                                self.start(ctx);
                            }
                            if busy {
                                ui.small("A job is already running — the tour can wait.");
                            }
                        });
                    }
                    4 => {
                        ui.heading("Read the record");
                        ui.label(if coupled {
                            "Results land on Overview: the selected screening option when available, the baseline, every candidate's verdict and cost ledger, and acceptance recomputation. Review unresolved checks and next actions; a candidate row opens its limiting-point detail."
                        } else {
                            "Results land on Overview: the optimized allocation beside baseline cost, per-module margins, and any checks the case could not close."
                        });
                        if ui.add_enabled(has_results, egui::Button::new("Go to results")).clicked() {
                            self.page = Page::Overview;
                        }
                        if !has_results {
                            ui.small("Run a search first — this button unlocks when a record exists.");
                        }
                    }
                    _ => {
                        ui.heading("Evidence & export");
                        ui.label("Every run binds its inputs and implementation identities. Export a review package with the original case, record, report, datasets and verification manifest. Uniform-price scenarios are supported for compatible records; graded and purchased-piece costs retain their original ledger.");
                        ui.horizontal(|ui| {
                            if ui.add_enabled(has_results, egui::Button::new("Export report…")).clicked() {
                                self.export_report(ctx);
                            }
                            if ui.button("Reference help  (F1)").clicked() {
                                self.help = true;
                            }
                        });
                        ui.small("Reminder: screening and acceptance are design evidence — final engineering qualification stays with you.");
                    }
                }
                ui.add_space(10.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.add_enabled(step > 0, egui::Button::new("← Back")).clicked() {
                        back = true;
                    }
                    if ui
                        .button(if step + 1 == STEPS { "Finish" } else { "Next →" })
                        .clicked()
                    {
                        next = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Skip tour").clicked() {
                            self.tour = None;
                        }
                    });
                });
            });
        if !open {
            self.tour = None;
        } else if next {
            self.tour = (step + 1 < STEPS).then_some(step + 1);
        } else if back {
            self.tour = Some(step - 1);
        }
    }

    /// 0→1 progress of a short UI animation; pinned to 1 in capture mode so
    /// screenshots never land mid-fade.
    fn anim_progress(&self, start: Instant, duration_ms: u64) -> f32 {
        if self.capture.is_some() {
            return 1.0;
        }
        (start.elapsed().as_secs_f32() / (duration_ms as f32 / 1000.0)).min(1.0)
    }

    /// Eased 0→1 progress of the results reveal delayed by `delay_ms` —
    /// staggers section entrances so a fresh run lands in reading order.
    /// 1 under capture and whenever nothing is gated behind the reveal.
    fn stage(&self, delay_ms: u64) -> f32 {
        if self.capture.is_some() {
            return 1.0;
        }
        let has_results = self.record.is_some() || self.search_record.is_some();
        let Some(start) = self.results_reveal_start else {
            return if has_results { 1.0 } else { 0.0 };
        };
        let elapsed = start.elapsed().as_millis() as u64;
        let t = elapsed.saturating_sub(delay_ms) as f32 / 380.0;
        egui::emath::easing::cubic_out(t.clamp(0.0, 1.0))
    }

    /// Eased 0→1 progress of the results reveal; 1 when nothing is on screen
    /// yet (baseline-only content is not gated behind the reveal).
    fn reveal_progress(&self) -> f32 {
        let has_results = self.record.is_some() || self.search_record.is_some();
        let Some(start) = self.results_reveal_start else {
            return if has_results { 1.0 } else { 0.0 };
        };
        egui::emath::easing::cubic_out(self.anim_progress(start, 520))
    }

    /// True while any UI animation is mid-flight — keeps repaints scheduled.
    fn animating(&self) -> bool {
        self.anim_progress(self.page_fade_start, 240) < 1.0
            || self
                .results_reveal_start
                .is_some_and(|s| self.anim_progress(s, 520 + 480) < 1.0)
            || self.anim_progress(self.inspector_fade_start, 180) < 1.0
            || self.anim_progress(self.message_fade_start, 220) < 1.0
            || self
                .drop_hint_start
                .is_some_and(|s| self.anim_progress(s, 140) < 1.0)
            || self.worker.is_some()
    }

    /// A navigation row: label, hover wash, and — for the active page — the
    /// target for the sliding accent bar drawn after the loop. Returns the
    /// row's center-y so the bar can track it.
    fn nav_item(&mut self, ui: &mut egui::Ui, page: Page, title: &str) -> f32 {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
        let active = self.page == page;
        let hover_t =
            ui.ctx()
                .animate_bool_with_time(response.id.with("wash"), response.hovered(), 0.10);
        if hover_t > 0.0 && !active {
            ui.painter().rect_filled(
                rect.shrink2(egui::vec2(4.0, 2.0)),
                6.0,
                brand::mix(brand::BACKGROUND, brand::status_tint(brand::BLUE), hover_t),
            );
        }
        ui.painter().text(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(13.5),
            if active {
                Color32::BLACK
            } else {
                brand::mix(brand::MUTED, Color32::BLACK, hover_t * 0.6)
            },
        );
        if response.clicked() {
            self.page = page;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        rect.center().y
    }

    fn navigation(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.label(RichText::new("PROJECT").color(brand::BLUE).small());
        let coupled = self.search_case.is_some();
        ui.strong(match &self.search_case {
            Some(case) => &case.id,
            None => &self.case.id,
        });
        ui.label(
            RichText::new(if coupled {
                if self.search_json.is_empty() {
                    "Saved run record"
                } else {
                    "Coupled search project"
                }
            } else if self.path.is_some() {
                "Loaded project"
            } else {
                "Bundled example"
            })
            .color(brand::MUTED),
        );
        ui.add_space(16.0);
        let rail_left = ui.max_rect().left() + 2.0;
        let mut active_y = None;
        for (page, title) in [
            (
                Page::Overview,
                if coupled {
                    "Search results"
                } else {
                    "Design comparison"
                },
            ),
            (Page::Materials, "Materials & operating point"),
            (Page::Checks, "Checks & evidence"),
            (Page::Reports, "Reports & artifacts"),
            (Page::Study, "Engineering study"),
            (Page::Integrations, "Imports & solvers"),
        ] {
            let y = self.nav_item(ui, page, title);
            if self.page == page {
                active_y = Some(y);
            }
        }
        // The one signature motion in the chrome: the accent bar eases to
        // whichever row is active instead of appearing/disappearing.
        if let Some(target) = active_y {
            let y = if self.capture.is_some() {
                target
            } else {
                ui.ctx()
                    .animate_value_with_time(egui::Id::new("nav-indicator"), target, 0.16)
            };
            ui.painter().rect_filled(
                egui::Rect::from_center_size(egui::pos2(rail_left + 1.5, y), egui::vec2(3.0, 22.0)),
                1.5,
                brand::BLUE,
            );
        }
        ui.add_space(22.0);
        ui.separator();
        ui.label(RichText::new("SEARCH").color(brand::BLUE).small());
        if let Some(case) = &self.search_case {
            ui.label("Candidate geometries");
            ui.strong(format!("{}", case.candidate_count()));
            ui.add_space(6.0);
            ui.label("Worker threads");
            ui.label(if cfg!(target_arch = "wasm32") {
                "1 browser search worker".into()
            } else {
                format!("up to {}", case.execution.max_threads)
            });
            ui.add_space(14.0);
            ui.checkbox(&mut self.show_inspector, "Candidate inspector");
        } else {
            ui.label("Maximum evaluations");
            ui.add_enabled(
                self.worker.is_none(),
                egui::DragValue::new(&mut self.max_evaluations)
                    .range(1..=10_000_000)
                    .speed(100.0),
            )
            .on_hover_text("A budget limit can stop search before a model optimum is established.");
            ui.add_space(14.0);
            ui.checkbox(&mut self.show_baseline, "Plot baseline");
            ui.checkbox(&mut self.show_inspector, "Module inspector");
            if ui.button("Reset plots").clicked() {
                self.plot_revision += 1;
            }
        }
        ui.add_space(22.0);
        ui.label(RichText::new("INPUT").color(brand::BLUE).small());
        ui.label(self.path.as_ref().map_or("Bundled example".into(), |p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }))
        .on_hover_text(
            self.path
                .as_ref()
                .map_or("Embedded reference case".into(), |p| {
                    p.display().to_string()
                }),
        );
        ui.small("Drop a Converra case file anywhere to open it.");
        ui.add_space(12.0);
        ui.colored_label(
            brand::MUTED,
            match &self.search_case {
                Some(case) => format!(
                    "Material dataset: {}. Cost figures use the case's declared pricing.",
                    case.material.dataset_id
                ),
                None => "Current results use invented material and cost data.".into(),
            },
        );
    }
}

impl eframe::App for Workbench {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.native_close_guard(&ctx);
        self.recovery_tick();
        self.recovery_window(&ctx);
        self.import_window(&ctx);
        // egui redraws on events; a live worker produces none, so ask for
        // periodic repaints while one is in flight — this drives the
        // search progress bar and spinner.
        if self.worker.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        // Screenshot reply — the frame image lands as an event; write it
        // to the picked path on a worker (PNG encode off the UI thread).
        if self.screenshot_target.is_some()
            && let Some(image) = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            })
        {
            let path = self.screenshot_target.take().unwrap();
            // Browser: encode the PNG and offer it as a download — there
            // is no file path to write to.
            #[cfg(target_arch = "wasm32")]
            {
                let mut bytes = Vec::new();
                let [w, h] = image.size;
                if image::write_buffer_with_format(
                    &mut std::io::Cursor::new(&mut bytes),
                    image.as_raw(),
                    w as u32,
                    h as u32,
                    image::ExtendedColorType::Rgba8,
                    image::ImageFormat::Png,
                )
                .is_ok()
                {
                    web::download_bytes(&path.to_string_lossy(), &bytes);
                    self.message = (false, "Screenshot downloaded.".into());
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            self.launch(&ctx, JobKind::Export, move |_| {
                let [w, h] = image.size;
                image::save_buffer(
                    &path,
                    image.as_raw(),
                    w as u32,
                    h as u32,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
                Ok(JobResult::Exported(path))
            });
        }
        // Watch mode — reload the open file when its mtime moves.
        // Throttled to ~3/4 s; reloading through open() keeps the
        // worker-thread discipline. Dropped/picked bytes have no live
        // file to watch on the web — the toggle is hidden there.
        #[cfg(not(target_arch = "wasm32"))]
        if self.watch_enabled
            && self.worker.is_none()
            && let Some(path) = self.path.clone()
        {
            let fresh = fs::metadata(&path).and_then(|m| m.modified()).ok();
            if fresh.is_some() && fresh != self.watch_mtime {
                self.watch_mtime = fresh;
                self.open(&ctx, Some(path));
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O))
            && self.worker.is_none()
        {
            self.open(&ctx, None);
        }
        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            )
        }) && self.worker.is_none()
        {
            self.export(&ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
            self.help = !self.help;
        }
        // ↑/↓ walk the candidate table's cost order — keyboard browsing
        // through results without reaching for the mouse.
        if self.search_record.is_some()
            && self.page == Page::Overview
            && !ctx.egui_wants_keyboard_input()
        {
            let step = if ctx
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                1i64
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                -1i64
            } else {
                0
            };
            if step != 0 {
                let order = self.sorted_candidates();
                if !order.is_empty() {
                    let pos = order
                        .iter()
                        .position(|&i| i == self.selected_candidate)
                        .unwrap_or(0) as i64;
                    let next = (pos + step).rem_euclid(order.len() as i64);
                    self.selected_candidate = order[next as usize];
                }
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            if self.worker.is_some() {
                self.message = (
                    true,
                    "Finish the current task before opening another project.".into(),
                );
            } else if dropped.len() != 1 {
                self.message = (true, "Open one Converra project at a time.".into());
            } else {
                // Browser drops carry no path — read bytes async.
                #[cfg(target_arch = "wasm32")]
                {
                    let file = dropped[0].clone();
                    let name = file
                        .path()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "dropped.json".into());
                    self.launch_wasm(&ctx, JobKind::Open, async move {
                        let bytes = file.bytes_async().await?;
                        let json = String::from_utf8(bytes)
                            .map_err(|e| format!("Cannot read {name}: {e}"))?;
                        load_project_json(json, PathBuf::from(name))
                    });
                }
                #[cfg(not(target_arch = "wasm32"))]
                self.open(&ctx, Some(dropped[0].path().to_path_buf()));
            }
        }
        if let Some(capture) = &self.capture {
            self.page = capture.page();
        }
        // The case builder is a modal window over whichever page is active.
        if let Some(draft) = &mut self.author {
            let result = draft.show(&ctx);
            let closed = draft.closed();
            if let Some((_case, json)) = result {
                if self.worker.is_some() {
                    self.message = (true, "Finish or cancel the current task before accepting the case. Your draft is retained.".into());
                } else {
                    self.pending_import_bundle = self
                        .author
                        .as_ref()
                        .and_then(|draft| draft.imported_bundle_json().map(str::to_owned));
                    self.save_case(&ctx, json);
                }
            }
            if closed {
                self.author = None;
                self.edit_variant = None;
            }
        }
        if self.page != self.last_page {
            self.last_page = self.page;
            self.page_fade_start = Instant::now();
        }
        if self.selected_module != self.last_selected_module
            || self.selected_candidate != self.last_selected_candidate
        {
            self.last_selected_module = self.selected_module;
            self.last_selected_candidate = self.selected_candidate;
            self.inspector_fade_start = Instant::now();
        }
        let header_panel = egui::Panel::top("header").show(ui, |ui| {
            self.header(ui);
            // Indeterminate progress shimmer while a worker runs: a short
            // bright segment sweeping the header's bottom edge.
            if self.worker.is_some() {
                let rect = ui.max_rect();
                let t = ui.ctx().input(|i| i.time) % 1.4;
                let frac = (t / 1.4) as f32;
                let width = rect.width() * 0.22;
                let x = rect.left() + frac * (rect.width() + width) - width;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x.max(rect.left()), rect.bottom() - 2.0),
                        egui::vec2(width, 2.0),
                    ),
                    1.0,
                    brand::BLUE,
                );
            }
        });
        self.suite
            .prompt(ui.ctx(), header_panel.response.rect.bottom());
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(notice) = self.suite.take_notice() {
            self.message = (true, notice);
        }
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(if self.message.0 {
                        brand::status_tint(Color32::from_rgb(166, 35, 41))
                    } else {
                        Color32::WHITE
                    })
                    .inner_margin(egui::Margin::symmetric(12, 7)),
            )
            .show(ui, |ui| {
                // New messages ease in so status changes register without
                // flashing — a short fade on the whole strip's content.
                ui.set_opacity(egui::emath::easing::cubic_out(
                    self.anim_progress(self.message_fade_start, 220),
                ));
                ui.horizontal_wrapped(|ui| {
                    if self.worker.is_some() {
                        // Gentle pulsing marker next to the status text.
                        let pulse = ((ui.ctx().input(|i| i.time) * 2.4).sin() as f32 + 1.0) * 0.5;
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            rect.center(),
                            3.0 + pulse,
                            brand::mix(brand::BLUE, brand::BLUE_BRIGHT, pulse),
                        );
                        ui.add_space(4.0);
                    }
                    ui.colored_label(
                        if self.message.0 {
                            Color32::from_rgb(166, 35, 41)
                        } else {
                            brand::MUTED
                        },
                        &self.message.1,
                    );
                    if self.recovery.enabled_for_ui() {
                        ui.separator();
                        ui.label(self.recovery_label());
                    }
                });
            });
        egui::Panel::left("navigation")
            .default_size(220.0)
            .min_size(185.0)
            .max_size(300.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("navigation-scroll")
                    .show(ui, |ui| self.navigation(ui));
            });
        let side_inspector = self.show_inspector && ui.available_width() > 1000.0;
        if side_inspector {
            egui::Panel::right("inspector")
                .default_size(260.0)
                .min_size(220.0)
                .max_size(360.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("inspector-scroll")
                        .show(ui, |ui| self.inspector(ui));
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            // Short fade on page switch — content only, navigation stays put.
            let fade =
                egui::emath::easing::cubic_out(self.anim_progress(self.page_fade_start, 240));
            egui::ScrollArea::vertical()
                .id_salt(("workspace-scroll", self.page))
                .show(ui, |ui| {
                    ui.set_opacity(fade);
                    ui.add_space((1.0 - fade) * 10.0);
                    match self.page {
                        Page::Overview => {
                            if self.search_case.is_some() {
                                self.search_overview(ui);
                            } else {
                                self.overview(ui);
                            }
                        }
                        Page::Materials => {
                            if self.search_case.is_some() {
                                self.search_materials(ui);
                            } else {
                                self.materials(ui);
                            }
                        }
                        Page::Checks => {
                            if self.search_case.is_some() {
                                self.search_checks(ui);
                            } else {
                                self.checks(ui);
                            }
                        }
                        Page::Reports => self.reports(ui),
                        Page::Study => self.study(ui),
                        Page::Integrations => self.integrations(ui),
                    }
                    if self.show_inspector && !side_inspector {
                        ui.add_space(16.0);
                        ui.collapsing(
                            if self.search_case.is_some() {
                                "Selected candidate details"
                            } else {
                                "Selected module details"
                            },
                            |ui| self.inspector(ui),
                        );
                    }
                });
        });
        self.tour_window(&ctx);
        if self.help {
            egui::Window::new("Using Converra").open(&mut self.help).default_width(490.0).show(ui, |ui| {
                ui.heading("Complete a supported coil study");
                ui.hyperlink_to("Converra Handbook", "https://converra.avilalabs.org/docs/");
                ui.label("1. Open a case or select the measured first study from File → Examples.\n2. Review inputs and applicability, then run.\n3. Inspect the decision and unresolved checks.\n4. Revise or duplicate the case, compare inputs, and export a review package. Prices in the examples are illustrative.");
                ui.separator();
                ui.label("Plots: drag to pan, scroll/pinch to zoom, double-click to reset. Click legend entries to hide a series. Hover data for values.");
                ui.label("Tables: click a module row to inspect it. Drag column boundaries to resize. Filter by module name or sort by field.");
                ui.label("Ctrl+O: open project · Ctrl+Shift+S: export run · F1: help");
                ui.label("File → Import spreadsheet / CSV previews measurement tables, maps columns and units, and validates source declarations. Validated imports stay with the case and study. Materials also accepts dataset bundles or metadata/CSV pairs. Drafts are protected on this device; export a study workspace for a portable copy. Declared field maps use the case builder. Browser calculations use background workers; folder features require desktop. STEP and native solver APIs remain planned.");
            });
        }
        let files_hovered = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if files_hovered && self.drop_hint_start.is_none() {
            self.drop_hint_start = Some(Instant::now());
        } else if !files_hovered {
            self.drop_hint_start = None;
        }
        if let Some(start) = self.drop_hint_start {
            // Ease the hint in — a hard pop reads as a glitch, not guidance.
            let fade = egui::emath::easing::cubic_out(self.anim_progress(start, 140));
            egui::Area::new(egui::Id::new("drop-hint"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .order(egui::Order::Foreground)
                .show(ui, |ui| {
                    ui.set_opacity(fade);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.heading("Drop a Converra project to open");
                    });
                });
        }
        if self.animating() {
            // ~60fps while anything animates or a worker runs; the app is
            // fully idle otherwise.
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.recovery_tick();
        #[cfg(target_arch = "wasm32")]
        self.publish_browser_status();
        if let Some(capture) = &mut self.capture {
            // Coupled pages render meaningfully before a run (case summary),
            // so a loaded coupled project counts as ready immediately.
            let ready = self.record.is_some() || self.search_case.is_some();
            capture.finish(&ctx, ready);
        }
    }
}

impl Drop for Workbench {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.flush_recovery_on_close();
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Relaxed);
            #[cfg(target_arch = "wasm32")]
            if let Some(w) = worker.web_worker.as_ref() {
                w.terminate();
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = self.browser_search_worker.take() {
            worker.terminate();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_project(path: PathBuf) -> Result<JobResult, String> {
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if extension != "json" {
        return Err(format!(
            ".{extension} import is not available yet. Open a Converra case (.json); see Imports & solvers for the format roadmap."
        ));
    }
    let json =
        fs::read_to_string(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    load_project_json(json, path)
}

/// Shared parse dispatch — `label` identifies the file for display and
/// recents (a real path natively, a picker/drop filename in the browser).
fn load_project_json(json: String, label: PathBuf) -> Result<JobResult, String> {
    // Dispatch on the declared schema: coupled-search cases and saved
    // coupled-search run records are distinct document kinds.
    let schema = serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v.get("schema")?.as_str().map(str::to_owned));
    match schema.as_deref() {
        Some(s) if s.starts_with("optcoil-coupled-search-run/") => {
            let record: CoupledSearchRunRecord = serde_json::from_str(&json)
                .map_err(|e| format!("Run record was not opened: {e}"))?;
            Ok(JobResult::LoadedSearchRun(Box::new(record), label))
        }
        Some(s) if s.starts_with("optcoil-coupled-search/") => {
            let case = CoupledSearchCase::from_json(&json)
                .map_err(|e| format!("Project was not opened: {e}"))?;
            Ok(JobResult::LoadedSearch(Box::new((case, json)), label))
        }
        _ => {
            let case =
                Case::from_json(&json).map_err(|e| format!("Project was not opened: {e}"))?;
            let baseline = acceptance::assess(&case, &case.baseline).map_err(|e| e.to_string())?;
            Ok(JobResult::Loaded(Box::new((case, baseline)), label))
        }
    }
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::Inconclusive => "INCONCLUSIVE",
        Status::NotEvaluated => "NOT_EVALUATED",
    }
}

fn status_color(status: Status) -> Color32 {
    match status {
        Status::Pass => Color32::from_rgb(22, 112, 78),
        Status::Fail => Color32::from_rgb(166, 35, 41),
        Status::Inconclusive => Color32::from_rgb(148, 89, 0),
        Status::NotEvaluated => brand::MUTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_open_preserves_results_and_successful_open_invalidates_them() {
        let ctx = egui::Context::default();
        let mut app = Workbench::new(&ctx).unwrap();
        let record = optcoil_search::run(&app.case, &SearchOptions::default()).unwrap();
        let identity = record.input_sha256.clone();
        app.record = Some(record);
        app.selected_module = 2;
        app.module_filter = "high".into();
        app.apply_result(
            &egui::Context::default(),
            load_project(PathBuf::from("unimplemented.step")),
        );
        assert!(app.message.0);
        assert_eq!(app.record.as_ref().unwrap().input_sha256, identity);
        assert_eq!(app.selected_module, 2);

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/synthetic/oc-001.json");
        app.apply_result(&egui::Context::default(), load_project(path));
        assert!(!app.message.0);
        assert!(app.record.is_none());
        assert_eq!(app.selected_module, 0);
        assert!(app.module_filter.is_empty());
        assert_eq!(app.baseline.cost.total_usd, 932.0);
    }

    #[test]
    fn coupled_search_case_opens_in_search_mode_and_reopen_exits_it() {
        let ctx = egui::Context::default();
        let mut app = Workbench::new(&ctx).unwrap();
        let path = std::env::temp_dir().join(format!(
            "optcoil-app-test-oc007-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, optcoil_model::coupled_search::OC007_JSON).unwrap();
        app.apply_result(&egui::Context::default(), load_project(path.clone()));
        let _ = std::fs::remove_file(&path);
        assert!(!app.message.0, "{}", app.message.1);
        assert_eq!(app.search_case.as_ref().unwrap().id, "oc-007");
        assert!(!app.search_json.is_empty());
        assert!(app.search_record.is_none());
        assert!(app.record.is_none());

        // Re-opening an allocation project leaves coupled-search mode.
        let alloc = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/synthetic/oc-001.json");
        app.apply_result(&egui::Context::default(), load_project(alloc));
        assert!(app.search_case.is_none());
        assert!(app.search_json.is_empty());
        assert_eq!(app.case.id, "oc-001");
    }

    #[test]
    fn coupled_run_record_files_route_to_the_record_view() {
        // Dispatch is by declared schema; a truncated record fails in the
        // record arm, not the case arm.
        let path = std::env::temp_dir().join(format!(
            "optcoil-app-test-record-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"schema":"optcoil-coupled-search-run/v9"}"#).unwrap();
        let result = load_project(path.clone());
        let _ = std::fs::remove_file(&path);
        match result {
            Err(error) => assert!(error.contains("Run record"), "{error}"),
            Ok(_) => panic!("a truncated run record must not open"),
        }
    }

    #[test]
    fn dismissing_file_dialog_keeps_the_active_project() {
        let ctx = egui::Context::default();
        let mut app = Workbench::new(&ctx).unwrap();
        app.record = Some(optcoil_search::run(&app.case, &SearchOptions::default()).unwrap());
        app.apply_result(&egui::Context::default(), Ok(JobResult::Dismissed));
        assert!(app.record.is_some());
        assert_eq!(app.case.id, "oc-001");
    }

    #[test]
    fn piece_priced_ledgers_are_never_repriced_by_the_slider() {
        // The oc-027 defect: the $/m slider's legacy closed form produced a
        // hybrid total on v24 records — installed x price x (1+scrap) with
        // splice-inclusive joints. A piece-catalogue ledger must always
        // display its own total_usd, whatever the slider says.
        let piece_ledger = optcoil_search::coupled_search::SearchCostLedger {
            installed_length_m: 268_800.0,
            purchased_length_m: 560_000.0,
            conductor_usd: 8_601_600.0,
            scrap_usd: 9_318_400.0,
            assembly_usd: 8_000.0,
            joints_usd: 2_003_000.0,
            total_usd: 19_931_000.0,
            opex_usd: None,
            lifecycle_usd: None,
            module_joints: Some(15),
            piece_splices: Some(4000),
            spec_splices: Some(0),
            pieces_bought: Some(8000),
            remnant_length_m: Some(264_320.0),
            piece_plan: Some(Vec::new()),
        };
        for hypothetical in [1.0, 30.0, 60.0, 90.0] {
            assert_eq!(
                views::repriced_total_usd(&piece_ledger, hypothetical, 0.1),
                19_931_000.0,
                "slider price {hypothetical} moved a piece-catalogue ledger"
            );
        }
        // And the legacy path still reprices for pre-v24 ledgers.
        let legacy = optcoil_search::coupled_search::SearchCostLedger {
            installed_length_m: 100.0,
            purchased_length_m: 110.0,
            conductor_usd: 3000.0,
            scrap_usd: 300.0,
            assembly_usd: 50.0,
            joints_usd: 20.0,
            total_usd: 3370.0,
            opex_usd: None,
            lifecycle_usd: None,
            module_joints: None,
            piece_splices: None,
            spec_splices: None,
            pieces_bought: None,
            remnant_length_m: None,
            piece_plan: None,
        };
        assert!((views::repriced_total_usd(&legacy, 60.0, 0.1) - 6670.0).abs() < 1e-6);
    }
}
