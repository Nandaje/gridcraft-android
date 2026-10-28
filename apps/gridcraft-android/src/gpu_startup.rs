//! Hardware GPU acceleration setup and crash-recovery sentinel for Android.
//!
//! GridCraft on Android uses `eframe`'s `wgpu` renderer backed by:
//! 1. **Vulkan 1.1+** (`Backends::VULKAN`) — primary low-overhead hardware path on Android 8.0+
//!    (Adreno, Mali, Immortalis, Xclipse, PowerVR).
//! 2. **OpenGL ES 3.0+** (`Backends::GL`) — automatic fallback for older drivers or devices with
//!    broken Vulkan swapchains.
//!
//! If the previous launch crashed inside `NativeWindow` / `vkCreateSwapchainKHR` / `eglCreateWindowSurface`,
//! a startup sentinel file (`gpu-starting.json`) detects the incomplete boot and switches to the
//! OpenGL ES compatibility tier on the next launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui_wgpu;
use eframe::wgpu;

/// Hardware acceleration tier selected for the Android rendering pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AndroidGpuTier {
    /// Vulkan 1.1+ primary with OpenGL ES 3.0+ fallback (`Backends::VULKAN | Backends::GL`).
    /// Prefers high-performance hardware adapters and unlocks hardware texture/buffer limits.
    VulkanAndGlesHardware,
    /// OpenGL ES 3.0+ compatibility mode (`Backends::GL`), used when `GRIDCRAFT_GPU_BACKEND=gles`
    /// is set or when the crash sentinel detects an interrupted Vulkan startup.
    GlesCompatibility,
    /// Software / fallback adapter mode (`Backends::GL | Backends::VULKAN` with fallback adapter).
    FallbackSafe,
}

impl AndroidGpuTier {
    /// Human-readable label for logs and diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::VulkanAndGlesHardware => "vulkan+gles-hardware",
            Self::GlesCompatibility => "gles-compatibility",
            Self::FallbackSafe => "fallback-safe",
        }
    }

    /// `wgpu::Backends` bitmask for this tier.
    pub fn backends(self) -> wgpu::Backends {
        match self {
            Self::VulkanAndGlesHardware | Self::FallbackSafe => {
                wgpu::Backends::VULKAN | wgpu::Backends::GL
            }
            Self::GlesCompatibility => wgpu::Backends::GL,
        }
    }

    /// Power preference requested from the Android GPU driver.
    pub fn power_preference(self) -> wgpu::PowerPreference {
        match self {
            Self::VulkanAndGlesHardware | Self::GlesCompatibility => {
                wgpu::PowerPreference::HighPerformance
            }
            Self::FallbackSafe => wgpu::PowerPreference::LowPower,
        }
    }
}

const SENTINEL_FILE: &str = "gpu-starting.json";

/// Path to the GPU startup sentinel inside the app's files directory.
pub fn sentinel_path(profile_dir: &Path) -> PathBuf {
    profile_dir.join(SENTINEL_FILE)
}

/// Inspect environment overrides and the startup crash sentinel to select the GPU tier,
/// then write a fresh sentinel before surface creation.
pub fn begin_gpu_startup(profile_dir: &Path) -> AndroidGpuTier {
    let env_override = std::env::var("GRIDCRAFT_GPU_BACKEND")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase());
    let sentinel = sentinel_path(profile_dir);
    let previous_crashed = sentinel.is_file();

    let tier = match env_override.as_deref() {
        Some("vulkan") | Some("hw") | Some("hardware") => AndroidGpuTier::VulkanAndGlesHardware,
        Some("gles") | Some("gl") | Some("opengles") => AndroidGpuTier::GlesCompatibility,
        Some("safe") | Some("software") => AndroidGpuTier::FallbackSafe,
        _ if previous_crashed => {
            log::warn!(
                "Previous Android GPU startup sentinel found at {}; switching to OpenGL ES compatibility tier",
                sentinel.display()
            );
            AndroidGpuTier::GlesCompatibility
        }
        _ => AndroidGpuTier::VulkanAndGlesHardware,
    };

    if let Err(err) = fs::create_dir_all(profile_dir) {
        log::warn!(
            "Failed to create profile dir {}: {err}",
            profile_dir.display()
        );
    }
    let payload = serde_json::json!({
        "tier": tier.label(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Err(err) = fs::write(&sentinel, payload.to_string()) {
        log::warn!(
            "Failed to write GPU sentinel {}: {err}",
            sentinel.display()
        );
    }

    tier
}

/// Clear the GPU startup sentinel once the first frame has rendered cleanly.
pub fn mark_gpu_startup_healthy(profile_dir: &Path) {
    let sentinel = sentinel_path(profile_dir);
    if sentinel.is_file()
        && let Err(err) = fs::remove_file(&sentinel)
    {
        log::warn!(
            "Failed to remove GPU sentinel {}: {err}",
            sentinel.display()
        );
    }
}

/// Build an [`egui_wgpu::WgpuConfiguration`] tuned for Android hardware GPU acceleration.
///
/// Key optimizations for smooth 60/120 Hz spreadsheet grid & chart rendering on mobile SoCs:
/// - `PresentMode::AutoVsync` (`VK_PRESENT_MODE_FIFO_KHR` / `eglSwapInterval(1)`), mandatory on
///   Android SurfaceFlinger to prevent tearing and thermal throttling.
/// - `PowerPreference::HighPerformance` to prefer hardware Vulkan/GLES queues over software
///   rasterizers.
/// - Device descriptor unlocks the adapter's native `max_texture_dimension_2d` (up to 8192 px
///   for high-DPI font atlases and charts) and `max_buffer_size`.
pub fn build_wgpu_configuration(tier: AndroidGpuTier) -> egui_wgpu::WgpuConfiguration {
    let mut cfg = egui_wgpu::WgpuConfiguration::default();
    cfg.surface.present_mode = wgpu::PresentMode::AutoVsync;
    cfg.surface.desired_maximum_frame_latency = Some(2);

    let backends = tier.backends();
    let power_preference = tier.power_preference();
    let force_fallback = matches!(tier, AndroidGpuTier::FallbackSafe);

    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut cfg.wgpu_setup {
        create.instance_descriptor.backends = backends;
        create.power_preference = power_preference;
        create.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
            select_android_adapter(adapters, surface, force_fallback)
        }));
        let base_device_descriptor = create.device_descriptor.clone();
        create.device_descriptor = Arc::new(move |adapter: &wgpu::Adapter| {
            let mut desc = base_device_descriptor(adapter);
            let supported = adapter.limits();
            desc.required_limits.max_buffer_size = supported.max_buffer_size;
            desc.required_limits.max_texture_dimension_2d = supported
                .max_texture_dimension_2d
                .max(desc.required_limits.max_texture_dimension_2d);
            desc
        });
    }

    cfg
}

fn select_android_adapter(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    force_fallback: bool,
) -> Result<wgpu::Adapter, String> {
    if adapters.is_empty() {
        return Err("No wgpu GPU adapters reported on Android device".to_string());
    }

    let compatible: Vec<&wgpu::Adapter> = adapters
        .iter()
        .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
        .collect();

    let pool: Vec<&wgpu::Adapter> = if compatible.is_empty() {
        adapters.iter().collect()
    } else {
        compatible
    };

    if force_fallback
        && let Some(cpu) = pool
            .iter()
            .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
    {
        log::info!("Selected fallback GPU adapter: {:?}", cpu.get_info());
        return Ok((*cpu).clone());
    }

    let best = pool
        .iter()
        .copied()
        .max_by_key(|a| score_adapter(&a.get_info()));

    if let Some(chosen) = best {
        let info = chosen.get_info();
        log::info!(
            "Selected Android hardware GPU adapter: {} ({:?}, backend={:?}, driver={})",
            info.name,
            info.device_type,
            info.backend,
            info.driver
        );
        Ok(chosen.clone())
    } else {
        Err("No suitable Android GPU adapter found".to_string())
    }
}

fn score_adapter(info: &wgpu::AdapterInfo) -> i32 {
    let type_score = match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 400,
        wgpu::DeviceType::IntegratedGpu => 300,
        wgpu::DeviceType::VirtualGpu => 200,
        wgpu::DeviceType::Other => 100,
        wgpu::DeviceType::Cpu => 10,
    };
    let backend_score = match info.backend {
        wgpu::Backend::Vulkan => 50,
        wgpu::Backend::Gl => 30,
        _ => 0,
    };
    type_score + backend_score
}

#[cfg(test)]
mod tests {
    use super::{
        AndroidGpuTier, begin_gpu_startup, build_wgpu_configuration, mark_gpu_startup_healthy,
        score_adapter, sentinel_path,
    };
    use eframe::wgpu;

    #[test]
    fn sentinel_roundtrip_recovers_from_unfinished_startup() {
        let dir = std::env::temp_dir().join(format!("gridcraft-gpu-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // First boot: clean state -> Vulkan + GLES hardware tier.
        let tier1 = begin_gpu_startup(&dir);
        assert_eq!(tier1, AndroidGpuTier::VulkanAndGlesHardware);
        assert!(sentinel_path(&dir).is_file());

        // Second boot without clearing sentinel -> switches to GLES compatibility tier.
        let tier2 = begin_gpu_startup(&dir);
        assert_eq!(tier2, AndroidGpuTier::GlesCompatibility);

        // Mark healthy -> third boot returns to primary Vulkan + GLES hardware tier.
        mark_gpu_startup_healthy(&dir);
        assert!(!sentinel_path(&dir).is_file());
        let tier3 = begin_gpu_startup(&dir);
        assert_eq!(tier3, AndroidGpuTier::VulkanAndGlesHardware);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardware_vulkan_adapter_scores_above_gles_and_cpu() {
        let vulkan_igpu = wgpu::AdapterInfo {
            name: "Adreno (TM) 740".to_string(),
            vendor: 0x5143,
            device: 0,
            device_type: wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: "Qualcomm".to_string(),
            driver_info: "Vulkan 1.3".to_string(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 32,
            subgroup_max_size: 64,
            transient_saves_memory: Some(true),
            limit_bucket: None,
        };
        let gles_igpu = wgpu::AdapterInfo {
            backend: wgpu::Backend::Gl,
            ..vulkan_igpu.clone()
        };
        let software_cpu = wgpu::AdapterInfo {
            device_type: wgpu::DeviceType::Cpu,
            ..vulkan_igpu.clone()
        };
        assert!(score_adapter(&vulkan_igpu) > score_adapter(&gles_igpu));
        assert!(score_adapter(&gles_igpu) > score_adapter(&software_cpu));
    }

    #[test]
    fn wgpu_config_builds_for_all_tiers() {
        for tier in [
            AndroidGpuTier::VulkanAndGlesHardware,
            AndroidGpuTier::GlesCompatibility,
            AndroidGpuTier::FallbackSafe,
        ] {
            let cfg = build_wgpu_configuration(tier);
            assert_eq!(cfg.surface.present_mode, wgpu::PresentMode::AutoVsync);
        }
    }
}
