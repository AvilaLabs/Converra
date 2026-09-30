// SPDX-License-Identifier: MIT

//! Browser entry point. The app code is target-agnostic — this module
//! hosts it on a canvas and dispatches numerical work inside Web Workers.
//! Files arrive as picker/drag-drop bytes and leave as browser downloads.

#![cfg(target_arch = "wasm32")]

use sha2::Digest;
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Workbench;

const CANVAS_ID: &str = "converra_canvas";

// IndexedDB transactions acknowledge durable draft writes without blocking the
// canvas event loop. Only an unprotected change triggers the browser's normal
// close/reload warning; a successfully autosaved draft can be restored later.
#[wasm_bindgen(inline_js = r#"
let draftUnprotected = false;
let draftDatabase;
let draftBaseToken = null;
function openDraftDatabase() {
  if (!draftDatabase) draftDatabase = new Promise((resolve, reject) => {
    const request = indexedDB.open('converra-workbench', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('recovery');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => { draftDatabase = null; reject(new Error(request.error?.message || 'Draft storage unavailable')); };
    request.onblocked = () => { draftDatabase = null; reject(new Error('Close another Converra tab that is blocking draft storage')); };
  });
  return draftDatabase;
}
async function draftTransaction(mode, operation) {
  const database = await openDraftDatabase();
  return new Promise((resolve, reject) => {
    const transaction = database.transaction('recovery', mode);
    const request = operation(transaction.objectStore('recovery'));
    let value;
    request.onsuccess = () => { value = request.result; };
    request.onerror = () => reject(new Error(request.error?.message || 'Draft storage operation failed'));
    transaction.oncomplete = () => resolve(value);
    transaction.onerror = () => reject(new Error(transaction.error?.message || 'Draft transaction failed'));
    transaction.onabort = () => reject(new Error(transaction.error?.message || 'Draft transaction aborted'));
  });
}
export async function converraLoadDraft() {
  const stored = await draftTransaction('readonly', store => store.get('latest'));
  if (stored !== undefined && typeof stored !== 'string' && (!stored || typeof stored.text !== 'string' || typeof stored.token !== 'string')) throw new Error('Saved draft has an invalid storage type; it has been retained');
  draftBaseToken = typeof stored === 'string' ? 'legacy' : stored?.token ?? null;
  const value = typeof stored === 'string' ? stored : stored?.text;
  if (value !== undefined && typeof value !== 'string') throw new Error('Saved draft has an invalid storage type');
  if (value && new TextEncoder().encode(value).length > 134217728) throw new Error('Saved draft exceeds the recovery size limit');
  if (value) await validateDraftText(value);
  return value ?? null;
}
export async function converraStoreDraft(text) {
  await validateDraftText(text);
  await replaceDraft(text);
}
function validateDraftText(text) {
  return new Promise((resolve, reject) => {
    const worker = new Worker('./worker.js', {type:'module'});
    let settled = false;
    const finish = error => { if (settled) return; settled = true; clearTimeout(timeout); worker.terminate(); error ? reject(error) : resolve(); };
    const timeout = setTimeout(() => finish(new Error('Draft validation timed out; previous draft retained')), 60000);
    worker.onmessage = event => {
      try { const reply = JSON.parse(event.data); finish(reply.status === 'recovery_ok' ? null : new Error(reply.error || 'Draft validation failed')); }
      catch (error) { finish(error); }
    };
    worker.onerror = () => finish(new Error('Draft validation worker failed; previous draft retained'));
    worker.postMessage(JSON.stringify({kind:'recovery-validate', snapshot_json:text}));
  });
}
export async function converraDiscardDraft() {
  await replaceDraft(null);
}
async function replaceDraft(text) {
  const database = await openDraftDatabase();
  return new Promise((resolve, reject) => {
    const transaction = database.transaction('recovery', 'readwrite');
    const store = transaction.objectStore('recovery');
    const read = store.get('latest');
    let nextToken = null;
    read.onsuccess = () => {
      const current = read.result;
      const token = typeof current === 'string' ? 'legacy' : current?.token ?? null;
      if (token !== draftBaseToken) {
        transaction.abort();
        reject(new Error('Another Converra tab saved newer work. Export your current study before replacing that draft.'));
        return;
      }
      if (text === null) store.delete('latest');
      else { nextToken = crypto.randomUUID(); store.put({token:nextToken,text}, 'latest'); }
    };
    transaction.oncomplete = () => { draftBaseToken = nextToken; resolve(); };
    transaction.onerror = () => reject(new Error(transaction.error?.message || 'Draft transaction failed; previous draft retained'));
    transaction.onabort = () => reject(new Error(transaction.error?.message || 'Draft transaction aborted; previous draft retained'));
  });
}
export function converraSetDraftUnprotected(value) { draftUnprotected = value; }
if (typeof window !== 'undefined') window.addEventListener('beforeunload', event => {
  if (draftUnprotected) { event.preventDefault(); event.returnValue = ''; }
});
export function converraPublishStatus(text) {
  if (typeof window !== 'undefined' && new URL(location.href).searchParams.get('verify') === '1') {
    const value = Object.freeze(JSON.parse(text));
    Object.defineProperty(window, '__converraStatus', {value, configurable:true, writable:false});
  }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch, js_name = converraLoadDraft)]
    async fn load_draft() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = converraStoreDraft)]
    async fn store_draft(text: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = converraDiscardDraft)]
    async fn discard_draft() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = converraSetDraftUnprotected)]
    pub(crate) fn set_unprotected_draft(value: bool);
    #[wasm_bindgen(js_name = converraPublishStatus)]
    fn publish_status(text: &str);
}

fn js_error(value: JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "Browser draft storage is unavailable.".into())
}

pub(crate) async fn load_recovery_text() -> Result<Option<String>, String> {
    let value = load_draft().await.map_err(js_error)?;
    Ok(value.as_string())
}
pub(crate) async fn store_recovery_text(text: &str) -> Result<(), String> {
    store_draft(text).await.map(|_| ()).map_err(js_error)
}
pub(crate) async fn discard_recovery_text() -> Result<(), String> {
    discard_draft().await.map(|_| ()).map_err(js_error)
}

impl Workbench {
    pub(crate) fn publish_browser_status(&self) {
        let input_sha256 = if self.search_json.is_empty() {
            None
        } else {
            Some(format!(
                "{:x}",
                sha2::Sha256::digest(self.search_json.as_bytes())
            ))
        };
        let result_case = self
            .search_record
            .as_ref()
            .map(|record| record.case_sha256.as_str());
        let job = self.worker.as_ref();
        let preview = self
            .material_import
            .as_ref()
            .and_then(|draft| draft.preview.as_ref());
        let text = serde_json::json!({
            "route": format!("{:?}", self.page),
            "currentProjectId": self.search_case.as_ref().map(|case| &case.id).unwrap_or(&self.case.id),
            "currentStudyId": self.study_workspace.name,
            "activeVariantId": self.study_variant_id,
            "inputSha256": input_sha256,
            "resultCaseSha256": result_case,
            "resultCurrent": input_sha256.as_deref().is_some_and(|hash| result_case == Some(hash)),
            "jobKind": job.map(|worker| match worker.kind { crate::JobKind::Search=>"Search",crate::JobKind::Sweep=>"Sweep",crate::JobKind::Robustness=>"Robustness",crate::JobKind::Import=>"Import",crate::JobKind::Workspace=>"Workspace",_=>"Other" }),
            "jobState": if job.is_some() { "running" } else { "idle" },
            "cancelRequested": job.is_some_and(|worker| worker.cancel.load(std::sync::atomic::Ordering::Relaxed)),
            "dirty": self.recovery.dirty(),
            "draftProtected": self.recovery.protected(),
            "draftSavedAt": self.recovery.saved_at,
            "importDialogOpen": self.material_import.is_some(),
            "importState": if self.material_import.as_ref().is_some_and(|draft| draft.validated.is_some()) { "validated" } else if preview.is_some() { "preview" } else { "empty" },
            "importRows": preview.map(|preview| preview.total_rows),
            "recoveryPromptOpen": self.recovery.prompt.is_some(),
            "editorOpen": self.author.is_some(),
            "resultCount": self.study_workspace.variants.iter().map(|variant| variant.results.len()).sum::<usize>(),
            "comparisonSummary": self.study_diff.as_ref().map(|diff| serde_json::json!({
                "leftVariantId": diff.left_variant_id,
                "rightVariantId": diff.right_variant_id,
                "changedPaths": diff.changes.iter().map(|change| &change.pointer).take(24).collect::<Vec<_>>(),
                "results": diff.result_comparison,
            })),
            "message": self.message.1,
        }).to_string();
        publish_status(&text);
    }
}

/// Trigger a browser download for in-memory bytes — the web counterpart
/// of a save dialog (authored cases, reports, screenshots).
pub(crate) fn download_bytes(name: &str, bytes: &[u8]) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array);
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence(&parts) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let Ok(anchor) = document.create_element("a") else {
        return;
    };
    let _ = anchor.set_attribute("href", &url);
    let _ = anchor.set_attribute("download", name);
    if let Ok(element) = anchor.dyn_into::<web_sys::HtmlElement>() {
        element.click();
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

/// Native file pickers do not exist in the browser — open files through
/// rfd's async dialog, which resolves to in-memory bytes.
pub(crate) async fn pick_bytes(extensions: &[&str]) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .add_filter("JSON", extensions)
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    Ok(Some((handle.file_name().to_owned(), handle.read().await)))
}

/// Pick a browser file while bounding allocation before reading its contents.
pub(crate) async fn pick_bytes_limited(
    extensions: &[&str],
    max_bytes: usize,
) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(handle) = rfd::AsyncFileDialog::new()
        .add_filter("File", extensions)
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let name = handle.file_name();
    let size = handle.inner().size();
    if size > max_bytes as f64 {
        return Err(format!("{name} exceeds the {max_bytes}-byte size limit"));
    }
    let bytes = handle.read().await;
    if bytes.len() > max_bytes {
        return Err(format!("{name} exceeds the {max_bytes}-byte size limit"));
    }
    Ok(Some((name, bytes)))
}

fn show_boot_error(message: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Some(veil) = document.get_element_by_id("converra-loading") {
        veil.set_inner_html(&format!("converra failed to start:<br><br>{message}"));
    }
}

/// Worker body: dispatch on `kind` — `search` (default) runs a coupled
/// search, `fieldmap` generates a declared filament-model map for the
/// builder. Cancellation is `worker.terminate()` from the page — a
/// blocked worker thread cannot observe a flag.
fn run_request(text: &str) -> String {
    let request: serde_json::Value = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return serde_json::json!({
                "status": "err",
                "error": format!("worker request parse: {e}"),
            })
            .to_string();
        }
    };
    match request
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("search")
    {
        "table-preview" => match run_table_preview(&request) {
            Ok(preview) => serde_json::json!({"status":"table_preview_ok", "preview":preview}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error}).to_string(),
        },
        "material-import" => match run_material_import(&request) {
            Ok(bundle) => serde_json::json!({"status":"material_import_ok", "bundle_json":bundle}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error}).to_string(),
        },
        "recovery-validate" => match crate::recovery::RecoverySnapshot::from_json(request["snapshot_json"].as_str().unwrap_or_default()) {
            Ok(_) => serde_json::json!({"status":"recovery_ok"}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error.to_string()}).to_string(),
        },
        "fieldmap" => match run_fieldmap(&request) {
            Ok(generated) => {
                serde_json::json!({"status": "ok", "generated": generated}).to_string()
            }
            Err(error) => serde_json::json!({"status": "err", "error": error}).to_string(),
        },
        "study-search" => match run_study_search(&request) {
            Ok((workspace, record, cache_hit)) => serde_json::json!({"status":"study_ok", "workspace_json":workspace, "record":record, "cache_hit":cache_hit}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error}).to_string(),
        },
        "study-robustness" => match run_study_robustness(&request) {
            Ok(record) => serde_json::json!({"status":"robustness_ok", "record":record}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error}).to_string(),
        },
        "margin-sweep" => match run_margin_sweep(&request) {
            Ok(record) => serde_json::json!({"status":"sweep_ok", "record":record}).to_string(),
            Err(error) => serde_json::json!({"status":"err", "error":error}).to_string(),
        },
        _ => match run_search(&request) {
            Ok(record) => serde_json::json!({"status": "ok", "record": record}).to_string(),
            Err(error) => serde_json::json!({"status": "err", "error": error}).to_string(),
        },
    }
}

thread_local! {
    static STUDY_ENGINE_SESSION: RefCell<optcoil_search::study::StudyEngineSession> = RefCell::new(Default::default());
}

fn run_study_search(request: &serde_json::Value) -> Result<(String, String, bool), String> {
    let workspace_json = request
        .get("workspace_json")
        .and_then(|v| v.as_str())
        .ok_or("study worker request lacks workspace_json")?;
    let variant_id = request
        .get("variant_id")
        .and_then(|v| v.as_str())
        .ok_or("study worker request lacks variant_id")?;
    let mut workspace = optcoil_search::study::StudyWorkspace::from_json(workspace_json)
        .map_err(|e| e.to_string())?;
    let (result_id, cache_hit) = STUDY_ENGINE_SESSION
        .with(|session| {
            let mut session = session.borrow_mut();
            let cache_hit = workspace
                .variant(variant_id)
                .ok()
                .and_then(|variant| session.cached_result(variant).ok())
                .flatten()
                .is_some();
            session
                .run_variant_with(
                    &mut workspace,
                    variant_id,
                    &std::sync::atomic::AtomicBool::new(false),
                    None,
                )
                .map(|result_id| (result_id, cache_hit))
        })
        .map_err(|e| e.to_string())?;
    let record = workspace
        .variant(variant_id)
        .map_err(|e| e.to_string())?
        .results
        .iter()
        .find(|result| result.id == result_id)
        .ok_or("study runner returned no attached result")?
        .record_json
        .clone();
    let saved = workspace.to_json().map_err(|e| e.to_string())?;
    Ok((saved, record, cache_hit))
}

fn run_study_robustness(request: &serde_json::Value) -> Result<String, String> {
    let workspace_json = request
        .get("workspace_json")
        .and_then(|v| v.as_str())
        .ok_or("what-if worker request lacks workspace_json")?;
    let variant_ids: Vec<String> = serde_json::from_value(
        request
            .get("variant_ids")
            .cloned()
            .ok_or("what-if worker request lacks variant_ids")?,
    )
    .map_err(|e| format!("what-if variant ids parse: {e}"))?;
    let spec: optcoil_search::robustness::RobustnessSpec = serde_json::from_value(
        request
            .get("spec")
            .cloned()
            .ok_or("what-if worker request lacks spec")?,
    )
    .map_err(|e| format!("what-if spec parse: {e}"))?;
    let workspace = optcoil_search::study::StudyWorkspace::from_json(workspace_json)
        .map_err(|e| e.to_string())?;
    for id in &variant_ids {
        if workspace
            .variant(id)
            .map_err(|e| e.to_string())?
            .options
            .threads
            != Some(1)
        {
            return Err("Browser scenarios require an explicitly reviewed one-thread override for every variant".into());
        }
    }
    let record = optcoil_search::robustness::run_study_robustness(
        &workspace,
        &variant_ids,
        &spec,
        &std::sync::atomic::AtomicBool::new(false),
        None,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_string(&record).map_err(|e| e.to_string())
}

fn run_search(request: &serde_json::Value) -> Result<String, String> {
    let case_json = request
        .get("case_json")
        .and_then(|v| v.as_str())
        .ok_or("worker request lacks case_json")?;
    let datasets: std::collections::BTreeMap<String, optcoil_model::material::MaterialDataset> =
        if let Some(value) = request.get("datasets_json").and_then(|v| v.as_str()) {
            serde_json::from_str(value).map_err(|e| format!("worker datasets parse: {e}"))?
        } else if let Some(value) = request.get("dataset_json").and_then(|v| v.as_str()) {
            let dataset: optcoil_model::material::MaterialDataset =
                serde_json::from_str(value).map_err(|e| format!("worker dataset parse: {e}"))?;
            [(dataset.metadata.id.clone(), dataset)].into()
        } else {
            std::collections::BTreeMap::new()
        };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let record = optcoil_search::coupled_search::run_coupled_search_case_with_datasets_progress(
        case_json,
        &optcoil_search::coupled_search::CoupledSearchOptions { threads: Some(1) },
        &datasets,
        &cancel,
        None,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_string(&record).map_err(|e| e.to_string())
}

fn run_margin_sweep(request: &serde_json::Value) -> Result<String, String> {
    let case_json = request
        .get("case_json")
        .and_then(|v| v.as_str())
        .ok_or("margin worker request lacks case_json")?;
    let spec_json = request
        .get("spec_json")
        .and_then(|v| v.as_str())
        .ok_or("margin worker request lacks spec_json")?;
    let datasets: std::collections::BTreeMap<String, optcoil_model::material::MaterialDataset> =
        serde_json::from_str(
            request
                .get("datasets_json")
                .and_then(|v| v.as_str())
                .ok_or("margin worker request lacks datasets_json")?,
        )
        .map_err(|e| format!("margin worker datasets parse: {e}"))?;
    let record = optcoil_search::sensitivity::run_sensitivity_sweep_with_datasets(
        case_json,
        spec_json,
        &optcoil_search::coupled_search::CoupledSearchOptions { threads: Some(1) },
        &datasets,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_string(&record).map_err(|e| e.to_string())
}

fn table_request(
    request: &serde_json::Value,
) -> Result<
    (
        Vec<u8>,
        optcoil_model::tabular_import::TabularFormat,
        Option<String>,
    ),
    String,
> {
    let bytes: Vec<u8> =
        serde_json::from_value(request["bytes"].clone()).map_err(|e| e.to_string())?;
    if bytes.len() > optcoil_model::tabular_import::MAX_TABULAR_INPUT_BYTES {
        return Err("Table exceeds the import size limit.".into());
    }
    let format = serde_json::from_value(request["format"].clone()).map_err(|e| e.to_string())?;
    let sheet = request["sheet"].as_str().map(str::to_owned);
    Ok((bytes, format, sheet))
}

fn run_table_preview(
    request: &serde_json::Value,
) -> Result<optcoil_model::tabular_import::ImportPreview, String> {
    let (bytes, format, sheet) = table_request(request)?;
    optcoil_model::tabular_import::inspect_tabular(&bytes, format, sheet.as_deref(), 6)
        .map_err(|e| e.to_string())
}

fn run_material_import(request: &serde_json::Value) -> Result<String, String> {
    let (bytes, format, sheet) = table_request(request)?;
    let mapping = serde_json::from_value(request["mapping"].clone()).map_err(|e| e.to_string())?;
    let metadata =
        serde_json::from_value(request["metadata"].clone()).map_err(|e| e.to_string())?;
    optcoil_model::tabular_import::import_material(
        &bytes,
        format,
        sheet.as_deref(),
        mapping,
        metadata,
    )
    .map_err(|e| e.to_string())?
    .to_json()
    .map_err(|e| e.to_string())
}

fn run_binary_table_request(data: &JsValue) -> Result<String, String> {
    let payload = js_sys::Reflect::get(data, &"payload".into())
        .map_err(js_error)?
        .as_string()
        .ok_or("Table worker request lacks its metadata")?;
    let bytes = js_sys::Reflect::get(data, &"bytes".into()).map_err(js_error)?;
    if !bytes.is_instance_of::<js_sys::Uint8Array>() {
        return Err("Table worker requires binary table bytes".into());
    }
    let bytes: js_sys::Uint8Array = bytes.unchecked_into();
    if bytes.length() as usize > optcoil_model::tabular_import::MAX_TABULAR_INPUT_BYTES {
        return Err("Table exceeds the import size limit".into());
    }
    let bytes = bytes.to_vec();
    let request: serde_json::Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    let format = serde_json::from_value(request["format"].clone()).map_err(|e| e.to_string())?;
    let sheet = request["sheet"].as_str();
    match request["kind"].as_str() {
        Some("table-preview") => {
            let preview = optcoil_model::tabular_import::inspect_tabular(&bytes, format, sheet, 6)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status":"table_preview_ok", "preview":preview}).to_string())
        }
        Some("material-import") => {
            let mapping =
                serde_json::from_value(request["mapping"].clone()).map_err(|e| e.to_string())?;
            let metadata =
                serde_json::from_value(request["metadata"].clone()).map_err(|e| e.to_string())?;
            let bundle = optcoil_model::tabular_import::import_material(
                &bytes, format, sheet, mapping, metadata,
            )
            .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status":"material_import_ok", "bundle_json":bundle.to_json().map_err(|e| e.to_string())?}).to_string())
        }
        _ => Err("Unknown binary table worker request".into()),
    }
}

fn run_fieldmap(
    request: &serde_json::Value,
) -> Result<optcoil_search::fieldmap::GeneratedFieldMap, String> {
    let take = |key: &str| {
        request
            .get(key)
            .cloned()
            .ok_or_else(|| format!("worker request lacks {key}"))
    };
    let path3d: optcoil_model::path3d::CoilPath3D =
        serde_json::from_value(take("path3d")?).map_err(|e| format!("worker path3d parse: {e}"))?;
    let probe: [f64; 3] = serde_json::from_value(take("bore_probe_m")?)
        .map_err(|e| format!("worker probe parse: {e}"))?;
    let radial: f64 = serde_json::from_value(take("radial_band_m")?).map_err(|e| e.to_string())?;
    let width: f64 = serde_json::from_value(take("width_band_m")?).map_err(|e| e.to_string())?;
    let ni: f64 = serde_json::from_value(take("reference_ni_a")?).map_err(|e| e.to_string())?;
    let grid: optcoil_search::fieldmap::MapGrid = serde_json::from_value(
        request
            .get("grid")
            .cloned()
            .unwrap_or(serde_json::json!({})),
    )
    .map_err(|e| e.to_string())?;
    optcoil_search::fieldmap::generate_path3d_field_map(&path3d, radial, width, probe, ni, &grid)
        .map_err(|e| e.to_string())
}

/// Entry point for `worker.js` — runs inside the worker's global
/// scope and registers the search handler. The search is synchronous
/// (that is the point of the worker: it blocks its own thread, not
/// the page's).
#[wasm_bindgen]
pub fn converra_search_worker_main() {
    console_error_panic_hook::set_once();
    let scope = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();
    let out = scope.clone();
    let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        let reply = match event.data().as_string() {
            Some(text) => run_request(&text),
            None => run_binary_table_request(&event.data()).unwrap_or_else(|error| {
                serde_json::json!({"status":"err", "error":error}).to_string()
            }),
        };
        let _ = out.post_message(&wasm_bindgen::JsValue::from_str(&reply));
    }) as Box<dyn FnMut(_)>);
    scope.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
}

#[wasm_bindgen(start)]
pub async fn start_web() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    // The same wasm module is loaded by the page and its calculation worker.
    // Only the page owns a canvas; worker.js installs its message handler.
    if js_sys::global().is_instance_of::<web_sys::DedicatedWorkerGlobalScope>() {
        return Ok(());
    }
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("no window/document"))?;
    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .ok_or_else(|| JsValue::from_str("no converra_canvas element"))?;
    let web_options = eframe::WebOptions::default();
    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            web_options,
            Box::new(|cc| Ok(Box::new(Workbench::new(&cc.egui_ctx)?))),
        )
        .await
        .map_err(|e| {
            let message = e.as_string().unwrap_or_else(|| format!("{e:?}"));
            show_boot_error(&message);
            e
        })
}
