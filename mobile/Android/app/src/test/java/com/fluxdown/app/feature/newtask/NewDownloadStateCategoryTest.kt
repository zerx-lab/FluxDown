package com.fluxdown.app.feature.newtask

import com.fluxdown.core.protocol.CustomCategoryDto
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class NewDownloadStateCategoryTest {
    private val categories = listOf(
        CustomCategoryDto(
            id = "builtin_program", name = "", extensions = listOf("exe", "msi"), position = 1,
            isBuiltin = true, builtinType = "program", saveDir = "/dl/Programs",
        ),
        CustomCategoryDto(
            id = "builtin_video", name = "", extensions = listOf("mp4"), position = 2,
            isBuiltin = true, builtinType = "video", saveDir = "/dl/Video",
        ),
    )

    private fun form(prefill: String = "") = NewDownloadState(
        prefill = prefill,
        defaultSaveDir = "/dl",
        offeredSaveDir = "/dl",
        defaultQueueId = "",
        defaultSegments = 0,
        categories = categories,
    ).also { it.syncAuto() }

    private fun requestOffered(form: NewDownloadState) =
        form.buildRequests(startPaused = false, queue = "", manualProxy = "").single().offeredSaveDir

    @Test fun autoFollowsFileNameAndStopsAfterManualEdit() {
        val form = form("https://x.test/WeChatWin_4.1.15.exe")
        assertEquals("/dl/Programs", form.saveDir)
        assertEquals("/dl/Programs", form.predictedDir())
        assertNotNull(form.autoMatched())

        form.rename = "movie.mp4"
        form.syncAuto()
        assertEquals("/dl/Video", form.saveDir)

        form.saveDir = "/custom"
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/custom", form.saveDir)
    }

    @Test fun unmatchedNameFallsBackToOffered() {
        val form = form("https://x.test/a.exe")
        form.rename = "notes.pdf"
        form.syncAuto()
        assertNull(form.predictedDir())
        assertEquals("/dl", form.saveDir)
    }

    @Test fun noneAndCategoryModesDoNotRouteOnSubmit() {
        val form = form("https://x.test/a.exe")
        assertEquals("/dl", requestOffered(form))

        form.selectMode(SaveDirMode.None)
        assertEquals("/dl", form.saveDir)
        assertNull(requestOffered(form))

        form.selectMode(SaveDirMode.Category("builtin_video"))
        assertEquals("/dl/Video", form.saveDir)
        assertNull(requestOffered(form))
        // 显式选择优先：文件类型变化不再覆盖
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/dl/Video", form.saveDir)

        form.selectMode(SaveDirMode.Auto)
        assertEquals("/dl/Programs", form.saveDir)
        assertEquals("/dl", requestOffered(form))
    }

    @Test fun manualEditBackToOfferedIsNotOverwrittenAndDoesNotRoute() {
        val form = form("https://x.test/a.exe")
        assertEquals("/dl/Programs", form.saveDir)
        assertEquals("/dl", requestOffered(form))

        // 用户把目录改回默认目录：手改优先，预测输入变化不覆盖，提交不路由
        form.saveDir = "/dl"
        form.rename = "movie.mp4"
        form.syncAuto()
        assertEquals("/dl", form.saveDir)
        assertNull(requestOffered(form))
        assertEquals(true, form.isDirty)

        // 改成另一个分类目录同样视为手改
        form.saveDir = "/dl/Video"
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/dl/Video", form.saveDir)
        assertNull(requestOffered(form))
    }
}
