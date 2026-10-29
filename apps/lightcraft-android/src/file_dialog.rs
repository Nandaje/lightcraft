//! Touch-friendly in-app Android File & Folder Browser modal for LightCraft.
//!
//! Integrates with [`lightcraft_ui_egui::pick::PickRequest`] (`Files`, `File`, `Folder`, `Save`)
//! as well as direct synchronous dialog actions, providing:
//! 1. Quick-access bookmark pills (`Camera`, `DCIM`, `Pictures`, `Downloads`, `Documents`,
//!    `Shared Storage`, `App Exports`, `SAF Imports`, `Library`).
//! 2. Multi-file selection (`PickKind::Files`) with checkboxes and `"Select All Photos"` for batch
//!    RAW + JPEG/HEIC/AVIF/TIFF/DNG imports.
//! 3. Folder selection (`PickKind::Folder`) with inline `"+ New Folder"` creation for importing
//!    folders, choosing export destinations, or creating/opening LightCraft libraries.
//! 4. Save-As (`PickKind::Save`) with filename editing and extension enforcement.
//! 5. Storage permission status bar with one-tap buttons for Android runtime permissions,
//!    Android 11+ `MANAGE_EXTERNAL_STORAGE` (`All-Files Access`), and the Android System
//!    Storage Access Framework (`ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use lightcraft_ui_egui::pick::{PickKind, PickRequest};

use crate::permissions::{AndroidDirs, StoragePermissionStatus, sanitize_filename};

/// Supported photo/RAW/preset/model extensions recognized by default when browsing in LightCraft.
pub const PHOTO_AND_RAW_EXTENSIONS: &[&str] = lightcraft_engine::import::EXTENSIONS;

/// Outcome of a single frame of [`AndroidFileDialog::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Dialog is not open or still being interacted with.
    None,
    /// User confirmed one or more paths while no channel waiter was attached (e.g. opened from toolbar).
    PickedPaths(Vec<PathBuf>),
    /// User tapped "System File Picker (SAF)" for opening file(s).
    LaunchSafOpen { multiple: bool },
    /// User tapped "System Save As (SAF)" with a suggested filename.
    LaunchSafCreate { suggested_name: String },
    /// User tapped "Request Storage Permissions".
    RequestRuntimePermissions,
    /// User tapped "Grant All-Files Access" (`MANAGE_EXTERNAL_STORAGE`).
    RequestAllFilesAccess,
    /// User cancelled the modal.
    Cancelled,
}

#[derive(Debug, Clone)]
struct DirEntryRow {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size_bytes: u64,
}

/// State of the Android modal file/folder browser.
pub struct AndroidFileDialog {
    open: bool,
    kind: PickKind,
    title: String,
    filter_label: Option<String>,
    filter_exts: Vec<String>,
    filter_enabled: bool,
    current_dir: PathBuf,
    entries: Vec<DirEntryRow>,
    selected_files: BTreeSet<PathBuf>,
    filename_input: String,
    new_folder_name: String,
    creating_folder: bool,
    status_error: Option<String>,
    /// Channel sender from `Services.picker` waiting for the user's selection.
    reply_tx: Option<Sender<Vec<String>>>,
    /// Channel sender held when the user delegated an active `PickRequest` to Android's SAF picker.
    saf_waiter: Option<Sender<Vec<String>>>,
    /// Optional callback invoked when a synchronous dialog (like Export folder browse) finishes.
    sync_target: Option<SyncPickTarget>,
}

/// Target for dialogs opened by synchronous `Services` callbacks where LightCraft's UI doesn't
/// use `pick::ask` (for example, choosing a folder or model from a settings/dialog button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPickTarget {
    /// Import selected photos/folders into the library.
    ImportPhotos,
    /// Import preset files.
    ImportPresets,
    /// Open face model dialog.
    FaceModel,
    /// Set display ICC profile in Settings.
    DisplayProfile,
    /// Set export directory on the open Export dialog.
    ExportFolder,
}

impl AndroidFileDialog {
    /// Create a closed file dialog rooted at the best initial photo directory.
    #[must_use]
    pub fn new(dirs: &AndroidDirs) -> Self {
        let initial_dir = if StoragePermissionStatus::current().can_browse_shared_storage()
            && dirs.shared_dcim_dir.exists()
        {
            dirs.shared_dcim_dir.clone()
        } else if StoragePermissionStatus::current().can_browse_shared_storage()
            && dirs.shared_pictures_dir.exists()
        {
            dirs.shared_pictures_dir.clone()
        } else {
            dirs.exports_dir()
        };

        Self {
            open: false,
            kind: PickKind::Files,
            title: String::from("Browse Files"),
            filter_label: None,
            filter_exts: Vec::new(),
            filter_enabled: true,
            current_dir: initial_dir,
            entries: Vec::new(),
            selected_files: BTreeSet::new(),
            filename_input: String::new(),
            new_folder_name: String::new(),
            creating_folder: false,
            status_error: None,
            reply_tx: None,
            saf_waiter: None,
            sync_target: None,
        }
    }

    /// Whether the modal is currently visible.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Whether an asynchronous SAF request is waiting to deliver into a `PickRequest` channel.
    #[must_use]
    pub const fn has_saf_waiter(&self) -> bool {
        self.saf_waiter.is_some()
    }

    /// Deliver paths picked via Android's system SAF picker (`ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT`)
    /// to a waiting `PickRequest` channel if one is active; returns `true` if consumed by a waiter.
    pub fn deliver_saf_paths(&mut self, paths: &[PathBuf]) -> bool {
        if let Some(tx) = self.saf_waiter.take() {
            let strings: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let _ = tx.send(strings);
            return true;
        }
        false
    }

    /// Take the synchronous target if the dialog was opened for a non-channel action.
    pub fn take_sync_target(&mut self) -> Option<SyncPickTarget> {
        self.sync_target.take()
    }

    /// Open the modal for an asynchronous [`PickRequest`] from `Services.picker`.
    pub fn open_for_request(
        &mut self,
        req: PickRequest,
        reply_tx: Sender<Vec<String>>,
        dirs: &AndroidDirs,
    ) {
        self.kind = req.kind;
        self.title = req.title.unwrap_or_else(|| match req.kind {
            PickKind::Files => "Select Files".to_string(),
            PickKind::File => "Select File".to_string(),
            PickKind::Folder => "Select Folder".to_string(),
            PickKind::Save => "Save As".to_string(),
        });
        if let Some((label, exts)) = req.filter {
            self.filter_label = Some(label);
            self.filter_exts = exts.iter().map(|e| e.to_ascii_lowercase()).collect();
            self.filter_enabled = true;
        } else {
            self.filter_label = None;
            self.filter_exts.clear();
            self.filter_enabled = false;
        }
        self.filename_input = req.file_name.unwrap_or_default();
        self.selected_files.clear();
        self.creating_folder = false;
        self.new_folder_name.clear();
        self.status_error = None;
        self.reply_tx = Some(reply_tx);
        self.sync_target = None;
        self.open = true;

        // Choose a sensible starting directory for the request kind.
        if matches!(req.kind, PickKind::Save) && self.current_dir == dirs.shared_dcim_dir {
            self.current_dir = dirs.preferred_export_dir();
        } else if !self.current_dir.exists() {
            self.current_dir = dirs.exports_dir();
        }
        self.refresh_entries();
    }

    /// Open the modal for a synchronous UI action.
    pub fn open_for_sync(
        &mut self,
        target: SyncPickTarget,
        kind: PickKind,
        title: impl Into<String>,
        exts: &[&str],
        dirs: &AndroidDirs,
    ) {
        self.kind = kind;
        self.title = title.into();
        if exts.is_empty() {
            self.filter_label = None;
            self.filter_exts.clear();
            self.filter_enabled = false;
        } else {
            self.filter_label = Some("Supported Files".to_string());
            self.filter_exts = exts.iter().map(|e| e.to_ascii_lowercase()).collect();
            self.filter_enabled = true;
        }
        self.filename_input.clear();
        self.selected_files.clear();
        self.creating_folder = false;
        self.new_folder_name.clear();
        self.status_error = None;
        self.reply_tx = None;
        self.sync_target = Some(target);
        self.open = true;
        if !self.current_dir.exists() {
            self.current_dir = dirs.exports_dir();
        }
        self.refresh_entries();
    }

    /// Navigate to `dir` and refresh the file list.
    pub fn navigate_to(&mut self, dir: PathBuf) {
        self.current_dir = dir;
        self.selected_files.clear();
        self.status_error = None;
        self.refresh_entries();
    }

    /// Re-read the current directory entries, applying extension filters and sorting folders first.
    pub fn refresh_entries(&mut self) {
        self.entries.clear();
        let read_dir = match std::fs::read_dir(&self.current_dir) {
            Ok(rd) => rd,
            Err(err) => {
                self.status_error = Some(format!(
                    "Cannot list {}: {err}. Tap 'All-Files Access' or 'System Picker (SAF)' above.",
                    self.current_dir.display()
                ));
                return;
            }
        };

        let mut rows = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let meta = entry.metadata().ok();
            let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
            let size_bytes = meta.as_ref().map_or(0, std::fs::Metadata::len);

            if !is_dir && matches!(self.kind, PickKind::Folder) {
                continue;
            }
            if !is_dir && self.filter_enabled && !self.filter_exts.is_empty() && !matches_exts(&path, &self.filter_exts) {
                continue;
            }
            rows.push(DirEntryRow {
                name,
                path,
                is_dir,
                size_bytes,
            });
        }

        rows.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
        });
        self.entries = rows;
    }

    fn finish_with_paths(&mut self, paths: Vec<PathBuf>) -> DialogOutcome {
        self.open = false;
        if let Some(tx) = self.reply_tx.take() {
            let strings: Vec<String> = paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let _ = tx.send(strings);
            DialogOutcome::None
        } else {
            DialogOutcome::PickedPaths(paths)
        }
    }

    fn cancel(&mut self) -> DialogOutcome {
        self.open = false;
        self.sync_target = None;
        if let Some(tx) = self.reply_tx.take() {
            let _ = tx.send(Vec::new());
        }
        DialogOutcome::Cancelled
    }

    /// Render the modal dialog if open and return any [`DialogOutcome`].
    pub fn show(&mut self, ctx: &egui::Context, dirs: &AndroidDirs) -> DialogOutcome {
        if !self.open {
            return DialogOutcome::None;
        }

        let mut outcome = DialogOutcome::None;
        let perms = StoragePermissionStatus::current();

        egui::Window::new(&self.title)
            .id(egui::Id::unique("lightcraft_android_file_dialog"))
            .collapsible(false)
            .resizable(true)
            .default_width(680.0)
            .default_height(500.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Permission status & SAF shortcut bar
                ui.horizontal_wrapped(|ui| {
                    if !perms.can_browse_shared_storage() {
                        ui.colored_label(
                            egui::Color32::from_rgb(245, 166, 35),
                            "Shared storage permission not granted:",
                        );
                        if ui.button("Grant Storage Permission").clicked() {
                            outcome = DialogOutcome::RequestRuntimePermissions;
                        }
                    }
                    if !perms.manage_all_files && ui.button("All-Files Access (Settings)").clicked() {
                        outcome = DialogOutcome::RequestAllFilesAccess;
                    }
                    if matches!(self.kind, PickKind::Files | PickKind::File)
                        && ui.button("System Picker (SAF)").clicked()
                    {
                        self.open = false;
                        self.saf_waiter = self.reply_tx.take();
                        outcome = DialogOutcome::LaunchSafOpen {
                            multiple: matches!(self.kind, PickKind::Files),
                        };
                    }
                    if matches!(self.kind, PickKind::Save)
                        && ui.button("System Save As (SAF)").clicked()
                    {
                        let suggested = self.resolved_save_filename();
                        self.open = false;
                        self.saf_waiter = self.reply_tx.take();
                        outcome = DialogOutcome::LaunchSafCreate {
                            suggested_name: suggested,
                        };
                    }
                });

                ui.separator();

                // Quick-access bookmark strip
                ui.horizontal_wrapped(|ui| {
                    ui.label("Bookmarks:");
                    for (label, path) in dirs.bookmarks() {
                        let selected = self.current_dir == path;
                        if ui.selectable_label(selected, label).clicked() {
                            self.navigate_to(path);
                        }
                    }
                });

                // Current folder path + Up + Refresh + New Folder
                ui.horizontal(|ui| {
                    if ui.button("⬆ Up").clicked() {
                        if let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) {
                            if !parent.as_os_str().is_empty() {
                                self.navigate_to(parent);
                            }
                        }
                    }
                    if ui.button("⟳ Refresh").clicked() {
                        self.refresh_entries();
                    }
                    if ui.button("➕ New Folder").clicked() {
                        self.creating_folder = !self.creating_folder;
                    }
                    if let Some(label) = &self.filter_label
                        && !self.filter_exts.is_empty()
                        && !matches!(self.kind, PickKind::Folder)
                    {
                        let filter_text = format!("Filter: {label}");
                        if ui.checkbox(&mut self.filter_enabled, filter_text).changed() {
                            self.refresh_entries();
                        }
                    }
                    ui.label(
                        egui::RichText::new(self.current_dir.to_string_lossy())
                            .monospace()
                            .small(),
                    );
                });

                // Inline new folder creation bar
                if self.creating_folder {
                    ui.horizontal(|ui| {
                        ui.label("Folder name:");
                        let resp = ui.text_edit_singleline(&mut self.new_folder_name);
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.button("Create").clicked() || enter) && !self.new_folder_name.trim().is_empty() {
                            let clean = sanitize_filename(self.new_folder_name.trim());
                            let new_dir = self.current_dir.join(clean);
                            match std::fs::create_dir_all(&new_dir) {
                                Ok(()) => {
                                    self.creating_folder = false;
                                    self.new_folder_name.clear();
                                    self.navigate_to(new_dir);
                                }
                                Err(e) => {
                                    self.status_error = Some(format!("Could not create folder: {e}"));
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.creating_folder = false;
                            self.new_folder_name.clear();
                        }
                    });
                }

                if let Some(err) = &self.status_error {
                    ui.colored_label(egui::Color32::from_rgb(235, 87, 87), err);
                }

                // Multi-file selection convenience bar
                if matches!(self.kind, PickKind::Files) {
                    ui.horizontal(|ui| {
                        let file_count = self.entries.iter().filter(|e| !e.is_dir).count();
                        ui.label(format!(
                            "{} selected ({} files in folder)",
                            self.selected_files.len(),
                            file_count
                        ));
                        if file_count > 0 && ui.small_button("Select All Files").clicked() {
                            for entry in &self.entries {
                                if !entry.is_dir {
                                    self.selected_files.insert(entry.path.clone());
                                }
                            }
                        }
                        if !self.selected_files.is_empty() && ui.small_button("Clear Selection").clicked() {
                            self.selected_files.clear();
                        }
                    });
                }

                ui.separator();

                // Directory listing scroll area
                let mut navigate_next: Option<PathBuf> = None;
                let mut immediate_pick: Option<Vec<PathBuf>> = None;

                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.entries.is_empty() {
                            ui.weak("No matching files or folders in this directory.");
                        }
                        for entry in &self.entries {
                            let is_selected = self.selected_files.contains(&entry.path);
                            let label = if entry.is_dir {
                                format!("📁  {}", entry.name)
                            } else {
                                let kb = (entry.size_bytes.saturating_add(1023)) / 1024;
                                format!("🖼  {}  ({kb} KB)", entry.name)
                            };
                            let resp = ui.selectable_label(is_selected, label);
                            if resp.clicked() {
                                if entry.is_dir {
                                    navigate_next = Some(entry.path.clone());
                                } else {
                                    match self.kind {
                                        PickKind::Files => {
                                            if is_selected {
                                                self.selected_files.remove(&entry.path);
                                            } else {
                                                self.selected_files.insert(entry.path.clone());
                                            }
                                        }
                                        PickKind::File => {
                                            self.selected_files.clear();
                                            self.selected_files.insert(entry.path.clone());
                                            self.filename_input.clone_from(&entry.name);
                                        }
                                        PickKind::Save => {
                                            self.filename_input.clone_from(&entry.name);
                                        }
                                        PickKind::Folder => {}
                                    }
                                }
                            }
                            if resp.double_clicked() && !entry.is_dir {
                                match self.kind {
                                    PickKind::Files | PickKind::File | PickKind::Save => {
                                        immediate_pick = Some(vec![entry.path.clone()]);
                                    }
                                    PickKind::Folder => {}
                                }
                            }
                        }
                    });

                if let Some(next_dir) = navigate_next {
                    self.navigate_to(next_dir);
                }
                if let Some(paths) = immediate_pick {
                    outcome = self.finish_with_paths(paths);
                }

                ui.separator();

                // Bottom action bar depending on PickKind
                match self.kind {
                    PickKind::Save => {
                        ui.horizontal(|ui| {
                            ui.label("File name:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.filename_input)
                                    .desired_width(300.0),
                            );
                            if ui.button("Save Here").clicked() {
                                let name = self.resolved_save_filename();
                                let target = dirs.resolve_writable_target(&self.current_dir.join(name));
                                outcome = self.finish_with_paths(vec![target]);
                            }
                            if ui.button("Cancel").clicked() {
                                outcome = self.cancel();
                            }
                        });
                    }
                    PickKind::Folder => {
                        ui.horizontal(|ui| {
                            if ui.button("Use Current Folder").clicked() {
                                outcome = self.finish_with_paths(vec![self.current_dir.clone()]);
                            }
                            if ui.button("Cancel").clicked() {
                                outcome = self.cancel();
                            }
                        });
                    }
                    PickKind::Files => {
                        ui.horizontal(|ui| {
                            let count = self.selected_files.len();
                            let btn_label = if count > 0 {
                                format!("Open Selected ({count})")
                            } else {
                                "Import Current Folder".to_string()
                            };
                            if ui.button(btn_label).clicked() {
                                let paths: Vec<PathBuf> = if self.selected_files.is_empty() {
                                    vec![self.current_dir.clone()]
                                } else {
                                    self.selected_files.iter().cloned().collect()
                                };
                                outcome = self.finish_with_paths(paths);
                            }
                            if ui.button("Cancel").clicked() {
                                outcome = self.cancel();
                            }
                        });
                    }
                    PickKind::File => {
                        ui.horizontal(|ui| {
                            let can_open = !self.selected_files.is_empty()
                                || !self.filename_input.trim().is_empty();
                            if ui
                                .add_enabled(can_open, egui::Button::new("Open"))
                                .clicked()
                            {
                                let chosen = self
                                    .selected_files
                                    .iter()
                                    .next()
                                    .cloned()
                                    .unwrap_or_else(|| self.current_dir.join(self.filename_input.trim()));
                                if chosen.is_file() {
                                    outcome = self.finish_with_paths(vec![chosen]);
                                } else {
                                    self.status_error = Some(format!(
                                        "File not found: {}",
                                        chosen.display()
                                    ));
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                outcome = self.cancel();
                            }
                        });
                    }
                }
            });

        outcome
    }

    fn resolved_save_filename(&self) -> String {
        let base = sanitize_filename(self.filename_input.trim());
        if let Some(default_ext) = self.filter_exts.first() {
            let has_matching_ext = Path::new(&base)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    self.filter_exts
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(ext))
                });
            if !has_matching_ext {
                return format!("{base}.{default_ext}");
            }
        }
        base
    }
}

/// Check whether `path` has an extension in `allowed_exts`.
#[must_use]
pub fn matches_exts(path: &Path, allowed_exts: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| allowed_exts.iter().any(|a| a.eq_ignore_ascii_case(ext)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter_matches_photos_raws_and_presets() {
        let exts: Vec<String> = PHOTO_AND_RAW_EXTENSIONS
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();
        assert!(matches_exts(Path::new("/sdcard/DCIM/IMG_0001.DNG"), &exts));
        assert!(matches_exts(Path::new("/sdcard/DCIM/DSC01234.ARW"), &exts));
        assert!(matches_exts(Path::new("/sdcard/DCIM/IMG_0002.CR3"), &exts));
        assert!(matches_exts(Path::new("/sdcard/DCIM/IMG_0003.NEF"), &exts));
        assert!(matches_exts(Path::new("/sdcard/Pictures/edit.avif"), &exts));
        assert!(matches_exts(Path::new("/sdcard/Pictures/edit.jxl"), &exts));
        assert!(!matches_exts(Path::new("/sdcard/Download/archive.apk"), &exts));
    }

    #[test]
    fn dialog_lists_matching_files_and_answers_pick_request_channel() {
        let tmp = std::env::temp_dir().join(format!("lc_android_dlg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dirs = AndroidDirs::resolve(Some(tmp.join("internal")), Some(tmp.join("external")));
        let exports = dirs.exports_dir();

        let dng_path = exports.join("sample.dng");
        let jpg_path = exports.join("sample.jpg");
        let sub_dir = exports.join("Subfolder");
        std::fs::write(&dng_path, b"dng").unwrap();
        std::fs::write(&jpg_path, b"jpg").unwrap();
        std::fs::write(exports.join("notes.apk"), b"apk").unwrap();
        std::fs::create_dir_all(&sub_dir).unwrap();

        let mut dlg = AndroidFileDialog::new(&dirs);
        let (tx, rx) = std::sync::mpsc::channel();
        let req = PickRequest::files(
            Some("Add Photos".to_string()),
            "Photos",
            PHOTO_AND_RAW_EXTENSIONS,
        );
        dlg.open_for_request(req, tx, &dirs);
        dlg.navigate_to(exports.clone());

        // Should list Subfolder + sample.dng + sample.jpg (3 entries), excluding notes.apk
        assert_eq!(dlg.entries.len(), 3);
        assert!(dlg.entries[0].is_dir);
        assert_eq!(dlg.entries[0].name, "Subfolder");

        // Completing the pick sends the selected paths over `rx`
        let outcome = dlg.finish_with_paths(vec![dng_path.clone(), jpg_path.clone()]);
        assert_eq!(outcome, DialogOutcome::None);
        let received = rx.try_recv().unwrap();
        assert_eq!(received.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
