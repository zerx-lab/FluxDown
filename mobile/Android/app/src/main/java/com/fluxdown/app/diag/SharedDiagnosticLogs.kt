package com.fluxdown.app.diag

import android.content.Context
import android.net.Uri
import android.os.FileObserver
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.provider.DocumentsContract
import android.util.Log
import com.fluxdown.bridge.FluxBridge
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow

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
    private val hostDirty = AtomicBoolean()
    private val agentDirty = AtomicBoolean(true)
    private val engineDirty = AtomicBoolean(true)
    private val observers = mutableListOf<FileObserver>()
    private val failedDocuments = mutableSetOf<String>()
    private val retries = mutableMapOf<String, Retry>()
    private val mutableStatus = MutableStateFlow(Status.NeedsPermission)
    val status = mutableStatus.asStateFlow()

    enum class Status { NeedsPermission, Available, Unavailable }

    private data class Retry(val at: Long, val task: Runnable)

    fun refresh() {
        handler.post { updateStatus() }
    }

    fun publishNow(reauthorized: Boolean) {
        handler.post {
            if (reauthorized) {
                clearFailures()
                hostDirty.set(true)
                agentDirty.set(true)
                engineDirty.set(true)
            }
            publishAll()
        }
    }

    fun publishCrash() {
        if (Thread.currentThread() === worker) return
        val finished = CountDownLatch(1)
        if (!handler.postAtFrontOfQueue {
                try {
                    // FileObserver may not have delivered the final native write yet.
                    agentDirty.set(true)
                    engineDirty.set(true)
                    publishAll()
                } finally {
                    finished.countDown()
                }
            }) return
        try {
            finished.await(1, TimeUnit.SECONDS)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
    }

    private fun publishAll() {
        publishHost()
        publishNative()
    }

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
                if (path?.endsWith(".log") == true) {
                    (if (engine) engineDirty else agentDirty).set(true)
                    scheduleNativePublish(engine)
                }
            }
        }.apply { startWatching() }

    private fun scheduleNativePublish(engine: Boolean) {
        val scheduled = if (engine) engineScheduled else agentScheduled
        if (!scheduled.compareAndSet(false, true)) return
        handler.postDelayed({
            scheduled.set(false)
            publishNative(engine)
        }, PUBLISH_INTERVAL_MS)
    }

    private fun publishNative() {
        publishNative(engine = false)
        publishNative(engine = true)
    }

    private fun publishNative(engine: Boolean) {
        val dirty = if (engine) engineDirty else agentDirty
        publish(if (engine) "engine.txt" else "agent.txt", dirty) {
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
            snapshotDiagnosticLogs(files)
        }
    }

    fun publishHostLater(dir: File) {
        hostDir = dir
        hostDirty.set(true)
        scheduleHostPublish()
    }

    private fun scheduleHostPublish() {
        if (!hostScheduled.compareAndSet(false, true)) return
        handler.postDelayed({
            hostScheduled.set(false)
            publishHost()
        }, PUBLISH_INTERVAL_MS)
    }

    private fun publishHost() {
        publish("app-events.txt", hostDirty) {
            val dir = hostDir ?: return@publish null
            snapshotDiagnosticLogs(listOf(File(dir, "app-events.log.1"), File(dir, "app-events.log")))
        }
    }

    private fun clearFailures() {
        for (retry in retries.values) handler.removeCallbacks(retry.task)
        retries.clear()
        failedDocuments.clear()
    }

    private fun publish(name: String, dirty: AtomicBoolean, content: () -> ByteArray?) {
        val retry = retries[name]
        if (retry != null && SystemClock.uptimeMillis() < retry.at) return
        if (!dirty.getAndSet(false)) return
        if (retry != null) {
            handler.removeCallbacks(retry.task)
            retries.remove(name)
        }
        try {
            val tree = authorizedTree(context)
            if (tree == null) {
                clearFailures()
                mutableStatus.value = Status.NeedsPermission
                return
            }
            content()?.let { writeDocument(tree, name, it) }
            failedDocuments.remove(name)
            mutableStatus.value = if (failedDocuments.isEmpty()) Status.Available else Status.Unavailable
        } catch (error: IOException) {
            deferRetry(name, dirty, content)
            Log.w(TAG, "Cannot publish $name to the shared log folder", error)
        } catch (error: RuntimeException) {
            deferRetry(name, dirty, content)
            // A stopped/unmounted provider must not prevent startup or replace the original crash.
            Log.w(TAG, "Shared log folder unavailable for $name", error)
        }
    }

    private fun deferRetry(name: String, dirty: AtomicBoolean, content: () -> ByteArray?) {
        dirty.set(true)
        failedDocuments.add(name)
        mutableStatus.value = Status.Unavailable
        val retry = Retry(SystemClock.uptimeMillis() + FAILURE_BACKOFF_MS, Runnable {
            publish(name, dirty, content)
        })
        retries[name] = retry
        handler.postAtTime(retry.task, retry.at)
    }

    private fun updateStatus() {
        mutableStatus.value = try {
            if (authorizedTree(context) == null) {
                clearFailures()
                Status.NeedsPermission
            } else if (failedDocuments.isEmpty()) {
                Status.Available
            } else {
                Status.Unavailable
            }
        } catch (_: RuntimeException) {
            Status.Unavailable
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

        promoteDiagnosticLog(staged, documents[name], documents[previousName], name, previousName, ::delete, ::rename)
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
        private const val PUBLISH_INTERVAL_MS = 20_000L
        private const val FAILURE_BACKOFF_MS = 60_000L
        private val ENGINE_NAME = Regex("fluxdown_\\d{4}-\\d{2}-\\d{2}(?:\\.\\d+)?\\.log")
    }
}

/** Preserve one complete copy even if promotion fails after moving the current copy aside. */
internal fun <Document> promoteDiagnosticLog(
    staged: Document,
    current: Document?,
    previous: Document?,
    name: String,
    previousName: String,
    delete: (Document) -> Unit,
    rename: (Document, String) -> Unit,
) {
    if (current != null) {
        previous?.let(delete)
        rename(current, previousName)
    }
    rename(staged, name)
}

internal fun snapshotDiagnosticLogs(files: List<File>, maxTailBytes: Int = 256 * 1024): ByteArray? {
    val out = java.io.ByteArrayOutputStream()
    val newline = '\n'.code.toByte()
    for (file in files) {
        try {
            RandomAccessFile(file, "r").use { input ->
                val length = input.length()
                val start = (length - maxTailBytes).coerceAtLeast(0)
                input.seek((start - 1).coerceAtLeast(0))
                val startsOnLine = start == 0L || input.readByte() == newline
                val bytes = ByteArray((length - start).toInt())
                input.readFully(bytes)
                val first = if (startsOnLine) 0 else bytes.indexOf(newline)
                    .let { if (it >= 0) it + 1 else bytes.size }
                val end = bytes.lastIndexOf(newline) + 1
                if (end > first) out.write(bytes, first, end - first)
            }
        } catch (_: IOException) {
            // Missing rotations and files truncated during the read are skipped.
        }
    }
    return if (out.size() == 0) null else out.toByteArray()
}
