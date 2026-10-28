# GridCraft on Android

GridCraft's Android port (`apps/gridcraft-android` + `packaging/android`) runs the full
`gridcraft-engine` spreadsheet calculation core and `gridcraft-ui-egui` Excel-style ribbon,
formula bar, grid, pivot tables, charts, and print/PDF engine natively on `arm64-v8a` Android
devices (Android 8.0 API 26 through Android 14+ API 34+).

## Architecture Overview

| Component | Path | Responsibility |
| --- | --- | --- |
| Android Crate | `apps/gridcraft-android/` | `cdylib` (`libgridcraft_android.so`) + `rlib` implementing the `NativeActivity` entry point, JNI bridge, GPU setup, permissions, and touch file browser |
| GPU Startup | `apps/gridcraft-android/src/gpu_startup.rs` | `wgpu` hardware GPU acceleration (Vulkan 1.1+ primary, OpenGL ES 3.0+ fallback), high-performance adapter selection, unlocked texture/buffer limits, and `gpu-starting.json` crash sentinel |
| Permissions | `apps/gridcraft-android/src/permissions.rs` | Runtime Android storage permission state (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE`) and scoped-storage path resolution |
| File Dialog | `apps/gridcraft-android/src/file_dialog.rs` | Touch-friendly modal file browser & exporter (`.xlsx`, `.csv`, `.tsv`, `.html`, `.pdf`, `.json`, and `.png`/`.jpg` pictures) with Storage Access Framework (SAF) picker integration |
| Services | `apps/gridcraft-android/src/services.rs` | Android implementation of `gridcraft_ui_egui::Services`, SAF `content://` URI staging, media scanner notification, and `ui.json` / `prefs.json` persistence |
| Shell | `apps/gridcraft-android/src/shell.rs` | `eframe::App` host wrapping `SheetApp`, draining UI/intent queues, and deduplicating touch-twin events (`touch_twins.rs`) |
| Kotlin Host | `packaging/android/app/src/main/java/ai/storyteller/gridcraft/MainActivity.kt` | `NativeActivity` subclass handling Android runtime permission dialogs, `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`, SAF `ACTION_OPEN_DOCUMENT` / `ACTION_CREATE_DOCUMENT`, and `ACTION_VIEW` / `ACTION_SEND` intents |

## Hardware GPU Acceleration

On startup, `gpu_startup::begin_gpu_startup` selects an `AndroidGpuTier`:
1. **`VulkanAndGlesHardware`** (default): initializes `wgpu::Backends::VULKAN | wgpu::Backends::GL`
   with `PowerPreference::HighPerformance` and `PresentMode::AutoVsync`, scoring discrete and
   integrated Vulkan GPUs (Adreno, Mali, Immortalis, Xclipse, PowerVR) above OpenGL ES and CPU
   adapters, and unlocking the hardware adapter's full `max_buffer_size` and `max_texture_dimension_2d`.
2. **`GlesCompatibility`**: uses `wgpu::Backends::GL` (`OpenGL ES 3.0+`) when `GRIDCRAFT_GPU_BACKEND=gles`
   is set or when the startup crash sentinel (`gpu-starting.json`) detects an incomplete previous
   Vulkan surface initialization. Once the first frame renders with fonts loaded,
   `mark_gpu_startup_healthy` removes the sentinel.

## File Access & Storage Permissions

GridCraft supports three complementary file-access paths on Android:
1. **Shared Storage (`/storage/emulated/0/{Documents,Download,Pictures}`)**:
   - On Android 8–10 (API 26–29), uses `READ_EXTERNAL_STORAGE` and `WRITE_EXTERNAL_STORAGE`
     (with `requestLegacyExternalStorage="true"`).
   - On Android 11+ (API 30+), the File Browser includes a one-tap **Grant All-Files Access**
     button that opens `Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` (`MANAGE_EXTERNAL_STORAGE`).
   - On Android 13+ (API 33+), requests `READ_MEDIA_IMAGES` for browsing photos in `Insert > Pictures`.
2. **System Storage Access Framework (`Intent.ACTION_OPEN_DOCUMENT` / `Intent.ACTION_CREATE_DOCUMENT`)**:
   - Works without any broad storage permissions and supports Google Drive, Nextcloud, OneDrive,
     and removable USB/SD storage.
   - Opened workbooks are staged into `<ExternalFilesDir>/Imports/<filename>` so `File > Save` and
     `AutoSave` work seamlessly; `Save via System Picker (SAF)…` streams exported `.xlsx`, `.csv`,
     `.tsv`, `.html`, `.pdf`, or `.json` bytes directly into the chosen `content://` URI.
3. **App-Scoped Storage (`Context.getExternalFilesDir(null)` / `Context.getFilesDir()`)**:
   - Always writable with zero permission prompts. If a save targets a read-only path,
     `permissions::resolve_export_target` automatically falls back to `<ExternalFilesDir>/Exports`.
