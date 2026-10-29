//! Android hardware GPU acceleration configuration and startup crash-sentinel recovery
//! for both the `eframe`/`egui-wgpu` UI surface and `lightcraft-gpu`'s WGSL compute
//! photo develop pipeline.
//!
//! On Android, LightCraft uses two hardware GPU paths:
//! 1. **UI & Loupe Surface (`eframe` / `egui-wgpu`)**:
//!    - **Tier 1 (`VulkanAndGlesHardware`)**: Vulkan 1.1+ primary backend with OpenGL ES 3.0+
//!      fallback (`Backends::VULKAN | Backends::GL`), `PowerPreference::HighPerformance`,
//!      `PresentMode::AutoVsync`, and unlocked `max_buffer_size` + `max_texture_dimension_2d`
//!      for full-resolution loupe textures, histograms, and filmstrip thumbnails.
//!    - **Tier 2 (`GlesCompatibility`)**: OpenGL ES 3.0 (`Backends::GL`) with `LowPower`
//!      preference if a previous launch crashed inside the device's Vulkan surface driver
//!      before the first frame completed.
//!    - **Tier 3 (`FallbackSafe`)**: `Backends::GL | Backends::VULKAN` with conservative limits
//!      and `force_fallback_adapter: true`.
//! 2. **WGSL Compute Photo Develop Pipeline (`lightcraft-gpu`)**:
//!    - Runs LightCraft's linear-light photo develop shaders (`finish.wgsl`, `blur.wgsl`,
//!      `resize.wgsl`, `map.wgsl`, `mask.wgsl`, `geom.wgsl`) and GPU AI denoise kernels on
//!      Android's Vulkan compute queue (`Backends::VULKAN`).
//!    - Protected by `gpu-init.marker` via [`lightcraft_engine::gpu::backend::set_init_marker`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;
use serde::{Deserialize, Serialize};

/// Sentinel file written before `wgpu` surface initialization and removed once frame 1 completes.
const SENTINEL_FILE: &str = "gpu-starting.json";

/// Persisted state file recording the last known working GPU tier.
const STATE_FILE: &str = "gpu-state.json";

/// Compute GPU initialization marker file written while `lightcraft-gpu` creates its Vulkan
/// compute device and compiles WGSL develop kernels.
const COMPUTE_MARKER_FILE: &str = "gpu-init.marker";

/// Hardware GPU acceleration tier for Android's window renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AndroidGpuTier {
    /// Hardware Vulkan 1.1+ with OpenGL ES 3.0+ fallback and high-performance adapter selection.
    #[default]
    VulkanAndGlesHardware,
    /// OpenGL ES 3.0+ compatibility backend (used when a buggy mobile Vulkan driver crashed on init).
    GlesCompatibility,
    /// Safe fallback mode with conservative limits.
    FallbackSafe,
}

impl AndroidGpuTier {
    /// Next safer tier if startup crashed during driver initialization.
    #[must_use]
    pub const fn demote(self) -> Self {
        match self {
            Self::VulkanAndGlesHardware => Self::GlesCompatibility,
            Self::GlesCompatibility | Self::FallbackSafe => Self::FallbackSafe,
        }
    }

    /// Human-readable label for diagnostics and logs.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::VulkanAndGlesHardware => "Vulkan 1.1 / GLES 3.0 Hardware GPU",
            Self::GlesCompatibility => "OpenGL ES 3.0 Hardware Compatibility",
            Self::FallbackSafe => "Safe Fallback GPU",
        }
    }

    /// `wgpu::Backends` bitmask for this tier.
    #[must_use]
    pub fn backends(self) -> wgpu::Backends {
        match self {
            Self::VulkanAndGlesHardware => wgpu::Backends::VULKAN | wgpu::Backends::GL,
            Self::GlesCompatibility => wgpu::Backends::GL,
            Self::FallbackSafe => wgpu::Backends::GL | wgpu::Backends::VULKAN,
        }
    }

    /// `wgpu::PowerPreference` for this tier.
    #[must_use]
    pub const fn power_preference(self) -> wgpu::PowerPreference {
        match self {
            Self::VulkanAndGlesHardware => wgpu::PowerPreference::HighPerformance,
            Self::GlesCompatibility | Self::FallbackSafe => wgpu::PowerPreference::LowPower,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SentinelPayload {
    tier: AndroidGpuTier,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedGpuState {
    preferred_tier: AndroidGpuTier,
    demoted_reason: Option<String>,
}

/// Manages Android GPU tier selection and crash-sentinel files on disk.
#[derive(Debug, Clone)]
pub struct AndroidGpuStartup {
    config_dir: PathBuf,
    tier: AndroidGpuTier,
    demoted_notice: Option<String>,
}

impl AndroidGpuStartup {
    /// Inspect `config_dir` for an interrupted startup sentinel, demote the tier if needed,
    /// and write a fresh sentinel before `wgpu` initializes.
    #[must_use]
    pub fn prepare(config_dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(config_dir);
        let sentinel_path = config_dir.join(SENTINEL_FILE);
        let state_path = config_dir.join(STATE_FILE);

        let mut state: PersistedGpuState = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let mut demoted_notice = None;
        if let Ok(raw) = std::fs::read_to_string(&sentinel_path) {
            let crashed_tier = serde_json::from_str::<SentinelPayload>(&raw)
                .map(|p| p.tier)
                .unwrap_or(state.preferred_tier);
            let next_tier = crashed_tier.demote();
            let reason = format!(
                "Previous GPU startup did not finish on {}; switched to {}.",
                crashed_tier.label(),
                next_tier.label()
            );
            log::warn!("{reason}");
            state.preferred_tier = next_tier;
            state.demoted_reason = Some(reason.clone());
            demoted_notice = Some(reason);
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                let _ = std::fs::write(&state_path, json);
            }
        }

        // Write sentinel before touching Vulkan/GLES drivers.
        let sentinel = SentinelPayload { tier: state.preferred_tier };
        if let Ok(json) = serde_json::to_string(&sentinel) {
            let _ = std::fs::write(&sentinel_path, json);
        }

        Self {
            config_dir: config_dir.to_path_buf(),
            tier: state.preferred_tier,
            demoted_notice,
        }
    }

    /// Selected hardware GPU tier.
    #[must_use]
    pub const fn tier(&self) -> AndroidGpuTier {
        self.tier
    }

    /// Optional notice if the tier was automatically demoted after a driver crash.
    #[must_use]
    pub fn demoted_notice(&self) -> Option<&str> {
        self.demoted_notice.as_deref()
    }

    /// Clear the startup sentinel once the first frame has rendered cleanly on the GPU.
    pub fn mark_frame_healthy(&self) {
        let sentinel_path = self.config_dir.join(SENTINEL_FILE);
        if sentinel_path.exists() {
            let _ = std::fs::remove_file(&sentinel_path);
        }
    }

    /// Build the hardware-accelerated `WgpuConfiguration` for `eframe::NativeOptions`.
    #[must_use]
    pub fn build_wgpu_configuration(&self) -> WgpuConfiguration {
        let tier = self.tier;
        let force_fallback = matches!(tier, AndroidGpuTier::FallbackSafe);

        let create_new = WgpuSetupCreateNew {
            instance_descriptor: wgpu::InstanceDescriptor {
                backends: tier.backends(),
                flags: wgpu::InstanceFlags::empty(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                backend_options: wgpu::BackendOptions::default(),
                display: None,
            },
            power_preference: tier.power_preference(),
            native_adapter_selector: None,
            device_descriptor: Arc::new(move |adapter: &wgpu::Adapter| {
                let adapter_limits = adapter.limits();
                // Start from downlevel defaults for broad mobile GPU compatibility (Adreno, Mali,
                // Immortalis, PowerVR, Xclipse) while unlocking the adapter's full texture
                // dimension and buffer size for high-DPI loupe renders, masks, and filmstrips.
                let mut limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter_limits.clone());
                if !force_fallback {
                    limits.max_buffer_size = adapter_limits.max_buffer_size;
                    limits.max_texture_dimension_2d = adapter_limits.max_texture_dimension_2d.max(4096);
                }
                wgpu::DeviceDescriptor {
                    label: Some("lightcraft-android-gpu-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                }
            }),
            display_handle: None,
        };

        let mut cfg = WgpuConfiguration::default();
        cfg.surface.present_mode = wgpu::PresentMode::AutoVsync;
        cfg.surface.desired_maximum_frame_latency = Some(2);
        cfg.wgpu_setup = WgpuSetup::CreateNew(create_new);
        cfg
    }
}

/// Configure `lightcraft-gpu`'s WGSL compute develop pipeline for Android:
/// 1. Checks `gpu-init.marker` inside `config_dir` to detect if a previous launch died while
///    initializing the Vulkan compute device or compiling WGSL develop kernels.
/// 2. Registers `gpu-init.marker` via [`lightcraft_engine::gpu::backend::set_init_marker`].
/// 3. Enables or disables `lightcraft_engine::gpu::set_enabled` accordingly.
///
/// Returns `Some(notice)` if compute GPU rendering was automatically switched off after an
/// interrupted driver initialization.
pub fn configure_compute_gpu(config_dir: &Path, prefs_gpu_enabled: bool) -> Option<String> {
    let _ = std::fs::create_dir_all(config_dir);
    let marker_path = config_dir.join(COMPUTE_MARKER_FILE);
    let recovered = lightcraft_engine::gpu::backend::take_init_marker(&marker_path).map(|what| {
        log::warn!("previous launch died during compute GPU init ({what}); disabling compute GPU");
        format!(
            "LightCraft switched off GPU rendering because the previous launch stopped while starting the graphics driver ({what}). \
             You can turn GPU rendering back on in Settings ▸ Performance."
        )
    });
    lightcraft_engine::gpu::backend::set_init_marker(Some(marker_path));
    let gpu_on = prefs_gpu_enabled && recovered.is_none();
    lightcraft_engine::gpu::set_enabled(gpu_on);
    recovered
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn gpu_tier_demotes_on_interrupted_startup_and_clears_on_healthy_frame() {
        let tmp = std::env::temp_dir().join(format!("lc_android_gpu_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        // First boot selects hardware Vulkan + GLES tier and writes sentinel.
        let boot1 = AndroidGpuStartup::prepare(&tmp);
        assert_eq!(boot1.tier(), AndroidGpuTier::VulkanAndGlesHardware);
        assert!(boot1.demoted_notice().is_none());
        assert!(tmp.join(SENTINEL_FILE).exists());

        // Simulate crash before mark_frame_healthy: second boot demotes to GLES compatibility.
        let boot2 = AndroidGpuStartup::prepare(&tmp);
        assert_eq!(boot2.tier(), AndroidGpuTier::GlesCompatibility);
        assert!(boot2.demoted_notice().is_some());

        // Mark frame healthy: third boot stays on GLES compatibility without new demotion notice.
        boot2.mark_frame_healthy();
        assert!(!tmp.join(SENTINEL_FILE).exists());
        let boot3 = AndroidGpuStartup::prepare(&tmp);
        assert_eq!(boot3.tier(), AndroidGpuTier::GlesCompatibility);
        assert!(boot3.demoted_notice().is_none());
        boot3.mark_frame_healthy();

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wgpu_configuration_uses_auto_vsync_and_hardware_backends() {
        let tmp = std::env::temp_dir().join(format!("lc_android_gpu_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let startup = AndroidGpuStartup::prepare(&tmp);
        let cfg = startup.build_wgpu_configuration();
        assert_eq!(cfg.surface.present_mode, wgpu::PresentMode::AutoVsync);
        assert_eq!(cfg.surface.desired_maximum_frame_latency, Some(2));
        startup.mark_frame_healthy();

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn compute_gpu_recovers_from_leftover_init_marker() {
        let tmp = std::env::temp_dir().join(format!("lc_android_compute_gpu_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Clean boot with GPU enabled: no notice, GPU enabled.
        let notice = configure_compute_gpu(&tmp, true);
        assert!(notice.is_none());
        assert!(lightcraft_engine::gpu::enabled());

        // Simulate leftover marker from crashed Vulkan compute kernel compilation:
        let marker = tmp.join(COMPUTE_MARKER_FILE);
        std::fs::write(&marker, "GPU device and kernel creation (backends VULKAN) started\n").unwrap();
        let notice2 = configure_compute_gpu(&tmp, true);
        assert!(notice2.is_some());
        assert!(!marker.exists());
        assert!(!lightcraft_engine::gpu::enabled());

        // Restore global state for other tests.
        lightcraft_engine::gpu::backend::set_init_marker(None);
        lightcraft_engine::gpu::set_enabled(true);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
