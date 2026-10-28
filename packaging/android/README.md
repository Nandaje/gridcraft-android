# GridCraft for Android (`packaging/android`)

This directory contains the Android Gradle project (`ai.storyteller.gridcraft`) that packages
the Rust `gridcraft-android` crate (`libgridcraft_android.so`) into a hardware-accelerated
Android APK (`arm64-v8a`, Android 8.0 Oreo API 26+).

## Key Capabilities

1. **Hardware GPU Acceleration (`wgpu` Vulkan 1.1+ & OpenGL ES 3.0+)**:
   - Uses `eframe`'s `wgpu` backend targeting Android's native `ANativeWindow` surface.
   - Prefers **Vulkan 1.1+** (`Backends::VULKAN`) with automatic fallback to **OpenGL ES 3.0+**
     (`Backends::GL`) and `PresentMode::AutoVsync` (`VK_PRESENT_MODE_FIFO_KHR` / `eglSwapInterval(1)`).
   - Unlocks the mobile GPU's native `max_buffer_size` and `max_texture_dimension_2d` for crisp
     high-DPI spreadsheet grids, charts, pivot tables, and embedded pictures.
   - Includes a startup crash sentinel (`gpu-starting.json`) that automatically switches to the
     OpenGL ES compatibility tier if an interrupted Vulkan driver init is detected.
2. **Android File Access Permissions & Storage Access Framework (SAF)**:
   - Declares and requests runtime storage permissions across Android 8–14+:
     - `READ_EXTERNAL_STORAGE` & `WRITE_EXTERNAL_STORAGE` (Android 8–10, plus `requestLegacyExternalStorage="true"`).
     - `READ_MEDIA_IMAGES` (Android 13+ for inserting pictures into spreadsheets).
     - `MANAGE_EXTERNAL_STORAGE` (`Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` on Android 11+
       for browsing and saving `.xlsx`, `.csv`, `.pdf`, `.html` directly anywhere on `/storage/emulated/0`).
   - Built-in touch-friendly **Android File Browser** modal with bookmarks (`Documents`, `Downloads`,
     `Pictures`, `Shared Storage`, `App Exports`) plus one-tap **System Picker (SAF)** (`ACTION_OPEN_DOCUMENT`
     and `ACTION_CREATE_DOCUMENT`).
   - Handles incoming `ACTION_VIEW`, `ACTION_EDIT`, and `ACTION_SEND` intents for `.xlsx`, `.xlsm`,
     `.xls`, `.xlsb`, `.ods`, `.csv`, `.tsv`, and `.json` files.

## Building

Run from the repository root:

```bash
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.0.12077973"
bash packaging/android/package.sh
```

The resulting APK is written to `dist/release/gridcraft-0.12.1-android-arm64-v8a.apk`.
