package com.fluxdown.app.feature.task

import com.fluxdown.app.actions.initialLocalFileAvailability
import com.fluxdown.app.actions.openableLocalFile
import com.fluxdown.app.actions.probeLocalFileAvailability
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class TaskDetailOpenFileTest {
    @get:Rule val files = TemporaryFolder()

    private fun completed(path: File) = Task(
        "test", "torrent-file://test", "", path.name, files.root.path, TaskStatus.Completed, 1, 1,
    )

    @Test fun multiFileTorrentNeverOffersOpenFileWhileProbingOrAfterward() {
        val folder = files.newFolder("multi-file")
        File(folder, "part.iso").writeText("downloaded file")
        val task = completed(folder)

        assertFalse(detailCanOpenFile(task.initialLocalFileAvailability(localHost = true)))
        assertFalse(detailCanOpenFile(task.probeLocalFileAvailability(localHost = true)))
    }

    @Test fun regularFileOffersOpenFileOnlyAfterProbe() {
        val file = files.newFile("single.iso")
        val task = completed(file)

        assertFalse(detailCanOpenFile(task.initialLocalFileAvailability(localHost = true)))
        assertTrue(detailCanOpenFile(task.probeLocalFileAvailability(localHost = true)))
        assertEquals(file, task.openableLocalFile(localHost = true))
    }
}
