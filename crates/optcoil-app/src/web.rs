// SPDX-License-Identifier: MIT

//! Browser entry point. The app code is target-agnostic — this module
//! only hosts it on a canvas. Filesystems and worker threads do not
//! exist here: files arrive as picker/drag-drop bytes and leave as
//! browser downloads.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Workbench;

const CANVAS_ID: &str = "converra_canvas";

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
        "fieldmap" => match run_fieldmap(&request) {
            Ok(generated) => {
                serde_json::json!({"status": "ok", "generated": generated}).to_string()
            }
            Err(error) => serde_json::json!({"status": "err", "error": error}).to_string(),
        },
        _ => match run_search(&request) {
            Ok(record) => serde_json::json!({"status": "ok", "record": record}).to_string(),
            Err(error) => serde_json::json!({"status": "err", "error": error}).to_string(),
        },
    }
}

fn run_search(request: &serde_json::Value) -> Result<String, String> {
    let case_json = request
        .get("case_json")
        .and_then(|v| v.as_str())
        .ok_or("worker request lacks case_json")?;
    let dataset: Option<optcoil_model::material::MaterialDataset> = request
        .get("dataset_json")
        .and_then(|v| v.as_str())
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| format!("worker dataset parse: {e}"))?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let record = optcoil_search::coupled_search::run_coupled_search_case_with_dataset_progress(
        case_json,
        &optcoil_search::coupled_search::CoupledSearchOptions { threads: Some(1) },
        dataset,
        &cancel,
        None,
    )
    .map_err(|e| e.to_string())?;
    serde_json::to_string(&record).map_err(|e| e.to_string())
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
            None => serde_json::json!({
                "status": "err",
                "error": "search worker received a non-string message",
            })
            .to_string(),
        };
        let _ = out.post_message(&wasm_bindgen::JsValue::from_str(&reply));
    }) as Box<dyn FnMut(_)>);
    scope.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();
}

#[wasm_bindgen(start)]
pub async fn start_web() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
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
