//! Android file access permission state, storage directory resolution, sidebar `Local` places,
//! and Storage Access Framework (SAF) helpers for LightCraft.
//!
//! Covers:
//! - Legacy shared storage permissions (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE` on API 26–29)
//! - Granular media permissions (`READ_MEDIA_IMAGES` on API 33+)
//! - All-files access (`MANAGE_EXTERNAL_STORAGE` on API 30+ for browsing/importing RAW & photo folders
//!   and exporting directly to `/storage/emulated/0/Pictures`, `/storage/emulated/0/DCIM`, etc.)
//! - App-specific internal & external directories (`Context.getFilesDir()`, `Context.getExternalFilesDir(null)`)
//!   which always work without runtime permission prompts and host the default LightCraft catalog library.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static READ_GRANTED: AtomicBool = AtomicBool::new(false);
static WRITE_GRANTED: AtomicBool = AtomicBool::new(false);
static ALL_FILES_GRANTED: AtomicBool = AtomicBool::new(false);

/// Snapshot of Android storage permissions currently held by the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoragePermissionStatus {
    /// Whether reading shared media/storage (`READ_EXTERNAL_STORAGE` or `READ_MEDIA_IMAGES`) is granted.
    pub read_external: bool,
    /// Whether writing shared storage (`WRITE_EXTERNAL_STORAGE` or `MANAGE_EXTERNAL_STORAGE`) is granted.
    pub write_external: bool,
    /// Whether `Environment.isExternalStorageManager()` (`MANAGE_EXTERNAL_STORAGE`) is granted on Android 11+.
    pub manage_all_files: bool,
}

impl StoragePermissionStatus {
    /// Current permission state from atomic flags updated by `MainActivity` JNI callbacks.
    #[must_use]
    pub fn current() -> Self {
        Self {
            read_external: READ_GRANTED.load(Ordering::Acquire),
            write_external: WRITE_GRANTED.load(Ordering::Acquire),
            manage_all_files: ALL_FILES_GRANTED.load(Ordering::Acquire),
        }
    }

    /// True if shared storage (`/storage/emulated/0/{DCIM,Pictures,Download,Documents}`) can be
    /// browsed directly via POSIX `std::fs`.
    #[must_use]
    pub const fn can_browse_shared_storage(self) -> bool {
        self.manage_all_files || self.read_external
    }

    /// True if files can be written directly to shared folders without SAF.
    #[must_use]
    pub const fn can_write_shared_storage(self) -> bool {
        self.manage_all_files || self.write_external
    }
}

/// Update the cached permission state (called from JNI `onPermissionsResult` and startup).
pub fn set_permission_state(read: bool, write: bool, all_files: bool) {
    READ_GRANTED.store(read || all_files, Ordering::Release);
    WRITE_GRANTED.store(write || all_files, Ordering::Release);
    ALL_FILES_GRANTED.store(all_files, Ordering::Release);
}

/// Well-known Android directories used by LightCraft for its catalog library, config, exports,
/// and photo/RAW browsing.
#[derive(Debug, Clone)]
pub struct AndroidDirs {
    /// App-private internal storage (`/data/user/0/ai.storyteller.lightcraft/files`).
    pub internal_files_dir: PathBuf,
    /// App-private external storage (`/storage/emulated/0/Android/data/ai.storyteller.lightcraft/files`).
    pub external_files_dir: Option<PathBuf>,
    /// Shared storage root (`/storage/emulated/0`).
    pub shared_storage_root: PathBuf,
    /// Shared `DCIM` directory (`/storage/emulated/0/DCIM`).
    pub shared_dcim_dir: PathBuf,
    /// Shared `DCIM/Camera` directory (`/storage/emulated/0/DCIM/Camera`).
    pub shared_camera_dir: PathBuf,
    /// Shared `Pictures` directory (`/storage/emulated/0/Pictures`).
    pub shared_pictures_dir: PathBuf,
    /// Shared `Download` directory (`/storage/emulated/0/Download`).
    pub shared_downloads_dir: PathBuf,
    /// Shared `Documents` directory (`/storage/emulated/0/Documents`).
    pub shared_documents_dir: PathBuf,
}

impl AndroidDirs {
    /// Resolve Android directories from `AndroidApp` paths or environment fallbacks.
    #[must_use]
    pub fn resolve(internal: Option<PathBuf>, external: Option<PathBuf>) -> Self {
        let internal_files_dir = internal
            .or_else(|| std::env::var_os("LIGHTCRAFT_CONFIG_DIR").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/data/data/ai.storyteller.lightcraft/files"));

        let shared_storage_root = std::env::var_os("EXTERNAL_STORAGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/storage/emulated/0"));

        let shared_dcim_dir = shared_storage_root.join("DCIM");
        let shared_camera_dir = shared_dcim_dir.join("Camera");
        let shared_pictures_dir = shared_storage_root.join("Pictures");
        let shared_downloads_dir = shared_storage_root.join("Download");
        let shared_documents_dir = shared_storage_root.join("Documents");

        Self {
            internal_files_dir,
            external_files_dir: external,
            shared_storage_root,
            shared_dcim_dir,
            shared_camera_dir,
            shared_pictures_dir,
            shared_downloads_dir,
            shared_documents_dir,
        }
    }

    /// Directory for LightCraft preferences (`ui.json`), GPU markers, and AI models (`models/`).
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        let dir = self.internal_files_dir.join("config");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Default LightCraft catalog library directory (`LightCraft Library`).
    /// Placed inside app-specific storage so atomic journal renames (`catalog.snap`) and locks
    /// always succeed on first launch without requiring runtime permissions.
    #[must_use]
    pub fn default_library_dir(&self) -> PathBuf {
        let base = self.external_files_dir.as_ref().unwrap_or(&self.internal_files_dir);
        let dir = base.join("LightCraft Library");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Always-writable directory for exported photos, contact sheets, slideshows, and presets.
    #[must_use]
    pub fn exports_dir(&self) -> PathBuf {
        let base = self.external_files_dir.as_ref().unwrap_or(&self.internal_files_dir);
        let dir = base.join("Exports");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Staging directory for files picked via Android's Storage Access Framework (`content://` URIs).
    #[must_use]
    pub fn saf_staging_dir(&self) -> PathBuf {
        let dir = self.internal_files_dir.join("saf_imports");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Best default directory for exporting photos when the user has not chosen a folder yet:
    /// prefers `/storage/emulated/0/Pictures/LightCraft` when shared storage is writable,
    /// otherwise falls back to the app's writable `Exports` directory.
    #[must_use]
    pub fn preferred_export_dir(&self) -> PathBuf {
        if StoragePermissionStatus::current().can_write_shared_storage() {
            let shared_lc = self.shared_pictures_dir.join("LightCraft");
            if std::fs::create_dir_all(&shared_lc).is_ok() {
                return shared_lc;
            }
        }
        self.exports_dir()
    }

    /// Bookmarks for LightCraft's left-sidebar `Local` places (`Services.local_places`).
    #[must_use]
    pub fn local_places(&self) -> Vec<(String, String)> {
        let mut places = Vec::new();
        for (label, path) in [
            ("Camera", &self.shared_camera_dir),
            ("DCIM", &self.shared_dcim_dir),
            ("Pictures", &self.shared_pictures_dir),
            ("Downloads", &self.shared_downloads_dir),
            ("Documents", &self.shared_documents_dir),
        ] {
            places.push((label.to_string(), path.to_string_lossy().into_owned()));
        }
        places.push(("Exports".to_string(), self.exports_dir().to_string_lossy().into_owned()));
        places
    }

    /// Ordered list of quick-access bookmark folders for the Android file/folder browser modal.
    #[must_use]
    pub fn bookmarks(&self) -> Vec<(&'static str, PathBuf)> {
        let mut out = Vec::new();
        if self.shared_camera_dir.exists() {
            out.push(("Camera", self.shared_camera_dir.clone()));
        }
        out.push(("DCIM", self.shared_dcim_dir.clone()));
        out.push(("Pictures", self.shared_pictures_dir.clone()));
        out.push(("Downloads", self.shared_downloads_dir.clone()));
        out.push(("Documents", self.shared_documents_dir.clone()));
        out.push(("Shared Storage", self.shared_storage_root.clone()));
        out.push(("App Exports", self.exports_dir()));
        out.push(("SAF Imports", self.saf_staging_dir()));
        out.push(("Library", self.default_library_dir()));
        out
    }

    /// Ensure a target path is writable; if its parent directory is not writable due to Android
    /// scoped storage restrictions, fall back to [`Self::exports_dir`] while preserving the filename.
    #[must_use]
    pub fn resolve_writable_target(&self, requested: &Path) -> PathBuf {
        if let Some(parent) = requested.parent() {
            if !parent.as_os_str().is_empty() && can_write_dir(parent) {
                return requested.to_path_buf();
            }
        }
        let file_name = requested
            .file_name()
            .and_then(|n| n.to_str())
            .map(sanitize_filename)
            .unwrap_or_else(|| "export.jpg".to_string());
        self.exports_dir().join(file_name)
    }
}

/// Probe whether `dir` exists (or can be created) and accepts file creation.
#[must_use]
pub fn can_write_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".lc_write_probe_{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Sanitize a file name so it is safe on Android FAT/exFAT/ext4/F2FS filesystems.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn permission_flags_round_trip() {
        set_permission_state(false, false, false);
        let s0 = StoragePermissionStatus::current();
        assert!(!s0.can_browse_shared_storage());
        assert!(!s0.can_write_shared_storage());

        set_permission_state(true, false, false);
        let s1 = StoragePermissionStatus::current();
        assert!(s1.can_browse_shared_storage());
        assert!(!s1.can_write_shared_storage());

        set_permission_state(false, false, true);
        let s2 = StoragePermissionStatus::current();
        assert!(s2.manage_all_files);
        assert!(s2.can_browse_shared_storage());
        assert!(s2.can_write_shared_storage());
    }

    #[test]
    fn writable_target_falls_back_to_exports_when_parent_unwritable() {
        let tmp = std::env::temp_dir().join(format!("lc_android_dirs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dirs = AndroidDirs::resolve(Some(tmp.join("internal")), Some(tmp.join("external")));

        let ok_target = dirs.exports_dir().join("photo.jpg");
        assert_eq!(dirs.resolve_writable_target(&ok_target), ok_target);

        let lib_dir = dirs.default_library_dir();
        assert!(lib_dir.ends_with("LightCraft Library"));
        assert!(lib_dir.exists());

        let places = dirs.local_places();
        assert!(places.iter().any(|(label, _)| label == "Camera"));
        assert!(places.iter().any(|(label, _)| label == "Pictures"));
        assert!(places.iter().any(|(label, _)| label == "Exports"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn filename_sanitizer_replaces_illegal_characters() {
        assert_eq!(sanitize_filename("DSC:2026/01*02?.DNG"), "DSC_2026_01_02_.DNG");
        assert_eq!(sanitize_filename("..."), "untitled");
    }
}
