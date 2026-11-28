package com.fluxdown.core.capture

import com.fluxdown.core.format.UrlText
import com.fluxdown.core.protocol.CloudConnectionDto
import com.fluxdown.core.protocol.CloudPresence
import com.fluxdown.core.protocol.DeviceRules
import com.fluxdown.core.protocol.DispatchTarget
import com.fluxdown.core.protocol.HostSection
import com.fluxdown.core.protocol.Json
import com.fluxdown.core.store.Connection
import com.fluxdown.core.store.HostState
import java.net.URI
import java.net.URISyntaxException

/** Device-local capture rules shared with desktop and iOS: one `extension=targetId` per line. */
object CaptureDeviceRules {
    const val PREFERENCE_KEY = "download.capture_device_rules"

    data class Rule(val extension: String, val targetId: String)

    fun normalizeExtension(input: String): String? = asciiExtension(input.trim().removePrefix("."))

    private fun asciiExtension(suffix: String): String? = suffix.takeIf {
        it.isNotEmpty() && it.all { ch -> ch in 'a'..'z' || ch in 'A'..'Z' || ch in '0'..'9' }
    }?.lowercase()

    fun parse(wire: String): List<Rule> = wire.lineSequence().mapNotNull { line ->
        val extension = normalizeExtension(line.substringBefore('=', "")) ?: return@mapNotNull null
        val target = line.substringAfter('=', "").trim()
        val id = when {
            target.startsWith("cloud:") -> target.removePrefix("cloud:")
            target.startsWith("link:") -> target.removePrefix("link:")
            else -> return@mapNotNull null
        }
        if (id.isEmpty() || id.any { it.isWhitespace() || it.isISOControl() || it == '=' || it == ':' }) return@mapNotNull null
        Rule(extension, target)
    }.toList()

    fun encode(rules: List<Rule>): String = rules.joinToString("\n") { "${it.extension}=${it.targetId}" }

    fun targetId(rules: List<Rule>, download: ExternalDownload): String? {
        if (rules.isEmpty()) return null
        val uri = httpUri(download.url) ?: return null
        val name = download.fileName.trim().ifEmpty {
            UrlText.percentDecode(uri.rawPath.substringAfterLast('/'))
        }
        val dot = name.lastIndexOf('.')
        if (dot <= 0 || '/' in name || '\\' in name) return null
        val extension = asciiExtension(name.substring(dot + 1)) ?: return null
        return rules.firstOrNull { it.extension == extension }?.targetId
    }

    /** Dispatch omits source-page metadata and the browser's UA; don't send credentialed or custom requests. */
    fun canDispatch(download: ExternalDownload): Boolean {
        val uri = httpUri(download.url) ?: return false
        return '@' !in uri.rawAuthority && download.cookies.isBlank() && !download.hasUnusableRequestContext &&
            download.headers.keys.all { it.equals("User-Agent", ignoreCase = true) }
    }

    /** Same roster, presence and preferred transport as the manual new-download destination. */
    fun targets(state: HostState): List<DispatchTarget> {
        val live = state.connection == Connection.Live
        val cloud = CloudConnectionDto.fromJson(Json.parseOrNull(state.sections[HostSection.agentCloudConnection]))
        return DeviceRules.dispatchTargets(state.cloudDevices, state.linkDevices, CloudPresence.isKnown(cloud, live), live)
    }

    private fun httpUri(url: String): URI? {
        val uri = try {
            URI(url)
        } catch (_: URISyntaxException) {
            return null
        }
        return uri.takeIf { !it.rawAuthority.isNullOrEmpty() &&
            (it.scheme.equals("http", ignoreCase = true) || it.scheme.equals("https", ignoreCase = true)) }
    }
}
