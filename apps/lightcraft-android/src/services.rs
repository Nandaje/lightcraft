//! Android implementation of [`lightcraft_ui_egui::Services`], preferences persistence (`ui.json`),
//! and thread-safe bridging between the UI, background export workers, and Android JNI callbacks.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;

use lightcraft_ui_egui::Services;
use lightcraft_ui_egui::pick::{PickKind, PickRequest};
use lightcraft_ui_egui::state::UiState;

use crate::file_dialog::{PHOTO_AND_RAW_EXTENSIONS, SyncPickTarget};
use crate::permissions::{AndroidDirs, sanitize_filename};

/// Requests queued from `Services` callbacks to be shown by `AndroidShell`'s modal `AndroidFileDialog`.
pub enum PendingDialogRequest {
    /// Asynchronous `PickRequest` from `Services.picker` (`lightcraft_ui_egui::pick::ask`).
    AsyncPick {
        request: PickRequest,
        reply_tx: Sender<Vec<String>>,
    },
    /// Synchronous fallback pick triggered by a direct button callback.
    SyncPick {
        target: SyncPickTarget,
        kind: PickKind,
        title: String,
        exts: &'static [&'static str],
    },
    /// Open the Android File Browser at a specific directory (`Show in File Manager`).
    RevealDir(PathBuf),
}

/// Shared state between `Services` closures, `AndroidShell`, and JNI callbacks.
#[derive(Default)]
pub struct DialogBridge {
    /// Queued modal requests from `Services`.
    pub pending_requests: VecDeque<PendingDialogRequest>,
    /// Files delivered asynchronously from Android intents (`ACTION_VIEW`, `ACTION_EDIT`, `ACTION_SEND`)
    /// or the system SAF picker (`ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`).
    pub external_files: VecDeque<PathBuf>,
    /// Last file written by `write` / `write_new` / `write_shared` (used if the user triggers SAF Save-As).
    pub last_saved_path: Option<PathBuf>,
}

impl DialogBridge {
    /// Create a new shared bridge handle.
    #[must_use]
    pub fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }
}

/// Build [`lightcraft_ui_egui::Services`] wired to Android storage permissions, the in-app
/// file/folder browser, SAF, and `MediaScannerConnection`.
#[must_use]
pub fn build_android_services(
    dirs: &AndroidDirs,
    bridge: Arc<Mutex<DialogBridge>>,
    log_file: Option<String>,
) -> Services {
    let picker_bridge = Arc::clone(&bridge);
    let pick_files_bridge = Arc::clone(&bridge);
    let pick_folder_bridge = Arc::clone(&bridge);
    let pick_presets_bridge = Arc::clone(&bridge);
    let pick_model_bridge = Arc::clone(&bridge);
    let pick_profile_bridge = Arc::clone(&bridge);
    let reveal_bridge = Arc::clone(&bridge);

    let write_dirs = dirs.clone();
    let write_bridge = Arc::clone(&bridge);

    let write_new_dirs = dirs.clone();
    let write_new_bridge = Arc::clone(&bridge);

    let write_shared_dirs = dirs.clone();
    let write_shared_bridge = Arc::clone(&bridge);

    Services {
        local_places: Some(dirs.local_places()),
        log_file,
        picker: Some(Box::new(move |request: PickRequest| {
            let (tx, rx) = std::sync::mpsc::channel();
            if let Ok(mut b) = picker_bridge.lock() {
                b.pending_requests
                    .push_back(PendingDialogRequest::AsyncPick { request, reply_tx: tx });
                Ok(rx)
            } else {
                Err("Android file dialog bridge lock poisoned".to_string())
            }
        })),
        pick_files: Some(Box::new(move || {
            if let Ok(mut b) = pick_files_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::SyncPick {
                    target: SyncPickTarget::ImportPhotos,
                    kind: PickKind::Files,
                    title: "Add Photos".to_string(),
                    exts: PHOTO_AND_RAW_EXTENSIONS,
                });
            }
            Vec::new()
        })),
        pick_folder: Some(Box::new(move || {
            if let Ok(mut b) = pick_folder_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::SyncPick {
                    target: SyncPickTarget::ExportFolder,
                    kind: PickKind::Folder,
                    title: "Select Folder".to_string(),
                    exts: &[],
                });
            }
            None
        })),
        pick_preset_files: Some(Box::new(move || {
            if let Ok(mut b) = pick_presets_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::SyncPick {
                    target: SyncPickTarget::ImportPresets,
                    kind: PickKind::Files,
                    title: "Import Presets & Profiles".to_string(),
                    exts: lightcraft_ui_egui::pick::PRESET_EXTENSIONS,
                });
            }
            Vec::new()
        })),
        pick_model_file: Some(Box::new(move || {
            if let Ok(mut b) = pick_model_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::SyncPick {
                    target: SyncPickTarget::FaceModel,
                    kind: PickKind::File,
                    title: "Select Face Recognition Model (.onnx)".to_string(),
                    exts: &["onnx"],
                });
            }
            Vec::new()
        })),
        pick_display_profile: Some(Box::new(move || {
            if let Ok(mut b) = pick_profile_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::SyncPick {
                    target: SyncPickTarget::DisplayProfile,
                    kind: PickKind::File,
                    title: "Select Display ICC Profile".to_string(),
                    exts: &["icc", "icm"],
                });
            }
            Vec::new()
        })),
        write: Some(Box::new(move |path_str, bytes| {
            let requested = Path::new(path_str);
            let resolved = write_dirs.resolve_writable_target(requested);
            let resolved_str = resolved.to_string_lossy();
            lightcraft_engine::export::write_file(&resolved_str, bytes)?;
            if let Ok(mut b) = write_bridge.lock() {
                b.last_saved_path = Some(resolved.clone());
            }
            notify_media_scanner(&resolved);
            Ok(())
        })),
        write_new: Some(Box::new(move |path_str, bytes| {
            let requested = Path::new(path_str);
            let resolved = write_new_dirs.resolve_writable_target(requested);
            let resolved_str = resolved.to_string_lossy();
            lightcraft_engine::export::write_file_new(&resolved_str, bytes)?;
            if let Ok(mut b) = write_new_bridge.lock() {
                b.last_saved_path = Some(resolved.clone());
            }
            notify_media_scanner(&resolved);
            Ok(())
        })),
        write_shared: Some(Arc::new(move |path_str, bytes| {
            let requested = Path::new(path_str);
            let resolved = write_shared_dirs.resolve_writable_target(requested);
            let resolved_str = resolved.to_string_lossy();
            lightcraft_engine::export::write_file(&resolved_str, bytes)?;
            if let Ok(mut b) = write_shared_bridge.lock() {
                b.last_saved_path = Some(resolved.clone());
            }
            notify_media_scanner(&resolved);
            Ok(())
        })),
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(
                &lightcraft_codecs::EncodeImage::rgba8(img),
                &lightcraft_codecs::EncodeMeta::default(),
            )
            .unwrap_or_default()
        })),
        open_url: Some(Box::new(|url| {
            validate_external_url(url)?;
            #[cfg(target_os = "android")]
            crate::android_entry::open_external_url_jni(url)?;
            Ok(())
        })),
        reveal: Some(Box::new(move |path_str| {
            let p = Path::new(path_str);
            let dir = if p.is_dir() {
                p.to_path_buf()
            } else {
                p.parent().unwrap_or(p).to_path_buf()
            };
            if let Ok(mut b) = reveal_bridge.lock() {
                b.pending_requests.push_back(PendingDialogRequest::RevealDir(dir));
            }
            Ok(())
        })),
        ..Services::default()
    }
}

/// Notify Android's `MediaScannerConnection` so exported photos/RAWs/PDFs appear immediately
/// in Android Gallery and Files apps.
fn notify_media_scanner(path: &Path) {
    #[cfg(target_os = "android")]
    crate::android_entry::scan_media_file_jni(&path.to_string_lossy());
    #[cfg(not(target_os = "android"))]
    let _ = path;
}

/// Validate that an external URL uses `https://` before handing it to Android's `Intent.ACTION_VIEW`.
pub fn validate_external_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("https://") {
        Ok(())
    } else {
        Err(format!("Refusing non-HTTPS external URL: {trimmed}"))
    }
}

/// Stage bytes received from an Android `content://` URI into [`AndroidDirs::saf_staging_dir`]
/// and return the staged local file path.
pub fn stage_saf_bytes(
    dirs: &AndroidDirs,
    display_name: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let clean_name = sanitize_filename(display_name);
    let target = dirs.saf_staging_dir().join(clean_name);
    std::fs::write(&target, bytes)
        .map_err(|e| format!("Failed to stage SAF file {}: {e}", target.display()))?;
    Ok(target)
}

/// Path to LightCraft's persisted `ui.json` preferences file on Android.
#[must_use]
pub fn android_prefs_path(dirs: &AndroidDirs) -> PathBuf {
    dirs.config_dir().join("ui.json")
}

/// Load `UiState` from `ui.json` in `dirs.config_dir()` if present.
#[must_use]
pub fn load_android_prefs(dirs: &AndroidDirs) -> Option<UiState> {
    let path = android_prefs_path(dirs);
    let bytes = std::fs::read(&path).ok()?;
    let mut ui: UiState = serde_json::from_slice(&bytes).ok()?;
    for id in std::mem::take(&mut ui.collapsed_sidebar) {
        if let Some(s) = lightcraft_ui_egui::sidebar::SidebarSection::from_id(&id) {
            ui.sidebar.set_collapsed(s, true);
        }
    }
    Some(ui)
}

/// Save `UiState` atomically to `ui.json` in `dirs.config_dir()`.
pub fn save_android_prefs(dirs: &AndroidDirs, ui: &UiState) -> Result<(), String> {
    let path = android_prefs_path(dirs);
    let bytes = serde_json::to_vec_pretty(ui).map_err(|e| e.to_string())?;
    lightcraft_engine::export::write_file(&path.to_string_lossy(), &bytes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn services_picker_and_writers_round_trip() {
        let tmp = std::env::temp_dir().join(format!("lc_android_svc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dirs = AndroidDirs::resolve(Some(tmp.join("internal")), Some(tmp.join("external")));
        let bridge = DialogBridge::shared();

        let mut services = build_android_services(&dirs, Arc::clone(&bridge), None);
        assert!(services.local_places.is_some());

        // Async picker queues an AsyncPick request and returns a channel receiver.
        let req = PickRequest::folder("Choose Import Folder");
        let rx = (services.picker.as_mut().unwrap())(req).unwrap();
        {
            let mut b = bridge.lock().unwrap();
            assert_eq!(b.pending_requests.len(), 1);
            if let Some(PendingDialogRequest::AsyncPick { reply_tx, .. }) = b.pending_requests.pop_front() {
                reply_tx.send(vec!["/storage/emulated/0/DCIM/Camera".to_string()]).unwrap();
            }
        }
        assert_eq!(
            rx.try_recv().unwrap(),
            vec!["/storage/emulated/0/DCIM/Camera".to_string()]
        );

        // Writer writes atomically and records last_saved_path.
        let out_file = dirs.exports_dir().join("exported.jpg");
        (services.write.as_mut().unwrap())(&out_file.to_string_lossy(), b"jpeg-bytes").unwrap();
        assert_eq!(std::fs::read(&out_file).unwrap(), b"jpeg-bytes");
        assert_eq!(bridge.lock().unwrap().last_saved_path, Some(out_file));

        // PNG encoder produces valid PNG header.
        let img = lightcraft_raster::Rgba8::new(2, 2);
        let png_bytes = (services.png.as_ref().unwrap())(&img);
        assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

        // URL validator allows https and rejects non-https.
        assert!(validate_external_url("https://storyteller.ai").is_ok());
        assert!(validate_external_url("http://insecure.example.com").is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn saf_staging_and_prefs_persistence_work() {
        let tmp = std::env::temp_dir().join(format!("lc_android_saf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dirs = AndroidDirs::resolve(Some(tmp.join("internal")), Some(tmp.join("external")));

        let staged = stage_saf_bytes(&dirs, "RAW:Photo/01.DNG", b"raw-data").unwrap();
        assert!(staged.exists());
        assert_eq!(std::fs::read(&staged).unwrap(), b"raw-data");

        let mut ui = UiState::default();
        ui.right_width = 342.0;
        save_android_prefs(&dirs, &ui).unwrap();
        let loaded = load_android_prefs(&dirs).unwrap();
        assert_eq!(loaded.right_width, 342.0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
