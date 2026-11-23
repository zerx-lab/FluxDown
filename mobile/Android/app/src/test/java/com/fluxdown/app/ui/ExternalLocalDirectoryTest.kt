package com.fluxdown.app.ui

import com.fluxdown.app.ui.ExternalLocalDirectory.SilentDestination
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class ExternalLocalDirectoryTest {
    @get:Rule
    val temporary = TemporaryFolder()

    @Test fun acceptsTrustedRootAndDescendantsAndUsesResolvedPath() {
        val default = temporary.newFolder("Download")
        val category = temporary.newFolder("Video")
        val last = temporary.newFolder("Last")
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")))
        val roots = sequenceOf(default.path, category.path, last.path)
        val alias = Files.createSymbolicLink(File(temporary.root, "default-alias").toPath(), default.toPath())
        for (path in listOf(alias.toString(), File(category, "nested/2026").path, File(last, "nested").path)) {
            val ready = policy.forSilent(path, roots) as SilentDestination.Ready
            assertEquals(File(path).toPath().toRealPath().toString(), ready.path)
            assertTrue(File(ready.path).isDirectory)
            assertEquals(emptyList<File>(), File(ready.path).listFiles()!!.toList())
        }
    }

    @Test fun outsideRootAndSymlinkEscapesConfirmWithoutPreparingTheDirectory() {
        val root = temporary.newFolder("Download")
        val outside = temporary.newFolder("Download-other")
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")))
        val nested = File(outside, "real").apply { check(mkdir()) }
        val alias = Files.createSymbolicLink(File(root, "escape").toPath(), nested.toPath())
        val escapePaths = listOf(File(outside, "new").path, "$alias/../new", File(root, "../Download-other/new").path)

        for (path in escapePaths) {
            assertEquals(SilentDestination.Confirm, policy.forSilent(path, sequenceOf(root.path)))
            assertEquals(File(outside, "new").canonicalPath, policy.forConfirmation(path))
            assertFalse(File(outside, "new").exists())
            assertFalse(File(root, "new").exists())
        }
    }

    @Test fun privateRootsAndTheirAliasesCannotBeWrittenEvenUnderTrustedRoot() {
        val data = temporary.newFolder("data")
        val deviceData = temporary.newFolder("device-data")
        val policy = ExternalLocalDirectory(listOf(data, deviceData))
        val alias = Files.createSymbolicLink(File(temporary.root, "private-alias").toPath(), data.toPath())

        for (path in listOf(data.path, "$alias/files/new", File(deviceData, "no_backup/new").path)) {
            assertEquals(SilentDestination.Confirm, policy.forSilent(path, sequenceOf(temporary.root.path)))
            assertNull(policy.forConfirmation(path))
        }
        assertFalse(File(data, "files").exists())
        assertFalse(File(deviceData, "no_backup").exists())
    }

    @Test fun trustedButUnwritableDestinationFallsBackAndDanglingSymlinkCannotCreateTarget() {
        val root = temporary.newFolder("Download")
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")))
        val file = File(root, "not-a-directory").apply { writeText("preserve") }
        val missing = File(temporary.root, "outside")
        val dangling = Files.createSymbolicLink(File(root, "dangling").toPath(), missing.toPath())

        assertEquals(SilentDestination.Unwritable, policy.forSilent(file.path, sequenceOf(root.path)))
        assertEquals("preserve", file.readText())
        assertEquals(SilentDestination.Unwritable, policy.forSilent("$dangling/new", sequenceOf(root.path)))
        assertNull(policy.forConfirmation("$dangling/new"))
        assertFalse(missing.exists())
    }
}
