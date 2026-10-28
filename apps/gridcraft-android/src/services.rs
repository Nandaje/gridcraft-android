//! Android implementation of [`gridcraft_ui_egui::Services`], preferences persistence, and
//! Storage Access Framework (`content://`) file staging.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gridcraft_ui_egui::{Inbox, Services, SheetApp, UiState};

use crate::file_dialog::SaveFormat;
use crate::permissions::{AndroidDirs, resolve_export_target, sanitize_file_name};

/// Requests sent from [`Services`] callbacks to [`crate::shell::AndroidShell`] on the UI thread.
#[derive(Clone, Debug)]
pub enum AndroidUiRequest {
    /// Open the interactive Android File Browser in `OpenWorkbook` mode.
    ShowOpenWorkbook,
    /// Open the interactive Android File Browser in `OpenPicture` mode for `insertPicture`.
    ShowOpenPicture(Inbox),
    /// Open the interactive Android File Browser in `SaveFile` mode with prebuilt bytes
    /// (from `Save As`, `Export`, or `Print`).
    ShowSaveBytes {
        suggested_name: String,
        bytes: Vec<u8>,
    },
}

/// External file events arriving asynchronously from Android JNI (`Intent.ACTION_VIEW`,
/// `Intent.ACTION_SEND`, or Storage Access Framework `onActivityResult`).
#[derive(Clone, Debug)]
pub enum AndroidExternalFileEvent {
    /// Open a staged or direct workbook file on the next UI frame.
    OpenWorkbookPath(PathBuf),
    /// Fulfill an `Intent.ACTION_CREATE_DOCUMENT` SAF result by writing to `uri_string`.
    SafCreateDocument {
        uri_string: String,
        display_name: String,
    },
}

/// Pending payload waiting for Android's `ACTION_CREATE_DOCUMENT` SAF callback.
#[derive(Clone, Debug)]
pub struct PendingSafSave {
    pub prebuilt_bytes: Option<Vec<u8>>,
    pub format: SaveFormat,
}

static UI_REQUEST_QUEUE: Mutex<Vec<AndroidUiRequest>> = Mutex::new(Vec::new());
static EXTERNAL_FILE_QUEUE: Mutex<Vec<AndroidExternalFileEvent>> = Mutex::new(Vec::new());
static PENDING_PICTURE_INBOX: Mutex<Option<Inbox>> = Mutex::new(None);
static PENDING_SAF_SAVE: Mutex<Option<PendingSafSave>> = Mutex::new(None);

/// Enqueue a UI request from a [`Services`] callback.
pub fn push_ui_request(req: AndroidUiRequest) {
    if let Ok(mut q) = UI_REQUEST_QUEUE.lock() {
        q.push(req);
    }
}

/// Drain all queued [`AndroidUiRequest`] items on the main UI loop.
pub fn drain_ui_requests() -> Vec<AndroidUiRequest> {
    if let Ok(mut q) = UI_REQUEST_QUEUE.lock() {
        std::mem::take(&mut *q)
    } else {
        Vec::new()
    }
}

/// Enqueue an external file event from JNI (`onFilePickedBytes`, `onFilePickedPath`, `onSafCreateResult`).
pub fn push_external_file_event(ev: AndroidExternalFileEvent) {
    if let Ok(mut q) = EXTERNAL_FILE_QUEUE.lock() {
        q.push(ev);
    }
}

/// Drain all queued [`AndroidExternalFileEvent`] items on the main UI loop.
pub fn drain_external_file_events() -> Vec<AndroidExternalFileEvent> {
    if let Ok(mut q) = EXTERNAL_FILE_QUEUE.lock() {
        std::mem::take(&mut *q)
    } else {
        Vec::new()
    }
}

/// Register the `Inbox` of an active `insertPicture` dialog before launching SAF `ACTION_OPEN_DOCUMENT`.
pub fn set_pending_picture_inbox(inbox: Option<Inbox>) {
    if let Ok(mut slot) = PENDING_PICTURE_INBOX.lock() {
        *slot = inbox;
    }
}

/// Take the pending `insertPicture` `Inbox`, if one was waiting for a SAF image pick.
pub fn take_pending_picture_inbox() -> Option<Inbox> {
    if let Ok(mut slot) = PENDING_PICTURE_INBOX.lock() {
        slot.take()
    } else {
        None
    }
}

/// Store a [`PendingSafSave`] while waiting for Android's `ACTION_CREATE_DOCUMENT` picker.
pub fn set_pending_saf_save(pending: Option<PendingSafSave>) {
    if let Ok(mut slot) = PENDING_SAF_SAVE.lock() {
        *slot = pending;
    }
}

/// Take the [`PendingSafSave`] when `onSafCreateResult` arrives.
pub fn take_pending_saf_save() -> Option<PendingSafSave> {
    if let Ok(mut slot) = PENDING_SAF_SAVE.lock() {
        slot.take()
    } else {
        None
    }
}

/// Build the Android [`Services`] table wired into [`SheetApp`].
pub fn android_services() -> Services {
    let inbox: Inbox = std::sync::Arc::new(Mutex::new(Vec::new()));
    Services {
        inbox: Some(inbox),
        open_async: Some(Box::new(|| {
            push_ui_request(AndroidUiRequest::ShowOpenWorkbook);
        })),
        pick_picture_async: Some(Box::new(|pic_inbox: Inbox| {
            push_ui_request(AndroidUiRequest::ShowOpenPicture(pic_inbox));
        })),
        download: Some(Box::new(|suggested_name: &str, bytes: &[u8]| {
            push_ui_request(AndroidUiRequest::ShowSaveBytes {
                suggested_name: suggested_name.to_string(),
                bytes: bytes.to_vec(),
            });
        })),
        print: Some(Box::new(|pdf_bytes: &[u8]| {
            push_ui_request(AndroidUiRequest::ShowSaveBytes {
                suggested_name: "Workbook.pdf".to_string(),
                bytes: pdf_bytes.to_vec(),
            });
        })),
        open_url: Some(Box::new(|url: &str| {
            open_url_on_android(url);
        })),
        ..Services::default()
    }
}

fn open_url_on_android(target: &str) {
    let gridcraft_engine::links::LinkKind::External(valid_url) =
        gridcraft_engine::links::classify(target)
    else {
        log::warn!("Refused to open {target:?}: only http, https and mailto links are opened");
        return;
    };
    log::info!("Opening external URL on Android: {valid_url}");
    #[cfg(target_os = "android")]
    crate::android_entry::open_external_url(&valid_url);
}

/// Stage bytes read from an Android `content://` URI into the app's `Imports` directory so
/// `gridcraft-engine` can open them as a standard file path with full `Save` / `AutoSave` support.
pub fn stage_saf_bytes(
    dirs: &AndroidDirs,
    display_name: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let safe_name = sanitize_file_name(display_name, "Imported.xlsx");
    let target = dirs.saf_staging_dir().join(safe_name);
    fs::write(&target, bytes)?;
    Ok(target)
}

/// Route incoming bytes from Android JNI (`onFilePickedBytes`) either to a waiting `insertPicture`
/// dialog `Inbox` or to a staged workbook path.
pub fn handle_incoming_saf_bytes(dirs: &AndroidDirs, display_name: &str, bytes: Vec<u8>) {
    if let Some(pic_inbox) = take_pending_picture_inbox() {
        let safe_name = sanitize_file_name(display_name, "picture.png");
        if let Ok(mut q) = pic_inbox.lock() {
            q.push((safe_name, bytes));
        }
        return;
    }
    match stage_saf_bytes(dirs, display_name, &bytes) {
        Ok(staged_path) => {
            push_external_file_event(AndroidExternalFileEvent::OpenWorkbookPath(staged_path));
        }
        Err(err) => {
            log::error!("Failed to stage SAF bytes for {display_name}: {err}");
        }
    }
}

/// Write exported/saved bytes to `requested_path`, resolving permissions and falling back to
/// `dirs.exports_dir()` if the requested parent directory is not writable. Also triggers Android's
/// `MediaScannerConnection` so the file appears immediately in Files / Gallery apps.
pub fn write_with_android_permissions(
    requested_path: &Path,
    bytes: &[u8],
    dirs: &AndroidDirs,
) -> std::io::Result<PathBuf> {
    let resolved = resolve_export_target(requested_path, dirs);
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&resolved, bytes)?;
    #[cfg(target_os = "android")]
    if let Some(path_str) = resolved.to_str() {
        crate::android_entry::scan_media_file(path_str);
    }
    Ok(resolved)
}

/// Load persisted `ui.json` and `prefs.json` from `dirs.profile_dir()`.
pub fn load_prefs(app: &mut SheetApp, dirs: &AndroidDirs) {
    app.ui.language = gridcraft_ui_egui::i18n::Language::system();
    app.ui.system_theme = true;
    let dir = dirs.profile_dir();
    if let Ok(b) = fs::read(dir.join("ui.json"))
        && let Ok(ui) = serde_json::from_slice::<UiState>(&b)
    {
        let saved = serde_json::from_slice(&b)
            .ok()
            .and_then(|v| gridcraft_ui_egui::i18n::saved_language(&v));
        let language = saved.unwrap_or(app.ui.language);
        app.ui = ui.at_startup();
        app.ui.language = language;
    }
    if let Ok(b) = fs::read(dir.join("prefs.json"))
        && let Ok(p) = serde_json::from_slice(&b)
    {
        app.session.prefs = p;
    }
}

/// Save `ui.json` and `prefs.json` into `dirs.profile_dir()`.
pub fn save_prefs(app: &SheetApp, dirs: &AndroidDirs) {
    let dir = dirs.profile_dir();
    let _ = fs::create_dir_all(dir);
    if let Ok(b) = serde_json::to_vec_pretty(&app.ui) {
        let _ = fs::write(dir.join("ui.json"), b);
    }
    if let Ok(b) = serde_json::to_vec_pretty(&app.session.prefs) {
        let _ = fs::write(dir.join("prefs.json"), b);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AndroidExternalFileEvent, android_services, drain_external_file_events,
        handle_incoming_saf_bytes, load_prefs, save_prefs, set_pending_picture_inbox,
        write_with_android_permissions,
    };
    use crate::permissions::AndroidDirs;
    use gridcraft_engine::Session;
    use gridcraft_ui_egui::{Inbox, SheetApp};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn test_dirs(tag: &str) -> AndroidDirs {
        let root =
            std::env::temp_dir().join(format!("gridcraft-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let internal = root.join("internal");
        let external = root.join("external");
        let shared = root.join("shared/Documents");
        std::fs::create_dir_all(&internal).unwrap_or(());
        std::fs::create_dir_all(&external).unwrap_or(());
        std::fs::create_dir_all(&shared).unwrap_or(());
        AndroidDirs {
            internal_files_dir: internal,
            external_files_dir: Some(external),
            shared_documents_dir: Some(shared),
            shared_downloads_dir: None,
            shared_pictures_dir: None,
        }
    }

    #[test]
    fn workbook_xlsx_csv_html_pdf_roundtrip_with_android_permissions() {
        let dirs = test_dirs("roundtrip");
        let mut session = Session::new();
        session.new_workbook();
        assert!(
            session
                .execute("cell.set", json!({"cell": "A1", "input": "Revenue"}))
                .is_ok()
        );
        assert!(
            session
                .execute("cell.set", json!({"cell": "B1", "input": "1250"}))
                .is_ok()
        );
        assert!(
            session
                .execute("cell.set", json!({"cell": "A2", "input": "Expenses"}))
                .is_ok()
        );
        assert!(
            session
                .execute("cell.set", json!({"cell": "B2", "input": "450"}))
                .is_ok()
        );
        assert!(
            session
                .execute("cell.set", json!({"cell": "B3", "input": "=B1-B2"}))
                .is_ok()
        );

        let Some(doc) = session.active() else {
            return;
        };
        let xlsx_bytes =
            gridcraft_engine::io::save_bytes(&doc.wb, "Financials.xlsx").unwrap_or_default();
        assert!(!xlsx_bytes.is_empty());

        let saved_xlsx = write_with_android_permissions(
            &dirs.exports_dir().join("Financials.xlsx"),
            &xlsx_bytes,
            &dirs,
        )
        .unwrap_or_default();
        assert!(saved_xlsx.is_file());

        // Export CSV, HTML, and PDF via engine commands.
        let csv_path = dirs.exports_dir().join("Financials.csv");
        let html_path = dirs.exports_dir().join("Financials.html");
        let pdf_path = dirs.exports_dir().join("Financials.pdf");
        assert!(
            session
                .execute("file.exportCsv", json!({"path": csv_path.to_string_lossy()}))
                .is_ok()
        );
        assert!(
            session
                .execute(
                    "file.exportHtml",
                    json!({"path": html_path.to_string_lossy()})
                )
                .is_ok()
        );
        assert!(
            session
                .execute("file.exportPdf", json!({"path": pdf_path.to_string_lossy()}))
                .is_ok()
        );
        assert!(csv_path.is_file());
        assert!(html_path.is_file());
        assert!(pdf_path.is_file());

        // Re-open the saved .xlsx workbook and verify formula result.
        let mut session2 = Session::new();
        assert!(
            session2
                .execute("file.open", json!({"path": saved_xlsx.to_string_lossy()}))
                .is_ok()
        );
        let val = session2
            .active()
            .and_then(|d| {
                d.wb.active().map(|sh| {
                    gridcraft_engine::display::cell_text(
                        &d.wb,
                        sh,
                        gridcraft_engine::core::CellRef::new(2, 1),
                    )
                })
            })
            .unwrap_or_default();
        assert_eq!(val, "800");
    }

    #[test]
    fn saf_bytes_route_to_picture_inbox_or_workbook_staging() {
        let dirs = test_dirs("saf");
        let _ = drain_external_file_events();

        // 1. When a picture inbox is waiting, SAF bytes go to that inbox.
        let pic_inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        set_pending_picture_inbox(Some(pic_inbox.clone()));
        handle_incoming_saf_bytes(&dirs, "Logo.png", vec![137, 80, 78, 71]);
        let pics = pic_inbox
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].0, "Logo.png");
        assert!(drain_external_file_events().is_empty());

        // 2. Otherwise, SAF bytes are staged into Imports and queued as OpenWorkbookPath.
        handle_incoming_saf_bytes(&dirs, "Quarterly.xlsx", b"PK\x03\x04".to_vec());
        let events = drain_external_file_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AndroidExternalFileEvent::OpenWorkbookPath(p) => {
                assert!(p.is_file());
                assert!(p.ends_with("Quarterly.xlsx"));
            }
            AndroidExternalFileEvent::SafCreateDocument { .. } => {}
        }
    }

    #[test]
    fn prefs_roundtrip_saves_and_restores_ui_and_session_prefs() {
        let dirs = test_dirs("prefs");
        let mut session = Session::new();
        session.new_workbook();
        let mut app = SheetApp::new(session, android_services());
        app.ui.dark = true;
        app.ui.system_theme = false;
        app.ui.autosave = true;
        app.session.prefs.user_name = "Android Tester".to_string();
        save_prefs(&app, &dirs);

        let mut session2 = Session::new();
        session2.new_workbook();
        let mut app2 = SheetApp::new(session2, android_services());
        load_prefs(&mut app2, &dirs);
        assert!(app2.ui.dark);
        assert!(!app2.ui.system_theme);
        assert!(app2.ui.autosave);
        assert_eq!(app2.session.prefs.user_name, "Android Tester");
    }
}
