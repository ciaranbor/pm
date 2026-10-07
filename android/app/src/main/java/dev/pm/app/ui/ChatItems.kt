package dev.pm.app.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
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
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** What a chat row does when touched: `read` opens an item whole; `act` offers its actions. */
@Stable class ChatTouch(val read: (id: String) -> Unit, val act: (id: String) -> Unit)

@Composable
internal fun ChatRowView(row: ChatRow, today: LocalDate, touch: ChatTouch) {
    when (row) {
        is ChatRow.Day -> DayDivider(dayLabel(row.date, today))
        is ChatRow.Wakes -> Wakes(row, touch)
        is ChatRow.Work -> Work(row, touch)
        is ChatRow.Single -> ItemView(row.item, touch)
    }
}

@Composable
private fun ItemView(item: Item, touch: ChatTouch) {
    when (item) {
        is Item.User ->
            Message(item, touch, Alignment.End) {
                Box(
                    Modifier.widthIn(max = 320.dp)
                        .background(MaterialTheme.colorScheme.primaryContainer, BUBBLE)
                        .padding(horizontal = Spacing.m, vertical = Spacing.s)
                ) {
                    Text(item.text, color = MaterialTheme.colorScheme.onPrimaryContainer)
                }
            }
        is Item.Assistant ->
            Message(item, touch, Alignment.Start) {
                PmMarkdown(keepLineBreaks(item.text), text = MaterialTheme.typography.bodyMedium)
            }
        is Item.Thinking -> ThinkingLine(item, touch)
        is Item.Tool -> ToolLine(item, touch)
        is Item.Continuation -> Unit
        is Item.Compaction ->
            SystemRow(
                "Context compacted",
                Modifier.touched(item.id, touch, readable = item.summary != null),
            )
        is Item.Event ->
            if (item.failure) FailureRow(item, touch)
            else SystemRow(item.text, Modifier.touched(item.id, touch))
    }
}

/**
 * A message: a tap shows when it was sent, a long press offers its actions. `align` puts it and its
 * time at the side it is from.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Message(
    item: Item,
    touch: ChatTouch,
    align: Alignment.Horizontal,
    content: @Composable () -> Unit,
) {
    var timed by rememberSaveable(item.id) { mutableStateOf(false) }
    Column(
        Modifier.fillMaxWidth()
            .combinedClickable(
                onClickLabel = if (timed) "Hide the time" else "Show the time",
                onLongClickLabel = "Message actions",
                onLongClick = { touch.act(item.id) },
                onClick = { timed = !timed },
            ),
        horizontalAlignment = align,
    ) {
        content()
        if (timed) {
            Text(
                timeLabel(item.at) ?: "Time unknown",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = Spacing.xxs),
            )
        }
    }
}

/** An item's time of day in the phone's zone, with its date when not today. */
internal fun timeLabel(at: String?, zone: ZoneId = ZoneId.systemDefault()): String? {
    val time = instant(at)?.atZone(zone) ?: return null
    val clock = time.format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
    return if (time.toLocalDate() == LocalDate.now(zone)) clock
    else "${dayLabel(time.toLocalDate(), LocalDate.now(zone))}, $clock"
}

/** Opens the item whole on a tap, where it has something to read, and its actions on a press. */
@OptIn(ExperimentalFoundationApi::class)
private fun Modifier.touched(id: String, touch: ChatTouch, readable: Boolean = true) =
    combinedClickable(
        onClickLabel = if (readable) "Read" else null,
        onLongClickLabel = "Message actions",
        onLongClick = { touch.act(id) },
        onClick = { if (readable) touch.read(id) },
    )

/** pm waking the agent, once or several times running: one row, open to each prompt. */
@Composable
private fun Wakes(row: ChatRow.Wakes, touch: ChatTouch) {
    val items = row.items
    val title = if (items.size == 1) "pm woke the agent" else "pm woke the agent ×${items.size}"
    Collapsible(
        row.key,
        header = { open ->
            Chevron(open)
            Text(
                title,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
            items.forEach {
                Text(
                    it.text,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().touched(it.id, touch),
                )
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
internal fun SystemRow(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier.fillMaxWidth(),
    )
}

/** An event that reports a failure, in the error colour so it doesn't pass for bookkeeping. */
@Composable
private fun FailureRow(event: Item.Event, touch: ChatTouch) {
    Row(
        Modifier.fillMaxWidth().touched(event.id, touch),
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
        Icon(
            painterResource(R.drawable.ic_error),
            null,
            tint = MaterialTheme.colorScheme.error,
            modifier = Modifier.size(18.dp),
        )
        Text(
            event.text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            maxLines = 4,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/**
 * A header that opens and closes `body`, kept by `key`; only the header takes the tap. `header` is
 * told whether it is open, to draw its chevron.
 */
@Composable
private fun Collapsible(
    key: String,
    header: @Composable RowScope.(open: Boolean) -> Unit,
    modifier: Modifier = Modifier,
    padding: PaddingValues = PaddingValues(vertical = Spacing.xs),
    body: @Composable () -> Unit,
) {
    var open by rememberSaveable(key) { mutableStateOf(false) }
    Column(modifier.animateContentSize()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
            modifier =
                Modifier.fillMaxWidth()
                    .clickable { open = !open }
                    .semantics { stateDescription = if (open) "Expanded" else "Collapsed" }
                    .padding(padding),
        ) {
            header(open)
        }
        if (open) body()
    }
}

@Composable
private fun Chevron(open: Boolean) {
    Icon(
        painterResource(if (open) R.drawable.ic_expand_more else R.drawable.ic_chevron_right),
        null,
        tint = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.size(18.dp),
    )
}

/**
 * A run of the agent's work. A lone tool call is its own line; several are one line saying what
 * they did, which opens to a line each.
 */
@Composable
private fun Work(row: ChatRow.Work, touch: ChatTouch) {
    val tools = row.tools
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        shape = MaterialTheme.shapes.small,
        modifier = Modifier.fillMaxWidth(),
    ) {
        if (row.items.size == 1) {
            ToolLine(tools.single(), touch)
            return@Surface
        }
        val failed = tools.count { it.result?.error == true }
        Collapsible(
            row.key,
            padding = PaddingValues(horizontal = Spacing.s, vertical = Spacing.xs),
            header = { open ->
                RunStatus(
                    running = tools.any { it.result == null && !it.unfinished },
                    failed = failed > 0,
                    unfinished = tools.any { it.unfinished },
                )
                Text(
                    workSummary(tools),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f).padding(start = Spacing.xs),
                )
                if (failed > 0) {
                    Text(
                        "$failed failed",
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.error,
                        maxLines = 1,
                    )
                }
                Chevron(open)
            },
        ) {
            row.items.forEach { item ->
                when (item) {
                    is Item.Tool -> ToolLine(item, touch)
                    is Item.Thinking -> ThinkingLine(item, touch)
                    else -> Unit
                }
            }
        }
    }
}

/** How a tool call or a run of them went: still running, failed, left without a result, or done. */
@Composable
private fun RunStatus(running: Boolean, failed: Boolean, unfinished: Boolean) {
    val size = Modifier.size(16.dp)
    when {
        running ->
            CircularProgressIndicator(
                size.padding(Spacing.xxs),
                strokeWidth = 2.dp,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        failed ->
            Icon(
                painterResource(R.drawable.ic_error),
                "Failed",
                tint = MaterialTheme.colorScheme.error,
                modifier = size,
            )
        unfinished ->
            Icon(
                painterResource(R.drawable.ic_close),
                "No result",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = size,
            )
        else ->
            Icon(
                painterResource(R.drawable.ic_check),
                "Done",
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = size,
            )
    }
}

/** One tool call: how it went, its name and what it ran, and how long it took. A tap reads it. */
@Composable
internal fun ToolLine(tool: Item.Tool, touch: ChatTouch) {
    val result = tool.result
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        modifier =
            Modifier.fillMaxWidth()
                .touched(tool.id, touch)
                .padding(horizontal = Spacing.s, vertical = Spacing.xs),
    ) {
        RunStatus(
            running = result == null && !tool.unfinished,
            failed = result?.error == true,
            unfinished = tool.unfinished,
        )
        Text(
            tool.name,
            style = MaterialTheme.typography.labelLarge,
            color =
                if (result?.error == true) MaterialTheme.colorScheme.error
                else MaterialTheme.colorScheme.onSurface,
        )
        Text(
            tool.input,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        duration(tool)?.let {
            Text(
                durationLabel(it),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** The model's reasoning, as one line of it; a tap reads it whole. */
@Composable
private fun ThinkingLine(thinking: Item.Thinking, touch: ChatTouch) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        modifier =
            Modifier.fillMaxWidth()
                .touched(thinking.id, touch)
                .padding(horizontal = Spacing.s, vertical = Spacing.xs),
    ) {
        Icon(
            painterResource(R.drawable.ic_lightbulb),
            null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(16.dp),
        )
        Text(
            thinking.text.lineSequence().firstOrNull { it.isNotBlank() }?.trim() ?: "Thinking",
            style = MaterialTheme.typography.bodySmall,
            fontStyle = FontStyle.Italic,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

private val BUBBLE = RoundedCornerShape(12.dp)
