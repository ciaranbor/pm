package dev.pm.app.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.model.Dialog

/** A question's header and whole text, in body type a size smaller where `compact`. */
@Composable
internal fun QuestionText(question: Dialog.Question, compact: Boolean = false) {
    Column {
        if (question.header.isNotBlank()) {
            Text(
                question.header,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        Text(
            question.question,
            style =
                if (compact) MaterialTheme.typography.bodyMedium
                else MaterialTheme.typography.bodyLarge,
        )
    }
}

/** A question to pick an answer to, and to answer in the user's own words where it takes them. */
@Composable
internal fun QuestionView(
    question: Dialog.Question,
    picked: Set<String>,
    typed: String,
    pick: (Set<String>) -> Unit,
    type: (String) -> Unit,
    compact: Boolean = false,
) {
    Column {
        QuestionText(question, compact)
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
                OptionText(option, Modifier.padding(start = Spacing.s))
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

/**
 * An option of a lone question, whole, that answers it on a tap; while `pending` it shows progress
 * over its text.
 */
@Composable
internal fun LoneOption(
    option: Dialog.Option,
    onClick: () -> Unit,
    pending: Boolean,
    enabled: Boolean,
) {
    OutlinedCard(
        onClick = { if (!pending) onClick() },
        enabled = enabled,
        modifier = Modifier.fillMaxWidth().pendingSemantics(pending, option.label),
    ) {
        Box(contentAlignment = Alignment.Center) {
            OptionText(
                option,
                Modifier.fillMaxWidth()
                    .padding(horizontal = Spacing.m, vertical = Spacing.s)
                    .alpha(if (pending) 0f else 1f),
            )
            if (pending) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }
    }
}

/** An option's label, then its description and preview, each whole. */
@Composable
private fun OptionText(option: Dialog.Option, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text(option.label, fontWeight = FontWeight.Medium)
        if (option.description.isNotBlank()) {
            Text(
                option.description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        option.preview?.takeIf { it.isNotBlank() }?.let { Preview(it) }
    }
}

/**
 * A preview as it was drawn: lines unwrapped, since a mockup's columns are its meaning, with a
 * sideways scroll for those wider than the screen.
 */
@Composable
private fun Preview(text: String) {
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerHighest,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier.fillMaxWidth().padding(top = Spacing.xs),
    ) {
        Text(
            text,
            style = terminalStyle(12.sp),
            softWrap = false,
            modifier = Modifier.horizontalScroll(rememberScrollState()).padding(Spacing.s),
        )
    }
}
