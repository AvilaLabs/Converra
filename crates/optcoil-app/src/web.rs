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

fn show_boot_error(message: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Some(veil) = document.get_element_by_id("converra-loading") {
        veil.set_inner_html(&format!("converra failed to start:<br><br>{message}"));
    }
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
