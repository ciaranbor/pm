package dev.pm.app.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/**
 * A dialog on an agent's screen that can be answered from the phone (`GET …/dialog`): a question
 * form, a permission prompt or a plan, with the choices its harness's CLI offers.
 */
@Serializable
data class Dialog(
    val id: String,
    val kind: String,
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
