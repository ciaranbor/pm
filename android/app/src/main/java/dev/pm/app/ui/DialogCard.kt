package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.R
import dev.pm.app.model.Dialog

/**
 * A dialog on the agent's screen, answered here in the composer's place: a question form, a
 * permission prompt, or a plan, with a button per choice its harness offers. A choice that takes a
 * message asks for it before it is sent.
 */
@Composable
internal fun DialogCard(
    dialog: Dialog,
    answering: Boolean,
    interrupting: Boolean,
    notice: String?,
    answer: (choice: String, answers: Map<String, List<String>>, message: String?) -> Unit,
    interrupt: () -> Unit,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val picked = remember(dialog.id) { mutableStateMapOf<String, Set<String>>() }
    val typed = remember(dialog.id) { mutableStateMapOf<String, String>() }
    var messaging by rememberSaveable(dialog.id) { mutableStateOf<String?>(null) }
    var message by rememberSaveable(dialog.id) { mutableStateOf("") }
    var tapped by rememberSaveable(dialog.id) { mutableStateOf<String?>(null) }
    val send = { choice: String, answers: Map<String, List<String>>, message: String? ->
        tapped = choice
        answer(choice, answers, message)
    }
    val answers = answersOf(dialog, picked, typed)
    Surface(modifier.fillMaxWidth(), tonalElevation = 2.dp) {
        Column(Modifier.padding(horizontal = Spacing.m, vertical = Spacing.s)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    title(dialog),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = openTerminal) {
                    Icon(painterResource(R.drawable.ic_terminal), "Show the terminal")
                }
                PendingIconButton(
                    painterResource(R.drawable.ic_stop),
                    "Interrupt",
                    onClick = interrupt,
                    pending = interrupting,
                    enabled = !answering,
                )
            }
            if (notice != null) {
                Text(
                    notice,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.labelMedium,
                )
            }
            Column(
                Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                when (dialog.kind) {
                    "question" ->
                        dialog.questions.forEach { q ->
                            QuestionView(
                                q,
                                picked[q.question].orEmpty(),
                                typed[q.question].orEmpty(),
                                pick = { picked[q.question] = it },
                                type = { typed[q.question] = it },
                            )
                        }
                    "plan" ->
                        PmMarkdown(
                            dialog.plan.orEmpty(),
                            text = MaterialTheme.typography.bodyMedium,
                        )
                    else -> PermissionView(dialog)
                }
            }
            val pending = messaging
            if (pending != null) {
                OutlinedTextField(
                    value = message,
                    onValueChange = { message = it },
                    placeholder = { Text("Tell the agent what to do instead (optional)") },
                    maxLines = 4,
                    modifier = Modifier.fillMaxWidth().padding(top = Spacing.s),
                )
                Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                    TextButton(onClick = { messaging = null }, enabled = !answering) {
                        Text("Cancel")
                    }
                    PendingButton(
                        dialog.choices.find { it.id == pending }?.label ?: pending,
                        onClick = { send(pending, emptyMap(), message) },
                        pending = answering,
                        enabled = !interrupting,
                    )
                }
            } else {
                Column(
                    Modifier.padding(top = Spacing.s),
                    verticalArrangement = Arrangement.spacedBy(Spacing.xs),
                ) {
                    dialog.choices.forEachIndexed { i, choice ->
                        val mine = answering && tapped == choice.id
                        PendingButton(
                            choice.label,
                            onClick = {
                                when {
                                    choice.takesMessage -> messaging = choice.id
                                    choice.id == Dialog.ANSWER -> send(choice.id, answers!!, null)
                                    else -> send(choice.id, emptyMap(), null)
                                }
                            },
                            modifier = Modifier.fillMaxWidth(),
                            pending = mine,
                            enabled =
                                mine ||
                                    !answering &&
                                        !interrupting &&
                                        (choice.id != Dialog.ANSWER || answers != null),
                            emphasis = if (i == 0) Emphasis.Filled else Emphasis.Outlined,
                        )
                    }
                }
            }
        }
    }
}

private fun title(dialog: Dialog): String =
    when (dialog.kind) {
        "question" -> "The agent asks"
        "plan" -> "Approve the plan?"
        else -> "Allow ${dialog.tool ?: "this"}?"
    }

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

@Composable
private fun PermissionView(dialog: Dialog) {
    dialog.detail?.let {
        Text(
            it,
            style = terminalStyle(13.sp),
            modifier = Modifier.fillMaxWidth().padding(vertical = Spacing.xs),
        )
    }
}
