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

/** What a push carries (`commands/serve/push.rs`): a transition, without its detail. */
@Serializable
data class PushedTransition(
    val project: String,
    val scope: String,
    val kind: String,
    val agent: String? = null,
) {
    val kindOf: AttentionKind
        get() = AttentionKind.of(kind)

    /** The notification's line, as the tmux alert words it. */
    val title: String
        get() {
            val where = "$project/$scope"
            return when (kindOf) {
                AttentionKind.Blocked -> "$where is blocked"
                AttentionKind.Asking ->
                    if (agent != null) "$where: $agent is asking" else "$where is asking"
                AttentionKind.Ready -> "$where is ready"
                AttentionKind.Dead ->
                    if (agent != null) "$where: $agent died" else "$where: an agent died"
                else -> "$where: $kind"
            }
        }

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun parse(bytes: ByteArray): PushedTransition? = runCatching {
            json.decodeFromString(serializer(), bytes.decodeToString())
        }
            .getOrNull()
    }
}
