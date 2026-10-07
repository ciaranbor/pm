package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.R
import dev.pm.app.model.Dialog

/**
 * A dialog over the whole screen: the plan, the whole command, or the question form, with every
 * choice in a bar that stays at the bottom. Back or Close returns to the card, keeping what was
 * filled in.
 */
@Composable
internal fun DialogReview(
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    notice: String?,
    onChoice: (Dialog.Choice) -> Unit,
    sendMessage: (String) -> Unit,
    close: () -> Unit,
) {
    FullScreen(close) {
        DialogReviewView(form, pendingOn, busy, notice, onChoice, sendMessage, close)
    }
}

/** [DialogReview]'s page, apart from the window it is shown in. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun DialogReviewView(
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    notice: String?,
    onChoice: (Dialog.Choice) -> Unit,
    sendMessage: (String) -> Unit,
    close: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val dialog = form.dialog
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(dialog.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = {
                    IconButton(onClick = close) {
                        Icon(painterResource(R.drawable.ic_close), "Close")
                    }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 2.dp) {
                Column(
                    Modifier.fillMaxWidth()
                        .navigationBarsPadding()
                        .imePadding()
                        .padding(horizontal = Spacing.gutter, vertical = Spacing.s)
                ) {
                    if (notice != null) {
                        Text(
                            notice,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.labelMedium,
                        )
                    }
                    val messaging = form.messaging
                    if (messaging != null) {
                        MessageField(
                            form,
                            dialog.choices.find { it.id == messaging }?.label ?: messaging,
                            pending = pendingOn == messaging,
                            enabled = !busy,
                            send = { sendMessage(messaging) },
                        )
                    } else AllChoices(form, pendingOn, busy, onChoice)
                }
            }
        },
    ) { padding ->
        Column(
            Modifier.padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(Spacing.gutter),
            verticalArrangement = Arrangement.spacedBy(Spacing.l),
        ) {
            when (dialog.kind) {
                "question" ->
                    dialog.questions.forEach { q ->
                        QuestionView(
                            q,
                            form.picked[q.question].orEmpty(),
                            form.typed[q.question].orEmpty(),
                            pick = { form.picked[q.question] = it },
                            type = { form.typed[q.question] = it },
                        )
                    }
                "plan" ->
                    Selectable {
                        PmMarkdown(
                            dialog.plan.orEmpty(),
                            text = MaterialTheme.typography.bodyLarge,
                        )
                    }
                else -> {
                    dialog.tool?.let { Text(it, style = MaterialTheme.typography.titleSmall) }
                    dialog.detail?.let { Selectable { Text(it, style = terminalStyle(13.sp)) } }
                }
            }
        }
    }
}

/** Every choice with its whole label, the main one filled, wrapping onto lines as they need. */
@Composable
private fun AllChoices(
    form: DialogForm,
    pendingOn: String?,
    busy: Boolean,
    onChoice: (Dialog.Choice) -> Unit,
) {
    val choices = form.dialog.choices
    FlowRow(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(Spacing.s, Alignment.End),
        verticalArrangement = Arrangement.spacedBy(Spacing.xs),
    ) {
        choices.asReversed().forEach { choice ->
            val mine = pendingOn == choice.id
            PendingButton(
                choice.label,
                onClick = { onChoice(choice) },
                pending = mine,
                enabled = mine || !busy && (choice.id != Dialog.ANSWER || form.answers != null),
                emphasis = if (choice == choices.first()) Emphasis.Filled else Emphasis.Outlined,
            )
        }
    }
}

@Composable
private fun QuestionView(
    question: Dialog.Question,
    picked: Set<String>,
    typed: String,
    pick: (Set<String>) -> Unit,
    type: (String) -> Unit,
) {
    Column {
        if (question.header.isNotBlank()) {
            Text(
                question.header,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        Text(question.question, style = MaterialTheme.typography.bodyLarge)
        question.options.forEach { option ->
            val on = option.label in picked
            val row =
                if (question.multiSelect) {
                    Modifier.toggleable(on, role = Role.Checkbox) {
                        pick(if (it) picked + option.label else picked - option.label)
                    }
                } else {
                    Modifier.selectable(on, role = Role.RadioButton) {
                        pick(setOf(option.label))
                        type("")
                    }
                }
            Row(
                row.fillMaxWidth().padding(vertical = Spacing.xxs),
                verticalAlignment = Alignment.Top,
            ) {
                if (question.multiSelect) Checkbox(on, null) else RadioButton(on, null)
                Column(Modifier.padding(start = Spacing.s)) {
                    Text(option.label, fontWeight = FontWeight.Medium)
                    if (option.description.isNotBlank()) {
                        Text(
                            option.description,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
        if (question.custom) {
            OutlinedTextField(
                value = typed,
                onValueChange = {
                    type(it)
                    if (!question.multiSelect && it.isNotBlank()) pick(emptySet())
                },
                placeholder = {
                    Text(if (question.options.isEmpty()) "Your answer" else "Something else")
                },
                maxLines = 4,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
