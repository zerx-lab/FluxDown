package com.fluxdown.app.feature.newtask

import com.fluxdown.core.capture.ExternalDownload
import com.fluxdown.core.protocol.CustomCategoryDto
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.jsonObject
import com.fluxdown.core.store.HostState
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

    private fun hostWithCategories() = HostState(
        config = mapOf("default_save_dir" to "/dl"),
        sections = mapOf(HostSection.agentPreferences to jsonObject(
            "revision" to 1,
            "values" to mapOf(CustomCategoryDto.PREFERENCE_KEY to CustomCategoryDto.preferenceValue(categories)),
        ).toJson()),
    )

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

        form.setSaveDirManually("/custom")
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/custom", form.saveDir)
        assertNull(requestOffered(form))
    }

    @Test fun manuallyChoosingDefaultOrCategoryDirectoryDisablesRoutingUntilAutoSelected() {
        val form = form("https://x.test/a.exe")

        form.setSaveDirManually("/dl")
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/dl", form.saveDir)
        assertNull(requestOffered(form))

        form.setSaveDirManually("/dl/Video")
        form.rename = "movie.mp4"
        form.syncAuto()
        assertEquals("/dl/Video", form.saveDir)
        assertNull(requestOffered(form))

        // 回到自动：按当前文件名（movie.mp4）重新推导分类目录。
        form.selectMode(SaveDirMode.Auto)
        assertEquals("/dl/Video", form.saveDir)
        assertEquals("/dl", requestOffered(form))
    }

    @Test fun manuallyChosenOrdinaryDirectoryDoesNotFollowLaterRename() {
        val form = form("https://x.test/a.exe")
        form.setSaveDirManually("/custom")
        form.rename = "movie.mp4"
        form.syncAuto()
        assertEquals("/custom", form.saveDir)
        assertNull(requestOffered(form))
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
        form.setSaveDirManually("/dl")
        form.rename = "movie.mp4"
        form.syncAuto()
        assertEquals("/dl", form.saveDir)
        assertNull(requestOffered(form))
        assertEquals(true, form.isDirty)

        // 改成另一个分类目录同样视为手改
        form.setSaveDirManually("/dl/Video")
        form.rename = "setup.exe"
        form.syncAuto()
        assertEquals("/dl/Video", form.saveDir)
        assertNull(requestOffered(form))
    }
    @Test fun noPickerOptionsKeepDefaultDirectoryRoutingAuthorized() {
        val form = NewDownloadState(
            prefill = "https://x.test/a.exe",
            defaultSaveDir = "/dl",
            offeredSaveDir = "/dl",
            defaultQueueId = "",
            defaultSegments = 0,
            categories = emptyList(),
        )

        assertEquals(emptyList<CustomCategoryDto>(), form.categoryOptions)
        assertEquals("/dl", requestOffered(form))
    }

    @Test fun rememberedModeRestoresDirectoryAndInvalidCategoryFallsBackToAuto() {
        val category = form("https://x.test/movie.mp4").let {
            NewDownloadState(
                prefill = "https://x.test/movie.mp4", defaultSaveDir = "/dl", offeredSaveDir = "/dl",
                defaultQueueId = "", defaultSegments = 0, categories = categories,
                rememberedMode = "category:builtin_video",
            )
        }
        assertEquals(SaveDirMode.Category("builtin_video"), category.mode)
        assertEquals("/dl/Video", category.saveDir)
        assertNull(requestOffered(category))
        assertEquals("category:builtin_video", category.rememberedMode())

        val none = NewDownloadState(
            prefill = "", defaultSaveDir = "/dl", offeredSaveDir = "/dl",
            defaultQueueId = "", defaultSegments = 0, categories = categories, rememberedMode = "none",
        )
        assertEquals(SaveDirMode.None, none.mode)
        assertEquals("/dl", none.saveDir)
        assertEquals(SaveDirMode.Auto, NewDownloadState.restoreMode("category:gone", categories))
        assertEquals(SaveDirMode.Auto, NewDownloadState.restoreMode("invalid", categories))
    }

    @Test fun adoptingPublicDefaultUpdatesCurrentRequestWithoutDisablingRouting() {
        val state = form("https://example.com/movie.mp4")
        state.adoptDefaultSaveDir("/public/Download")

        assertEquals("/dl/Video", state.saveDir)
        assertEquals("/public/Download", requestOffered(state))
        state.rename = "unclassified.txt"
        state.syncAuto()
        assertEquals("/public/Download", state.saveDir)
    }

    @Test fun externalAutoStartFallbackKeepsAutomaticCategoryRoutingWithoutDirectoryOverride() {
        val form = NewDownloadState.create(
            "", hostWithCategories(), ExternalDownload("https://x.test/movie.mp4", autoStart = true),
        )

        assertEquals(SaveDirMode.Auto, form.mode)
        assertEquals("/dl/Video", form.saveDir)
        assertEquals("/dl", requestOffered(form))
    }

    @Test fun externalDirectoryOverridesRememberedCategoryOnlyForCurrentForm() {
        val form = NewDownloadState.create(
            "", hostWithCategories(), ExternalDownload("https://x.test/movie.mp4", saveDir = "/custom"),
            rememberedMode = "category:builtin_video",
        )

        assertEquals(SaveDirMode.None, form.mode)
        assertEquals("/custom", form.buildRequests(false, "", "").single().saveDir)
        assertNull(requestOffered(form))
        assertNull(form.rememberedMode())

        form.selectMode(SaveDirMode.Auto)
        assertEquals("auto", form.rememberedMode())
        assertEquals("/dl/Video", form.saveDir)
        assertEquals("/dl", requestOffered(form))
    }

    @Test fun externalNoCategoryKeepsItsDirectoryAndDoesNotOverwriteRememberedMode() {
        val form = NewDownloadState.create(
            "", hostWithCategories(), ExternalDownload("https://x.test/movie.mp4", saveDir = "/custom"),
            rememberedMode = "category:builtin_video",
        )
        fun assertExternalDestination() {
            assertEquals("/custom", form.buildRequests(false, "", "").single().saveDir)
            assertNull(requestOffered(form))
            assertNull(form.rememberedMode())
        }

        form.selectMode(SaveDirMode.None)
        assertExternalDestination()

        form.selectMode(SaveDirMode.Category("builtin_video"))
        assertEquals("/dl/Video", form.buildRequests(false, "", "").single().saveDir)
        assertEquals("category:builtin_video", form.rememberedMode())
        form.selectMode(SaveDirMode.None)
        assertExternalDestination()

        form.setSaveDirManually("/picked")
        form.selectMode(SaveDirMode.None)
        assertEquals("/picked", form.buildRequests(false, "", "").single().saveDir)
        assertNull(form.rememberedMode())
    }

    @Test fun adoptingPublicDefaultPreservesExplicitTaskFolder() {
        val state = form("https://example.com/file.txt")
        state.setSaveDirManually("/other")
        state.adoptDefaultSaveDir("/public/Download")

        assertEquals("/other", state.saveDir)
        assertNull(requestOffered(state))
    }
}
