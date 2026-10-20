package dev.pm.app.model

import kotlinx.serialization.Serializable

/**
 * An agent definition a spawn can name, with the harness and model config gives a spawn of it now,
 * or the `error` config gives instead.
 */
@Serializable
data class AgentDefinition(
    val name: String,
    val description: String? = null,
    val harness: String? = null,
    val model: String? = null,
    val error: String? = null,
) {
    /** What a spawn of it launches, as one line: `claude-code · opus`, or why it can't say. */
    val launches: String
        get() = error ?: listOfNotNull(harness, model).joinToString(" · ")
}

/** Whether `name` is one an agent may take: letters, digits, `-` and `_`. */
fun isAgentName(name: String): Boolean =
    name.isNotEmpty() && name.all { it.isLetterOrDigit() || it == '-' || it == '_' }
