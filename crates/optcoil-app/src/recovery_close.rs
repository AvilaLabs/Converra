//! Native close guard for unsaved recovery drafts.
//!
//! A window close first becomes a save request. If persistence fails, this
//! keeps the workbench open and offers retry, export, or an explicit discard.

#[cfg(not(target_arch = "wasm32"))]
use crate::Workbench;
#[cfg(not(target_arch = "wasm32"))]
use eframe::egui;

#[cfg(not(target_arch = "wasm32"))]
impl Workbench {
    pub(crate) fn native_close_guard(&mut self, ctx: &egui::Context) {
        // Screenshot capture deliberately closes itself after finishing. Keep
        // that opt-in rendering workflow independent of user draft persistence.
        if self.capture.is_some() || self.recovery.allow_close {
            return;
        }

        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.recovery.close_pending = true;
        }
        if !self.recovery.close_pending {
            return;
        }

        // Closing at startup must leave an unreviewed stored draft untouched.
        // No live edits have been observed while the restore prompt is active.
        if self.recovery.startup_recovery_without_edits() {
            self.allow_native_close(ctx);
            return;
        }

        if self.recovery.error.is_none() && !self.recovery.saving {
            self.request_immediate_recovery_save();
        }

        if self.recovery.can_close_cleanly() {
            self.allow_native_close(ctx);
            return;
        }

        let mut retry = false;
        let mut export = false;
        let mut close_without_saving = false;
        egui::Window::new("Save your work before closing")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Your recovery draft is not safely saved yet.");
                if let Some(error) = &self.recovery.error {
                    ui.colored_label(egui::Color32::from_rgb(166, 35, 41), error);
                } else if self.recovery.saving {
                    ui.label("Saving the latest draft…");
                } else {
                    ui.label("Waiting to save the latest draft…");
                }
                ui.horizontal(|ui| {
                    retry = ui.button("Retry draft save").clicked();
                    export = ui
                        .add_enabled(self.worker.is_none(), egui::Button::new("Export study workspace"))
                        .clicked();
                    close_without_saving = ui.button("Close without saving").clicked();
                });
                ui.small("Export saves the study workspace. Closing without saving discards unfinished local edits.");
            });

        if retry {
            self.retry_recovery_save();
            self.request_immediate_recovery_save();
        }
        if export {
            self.save_study_workspace(ctx);
        }
        if close_without_saving {
            // Drop must observe this flag and skip its best-effort final write;
            // the user's explicit choice must not turn into a silent overwrite.
            self.allow_native_close(ctx);
        }
    }

    fn allow_native_close(&mut self, ctx: &egui::Context) {
        self.recovery.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn native_close_with_recovery_error_stays_open_until_user_choice() {
        let ctx = egui::Context::default();
        let mut app = Workbench::new(&ctx).expect("headless workbench");
        app.mark_recovery_protected_for_test();
        app.recovery.error = Some("simulated recovery write failure".into());

        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("root viewport")
            .events
            .push(egui::ViewportEvent::Close);
        ctx.begin_pass(input);
        app.native_close_guard(&ctx);
        ctx.end_pass().drop_without_applying_deltas();

        assert!(app.recovery.close_pending);
        assert!(!app.recovery.allow_close);
    }

    #[test]
    fn close_recomputes_signature_before_trusting_a_saved_state() {
        let ctx = egui::Context::default();
        let mut app = Workbench::new(&ctx).expect("headless workbench");
        app.mark_recovery_protected_for_test();
        app.author = Some(crate::author::CaseDraft::guided());

        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("root viewport")
            .events
            .push(egui::ViewportEvent::Close);
        ctx.begin_pass(input);
        app.native_close_guard(&ctx);
        ctx.end_pass().drop_without_applying_deltas();

        assert!(app.recovery.close_pending);
        assert!(!app.recovery.allow_close);
        assert!(!app.recovery.protected());
    }
}
