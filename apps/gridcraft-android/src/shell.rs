//! [`eframe::App`] wrapper around [`gridcraft_ui_egui::SheetApp`] for Android.

use std::fs;
use std::path::Path;

use gridcraft_engine::Session;
use gridcraft_ui_egui::{SheetApp, now_ms};
use serde_json::json;

use crate::file_dialog::{AndroidFileDialog, FileDialogOutcome, SaveFormat};
use crate::gpu_startup::mark_gpu_startup_healthy;
use crate::permissions::{AndroidDirs, resolve_export_target, sanitize_file_name};
use crate::services::{
    AndroidExternalFileEvent, AndroidUiRequest, PendingSafSave, android_services,
    drain_external_file_events, drain_ui_requests, load_prefs, save_prefs, set_pending_saf_save,
    take_pending_saf_save, write_with_android_permissions,
};
use crate::touch_twins;

/// Top-level Android [`eframe::App`] hosting [`SheetApp`] and [`AndroidFileDialog`].
pub struct AndroidShell {
    pub app: SheetApp,
    pub file_dialog: AndroidFileDialog,
    pub dirs: AndroidDirs,
    gpu_marked_healthy: bool,
}

impl AndroidShell {
    /// Construct a new [`AndroidShell`] with an initial blank workbook and loaded Android preferences.
    pub fn new(cc: &eframe::CreationContext<'_>, dirs: AndroidDirs) -> Self {
        let mut session = Session::new();
        session.new_workbook();
        let mut app = SheetApp::new(session, android_services());
        load_prefs(&mut app, &dirs);
        SheetApp::setup_context_for_language(&cc.egui_ctx, app.ui.dark, app.ui.language);
        let file_dialog = AndroidFileDialog::new(dirs.clone());
        Self {
            app,
            file_dialog,
            dirs,
            gpu_marked_healthy: false,
        }
    }

    fn process_ui_requests(&mut self) {
        for req in drain_ui_requests() {
            match req {
                AndroidUiRequest::ShowOpenWorkbook => {
                    self.file_dialog.show_open_workbook();
                }
                AndroidUiRequest::ShowOpenPicture(inbox) => {
                    self.file_dialog.show_open_picture(inbox);
                }
                AndroidUiRequest::ShowSaveBytes {
                    suggested_name,
                    bytes,
                } => {
                    self.file_dialog.show_save_bytes(&suggested_name, bytes);
                }
            }
        }
    }

    fn process_external_events(&mut self) {
        for ev in drain_external_file_events() {
            match ev {
                AndroidExternalFileEvent::OpenWorkbookPath(path) => {
                    if let Some(p) = path.to_str() {
                        self.app.open_path(p);
                        save_prefs(&self.app, &self.dirs);
                    }
                }
                AndroidExternalFileEvent::SafCreateDocument {
                    uri_string,
                    display_name,
                } => {
                    self.complete_saf_create(&uri_string, &display_name);
                }
            }
        }
    }

    fn complete_saf_create(&mut self, uri_string: &str, display_name: &str) {
        let pending = take_pending_saf_save().unwrap_or(PendingSafSave {
            prebuilt_bytes: None,
            format: SaveFormat::from_filename(display_name),
        });
        let safe_name = sanitize_file_name(
            display_name,
            &format!("Workbook.{}", pending.format.extension()),
        );
        let local_target = self.dirs.exports_dir().join(&safe_name);

        let bytes_res: Result<Vec<u8>, String> = if let Some(bytes) = pending.prebuilt_bytes {
            let _ = write_with_android_permissions(&local_target, &bytes, &self.dirs);
            Ok(bytes)
        } else {
            let cmd = pending.format.engine_command();
            match self
                .app
                .session
                .execute(cmd, json!({"path": local_target.to_string_lossy()}))
            {
                Ok(_) => fs::read(&local_target).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        };

        match bytes_res {
            Ok(bytes) => {
                #[cfg(target_os = "android")]
                crate::android_entry::write_uri_bytes(uri_string, &bytes);
                let _ = (uri_string, &bytes);
                if matches!(pending.format, SaveFormat::Xlsx | SaveFormat::Json) {
                    self.record_saved_workbook_path(&local_target);
                }
                self.app.toast = Some((format!("Saved {safe_name}"), now_ms()));
                save_prefs(&self.app, &self.dirs);
            }
            Err(err) => {
                self.app.message = Some(("Save Error".to_string(), err));
            }
        }
    }

    fn handle_dialog_outcome(&mut self, outcome: FileDialogOutcome) {
        match outcome {
            FileDialogOutcome::OpenWorkbookPath(path) => {
                if let Some(p) = path.to_str() {
                    self.app.open_path(p);
                    save_prefs(&self.app, &self.dirs);
                }
            }
            FileDialogOutcome::PickedPicture { inbox, name, bytes } => {
                if let Ok(mut q) = inbox.lock() {
                    q.push((name, bytes));
                }
            }
            FileDialogOutcome::SaveDirectBytes {
                path,
                bytes,
                is_workbook,
            } => match write_with_android_permissions(&path, &bytes, &self.dirs) {
                Ok(saved_path) => {
                    if is_workbook {
                        self.record_saved_workbook_path(&saved_path);
                    }
                    let label = saved_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("file");
                    self.app.toast = Some((format!("Saved {label}"), now_ms()));
                    save_prefs(&self.app, &self.dirs);
                }
                Err(err) => {
                    self.app.message = Some((
                        "Save Error".to_string(),
                        format!("Could not write {}: {err}", path.display()),
                    ));
                }
            },
            FileDialogOutcome::SaveEngineCommand { command, path } => {
                let target = resolve_export_target(&path, &self.dirs);
                self.app
                    .run_or_alert(command, json!({"path": target.to_string_lossy()}));
                if target.is_file() {
                    #[cfg(target_os = "android")]
                    if let Some(s) = target.to_str() {
                        crate::android_entry::scan_media_file(s);
                    }
                    let label = target
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("file");
                    self.app.toast = Some((format!("Saved {label}"), now_ms()));
                    save_prefs(&self.app, &self.dirs);
                }
            }
            FileDialogOutcome::SaveViaSaf {
                file_name,
                mime_type,
                prebuilt_bytes,
                format,
            } => {
                set_pending_saf_save(Some(PendingSafSave {
                    prebuilt_bytes,
                    format,
                }));
                #[cfg(target_os = "android")]
                crate::android_entry::launch_saf_create_document(&file_name, &mime_type);
                let _ = (file_name, mime_type);
            }
        }
    }

    fn record_saved_workbook_path(&mut self, saved_path: &Path) {
        if let Some(d) = self.app.session.active_mut() {
            if let Some(p_str) = saved_path.to_str() {
                d.path = Some(p_str.to_string());
            }
            d.saved = d.wb.clone();
            if let Some(name) = saved_path.file_name().and_then(|s| s.to_str()) {
                d.title = name.to_string();
            }
        }
        if let Some(path_str) = saved_path.to_str() {
            self.app.ui.recent.retain(|r| r != path_str);
            self.app.ui.recent.insert(0, path_str.to_string());
            self.app.ui.recent.truncate(15);
        }
    }
}

impl eframe::App for AndroidShell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        touch_twins::drop_twins(ctx, raw_input);
        self.app.raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_ui_requests();
        self.process_external_events();
        self.app.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        let ctx = ui.ctx().clone();
        if let Some(outcome) = self.file_dialog.show(&ctx) {
            self.handle_dialog_outcome(outcome);
        }
        if self.app.fonts_ready && !self.gpu_marked_healthy {
            mark_gpu_startup_healthy(self.dirs.profile_dir());
            self.gpu_marked_healthy = true;
        }
    }

    fn on_exit(&mut self) {
        save_prefs(&self.app, &self.dirs);
    }
}
