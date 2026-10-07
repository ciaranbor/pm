package dev.pm.app.model

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** A feature's details (`GET features/{project}/{feature}`): `pm feat info`'s fields. */
@Serializable
data class FeatureInfo(
    val name: String = "",
    val progress: String = "",
    val lifecycle: String = "",
    val branch: String = "",
    val base: String = "",
    val divergence: Divergence? = null,
    val remote: String? = null,
    @SerialName("rebase_in_progress") val rebaseInProgress: Boolean = false,
    val pr: String? = null,
    val workflow: WorkflowInfo? = null,
    val context: String? = null,
    @SerialName("has_summary") val hasSummary: Boolean = false,
    val created: String? = null,
    @SerialName("last_active") val lastActive: String? = null,
) {
    /** The details as label and value, times in `zone`. */
    fun rows(zone: ZoneId = ZoneId.systemDefault()): List<Pair<String, String>> =
        listOfNotNull(
            "Status" to progressLabel(progress),
            "Branch" to branch,
            ("Rebase" to "in progress").takeIf { rebaseInProgress },
            "Remote" to (remote ?: "none"),
            "Base" to base,
            pr?.let { "PR" to listOfNotNull("#$it", prLabel(lifecycle)).joinToString(" ") },
            divergence?.let { "Divergence" to "$it $base" },
            workflow?.let {
                "Workflow" to listOfNotNull(it.name, it.description).joinToString(" — ")
            },
            created?.let { "Created" to stamp(it, zone) },
            lastActive?.let { "Last active" to stamp(it, zone) },
        )
}

private val STAMP = DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)

/** An RFC 3339 time in `zone`, in the phone's format; one that doesn't parse, as it came. */
private fun stamp(time: String, zone: ZoneId): String = runCatching {
    STAMP.withZone(zone).format(Instant.parse(time))
}
    .getOrDefault(time)

@Serializable
data class Divergence(val ahead: Int = 0, val behind: Int = 0) {
    override fun toString(): String =
        when {
            ahead == 0 && behind == 0 -> "up to date with"
            behind == 0 -> "$ahead ahead of"
            ahead == 0 -> "$behind behind"
            else -> "$ahead ahead, $behind behind"
        }
}

@Serializable data class WorkflowInfo(val name: String = "", val description: String? = null)
