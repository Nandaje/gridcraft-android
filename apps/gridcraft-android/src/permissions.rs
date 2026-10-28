//! Android runtime file-access permissions and scoped-storage path resolution.
//!
//! Handles Android's multi-generation storage permission model:
//! - **Android 8–9 (API 26–28)**: `READ_EXTERNAL_STORAGE` + `WRITE_EXTERNAL_STORAGE` runtime
//!   permissions for `/storage/emulated/0/{Documents,Download,Pictures}` access.
//! - **Android 10 (API 29)**: `requestLegacyExternalStorage="true"` + runtime read/write
//!   permissions alongside Storage Access Framework (`Intent.ACTION_OPEN_DOCUMENT` /
//!   `Intent.ACTION_CREATE_DOCUMENT`).
//! - **Android 11–14+ (API 30–35)**:
//!   - App-specific internal (`Context.getFilesDir()`) and external
//!     (`Context.getExternalFilesDir(null)`) directories require zero runtime dialog prompts.
//!   - Shared media (`READ_MEDIA_IMAGES` on API 33+) for inserting photos/pictures into sheets.
//!   - Broad shared-storage file access (`MANAGE_EXTERNAL_STORAGE` via
//!     `Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`) for power users managing `.xlsx` /
//!     `.csv` trees directly on `/storage/emulated/0`.
//!   - Storage Access Framework (SAF) content URI bridge for zero-permission picker workflows.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Android manifest permission constants.
pub const PERM_READ_EXTERNAL_STORAGE: &str = "android.permission.READ_EXTERNAL_STORAGE";
pub const PERM_WRITE_EXTERNAL_STORAGE: &str = "android.permission.WRITE_EXTERNAL_STORAGE";
pub const PERM_READ_MEDIA_IMAGES: &str = "android.permission.READ_MEDIA_IMAGES";
pub const PERM_MANAGE_EXTERNAL_STORAGE: &str = "android.permission.MANAGE_EXTERNAL_STORAGE";

static STORAGE_PERMISSIONS_GRANTED: AtomicBool = AtomicBool::new(false);
static ALL_FILES_ACCESS_GRANTED: AtomicBool = AtomicBool::new(false);

/// Current runtime storage permission state reported by Android.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoragePermissionStatus {
    /// Standard runtime storage permissions (`READ_EXTERNAL_STORAGE` / `WRITE_EXTERNAL_STORAGE` /
    /// `READ_MEDIA_IMAGES`) or app-scoped storage availability.
    pub runtime_storage_granted: bool,
    /// Android 11+ `Environment.isExternalStorageManager()` (`MANAGE_EXTERNAL_STORAGE`).
    pub all_files_access_granted: bool,
}

/// Update the cached permission state (called from JNI `onPermissionsResult` and startup).
pub fn set_permission_status(runtime_granted: bool, all_files_granted: bool) {
    STORAGE_PERMISSIONS_GRANTED.store(runtime_granted, Ordering::SeqCst);
    ALL_FILES_ACCESS_GRANTED.store(all_files_granted, Ordering::SeqCst);
    log::info!(
        "Android file permissions updated: runtime_storage_granted={runtime_granted}, all_files_access_granted={all_files_granted}"
    );
}

/// Query the cached Android storage permission status.
pub fn permission_status() -> StoragePermissionStatus {
    StoragePermissionStatus {
        runtime_storage_granted: STORAGE_PERMISSIONS_GRANTED.load(Ordering::SeqCst),
        all_files_access_granted: ALL_FILES_ACCESS_GRANTED.load(Ordering::SeqCst),
    }
}

/// Resolved directories used by GridCraft on Android for preferences, autosave, imports, and exports.
#[derive(Clone, Debug)]
pub struct AndroidDirs {
    /// Internal app-private directory (`Context.getFilesDir()`), always writable.
    pub internal_files_dir: PathBuf,
    /// App-specific external directory (`/storage/emulated/0/Android/data/ai.storyteller.gridcraft/files`),
    /// visible to USB MTP & file managers without requiring runtime permissions.
    pub external_files_dir: Option<PathBuf>,
    /// Shared `/storage/emulated/0/Documents` directory when accessible.
    pub shared_documents_dir: Option<PathBuf>,
    /// Shared `/storage/emulated/0/Download` directory when accessible.
    pub shared_downloads_dir: Option<PathBuf>,
    /// Shared `/storage/emulated/0/Pictures` directory when accessible.
    pub shared_pictures_dir: Option<PathBuf>,
}

impl AndroidDirs {
    /// Discover and create the required GridCraft directories on Android (or host test environment).
    pub fn discover() -> Self {
        let internal_files_dir = std::env::var_os("GRIDCRAFT_PROFILE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/data/data/ai.storyteller.gridcraft/files"));
        let _ = fs::create_dir_all(&internal_files_dir);

        let external_files_dir = std::env::var_os("GRIDCRAFT_EXTERNAL_FILES_DIR")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty());
        if let Some(ext) = &external_files_dir {
            let _ = fs::create_dir_all(ext);
        }

        let shared_root = std::env::var_os("GRIDCRAFT_SHARED_STORAGE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/storage/emulated/0"));

        let shared_documents_dir = Some(shared_root.join("Documents"));
        let shared_downloads_dir = Some(shared_root.join("Download"));
        let shared_pictures_dir = Some(shared_root.join("Pictures"));

        Self {
            internal_files_dir,
            external_files_dir,
            shared_documents_dir,
            shared_downloads_dir,
            shared_pictures_dir,
        }
    }

    /// Directory where `ui.json`, `prefs.json`, and `gpu-starting.json` are stored.
    pub fn profile_dir(&self) -> &Path {
        &self.internal_files_dir
    }

    /// Directory where SAF (`content://`) workbooks and pictures are staged so the engine can read
    /// them as standard filesystem paths.
    pub fn saf_staging_dir(&self) -> PathBuf {
        let base = self
            .external_files_dir
            .as_deref()
            .unwrap_or(&self.internal_files_dir);
        let dir = base.join("Imports");
        let _ = fs::create_dir_all(&dir);
        dir
    }

    /// Preferred directory for saving/exporting user workbooks (`.xlsx`), CSVs, HTML, and PDFs.
    pub fn exports_dir(&self) -> PathBuf {
        if let Some(docs) = &self.shared_documents_dir {
            let gridcraft_docs = docs.join("GridCraft");
            if is_dir_writable(&gridcraft_docs) {
                return gridcraft_docs;
            }
        }
        let base = self
            .external_files_dir
            .as_deref()
            .unwrap_or(&self.internal_files_dir);
        let dir = base.join("Exports");
        let _ = fs::create_dir_all(&dir);
        dir
    }
}

/// Test whether a directory can be created and written to under the current Android permissions.
pub fn is_dir_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".gridcraft_write_probe");
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Sanitize an untrusted filename from a `content://` URI or user input.
pub fn sanitize_file_name(name: &str, fallback: &str) -> String {
    let trimmed = name.trim();
    let cleaned: String = trimmed
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned.to_string()
    }
}

/// Resolve a requested save/export path into a writable location on Android.
///
/// If the requested path's parent directory is writable (for example `/storage/emulated/0/Documents`
/// when `MANAGE_EXTERNAL_STORAGE` or legacy write permission is granted), it is used directly.
/// Otherwise, if runtime permissions have not been granted yet, this triggers an Android permission
/// request via JNI and falls back to the app's writable `Exports` directory so data is never lost.
pub fn resolve_export_target(requested: &Path, dirs: &AndroidDirs) -> PathBuf {
    let file_name = requested
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| sanitize_file_name(s, "Book1.xlsx"))
        .unwrap_or_else(|| "Book1.xlsx".to_string());

    if requested.is_absolute()
        && let Some(parent) = requested.parent()
        && !parent.as_os_str().is_empty()
        && is_dir_writable(parent)
    {
        return parent.join(&file_name);
    }

    // Request runtime permissions on Android if shared storage was targeted but isn't writable yet.
    #[cfg(target_os = "android")]
    {
        let status = permission_status();
        if !status.runtime_storage_granted {
            crate::android_entry::request_runtime_storage_permissions();
        }
    }

    dirs.exports_dir().join(file_name)
}

#[cfg(test)]
mod tests {
    use super::{
        AndroidDirs, is_dir_writable, permission_status, resolve_export_target, sanitize_file_name,
        set_permission_status,
    };
    use std::path::Path;

    #[test]
    fn sanitize_strips_path_separators_and_reserved_chars() {
        assert_eq!(
            sanitize_file_name("../../etc/passwd:evil?.xlsx", "Book1.xlsx"),
            "_.._etc_passwd_evil_.xlsx"
        );
        assert_eq!(sanitize_file_name("   ...   ", "Book1.xlsx"), "Book1.xlsx");
        assert_eq!(
            sanitize_file_name("Q4 Financials (2026).xlsx", "Book1.xlsx"),
            "Q4 Financials (2026).xlsx"
        );
    }

    #[test]
    fn resolve_export_target_uses_writable_dir_or_falls_back_to_exports() {
        let root = std::env::temp_dir().join(format!("gridcraft-perm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let internal = root.join("internal");
        let external = root.join("external");
        let shared_docs = root.join("shared/Documents");

        let dirs = AndroidDirs {
            internal_files_dir: internal,
            external_files_dir: Some(external),
            shared_documents_dir: Some(shared_docs.clone()),
            shared_downloads_dir: None,
            shared_pictures_dir: None,
        };

        assert!(is_dir_writable(&dirs.saf_staging_dir()));
        let target = resolve_export_target(Path::new("Budget.xlsx"), &dirs);
        assert_eq!(target, shared_docs.join("GridCraft").join("Budget.xlsx"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn permission_status_roundtrips_atomic_flags() {
        set_permission_status(true, false);
        let st = permission_status();
        assert!(st.runtime_storage_granted);
        assert!(!st.all_files_access_granted);

        set_permission_status(true, true);
        let st2 = permission_status();
        assert!(st2.runtime_storage_granted);
        assert!(st2.all_files_access_granted);
    }
}
