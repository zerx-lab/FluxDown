package com.fluxdown.app.feature.downloads

import androidx.compose.runtime.State
import com.fluxdown.app.actions.LocalFileAvailability
import com.fluxdown.app.actions.dispatchLocalFile
import com.fluxdown.app.actions.initialLocalFileAvailability
import com.fluxdown.app.actions.openableLocalFile
import com.fluxdown.app.actions.probeLocalFileAvailability
import com.fluxdown.app.actions.showProbingTaskMenu
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.fluxui.overlay.FluxMenuItem
import java.io.File
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
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

    @Test fun pendingRegularFileMenuEnablesOpenWithoutReplacingDetails() = runBlocking {
        val file = files.newFile("single.iso")
        val task = completed(file)
        var opened: File? = null
        val details = FluxMenuItem.Action("Details", { error("Open must not route to Details") })
        var openJob: Job? = null
        val open = FluxMenuItem.Action("Open", {
            openJob = launch {
                dispatchLocalFile(task, { true }, { error("File must not route to Details") }) { opened = it }
            }
        })
        var items: State<List<FluxMenuItem>>? = null
        showProbingTaskMenu(task, { true }, open, listOf(details)) { pending ->
            items = pending
            assertEquals(listOf(details), pending.value.filterIsInstance<FluxMenuItem.Action>().filter { it.visible })
        }
        val resolved = checkNotNull(items).value
        assertSame(details, resolved[1])
        val action = resolved.first() as FluxMenuItem.Action
        check(action.visible && action.enabled)
        action.onClick()
        checkNotNull(openJob).join()
        assertEquals(file, opened)
    }

    @Test fun pendingDirectoryTapFallsBackToDetailsAndMenuNeverOffersOpen() = runBlocking {
        val task = completed(files.newFolder("multi-file"))
        val destinations = mutableListOf<String>()
        val details = FluxMenuItem.Action("Details", { destinations.add(task.taskId) })
        val open = FluxMenuItem.Action("Open", { error("Directory must not open as a file") })
        var items: State<List<FluxMenuItem>>? = null
        showProbingTaskMenu(task, { true }, open, listOf(details)) { items = it }
        val visible = checkNotNull(items).value.filterIsInstance<FluxMenuItem.Action>().filter { it.visible }
        assertEquals(listOf(details), visible)
        visible.single().onClick()
        dispatchLocalFile(task, { true }, details.onClick) { error("Directory tap must show Details") }
        assertEquals(listOf(task.taskId, task.taskId), destinations)
    }

    private fun assertDetails(task: Task, localHost: Boolean) {
        assertEquals(null, task.openableLocalFile(localHost))
        assertEquals(DownloadRowTap.Details, downloadRowTap(selecting = false, file = task.probeLocalFileAvailability(localHost)))
    }
}
