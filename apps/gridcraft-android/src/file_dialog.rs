//! Interactive touch-friendly Android File Browser & Save/Export dialog.
//!
//! Because desktop file pickers (`rfd`) do not exist on Android, GridCraft provides an integrated
//! modal file browser tailored for phones and tablets:
//! - Quick-access bookmark pills for **Documents**, **Downloads**, **Pictures**,
//!   **Shared Storage** (`/storage/emulated/0`), and **App Storage**.
//! - Direct integration with Android's native **Storage Access Framework (SAF)** system picker
//!   (`Intent.ACTION_OPEN_DOCUMENT` / `Intent.ACTION_CREATE_DOCUMENT`) for cloud drives
//!   (Google Drive, Nextcloud, OneDrive) and SD cards.
//! - One-tap **Grant All-Files Access** button when browsing `/storage/emulated/0` on Android 11+
//!   without `MANAGE_EXTERNAL_STORAGE`.
//! - Format selector when saving/exporting workbooks (`.xlsx`, `.csv`, `.tsv`, `.html`, `.pdf`,
//!   `.json`).

use std::fs;
use std::path::{Path, PathBuf};

use gridcraft_ui_egui::Inbox;

use crate::permissions::{AndroidDirs, permission_status, sanitize_file_name};

/// Supported workbook extensions for `OpenWorkbook` mode.
pub const WORKBOOK_EXTENSIONS: &[&str] = &[
    "xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "tsv", "txt", "json",
];

/// Supported image extensions for `OpenPicture` mode.
pub const PICTURE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg"];

/// Export format selectable in the Save/Export dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFormat {
    Xlsx,
    Csv,
    Tsv,
    Html,
    Pdf,
    Json,
}

impl SaveFormat {
    pub const ALL: [Self; 6] = [
        Self::Xlsx,
        Self::Csv,
        Self::Tsv,
        Self::Html,
        Self::Pdf,
        Self::Json,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Html => "html",
            Self::Pdf => "pdf",
            Self::Json => "json",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Xlsx => "Excel Workbook (.xlsx)",
            Self::Csv => "CSV UTF-8 (.csv)",
            Self::Tsv => "Tab-Separated (.tsv)",
            Self::Html => "Web Page (.html)",
            Self::Pdf => "PDF Document (.pdf)",
            Self::Json => "GridCraft JSON (.json)",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
            Self::Html => "text/html",
            Self::Pdf => "application/pdf",
            Self::Json => "application/json",
        }
    }

    pub fn engine_command(self) -> &'static str {
        match self {
            Self::Xlsx | Self::Json => "file.save",
            Self::Csv => "file.exportCsv",
            Self::Tsv => "file.exportTsv",
            Self::Html => "file.exportHtml",
            Self::Pdf => "file.exportPdf",
        }
    }

    pub fn from_filename(name: &str) -> Self {
        let ext = Path::new(name)
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("csv") => Self::Csv,
            Some("tsv") | Some("txt") => Self::Tsv,
            Some("html") | Some("htm") => Self::Html,
            Some("pdf") => Self::Pdf,
            Some("json") => Self::Json,
            _ => Self::Xlsx,
        }
    }
}

/// Active mode of the Android File Browser modal.
#[derive(Clone, Debug)]
pub enum AndroidFileDialogMode {
    /// Browse to open a spreadsheet workbook or data file.
    OpenWorkbook,
    /// Browse to pick a picture (`.png`, `.jpg`, `.jpeg`) for `insertPicture`.
    OpenPicture { inbox: Inbox },
    /// Browse to save/export a file. If `prebuilt_bytes` is `Some`, those exact bytes were
    /// supplied by `services.download` or `services.print` for `initial_format`; if the user
    /// switches formats in the selector, the engine command for the new format is used instead.
    SaveFile {
        prebuilt_bytes: Option<Vec<u8>>,
        initial_format: SaveFormat,
    },
}

/// Entry in a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntryItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size_bytes: u64,
}

/// Action returned when the user confirms a choice in [`AndroidFileDialog`].
#[derive(Clone, Debug)]
pub enum FileDialogOutcome {
    /// Open a workbook at the chosen filesystem path.
    OpenWorkbookPath(PathBuf),
    /// Deliver a picked picture into an `insertPicture` dialog `Inbox`.
    PickedPicture {
        inbox: Inbox,
        name: String,
        bytes: Vec<u8>,
    },
    /// Write pre-rendered bytes (`services.download` / `services.print`) to `path`.
    SaveDirectBytes {
        path: PathBuf,
        bytes: Vec<u8>,
        is_workbook: bool,
    },
    /// Run an engine save/export command (`file.save`, `file.exportCsv`, `file.exportPdf`, etc.)
    /// targeting `path`.
    SaveEngineCommand {
        command: &'static str,
        path: PathBuf,
    },
    /// Launch Android's SAF `ACTION_CREATE_DOCUMENT` system picker for `file_name`.
    SaveViaSaf {
        file_name: String,
        mime_type: String,
        prebuilt_bytes: Option<Vec<u8>>,
        format: SaveFormat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogButtonAction {
    None,
    Confirm,
    ConfirmSaf,
    Cancel,
}

/// State of the interactive Android File Browser modal.
pub struct AndroidFileDialog {
    pub open: bool,
    pub mode: AndroidFileDialogMode,
    pub current_dir: PathBuf,
    pub entries: Vec<DirEntryItem>,
    pub file_name_input: String,
    pub save_format: SaveFormat,
    pub status_error: Option<String>,
    pub new_folder_name: Option<String>,
    dirs: AndroidDirs,
}

impl AndroidFileDialog {
    pub fn new(dirs: AndroidDirs) -> Self {
        let start_dir = default_browse_dir(&dirs);
        let mut dlg = Self {
            open: false,
            mode: AndroidFileDialogMode::OpenWorkbook,
            current_dir: start_dir,
            entries: Vec::new(),
            file_name_input: "Book1.xlsx".to_string(),
            save_format: SaveFormat::Xlsx,
            status_error: None,
            new_folder_name: None,
            dirs,
        };
        dlg.refresh_entries();
        dlg
    }

    /// Open the modal in `OpenWorkbook` mode.
    pub fn show_open_workbook(&mut self) {
        self.mode = AndroidFileDialogMode::OpenWorkbook;
        self.open = true;
        self.status_error = None;
        self.new_folder_name = None;
        if !self.current_dir.exists() {
            self.current_dir = default_browse_dir(&self.dirs);
        }
        self.refresh_entries();
    }

    /// Open the modal in `OpenPicture` mode for `insertPicture`.
    pub fn show_open_picture(&mut self, inbox: Inbox) {
        self.mode = AndroidFileDialogMode::OpenPicture { inbox };
        self.open = true;
        self.status_error = None;
        self.new_folder_name = None;
        if let Some(pics) = &self.dirs.shared_pictures_dir
            && pics.is_dir()
        {
            self.current_dir = pics.clone();
        }
        self.refresh_entries();
    }

    /// Open the modal in `SaveFile` mode with prebuilt bytes (from `services.download` or `services.print`).
    pub fn show_save_bytes(&mut self, suggested_name: &str, bytes: Vec<u8>) {
        let format = SaveFormat::from_filename(suggested_name);
        self.mode = AndroidFileDialogMode::SaveFile {
            prebuilt_bytes: Some(bytes),
            initial_format: format,
        };
        self.save_format = format;
        self.file_name_input = ensure_extension(suggested_name, format.extension());
        self.open = true;
        self.status_error = None;
        self.new_folder_name = None;
        if !self.current_dir.exists() {
            self.current_dir = self.dirs.exports_dir();
        }
        self.refresh_entries();
    }

    /// Refresh the directory listing for `self.current_dir`.
    pub fn refresh_entries(&mut self) {
        let filter_exts = match &self.mode {
            AndroidFileDialogMode::OpenWorkbook => Some(WORKBOOK_EXTENSIONS),
            AndroidFileDialogMode::OpenPicture { .. } => Some(PICTURE_EXTENSIONS),
            AndroidFileDialogMode::SaveFile { .. } => None,
        };
        match read_directory_entries(&self.current_dir, filter_exts) {
            Ok(list) => {
                self.entries = list;
                self.status_error = None;
            }
            Err(err) => {
                self.entries.clear();
                self.status_error = Some(format!(
                    "Cannot read {}: {err}. Use the bookmark buttons above, grant All-Files Access, or tap System Picker (SAF).",
                    self.current_dir.display()
                ));
            }
        }
    }

    /// Navigate to a directory and refresh its contents.
    pub fn navigate_to(&mut self, dir: PathBuf) {
        let _ = fs::create_dir_all(&dir);
        self.current_dir = dir;
        self.refresh_entries();
    }

    /// Render the modal window and return a [`FileDialogOutcome`] if the user confirmed an action.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<FileDialogOutcome> {
        if !self.open {
            return None;
        }

        let title = match &self.mode {
            AndroidFileDialogMode::OpenWorkbook => "Open Workbook",
            AndroidFileDialogMode::OpenPicture { .. } => "Insert Picture",
            AndroidFileDialogMode::SaveFile { .. } => "Save / Export Workbook",
        };

        let mut keep_open = self.open;
        let mut button_action = DialogButtonAction::None;
        let mut clicked_file: Option<PathBuf> = None;

        egui::Window::new(title)
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .default_height(460.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                self.render_bookmarks_bar(ui);
                ui.separator();
                self.render_path_bar(ui);
                self.render_new_folder_row(ui);
                self.render_permission_banner(ui);
                self.render_entries_list(ui, &mut clicked_file, &mut button_action);
                ui.separator();
                self.render_footer(ui, &mut button_action);
            });

        self.open = keep_open;
        self.resolve_outcome(clicked_file, button_action)
    }

    fn render_bookmarks_bar(&mut self, ui: &mut egui::Ui) {
        let mut nav_target: Option<PathBuf> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Locations:").strong());
            for (label, path) in self.bookmark_locations() {
                let selected = self.current_dir == path;
                if ui.selectable_label(selected, label).clicked() {
                    nav_target = Some(path);
                }
            }
            if ui
                .button("📂 System Picker (SAF)")
                .on_hover_text("Open Android's native Storage Access Framework document picker")
                .clicked()
            {
                self.trigger_saf_picker();
            }
        });
        if let Some(dir) = nav_target {
            self.navigate_to(dir);
        }
    }

    fn render_path_bar(&mut self, ui: &mut egui::Ui) {
        let mut nav_target: Option<PathBuf> = None;
        let mut do_refresh = false;
        ui.horizontal(|ui| {
            if ui.button("⬆ Up").clicked()
                && let Some(parent) = self.current_dir.parent()
                && !parent.as_os_str().is_empty()
            {
                nav_target = Some(parent.to_path_buf());
            }
            if ui.button("⟳ Refresh").clicked() {
                do_refresh = true;
            }
            if matches!(self.mode, AndroidFileDialogMode::SaveFile { .. })
                && ui.button("+ New Folder").clicked()
            {
                self.new_folder_name = Some("New Folder".to_string());
            }
            ui.label(
                egui::RichText::new(self.current_dir.to_string_lossy().to_string()).monospace(),
            );
        });
        if let Some(dir) = nav_target {
            self.navigate_to(dir);
        } else if do_refresh {
            self.refresh_entries();
        }
    }

    fn render_new_folder_row(&mut self, ui: &mut egui::Ui) {
        let Some(mut folder_name) = self.new_folder_name.clone() else {
            return;
        };
        let mut create_clicked = false;
        let mut cancel_clicked = false;
        ui.horizontal(|ui| {
            ui.label("Folder name:");
            ui.text_edit_singleline(&mut folder_name);
            if ui.button("Create").clicked() {
                create_clicked = true;
            }
            if ui.button("Cancel").clicked() {
                cancel_clicked = true;
            }
        });
        self.new_folder_name = Some(folder_name.clone());
        if create_clicked {
            let safe = sanitize_file_name(&folder_name, "New Folder");
            let new_dir = self.current_dir.join(safe);
            match fs::create_dir_all(&new_dir) {
                Ok(()) => {
                    self.new_folder_name = None;
                    self.navigate_to(new_dir);
                }
                Err(err) => {
                    self.status_error = Some(format!("Could not create folder: {err}"));
                }
            }
        } else if cancel_clicked {
            self.new_folder_name = None;
        }
    }

    fn render_permission_banner(&mut self, ui: &mut egui::Ui) {
        let perm = permission_status();
        if !perm.all_files_access_granted {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Tip: Grant All-Files Access to browse & save anywhere on shared storage (/storage/emulated/0).",
                    )
                    .small(),
                );
                if ui.small_button("Grant All-Files Access").clicked() {
                    #[cfg(target_os = "android")]
                    crate::android_entry::request_all_files_access();
                }
            });
        }
        if let Some(err) = &self.status_error {
            ui.colored_label(egui::Color32::from_rgb(220, 60, 60), err);
        }
    }

    fn render_entries_list(
        &mut self,
        ui: &mut egui::Ui,
        clicked_file: &mut Option<PathBuf>,
        button_action: &mut DialogButtonAction,
    ) {
        let mut nav_target: Option<PathBuf> = None;
        egui::ScrollArea::vertical()
            .max_height(250.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.entries.is_empty() {
                    ui.weak("No matching files or folders in this directory.");
                    return;
                }
                for entry in &self.entries {
                    let icon = if entry.is_dir { "📁" } else { "📄" };
                    let label = if entry.is_dir {
                        format!("{icon}  {}/", entry.name)
                    } else {
                        format!(
                            "{icon}  {}  ({})",
                            entry.name,
                            format_size(entry.size_bytes)
                        )
                    };
                    let resp = ui.selectable_label(false, label);
                    if resp.clicked() {
                        if entry.is_dir {
                            nav_target = Some(entry.path.clone());
                        } else {
                            self.file_name_input = entry.name.clone();
                            *clicked_file = Some(entry.path.clone());
                        }
                    }
                    if resp.double_clicked() && !entry.is_dir {
                        self.file_name_input = entry.name.clone();
                        *clicked_file = Some(entry.path.clone());
                        *button_action = DialogButtonAction::Confirm;
                    }
                }
            });
        if let Some(dir) = nav_target {
            self.navigate_to(dir);
        }
    }

    fn render_footer(&mut self, ui: &mut egui::Ui, button_action: &mut DialogButtonAction) {
        if matches!(self.mode, AndroidFileDialogMode::SaveFile { .. }) {
            ui.horizontal(|ui| {
                ui.label("File name:");
                ui.text_edit_singleline(&mut self.file_name_input);
            });
            let mut chosen_format = self.save_format;
            ui.horizontal_wrapped(|ui| {
                ui.label("Format:");
                for fmt in SaveFormat::ALL {
                    if ui
                        .selectable_label(chosen_format == fmt, fmt.label())
                        .clicked()
                    {
                        chosen_format = fmt;
                    }
                }
            });
            if chosen_format != self.save_format {
                self.save_format = chosen_format;
                self.file_name_input =
                    ensure_extension(&self.file_name_input, chosen_format.extension());
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("Selected:");
                ui.label(egui::RichText::new(&self.file_name_input).strong());
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let confirm_text = match &self.mode {
                AndroidFileDialogMode::OpenWorkbook => "Open Workbook",
                AndroidFileDialogMode::OpenPicture { .. } => "Insert Picture",
                AndroidFileDialogMode::SaveFile { .. } => "Save Here",
            };
            if ui.button(confirm_text).clicked() {
                *button_action = DialogButtonAction::Confirm;
            }
            if matches!(self.mode, AndroidFileDialogMode::SaveFile { .. })
                && ui
                    .button("Save via System Picker (SAF)…")
                    .on_hover_text("Choose where to save using Android's system file dialog")
                    .clicked()
            {
                *button_action = DialogButtonAction::ConfirmSaf;
            }
            if ui.button("Cancel").clicked() {
                *button_action = DialogButtonAction::Cancel;
            }
        });
    }

    fn resolve_outcome(
        &mut self,
        clicked_file: Option<PathBuf>,
        button_action: DialogButtonAction,
    ) -> Option<FileDialogOutcome> {
        match button_action {
            DialogButtonAction::None => None,
            DialogButtonAction::Cancel => {
                self.open = false;
                None
            }
            DialogButtonAction::ConfirmSaf => {
                let AndroidFileDialogMode::SaveFile {
                    prebuilt_bytes,
                    initial_format,
                } = &self.mode
                else {
                    return None;
                };
                let format = self.save_format;
                let file_name = ensure_extension(&self.file_name_input, format.extension());
                let bytes = if format == *initial_format {
                    prebuilt_bytes.clone()
                } else {
                    None
                };
                self.open = false;
                Some(FileDialogOutcome::SaveViaSaf {
                    file_name,
                    mime_type: format.mime_type().to_string(),
                    prebuilt_bytes: bytes,
                    format,
                })
            }
            DialogButtonAction::Confirm => self.resolve_confirm_outcome(clicked_file),
        }
    }

    fn resolve_confirm_outcome(&mut self, clicked_file: Option<PathBuf>) -> Option<FileDialogOutcome> {
        match &self.mode {
            AndroidFileDialogMode::OpenWorkbook => {
                let candidate =
                    clicked_file.unwrap_or_else(|| self.current_dir.join(self.file_name_input.trim()));
                if candidate.is_file() {
                    self.open = false;
                    Some(FileDialogOutcome::OpenWorkbookPath(candidate))
                } else {
                    self.status_error = Some(format!(
                        "Please select an existing workbook file ({} not found).",
                        candidate.display()
                    ));
                    None
                }
            }
            AndroidFileDialogMode::OpenPicture { inbox } => {
                let candidate =
                    clicked_file.unwrap_or_else(|| self.current_dir.join(self.file_name_input.trim()));
                match fs::read(&candidate) {
                    Ok(bytes) => {
                        let name = candidate
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("picture.png")
                            .to_string();
                        let inbox_clone = inbox.clone();
                        self.open = false;
                        Some(FileDialogOutcome::PickedPicture {
                            inbox: inbox_clone,
                            name,
                            bytes,
                        })
                    }
                    Err(err) => {
                        self.status_error =
                            Some(format!("Could not read {}: {err}", candidate.display()));
                        None
                    }
                }
            }
            AndroidFileDialogMode::SaveFile {
                prebuilt_bytes,
                initial_format,
            } => {
                let format = self.save_format;
                let file_name = ensure_extension(&self.file_name_input, format.extension());
                let path = self.current_dir.join(file_name);
                let use_prebuilt = format == *initial_format && prebuilt_bytes.is_some();
                let bytes_opt = prebuilt_bytes.clone();
                self.open = false;
                if use_prebuilt && let Some(bytes) = bytes_opt {
                    Some(FileDialogOutcome::SaveDirectBytes {
                        path,
                        bytes,
                        is_workbook: matches!(format, SaveFormat::Xlsx | SaveFormat::Json),
                    })
                } else {
                    Some(FileDialogOutcome::SaveEngineCommand {
                        command: format.engine_command(),
                        path,
                    })
                }
            }
        }
    }

    fn trigger_saf_picker(&mut self) {
        match &self.mode {
            AndroidFileDialogMode::OpenWorkbook => {
                #[cfg(target_os = "android")]
                crate::android_entry::launch_saf_open_document(false);
                self.open = false;
            }
            AndroidFileDialogMode::OpenPicture { inbox } => {
                crate::services::set_pending_picture_inbox(Some(inbox.clone()));
                #[cfg(target_os = "android")]
                crate::android_entry::launch_saf_open_document(true);
                self.open = false;
            }
            AndroidFileDialogMode::SaveFile { .. } => {
                let format = self.save_format;
                let file_name = ensure_extension(&self.file_name_input, format.extension());
                #[cfg(target_os = "android")]
                crate::android_entry::launch_saf_create_document(&file_name, format.mime_type());
                let _ = file_name;
            }
        }
    }

    fn bookmark_locations(&self) -> Vec<(&'static str, PathBuf)> {
        let mut out = Vec::new();
        if let Some(docs) = &self.dirs.shared_documents_dir {
            out.push(("Documents", docs.clone()));
            out.push(("Documents/GridCraft", docs.join("GridCraft")));
        }
        if let Some(dl) = &self.dirs.shared_downloads_dir {
            out.push(("Downloads", dl.clone()));
        }
        if let Some(pics) = &self.dirs.shared_pictures_dir {
            out.push(("Pictures", pics.clone()));
        }
        out.push(("Shared Storage", PathBuf::from("/storage/emulated/0")));
        out.push(("App Exports", self.dirs.exports_dir()));
        out.push(("SAF Imports", self.dirs.saf_staging_dir()));
        out
    }
}

fn default_browse_dir(dirs: &AndroidDirs) -> PathBuf {
    if let Some(docs) = &dirs.shared_documents_dir {
        let gc = docs.join("GridCraft");
        if gc.is_dir() || fs::create_dir_all(&gc).is_ok() {
            return gc;
        }
        if docs.is_dir() {
            return docs.clone();
        }
    }
    dirs.exports_dir()
}

/// Ensure a filename ends with the given extension.
pub fn ensure_extension(name: &str, ext: &str) -> String {
    let safe = sanitize_file_name(name, &format!("Book1.{ext}"));
    let expected_suffix = format!(".{ext}");
    if safe
        .to_ascii_lowercase()
        .ends_with(&expected_suffix.to_ascii_lowercase())
    {
        safe
    } else if let Some((stem, _)) = safe.rsplit_once('.') {
        if stem.is_empty() {
            format!("Book1.{ext}")
        } else {
            format!("{stem}.{ext}")
        }
    } else {
        format!("{safe}.{ext}")
    }
}

/// Read and sort directory entries (folders first, then files matching `allowed_extensions`).
pub fn read_directory_entries(
    dir: &Path,
    allowed_extensions: Option<&[&str]>,
) -> std::io::Result<Vec<DirEntryItem>> {
    let mut items = Vec::new();
    for entry_res in fs::read_dir(dir)? {
        let Ok(entry) = entry_res else {
            continue;
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(fs::Metadata::is_dir) || path.is_dir();
        if !is_dir && let Some(exts) = allowed_extensions {
            let ext_ok = path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| exts.iter().any(|allowed| e.eq_ignore_ascii_case(allowed)));
            if !ext_ok {
                continue;
            }
        }
        let size_bytes = meta.as_ref().map_or(0, fs::Metadata::len);
        items.push(DirEntryItem {
            name: name.to_string(),
            path,
            is_dir,
            size_bytes,
        });
    }

    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    Ok(items)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PICTURE_EXTENSIONS, SaveFormat, WORKBOOK_EXTENSIONS, ensure_extension,
        read_directory_entries,
    };

    #[test]
    fn ensure_extension_replaces_or_appends_cleanly() {
        assert_eq!(ensure_extension("Budget", "xlsx"), "Budget.xlsx");
        assert_eq!(ensure_extension("Budget.xlsx", "csv"), "Budget.csv");
        assert_eq!(ensure_extension("Budget.CSV", "csv"), "Budget.CSV");
        assert_eq!(ensure_extension("   ", "pdf"), "Book1.pdf");
    }

    #[test]
    fn save_format_detects_extension() {
        assert_eq!(SaveFormat::from_filename("Report.xlsx"), SaveFormat::Xlsx);
        assert_eq!(SaveFormat::from_filename("Report.csv"), SaveFormat::Csv);
        assert_eq!(SaveFormat::from_filename("Report.tsv"), SaveFormat::Tsv);
        assert_eq!(SaveFormat::from_filename("Report.html"), SaveFormat::Html);
        assert_eq!(SaveFormat::from_filename("Report.pdf"), SaveFormat::Pdf);
        assert_eq!(SaveFormat::from_filename("Report.json"), SaveFormat::Json);
    }

    #[test]
    fn read_directory_entries_filters_by_workbook_and_picture_extensions() {
        let dir = std::env::temp_dir().join(format!("gridcraft-dlg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Subfolder")).unwrap_or(());
        std::fs::write(dir.join("Sheet1.xlsx"), b"xlsx").unwrap_or(());
        std::fs::write(dir.join("Data.csv"), b"a,b\n1,2").unwrap_or(());
        std::fs::write(dir.join("Chart.png"), b"png").unwrap_or(());
        std::fs::write(dir.join("Ignore.exe"), b"bin").unwrap_or(());

        let wb_entries = read_directory_entries(&dir, Some(WORKBOOK_EXTENSIONS)).unwrap_or_default();
        let wb_names: Vec<&str> = wb_entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(wb_names, vec!["Subfolder", "Data.csv", "Sheet1.xlsx"]);

        let pic_entries =
            read_directory_entries(&dir, Some(PICTURE_EXTENSIONS)).unwrap_or_default();
        let pic_names: Vec<&str> = pic_entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(pic_names, vec!["Subfolder", "Chart.png"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
