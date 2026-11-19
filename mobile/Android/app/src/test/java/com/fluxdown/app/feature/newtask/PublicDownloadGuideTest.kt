package com.fluxdown.app.feature.newtask

import com.fluxdown.core.model.HostRef
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PublicDownloadGuideTest {
    private val appDir = "/storage/emulated/0/Android/data/com.fluxdown.app/files/Download"
    private val local = HostRef.Local("Android")
    private val ready = HostState(connection = Connection.Live, config = mapOf("default_save_dir" to appDir))

    @Test fun existingTorrentDoesNotSuppressGuide() {
        val torrent = Task("1", "torrent-file://test", "", "test.iso", appDir, TaskStatus.Completed, 1, 1)
        assertTrue(ready.copy(tasks = listOf(torrent)).needsPublicDownloadGuide(local, done = false, appDir = appDir))
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
}
