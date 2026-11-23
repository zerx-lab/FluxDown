package com.fluxdown.app.ui

import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.NoSuchFileException
import java.nio.file.Path

/** 外部指定的本机目录：只在解析真实路径并排除私有目录后才允许写入探测。 */
internal class ExternalLocalDirectory(
    private val privateRoots: List<File>,
    private val protectedExternalRoot: File? = null,
) {
    sealed interface SilentDestination {
        data object Confirm : SilentDestination
        data object Unwritable : SilentDestination
        data class Ready(val path: String) : SilentDestination
    }

    fun forConfirmation(path: String): String? =
        resolve(path)?.takeUnless(::isPrivate)?.toString()

    fun forSilent(path: String, trustedRoots: Sequence<String>): SilentDestination {
        // 引擎可能在重启后按字符串路径打开目录；用户配置的共享目录不保证届时路径组件未被替换。
        // 静默只接受系统保护的路径，解析结果也必须与该路径的词法规范化结果一致。
        val protectedRoot = protectedExternalRoot?.let { normalizedAbsolute(it.path) }
            ?: return SilentDestination.Confirm
        val expected = normalizedAbsolute(path) ?: return SilentDestination.Unwritable
        if (!expected.startsWith(protectedRoot)) return SilentDestination.Confirm

        val destination = resolve(path) ?: return SilentDestination.Unwritable
        if (destination != expected || isPrivate(destination)) return SilentDestination.Confirm
        if (trustedRoots.none { root ->
                val configured = normalizedAbsolute(root)
                configured != null && destination.startsWith(configured) && resolve(root) == configured
            }
        ) return SilentDestination.Confirm

        val resolved = destination.toString()
        return if (isLocalDirWritable(resolved)) SilentDestination.Ready(resolved) else SilentDestination.Unwritable
    }

    private fun isPrivate(path: Path): Boolean = privateRoots.any { root ->
        val resolved = resolve(root.path) ?: return true
        path.startsWith(resolved)
    }

    private fun normalizedAbsolute(value: String): Path? = try {
        File(value.trim()).toPath().takeIf { it.isAbsolute }?.normalize()
    } catch (_: InvalidPathException) {
        null
    }

    /** 先解析实际存在的祖先，再拼回不存在的目录；不能先折叠 `..`，也不能把悬空链接当作不存在。 */
    private fun resolve(value: String): Path? = try {
        resolveAbsolute(File(value.trim()).toPath())?.takeIf { it.toString().let { path -> path == path.trim() } }
    } catch (_: InvalidPathException) {
        null
    } catch (_: IOException) {
        null
    } catch (_: SecurityException) {
        null
    }

    private fun resolveAbsolute(path: Path): Path? {
        if (!path.isAbsolute) return null
        var prefix = path
        while (true) {
            try {
                val real = prefix.toRealPath()
                return if (prefix == path) real else real.resolve(path.subpath(prefix.nameCount, path.nameCount))
            } catch (_: NoSuchFileException) {
                val name = prefix.fileName?.toString() ?: return null
                if (name == "." || name == ".." || !Files.notExists(prefix, NOFOLLOW_LINKS)) return null
                prefix = prefix.parent ?: return null
            }
        }
    }
}
