package dev.pm.app.model

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

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

    val where: String
        get() = "$project/$scope"

    /** What the alert is titled: the feature, or for `main` its project. */
    val title: String
        get() = if (scope == Snapshot.MAIN) project else scope

    /** What happened, naming the agent when the push does. */
    val text: String
        get() =
            when (kindOf) {
                AttentionKind.Blocked -> agent?.let { "$it is blocked on you" } ?: "Blocked on you"
                AttentionKind.Asking -> agent?.let { "$it is asking" } ?: "An agent is asking"
                AttentionKind.Ready -> "Ready for review"
                AttentionKind.Dead ->
                    agent?.let { "$it stopped running" } ?: "An agent stopped running"
                else -> agent?.let { "$it: $kind" } ?: kind
            }

    /**
     * What a later push replaces rather than adds to: a scope's blocked or ready alert whichever
     * agent it names, and an asking or dying one per agent.
     */
    val key: PushedTransition
        get() =
            when (kindOf) {
                AttentionKind.Asking,
                AttentionKind.Dead -> this
                else -> copy(agent = null)
            }

    /**
     * Whether `snapshot` still shows what this push announced, as pm judges a kind's episode
     * (`attention/transition.rs`): whatever outranks it as the scope's attention. A project the
     * snapshot couldn't read counts as still holding.
     */
    fun holds(snapshot: Snapshot): Boolean {
        val project = snapshot.project(project) ?: return false
        if (project.skipped != null) return true
        val feature = if (scope == Snapshot.MAIN) null else snapshot.feature(this.project, scope)
        if (scope != Snapshot.MAIN && feature == null) return false
        val agents = snapshot.agents(this.project, scope)
        fun anyIn(state: AgentState) = agents.any {
            it.stateOf == state && (agent == null || it.name == agent)
        }
        return when (kindOf) {
            AttentionKind.Blocked -> feature?.progress == "blocked"
            AttentionKind.Ready -> feature?.progress == "ready" || feature?.lifecycle == "approved"
            AttentionKind.Asking -> anyIn(AgentState.Asking)
            AttentionKind.Dead -> anyIn(AgentState.Dead)
            else -> false
        }
    }

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun parse(text: String): PushedTransition? = runCatching {
            json.decodeFromString(serializer(), text)
        }
            .getOrNull()

        fun parse(bytes: ByteArray): PushedTransition? = parse(bytes.decodeToString())

        fun encode(transition: PushedTransition): String =
            json.encodeToString(serializer(), transition)
    }
}

/**
 * What an end push carries (`commands/serve/push.rs`): a scope's need of kind [ended] is over, for
 * [agent] when it names one.
 */
@Serializable
data class PushedEnd(
    val project: String,
    val scope: String,
    val ended: String,
    val agent: String? = null,
) {
    /** Whether `transition` announced the need this ends. */
    fun ends(transition: PushedTransition): Boolean =
        transition.project == project &&
            transition.scope == scope &&
            transition.kind == ended &&
            (agent == null || transition.agent == agent)

    companion object {
        private val json = Json { ignoreUnknownKeys = true }

        fun parse(bytes: ByteArray): PushedEnd? = runCatching {
            json.decodeFromString(serializer(), bytes.decodeToString())
        }
            .getOrNull()
    }
}
