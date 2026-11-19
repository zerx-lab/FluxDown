package com.fluxdown.app.feature.newtask

import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshots.Snapshot
import com.fluxdown.app.ExternalTorrentGuide
import com.fluxdown.core.host.HostErrorCode
import com.fluxdown.core.host.HostException
import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PublicDownloadGuideTest {
    private val appDir = "/storage/emulated/0/Android/data/com.fluxdown.app/files/Download"
    private val local = HostRef.Local("Android")
    private val ready = HostState(connection = Connection.Live, config = mapOf("default_save_dir" to appDir))

    @Test fun partialTaskTableDoesNotSuppressGuide() {
        val torrent = Task("1", "torrent-file://test", "", "test.iso", appDir, TaskStatus.Completed, 1, 1)
        val loading = ready.copy(tasksLoading = true, taskTotal = 2)
        assertTrue(loading.needsPublicDownloadGuide(local, done = false, appDir = appDir))
        assertTrue(loading.copy(tasks = listOf(torrent)).needsPublicDownloadGuide(local, done = false, appDir = appDir))
    }

    @Test fun trailingSlashesDoNotMakeDefaultDirectoryCustom() {
        val trailingSlash = ready.copy(config = mapOf("default_save_dir" to "$appDir/"))
        assertTrue(trailingSlash.needsPublicDownloadGuide(local, done = false, appDir = appDir))
        assertTrue(ready.needsPublicDownloadGuide(local, done = false, appDir = "$appDir/"))
    }

    @Test fun surroundingWhitespaceDoesNotMakeDefaultDirectoryCustom() {
        val padded = ready.copy(config = mapOf("default_save_dir" to " $appDir/ \t"))
        assertTrue(padded.needsPublicDownloadGuide(local, done = false, appDir = " $appDir \n"))
    }

    @Test fun completedGuideAndCustomDefaultAreNotOverwritten() {
        assertFalse(ready.needsPublicDownloadGuide(local, done = true, appDir = appDir))
        val custom = ready.copy(config = mapOf("default_save_dir" to "/storage/emulated/0/My Downloads"))
        assertFalse(custom.needsPublicDownloadGuide(local, done = false, appDir = appDir))
    }

    @Test fun guideRequiresALiveLocalHost() {
        val remote = HostRef.Remote("nas", "NAS", "http://nas.test")
        assertFalse(ready.needsPublicDownloadGuide(remote, done = false, appDir = appDir))
        assertFalse(ready.copy(connection = Connection.Connecting).needsPublicDownloadGuide(local, done = false, appDir = appDir))
    }

    @Test fun skipDoesNotMarkGuideDoneAfterHostConnectionOrDefaultChanges() {
        var finished = false
        val remote = HostRef.Remote("nas", "NAS", "http://nas.test")
        ready.skipPublicDownloadGuide(remote, false, appDir) { finished = true }
        ready.copy(connection = Connection.Connecting).skipPublicDownloadGuide(local, false, appDir) { finished = true }
        ready.copy(config = mapOf("default_save_dir" to "/storage/emulated/0/Download"))
            .skipPublicDownloadGuide(local, false, appDir) { finished = true }
        assertFalse(finished)
        ready.skipPublicDownloadGuide(local, false, appDir) { finished = true }
        assertTrue(finished)
    }

    @Test(timeout = 5_000)
    fun externalTorrentWaitsForSuccessfulSaveAndLiveHost() = runBlocking {
        val publicDir = "/storage/emulated/0/Download"
        val hostState = mutableStateOf(ready)
        val done = mutableStateOf(false)
        val guide = ExternalTorrentGuide(appDir, { done.value }, { done.value = true })
        val submitted = mutableListOf<String>()
        guide.saving = true
        val pendingImport = launch(start = CoroutineStart.UNDISPATCHED) {
            guide.submitWhenReady({ hostState.value }, { local }) { submitted.add(it) }
        }
        Snapshot.withMutableSnapshot { guide.dismiss(ready, local) }
        assertFalse(done.value)
        Snapshot.withMutableSnapshot { guide.saving = false } // Save failed.
        yield()
        assertTrue(guide.canChooseDirectory(ready, local))
        assertTrue(submitted.isEmpty())

        Snapshot.withMutableSnapshot {
            hostState.value = ready.copy(connection = Connection.Stale)
            guide.selectedDirectory = publicDir
            done.value = true
        }
        yield()
        assertTrue(submitted.isEmpty())
        Snapshot.withMutableSnapshot { hostState.value = ready }
        pendingImport.join()
        assertEquals(listOf(publicDir), submitted)
    }

    @Test(timeout = 5_000)
    fun dismissingConnectedExternalGuideContinuesUsingDefaultWithoutPromptingNextTorrent() = runBlocking {
        val done = mutableStateOf(false)
        val guide = ExternalTorrentGuide(appDir, { done.value }, { done.value = true })
        val submitted = mutableListOf<String>()
        val pendingImport = launch(start = CoroutineStart.UNDISPATCHED) {
            guide.submitWhenReady({ ready }, { local }) { submitted.add(it) }
        }
        assertTrue(submitted.isEmpty())
        Snapshot.withMutableSnapshot { guide.dismiss(ready, local) }
        pendingImport.join()
        assertEquals(listOf(""), submitted)
        assertFalse(ready.needsPublicDownloadGuide(local, done.value, appDir))
    }

    @Test(timeout = 5_000)
    fun disconnectThenDismissKeepsTorrentPendingAndSubmitsOnceAfterReconnect() = runBlocking {
        val done = mutableStateOf(false)
        var skips = 0
        val hostState = mutableStateOf(ready)
        val guide = ExternalTorrentGuide(appDir, { done.value }, { skips++; done.value = true })
        val submitted = mutableListOf<String>()
        val pendingImport = launch(start = CoroutineStart.UNDISPATCHED) {
            guide.submitWhenReady({ hostState.value }, { local }) { submitted.add(it) }
        }
        assertTrue(guide.canChooseDirectory(ready, local))

        val failed = ready.copy(connection = Connection.Failed(HostException(HostErrorCode.Unavailable)))
        Snapshot.withMutableSnapshot { hostState.value = failed }
        yield()
        assertFalse(guide.canChooseDirectory(failed, local))
        assertFalse(guide.resolved)
        Snapshot.withMutableSnapshot {
            guide.dismiss(failed, local)
            guide.dismiss(failed, local)
        }
        yield()
        assertTrue(guide.resolved)
        assertEquals(1, skips)
        assertTrue(submitted.isEmpty())

        Snapshot.withMutableSnapshot { hostState.value = ready }
        pendingImport.join()
        Snapshot.withMutableSnapshot { hostState.value = failed }
        Snapshot.withMutableSnapshot { hostState.value = ready }
        yield()
        assertEquals(listOf(""), submitted)
        assertFalse(ready.needsPublicDownloadGuide(local, done.value, appDir))
    }

    @Test(timeout = 5_000)
    fun externalTorrentUsesHostDefaultWhenGuideNoLongerApplies() = runBlocking {
        val remote = HostRef.Remote("nas", "NAS", "http://nas.test")
        val publicDir = "/storage/emulated/0/Download"
        val guide = ExternalTorrentGuide(appDir, { false }, { error("No skip requested") })
        val submitted = mutableListOf<String>()
        guide.selectedDirectory = publicDir
        guide.submitWhenReady({ ready }, { remote }) { submitted.add(it) }
        guide.selectedDirectory = null
        val custom = ready.copy(config = mapOf("default_save_dir" to publicDir))
        guide.submitWhenReady({ custom }, { local }) { submitted.add(it) }
        assertEquals(listOf("", ""), submitted)
    }
}
