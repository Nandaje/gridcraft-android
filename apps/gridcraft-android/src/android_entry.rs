//! Android `NativeActivity` entrypoint (`android_main`) and JNI bridge to
//! `ai.storyteller.gridcraft.MainActivity`.

#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::Mutex;

use android_activity::AndroidApp;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::sys::jboolean;
use jni::{EnvUnowned, JavaVM, jni_sig, jni_str};

use crate::gpu_startup::{begin_gpu_startup, build_wgpu_configuration};
use crate::permissions::{AndroidDirs, set_permission_status};
use crate::services::{
    AndroidExternalFileEvent, handle_incoming_saf_bytes, push_external_file_event,
};
use crate::shell::AndroidShell;

const APP_ID: &str = "ai.storyteller.gridcraft";

static ANDROID_APP_HANDLE: Mutex<Option<AndroidApp>> = Mutex::new(None);
static ANDROID_DIRS_CACHE: Mutex<Option<AndroidDirs>> = Mutex::new(None);

fn store_android_app(app: &AndroidApp, dirs: &AndroidDirs) {
    if let Ok(mut slot) = ANDROID_APP_HANDLE.lock() {
        *slot = Some(app.clone());
    }
    if let Ok(mut slot) = ANDROID_DIRS_CACHE.lock() {
        *slot = Some(dirs.clone());
    }
}

fn cached_dirs() -> AndroidDirs {
    if let Ok(slot) = ANDROID_DIRS_CACHE.lock()
        && let Some(dirs) = slot.clone()
    {
        return dirs;
    }
    AndroidDirs::discover()
}

/// Invoke a method on the Java `MainActivity` on the Android UI thread via `AndroidApp`.
fn with_main_activity(
    f: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<()> + Send + 'static,
) {
    let app_opt = ANDROID_APP_HANDLE.lock().ok().and_then(|g| g.clone());
    let Some(app) = app_opt else {
        return;
    };
    let app_clone = app.clone();
    app.run_on_java_main_thread(Box::new(move || {
        let vm_ptr: *mut jni::sys::JavaVM = app_clone.vm_as_ptr().cast();
        if vm_ptr.is_null() {
            return;
        }
        // SAFETY: `AndroidApp::vm_as_ptr` returns the process-wide `JavaVM*` provided by the
        // Android NDK runtime, valid for the lifetime of the JVM.
        let vm = unsafe { JavaVM::from_raw(vm_ptr) };
        let act_raw = app_clone.activity_as_ptr() as jni::sys::jobject;
        if act_raw.is_null() {
            return;
        }
        if let Err(err) = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // SAFETY: `act_raw` is a valid JNI object reference to the running `Activity` for the
            // duration of `run_on_java_main_thread`.
            let activity = unsafe { env.as_cast_raw::<JObject<'_>>(&act_raw)? };
            f(env, &activity)
        }) {
            log::warn!("gridcraft-android: JNI call to MainActivity failed: {err:?}");
        }
    }));
}

/// Request runtime storage permissions (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`,
/// `READ_MEDIA_IMAGES`) via `MainActivity.requestStoragePermissions(false)`.
pub fn request_runtime_storage_permissions() {
    with_main_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("requestStoragePermissions"),
            jni_sig!((all_files: jboolean) -> ()),
            &[JValue::Bool(false)],
        )?;
        Ok(())
    });
}

/// Launch the Android 11+ `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` settings screen via
/// `MainActivity.requestStoragePermissions(true)`.
pub fn request_all_files_access() {
    with_main_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("requestStoragePermissions"),
            jni_sig!((all_files: jboolean) -> ()),
            &[JValue::Bool(true)],
        )?;
        Ok(())
    });
}

/// Launch Android's Storage Access Framework `Intent.ACTION_OPEN_DOCUMENT` picker.
pub fn launch_saf_open_document(pictures_only: bool) {
    with_main_activity(move |env, activity| {
        env.call_method(
            activity,
            jni_str!("launchSafOpenDocument"),
            jni_sig!((pictures_only: jboolean) -> ()),
            &[JValue::Bool(pictures_only)],
        )?;
        Ok(())
    });
}

/// Launch Android's Storage Access Framework `Intent.ACTION_CREATE_DOCUMENT` save picker.
pub fn launch_saf_create_document(suggested_name: &str, mime_type: &str) {
    let name = suggested_name.to_string();
    let mime = mime_type.to_string();
    with_main_activity(move |env, activity| {
        let jname = env.new_string(&name)?;
        let jmime = env.new_string(&mime)?;
        env.call_method(
            activity,
            jni_str!("launchSafCreateDocument"),
            jni_sig!((suggested: JString, mime: JString) -> ()),
            &[JValue::Object(&jname), JValue::Object(&jmime)],
        )?;
        Ok(())
    });
}

/// Stream exported bytes directly into an Android `content://` URI returned by SAF.
pub fn write_uri_bytes(uri_string: &str, bytes: &[u8]) {
    let uri = uri_string.to_string();
    let payload = bytes.to_vec();
    with_main_activity(move |env, activity| {
        let juri = env.new_string(&uri)?;
        let jbytes = env.byte_array_from_slice(&payload)?;
        env.call_method(
            activity,
            jni_str!("writeUriBytes"),
            jni_sig!((uri: JString, data: [jbyte]) -> jboolean),
            &[JValue::Object(&juri), JValue::Object(&jbytes)],
        )?;
        Ok(())
    });
}

/// Notify Android's `MediaScannerConnection` after writing a file to shared storage.
pub fn scan_media_file(file_path: &str) {
    let path = file_path.to_string();
    with_main_activity(move |env, activity| {
        let jpath = env.new_string(&path)?;
        env.call_method(
            activity,
            jni_str!("scanMediaFile"),
            jni_sig!((path: JString) -> ()),
            &[JValue::Object(&jpath)],
        )?;
        Ok(())
    });
}

/// Open an external `http`, `https`, or `mailto` URL via Android's `Intent.ACTION_VIEW`.
pub fn open_external_url(url: &str) {
    let target = url.to_string();
    with_main_activity(move |env, activity| {
        let jurl = env.new_string(&target)?;
        env.call_method(
            activity,
            jni_str!("openExternalUrl"),
            jni_sig!((url: JString) -> ()),
            &[JValue::Object(&jurl)],
        )?;
        Ok(())
    });
}

/// Android `NativeActivity` main entry point invoked by `android-activity`.
// SAFETY: `android_main` is the required unmangled entrypoint symbol expected by `android-activity`.
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    // Enable stylus / S-Pen pressure, tilt, orientation and hover distance axes (used by GridCraft's
    // Draw tab ink pens and highlighters).
    app.enable_motion_axis(android_activity::input::Axis::Pressure);
    app.enable_motion_axis(android_activity::input::Axis::Tilt);
    app.enable_motion_axis(android_activity::input::Axis::Orientation);
    app.enable_motion_axis(android_activity::input::Axis::Distance);

    if let Some(internal) = app.internal_data_path() {
        // SAFETY: Called once on the main thread at process startup before spawning worker threads.
        unsafe {
            std::env::set_var("GRIDCRAFT_PROFILE_DIR", &internal);
        }
    }
    if let Some(external) = app.external_data_path() {
        // SAFETY: Called once on the main thread at process startup before spawning worker threads.
        unsafe {
            std::env::set_var("GRIDCRAFT_EXTERNAL_FILES_DIR", &external);
        }
    }

    let dirs = AndroidDirs::discover();
    store_android_app(&app, &dirs);

    // Request runtime file/media storage permissions on startup if not yet granted.
    request_runtime_storage_permissions();

    // Configure hardware GPU acceleration (Vulkan 1.1+ primary with OpenGL ES 3.0+ fallback).
    let gpu_tier = begin_gpu_startup(dirs.profile_dir());
    let wgpu_options = build_wgpu_configuration(gpu_tier);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("GridCraft")
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        android_app: Some(app),
        ..eframe::NativeOptions::default()
    };

    let run_dirs = dirs.clone();
    if let Err(err) = eframe::run_native(
        "GridCraft",
        options,
        Box::new(move |cc| Ok(Box::new(AndroidShell::new(cc, run_dirs)))),
    ) {
        log::error!("GridCraft Android run_native exited with error: {err}");
    }
}

/// JNI callback invoked by `MainActivity.onPermissionsResult(boolean, boolean)`.
// SAFETY: Standard JNI export signature for `ai.storyteller.gridcraft.MainActivity.onPermissionsResult`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_gridcraft_MainActivity_onPermissionsResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    runtime_granted: jboolean,
    all_files_granted: jboolean,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        set_permission_status(runtime_granted, all_files_granted);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity.onFilePickedBytes(String, byte[])` when a workbook or
/// picture is opened via SAF `ACTION_OPEN_DOCUMENT` or `Intent.ACTION_VIEW` / `Intent.ACTION_SEND`.
// SAFETY: Standard JNI export signature for `ai.storyteller.gridcraft.MainActivity.onFilePickedBytes`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_gridcraft_MainActivity_onFilePickedBytes<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    display_name: JString<'caller>,
    file_bytes: JByteArray<'caller>,
) {
    let outcome = unowned_env.with_env(|env| -> Result<(), jni::errors::Error> {
        let name = display_name.to_string();
        let bytes = env.convert_byte_array(&file_bytes)?;
        let dirs = cached_dirs();
        handle_incoming_saf_bytes(&dirs, &name, bytes);
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity.onFilePickedPath(String)` when a direct `file://` path
/// is opened on Android.
// SAFETY: Standard JNI export signature for `ai.storyteller.gridcraft.MainActivity.onFilePickedPath`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_gridcraft_MainActivity_onFilePickedPath<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    file_path: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        let path = file_path.to_string();
        if !path.trim().is_empty() {
            push_external_file_event(AndroidExternalFileEvent::OpenWorkbookPath(PathBuf::from(
                path,
            )));
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}

/// JNI callback invoked by `MainActivity.onSafCreateResult(String, String)` when the user selects
/// a target URI in Android's `Intent.ACTION_CREATE_DOCUMENT` picker.
// SAFETY: Standard JNI export signature for `ai.storyteller.gridcraft.MainActivity.onSafCreateResult`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_gridcraft_MainActivity_onSafCreateResult<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    uri_string: JString<'caller>,
    display_name: JString<'caller>,
) {
    let outcome = unowned_env.with_env(|_env| -> Result<(), jni::errors::Error> {
        let uri = uri_string.to_string();
        let name = display_name.to_string();
        if !uri.trim().is_empty() {
            push_external_file_event(AndroidExternalFileEvent::SafCreateDocument {
                uri_string: uri,
                display_name: name,
            });
        }
        Ok(())
    });
    outcome.resolve::<ThrowRuntimeExAndDefault>();
}
