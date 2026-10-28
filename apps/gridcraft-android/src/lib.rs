//! GridCraft for Android — native mobile spreadsheet shell.
//!
//! Architecture:
//! - [`gpu_startup`]: Hardware-accelerated `wgpu` configuration targeting Android GPUs
//!   (Vulkan 1.1+ primary with OpenGL ES 3.0+ fallback, high-performance adapter selection,
//!   unlocked hardware buffer/texture limits, and crash-sentinel recovery).
//! - [`permissions`]: Runtime Android file-access permission requests (`READ_EXTERNAL_STORAGE`,
//!   `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE`) and scoped-storage
//!   directory resolution.
//! - [`file_dialog`]: Interactive touch-friendly Android File Browser modal for opening workbooks
//!   (`.xlsx`, `.xlsm`, `.xls`, `.xlsb`, `.ods`, `.csv`, `.tsv`, `.txt`, `.json`), inserting
//!   pictures (`.png`, `.jpg`, `.jpeg`), and saving/exporting workbooks, CSVs, HTML, and PDFs.
//! - [`services`]: Android implementation of [`gridcraft_ui_egui::Services`] (Storage Access
//!   Framework content URI staging, direct filesystem I/O, preferences persistence, and URL/share
//!   intent hooks).
//! - [`shell`]: [`eframe::App`] wrapper around [`gridcraft_ui_egui::SheetApp`] that drains queued
//!   Android intent events (`ACTION_VIEW` / `ACTION_SEND` / SAF picker results) and drives the
//!   interactive file dialog.
//! - [`touch_twins`]: Deduplicates synthesized mouse events emitted alongside Android touch events.
//! - [`android_entry`]: `android_main` entrypoint for `android-activity` (`NativeActivity`) plus
//!   JNI callbacks from `MainActivity.kt`.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

#[cfg(target_os = "android")]
mod android_entry;
pub mod file_dialog;
pub mod gpu_startup;
pub mod permissions;
pub mod services;
pub mod shell;
pub mod touch_twins;
