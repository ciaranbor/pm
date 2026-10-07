package dev.pm.app.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * A dialog on an agent's screen that can be answered from the phone (`GET …/dialogs`): a question
 * form, a permission prompt or a plan, with the choices its harness's CLI offers.
 */
@Serializable
data class Dialog(
    val id: String,
    val kind: String,
    /** The harness's id for the subagent asking; null for the agent's own dialog. */
    val subagent: String? = null,
    val questions: List<Question> = emptyList(),
    val tool: String? = null,
    val detail: String? = null,
    val plan: String? = null,
    val choices: List<Choice> = emptyList(),
) {
    @Serializable
    data class Question(
        val question: String,
        val header: String = "",
        val options: List<Option> = emptyList(),
        @SerialName("multi_select") val multiSelect: Boolean = false,
        /** Whether the user's own words are taken as an answer. */
        val custom: Boolean = false,
    )

    @Serializable data class Option(val label: String, val description: String = "")

    @Serializable
    data class Choice(
        val id: String,
        val label: String,
        @SerialName("takes_message") val takesMessage: Boolean = false,
    )

    /** What the dialog asks, as its card's title says it. */
    val title: String
        get() =
            when (kind) {
                "question" -> "The agent asks"
                "plan" -> "Approve the plan?"
                else -> "Allow ${tool ?: "this"}?"
            }

    /**
     * What the dialog is about, in a line: the command or file, the plan's title, or the question
     * with how many more follow it.
     */
    val target: String?
        get() {
            if (kind != "question") return detail
            val first = questions.firstOrNull()?.question ?: return detail
            val more = questions.size - 1
            return if (more > 0) "$first (+$more more)" else first
        }

    /** The choices by weight: the first is the main one, the last declines, the rest between. */
    val groups: ChoiceGroups?
        get() {
            val primary = choices.firstOrNull() ?: return null
            if (choices.size == 1) return ChoiceGroups(primary, emptyList(), null)
            return ChoiceGroups(primary, choices.subList(1, choices.size - 1), choices.last())
        }

    data class ChoiceGroups(val primary: Choice, val more: List<Choice>, val negative: Choice?)

    companion object {
        /** The choice that submits the answers to [questions]. */
        const val ANSWER = "answer"
    }
}

/** The user's answer to a dialog (`POST …/dialog`). */
@Serializable
data class DialogAnswer(
    val id: String,
    val choice: String,
    /** The labels picked, or words typed, per question text. */
    val answers: Map<String, List<String>> = emptyMap(),
    val message: String? = null,
)
