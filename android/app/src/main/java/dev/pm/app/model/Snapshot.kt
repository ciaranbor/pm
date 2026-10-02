package dev.pm.app.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The attention snapshot `pm serve` sends (`pm feat status --all --json`;
 * README, "Attention view"). Kinds and states stay strings on the wire: a
 * newer server may send values this app doesn't know, which read as
 * [AttentionKind.Unknown] / [AgentState.Unknown] rather than failing.
 */
@Serializable
data class Snapshot(
    val version: Int,
    val projects: List<ProjectSnapshot> = emptyList(),
    val features: List<FeatureSnapshot> = emptyList(),
) {
    /** Whether the server speaks a snapshot version this app was built for. */
    val understood: Boolean get() = version == VERSION

    fun project(name: String): ProjectSnapshot? = projects.find { it.name == name }

    fun featuresOf(project: String): List<FeatureSnapshot> = features.filter { it.project == project }

    fun feature(project: String, name: String): FeatureSnapshot? =
        features.find { it.project == project && it.name == name }

    /** The agents of `scope` (a feature, or `main`) in `project`. */
    fun agents(project: String, scope: String): List<AgentSnapshot> =
        if (scope == MAIN) project(project)?.main?.agents.orEmpty()
        else feature(project, scope)?.agents.orEmpty()

    /** How many of `project`'s scopes need each kind, most urgent first. */
    fun attentionCounts(project: String): List<Pair<AttentionKind, Int>> {
        val kinds = featuresOf(project).map { it.attention.kindOf } +
            listOfNotNull(project(project)?.main?.attention?.kindOf)
        return kinds.filter { it != AttentionKind.None }
            .groupingBy { it }.eachCount()
            .toList()
            .sortedBy { it.first.ordinal }
    }

    companion object {
        const val VERSION = 1
        const val MAIN = "main"

        val json = Json {
            ignoreUnknownKeys = true
            explicitNulls = false
        }

        fun parse(text: String): Snapshot = json.decodeFromString(serializer(), text)
    }
}

@Serializable
data class ProjectSnapshot(
    val name: String,
    val root: String = "",
    val skipped: String? = null,
    val main: ScopeSnapshot? = null,
)

@Serializable
data class ScopeSnapshot(
    val session: String = "",
    @SerialName("session_exists") val sessionExists: Boolean = false,
    val agents: List<AgentSnapshot> = emptyList(),
    val attention: Attention = Attention(),
    val working: Boolean = false,
    @SerialName("last_activity") val lastActivity: String? = null,
)

@Serializable
data class FeatureSnapshot(
    val project: String,
    val name: String,
    val attention: Attention = Attention(),
    val progress: String = "",
    @SerialName("blocked_reason") val blockedReason: String? = null,
    @SerialName("blocked_by") val blockedBy: String? = null,
    val summary: String? = null,
    val lifecycle: String = "",
    val pr: String? = null,
    val session: String = "",
    @SerialName("session_exists") val sessionExists: Boolean = false,
    val agents: List<AgentSnapshot> = emptyList(),
    val working: Boolean = false,
    @SerialName("last_activity") val lastActivity: String? = null,
)

@Serializable
data class AgentSnapshot(
    val name: String,
    val state: String = "",
    val unread: Int = 0,
    val window: String? = null,
    val waiting: Waiting? = null,
) {
    val stateOf: AgentState get() = AgentState.of(state)
}

@Serializable
data class Waiting(val kind: String = "", val detail: String = "")

@Serializable
data class Attention(
    val kind: String = "none",
    val detail: String? = null,
    val agent: String? = null,
) {
    val kindOf: AttentionKind get() = AttentionKind.of(kind)
}
