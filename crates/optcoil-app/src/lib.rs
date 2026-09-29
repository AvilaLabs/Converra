mod author;
mod brand;
mod capture;
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
    verify::{self, CheckLine},
};
// Searches launch only on native builds — the browser refuses them
// before these are referenced.
#[cfg(not(target_arch = "wasm32"))]
use optcoil_search::{
    SearchOptions, coupled_search::run_coupled_search_case_with_dataset_progress, run_cancellable,
};
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

/// Native entry point — the browser build enters through `web::start_web`.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(brand::LOGO).expect("embedded Avila Labs icon");
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

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Materials,
    Checks,
    /// Decision artifacts computed from a coupled-search record —
    /// BOM, margin frontier, vendor bake-off, integrity verify.
    Reports,
    Integrations,
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
    /// A material dataset bundle the user picked explicitly (coupled mode).
    DatasetLoaded(Box<MaterialBundle>, PathBuf),
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
enum DatasetOrigin {
    Embedded,
    File(PathBuf),
}

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
    preflight: Option<optcoil_search::preflight::StudyPreflight>,
    /// The coupled-search case builder window, while open.
    author: Option<author::CaseDraft>,
    search_record: Option<CoupledSearchRunRecord>,
    selected_candidate: usize,
    worker: Option<Worker>,
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
            preflight: None,
            author: None,
            search_record: None,
            selected_candidate: 0,
            worker: None,
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
            let mut datasets = std::collections::BTreeMap::new();
            if let Some(source) = &self.search_dataset {
                datasets.insert(source.dataset.metadata.id.clone(), source.dataset.clone());
            }
            optcoil_search::preflight::preflight_coupled_search(
                case,
                Some(&datasets),
                &Default::default(),
            )
        });
    }

    fn edit_case(&mut self, duplicate: bool) {
        let Some(case) = self.search_case.as_ref() else {
            return;
        };
        let draft = if duplicate {
            author::CaseDraft::duplicate(case)
        } else {
            let json = self.case_json().unwrap_or_default();
            author::CaseDraft::revision(&json)
        };
        match draft {
            Ok(draft) => self.author = Some(draft),
            Err(error) => self.message = (true, error),
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        thread::spawn(move || {
            let _ = sender.send(work(worker_cancel));
            ctx.request_repaint();
        });
        // The browser is single-threaded: cheap jobs (open, verify,
        // export) run inline and complete inside the frame. Long jobs —
        // Search, Sweep, Bakeoff — are refused by their callers before
        // reaching here.
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
        json: String,
        dataset: Option<MaterialDataset>,
    ) {
        use wasm_bindgen::JsCast as _;
        if self.worker.is_some() {
            return;
        }
        let options = web_sys::WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);
        let web_worker = match web_sys::Worker::new_with_options("./worker.js", &options) {
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
            "case_json": json,
            "dataset_json": dataset
                .and_then(|d| serde_json::to_string(&d).ok()),
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
        let Some(worker) = self.worker.as_ref().filter(|w| w.kind == JobKind::Search) else {
            return;
        };
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
                "Search cancelled. The previous completed result is retained.".into(),
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
                if self.search_json.is_empty() {
                    return;
                }
                if self.search_dataset.is_none() {
                    self.message = (
                        true,
                        format!(
                            "Declared dataset '{}' is not embedded — load a dataset bundle on the Materials page first.",
                            self.search_case
                                .as_ref()
                                .map(|c| c.material.dataset_id.as_str())
                                .unwrap_or("?")
                        ),
                    );
                    return;
                }
                let json = self.search_json.clone();
                let dataset = self
                    .search_dataset
                    .as_ref()
                    .map(|source| source.dataset.clone());
                self.message = (
                    false,
                    "Searching in a browser worker — single-threaded, so a real grid takes a while; the page stays responsive…".into(),
                );
                self.launch_search_worker(ctx, json, dataset);
            }
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.search_case.is_some() {
            if self.search_json.is_empty() {
                // A record view has no source case bytes to hash and run.
                return;
            }
            let json = self.search_json.clone();
            if self.search_dataset.is_none() {
                self.message = (
                    true,
                    format!(
                        "Declared dataset '{}' is not embedded — load a dataset bundle on the Materials page first.",
                        self.search_case
                            .as_ref()
                            .map(|c| c.material.dataset_id.as_str())
                            .unwrap_or("?")
                    ),
                );
                return;
            }
            let dataset = self
                .search_dataset
                .as_ref()
                .map(|source| source.dataset.clone());
            self.message = (
                false,
                "Running coupled candidate search — every geometry is screened against the requirement…".into(),
            );
            let progress = Arc::new(SearchProgress::new());
            self.search_progress = Some(progress.clone());
            self.launch(ctx, JobKind::Search, move |cancel| {
                run_coupled_search_case_with_dataset_progress(
                    &json,
                    &CoupledSearchOptions { threads: None },
                    dataset,
                    &cancel,
                    Some(&progress),
                )
                .map(|r| JobResult::SearchCompleted(Box::new(r)))
                .map_err(|e| e.to_string())
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
        self.launch(&ctx, JobKind::Verify, move |_| {
            verify::verify_record_checks(&record_json, case_json.as_deref(), None, None, &[])
                .map(JobResult::Verified)
                .map_err(|e| e.to_string())
        });
    }

    /// Utilization-limit frontier — the workbench synthesizes a v3
    /// sensitivity spec over a default margin grid and runs the sweep.
    fn run_frontier(&mut self) {
        let Some(case_json) = self.case_json() else {
            return;
        };
        let dataset = self
            .search_dataset
            .as_ref()
            .map(|source| source.dataset.clone());
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
        self.launch(&ctx, JobKind::Sweep, move |cancel| {
            optcoil_search::sensitivity::run_sensitivity_sweep(
                &case_json,
                &spec.to_string(),
                &CoupledSearchOptions { threads: None },
                dataset,
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
            Ok(JobResult::DatasetLoaded(Box::new(bundle), path))
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
            Ok(JobResult::DatasetLoaded(Box::new(bundle), metadata))
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
            Ok(JobResult::DatasetLoaded(
                Box::new(bundle),
                PathBuf::from(name),
            ))
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn load_dataset(&mut self, ctx: &egui::Context) {
        self.launch_wasm(ctx, JobKind::Open, async move {
            let Some((name, bytes)) = web::pick_bytes(&["json"]).await? else {
                return Ok(JobResult::Dismissed);
            };
            let bundle = MaterialBundle::from_json(&String::from_utf8_lossy(&bytes))
                .map_err(|e| e.to_string())?;
            Ok(JobResult::DatasetLoaded(
                Box::new(bundle),
                PathBuf::from(name),
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
        let mut bundles = Vec::new();
        if let Some(text) = self
            .search_dataset
            .as_ref()
            .and_then(|source| source.bundle_json.as_deref())
        {
            bundles.push(MaterialBundle::from_json(text).map_err(|e| e.to_string())?);
        }
        optcoil_search::review::review_package(
            &self.search_record_json,
            (!self.search_json.is_empty()).then_some(self.search_json.as_str()),
            &bundles,
        )
        .map_err(|e| e.to_string())
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
        let result = match worker.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("Background task stopped unexpectedly.".into()),
        };
        self.worker = None;
        self.search_progress = None;
        self.apply_result(ctx, result);
    }

    fn apply_result(&mut self, ctx: &egui::Context, result: Result<JobResult, String>) {
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
                let previous_dataset = self
                    .search_dataset
                    .take()
                    .filter(|source| case.validate_against_dataset(&source.dataset).is_ok());
                self.search_dataset = previous_dataset.or_else(|| resolve_dataset(&case, &path));
                self.search_case = Some(case);
                self.search_json = json;
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
                    if let Some(case) = &self.search_case {
                        let previous = self.search_dataset.take().filter(|source| {
                            case.validate_against_dataset(&source.dataset).is_ok()
                        });
                        self.search_dataset = previous.or_else(|| resolve_dataset(case, &path));
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
            Ok(JobResult::DatasetLoaded(bundle, path)) => {
                let bundle = *bundle;
                let verdict = self
                    .search_case
                    .as_ref()
                    .map(|case| case.validate_against_dataset(&bundle.dataset));
                match verdict {
                    Some(Ok(())) => {
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
                        let bundle_json = bundle.to_json().ok();
                        self.search_dataset = Some(DatasetSource {
                            bundle_json,
                            dataset: bundle.dataset,
                            origin: DatasetOrigin::File(path),
                            attestation: bundle.attestation,
                        });
                    }
                    Some(Err(e)) => {
                        self.message = (true, format!("Dataset rejected — {e}"));
                    }
                    None => {
                        self.message = (
                            true,
                            "No coupled-search case open — nothing to bind the dataset to.".into(),
                        );
                    }
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
                if ui
                    .add_enabled(
                        self.worker.is_none() && self.author.is_none(),
                        egui::Button::new("New search case…"),
                    )
                    .on_hover_text("Create a supported coupled-search case; revise loaded cases with the advanced editor")
                    .clicked()
                {
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
                        egui::Button::new("Pilot bundle…"),
                    )
                    .on_hover_text(
                        "Write one directory: case.json + record.json + bom.json + rfq.md — the \
                         auditable package for a reviewer or vendor",
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
                .on_hover_text("Create a supported coupled-search case; revise loaded cases with the advanced editor")
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
                if worker.kind == JobKind::Search && ui.button("Cancel").clicked() {
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
            ui.label(format!("up to {}", case.execution.max_threads));
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
                self.save_case(&ctx, json);
            }
            if closed {
                self.author = None;
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
        egui::Panel::top("header").show(ui, |ui| {
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
                .id_salt("workspace-scroll")
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
                ui.label("1. Open a case or select the measured first study from File → Examples.\n2. Review inputs and applicability, then run.\n3. Inspect the decision and unresolved checks.\n4. Revise or duplicate the case, compare inputs, and export a review package. Prices in the examples are illustrative.");
                ui.separator();
                ui.label("Plots: drag to pan, scroll/pinch to zoom, double-click to reset. Click legend entries to hide a series. Hover data for values.");
                ui.label("Tables: click a module row to inspect it. Drag column boundaries to resize. Filter by module name or sort by field.");
                ui.label("Ctrl+O: open project · Ctrl+Shift+S: export run · F1: help");
                ui.label("Import accepts allocation cases, coupled-search cases and saved run records. Materials accepts validated dataset bundles or metadata/CSV pairs. Declared field maps use the case builder. Browser searches use a background worker; folder features require desktop. STEP, arbitrary Excel column mapping and native solver APIs are planned.");
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
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Relaxed);
            #[cfg(target_arch = "wasm32")]
            if let Some(w) = worker.web_worker.as_ref() {
                w.terminate();
            }
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
