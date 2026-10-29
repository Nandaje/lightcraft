package ai.storyteller.lightcraft

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

/**
 * Hardware-accelerated Android host activity for LightCraft (`liblightcraft_android.so`).
 *
 * Bridges Android runtime file permissions (`READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`,
 * `READ_MEDIA_IMAGES`, `MANAGE_EXTERNAL_STORAGE`), Storage Access Framework (`ACTION_OPEN_DOCUMENT`,
 * `ACTION_CREATE_DOCUMENT` with multi-select support), `MediaScannerConnection`, and incoming
 * `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` / `ACTION_SEND_MULTIPLE` intents to Rust via JNI.
 */
class MainActivity : NativeActivity() {

    companion object {
        private const val REQ_PERMISSIONS = 1001
        private const val REQ_ALL_FILES = 1002
        private const val REQ_SAF_OPEN = 1003
        private const val REQ_SAF_CREATE = 1004

        init {
            System.loadLibrary("lightcraft_android")
        }

        @JvmStatic
        external fun onPermissionsResult(
            readGranted: Boolean,
            writeGranted: Boolean,
            allFilesGranted: Boolean
        )

        @JvmStatic
        external fun onFilePickedBytes(displayName: String, bytes: ByteArray)

        @JvmStatic
        external fun onFilePickedPath(path: String)

        @JvmStatic
        external fun onSafCreateResult(displayName: String): ByteArray
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        syncPermissionStateToRust()
        handleIncomingIntent(intent)
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIncomingIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        syncPermissionStateToRust()
    }

    fun hasReadStoragePermission(): Boolean {
        if (hasAllFilesAccess()) return true
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) == PackageManager.PERMISSION_GRANTED
        } else {
            checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        }
    }

    fun hasWriteStoragePermission(): Boolean {
        if (hasAllFilesAccess()) return true
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            true
        } else {
            checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        }
    }

    fun hasAllFilesAccess(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            hasReadStoragePermission() && hasWriteStoragePermission()
        }
    }

    fun requestStoragePermissions() {
        runOnUiThread {
            val needed = mutableListOf<String>()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                if (checkSelfPermission(Manifest.permission.READ_MEDIA_IMAGES) != PackageManager.PERMISSION_GRANTED) {
                    needed.add(Manifest.permission.READ_MEDIA_IMAGES)
                }
            } else {
                if (checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED) {
                    needed.add(Manifest.permission.READ_EXTERNAL_STORAGE)
                }
                if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.P &&
                    checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED
                ) {
                    needed.add(Manifest.permission.WRITE_EXTERNAL_STORAGE)
                }
            }
            if (needed.isNotEmpty()) {
                requestPermissions(needed.toTypedArray(), REQ_PERMISSIONS)
            } else {
                syncPermissionStateToRust()
            }
        }
    }

    fun requestAllFilesAccess() {
        runOnUiThread {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !Environment.isExternalStorageManager()) {
                try {
                    val uri = Uri.parse("package:$packageName")
                    val intent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri)
                    startActivityForResult(intent, REQ_ALL_FILES)
                } catch (_: Throwable) {
                    try {
                        val fallback = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
                        startActivityForResult(fallback, REQ_ALL_FILES)
                    } catch (_: Throwable) {
                    }
                }
            } else {
                requestStoragePermissions()
            }
        }
    }

    fun launchSafOpenPicker(multiple: Boolean) {
        runOnUiThread {
            try {
                val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = "*/*"
                    putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple)
                }
                startActivityForResult(intent, REQ_SAF_OPEN)
            } catch (_: Throwable) {
            }
        }
    }

    fun launchSafCreatePicker(suggestedName: String) {
        runOnUiThread {
            try {
                val mime = when {
                    suggestedName.endsWith(".png", ignoreCase = true) -> "image/png"
                    suggestedName.endsWith(".tif", ignoreCase = true) ||
                        suggestedName.endsWith(".tiff", ignoreCase = true) -> "image/tiff"
                    suggestedName.endsWith(".webp", ignoreCase = true) -> "image/webp"
                    suggestedName.endsWith(".avif", ignoreCase = true) -> "image/avif"
                    suggestedName.endsWith(".dng", ignoreCase = true) -> "image/x-adobe-dng"
                    suggestedName.endsWith(".pdf", ignoreCase = true) -> "application/pdf"
                    else -> "image/jpeg"
                }
                val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = mime
                    putExtra(Intent.EXTRA_TITLE, suggestedName)
                }
                startActivityForResult(intent, REQ_SAF_CREATE)
            } catch (_: Throwable) {
            }
        }
    }

    fun scanMediaFile(path: String) {
        try {
            MediaScannerConnection.scanFile(applicationContext, arrayOf(path), null, null)
        } catch (_: Throwable) {
        }
    }

    fun openExternalUrl(url: String) {
        runOnUiThread {
            try {
                val uri = Uri.parse(url)
                if (uri.scheme?.lowercase() == "https") {
                    val intent = Intent(Intent.ACTION_VIEW, uri).apply {
                        addCategory(Intent.CATEGORY_BROWSABLE)
                    }
                    startActivity(intent)
                }
            } catch (_: Throwable) {
            }
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        syncPermissionStateToRust()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        syncPermissionStateToRust()
        if (resultCode != Activity.RESULT_OK || data == null) return

        when (requestCode) {
            REQ_SAF_OPEN -> {
                val clip = data.clipData
                if (clip != null && clip.itemCount > 0) {
                    for (i in 0 until clip.itemCount) {
                        clip.getItemAt(i)?.uri?.let { deliverUriToRust(it) }
                    }
                } else {
                    data.data?.let { deliverUriToRust(it) }
                }
            }
            REQ_SAF_CREATE -> {
                val uri = data.data ?: return
                val name = queryDisplayName(uri) ?: "export.jpg"
                val bytes = onSafCreateResult(name)
                if (bytes.isNotEmpty()) {
                    try {
                        contentResolver.openOutputStream(uri, "wt")?.use { out ->
                            out.write(bytes)
                            out.flush()
                        }
                    } catch (_: Throwable) {
                    }
                }
            }
        }
    }

    private fun syncPermissionStateToRust() {
        onPermissionsResult(
            hasReadStoragePermission(),
            hasWriteStoragePermission(),
            hasAllFilesAccess()
        )
    }

    private fun handleIncomingIntent(intent: Intent?) {
        if (intent == null) return
        when (intent.action) {
            Intent.ACTION_VIEW, Intent.ACTION_EDIT -> {
                intent.data?.let { deliverUriToRust(it) }
            }
            Intent.ACTION_SEND -> {
                @Suppress("DEPRECATION")
                val streamUri = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    intent.getParcelableExtra(Intent.EXTRA_STREAM) as? Uri
                }
                streamUri?.let { deliverUriToRust(it) }
            }
            Intent.ACTION_SEND_MULTIPLE -> {
                @Suppress("DEPRECATION")
                val uris: ArrayList<Uri>? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
                } else {
                    intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)
                }
                uris?.forEach { deliverUriToRust(it) }
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
        try {
            val name = queryDisplayName(uri) ?: (uri.lastPathSegment ?: "imported_photo.jpg")
            contentResolver.openInputStream(uri)?.use { input ->
                val bytes = input.readBytes()
                if (bytes.isNotEmpty()) {
                    onFilePickedBytes(name, bytes)
                }
            }
        } catch (_: Throwable) {
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
