package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
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
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.R
import dev.pm.app.model.Dialog

/**
 * The agent's open dialogs, answered here in the composer's place, one at a time with a way to step
 * between them: each a title, its target in a line, and its choices in a row. Review opens it
 * whole, over the screen. A lone question's options answer it on a tap; a form of several waits for
 * Submit. A choice that takes a message asks for it first.
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
        Column(Modifier.padding(start = Spacing.m, end = Spacing.xs, bottom = Spacing.s)) {
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
            Column(Modifier.padding(end = Spacing.s)) {
                Target(dialog)
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
                        LoneQuestion(dialog, form, pendingOn, busy, answer) {
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

/** What the dialog is about, in a line: the command or file, the plan's title, the question. */
@Composable
private fun Target(dialog: Dialog) {
    val text = dialog.target ?: return
    if (dialog.kind == "permission") {
        Text(text, style = terminalStyle(13.sp), maxLines = 1, overflow = targetCut(text))
    } else {
        Text(
            text,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = if (isLoneChoice(dialog)) 3 else 1,
            overflow = TextOverflow.Ellipsis,
        )
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

/**
 * A lone single-select question: a tap on an option answers it. Review shows the options'
 * descriptions, and takes the user's own words where the question does.
 */
@Composable
private fun LoneQuestion(
    dialog: Dialog,
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    answer: (Dialog, String, Map<String, List<String>>, String?) -> Unit,
    review: () -> Unit,
) {
    val question = dialog.questions.single()
    Column {
        FlowRow(
            Modifier.fillMaxWidth().padding(top = Spacing.s),
            horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
            question.options.forEach { option ->
                val mine = pendingOn == option.label
                PendingButton(
                    option.label,
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
                    emphasis = Emphasis.Outlined,
                )
            }
        }
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
}
