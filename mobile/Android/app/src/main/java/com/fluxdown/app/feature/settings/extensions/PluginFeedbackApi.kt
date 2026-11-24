package com.fluxdown.app.feature.settings.extensions

import com.fluxdown.core.protocol.Json
import com.fluxdown.core.protocol.JsonValue
import com.fluxdown.core.protocol.PluginDto
import com.fluxdown.core.protocol.arrayOrNull
import com.fluxdown.core.protocol.get
import com.fluxdown.core.protocol.jsonObjectOmitNulls
import com.fluxdown.core.protocol.longOrNull
import com.fluxdown.core.protocol.strOrNull
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal const val GENERAL_FEEDBACK_URL = "https://fluxdown.com/feedback"
internal const val FEEDBACK_TITLE_LIMIT = 200
internal const val FEEDBACK_DESCRIPTION_LIMIT = 5000
internal const val FEEDBACK_AUDIT_LIMIT = 20

internal data class PluginFeedbackTarget(val identity: String, val version: String) {
    companion object {
        private val identityFormat = Regex("[a-z0-9_-]+@[a-z0-9_-]+")

        fun forPlugin(plugin: PluginDto): PluginFeedbackTarget? {
            val version = plugin.version.trim()
            if (plugin.devMode || plugin.source == "private" || version.length !in 1..64 ||
                plugin.identity.length > 200 || !identityFormat.matches(plugin.identity)) return null
            return PluginFeedbackTarget(plugin.identity, version)
        }
    }
}

/** Project only bounded counts; never attach audit paths, target URLs, or details. */
internal data class PluginFeedbackAudit(val errors: Int, val denied: Int) {
    companion object {
        fun fromJson(json: JsonValue, identity: String): PluginFeedbackAudit? {
            val entries = json["entries"].arrayOrNull ?: return null
            var errors = 0
            var denied = 0
            for (index in 0 until minOf(entries.size, FEEDBACK_AUDIT_LIMIT)) {
                val entry = entries[index]
                if (entry.strOrNull("identity") != identity) continue
                when (entry.strOrNull("outcome")) {
                    "error" -> errors++
                    "denied" -> denied++
                }
            }
            return PluginFeedbackAudit(errors, denied)
        }
    }
}

internal data class PluginFeedbackReport(
    val target: PluginFeedbackTarget,
    val repository: String,
    val title: String,
    val description: String,
    val appVersion: String,
    val environment: String?,
) {
    fun toJson(): String = jsonObjectOmitNulls(
        "type" to "bug",
        "source" to "app",
        "title" to title,
        "description" to description,
        "appVersion" to appVersion,
        "pluginIdentity" to target.identity,
        "pluginVersion" to target.version,
        "repository" to repository,
        "environment" to environment,
    ).toJson()
}

internal sealed interface PluginFeedbackResult<out T> {
    class Ok<T>(val value: T) : PluginFeedbackResult<T>
    enum class Failure : PluginFeedbackResult<Nothing> { Unavailable, RouteChanged, RateLimited, Failed, Unconfirmed }
}

internal object PluginFeedbackApi {
    private const val ENDPOINT = "https://fluxdown.com/api/feedback"
    private const val TIMEOUT_MS = 15_000
    private const val MAX_RESPONSE_BYTES = 1024
    private val repositoryFormat = Regex("[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,37}[a-zA-Z0-9])?/[a-zA-Z0-9_.-]{1,100}")

    internal fun destinationUrl(identity: String): URL =
        URL("$ENDPOINT?pluginIdentity=${URLEncoder.encode(identity, Charsets.UTF_8.name())}")

    suspend fun destination(target: PluginFeedbackTarget): PluginFeedbackResult<String> =
        request(destinationUrl(target.identity), successStatus = 200, decode = ::decodeDestination)

    suspend fun submit(report: PluginFeedbackReport): PluginFeedbackResult<Long> =
        request(URL(ENDPOINT), successStatus = 201, body = report.toJson().toByteArray(Charsets.UTF_8), decode = ::decodeSubmission)

    internal fun decodeDestination(status: Int, body: String?): PluginFeedbackResult<String> {
        if (status != 200) return failure(status)
        val repository = Json.parseOrNull(body).strOrNull("repository") ?: return PluginFeedbackResult.Failure.Failed
        if (!repositoryFormat.matches(repository) || repository.endsWith("/.") || repository.endsWith("/..")) {
            return PluginFeedbackResult.Failure.Failed
        }
        return PluginFeedbackResult.Ok(repository)
    }

    internal fun decodeSubmission(status: Int, body: String?): PluginFeedbackResult<Long> {
        if (status in 500..599) return PluginFeedbackResult.Failure.Unconfirmed
        if (status != 201) return failure(status)
        val number = Json.parseOrNull(body).longOrNull("issueNumber")
        return if (number != null && number > 0) PluginFeedbackResult.Ok(number) else PluginFeedbackResult.Failure.Unconfirmed
    }

    private fun failure(status: Int): PluginFeedbackResult.Failure = when (status) {
        404 -> PluginFeedbackResult.Failure.Unavailable
        409 -> PluginFeedbackResult.Failure.RouteChanged
        429 -> PluginFeedbackResult.Failure.RateLimited
        else -> PluginFeedbackResult.Failure.Failed
    }

    private suspend fun <T> request(
        url: URL,
        successStatus: Int,
        body: ByteArray? = null,
        decode: (Int, String?) -> PluginFeedbackResult<T>,
    ): PluginFeedbackResult<T> = withContext(Dispatchers.IO) {
        try {
            val connection = url.openConnection() as HttpURLConnection
            try {
                connection.instanceFollowRedirects = false
                connection.useCaches = false
                connection.connectTimeout = TIMEOUT_MS
                connection.readTimeout = TIMEOUT_MS
                connection.setRequestProperty("Accept", "application/json")
                if (body != null) {
                    connection.requestMethod = "POST"
                    connection.doOutput = true
                    connection.setFixedLengthStreamingMode(body.size)
                    connection.setRequestProperty("Content-Type", "application/json")
                    connection.outputStream.use { it.write(body) }
                }
                val status = connection.responseCode
                val response = if (status == successStatus) connection.inputStream.use { it.readSmallBody() } else null
                decode(status, response)
            } finally {
                connection.disconnect()
            }
        } catch (_: IOException) {
            if (body == null) PluginFeedbackResult.Failure.Failed else PluginFeedbackResult.Failure.Unconfirmed
        }
    }

    private fun InputStream.readSmallBody(): String? {
        val bytes = ByteArray(MAX_RESPONSE_BYTES)
        var count = 0
        while (count < bytes.size) {
            val read = read(bytes, count, bytes.size - count)
            if (read == -1) return String(bytes, 0, count, Charsets.UTF_8)
            count += read
        }
        return null
    }
}
