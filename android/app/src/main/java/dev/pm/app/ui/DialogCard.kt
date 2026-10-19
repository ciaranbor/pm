package dev.pm.app.ui

import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.R
import dev.pm.app.model.Dialog

/**
 * The agent's open dialogs, answered here in the composer's place, one at a time with a way to step
 * between them: each a title, then what it asks whole — the question form, the command, the plan —
 * scrolling within part of the screen, with its choices in a row below that stay in reach. Review
 * opens it over the whole screen. A lone question's options answer it on a tap; a form of several
 * waits for its main choice. A choice that takes a message asks for it first.
 */
@Composable
internal fun DialogCard(
    dialogs: List<Dialog>,
    answering: Answering?,
    interrupting: Boolean,
    notice: DialogNotice?,
    answer: (Dialog, choice: String, answers: Map<String, List<String>>, message: String?) -> Unit,
    interrupt: () -> Unit,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var at by rememberSaveable { mutableStateOf<String?>(null) }
    val index = dialogs.indexOfFirst { it.id == at }.coerceAtLeast(0)
    val dialog = dialogs.getOrNull(index) ?: return
    val form = rememberDialogForm(dialog)
    var reviewing by rememberSaveable { mutableStateOf<String?>(null) }
    val busy = answering != null || interrupting
    // A refused dialog may have left the list: its notice then shows on the one shown instead.
    val said = notice?.takeIf { n -> n.id == dialog.id || dialogs.none { it.id == n.id } }?.text
    val send = { choice: Dialog.Choice ->
        when {
            choice.takesMessage -> form.messaging = choice.id
            choice.id == Dialog.ANSWER -> {
                val answers = form.answers
                if (answers == null) reviewing = dialog.id
                else {
                    form.tapped = choice.id
                    answer(dialog, choice.id, answers, null)
                }
            }
            else -> {
                form.tapped = choice.id
                answer(dialog, choice.id, emptyMap(), null)
            }
        }
    }
    val sendMessage = { choice: String ->
        form.tapped = choice
        answer(dialog, choice, emptyMap(), form.message)
    }
    val pendingOn = answering?.takeIf { it.id == dialog.id }?.let { form.tapped }

    Surface(modifier.fillMaxWidth(), tonalElevation = 2.dp) {
        BoxWithConstraints {
            // The card's own room is what the screen leaves; the text takes part of it, so the
            // conversation above stays in sight, and never the room the title and choices need.
            val room = if (constraints.hasBoundedHeight) maxHeight * TEXT_SHARE else Dp.Unspecified
            Column(
                Modifier.heightIn(max = maxHeight)
                    .padding(start = Spacing.m, end = Spacing.xs, bottom = Spacing.s)
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        dialog.title,
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.weight(1f),
                    )
                    if (dialogs.size > 1) {
                        Stepper(index, dialogs.size) { at = dialogs[it].id }
                    }
                    AgentActions(answering == null, interrupting, interrupt, openTerminal)
                }
                Column(Modifier.weight(1f, fill = false).padding(end = Spacing.s)) {
                    val scroll =
                        rememberSaveable(dialog.id, saver = ScrollState.Saver) { ScrollState(0) }
                    Column(
                        Modifier.weight(1f, fill = false)
                            .heightIn(max = room)
                            .verticalScroll(scroll)
                            .testTag(DIALOG_TEXT),
                        verticalArrangement = Arrangement.spacedBy(Spacing.s),
                    ) {
                        Asked(dialog, form, pendingOn, busy, answer)
                    }
                    if (scroll.maxValue > 0) HorizontalDivider()
                    if (said != null) {
                        Text(
                            said,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.labelMedium,
                        )
                    }
                    val messaging = form.messaging
                    when {
                        messaging != null ->
                            MessageField(
                                form,
                                dialog.choices.find { it.id == messaging }?.label ?: messaging,
                                pending = pendingOn == messaging,
                                enabled = !busy,
                                send = { sendMessage(messaging) },
                            )
                        isLoneChoice(dialog) ->
                            LoneActions(dialog, form, pendingOn, busy, answer) {
                                reviewing = dialog.id
                            }
                        else ->
                            ChoiceRow(
                                dialog,
                                pendingOn,
                                busy,
                                review = { reviewing = dialog.id },
                                onChoice = send,
                            )
                    }
                }
            }
        }
    }
    if (reviewing == dialog.id) {
        DialogReview(
            form,
            pendingOn,
            busy,
            said,
            onChoice = send,
            sendMessage = sendMessage,
            close = { reviewing = null },
        )
    }
}

/** The share of the card's room its text may fill before it scrolls. */
private const val TEXT_SHARE = 0.6f

/** The tag of the card's scrolling text, for tests to scroll it. */
internal const val DIALOG_TEXT = "dialog-text"

/**
 * What the dialog asks, whole: a lone question with its options to tap, a form's questions to fill
 * in, the plan, or the command or file a tool would act on.
 */
@Composable
private fun Asked(
    dialog: Dialog,
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    answer: (Dialog, String, Map<String, List<String>>, String?) -> Unit,
) {
    when {
        isLoneChoice(dialog) -> {
            val question = dialog.questions.single()
            Selectable { QuestionText(question, compact = true) }
            question.options.forEach { option ->
                val mine = pendingOn == option.label
                LoneOption(
                    option,
                    onClick = {
                        form.tapped = option.label
                        answer(
                            dialog,
                            Dialog.ANSWER,
                            mapOf(question.question to listOf(option.label)),
                            null,
                        )
                    },
                    pending = mine,
                    enabled = mine || !busy,
                )
            }
        }
        dialog.kind == "question" && dialog.questions.isNotEmpty() ->
            dialog.questions.forEach { q ->
                QuestionView(
                    q,
                    form.picked[q.question].orEmpty(),
                    form.typed[q.question].orEmpty(),
                    pick = { form.picked[q.question] = it },
                    type = { form.typed[q.question] = it },
                    compact = true,
                )
            }
        dialog.kind == "plan" && !dialog.plan.isNullOrBlank() ->
            Selectable { PmMarkdown(dialog.plan, text = MaterialTheme.typography.bodyMedium) }
        else ->
            dialog.target?.let {
                val style =
                    if (dialog.kind == "permission") terminalStyle(13.sp)
                    else MaterialTheme.typography.bodyMedium
                Selectable { Text(it, style = style) }
            }
    }
    dialog.subagent?.let {
        Text(
            "From a subagent",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** "2 of 3", with steps back and on. */
@Composable
private fun Stepper(index: Int, count: Int, go: (Int) -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = { go(index - 1) }, enabled = index > 0) {
            Icon(painterResource(R.drawable.ic_chevron_left), "Previous dialog")
        }
        Text(
            "${index + 1} of $count",
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.semantics { contentDescription = "Dialog ${index + 1} of $count" },
        )
        IconButton(onClick = { go(index + 1) }, enabled = index < count - 1) {
            Icon(painterResource(R.drawable.ic_chevron_right), "Next dialog")
        }
    }
}

/** A lone question's own words through its review, and its declining choice. */
@Composable
private fun LoneActions(
    dialog: Dialog,
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    answer: (Dialog, String, Map<String, List<String>>, String?) -> Unit,
    review: () -> Unit,
) {
    val question = dialog.questions.single()
    Row(verticalAlignment = Alignment.CenterVertically) {
        TextButton(onClick = review) { Text(if (question.custom) "Other answer" else "Review") }
        Spacer(Modifier.weight(1f))
        dialog.groups?.negative?.let { decline ->
            PendingButton(
                shortLabel(decline.label),
                onClick = {
                    form.tapped = decline.id
                    answer(dialog, decline.id, emptyMap(), null)
                },
                pending = pendingOn == decline.id,
                enabled = pendingOn == decline.id || !busy,
                emphasis = Emphasis.Text,
            )
        }
    }
}
