package com.fluxdown.app.feature.downloads

import com.fluxdown.app.actions.LocalFileAvailability
import com.fluxdown.app.actions.initialLocalFileAvailability
import com.fluxdown.app.actions.openableLocalFile
import com.fluxdown.app.actions.probeLocalFileAvailability
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class DownloadRowTapTest {
    @get:Rule val files = TemporaryFolder()

    private fun completed(path: File) = Task(
        "test", "torrent-file://test", "", path.name, path.parentFile!!.path, TaskStatus.Completed, 1, 1,
    )

    @Test fun completedRegularFileOpensUnlessSelecting() {
        val file = files.newFile("single.iso")
        val openable = completed(file).openableLocalFile(localHost = true)
        assertEquals(file, openable)
        val probed = completed(file).probeLocalFileAvailability(localHost = true)
        assertEquals(DownloadRowTap.TryOpenFile, downloadRowTap(selecting = false, file = probed))
        assertEquals(DownloadRowTap.ToggleSelection, downloadRowTap(selecting = true, file = probed))
    }

    @Test fun tappingBeforeProbeFinishesStillAttemptsToOpenAnEligibleTask() {
        val task = completed(files.newFile("single.iso"))
        val initial = task.initialLocalFileAvailability(localHost = true)
        assertEquals(DownloadRowTap.TryOpenFile, downloadRowTap(selecting = false, file = initial))
        assertEquals(DownloadRowTap.ToggleSelection, downloadRowTap(selecting = true, file = initial))
        assertEquals(DownloadRowTap.Details, downloadRowTap(selecting = false, file = task.initialLocalFileAvailability(false)))
    }

    @Test fun multiFileTorrentContainerShowsDetailsInsteadOfOpening() {
        val folder = files.newFolder("multi-file")
        File(folder, "part.iso").writeText("downloaded file")
        assertDetails(completed(folder), localHost = true)
    }

    @Test fun remoteOrMissingTaskShowsDetailsEvenIfLocalPathMatchesAFile() {
        val task = completed(files.newFile("single.iso"))
        assertDetails(task, localHost = false)
        assertDetails(task.copy(fileMissing = true), localHost = true)
        assertDetails(task.copy(fileName = "deleted.iso"), localHost = true)
    }

    private fun assertDetails(task: Task, localHost: Boolean) {
        assertEquals(null, task.openableLocalFile(localHost))
        assertEquals(DownloadRowTap.Details, downloadRowTap(selecting = false, file = task.probeLocalFileAvailability(localHost)))
    }
}
