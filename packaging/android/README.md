# LightCraft for Android (`packaging/android`)

This directory contains the Android Gradle project (`ai.storyteller.lightcraft`) that packages
the Rust `lightcraft-android` crate (`liblightcraft_android.so`) into a hardware-accelerated
Android APK (`arm64-v8a`, Android 8.0 Oreo API 26+).

## Key Capabilities

1. **Dual Hardware GPU Acceleration (`wgpu` Vulkan 1.1+ & OpenGL ES 3.0+)**:
   - **WGSL Compute Photo Develop Pipeline (`lightcraft-gpu`)**: Runs LightCraft's linear-light
     compute shaders (`finish.wgsl`, `blur.wgsl`, `resize.wgsl`, `map.wgsl`, `mask.wgsl`,
     `geom.wgsl`) and GPU AI denoise kernels on Android's Vulkan compute queue (`Backends::VULKAN`),
     protected by `gpu-init.marker` crash-sentinel recovery.
   - **UI & Loupe Surface (`eframe` / `egui-wgpu`)**: Renders the UI, loupe, compare views,
     masks, and filmstrip on Android's native `ANativeWindow` surface using **Vulkan 1.1+**
     (`Backends::VULKAN`) with automatic fallback to **OpenGL ES 3.0+** (`Backends::GL`),
     `PresentMode::AutoVsync`, unlocked `max_buffer_size` and `max_texture_dimension_2d`, and
     a startup crash sentinel (`gpu-starting.json`).
2. **Android File Access Permissions & Storage Access Framework (SAF)**:
   - Declares and requests runtime storage permissions across Android 8–15+:
     - `READ_EXTERNAL_STORAGE` & `WRITE_EXTERNAL_STORAGE` (Android 8–10, plus `requestLegacyExternalStorage="true"`).
     - `READ_MEDIA_IMAGES` (Android 13+ for reading photos and RAW files).
     - `MANAGE_EXTERNAL_STORAGE` (`Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` on Android 11+
       for browsing and exporting RAWs, JPEGs, PNGs, TIFFs, DNGs, AVIFs, JXLs, and presets anywhere on `/storage/emulated/0`).
   - Built-in touch-friendly **Android File & Folder Browser** modal wired into `Services.picker`
     and `Services.local_places` with bookmarks (`Camera`, `DCIM`, `Pictures`, `Downloads`,
     `Documents`, `Shared Storage`, `App Exports`, `SAF Imports`, `Library`), multi-select, inline
     `+ New Folder`, and one-tap **System Picker (SAF)** (`ACTION_OPEN_DOCUMENT` with multi-select
     and `ACTION_CREATE_DOCUMENT`).
   - Notifies `MediaScannerConnection` on every exported file so exports appear immediately in
     Android Gallery and Files.
   - Handles incoming `ACTION_VIEW`, `ACTION_EDIT`, `ACTION_SEND`, and `ACTION_SEND_MULTIPLE`
     intents for photos, RAW files (`.dng`, `.cr2`, `.cr3`, `.nef`, `.arw`, `.raf`, `.orf`, `.rw2`,
     `.pef`, `.heic`, `.avif`, `.jxl`), and presets (`.lcpreset`, `.xmp`, `.cube`).

## Building

Run from the repository root:

```bash
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
bash packaging/android/package.sh
```

The resulting APK is written to `dist/release/lightcraft-0.15.0-android-arm64-v8a.apk`.
