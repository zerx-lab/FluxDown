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
    private val root: File get() = temporary.root.canonicalFile

    @Test fun acceptsTrustedRootAndDescendantsInsideProtectedStorage() {
        val protected = temporary.newFolder("app-files").canonicalFile
        val default = File(protected, "Download")
        val category = File(protected, "Video")
        val last = File(protected, "Last")
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")), protected)
        val roots = sequenceOf(default.path, category.path, last.path)
        for (path in listOf(default.path, File(category, "nested/2026").path, File(last, "nested").path)) {
            val ready = policy.forSilent(path, roots) as SilentDestination.Ready
            assertEquals(File(path).toPath().toRealPath().toString(), ready.path)
            assertTrue(File(ready.path).isDirectory)
            assertEquals(emptyList<File>(), File(ready.path).listFiles()!!.toList())
        }
    }

    @Test fun outsideRootAndSymlinkEscapesConfirmWithoutPreparingTheDirectory() {
        val root = temporary.newFolder("Download").canonicalFile
        val outside = temporary.newFolder("Download-other").canonicalFile
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")), this.root)
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
        val policy = ExternalLocalDirectory(listOf(data, deviceData), root)
        val alias = Files.createSymbolicLink(File(root, "private-alias").toPath(), data.toPath())

        for (path in listOf(data.canonicalPath, "$alias/files/new", File(deviceData.canonicalFile, "no_backup/new").path)) {
            assertEquals(SilentDestination.Confirm, policy.forSilent(path, sequenceOf(root.path)))
            assertNull(policy.forConfirmation(path))
        }
        assertFalse(File(data, "files").exists())
        assertFalse(File(deviceData, "no_backup").exists())
    }

    @Test fun trustedButUnwritableDestinationFallsBackAndDanglingSymlinkCannotCreateTarget() {
        val root = temporary.newFolder("Download").canonicalFile
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")), root)
        val file = File(root, "not-a-directory").apply { writeText("preserve") }
        val missing = File(temporary.root, "outside")
        val dangling = Files.createSymbolicLink(File(root, "dangling").toPath(), missing.toPath())

        assertEquals(SilentDestination.Unwritable, policy.forSilent(file.path, sequenceOf(root.path)))
        assertEquals("preserve", file.readText())
        assertEquals(SilentDestination.Unwritable, policy.forSilent("$dangling/new", sequenceOf(root.path)))
        assertNull(policy.forConfirmation("$dangling/new"))
        assertFalse(missing.exists())
    }

    @Test fun configuredSharedDirectoryRequiresConfirmationBeforeAndAfterReplacement() {
        val shared = temporary.newFolder("shared").canonicalFile
        val protected = temporary.newFolder("app-files").canonicalFile
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")), protected)
        val requested = File(shared, "new")
        val configured = sequenceOf(shared.path, protected.path)

        assertEquals(SilentDestination.Confirm, policy.forSilent(requested.path, configured))
        assertFalse(requested.exists())
        assertEquals(requested.path, policy.forConfirmation(requested.path))

        Files.move(shared.toPath(), File(root, "moved").toPath())
        Files.createSymbolicLink(shared.toPath(), protected.toPath())
        assertEquals(SilentDestination.Confirm, policy.forSilent(requested.path, configured))
        assertFalse(File(protected, "new").exists())
    }

    @Test fun noProtectedStorageRequiresConfirmationEvenForConfiguredTarget() {
        val configured = temporary.newFolder("Download").canonicalFile
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")))
        val requested = File(configured, "new")

        assertEquals(SilentDestination.Confirm, policy.forSilent(requested.path, sequenceOf(configured.path)))
        assertEquals(requested.path, policy.forConfirmation(requested.path))
        assertFalse(requested.exists())
    }

    @Test fun redirectedProtectedRootCannotAuthorizeWrites() {
        val protected = temporary.newFolder("app-files").canonicalFile
        val outside = temporary.newFolder("outside").canonicalFile
        val policy = ExternalLocalDirectory(listOf(temporary.newFolder("private")), protected)
        val requested = File(protected, "new")
        Files.move(protected.toPath(), File(root, "moved").toPath())
        Files.createSymbolicLink(protected.toPath(), outside.toPath())

        assertEquals(SilentDestination.Confirm, policy.forSilent(requested.path, sequenceOf(root.path)))
        assertFalse(File(outside, "new").exists())
    }
}
