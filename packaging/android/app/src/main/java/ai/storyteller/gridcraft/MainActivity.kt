package ai.storyteller.gridcraft

import android.Manifest
import android.app.Activity
import android.app.NativeActivity
import android.content.Intent
import android.content.pm.PackageManager
import android.database.Cursor
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.OpenableColumns
import android.provider.Settings
import android.view.WindowManager

/**
 * Hardware-accelerated Android host activity for GridCraft (`libgridcraft_android.so`).
 *
 * Responsibilities:
 * 1. Requests runtime file/media permissions (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`,
 *    `READ_MEDIA_IMAGES`) and Android 11+ `MANAGE_EXTERNAL_STORAGE` (`ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`).
 * 2. Bridges Storage Access Framework (`Intent.ACTION_OPEN_DOCUMENT` and `Intent.ACTION_CREATE_DOCUMENT`)
 *    content URIs to the Rust `gridcraft-android` crate over JNI.
 * 3. Forwards incoming `ACTION_VIEW`, `ACTION_EDIT`, and `ACTION_SEND` workbook intents.
 * 4. Keeps the hardware-accelerated Vulkan / OpenGL ES window surface configured for high-refresh
 *    display rendering.
 */
class MainActivity : NativeActivity() {

    companion object {
        private const val REQ_STORAGE_PERMISSIONS = 1001
        private const val REQ_ALL_FILES_ACCESS = 1002
        private const val REQ_SAF_OPEN_DOCUMENT = 2001
        private const val REQ_SAF_CREATE_DOCUMENT = 2002

        init {
            System.loadLibrary("gridcraft_android")
        }

        @JvmStatic
        external fun onPermissionsResult(runtimeGranted: Boolean, allFilesGranted: Boolean)

        @JvmStatic
        external fun onFilePickedBytes(displayName: String, bytes: ByteArray)

        @JvmStatic
        external fun onFilePickedPath(filePath: String)

        @JvmStatic
        external fun onSafCreateResult(uriString: String, displayName: String)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        window.addFlags(WindowManager.LayoutParams.FLAG_HARDWARE_ACCELERATED)
        super.onCreate(savedInstanceState)
        reportCurrentPermissions()
        handleIncomingIntent(intent)
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIncomingIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        reportCurrentPermissions()
    }

    /**
     * Called from Rust via JNI to request runtime storage permissions or launch the Android 11+
     * All-Files Access settings screen.
     */
    fun requestStoragePermissions(allFiles: Boolean) {
        if (allFiles && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (!Environment.isExternalStorageManager()) {
                try {
                    val uri = Uri.parse("package:$packageName")
                    val intent = Intent(
                        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        uri
                    )
                    startActivityForResult(intent, REQ_ALL_FILES_ACCESS)
                } catch (_: Throwable) {
                    try {
                        val fallback = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
                        startActivityForResult(fallback, REQ_ALL_FILES_ACCESS)
                    } catch (_: Throwable) {
                    }
                }
            } else {
                reportCurrentPermissions()
            }
            return
        }

        val needed = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) != PackageManager.PERMISSION_GRANTED) {
                needed.add(Manifest.permission.READ_MEDIA_IMAGES)
            }
        } else {
            if (checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED) {
                needed.add(Manifest.permission.READ_EXTERNAL_STORAGE)
            }
            if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.Q &&
                checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED
            ) {
                needed.add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
            }
        }

        if (needed.isNotEmpty()) {
            requestPermissions(needed.toTypedArray(), REQ_STORAGE_PERMISSIONS)
        } else {
            reportCurrentPermissions()
        }
    }

    /**
     * Launch Android's Storage Access Framework `Intent.ACTION_OPEN_DOCUMENT` picker.
     */
    fun launchSafOpenDocument(picturesOnly: Boolean) {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                if (picturesOnly) {
                    type = "image/*"
                    putExtra(Intent.EXTRA_MIME_TYPES, arrayOf("image/png", "image/jpeg"))
                } else {
                    type = "*/*"
                    putExtra(
                        Intent.EXTRA_MIME_TYPES,
                        arrayOf(
                            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                            "application/vnd.ms-excel.sheet.macroEnabled.12",
                            "application/vnd.ms-excel",
                            "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
                            "application/vnd.oasis.opendocument.spreadsheet",
                            "text/csv",
                            "text/comma-separated-values",
                            "text/tab-separated-values",
                            "text/plain",
                            "application/json",
                            "application/octet-stream"
                        )
                    )
                }
            }
            startActivityForResult(intent, REQ_SAF_OPEN_DOCUMENT)
        } catch (_: Throwable) {
        }
    }

    /**
     * Launch Android's Storage Access Framework `Intent.ACTION_CREATE_DOCUMENT` save picker.
     */
    fun launchSafCreateDocument(suggestedName: String, mimeType: String) {
        try {
            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = mimeType.ifEmpty {
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                }
                putExtra(Intent.EXTRA_TITLE, suggestedName.ifEmpty { "Book1.xlsx" })
            }
            startActivityForResult(intent, REQ_SAF_CREATE_DOCUMENT)
        } catch (_: Throwable) {
        }
    }

    /**
     * Write exported bytes into a `content://` URI returned by SAF `ACTION_CREATE_DOCUMENT`.
     */
    fun writeUriBytes(uriString: String, data: ByteArray): Boolean {
        return try {
            val uri = Uri.parse(uriString)
            contentResolver.openOutputStream(uri, "wt")?.use { out ->
                out.write(data)
                out.flush()
            } ?: contentResolver.openOutputStream(uri)?.use { out ->
                out.write(data)
                out.flush()
            }
            true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Notify Android's `MediaScannerConnection` so newly saved workbooks/PDFs/CSVs appear in Files.
     */
    fun scanMediaFile(filePath: String) {
        try {
            MediaScannerConnection.scanFile(this, arrayOf(filePath), null, null)
        } catch (_: Throwable) {
        }
    }

    /**
     * Open an external `http`, `https`, or `mailto` URL in the user's browser or mail client.
     */
    fun openExternalUrl(url: String) {
        try {
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            startActivity(intent)
        } catch (_: Throwable) {
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        reportCurrentPermissions()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        when (requestCode) {
            REQ_ALL_FILES_ACCESS -> reportCurrentPermissions()
            REQ_SAF_OPEN_DOCUMENT -> {
                if (resultCode == Activity.RESULT_OK && data?.data != null) {
                    deliverUriToRust(data.data!!)
                }
            }
            REQ_SAF_CREATE_DOCUMENT -> {
                if (resultCode == Activity.RESULT_OK && data?.data != null) {
                    val uri = data.data!!
                    takePersistableReadWrite(uri)
                    val name = queryDisplayName(uri) ?: "Book1.xlsx"
                    onSafCreateResult(uri.toString(), name)
                }
            }
        }
    }

    private fun reportCurrentPermissions() {
        val allFiles = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            false
        }
        val runtimeGranted = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            allFiles || checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) == PackageManager.PERMISSION_GRANTED
        } else {
            allFiles || checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        }
        try {
            onPermissionsResult(runtimeGranted, allFiles)
        } catch (_: Throwable) {
        }
    }

    private fun handleIncomingIntent(intent: Intent?) {
        if (intent == null) return
        when (intent.action) {
            Intent.ACTION_VIEW, Intent.ACTION_EDIT -> {
                val uri = intent.data ?: return
                deliverUriToRust(uri)
            }
            Intent.ACTION_SEND -> {
                @Suppress("DEPRECATION")
                val uri = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM) as? Uri
                } ?: return
                deliverUriToRust(uri)
            }
        }
    }

    private fun deliverUriToRust(uri: Uri) {
        if (uri.scheme == "file") {
            val path = uri.path
            if (!path.isNullOrEmpty()) {
                onFilePickedPath(path)
                return
            }
        }
        takePersistableReadWrite(uri)
        try {
            val name = queryDisplayName(uri) ?: (uri.lastPathSegment ?: "Imported.xlsx")
            val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() } ?: return
            onFilePickedBytes(name, bytes)
        } catch (_: Throwable) {
        }
    }

    private fun takePersistableReadWrite(uri: Uri) {
        try {
            val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            contentResolver.takePersistableUriPermission(uri, flags)
        } catch (_: Throwable) {
            try {
                contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            } catch (_: Throwable) {
            }
        }
    }

    private fun queryDisplayName(uri: Uri): String? {
        var cursor: Cursor? = null
        return try {
            cursor = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            if (cursor != null && cursor.moveToFirst()) {
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0) cursor.getString(idx) else null
            } else {
                null
            }
        } catch (_: Throwable) {
            null
        } finally {
            cursor?.close()
        }
    }
}
