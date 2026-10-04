package dev.pm.app.model

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** What `pm serve pair`'s QR code carries (`commands/serve_pair.rs`). */
@Serializable
data class Pairing(val url: String, val device: String, val token: String) {
    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        /**
         * The pairing `text` encodes: the QR code's JSON, or the `url:`, `device:` and `token:`
         * lines `pm serve pair` prints. `null` when it is no pairing.
         */
        fun parse(text: String): Pairing? {
            val pairing =
                runCatching { json.decodeFromString(serializer(), text.trim()) }.getOrNull()
                    ?: printed(text)
                    ?: return null
            val url = pairing.url.trim().trimEnd('/')
            val valid =
                (url.startsWith("https://") || url.startsWith("http://")) &&
                    pairing.device.isNotBlank() &&
                    pairing.token.isNotBlank()
            return if (valid) pairing.copy(url = url) else null
        }

        private fun printed(text: String): Pairing? {
            val fields =
                text
                    .lines()
                    .mapNotNull { line ->
                        val parts = line.split(":", limit = 2)
                        if (parts.size == 2) parts[0].trim() to parts[1].trim() else null
                    }
                    .toMap()
            return Pairing(
                fields["url"] ?: return null,
                fields["device"] ?: return null,
                fields["token"] ?: return null,
            )
        }

        fun encode(pairing: Pairing): String = json.encodeToString(serializer(), pairing)
    }
}
