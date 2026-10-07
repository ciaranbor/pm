package dev.pm.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import dev.pm.app.model.Dialog

/**
 * What the user has put into one dialog so far, shared by its card and its full-screen review: the
 * options picked and words typed per question, the choice waiting on a message, and that message.
 */
@Stable
internal class DialogForm(val dialog: Dialog) {
    val picked = mutableStateMapOf<String, Set<String>>()
    val typed = mutableStateMapOf<String, String>()

    /** The choice that asked for a message, until it is sent or cancelled. */
    var messaging by mutableStateOf<String?>(null)
    var message by mutableStateOf("")

    /** The choice, or a lone question's option, last pressed: the one shown pending. */
    var tapped by mutableStateOf<String?>(null)

    val answers: Map<String, List<String>>?
        get() = answersOf(dialog, picked, typed)

    companion object {
        fun saver(dialog: Dialog) =
            listSaver<DialogForm, Any?>(
                save = { form ->
                    listOf(
                        HashMap(form.picked.mapValues { ArrayList(it.value) }),
                        HashMap(form.typed),
                        form.messaging,
                        form.message,
                    )
                },
                restore = { saved ->
                    DialogForm(dialog).apply {
                        @Suppress("UNCHECKED_CAST")
                        (saved[0] as Map<String, List<String>>).forEach { (q, labels) ->
                            picked[q] = labels.toSet()
                        }
                        @Suppress("UNCHECKED_CAST") typed.putAll(saved[1] as Map<String, String>)
                        messaging = saved[2] as String?
                        message = saved[3] as String
                    }
                },
            )
    }
}

@Composable
internal fun rememberDialogForm(dialog: Dialog): DialogForm =
    rememberSaveable(dialog.id, saver = DialogForm.saver(dialog)) { DialogForm(dialog) }

/**
 * The answers to `dialog`'s questions, per question text: the options picked, then the words typed;
 * null until every question has one.
 */
internal fun answersOf(
    dialog: Dialog,
    picked: Map<String, Set<String>>,
    typed: Map<String, String>,
): Map<String, List<String>>? {
    val answers =
        dialog.questions.associate { q ->
            val own = typed[q.question]?.trim()?.takeIf { it.isNotEmpty() && q.custom }
            val labels = q.options.map { it.label }.filter { it in picked[q.question].orEmpty() }
            q.question to
                when {
                    q.multiSelect -> labels + listOfNotNull(own)
                    own != null -> listOf(own)
                    else -> labels.take(1)
                }
        }
    return answers.takeIf { a -> a.values.all { it.isNotEmpty() } }
}

/**
 * A choice's label cut to what comes before its first comma, for a row with little room: "Yes, and
 * use auto mode" is "Yes". Its menu or the review gives the whole.
 */
internal fun shortLabel(label: String): String = label.substringBefore(", ").ifBlank { label }

/** Whether `dialog` is one question with options, one of which answers it on a tap (D4). */
internal fun isLoneChoice(dialog: Dialog): Boolean {
    val q = dialog.questions.singleOrNull() ?: return false
    return dialog.kind == "question" && !q.multiSelect && q.options.isNotEmpty()
}
