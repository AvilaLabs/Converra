use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use optcoil_model::material::MaterialDataset;
use optcoil_search::{
    coupled_search::{CoupledSearchOptions, SearchProgress},
    review,
    robustness::{RobustnessSpec, robustness_preflight, run_study_robustness},
    sensitivity::run_sensitivity_sweep_with_datasets,
    study::{FollowUpAxis, StudyEngineSession, StudyWorkspace, exact_input_key},
    verify::{self, Outcome},
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerConfig,
    },
    schemars::JsonSchema,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_JOBS: usize = 32;
const MAX_CANDIDATES: usize = 256;
const MAX_REFINED_KERNEL_PROXY: u128 = 10_000_000_000;
const MAX_SENSITIVITY_POINTS: usize = 64;
const MAX_JOB_WALL_TIME: Duration = Duration::from_secs(15 * 60);
const MAX_SENSITIVITY_SPEC_BYTES: usize = 256 * 1024;
const MAX_ARTIFACT_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;
const WORKSPACE_FILE: &str = "workspace.json";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct ServerState(Arc<Inner>);

struct Inner {
    root: PathBuf,
    workspace_path: PathBuf,
    workspace: Mutex<StudyWorkspace>,
    engine: Mutex<StudyEngineSession>,
    export_resources: Mutex<BTreeMap<String, String>>,
    jobs: Mutex<BTreeMap<String, Arc<JobControl>>>,
    active_job: Mutex<Option<String>>,
    mutation_gate: Mutex<()>,
    next_job: AtomicU64,
    next_export: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum JobPhase {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
struct JobView {
    job_id: String,
    kind: String,
    phase: JobPhase,
    progress_fraction: f32,
    progress_done: u64,
    progress_planned: u64,
    phase_label: String,
    reused_calculation: bool,
    result_id: Option<String>,
    result_resource: Option<String>,
    error: Option<String>,
}

struct JobControl {
    cancel: Arc<AtomicBool>,
    progress: Arc<SearchProgress>,
    view: Mutex<JobView>,
    join: Mutex<Option<JoinHandle<()>>>,
    monitor: Mutex<Option<JoinHandle<()>>>,
    wall_expired: AtomicBool,
}

impl JobControl {
    fn new(job_id: String, kind: &str) -> Self {
        let progress = Arc::new(SearchProgress::new());
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            progress,
            view: Mutex::new(JobView {
                job_id,
                kind: kind.into(),
                phase: JobPhase::Running,
                progress_fraction: 0.0,
                progress_done: 0,
                progress_planned: 0,
                phase_label: "Preparing".into(),
                reused_calculation: false,
                result_id: None,
                result_resource: None,
                error: None,
            }),
            join: Mutex::new(None),
            monitor: Mutex::new(None),
            wall_expired: AtomicBool::new(false),
        }
    }

    fn snapshot(&self) -> JobView {
        let (fraction, done, planned) = self.progress.fraction();
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(view.phase, JobPhase::Running) {
            if view.kind == "sensitivity" || view.kind == "robustness" {
                view.progress_fraction = 0.0;
                view.progress_done = 0;
                view.progress_planned = 0;
                view.phase_label = format!("{} running; aggregate progress unavailable", view.kind);
            } else {
                view.progress_fraction = fraction;
                view.progress_done = done;
                view.progress_planned = planned;
                view.phase_label = self.progress.phase_label();
            }
        }
        view.clone()
    }
}

struct WorkerTerminalGuard {
    state: ServerState,
    job: Arc<JobControl>,
}

impl Drop for WorkerTerminalGuard {
    fn drop(&mut self) {
        if matches!(self.job.snapshot().phase, JobPhase::Running) {
            self.state.finish_job(
                &self.job,
                JobPhase::Failed,
                None,
                Some("worker exited unexpectedly before reporting a terminal result".into()),
            );
        }
    }
}

impl ServerState {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        fs::create_dir_all(path.as_ref())?;
        let root = fs::canonicalize(path.as_ref())?;
        let workspace_path = root.join(WORKSPACE_FILE);
        let workspace = match fs::symlink_metadata(&workspace_path) {
            Ok(file_metadata) => {
                if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
                    return Err("workspace.json must be a regular file inside the explicit workspace directory".into());
                }
                if fs::canonicalize(&workspace_path)?.parent() != Some(root.as_path()) {
                    return Err(
                        "workspace.json resolves outside the explicit workspace directory".into(),
                    );
                }
                let metadata = fs::metadata(&workspace_path)?;
                if metadata.len() > 64 * 1024 * 1024 {
                    return Err("workspace.json exceeds the 64 MiB limit".into());
                }
                StudyWorkspace::from_json(&fs::read_to_string(&workspace_path)?)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let workspace = StudyWorkspace::new("Converra engineering study");
                write_new(&workspace_path, workspace.to_json()?.as_bytes())?;
                workspace
            }
            Err(error) => return Err(error.into()),
        };
        fs::create_dir_all(root.join("exports"))?;
        let exports_root = fs::canonicalize(root.join("exports"))?;
        if !exports_root.starts_with(&root) {
            return Err("exports directory must stay inside the workspace directory".into());
        }
        Ok(Self(Arc::new(Inner {
            root,
            workspace_path,
            workspace: Mutex::new(workspace),
            engine: Mutex::new(StudyEngineSession::default()),
            export_resources: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
            active_job: Mutex::new(None),
            mutation_gate: Mutex::new(()),
            next_job: AtomicU64::new(1),
            next_export: AtomicU64::new(1),
        })))
    }

    fn workspace_json(&self) -> Result<String, String> {
        self.0
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned".to_owned())?
            .to_json()
            .map_err(|e| e.to_string())
    }

    fn persist(&self, workspace: &StudyWorkspace) -> Result<(), String> {
        let json = workspace.to_json().map_err(|e| e.to_string())?;
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp = self.0.root.join(format!(
            ".workspace.json.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|e| format!("cannot stage workspace: {e}"))?;
        if let Err(error) = file
            .write_all(json.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = fs::remove_file(&temp);
            return Err(format!("cannot write workspace: {error}"));
        }
        if let Err(error) = fs::rename(&temp, &self.0.workspace_path) {
            let _ = fs::remove_file(&temp);
            return Err(format!("cannot publish workspace: {error}"));
        }
        Ok(())
    }

    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut StudyWorkspace) -> Result<T, String>,
    ) -> Result<T, String> {
        let _gate = self
            .0
            .mutation_gate
            .lock()
            .map_err(|_| "mutation lock poisoned")?;
        if self
            .0
            .active_job
            .lock()
            .map_err(|_| "job lock poisoned")?
            .is_some()
        {
            return Err("workspace edits are paused while an engine job is running".into());
        }
        let mut workspace = self
            .0
            .workspace
            .lock()
            .map_err(|_| "workspace lock poisoned")?;
        let mut candidate = workspace.clone();
        let result = f(&mut candidate)?;
        self.persist(&candidate)?;
        *workspace = candidate;
        Ok(result)
    }

    fn start_job_locked(&self, kind: &str) -> Result<Arc<JobControl>, String> {
        let mut active = self.0.active_job.lock().map_err(|_| "job lock poisoned")?;
        if active.is_some() {
            return Err("another engine job is already running".into());
        }
        let mut jobs = self.0.jobs.lock().map_err(|_| "job lock poisoned")?;
        if jobs.len() >= MAX_JOBS {
            return Err(format!(
                "process has reached its {MAX_JOBS} job history limit"
            ));
        }
        let sequence = self.0.next_job.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("job-{sequence:06}");
        let job = Arc::new(JobControl::new(job_id.clone(), kind));
        jobs.insert(job_id.clone(), job.clone());
        *active = Some(job_id);
        let monitored = job.clone();
        let monitor = thread::spawn(move || {
            let started = std::time::Instant::now();
            while started.elapsed() < MAX_JOB_WALL_TIME {
                if !matches!(monitored.snapshot().phase, JobPhase::Running) {
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
            if matches!(monitored.snapshot().phase, JobPhase::Running) {
                monitored.wall_expired.store(true, Ordering::Relaxed);
                monitored.cancel.store(true, Ordering::Relaxed);
            }
        });
        *job.monitor
            .lock()
            .map_err(|_| "job monitor lock poisoned")? = Some(monitor);
        Ok(job)
    }

    fn finish_job(
        &self,
        job: &JobControl,
        phase: JobPhase,
        result: Option<(String, String)>,
        error: Option<String>,
    ) {
        let mut view = job.view.lock().unwrap_or_else(|e| e.into_inner());
        view.phase = phase;
        view.progress_fraction = if matches!(view.phase, JobPhase::Completed) {
            1.0
        } else {
            view.progress_fraction
        };
        if let Some((id, uri)) = result {
            view.result_id = Some(id);
            view.result_resource = Some(uri);
        }
        view.error = error;
        drop(view);
        let job_id = job.snapshot().job_id;
        if let Ok(mut active) = self.0.active_job.lock()
            && active.as_deref() == Some(&job_id)
        {
            *active = None;
        }
    }

    pub(crate) fn shutdown(&self) {
        let jobs: Vec<Arc<JobControl>> = self
            .0
            .jobs
            .lock()
            .map(|jobs| jobs.values().cloned().collect())
            .unwrap_or_default();
        for job in &jobs {
            job.cancel.store(true, Ordering::Relaxed);
        }
        for job in jobs {
            if let Ok(mut handle) = job.join.lock()
                && let Some(handle) = handle.take()
            {
                let _ = handle.join();
            }
            if let Ok(mut handle) = job.monitor.lock()
                && let Some(handle) = handle.take()
            {
                let _ = handle.join();
            }
        }
    }

    fn start_search(&self, variant_id: String) -> Result<JobView, String> {
        let (mut input, input_key, job) = {
            let _gate = self
                .0
                .mutation_gate
                .lock()
                .map_err(|_| "mutation lock poisoned")?;
            let workspace = self
                .0
                .workspace
                .lock()
                .map_err(|_| "workspace lock poisoned")?;
            let input = workspace.clone();
            check_workload(&input, &variant_id, None)?;
            let variant = input
                .variant(&variant_id)
                .map_err(|e| e.to_string())?
                .clone();
            let input_key = exact_input_key(&variant).map_err(|e| e.to_string())?;
            let job = self.start_job_locked("search")?;
            (input, input_key, job)
        };
        let state = self.clone();
        let worker_job = job.clone();
        let join = thread::spawn(move || {
            let _terminal_guard = WorkerTerminalGuard {
                state: state.clone(),
                job: worker_job.clone(),
            };
            let result = state
                .0
                .engine
                .lock()
                .map_err(|_| "engine lock poisoned".to_owned())
                .and_then(|mut engine| {
                    let cached = input
                        .variant(&variant_id)
                        .map_err(|e| e.to_string())
                        .and_then(|variant| {
                            engine
                                .cached_result(variant)
                                .map(|value| value.is_some())
                                .map_err(|e| e.to_string())
                        })?;
                    if cached {
                        worker_job
                            .view
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .reused_calculation = true;
                    }
                    engine
                        .run_variant_with(
                            &mut input,
                            &variant_id,
                            &worker_job.cancel,
                            Some(&worker_job.progress),
                        )
                        .map_err(|e| e.to_string())
                })
                .and_then(|result_id| {
                    let record = input
                        .variant(&variant_id)
                        .map_err(|e| e.to_string())?
                        .results
                        .iter()
                        .find(|r| r.id == result_id)
                        .ok_or_else(|| {
                            "study runner did not retain its completed record".to_owned()
                        })?;
                    Ok((result_id, record.record_json.clone()))
                });
            match result {
                Ok((result_id, record_json)) => {
                    let committed = (|| {
                        let _gate = state
                            .0
                            .mutation_gate
                            .lock()
                            .map_err(|_| "mutation lock poisoned".to_owned())?;
                        let mut workspace = state
                            .0
                            .workspace
                            .lock()
                            .map_err(|_| "workspace lock poisoned".to_owned())?;
                        let current = workspace.variant(&variant_id).map_err(|e| e.to_string())?;
                        if exact_input_key(current).map_err(|e| e.to_string())? != input_key {
                            return Err(
                                "variant changed while job ran; result was not attached".into()
                            );
                        }
                        let mut candidate = workspace.clone();
                        if !candidate
                            .variant(&variant_id)
                            .map_err(|e| e.to_string())?
                            .results
                            .iter()
                            .any(|r| r.id == result_id)
                        {
                            candidate
                                .attach_result(&variant_id, record_json)
                                .map_err(|e| e.to_string())?;
                        }
                        state.persist(&candidate)?;
                        *workspace = candidate;
                        Ok::<(), String>(())
                    })();
                    match committed {
                        Ok(()) => state.finish_job(
                            &worker_job,
                            JobPhase::Completed,
                            Some((
                                result_id.clone(),
                                format!("optcoil://study/variant/{variant_id}/result/{result_id}"),
                            )),
                            None,
                        ),
                        Err(error) => {
                            state.finish_job(&worker_job, JobPhase::Failed, None, Some(error))
                        }
                    }
                }
                Err(error) => {
                    let phase = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        JobPhase::Failed
                    } else if worker_job.cancel.load(Ordering::Relaxed) {
                        JobPhase::Cancelled
                    } else {
                        JobPhase::Failed
                    };
                    let error = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        format!("15-minute server wall-time budget expired: {error}")
                    } else {
                        error
                    };
                    state.finish_job(&worker_job, phase, None, Some(error));
                }
            }
        });
        *job.join.lock().map_err(|_| "job lock poisoned")? = Some(join);
        Ok(job.snapshot())
    }

    fn start_sensitivity(&self, variant_id: String, spec_json: String) -> Result<JobView, String> {
        if spec_json.len() > MAX_SENSITIVITY_SPEC_BYTES {
            return Err("sensitivity specification exceeds the 256 KiB limit".into());
        }
        let (variant, job) = {
            let _gate = self
                .0
                .mutation_gate
                .lock()
                .map_err(|_| "mutation lock poisoned")?;
            let workspace = self
                .0
                .workspace
                .lock()
                .map_err(|_| "workspace lock poisoned")?;
            let workspace = workspace.clone();
            check_workload(&workspace, &variant_id, Some(&spec_json))?;
            let variant = workspace
                .variant(&variant_id)
                .map_err(|e| e.to_string())?
                .clone();
            let job = self.start_job_locked("sensitivity")?;
            (variant, job)
        };
        let state = self.clone();
        let worker_job = job.clone();
        let join = thread::spawn(move || {
            let _terminal_guard = WorkerTerminalGuard {
                state: state.clone(),
                job: worker_job.clone(),
            };
            let result = (|| {
                let mut datasets = BTreeMap::new();
                for raw in &variant.dataset_bundles {
                    let bundle = optcoil_model::material::MaterialBundle::from_json(raw)
                        .map_err(|e| format!("dataset bundle: {e}"))?;
                    if datasets
                        .insert(bundle.dataset.metadata.id.clone(), bundle.dataset)
                        .is_some()
                    {
                        return Err("duplicate dataset bundle identity".to_owned());
                    }
                }
                let record = run_sensitivity_sweep_with_datasets(
                    &variant.case_json,
                    &spec_json,
                    &variant.options,
                    &datasets,
                    &worker_job.cancel,
                )
                .map_err(|e| e.to_string())?;
                serde_json::to_string_pretty(&record).map_err(|e| e.to_string())
            })();
            match result {
                Ok(contents) => {
                    let id = match reserve_export_dir(&state, "sensitivity") {
                        Ok(id) => id,
                        Err(error) => {
                            state.finish_job(&worker_job, JobPhase::Failed, None, Some(error));
                            return;
                        }
                    };
                    let uri = format!("optcoil://artifact/{id}/record.json");
                    let persisted =
                        write_output(&state.0.root, &id, "record.json", contents.as_bytes());
                    match persisted {
                        Ok(()) => {
                            if let Ok(mut resources) = state.0.export_resources.lock() {
                                resources.insert(uri.clone(), contents);
                            }
                            state.finish_job(
                                &worker_job,
                                JobPhase::Completed,
                                Some((id, uri)),
                                None,
                            );
                        }
                        Err(error) => {
                            let _ = fs::remove_dir_all(state.0.root.join("exports").join(&id));
                            state.finish_job(&worker_job, JobPhase::Failed, None, Some(error))
                        }
                    }
                }
                Err(error) => {
                    let phase = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        JobPhase::Failed
                    } else if worker_job.cancel.load(Ordering::Relaxed) {
                        JobPhase::Cancelled
                    } else {
                        JobPhase::Failed
                    };
                    let error = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        format!("15-minute server wall-time budget expired: {error}")
                    } else {
                        error
                    };
                    state.finish_job(&worker_job, phase, None, Some(error));
                }
            }
        });
        *job.join.lock().map_err(|_| "job lock poisoned")? = Some(join);
        Ok(job.snapshot())
    }

    fn start_robustness(
        &self,
        variant_ids: Vec<String>,
        spec_json: String,
    ) -> Result<JobView, String> {
        if spec_json.len() > MAX_SENSITIVITY_SPEC_BYTES {
            return Err("scenario specification exceeds the 256 KiB limit".into());
        }
        let spec: RobustnessSpec = serde_json::from_str(&spec_json).map_err(|e| e.to_string())?;
        let (input, input_keys, job) = {
            let _gate = self
                .0
                .mutation_gate
                .lock()
                .map_err(|_| "mutation lock poisoned")?;
            let workspace = self
                .0
                .workspace
                .lock()
                .map_err(|_| "workspace lock poisoned")?;
            let preflight = check_robustness_workload(&workspace, &variant_ids, &spec)?;
            if !preflight.ready_to_run {
                return Err(
                    "scenario inputs are not ready; preview and resolve the reported errors".into(),
                );
            }
            let keys = variant_ids
                .iter()
                .map(|id| {
                    let variant = workspace.variant(id).map_err(|e| e.to_string())?;
                    Ok((
                        id.clone(),
                        exact_input_key(variant).map_err(|e| e.to_string())?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            (
                workspace.clone(),
                keys,
                self.start_job_locked("robustness")?,
            )
        };
        let state = self.clone();
        let worker_job = job.clone();
        let join = thread::spawn(move || {
            let _terminal_guard = WorkerTerminalGuard {
                state: state.clone(),
                job: worker_job.clone(),
            };
            let result =
                run_study_robustness(&input, &variant_ids, &spec, &worker_job.cancel, None)
                    .map_err(|e| e.to_string())
                    .and_then(|record| {
                        let artifact_id = robustness_artifact_id(&record)?;
                        let _gate = state
                            .0
                            .mutation_gate
                            .lock()
                            .map_err(|_| "mutation lock poisoned")?;
                        let mut workspace = state
                            .0
                            .workspace
                            .lock()
                            .map_err(|_| "workspace lock poisoned")?;
                        if worker_job.cancel.load(Ordering::Relaxed) {
                            return Err("scenario study cancelled before attachment".into());
                        }
                        for (id, key) in &input_keys {
                            if exact_input_key(workspace.variant(id).map_err(|e| e.to_string())?)
                                .map_err(|e| e.to_string())?
                                != *key
                            {
                                return Err("scenario inputs changed before attachment".into());
                            }
                        }
                        let mut candidate = workspace.clone();
                        candidate
                            .attach_robustness_result(record.clone())
                            .map_err(|e| e.to_string())?;
                        state.persist(&candidate)?;
                        *workspace = candidate;
                        // The workspace is the durable source of evidence. Its resource
                        // stays available across server restarts without an export cache.
                        Ok(artifact_id)
                    });
            match result {
                Ok(id) => state.finish_job(
                    &worker_job,
                    JobPhase::Completed,
                    Some((id.clone(), format!("optcoil://study/robustness/{id}"))),
                    None,
                ),
                Err(error) => {
                    let phase = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        JobPhase::Failed
                    } else if worker_job.cancel.load(Ordering::Relaxed) {
                        JobPhase::Cancelled
                    } else {
                        JobPhase::Failed
                    };
                    let error = if worker_job.wall_expired.load(Ordering::Relaxed) {
                        format!("15-minute server wall-time budget expired: {error}")
                    } else {
                        error
                    };
                    state.finish_job(&worker_job, phase, None, Some(error));
                }
            }
        });
        *job.join.lock().map_err(|_| "job lock poisoned")? = Some(join);
        Ok(job.snapshot())
    }

    fn job_view(&self, job_id: &str) -> Result<JobView, String> {
        self.0
            .jobs
            .lock()
            .map_err(|_| "job lock poisoned".to_owned())?
            .get(job_id)
            .map(|job| job.snapshot())
            .ok_or_else(|| format!("unknown job '{job_id}'"))
    }

    fn resource_text(&self, uri: &str) -> Result<String, String> {
        if let Some(text) = self
            .0
            .export_resources
            .lock()
            .map_err(|_| "resource lock poisoned".to_owned())?
            .get(uri)
        {
            return Ok(text.clone());
        }
        if uri == "optcoil://study/workspace" {
            return self.workspace_json();
        }
        if let Some(id) = uri.strip_prefix("optcoil://study/robustness/") {
            let workspace = self
                .0
                .workspace
                .lock()
                .map_err(|_| "workspace lock poisoned")?;
            for record in workspace.robustness_results.iter().rev() {
                if robustness_artifact_id(record)? == id {
                    return serde_json::to_string_pretty(record).map_err(|e| e.to_string());
                }
            }
            return Err("scenario evidence is no longer retained in this workspace".into());
        }
        if uri == "optcoil://examples/first-study" {
            return Ok(include_str!("../../../benchmarks/coupled/first-study.json").to_owned());
        }
        if let Some(path) = uri.strip_prefix("optcoil://artifact/") {
            return self.read_artifact_resource(path);
        }
        let parts: Vec<_> = uri.split('/').collect();
        if parts.len() == 5
            && parts[0] == "optcoil:"
            && parts[2] == "study"
            && parts[3] == "variant"
        {
            return Err("invalid study resource URI".into());
        }
        if let Some(path) = uri.strip_prefix("optcoil://study/variant/") {
            let mut parts = path.split('/');
            let variant_id = parts.next().unwrap_or_default();
            let kind = parts.next().unwrap_or_default();
            let remainder = parts.next();
            if parts.next().is_some() {
                return Err("invalid study resource URI".into());
            }
            let workspace = self
                .0
                .workspace
                .lock()
                .map_err(|_| "workspace lock poisoned")?;
            let variant = workspace.variant(variant_id).map_err(|e| e.to_string())?;
            match (kind, remainder) {
                ("case", None) => Ok(variant.case_json.clone()),
                ("result", Some(result_id)) => variant
                    .results
                    .iter()
                    .find(|r| r.id == result_id)
                    .map(|r| r.record_json.clone())
                    .ok_or_else(|| format!("unknown result '{result_id}'")),
                _ => Err("invalid study resource URI".into()),
            }
        } else {
            Err("resource not found".into())
        }
    }

    fn read_artifact_resource(&self, uri_path: &str) -> Result<String, String> {
        let (export_id, relative) = uri_path
            .split_once('/')
            .ok_or_else(|| "invalid artifact resource URI".to_owned())?;
        if export_id.is_empty()
            || !export_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("invalid artifact resource identifier".into());
        }
        let relative_path = Path::new(relative);
        let components: Vec<_> = relative_path.components().collect();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("invalid artifact resource path".into());
        }

        let root = fs::canonicalize(&self.0.root).map_err(|e| e.to_string())?;
        let exports = self.0.root.join("exports");
        let canonical_exports = fs::canonicalize(&exports).map_err(|e| e.to_string())?;
        if !canonical_exports.starts_with(&root) {
            return Err("exports directory escapes workspace".into());
        }
        let directory_path = exports.join(export_id);
        let directory_metadata = fs::symlink_metadata(&directory_path)
            .map_err(|_| "artifact export was not found".to_owned())?;
        if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
            return Err("artifact export directory must be a regular directory".into());
        }
        let directory = fs::canonicalize(&directory_path).map_err(|e| e.to_string())?;
        if !directory.starts_with(&canonical_exports) {
            return Err("artifact export directory escapes exports".into());
        }

        let mut path = directory_path;
        for (index, component) in components.iter().enumerate() {
            let Component::Normal(name) = component else {
                return Err("invalid artifact resource path".into());
            };
            path.push(name);
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| "artifact resource was not found".to_owned())?;
            if metadata.file_type().is_symlink() {
                return Err("artifact resource paths cannot contain symlinks".into());
            }
            if index + 1 < components.len() && !metadata.is_dir() {
                return Err("artifact resource parent is not a directory".into());
            }
            if index + 1 == components.len() {
                if !metadata.is_file() {
                    return Err("artifact resource must be a regular file".into());
                }
                if metadata.len() > MAX_ARTIFACT_RESOURCE_BYTES {
                    return Err("artifact resource exceeds the 64 MiB limit".into());
                }
            }
        }
        let canonical_path = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if !canonical_path.starts_with(&directory) {
            return Err("artifact resource escapes its export directory".into());
        }
        fs::read_to_string(canonical_path)
            .map_err(|e| format!("cannot read artifact resource: {e}"))
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn reserve_export_dir(state: &ServerState, prefix: &str) -> Result<String, String> {
    let exports = state.0.root.join("exports");
    let canonical_root = fs::canonicalize(&state.0.root).map_err(|e| e.to_string())?;
    let canonical_exports = fs::canonicalize(&exports).map_err(|e| e.to_string())?;
    if !canonical_exports.starts_with(&canonical_root) {
        return Err("exports directory escapes workspace".into());
    }
    for _ in 0..1000 {
        let sequence = state.0.next_export.fetch_add(1, Ordering::Relaxed);
        let id = format!("{prefix}-{sequence:06}");
        match fs::create_dir(exports.join(&id)) {
            Ok(()) => return Ok(id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot reserve export directory: {error}")),
        }
    }
    Err("could not allocate an unused export identifier".into())
}

fn robustness_artifact_id(
    record: &optcoil_search::robustness::RobustnessStudyRecord,
) -> Result<String, String> {
    // Bind a resource to this exact completed artifact, including its
    // calculation timestamps; repeated inputs can have distinct records.
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    Ok(optcoil_model::attestation::sha256_hex(&bytes))
}

fn check_robustness_workload(
    workspace: &StudyWorkspace,
    ids: &[String],
    spec: &RobustnessSpec,
) -> Result<optcoil_search::robustness::RobustnessPreflight, String> {
    let preflight = robustness_preflight(workspace, ids, spec).map_err(|e| e.to_string())?;
    if preflight
        .preflights
        .iter()
        .any(|run| run.candidate_count > MAX_CANDIDATES)
    {
        return Err(format!(
            "scenario candidate count exceeds server limit of {MAX_CANDIDATES}"
        ));
    }
    if preflight.kernel_work_proxy > MAX_REFINED_KERNEL_PROXY {
        return Err(format!(
            "scenario aggregate kernel work proxy exceeds server limit of {MAX_REFINED_KERNEL_PROXY}"
        ));
    }
    Ok(preflight)
}

fn check_workload(
    workspace: &StudyWorkspace,
    variant_id: &str,
    sensitivity_json: Option<&str>,
) -> Result<(), String> {
    let preflight = workspace
        .preflight_variant(variant_id)
        .map_err(|e| e.to_string())?;
    if !preflight.ready_to_run {
        let details = preflight
            .errors
            .iter()
            .map(|item| item.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("preflight blocks execution: {details}"));
    }
    let workload = &preflight.workload;
    if workload.candidate_count == 0 || workload.candidate_count > MAX_CANDIDATES {
        return Err(format!(
            "candidate count {} exceeds the server limit of {MAX_CANDIDATES}",
            workload.candidate_count
        ));
    }
    if workload.refined_kernel_work_proxy > MAX_REFINED_KERNEL_PROXY {
        return Err(format!(
            "refined kernel work proxy {} exceeds the server limit of {MAX_REFINED_KERNEL_PROXY}",
            workload.refined_kernel_work_proxy
        ));
    }
    if let Some(json) = sensitivity_json {
        let spec = optcoil_model::sensitivity::SensitivitySpec::from_json(json)
            .map_err(|e| e.to_string())?;
        let points = spec.combination_count();
        if points == 0 || points > MAX_SENSITIVITY_POINTS {
            return Err(format!(
                "sensitivity point count {points} exceeds the server limit of {MAX_SENSITIVITY_POINTS}"
            ));
        }
        let total_proxy = workload
            .refined_kernel_work_proxy
            .saturating_mul(points as u128);
        if total_proxy > MAX_REFINED_KERNEL_PROXY {
            return Err(format!(
                "sensitivity total work proxy {total_proxy} exceeds the server limit of {MAX_REFINED_KERNEL_PROXY}"
            ));
        }
    }
    Ok(())
}

fn write_output(root: &Path, folder: &str, relative: &str, bytes: &[u8]) -> Result<(), String> {
    if folder.is_empty()
        || !folder
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("unsafe generated artifact path".into());
    }
    let exports = root.join("exports");
    let canonical_root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let canonical_exports = fs::canonicalize(&exports).map_err(|e| e.to_string())?;
    if !canonical_exports.starts_with(&canonical_root) {
        return Err("exports directory escapes workspace".into());
    }
    let directory = exports.join(folder);
    fs::create_dir_all(&directory).map_err(|e| format!("cannot create export directory: {e}"))?;
    if !fs::canonicalize(&directory)
        .map_err(|e| e.to_string())?
        .starts_with(&canonical_exports)
    {
        return Err("export directory escapes workspace".into());
    }
    let path = directory.join(relative);
    let parent = path.parent().ok_or("invalid output path")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if !fs::canonicalize(parent)
        .map_err(|e| e.to_string())?
        .starts_with(fs::canonicalize(&directory).map_err(|e| e.to_string())?)
    {
        return Err("artifact path escapes its export directory".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("cannot create artifact: {e}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("cannot write artifact: {e}"))
}

fn ok<T: Serialize>(value: T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => fail(format!("cannot encode result: {error}")),
    }
}

fn fail(error: impl ToString) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(error.to_string())])
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NameArgs {
    name: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct ImportArgs {
    workspace_json: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct CreateVariantArgs {
    name: String,
    case_json: String,
    #[serde(default)]
    dataset_bundles: Vec<String>,
    #[serde(default)]
    threads: Option<u32>,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct VariantArgs {
    variant_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct ReviseArgs {
    variant_id: String,
    case_json: String,
    #[serde(default)]
    dataset_bundles: Vec<String>,
    #[serde(default)]
    threads: Option<u32>,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct RenameArgs {
    variant_id: String,
    name: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct AttachBundleArgs {
    variant_id: String,
    bundle_json: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct CompareArgs {
    left_variant_id: String,
    right_variant_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct FollowUpArgs {
    variant_id: String,
    axis: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct StartSensitivityArgs {
    variant_id: String,
    spec_json: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct RobustnessArgs {
    variant_ids: Vec<String>,
    spec_json: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct JobArgs {
    job_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct ResultArgs {
    variant_id: String,
    result_id: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
struct ExportArgs {
    variant_id: String,
    result_id: String,
}

#[derive(Clone)]
pub(crate) struct McpServer {
    state: ServerState,
}

impl McpServer {
    pub(crate) fn new(state: ServerState) -> Self {
        Self { state }
    }
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "Report supported local Converra engine operations, model/checker identifiers, status meanings, and important engineering limits."
    )]
    fn engine_capabilities(&self) -> CallToolResult {
        ok(json!({
            "engine": "optcoil-search",
            "model_id": optcoil_search::coupled_search::COUPLED_SEARCH_MODEL_ID,
            "checker_id": optcoil_search::coupled_search::COUPLED_SEARCH_CHECKER_ID,
            "statuses": ["PASS", "FAIL", "INCONCLUSIVE", "NOT_EVALUATED"],
            "engineering_acceptance": "not established by search or screening PASS",
            "multi_dataset_bindings": true,
            "workspace_schema": optcoil_search::study::STUDY_WORKSPACE_SCHEMA,
            "limits": {
                "workspace_bytes": 67108864,
                "case_json_bytes": 4194304,
                "dataset_bundle_json_bytes_each": 8388608,
                "candidate_count": MAX_CANDIDATES,
                "refined_kernel_work_proxy": MAX_REFINED_KERNEL_PROXY.to_string(),
                "sensitivity_points": MAX_SENSITIVITY_POINTS,
                "robustness_variants": optcoil_search::robustness::MAX_ROBUSTNESS_VARIANTS,
                "robustness_scenarios": optcoil_search::robustness::MAX_ROBUSTNESS_SCENARIOS,
                "robustness_runs": optcoil_search::robustness::MAX_ROBUSTNESS_RUNS,
                "robustness_total_kernel_work_proxy": MAX_REFINED_KERNEL_PROXY.min(optcoil_search::robustness::MAX_ROBUSTNESS_KERNEL_WORK).to_string(),
                "robustness_total_candidates": optcoil_search::robustness::MAX_ROBUSTNESS_CANDIDATES.to_string(),
                "sensitivity_total_kernel_work_proxy": MAX_REFINED_KERNEL_PROXY.to_string(),
                "job_wall_time_seconds": MAX_JOB_WALL_TIME.as_secs(),
                "concurrent_engine_jobs": 1,
                "job_history": MAX_JOBS
            },
            "input_policy": "case and material sources are JSON strings; no tool accepts a filesystem path or shell command"
        }))
    }

    #[tool(
        description = "List embedded material datasets and their declared metadata. Customer datasets are supplied as exact bundle JSON through attach_dataset_bundle."
    )]
    fn list_datasets(&self) -> CallToolResult {
        let datasets = MaterialDataset::EMBEDDED_IDS
            .iter()
            .filter_map(|id| {
                MaterialDataset::embedded_by_id(id)
                    .ok()
                    .and_then(|dataset| serde_json::to_value(dataset.metadata).ok())
            })
            .collect::<Vec<_>>();
        ok(
            json!({ "embedded_datasets": datasets, "external_datasets": "attach raw bundle JSON to a variant" }),
        )
    }

    #[tool(
        description = "Initialize a named empty engineering study. Refuses to replace variants already in the workspace."
    )]
    fn create_study(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        if args.name.trim().is_empty() || args.name.len() > 200 {
            return fail("study name must contain 1 to 200 bytes");
        }
        match self.state.mutate(|workspace| {
            if !workspace.variants.is_empty() {
                return Err(
                    "workspace already contains variants; use import_study to replace it".into(),
                );
            }
            *workspace = StudyWorkspace::new(args.name);
            Ok(json!({ "resource": "optcoil://study/workspace" }))
        }) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Import a validated versioned StudyWorkspace JSON string, replacing the current workspace. The exact case, bundle, and result source strings stay embedded in the workspace."
    )]
    fn import_study(&self, Parameters(args): Parameters<ImportArgs>) -> CallToolResult {
        match self.state.mutate(|workspace| {
            let imported =
                StudyWorkspace::from_json(&args.workspace_json).map_err(|e| e.to_string())?;
            *workspace = imported;
            Ok(json!({ "resource": "optcoil://study/workspace" }))
        }) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Create a named variant from exact case JSON and zero or more exact material bundle JSON strings. Bundle ids and CSV hashes must match the case. Optional threads may only reduce the case's limit."
    )]
    fn create_variant(&self, Parameters(args): Parameters<CreateVariantArgs>) -> CallToolResult {
        if args.name.trim().is_empty() || args.name.len() > 200 {
            return fail("variant name must contain 1 to 200 bytes");
        }
        if args.case_json.len() > 4 * 1024 * 1024
            || args
                .dataset_bundles
                .iter()
                .any(|b| b.len() > 8 * 1024 * 1024)
        {
            return fail("case JSON is limited to 4 MiB and each dataset bundle to 8 MiB");
        }
        match self.state.mutate(|workspace| {
            let options = CoupledSearchOptions { threads: args.threads };
            let id = workspace.add_variant(args.name, args.case_json, args.dataset_bundles, options).map_err(|e| e.to_string())?;
            Ok(json!({ "variant_id": id, "case_resource": format!("optcoil://study/variant/{id}/case") }))
        }) { Ok(v) => ok(v), Err(e) => fail(e) }
    }

    #[tool(
        description = "Revise a variant with new exact case/bundle JSON and optional thread limit. Existing results remain as historical evidence and become non-current when exact inputs change."
    )]
    fn revise_variant(&self, Parameters(args): Parameters<ReviseArgs>) -> CallToolResult {
        if args.case_json.len() > 4 * 1024 * 1024
            || args
                .dataset_bundles
                .iter()
                .any(|b| b.len() > 8 * 1024 * 1024)
        {
            return fail("case JSON is limited to 4 MiB and each dataset bundle to 8 MiB");
        }
        match self.state.mutate(|workspace| {
            let options = CoupledSearchOptions { threads: args.threads };
            workspace.revise_variant(&args.variant_id, args.case_json, args.dataset_bundles, options).map_err(|e| e.to_string())?;
            Ok(json!({ "variant_id": args.variant_id, "case_resource": format!("optcoil://study/variant/{}/case", args.variant_id) }))
        }) { Ok(v) => ok(v), Err(e) => fail(e) }
    }

    #[tool(
        description = "Duplicate a variant's exact inputs into a new named variant. Results are not copied as authority for the duplicate."
    )]
    fn duplicate_variant(&self, Parameters(args): Parameters<RenameArgs>) -> CallToolResult {
        if args.name.trim().is_empty() || args.name.len() > 200 {
            return fail("variant name must contain 1 to 200 bytes");
        }
        match self.state.mutate(|workspace| {
            let id = workspace
                .duplicate_variant(&args.variant_id, args.name)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "variant_id": id }))
        }) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "Rename a named study variant without changing its inputs or evidence.")]
    fn rename_variant(&self, Parameters(args): Parameters<RenameArgs>) -> CallToolResult {
        if args.name.trim().is_empty() || args.name.len() > 200 {
            return fail("variant name must contain 1 to 200 bytes");
        }
        match self.state.mutate(|workspace| {
            workspace
                .rename_variant(&args.variant_id, args.name)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "variant_id": args.variant_id }))
        }) {
            Ok(value) => ok(value),
            Err(error) => fail(error),
        }
    }

    #[tool(
        description = "Attach an exact raw material bundle JSON string to the variant's retained dependencies. The parsed identity must be declared by the case and its id/hash must match."
    )]
    fn attach_dataset_bundle(
        &self,
        Parameters(args): Parameters<AttachBundleArgs>,
    ) -> CallToolResult {
        if args.bundle_json.len() > 8 * 1024 * 1024 {
            return fail("dataset bundle exceeds the 8 MiB limit");
        }
        match self.state.mutate(|workspace| {
            let variant = workspace
                .variant(&args.variant_id)
                .map_err(|e| e.to_string())?
                .clone();
            let mut bundles = variant.dataset_bundles;
            bundles.push(args.bundle_json);
            workspace
                .revise_variant(
                    &args.variant_id,
                    variant.case_json,
                    bundles,
                    variant.options,
                )
                .map_err(|e| e.to_string())?;
            workspace
                .preflight_variant(&args.variant_id)
                .map_err(|e| e.to_string())
        }) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "List named variants, exact case hashes, dependency identities and retained result IDs/currentness."
    )]
    fn list_variants(&self) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => {
                let variants = match workspace
                    .variants
                    .iter()
                    .map(|v| workspace.compact_summary(&v.id))
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(variants) => variants,
                    Err(error) => return fail(error),
                };
                ok(
                    json!({ "schema": &workspace.schema, "name": &workspace.name, "selected_variant_id": &workspace.selected_variant_id, "variants": variants }),
                )
            }
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(description = "Select the active study variant by its opaque workspace id.")]
    fn select_variant(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.mutate(|workspace| {
            workspace
                .select_variant(&args.variant_id)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "selected_variant_id": args.variant_id }))
        }) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Run declared applicability, dependency, sampling and work-estimate preflight. A ready result is not physics coverage or engineering acceptance."
    )]
    fn preflight_variant(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => match workspace.preflight_variant(&args.variant_id) {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Summarize unresolved input/result gates and concrete follow-up axes for a study variant; this is diagnosis, not a new evaluation."
    )]
    fn diagnose_variant(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => match workspace.diagnose_variant(&args.variant_id) {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Return a compact variant summary with exact source hashes, result currentness, recommendation statuses, source cost components and unresolved decision gates. No engineering acceptance is implied."
    )]
    fn summarize_variant(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => match workspace.compact_summary(&args.variant_id) {
                Ok(summary) => ok(summary),
                Err(error) => fail(error),
            },
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Start a full coupled search as a cancellable background job. Poll get_job; completed source records are attached only after exact workspace input checks pass."
    )]
    fn start_search(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.start_search(args.variant_id) {
            Ok(job) => ok(job),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Start a sensitivity sweep from exact variant inputs and an optcoil-sensitivity/v1-v3 JSON specification. The result is an artifact, not a new variant's accepted design."
    )]
    fn start_sensitivity(
        &self,
        Parameters(args): Parameters<StartSensitivityArgs>,
    ) -> CallToolResult {
        match self
            .state
            .start_sensitivity(args.variant_id, args.spec_json)
        {
            Ok(job) => ok(job),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Preview exact scenario inputs, comparability, aggregate work and readiness for named alternatives. Scenarios have no implied probabilities; nominal is required."
    )]
    fn preview_robustness(&self, Parameters(args): Parameters<RobustnessArgs>) -> CallToolResult {
        if args.spec_json.len() > MAX_SENSITIVITY_SPEC_BYTES {
            return fail("scenario specification exceeds the 256 KiB limit");
        }
        let spec: RobustnessSpec = match serde_json::from_str(&args.spec_json) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        match self.state.0.workspace.lock() {
            Ok(workspace) => {
                match check_robustness_workload(&workspace, &args.variant_ids, &spec) {
                    Ok(preflight) => ok(preflight),
                    Err(e) => fail(e),
                }
            }
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Start bounded cancellable fresh reruns across explicit price, Ic and operating-temperature scenarios at unchanged numerical fidelity. Full source evidence is retained in the workspace; no robust engineering acceptance is claimed."
    )]
    fn start_robustness(&self, Parameters(args): Parameters<RobustnessArgs>) -> CallToolResult {
        match self
            .state
            .start_robustness(args.variant_ids, args.spec_json)
        {
            Ok(job) => ok(job),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "List retained scenario analyses with exact input fingerprints, winners, switches and current/history binding. Read each resource for full offline rerun evidence."
    )]
    fn list_robustness_results(&self) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => {
                let summaries = workspace.robustness_results.iter().map(|record| {
                    let artifact_id = robustness_artifact_id(record)?;
                    let current = optcoil_search::robustness::robustness_record_binding_is_current(&workspace, record)
                        .map_err(|error| error.to_string())?;
                    Ok(json!({
                        "input_fingerprint": record.input_fingerprint,
                        "current": current,
                        "scenario_summaries": record.scenario_summaries,
                        "winner_switches": record.winner_switches,
                        "all_scenarios_have_supported_winner": record.all_scenarios_have_supported_winner,
                        "artifact_sha256": artifact_id,
                        "resource": format!("optcoil://study/robustness/{artifact_id}"),
                    }))
                }).collect::<Result<Vec<_>, String>>();
                match summaries {
                    Ok(summaries) => ok(summaries),
                    Err(error) => fail(error),
                }
            }
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Get background job phase, engine progress, result id/resource or a structured error."
    )]
    fn get_job(&self, Parameters(args): Parameters<JobArgs>) -> CallToolResult {
        match self.state.job_view(&args.job_id) {
            Ok(v) => ok(v),
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Fetch the completed result locator for a job. Read the returned opaque resource URI with resources/read for the full record or sensitivity artifact."
    )]
    fn get_job_result(&self, Parameters(args): Parameters<JobArgs>) -> CallToolResult {
        match self.state.job_view(&args.job_id) {
            Ok(view) if matches!(view.phase, JobPhase::Completed) => ok(
                json!({ "job_id": args.job_id, "result_id": view.result_id, "resource": view.result_resource }),
            ),
            Ok(view) => fail(format!(
                "job is {:?}; no completed result is available",
                view.phase
            )),
            Err(error) => fail(error),
        }
    }

    #[tool(
        description = "Request cancellation of a running engine job. The job reports Cancelled only after the worker stops; no partial record is attached."
    )]
    fn cancel_job(&self, Parameters(args): Parameters<JobArgs>) -> CallToolResult {
        let jobs = match self.state.0.jobs.lock() {
            Ok(jobs) => jobs,
            Err(_) => return fail("job lock poisoned"),
        };
        let Some(job) = jobs.get(&args.job_id) else {
            return fail(format!("unknown job '{}'", args.job_id));
        };
        if !matches!(job.snapshot().phase, JobPhase::Running) {
            return fail("job is already complete");
        }
        job.cancel.store(true, Ordering::Relaxed);
        ok(json!({ "job_id": args.job_id, "cancellation_requested": true }))
    }

    #[tool(
        description = "List retained result ids and exact source record resource URIs for one variant."
    )]
    fn list_results(&self, Parameters(args): Parameters<VariantArgs>) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => match workspace.variant(&args.variant_id) {
                Ok(variant) => ok(variant.results.iter().map(|r| json!({ "result_id": r.id, "case_sha256": r.case_sha256, "input_sha256": r.input_sha256, "exact_input_key": r.exact_input_key, "resource": format!("optcoil://study/variant/{}/result/{}", args.variant_id, r.id) })).collect::<Vec<_>>()),
                Err(e) => fail(e),
            },
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Compare exact case fields and dataset identities across two named variants."
    )]
    fn compare_variants(&self, Parameters(args): Parameters<CompareArgs>) -> CallToolResult {
        match self.state.0.workspace.lock() {
            Ok(workspace) => {
                match workspace.case_diff(&args.left_variant_id, &args.right_variant_id) {
                    Ok(v) => ok(v),
                    Err(e) => fail(e),
                }
            }
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Propose an explicit wider tape-count or strand-count follow-up case. The proposal is not calculated and does not imply acceptance."
    )]
    fn propose_follow_up(&self, Parameters(args): Parameters<FollowUpArgs>) -> CallToolResult {
        let axis = match args.axis.as_str() {
            "tapes_along_width" => FollowUpAxis::TapesAlongWidth,
            "strands_parallel" => FollowUpAxis::StrandsParallel,
            _ => return fail("axis must be tapes_along_width or strands_parallel"),
        };
        match self.state.0.workspace.lock() {
            Ok(workspace) => match workspace.propose_followup(&args.variant_id, axis) {
                Ok(v) => ok(v),
                Err(e) => fail(e),
            },
            Err(_) => fail("workspace lock poisoned"),
        }
    }

    #[tool(
        description = "Independently verify source record ledger and exact case/dataset bindings for one retained result. This does not establish engineering acceptance."
    )]
    fn verify_result(&self, Parameters(args): Parameters<ResultArgs>) -> CallToolResult {
        let workspace = match self.state.0.workspace.lock() {
            Ok(w) => w,
            Err(_) => return fail("workspace lock poisoned"),
        };
        let variant = match workspace.variant(&args.variant_id) {
            Ok(v) => v,
            Err(e) => return fail(e),
        };
        let Some(result) = variant.results.iter().find(|r| r.id == args.result_id) else {
            return fail("unknown result id");
        };
        let datasets = variant
            .dataset_bundles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        match verify::verify_record_checks(
            &result.record_json,
            Some(&variant.case_json),
            None,
            None,
            &datasets,
        ) {
            Ok(checks) => {
                let failed = checks.iter().any(|c| c.outcome == Outcome::Fail);
                let checks = checks.iter().map(|c| json!({
                    "name": c.name,
                    "outcome": match c.outcome { Outcome::Pass => "PASS", Outcome::Fail => "FAIL", Outcome::NotChecked => "NOT_CHECKED" },
                    "detail": c.detail,
                })).collect::<Vec<_>>();
                ok(
                    json!({ "status": if failed { "FAIL" } else { "VERIFIED_ARTIFACTS" }, "engineering_acceptance": "not established by artifact verification", "checks": checks }),
                )
            }
            Err(e) => fail(e),
        }
    }

    #[tool(
        description = "Export a verified review package for one retained result into the fixed exports directory. Returns generated resource URIs; clients read large artifacts through resources/read."
    )]
    fn export_review_package(&self, Parameters(args): Parameters<ExportArgs>) -> CallToolResult {
        let workspace = match self.state.0.workspace.lock() {
            Ok(w) => w,
            Err(_) => return fail("workspace lock poisoned"),
        };
        let package = match workspace.review_package_variant(&args.variant_id, &args.result_id) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        if let Err(e) = review::verify_review_package(&package.artifacts) {
            return fail(e);
        }
        let export_id = match reserve_export_dir(&self.state, "export") {
            Ok(id) => id,
            Err(error) => return fail(error),
        };
        let mut resources = Vec::new();
        let mut staged = Vec::new();
        let mut resource_contents = Vec::new();
        for (path, contents) in &package.artifacts {
            if let Err(error) =
                write_output(&self.state.0.root, &export_id, path, contents.as_bytes())
            {
                for file in staged {
                    let _ = fs::remove_file(file);
                }
                let _ = fs::remove_dir_all(self.state.0.root.join("exports").join(&export_id));
                return fail(error);
            }
            let uri = format!("optcoil://artifact/{export_id}/{path}");
            resources.push(uri.clone());
            staged.push(
                self.state
                    .0
                    .root
                    .join("exports")
                    .join(&export_id)
                    .join(path),
            );
            resource_contents.push((uri, contents.clone()));
        }
        if let Ok(mut map) = self.state.0.export_resources.lock() {
            map.extend(resource_contents);
        }
        ok(
            json!({ "export_id": export_id, "resources": resources, "manifest_resource": format!("optcoil://artifact/{export_id}/manifest.json") }),
        )
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("optcoil-mcp", env!("CARGO_PKG_VERSION")))
        .with_instructions("Local Converra engineering study tools over stdio. Inputs are JSON strings and generated opaque IDs. Search PASS and artifact verification do not establish engineering acceptance.")
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let workspace = self
            .state
            .0
            .workspace
            .lock()
            .map_err(|_| McpError::internal_error("workspace lock poisoned", None))?;
        let mut resources = vec![
            Resource::new("optcoil://study/workspace", "Study workspace")
                .with_description("Current named variants, exact source data and attached results")
                .with_mime_type("application/json"),
            Resource::new("optcoil://examples/first-study", "First study example")
                .with_description("Versioned case with attributed measured material data and explicitly invented placeholder prices; not a production design or supplier quote.")
                .with_mime_type("application/json"),
        ];
        for variant in &workspace.variants {
            resources.push(
                Resource::new(
                    format!("optcoil://study/variant/{}/case", variant.id),
                    format!("{} case", variant.name),
                )
                .with_mime_type("application/json"),
            );
            for result in &variant.results {
                resources.push(
                    Resource::new(
                        format!(
                            "optcoil://study/variant/{}/result/{}",
                            variant.id, result.id
                        ),
                        format!("{} result {}", variant.name, result.id),
                    )
                    .with_mime_type("application/json"),
                );
            }
        }
        for record in &workspace.robustness_results {
            resources.push(
                Resource::new(
                    format!(
                        "optcoil://study/robustness/{}",
                        robustness_artifact_id(record)
                            .map_err(|error| McpError::internal_error(error, None))?
                    ),
                    "Retained scenario evidence",
                )
                .with_mime_type("application/json"),
            );
        }
        drop(workspace);
        if let Ok(exports) = self.state.0.export_resources.lock() {
            for uri in exports.keys() {
                resources
                    .push(Resource::new(uri, "Export artifact").with_mime_type("application/json"));
            }
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match self.state.resource_text(&request.uri) {
            Ok(text) => Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, request.uri).with_mime_type("application/json"),
            ])
            .into()),
            Err(error) => Err(McpError::resource_not_found(error, None)),
        }
    }
}
