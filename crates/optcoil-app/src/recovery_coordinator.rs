//! Autosave is separate from calculation jobs. Failed writes retain the last
//! successful snapshot, and a startup draft is never overwritten before the
//! user chooses whether to restore it.

use crate::{
    Instant, JobResult, Page, Workbench,
    author::CaseDraft,
    recovery::{ActiveSource, ActiveSourceKind, RecoverySnapshot},
};
use eframe::egui;
use std::{
    hash::{Hash, Hasher},
    sync::mpsc::{self, Receiver},
};

enum RecoveryEvent {
    Loaded(Box<Result<Option<RecoverySnapshot>, String>>),
    Stored(String, u64, Result<(), String>),
    Discarded(Result<(), String>),
}

pub(crate) struct RecoveryRuntime {
    enabled: bool,
    pub(crate) close_pending: bool,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) allow_close: bool,
    historical_search_record_json: Option<String>,
    historical_sweep_record_json: Option<String>,
    loaded: bool,
    pub prompt: Option<RecoverySnapshot>,
    pub error: Option<String>,
    pub saving: bool,
    pub saved_at: u64,
    signature: Option<u64>,
    content_signature: Option<u64>,
    last_saved_signature: Option<u64>,
    export_signature: Option<u64>,
    changed_at: Instant,
    checked_at: Instant,
    receiver: Option<Receiver<RecoveryEvent>>,
}

impl RecoveryRuntime {
    pub fn new() -> Self {
        Self {
            enabled: !cfg!(test),
            close_pending: false,
            #[cfg(not(target_arch = "wasm32"))]
            allow_close: false,
            historical_search_record_json: None,
            historical_sweep_record_json: None,
            loaded: cfg!(test),
            prompt: None,
            error: None,
            saving: false,
            saved_at: 0,
            signature: None,
            content_signature: None,
            last_saved_signature: None,
            export_signature: None,
            changed_at: Instant::now(),
            checked_at: Instant::now(),
            receiver: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn can_close_cleanly(&self) -> bool {
        self.protected() && !self.saving && self.receiver.is_none() && self.error.is_none()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn startup_recovery_without_edits(&self) -> bool {
        self.signature.is_none() && (!self.loaded || self.prompt.is_some())
    }

    pub fn dirty(&self) -> bool {
        self.content_signature.is_some() && self.content_signature != self.export_signature
    }
    pub fn enabled_for_ui(&self) -> bool {
        self.enabled
    }
    pub fn protected(&self) -> bool {
        self.signature.is_some() && self.signature == self.last_saved_signature
    }
}

pub(crate) fn unix_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now().max(0.0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
}

impl Workbench {
    pub(crate) fn initialize_recovery(&mut self) {
        if !self.recovery.enabled {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.recovery.receiver = Some(receiver);
        let ctx = self.ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = Self::persisted_path().map(|path| path.with_file_name("recovery.json"));
            std::thread::spawn(move || {
                let result = match path {
                    Some(path) if path.exists() => crate::recovery::read_snapshot(&path)
                        .map(Some)
                        .map_err(|e| e.to_string()),
                    Some(_) => Ok(None),
                    None => Err("No writable configuration directory is available.".into()),
                };
                let _ = sender.send(RecoveryEvent::Loaded(Box::new(result)));
                ctx.request_repaint();
            });
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            // The browser storage adapter validates on a Web Worker before
            // returning the bytes. Deserialization here only materializes them.
            let result = crate::web::load_recovery_text().await.and_then(|text| {
                text.map(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
                    .transpose()
            });
            let _ = sender.send(RecoveryEvent::Loaded(Box::new(result)));
            ctx.request_repaint();
        });
    }

    /// Hash only small identities and changed source allocations, not the full
    /// result ledger on every frame. Editable drafts are observed twice per
    /// second; large serialization/write validation happens only after a change.
    fn recovery_signature(&self, include_context: bool) -> Result<u64, String> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.search_json.hash(&mut h);
        if self.search_case.is_none() {
            serde_json::to_string(&self.case)
                .map_err(|e| e.to_string())?
                .hash(&mut h);
        }
        self.study_workspace.name.hash(&mut h);
        self.study_workspace.selected_variant_id.hash(&mut h);
        for variant in &self.study_workspace.variants {
            variant.id.hash(&mut h);
            variant.name.hash(&mut h);
            variant.case_json.hash(&mut h);
            variant.options.threads.hash(&mut h);
            for bundle in &variant.dataset_bundles {
                (bundle.as_ptr() as usize).hash(&mut h);
                bundle.len().hash(&mut h);
            }
            for result in &variant.results {
                result.id.hash(&mut h);
                result.input_sha256.hash(&mut h);
                result.attached_unix_ms.hash(&mut h);
                result.exact_input_key.hash(&mut h);
            }
        }
        self.study_workspace.robustness_results.len().hash(&mut h);
        for record in &self.study_workspace.robustness_results {
            record.input_fingerprint.hash(&mut h);
            record.engine_fingerprint.hash(&mut h);
            for row in &record.rows {
                row.run_record_json
                    .as_ref()
                    .map(|json| (json.as_ptr() as usize, json.len()))
                    .hash(&mut h);
            }
        }
        (self.search_record_json.as_ptr() as usize).hash(&mut h);
        self.search_record_json.len().hash(&mut h);
        self.robustness_variant_ids.hash(&mut h);
        serde_json::to_string(&self.robustness_scenarios)
            .map_err(|e| e.to_string())?
            .hash(&mut h);
        self.sweep_record
            .as_ref()
            .map(|record| &record.case_sha256)
            .hash(&mut h);
        self.sweep_record
            .as_ref()
            .map(|record| &record.spec_sha256)
            .hash(&mut h);
        self.sweep_record
            .as_ref()
            .map(|record| record.points.len())
            .hash(&mut h);
        self.compare_record
            .as_ref()
            .map(|record| &record.input_sha256)
            .hash(&mut h);
        self.search_record
            .as_ref()
            .map(|r| &r.input_sha256)
            .hash(&mut h);
        self.record.as_ref().map(|r| &r.input_sha256).hash(&mut h);
        self.reprice_usd_per_m.map(f64::to_bits).hash(&mut h);
        if include_context {
            self.page.hash(&mut h);
            self.edit_variant.hash(&mut h);
        }
        if let Some(author) = &self.author {
            author.recovery_json()?.hash(&mut h);
        }
        if let Some(importer) = &self.material_import {
            importer.state_signature()?.hash(&mut h);
            (importer.bytes.as_ptr() as usize).hash(&mut h);
            importer.bytes.len().hash(&mut h);
        }
        Ok(h.finish())
    }

    fn capture_recovery_snapshot(&self) -> Result<RecoverySnapshot, String> {
        let workspace_json =
            serde_json::to_string(&self.study_workspace).map_err(|e| e.to_string())?;
        let (kind, source_json, current_record_json) = if self.search_case.is_some() {
            if self.search_json.is_empty() {
                (
                    ActiveSourceKind::SavedRunRecord,
                    self.search_record_json.clone(),
                    None,
                )
            } else {
                let case_sha = {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha256::digest(self.search_json.as_bytes()))
                };
                (
                    ActiveSourceKind::CoupledCase,
                    self.search_json.clone(),
                    self.search_record
                        .as_ref()
                        .filter(|record| record.case_sha256 == case_sha)
                        .map(|_| self.search_record_json.clone()),
                )
            }
        } else {
            (
                ActiveSourceKind::Allocation,
                serde_json::to_string(&self.case).map_err(|e| e.to_string())?,
                self.record
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| e.to_string())?,
            )
        };
        let historical_search_record_json =
            if self.search_record.is_some() && current_record_json.is_none() {
                Some(self.search_record_json.clone())
            } else {
                self.recovery.historical_search_record_json.clone()
            };
        Ok(RecoverySnapshot {
            schema_version: crate::recovery::RECOVERY_SCHEMA_VERSION,
            saved_at_unix_ms: unix_ms().max(self.recovery.saved_at.saturating_add(1)),
            workspace_json,
            active_source: Some(ActiveSource {
                kind,
                source_json,
                current_record_json,
                page: format!("{:?}", self.page),
                edit_variant_id: self.edit_variant.clone(),
                display_price_usd_per_m: self.reprice_usd_per_m,
            }),
            author_json: self
                .author
                .as_ref()
                .map(CaseDraft::recovery_json)
                .transpose()?,
            importer_json: self
                .material_import
                .as_ref()
                .map(crate::importer::ImportDraft::recovery_json)
                .transpose()?,
            artifacts: Some(crate::recovery::RecoveryArtifacts {
                sweep_record_json: self
                    .sweep_record
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .or_else(|| self.recovery.historical_sweep_record_json.clone()),
                comparison_record_json: self
                    .compare_record
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|e| e.to_string())?,
                robustness_scenarios_json: Some(
                    serde_json::to_string(&self.robustness_scenarios).map_err(|e| e.to_string())?,
                ),
                robustness_variant_ids: self.robustness_variant_ids.clone(),
                dataset_bundle_jsons: self.current_recovery_bundles(),
                historical_search_record_json,
            }),
        })
    }

    #[cfg(any(test, not(target_arch = "wasm32")))]
    pub(crate) fn create_recovery_snapshot(&self) -> Result<RecoverySnapshot, String> {
        let snapshot = self.capture_recovery_snapshot()?;
        RecoverySnapshot::new_with_artifacts(
            snapshot.saved_at_unix_ms,
            snapshot.workspace_json,
            snapshot.active_source,
            snapshot.author_json,
            snapshot.artifacts,
            snapshot.importer_json,
        )
        .map_err(|e| e.to_string())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn flush_recovery_on_close(&mut self) {
        if self.recovery.allow_close
            || !self.recovery.enabled
            || !self.recovery.loaded
            || self.recovery.prompt.is_some()
        {
            return;
        }
        // Finish an earlier atomic writer before the final writer so an older
        // snapshot cannot land after the last edits made before closing.
        if self.recovery.saving {
            if let Some(receiver) = &self.recovery.receiver {
                match receiver.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(RecoveryEvent::Stored(_, saved_at, Ok(()))) => {
                        self.recovery.saved_at = saved_at
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            self.recovery.saving = false;
            self.recovery.receiver = None;
        }
        if let (Some(path), Ok(snapshot)) = (
            Self::persisted_path().map(|p| p.with_file_name("recovery.json")),
            self.create_recovery_snapshot(),
        ) {
            let _ = crate::recovery::write_snapshot_checked(
                &path,
                &snapshot,
                (self.recovery.saved_at != 0).then_some(self.recovery.saved_at),
            );
        }
    }

    /// All validation precedes mutation. Restoration creates no solver cache
    /// entry and never attaches an interrupted calculation as completed evidence.
    pub(crate) fn restore_recovery_snapshot(
        &mut self,
        ctx: &egui::Context,
        snapshot: RecoverySnapshot,
    ) -> Result<(), String> {
        snapshot.validate().map_err(|e| e.to_string())?;
        if !snapshot
            .editor_source_matches_workspace()
            .map_err(|e| e.to_string())?
        {
            return Err("The saved editor source no longer matches its study variant. The recovery file is retained.".into());
        }
        let workspace = optcoil_search::study::StudyWorkspace::from_json(&snapshot.workspace_json)
            .map_err(|e| e.to_string())?;
        let author = snapshot
            .author_json
            .as_deref()
            .map(CaseDraft::from_recovery_json)
            .transpose()?;
        let importer = snapshot
            .importer_json
            .as_deref()
            .map(crate::importer::ImportDraft::from_recovery_json)
            .transpose()?;
        let comparison = snapshot
            .artifacts
            .as_ref()
            .and_then(|a| a.comparison_record_json.as_deref())
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| e.to_string())?;
        let scenarios = snapshot
            .artifacts
            .as_ref()
            .and_then(|a| a.robustness_scenarios_json.as_deref())
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let sweep = if snapshot
            .sweep_matches_active_source()
            .map_err(|e| e.to_string())?
            == Some(true)
        {
            snapshot
                .artifacts
                .as_ref()
                .and_then(|a| a.sweep_record_json.as_deref())
                .map(serde_json::from_str)
                .transpose()
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let source_result = snapshot
            .active_source
            .as_ref()
            .map(|source| {
                crate::load_project_json(
                    source.source_json.clone(),
                    std::path::PathBuf::from("recovered-project.json"),
                )
            })
            .transpose()?;
        let current_result = snapshot
            .active_source
            .as_ref()
            .and_then(|source| source.current_record_json.as_deref())
            .map(|json| {
                if snapshot
                    .active_source
                    .as_ref()
                    .is_some_and(|s| s.kind == ActiveSourceKind::CoupledCase)
                {
                    serde_json::from_str(json)
                        .map(|record| JobResult::SearchCompleted(Box::new(record)))
                        .map_err(|e| e.to_string())
                } else {
                    serde_json::from_str(json)
                        .map(|record| JobResult::Calculated(Box::new(record)))
                        .map_err(|e| e.to_string())
                }
            })
            .transpose()?;
        if self.worker.is_some() {
            return Err("Wait for the current task before restoring a draft.".into());
        }
        self.importing_workspace = true;
        if let Some(result) = source_result {
            self.apply_result(ctx, Ok(result));
        }
        self.study_workspace = workspace;
        self.study_variant_id = self
            .study_workspace
            .selected_variant_id
            .clone()
            .filter(|id| {
                self.study_workspace
                    .variant(id)
                    .is_ok_and(|variant| variant.case_json == self.search_json)
            });
        if let Some(id) = self.study_variant_id.clone() {
            let bundles = self
                .study_workspace
                .variant(&id)
                .map(|v| v.dataset_bundles.clone())
                .unwrap_or_default();
            self.install_workspace_bundles(&bundles);
        }
        if let Some(artifacts) = &snapshot.artifacts {
            self.install_workspace_bundles(&artifacts.dataset_bundle_jsons);
        }
        if let Some(result) = current_result {
            self.apply_result(ctx, Ok(result));
        }
        self.importing_workspace = false;
        self.study_session = Default::default();
        self.author = author;
        self.material_import = importer;
        self.compare_record = comparison;
        self.robustness_scenarios = scenarios;
        self.sweep_record = sweep;
        self.recovery.historical_search_record_json = snapshot
            .artifacts
            .as_ref()
            .and_then(|a| a.historical_search_record_json.clone());
        self.recovery.historical_sweep_record_json = if self.sweep_record.is_none() {
            snapshot
                .artifacts
                .as_ref()
                .and_then(|a| a.sweep_record_json.clone())
        } else {
            None
        };
        if let Some(artifacts) = &snapshot.artifacts {
            self.robustness_variant_ids = artifacts.robustness_variant_ids.clone();
        }
        if let Some(source) = snapshot.active_source {
            self.page = match source.page.as_str() {
                "Materials" => Page::Materials,
                "Checks" => Page::Checks,
                "Reports" => Page::Reports,
                "Study" => Page::Study,
                "Integrations" => Page::Integrations,
                _ => Page::Overview,
            };
            self.edit_variant = source.edit_variant_id;
            self.reprice_usd_per_m = source.display_price_usd_per_m;
        }
        self.recovery.prompt = None;
        self.recovery.error = None;
        self.recovery.saved_at = snapshot.saved_at_unix_ms;
        let signature = self.recovery_signature(true)?;
        self.recovery.signature = Some(signature);
        self.recovery.content_signature = Some(self.recovery_signature(false)?);
        self.recovery.last_saved_signature = Some(signature);
        self.message = (false, "Draft restored, including unfinished edits and completed evidence. Interrupted calculations can be run again.".into());
        self.refresh_study_summary();
        Ok(())
    }

    pub(crate) fn mark_study_exported(&mut self) {
        if let Ok(signature) = self.recovery_signature(false) {
            self.recovery.export_signature = Some(signature);
        }
    }

    fn current_recovery_bundles(&self) -> Vec<String> {
        self.search_dataset
            .iter()
            .chain(self.search_spec_datasets.values())
            .filter_map(|source| source.bundle_json.clone())
            .collect()
    }

    fn poll_recovery(&mut self) {
        let event = self
            .recovery
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some(event) = event {
            self.recovery.receiver = None;
            match event {
                RecoveryEvent::Loaded(result) => match *result {
                    Ok(snapshot) => {
                        self.recovery.loaded = true;
                        self.recovery.prompt = snapshot;
                        self.recovery.error = None;
                    }
                    Err(error) => {
                        self.recovery.loaded = false;
                        self.recovery.error = Some(error);
                    }
                },
                RecoveryEvent::Stored(signature, saved_at, result) => {
                    self.recovery.saving = false;
                    match result {
                        Ok(()) => {
                            self.recovery.last_saved_signature = signature.parse().ok();
                            self.recovery.saved_at = saved_at;
                            self.recovery.error = None;
                        }
                        Err(error) => {
                            self.recovery.error = Some(error);
                            self.recovery.changed_at = Instant::now();
                        }
                    }
                }
                RecoveryEvent::Discarded(result) => match result {
                    Ok(()) => {
                        self.recovery.loaded = true;
                        self.recovery.prompt = None;
                        self.recovery.error = None;
                        self.recovery.saved_at = 0;
                        self.recovery.signature = None;
                        self.recovery.content_signature = None;
                        self.recovery.last_saved_signature = None;
                        self.recovery.changed_at = Instant::now();
                    }
                    Err(error) => self.recovery.error = Some(error),
                },
            }
        }
    }

    fn store_recovery(&mut self, snapshot: RecoverySnapshot, signature: u64) {
        let saved_at = snapshot.saved_at_unix_ms;
        let (sender, receiver) = mpsc::channel();
        self.recovery.receiver = Some(receiver);
        self.recovery.saving = true;
        let ctx = self.ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = Self::persisted_path().map(|p| p.with_file_name("recovery.json"));
            let expected_saved_at = (self.recovery.saved_at != 0).then_some(self.recovery.saved_at);
            std::thread::spawn(move || {
                let result = path
                    .ok_or_else(|| "No writable configuration directory is available.".to_owned())
                    .and_then(|path| {
                        crate::recovery::write_snapshot_checked(&path, &snapshot, expected_saved_at)
                            .map_err(|e| e.to_string())
                    });
                let _ = sender.send(RecoveryEvent::Stored(
                    signature.to_string(),
                    saved_at,
                    result,
                ));
                ctx.request_repaint();
            });
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            // Validation runs in the browser storage adapter's worker; an
            // invalid draft never replaces the previous committed snapshot.
            let result = match serde_json::to_string(&snapshot) {
                Ok(json) => crate::web::store_recovery_text(&json).await,
                Err(error) => Err(error.to_string()),
            };
            let _ = sender.send(RecoveryEvent::Stored(
                signature.to_string(),
                saved_at,
                result,
            ));
            ctx.request_repaint();
        });
    }

    fn discard_recovery(&mut self) {
        let (sender, receiver) = mpsc::channel();
        self.recovery.receiver = Some(receiver);
        let ctx = self.ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = Self::persisted_path().map(|p| p.with_file_name("recovery.json"));
            let expected_saved_at = self
                .recovery
                .prompt
                .as_ref()
                .map(|snapshot| snapshot.saved_at_unix_ms)
                .or_else(|| (self.recovery.saved_at != 0).then_some(self.recovery.saved_at));
            let unreadable = !self.recovery.loaded && self.recovery.error.is_some();
            std::thread::spawn(move || {
                let result = path
                    .map(|path| {
                        if unreadable {
                            crate::recovery::discard_snapshot(&path)
                        } else {
                            crate::recovery::discard_snapshot_checked(&path, expected_saved_at)
                        }
                        .map_err(|e| e.to_string())
                    })
                    .unwrap_or(Ok(()));
                let _ = sender.send(RecoveryEvent::Discarded(result));
                ctx.request_repaint();
            });
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            let result = crate::web::discard_recovery_text().await;
            let _ = sender.send(RecoveryEvent::Discarded(result));
            ctx.request_repaint();
        });
    }

    pub(crate) fn recovery_tick(&mut self) {
        self.poll_recovery();
        if !self.recovery.enabled || !self.recovery.loaded || self.recovery.prompt.is_some() {
            return;
        }
        let editing_event = self.ctx.input(|input| {
            input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Key { pressed: true, .. }
                        | egui::Event::PointerButton { pressed: false, .. }
                )
            })
        });
        if self.recovery.close_pending
            || editing_event
            || self.recovery.checked_at.elapsed().as_millis() >= 500
        {
            self.recovery.checked_at = Instant::now();
            self.recovery.content_signature = self.recovery_signature(false).ok();
            match self.recovery_signature(true) {
                Ok(signature) if self.recovery.signature != Some(signature) => {
                    self.recovery.signature = Some(signature);
                    self.recovery.changed_at = Instant::now();
                }
                Err(error) => self.recovery.error = Some(error),
                _ => {}
            }
        }
        if self.recovery.receiver.is_none()
            && self.recovery.error.is_none()
            && !self.recovery.protected()
            && self.recovery.changed_at.elapsed().as_millis() >= 1200
        {
            match self.capture_recovery_snapshot() {
                Ok(snapshot) => {
                    if let Some(signature) = self.recovery.signature {
                        self.store_recovery(snapshot, signature);
                    }
                }
                Err(error) => {
                    self.recovery.error = Some(error);
                    self.recovery.changed_at = Instant::now();
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        crate::web::set_unprotected_draft(self.recovery.dirty() && !self.recovery.protected());
        self.ctx
            .request_repaint_after(std::time::Duration::from_millis(500));
    }

    pub(crate) fn recovery_window(&mut self, ctx: &egui::Context) {
        if self.recovery.prompt.is_none() && (self.recovery.loaded || self.recovery.error.is_none())
        {
            return;
        }
        let mut restore = false;
        let mut discard = false;
        egui::Modal::new(egui::Id::new("recover-work")).show(ctx, |ui| {
            ui.set_max_width(620.0);
            ui.heading(if self.recovery.prompt.is_some() { "A saved draft is available" } else { "The previous draft could not be read" });
            ui.label(if self.recovery.prompt.is_some() { "Restore replaces the current session with your saved study, inputs, results and unfinished edits. Discard keeps the current session and removes the saved draft." } else { "The stored draft is retained. Retry after resolving the storage error, or explicitly discard it to start a new draft." });
            ui.small("A recovery draft is stored on this device. Export a study file for a portable copy.");
            ui.horizontal(|ui| { restore = ui.add_enabled(self.worker.is_none() && self.recovery.prompt.is_some(), egui::Button::new("Restore draft")).clicked(); discard = ui.add_enabled(self.recovery.receiver.is_none(), egui::Button::new("Discard draft")).clicked(); if ui.add_enabled(self.recovery.receiver.is_none(), egui::Button::new("Retry storage")).clicked() { self.recovery.error = None; self.initialize_recovery(); } });
            if let Some(error) = &self.recovery.error { ui.colored_label(egui::Color32::from_rgb(166, 35, 41), error); }
        });
        if restore
            && let Some(snapshot) = self.recovery.prompt.clone()
            && let Err(error) = self.restore_recovery_snapshot(ctx, snapshot)
        {
            self.recovery.error = Some(error);
        }
        if discard {
            self.discard_recovery();
        }
    }

    pub(crate) fn recovery_label(&self) -> String {
        if let Some(error) = &self.recovery.error {
            return format!("Draft recovery needs attention: {error}");
        }
        if self.recovery.saving {
            "Saving recovery draft…".into()
        } else if self.recovery.protected() {
            if self.recovery.dirty() {
                "Draft protected on this device · study has unexported changes".into()
            } else {
                "Study exported · recovery draft protected".into()
            }
        } else {
            "Unsaved changes · recovery draft pending".into()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn request_immediate_recovery_save(&mut self) {
        if !self.recovery.loaded
            || self.recovery.prompt.is_some()
            || self.recovery.receiver.is_some()
            || self.recovery.error.is_some()
        {
            return;
        }
        match self.recovery_signature(true) {
            Ok(signature) => {
                self.recovery.signature = Some(signature);
                self.recovery.content_signature = self.recovery_signature(false).ok();
                if self.recovery.enabled && !self.recovery.protected() {
                    match self.capture_recovery_snapshot() {
                        Ok(snapshot) => self.store_recovery(snapshot, signature),
                        Err(error) => self.recovery.error = Some(error),
                    }
                }
            }
            Err(error) => self.recovery.error = Some(error),
        }
    }

    pub(crate) fn has_recovered_history(&self) -> bool {
        self.recovery.historical_search_record_json.is_some()
            || self.recovery.historical_sweep_record_json.is_some()
    }

    #[cfg(test)]
    pub(crate) fn mark_recovery_protected_for_test(&mut self) {
        let signature = self.recovery_signature(true).unwrap();
        self.recovery.signature = Some(signature);
        self.recovery.content_signature = Some(self.recovery_signature(false).unwrap());
        self.recovery.last_saved_signature = Some(signature);
    }

    pub(crate) fn export_recovered_history(&mut self, ctx: &egui::Context) {
        let json = serde_json::json!({"schema":"converra-recovered-history/v1", "historical_run_record_json":self.recovery.historical_search_record_json, "historical_sweep_record_json":self.recovery.historical_sweep_record_json}).to_string();
        #[cfg(target_arch = "wasm32")]
        {
            let _ = ctx;
            crate::web::download_bytes("recovered-history.json", json.as_bytes());
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.launch(ctx, crate::JobKind::Export, move |_| {
            use std::io::Write;
            let Some(path) = rfd::FileDialog::new()
                .set_title("Export historical recovery evidence")
                .set_file_name("recovered-history.json")
                .save_file()
            else {
                return Ok(crate::JobResult::Dismissed);
            };
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
            Ok(crate::JobResult::Exported(path))
        });
    }

    pub(crate) fn retry_recovery_save(&mut self) {
        let conflict = self
            .recovery
            .error
            .as_ref()
            .is_some_and(|error| error.to_ascii_lowercase().contains("another converra"));
        self.recovery.error = None;
        if conflict && self.recovery.receiver.is_none() {
            self.initialize_recovery();
            return;
        }
        if !self.recovery.loaded && self.recovery.receiver.is_none() {
            self.initialize_recovery();
        }
        self.recovery.changed_at = Instant::now();
    }
}
