// SPDX-License-Identifier: MIT

// The workbench's native shell. The browser build enters through the
// cdylib's `start_web` in `web.rs`; this target only exists natively.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    optcoil_app::run_native()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
