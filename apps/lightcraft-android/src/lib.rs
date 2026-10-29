//! LightCraft on Android — NativeActivity entrypoint (`android_main`), hardware GPU
//! acceleration via `wgpu` (Vulkan 1.1+ & OpenGL ES 3.0+ for both UI and WGSL compute
//! photo develop), Android file access permissions (`READ_EXTERNAL_STORAGE`,
//! `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE` + SAF),
//! and an interactive touch-friendly file/folder browser modal wired into `lightcraft-ui-egui`.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod file_dialog;
pub mod gpu_startup;
pub mod permissions;
pub mod services;
pub mod shell;
pub mod touch_twins;

#[cfg(target_os = "android")]
mod android_entry;

#[cfg(target_os = "android")]
pub use android_entry::android_main;
