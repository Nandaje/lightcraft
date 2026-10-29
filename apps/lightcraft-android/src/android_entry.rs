//! Android `NativeActivity` entry point (`android_main`) and JNI bridge to
//! `ai.storyteller.lightcraft.MainActivity`.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use android_activity::AndroidApp;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::sys::jboolean;
use jni::{EnvUnowned, JavaVM, jni_sig, jni_str};
use lightcraft_engine::Session;
use lightcraft_ui_egui::LightcraftApp;

use crate::gpu_startup::{AndroidGpuStartup, configure_compute_gpu};
use crate::permissions::{AndroidDirs, set_permission_state};
use crate::services::{
    DialogBridge, build_android_services, load_android_prefs, stage_saf_bytes,
};
use crate::shell::AndroidShell;

struct AndroidRuntimeState {
    app: AndroidApp,
    dirs: AndroidDirs,
    bridge: Arc<Mutex<DialogBridge>>,
}

static RUNTIME: OnceLock<AndroidRuntimeState> = OnceLock::new();

/// Entry point invoked by `android-activity` when `liblightcraft_android.so` starts inside `MainActivity`.
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    // Enable stylus / S-Pen pressure and tilt axes for brush masks and spot healing.
    app.enable_motion_axis(android_activity::input::Axis::Pressure);
    app.enable_motion_axis(android_activity::input::Axis::Tilt);
    app.enable_motion_axis(android_activity::input::Axis::Orientation);
    app.enable_motion_axis(android_activity::input::Axis::Distance);

    let internal = app.internal_data_path();
    let external = app.external_data_path();
    let dirs = AndroidDirs::resolve(internal, external);
    let config_dir = dirs.config_dir();
    let default_lib_dir = dirs.default_library_dir();

    // Safe during single-threaded process startup before worker threads spawn.
    unsafe {
        std::env::set_var("LIGHTCRAFT_CONFIG_DIR", &config_dir);
        std::env::set_var("LIGHTCRAFT_LIBRARY", &default_lib_dir);
        if let Some(ext) = &dirs.external_files_dir {
            std::env::set_var("LIGHTCRAFT_EXTERNAL_FILES_DIR", ext);
            std::env::set_var("HOME", ext);
        } else {
            std::env::set_var("HOME", &dirs.internal_files_dir);
        }
    }

    let bridge = DialogBridge::shared();
    let _ = RUNTIME.set(AndroidRuntimeState {
        app: app.clone(),
        dirs: dirs.clone(),
        bridge: Arc::clone(&bridge),
    });

    // Request runtime storage permissions on startup if not yet granted.
    request_storage_permissions_jni();

    // Load saved preferences (`ui.json`) before configuring GPU compute.
    let saved_prefs = load_android_prefs(&dirs);
    if let Some(ui) = &saved_prefs {
        lightcraft_ui_egui::i18n::set_language(ui.language);
    }

    // Configure `lightcraft-gpu` (Vulkan WGSL compute pipeline + crash sentinel `gpu-init.marker`).
    let prefs_gpu_on = saved_prefs.as_ref().is_none_or(|u| u.settings.gpu);
    let compute_gpu_notice = configure_compute_gpu(&config_dir, prefs_gpu_on);

    // Prepare `eframe`/`egui-wgpu` hardware GPU surface tier + crash sentinel `gpu-starting.json`.
    let gpu_startup = AndroidGpuStartup::prepare(&config_dir);
    let wgpu_options = gpu_startup.build_wgpu_configuration();

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("LightCraft")
            .with_app_id("ai.storyteller.lightcraft")
            .with_drag_and_drop(true),
        android_app: Some(app),
        ..Default::default()
    };

    let run_dirs = dirs;
    let run_bridge = bridge;
    let run_startup = gpu_startup;

    let result = eframe::run_native(
        "LightCraft",
        native_options,
        Box::new(move |_cc| {
            let lib_dir = saved_prefs
                .as_ref()
                .map(|u| u.settings.library_path.trim())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| run_dirs.default_library_dir());

            let mut session = Session::new()
                .with_fs()
                .with_default_denoise_models()
                .with_system_clock();
            if let Err(e) = session.open_library(&lib_dir, true) {
                log::warn!(
                    "Could not open library at {}: {e}; falling back to default library dir",
                    lib_dir.display()
                );
                let _ = session.open_library(&run_dirs.default_library_dir(), true);
            }
            session.face_models_dir = lightcraft_engine::config::default_face_models_dir();
            session.slide_templates_dir = lightcraft_engine::config::config_dir()
                .map(|d| d.join(lightcraft_engine::slide_template::DIR_NAME));
            session = session.with_default_sam3_model();

            let services = build_android_services(&run_dirs, Arc::clone(&run_bridge), None);
            let mut lc_app = LightcraftApp::new(session, services);
            if let Some(ui) = saved_prefs {
                lc_app.ui = ui;
            }
            if let Some(notice) = compute_gpu_notice {
                lc_app.ui.settings.gpu = false;
                lc_app.notices.push(notice);
            }
            lightcraft_ui_egui::i18n::set_language(lc_app.ui.language);
            lc_app.gpu_notice = true;
            lightcraft_ui_egui::panels::empty_library::open_on_empty_library(&mut lc_app);

            let shell = AndroidShell::new(lc_app, run_dirs, run_bridge, run_startup);
            Ok(Box::new(shell))
        }),
    );

    if let Err(err) = result {
        log::error!("eframe::run_native exited with error on Android: {err}");
    }
}

/// Invoke a method on the Java `MainActivity` on the Android UI thread via `AndroidApp`.
fn with_main_activity(
    f: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<()> + Send + 'static,
) {
    let Some(rt) = RUNTIME.get() else {
        return;
    };
    let app_clone = rt.app.clone();
    rt.app.run_on_java_main_thread(Box::new(move || {
        let vm_ptr: *mut jni::sys::JavaVM = app_clone.vm_as_ptr().cast();
        if vm_ptr.is_null() {
            return;
        }
        // SAFETY: `AndroidApp::vm_as_ptr` returns the process-wide `JavaVM*` provided by the Android NDK runtime.
        let vm = unsafe { JavaVM::from_raw(vm_ptr) };
        let act_raw = app_clone.activity_as_ptr() as jni::sys::jobject;
        if act_raw.is_null() {
            return;
        }
        if let Err(err) = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // SAFETY: `act_raw` is a valid JNI object reference to the running `Activity`.
            let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&act_raw)? };
            f(env, &activity)
        }) {
            log::warn!("lightcraft-android: JNI call to MainActivity failed: {err:?}");
        }
    }));
}

/// Ask `MainActivity` to request runtime `READ_EXTERNAL_STORAGE` / `WRITE_EXTERNAL_STORAGE` / `READ_MEDIA_IMAGES`.
pub fn request_storage_permissions_jni() {
    with_main_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("requestStoragePermissions"),
            jni_sig!(() -> ()),
            &[],
        )?;
        Ok(())
    });
}

/// Ask `MainActivity` to open Android 11+ `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`.
pub fn request_all_files_access_jni() {
    with_main_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("requestAllFilesAccess"),
            jni_sig!(() -> ()),
            &[],
        )?;
        Ok(())
    });
}

/// Ask `MainActivity` to open the Android system SAF file picker (`ACTION_OPEN_DOCUMENT`).
pub fn launch_saf_open_picker_jni(multiple: bool) {
    with_main_activity(move |env, activity| {
        env.call_method(
            activity,
            jni_str!("launchSafOpenPicker"),
            jni_sig!((multiple: jboolean) -> ()),
            &[JValue::Bool(multiple)],
        )?;
        Ok(())
    });
}

/// Ask `MainActivity` to open the Android system SAF save-as picker (`ACTION_CREATE_DOCUMENT`).
pub fn launch_saf_create_picker_jni(suggested_name: &str) {
    let name = suggested_name.to_string();
    with_main_activity(move |env, activity| {
        let jname = env.new_string(&name)?;
        env.call_method(
            activity,
            jni_str!("launchSafCreatePicker"),
            jni_sig!((suggested: JString) -> ()),
            &[JValue::Object(&jname)],
        )?;
        Ok(())
    });
}

/// Notify Android's `MediaScannerConnection` so a newly exported file appears in Gallery/Files.
pub fn scan_media_file_jni(path: &str) {
    let target = path.to_string();
    with_main_activity(move |env, activity| {
        let jpath = env.new_string(&target)?;
        env.call_method(
            activity,
            jni_str!("scanMediaFile"),
            jni_sig!((path: JString) -> ()),
            &[JValue::Object(&jpath)],
        )?;
        Ok(())
    });
}

/// Open an external `https://` URL via `MainActivity.openExternalUrl`.
pub fn open_external_url_jni(url: &str) -> Result<(), String> {
    let target = url.to_string();
    with_main_activity(move |env, activity| {
        let jurl = env.new_string(&target)?;
        env.call_method(
            activity,
            jni_str!("openExternalUrl"),
            jni_sig!((url: JString) -> ()),
            &[JValue::Object(&jurl)],
        )?;
        Ok(())
    });
    Ok(())
}

// ============================================================================
// JNI Callbacks from `ai.storyteller.lightcraft.MainActivity`
// ============================================================================

/// Called by `MainActivity` when storage permission state changes.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_lightcraft_MainActivity_onPermissionsResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    read_granted: jboolean,
    write_granted: jboolean,
    all_files_granted: jboolean,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        set_permission_state(read_granted, write_granted, all_files_granted);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// Called by `MainActivity` when a file is picked via SAF (`ACTION_OPEN_DOCUMENT`) or received
/// via `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` content URI.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_lightcraft_MainActivity_onFilePickedBytes<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    display_name: JString<'caller>,
    file_bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        let Some(rt) = RUNTIME.get() else {
            return Ok(());
        };
        let name = display_name.to_string();
        let data = env.convert_byte_array(&file_bytes)?;
        if let Ok(staged_path) = stage_saf_bytes(&rt.dirs, &name, &data) {
            if let Ok(mut b) = rt.bridge.lock() {
                b.external_files.push_back(staged_path);
            }
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// Called by `MainActivity` when a direct filesystem path (`file://`) is opened or picked.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_lightcraft_MainActivity_onFilePickedPath<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    file_path: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        let Some(rt) = RUNTIME.get() else {
            return Ok(());
        };
        let path_str = file_path.to_string();
        if !path_str.trim().is_empty() {
            if let Ok(mut b) = rt.bridge.lock() {
                b.external_files.push_back(PathBuf::from(path_str));
            }
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// Called by `MainActivity` when `ACTION_CREATE_DOCUMENT` finishes: returns the latest saved bytes
/// if a file was already written, or stages a target path for the pending `PickKind::Save` request.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_lightcraft_MainActivity_onSafCreateResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    display_name: JString<'caller>,
) -> JByteArray<'caller> {
    let outcome = unowned_env.with_env(|env| -> Result<JByteArray<'caller>, jni::errors::Error> {
        let Some(rt) = RUNTIME.get() else {
            return env.byte_array_from_slice(&[]);
        };
        let name = display_name.to_string();
        let target = rt
            .dirs
            .exports_dir()
            .join(crate::permissions::sanitize_filename(&name));
        if let Ok(mut b) = rt.bridge.lock() {
            b.external_files.push_back(target);
            if let Some(last) = &b.last_saved_path {
                if let Ok(bytes) = std::fs::read(last) {
                    return env.byte_array_from_slice(&bytes);
                }
            }
        }
        env.byte_array_from_slice(&[])
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>()
}
