package dev.pm.app.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.Item
import java.time.LocalDate

@Composable
internal fun ChatRowView(
    row: ChatRow,
    today: LocalDate,
    openResult: (tool: String, ref: String) -> Unit,
) {
    when (row) {
        is ChatRow.Day -> DayDivider(dayLabel(row.date, today))
        is ChatRow.Wakes -> Wakes(row.items)
        is ChatRow.Single -> ItemView(row.item, openResult)
    }
}

@Composable
private fun ItemView(item: Item, openResult: (tool: String, ref: String) -> Unit) {
    when (item) {
        is Item.User ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Box(
                    Modifier.widthIn(max = 320.dp)
                        .background(
                            MaterialTheme.colorScheme.primaryContainer,
                            RoundedCornerShape(12.dp),
                        )
                        .padding(Spacing.m)
                ) {
                    SelectionContainer {
                        Text(item.text, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    }
                }
            }
        is Item.Assistant -> AssistantText(item.text)
        is Item.Thinking -> Collapsible(title = "thinking") { Thinking(item.text) }
        is Item.Tool -> ToolCard(item, openResult)
        is Item.Continuation -> Wakes(listOf(item))
        is Item.Compaction -> SystemRow("context compacted")
        is Item.Event -> SystemRow(item.text)
    }
}

@Composable
private fun AssistantText(text: String) {
    SelectionContainer { PmMarkdown(text, text = MaterialTheme.typography.bodyMedium) }
}

@Composable
private fun Thinking(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, fontStyle = FontStyle.Italic)
}

/** pm waking the agent, once or several times running: one row, open to each prompt. */
@Composable
internal fun Wakes(items: List<Item.Continuation>) {
    val title = if (items.size == 1) "pm woke the agent" else "pm woke the agent ×${items.size}"
    Collapsible(title, Modifier.fillMaxWidth()) {
        SelectionContainer {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                items.forEach {
                    Text(
                        it.text,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun DayDivider(label: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        HorizontalDivider(Modifier.weight(1f))
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = Spacing.s),
        )
        HorizontalDivider(Modifier.weight(1f))
    }
}

@Composable
internal fun SystemRow(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.fillMaxWidth(),
    )
}

/** A title that opens and closes `body`; only the title takes the tap. */
@Composable
private fun Collapsible(
    title: String,
    modifier: Modifier = Modifier,
    body: @Composable () -> Unit,
) {
    var open by rememberSaveable { mutableStateOf(false) }
    Column(modifier.animateContentSize()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
            modifier =
                Modifier.clickable { open = !open }
                    .semantics { stateDescription = if (open) "Expanded" else "Collapsed" }
                    .padding(vertical = Spacing.xs),
        ) {
            Icon(
                painterResource(
                    if (open) R.drawable.ic_expand_more else R.drawable.ic_chevron_right
                ),
                null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.size(18.dp),
            )
            Text(
                title,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (open) body()
    }
}

/**
 * A tool call, collapsed to its name and input. Its header opens it to the result, cut short as pm
 * serves it; "Show all" asks `openResult` for the whole output by its `full` reference.
 */
@Composable
internal fun ToolCard(tool: Item.Tool, openResult: (tool: String, ref: String) -> Unit) {
    var open by rememberSaveable(tool.id) { mutableStateOf(false) }
    val result = tool.result
    Card(Modifier.fillMaxWidth().animateContentSize()) {
        Column(
            Modifier.fillMaxWidth().clickable { open = !open }.padding(Spacing.m),
            verticalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(tool.name, style = MaterialTheme.typography.labelLarge)
                Text(
                    when {
                        result == null -> "  running"
                        result.error -> "  failed"
                        else -> ""
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color =
                        if (result?.error == true) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (!open) {
                Text(
                    tool.input,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (open) {
            Column(
                Modifier.padding(start = Spacing.m, end = Spacing.m, bottom = Spacing.m),
                verticalArrangement = Arrangement.spacedBy(Spacing.xs),
            ) {
                SelectionContainer {
                    Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                        Text(
                            tool.input,
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                        )
                        if (result != null) {
                            Text(
                                result.text,
                                style = MaterialTheme.typography.bodySmall,
                                fontFamily = FontFamily.Monospace,
                                softWrap = false,
                                modifier = Modifier.horizontalScroll(rememberScrollState()),
                            )
                        }
                    }
                }
                val ref = result?.full
                if (result?.truncated == true && ref != null) {
                    TextButton(onClick = { openResult(tool.name, ref) }) { Text("Show all") }
                }
            }
        }
    }
}
