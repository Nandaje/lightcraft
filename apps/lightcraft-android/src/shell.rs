//! `AndroidShell` — wraps [`lightcraft_ui_egui::LightcraftApp`] with the Android File/Folder
//! Browser modal, SAF intent draining, GPU startup sentinel clearance, and preferences persistence.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_ui_egui::LightcraftApp;
use lightcraft_ui_egui::state::{Dialog, ViewMode};
use serde_json::json;

use crate::file_dialog::{AndroidFileDialog, DialogOutcome, SyncPickTarget};
use crate::gpu_startup::AndroidGpuStartup;
use crate::permissions::AndroidDirs;
use crate::services::{DialogBridge, PendingDialogRequest, save_android_prefs};

/// Top-level `eframe::App` for LightCraft on Android.
pub struct AndroidShell {
    /// Core LightCraft application state.
    pub inner: LightcraftApp,
    dirs: AndroidDirs,
    bridge: Arc<Mutex<DialogBridge>>,
    file_dialog: AndroidFileDialog,
    gpu_startup: AndroidGpuStartup,
    first_frame_marked: bool,
    last_prefs_bytes: Vec<u8>,
    last_prefs_check: f64,
}

impl AndroidShell {
    /// Create a new `AndroidShell` wrapping `inner`.
    #[must_use]
    pub fn new(
        mut inner: LightcraftApp,
        dirs: AndroidDirs,
        bridge: Arc<Mutex<DialogBridge>>,
        gpu_startup: AndroidGpuStartup,
    ) -> Self {
        if let Some(notice) = gpu_startup.demoted_notice() {
            inner.notices.push(notice.to_string());
        }
        // Pre-seed default export folder to Android's writable Pictures/LightCraft or Exports folder
        // if no previous export folder has been recorded yet.
        if inner.session.last_export.is_none() {
            let export_dir = dirs.preferred_export_dir().to_string_lossy().into_owned();
            inner.session.last_export = Some(json!({ "dir": export_dir }));
        }
        let file_dialog = AndroidFileDialog::new(&dirs);
        let last_prefs_bytes = serde_json::to_vec_pretty(&inner.ui).unwrap_or_default();
        Self {
            inner,
            dirs,
            bridge,
            file_dialog,
            gpu_startup,
            first_frame_marked: false,
            last_prefs_bytes,
            last_prefs_check: 0.0,
        }
    }

    /// Drain queued dialog requests and incoming external files from [`DialogBridge`].
    fn drain_bridge_events(&mut self, ctx: &egui::Context) {
        let mut requests = Vec::new();
        let mut external_files = Vec::new();

        if let Ok(mut b) = self.bridge.lock() {
            while let Some(req) = b.pending_requests.pop_front() {
                requests.push(req);
            }
            while let Some(path) = b.external_files.pop_front() {
                external_files.push(path);
            }
        }

        if !external_files.is_empty() {
            // If an async `PickRequest` delegated to Android's system SAF picker, deliver to its channel!
            if !self.file_dialog.deliver_saf_paths(&external_files) {
                let target = self.file_dialog.take_sync_target();
                self.handle_picked_or_incoming_paths(external_files, target, ctx);
            }
            ctx.request_repaint();
        }

        for req in requests {
            match req {
                PendingDialogRequest::AsyncPick { request, reply_tx } => {
                    self.file_dialog.open_for_request(request, reply_tx, &self.dirs);
                }
                PendingDialogRequest::SyncPick {
                    target,
                    kind,
                    title,
                    exts,
                } => {
                    self.file_dialog
                        .open_for_sync(target, kind, title, exts, &self.dirs);
                }
                PendingDialogRequest::RevealDir(dir) => {
                    self.file_dialog.open_for_sync(
                        SyncPickTarget::ImportPhotos,
                        lightcraft_ui_egui::pick::PickKind::Files,
                        "Browse Folder",
                        crate::file_dialog::PHOTO_AND_RAW_EXTENSIONS,
                        &self.dirs,
                    );
                    self.file_dialog.navigate_to(dir);
                }
            }
        }
    }

    /// Process paths chosen via a synchronous dialog button or received from an external Android
    /// `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` intent.
    pub fn handle_picked_or_incoming_paths(
        &mut self,
        paths: Vec<PathBuf>,
        target: Option<SyncPickTarget>,
        ctx: &egui::Context,
    ) {
        if paths.is_empty() {
            return;
        }
        let strings: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();

        match target {
            Some(SyncPickTarget::ExportFolder) => {
                if let Some(folder) = strings.first() {
                    if let Some(Dialog::Export { dir, .. }) = &mut self.inner.ui.dialog {
                        dir.clone_from(folder);
                    }
                    let mut prev = self.inner.session.last_export.clone().unwrap_or_else(|| json!({}));
                    prev["dir"] = json!(folder);
                    self.inner.session.last_export = Some(prev);
                }
            }
            Some(SyncPickTarget::DisplayProfile) => {
                if let Some(profile) = strings.first() {
                    self.inner.ui.settings.display_profile.clone_from(profile);
                }
            }
            Some(SyncPickTarget::FaceModel) => {
                if let Some(model) = strings.first() {
                    let _ = self.inner.run("dialog.faceModel", json!({ "path": model }));
                }
            }
            Some(SyncPickTarget::ImportPresets) => {
                let _ = self.inner.run("file.importPresets", json!({ "paths": strings }));
            }
            Some(SyncPickTarget::ImportPhotos) | None => {
                // Classify incoming files into models, presets, and photos/folders.
                let mut models = Vec::new();
                let mut others = Vec::new();
                for s in strings {
                    if lightcraft_ui_egui::is_model_file(&s) {
                        models.push(s);
                    } else {
                        others.push(s);
                    }
                }
                if let Some(model) = models.first() {
                    let _ = self.inner.run("dialog.faceModel", json!({ "path": model }));
                }
                let (presets, photos) = lightcraft_ui_egui::split_presets(others);
                if !presets.is_empty() {
                    let _ = self.inner.run("file.importPresets", json!({ "paths": presets }));
                }
                if !photos.is_empty() {
                    if let Err(e) = lightcraft_ui_egui::import::start_paths(&mut self.inner, photos) {
                        self.inner.toast_error(ctx, e);
                    } else {
                        self.inner.ui.view = ViewMode::PhotoGrid;
                    }
                }
            }
        }
    }

    fn tick_prefs(&mut self) {
        let now = self.inner.tick_now();
        if (0.0..1.0).contains(&(now - self.last_prefs_check)) {
            return;
        }
        self.last_prefs_check = now;
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.inner.ui)
            && bytes != self.last_prefs_bytes
        {
            if let Err(e) = save_android_prefs(&self.dirs, &self.inner.ui) {
                log::warn!("failed to save Android prefs: {e}");
            } else {
                self.last_prefs_bytes = bytes;
            }
        }
    }
}

impl eframe::App for AndroidShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        crate::touch_twins::dedupe_touch_mouse_twins(raw);
        self.inner.raw_input_hook(ctx, raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_bridge_events(ctx);
        self.inner.logic(ctx);
        self.tick_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.perf.host_frame_ms = frame.info().cpu_usage.map(|s| f64::from(s) * 1000.0);
        self.inner.ui(ui);

        match self.file_dialog.show(ui.ctx(), &self.dirs) {
            DialogOutcome::None | DialogOutcome::Cancelled => {}
            DialogOutcome::PickedPaths(paths) => {
                let target = self.file_dialog.take_sync_target();
                self.handle_picked_or_incoming_paths(paths, target, ui.ctx());
            }
            DialogOutcome::LaunchSafOpen { multiple } => {
                launch_saf_open_picker(multiple);
            }
            DialogOutcome::LaunchSafCreate { suggested_name } => {
                launch_saf_create_picker(&suggested_name);
            }
            DialogOutcome::RequestRuntimePermissions => {
                request_runtime_permissions();
            }
            DialogOutcome::RequestAllFilesAccess => {
                request_all_files_access();
            }
        }

        if !self.first_frame_marked {
            self.gpu_startup.mark_frame_healthy();
            self.first_frame_marked = true;
        }
    }

    fn on_exit(&mut self) {
        if let Err(e) = save_android_prefs(&self.dirs, &self.inner.ui) {
            log::error!("saving Android prefs failed: {e}");
        }
        if let Err(e) = self.inner.session.close_library() {
            log::error!("saving the library failed: {e}");
        }
        if !self.inner.shutdown(Duration::from_secs(2)) {
            log::warn!("quit: background work was still running after 2s");
        }
    }
}

fn launch_saf_open_picker(multiple: bool) {
    #[cfg(target_os = "android")]
    crate::android_entry::launch_saf_open_picker_jni(multiple);
    #[cfg(not(target_os = "android"))]
    let _ = multiple;
}

fn launch_saf_create_picker(suggested_name: &str) {
    #[cfg(target_os = "android")]
    crate::android_entry::launch_saf_create_picker_jni(suggested_name);
    #[cfg(not(target_os = "android"))]
    let _ = suggested_name;
}

fn request_runtime_permissions() {
    #[cfg(target_os = "android")]
    crate::android_entry::request_storage_permissions_jni();
}

fn request_all_files_access() {
    #[cfg(target_os = "android")]
    crate::android_entry::request_all_files_access_jni();
}
