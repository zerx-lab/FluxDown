package com.fluxdown.app.diag

import android.content.Context
import android.net.Uri
import android.os.FileObserver
import android.os.Handler
import android.os.HandlerThread
import android.provider.DocumentsContract
import android.util.Log
import com.fluxdown.bridge.FluxBridge
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.util.concurrent.atomic.AtomicBoolean

/** Publishes bounded copies of the already-redacted local logs; never reads legacy raw logs. */
internal class SharedDiagnosticLogs(
    private val context: Context,
    externalRoot: File?,
    privateRoot: File,
) {
    private val nativeRoots = listOfNotNull(externalRoot, privateRoot)
    private val hostScheduled = AtomicBoolean()
    @Volatile private var hostDir: File? = null
    private val worker = HandlerThread("FluxDown shared logs").apply { start() }
    private val handler = Handler(worker.looper)
    private val agentScheduled = AtomicBoolean()
    private val engineScheduled = AtomicBoolean()
    private val observers = mutableListOf<FileObserver>()

    fun watch() {
        handler.post {
            for (root in nativeRoots) {
                for (engine in listOf(false, true)) {
                    val dir = File(root, if (engine) "engine" else "agent")
                    try {
                        if (!dir.isDirectory && !dir.mkdirs() && !dir.isDirectory) continue
                        observers += watchDirectory(dir, engine)
                    } catch (error: SecurityException) {
                        Log.w(TAG, "Cannot watch diagnostic source", error)
                    }
                }
            }
            publishNative()
        }
    }

    @Suppress("DEPRECATION") // The String constructor also supports Android 8–9.
    private fun watchDirectory(dir: File, engine: Boolean): FileObserver =
        object : FileObserver(dir.path, CREATE or MODIFY or CLOSE_WRITE or MOVED_TO) {
            override fun onEvent(event: Int, path: String?) {
                if (path?.endsWith(".log") == true) scheduleNativePublish(engine)
            }
        }.apply { startWatching() }

    private fun scheduleNativePublish(engine: Boolean) {
        val scheduled = if (engine) engineScheduled else agentScheduled
        if (!scheduled.compareAndSet(false, true)) return
        handler.postDelayed({
            scheduled.set(false)
            publishNative(engine)
        }, 750)
    }

    fun publishNative() {
        publishNative(engine = false)
        publishNative(engine = true)
    }

    @Synchronized
    private fun publishNative(engine: Boolean) {
        publish(if (engine) "engine.txt" else "agent.txt") {
            val path = FluxBridge.androidLogDirectory(engine) ?: return@publish null
            val dir = File(path)
            val files = if (engine) {
                dir.listFiles().orEmpty()
                    .filter { it.isFile && ENGINE_NAME.matches(it.name) }
                    .sortedWith(compareBy<File> { it.lastModified() }.thenByDescending { it.name })
                    .takeLast(2)
            } else {
                listOf(File(dir, "agent.log.1"), File(dir, "agent.log"))
            }
            snapshot(files)
        }
    }

    fun publishHostLater(dir: File) {
        hostDir = dir
        if (!hostScheduled.compareAndSet(false, true)) return
        handler.postDelayed({
            hostScheduled.set(false)
            hostDir?.let(::publishHost)
        }, 750)
    }

    @Synchronized
    fun publishHost(dir: File) = publish("app-events.txt") {
        snapshot(listOf(File(dir, "app-events.log.1"), File(dir, "app-events.log")))
    }

    private fun publish(name: String, content: () -> ByteArray?) {
        try {
            val tree = authorizedTree(context) ?: return
            writeDocument(tree, name, content() ?: return)
        } catch (error: IOException) {
            Log.w(TAG, "Cannot publish $name to the shared log folder", error)
        } catch (error: RuntimeException) {
            // A stopped/unmounted provider must not prevent startup or replace the original crash.
            Log.w(TAG, "Shared log folder unavailable for $name", error)
        }
    }

    private fun writeDocument(tree: Uri, name: String, bytes: ByteArray) {
        val resolver = context.contentResolver
        val id = DocumentsContract.getTreeDocumentId(tree)
        val directory = DocumentsContract.buildDocumentUriUsingTree(tree, id)
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id)
        val stagingName = ".$name.pending.txt"
        val previousName = "$name.previous.txt"
        val documents = resolver.query(
            children,
            arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME),
            null, null, null,
        )?.use { cursor ->
            buildMap<String, Uri> {
                while (cursor.moveToNext()) {
                    val displayName = cursor.getString(1)
                    if (displayName == name || displayName == stagingName || displayName == previousName) {
                        put(displayName, DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0)))
                    }
                }
            }
        } ?: throw IOException("cannot list shared log directory")
        fun delete(uri: Uri) {
            if (!DocumentsContract.deleteDocument(resolver, uri)) throw IOException("cannot delete $uri")
        }
        fun rename(uri: Uri, target: String) {
            if (DocumentsContract.renameDocument(resolver, uri, target) == null) {
                throw IOException("cannot rename shared log to $target")
            }
        }

        documents[stagingName]?.let(::delete)
        val staged = DocumentsContract.createDocument(resolver, directory, "text/plain", stagingName)
            ?: throw IOException("cannot create staged shared log")
        val stream = resolver.openOutputStream(staged, "w") ?: throw IOException("cannot open staged shared log")
        stream.use { it.write(bytes) }

        // Keep the last complete copy even if the process dies between the two renames.
        documents[name]?.let { current ->
            documents[previousName]?.let(::delete)
            rename(current, previousName)
        }
        rename(staged, name)
    }

    private fun snapshot(files: List<File>): ByteArray? {
        val out = java.io.ByteArrayOutputStream()
        for (file in files) {
            if (!file.isFile) continue
            try {
                RandomAccessFile(file, "r").use { input ->
                    val length = input.length()
                    val start = (length - MAX_TAIL_BYTES).coerceAtLeast(0)
                    input.seek(start)
                    val bytes = ByteArray((length - start).toInt())
                    input.readFully(bytes)
                    // If truncated, discard the first partial UTF-8 line.
                    val first = if (start == 0L) 0 else
                        bytes.indexOf('\n'.code.toByte()).let { if (it >= 0) it + 1 else bytes.size }
                    out.write(bytes, first, bytes.size - first)
                }
            } catch (error: IOException) {
                Log.w(TAG, "Diagnostic source changed while reading", error)
            }
        }
        return if (out.size() == 0) null else out.toByteArray()
    }

    companion object {
        const val FOLDER = "Documents/FluxDown/Logs"
        private const val PROVIDER = "com.android.externalstorage.documents"

        fun isLogTree(uri: Uri): Boolean = uri.authority == PROVIDER &&
            DocumentsContract.isTreeUri(uri) &&
            DocumentsContract.getTreeDocumentId(uri) == "primary:$FOLDER"

        fun authorizedTree(context: Context): Uri? = context.contentResolver.persistedUriPermissions
            .firstOrNull { it.isReadPermission && it.isWritePermission && isLogTree(it.uri) }?.uri
        private const val TAG = "FluxSharedLogs"
        private const val MAX_TAIL_BYTES = 256 * 1024L
        private val ENGINE_NAME = Regex("fluxdown_\\d{4}-\\d{2}-\\d{2}(?:\\.\\d+)?\\.log")
    }
}
