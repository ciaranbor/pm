package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.SavedStateHandle
import dev.pm.app.R
import dev.pm.app.model.AgentState
import dev.pm.app.model.Waiting

/**
 * Unsent drafts by agent, so leaving an agent and coming back keeps one; in `saved`, if given, so
 * they outlive Android stopping the app in the background. Saved state rides in a Binder
 * transaction, so the longest drafts past [SAVED_MAX] characters in all are kept in memory only.
 */
@Stable
class Drafts(private val saved: SavedStateHandle? = null) {
    private val held =
        mutableStateMapOf<String, String>().apply {
            saved?.get<HashMap<String, String>>(SAVED)?.let(::putAll)
        }

    operator fun get(key: String): String = held[key].orEmpty()

    operator fun set(key: String, draft: String) {
        if (draft.isEmpty()) held.remove(key) else held[key] = draft
        val saving = saved ?: return
        var room = SAVED_MAX
        val kept = HashMap<String, String>()
        for ((at, text) in held.entries.sortedBy { it.value.length }) {
            if (text.length > room) break
            kept[at] = text
            room -= text.length
        }
        saving[SAVED] = kept
    }

    /** Put `text` back into the draft under `key`, after what is there. */
    fun restore(key: String, text: String) {
        val held = get(key)
        set(key, if (held.isBlank()) text else "$held\n$text")
    }

    companion object {
        const val SAVED_MAX = 64 * 1024
    }
}

private const val SAVED = "drafts"

/**
 * Where the user writes to the agent: a draft, kept under `draftKey`, sent as one prompt. A dialog
 * up, or an agent not running, takes no text; a dialog up offers the terminal, where it can be
 * answered with keys. Interrupt and the terminal sit beside it.
 */
@Composable
internal fun Composer(
    state: AgentState?,
    waiting: Waiting?,
    sending: Boolean,
    notice: String?,
    interrupting: Boolean,
    drafts: Drafts,
    draftKey: String,
    send: (String) -> Unit,
    interrupt: () -> Unit,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val draft = drafts[draftKey]
    val edit = { text: String -> drafts[draftKey] = text }
    val blocked =
        when (state) {
            AgentState.Asking ->
                "A dialog is up at the terminal" + (waiting?.detail?.let { ": $it" } ?: ".")
            AgentState.Dead,
            AgentState.Stopped,
            AgentState.Closed -> "The agent isn't running."
            else -> null
        }
    Surface(modifier.fillMaxWidth(), tonalElevation = 2.dp) {
        Column(Modifier.padding(horizontal = Spacing.s, vertical = Spacing.xs)) {
            val status = notice ?: blocked
            if (status != null) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        status,
                        style = MaterialTheme.typography.labelMedium,
                        color =
                            if (notice != null) MaterialTheme.colorScheme.error
                            else MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.weight(1f).padding(Spacing.xs),
                    )
                    if (state == AgentState.Asking) {
                        TextButton(onClick = openTerminal) {
                            Icon(painterResource(R.drawable.ic_terminal), null)
                            Text("Show the terminal", Modifier.padding(start = Spacing.s))
                        }
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = edit,
                    enabled = blocked == null,
                    placeholder = { Text("Message the agent") },
                    maxLines = 6,
                    modifier = Modifier.weight(1f),
                )
                AgentActions(
                    canInterrupt =
                        !sending && (state == AgentState.Busy || state == AgentState.Asking),
                    interrupting,
                    interrupt,
                    openTerminal,
                )
                PendingIconButton(
                    painterResource(R.drawable.ic_send),
                    "Send",
                    onClick = {
                        send(draft)
                        edit("")
                    },
                    pending = sending,
                    enabled = sending || !interrupting && blocked == null && draft.isNotBlank(),
                    filled = true,
                )
            }
        }
    }
}

/**
 * Text sent to the agent, as the bubble it will be in the conversation, with how far it got, until
 * the conversation shows it. A failed one stays, to be sent again or taken back into the draft.
 */
@Composable
internal fun OutboxBubble(
    outbox: Outbox,
    retry: () -> Unit,
    edit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val failed = outbox as? Outbox.Failed
    Column(
        modifier.fillMaxWidth().padding(horizontal = Spacing.m, vertical = Spacing.xs),
        horizontalAlignment = Alignment.End,
    ) {
        Text(
            outbox.text,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
            modifier =
                Modifier.widthIn(max = 320.dp)
                    .alpha(if (failed == null) 0.7f else 1f)
                    .background(MaterialTheme.colorScheme.primaryContainer, BUBBLE)
                    .padding(horizontal = Spacing.m, vertical = Spacing.s),
        )
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            if (outbox is Outbox.Sending) {
                CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
            }
            Text(
                describe(outbox),
                style = MaterialTheme.typography.labelSmall,
                color =
                    if (failed != null) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            if (failed != null) {
                TextButton(onClick = edit) { Text("Edit") }
                TextButton(onClick = retry) { Text("Retry") }
            }
        }
    }
}

/** What the user answered a dialog, as a reply of theirs, until the conversation moves on. */
@Composable
internal fun AnsweredRow(text: String, modifier: Modifier = Modifier) {
    Row(
        modifier.fillMaxWidth().padding(horizontal = Spacing.m, vertical = Spacing.xs),
        horizontalArrangement = Arrangement.spacedBy(Spacing.xs, Alignment.End),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            painterResource(R.drawable.ic_check),
            null,
            Modifier.size(16.dp),
            tint = MaterialTheme.colorScheme.primary,
        )
        Text(
            text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 2,
        )
    }
}

private fun describe(outbox: Outbox): String =
    when (outbox) {
        is Outbox.Sending -> "Sending…"
        is Outbox.Queued -> "Queued: the agent reads it when its current step ends"
        is Outbox.Sent -> "Sent"
        is Outbox.Seen -> "Delivered"
        is Outbox.Failed -> "Not sent: ${outbox.reason}"
    }

/**
 * The actions beside the composer or a dialog card: the terminal, and Interrupt where it
 * `canInterrupt`; while an interrupt is on its way, progress in its place.
 */
@Composable
internal fun AgentActions(
    canInterrupt: Boolean,
    interrupting: Boolean,
    interrupt: () -> Unit,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Row(modifier, verticalAlignment = Alignment.CenterVertically) {
        if (interrupting) {
            CircularProgressIndicator(
                Modifier.size(20.dp).semantics { contentDescription = "Interrupting" },
                strokeWidth = 2.dp,
            )
        }
        ActionMenu(
            buildList {
                if (canInterrupt && !interrupting)
                    add(MenuItem("Interrupt", R.drawable.ic_stop, onClick = interrupt))
                add(MenuItem("Show the terminal", R.drawable.ic_terminal, onClick = openTerminal))
            }
        )
    }
}

private val BUBBLE = RoundedCornerShape(12.dp)
