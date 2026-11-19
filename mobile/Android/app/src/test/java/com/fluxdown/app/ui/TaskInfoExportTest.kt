package com.fluxdown.app.ui

import com.fluxdown.app.feature.downloads.REMOTE_ID_PREFIX
import com.fluxdown.app.feature.downloads.selectedTaskInfoRows
import com.fluxdown.app.feature.newtask.UrlEntry
import com.fluxdown.app.feature.newtask.batchExportName
import com.fluxdown.app.feature.newtask.batchExportSize
import com.fluxdown.app.feature.newtask.parseEntries
import com.fluxdown.core.model.Task
import com.fluxdown.core.model.TaskStatus
import com.fluxdown.core.protocol.RemoteTaskDto
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class TaskInfoExportTest {
    @get:Rule val temp = TemporaryFolder()

    @Test
    fun exportsMixedAndRemoteOnlySelectionsInListOrder() {
        val local = Task(
            taskId = "same-id",
            url = "https://example.com/resolved",
            originUrl = "https://example.com/original",
            fileName = "local.bin",
            saveDir = "",
            status = TaskStatus.Pending,
            downloadedBytes = 0,
            totalBytes = 0,
        )
        val remote = RemoteTaskDto("same-id", fileName = "remote.bin", url = "https://example.com/remote", totalBytes = 42)
        val hidden = RemoteTaskDto("hidden", fileName = "hidden.bin")
        val visibleIds = listOf(REMOTE_ID_PREFIX + remote.id, local.taskId)
        val header = "Name\tURL\tSize (bytes)"
        val remoteLine = "\nremote.bin\thttps://example.com/remote\t42"

        assertEquals(
            "$header$remoteLine\nlocal.bin\thttps://example.com/original\t\nhidden.bin\t\t",
            taskInfoText(selectedTaskInfoRows(listOf(local), listOf(hidden, remote), visibleIds)),
        )
        assertEquals(
            "$header$remoteLine",
            taskInfoText(selectedTaskInfoRows(emptyList(), listOf(remote), visibleIds)),
        )
    }

    @Test
    fun writesOrderedUtf8RowsWithUnknownFieldsBlank() {
        val empty = "ed2k://|file|empty.txt|0|31D6CFE0D16AE931B73C59D7E0C089C0|/"
        val wrongHash = "ed2k://|file|invalid.txt|0|1555B7DCA052B5958EE68DB58A42408D|/"
        val batch = parseEntries(
            "https://example.com/a%20b.zip?key=v\nftp://example.com/f\n  out=报告.zip\nhttps://example.com/" +
                "\nMAGNET:?xt=urn:btih:a&dn=My+Show%21\nED2K://|FILE|f%20x.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/" +
                "\nmagnet:?xt=urn:btih:b\ned2k://|file||100|h|/" +
                "\ned2k://|file|ignored|100|1555B7DCA052B5958EE68DB58A42408D|/\n  out=keep%20name" +
                "\n$empty\n$wrongHash",
        )
        val rows = listOf(
            TaskInfoRow("=报告\\x\tpart\r\n2", "https://example.com/original", 4294967296L),
            TaskInfoRow("file.bin", "torrent-file://local"),
        ) + batch.map { TaskInfoRow(it.batchExportName(), it.url, it.batchExportSize()) }
        val file = temp.newFile("fluxdown-tasks.txt")
        writeTaskInfo(file.outputStream(), taskInfoText(rows))
        assertEquals(
            "Name\tURL\tSize (bytes)\n=报告\\\\x\\tpart\\r\\n2\thttps://example.com/original\t4294967296" +
                "\nfile.bin\t\t\na b.zip\thttps://example.com/a%20b.zip?key=v\t\n报告.zip\tftp://example.com/f\t" +
                "\n\thttps://example.com/\t\nMy+Show!\tMAGNET:?xt=urn:btih:a&dn=My+Show%21\t" +
                "\nf x.iso\tED2K://|FILE|f%20x.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/\t5044211712\n\tmagnet:?xt=urn:btih:b\t" +
                "\n\ted2k://|file||100|h|/\t\nkeep%20name\ted2k://|file|ignored|100|1555B7DCA052B5958EE68DB58A42408D|/\t100" +
                "\nempty.txt\t$empty\t0\ninvalid.txt\t$wrongHash\t",
            file.readText(Charsets.UTF_8),
        )
    }

    @Test
    fun batchNameSurvivesUrlsThatJavaUriRejects() {
        val entry = UrlEntry("https://example.com/dir/a b.zip?x=1")
        assertEquals("a b.zip", entry.batchExportName())
    }
}
