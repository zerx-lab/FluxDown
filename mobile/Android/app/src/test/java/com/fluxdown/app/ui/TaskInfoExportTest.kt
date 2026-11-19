package com.fluxdown.app.ui

import com.fluxdown.app.feature.newtask.batchExportName
import com.fluxdown.app.feature.newtask.parseEntries
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class TaskInfoExportTest {
    @get:Rule val temp = TemporaryFolder()

    @Test
    fun writesOrderedUtf8RowsWithUnknownFieldsBlank() {
        val batch = parseEntries("https://example.com/a%20b.zip?key=v\nftp://example.com/f\n  out=报告.zip")
        val rows = listOf(
            TaskInfoRow("=报告\\x\tpart\r\n2", "https://example.com/original", 4294967296L),
            TaskInfoRow("file.bin", "torrent-file://local"),
        ) + batch.map { TaskInfoRow(it.batchExportName(), it.url) }
        val file = temp.newFile("fluxdown-tasks.txt")
        writeTaskInfo(file.outputStream(), taskInfoText(rows))
        assertEquals(
            "Name\tURL\tSize (bytes)\n=报告\\\\x\\tpart\\r\\n2\thttps://example.com/original\t4294967296" +
                "\nfile.bin\t\t\na b.zip\thttps://example.com/a%20b.zip?key=v\t\n报告.zip\tftp://example.com/f\t",
            file.readText(Charsets.UTF_8),
        )
    }
}
