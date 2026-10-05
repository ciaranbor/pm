package dev.pm.app.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.AgentState

/**
 * Where the user writes to the agent: a draft, sent as one prompt, and how the last send went. A
 * dialog up, or an agent not running, takes no text; the draft of a failed send comes back.
 */
@Composable
internal fun Composer(
    state: AgentState?,
    outbox: Outbox?,
    notice: String?,
    send: (String) -> Unit,
    interrupt: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var draft by rememberSaveable { mutableStateOf("") }
    LaunchedEffect(outbox) { if (outbox is Outbox.Failed && draft.isBlank()) draft = outbox.text }
    val blocked =
        when (state) {
            AgentState.Asking -> "A dialog is up: answer it on the Screen tab."
            AgentState.Dead,
            AgentState.Stopped,
            AgentState.Closed -> "The agent isn't running."
            else -> null
        }
    Surface(modifier.fillMaxWidth(), tonalElevation = 2.dp) {
        Column(Modifier.padding(horizontal = 8.dp, vertical = 4.dp)) {
            val status = notice ?: blocked ?: outbox?.let(::describe)
            if (status != null) {
                Text(
                    status,
                    style = MaterialTheme.typography.labelMedium,
                    color =
                        if (notice != null || outbox is Outbox.Failed)
                            MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(4.dp),
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { draft = it },
                    enabled = blocked == null,
                    placeholder = { Text("Message the agent") },
                    maxLines = 6,
                    modifier = Modifier.weight(1f),
                )
                if (state == AgentState.Busy || state == AgentState.Asking) {
                    IconButton(onClick = interrupt) {
                        Icon(painterResource(R.drawable.ic_stop), "Interrupt")
                    }
                }
                FilledIconButton(
                    onClick = {
                        send(draft)
                        draft = ""
                    },
                    enabled = blocked == null && draft.isNotBlank() && outbox !is Outbox.Sending,
                    modifier = Modifier.padding(start = 4.dp),
                ) {
                    Icon(painterResource(R.drawable.ic_send), "Send")
                }
            }
        }
    }
}

private fun describe(outbox: Outbox): String =
    when (outbox) {
        is Outbox.Sending -> "Sending…"
        is Outbox.Queued -> "Queued: the agent reads it when its current step ends."
        is Outbox.Sent -> "Sent; not in the conversation yet."
        is Outbox.Seen -> "Delivered."
        is Outbox.Failed -> "Not sent: ${outbox.reason}"
    }

/** A key the bar presses, by tmux's name for it, and what the button says. */
private data class Key(val name: String, val label: String, val description: String = label)

private val KEYS =
    listOf(
        Key("Escape", "Esc", "Escape"),
        Key("Enter", "⏎", "Enter"),
        Key("Up", "↑", "Up"),
        Key("Down", "↓", "Down"),
        Key("Left", "←", "Left"),
        Key("Right", "→", "Right"),
        Key("Tab", "Tab"),
        Key("BTab", "⇧Tab", "Shift Tab"),
        Key("Space", "Space"),
        Key("BSpace", "⌫", "Backspace"),
        Key("1", "1"),
        Key("2", "2"),
        Key("3", "3"),
        Key("4", "4"),
    )

/**
 * Keys for answering what the agent's screen shows: a dialog's options, Escape, Ctrl-C. Ctrl-C asks
 * first: twice in a row exits Claude Code and codex. An agent between turns takes none.
 */
@Composable
internal fun KeyBar(
    state: AgentState?,
    press: (List<String>) -> Unit,
    modifier: Modifier = Modifier,
) {
    var confirming by rememberSaveable { mutableStateOf(false) }
    // Between turns the agent waits in pm's Stop hook: a key would only end it.
    val enabled = state != AgentState.Idle
    Row(
        modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        KEYS.forEach { key ->
            FilledTonalButton(onClick = { press(listOf(key.name)) }, enabled = enabled) {
                Text(
                    key.label,
                    Modifier.clearAndSetSemantics { contentDescription = key.description },
                )
            }
        }
        FilledTonalButton(onClick = { confirming = true }, enabled = enabled) {
            Text("^C", Modifier.clearAndSetSemantics { contentDescription = "Control C" })
        }
    }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("Press Ctrl-C?") },
            text = { Text("Twice in a row exits Claude Code and codex.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirming = false
                        press(listOf("C-c"))
                    }
                ) {
                    Text("Press")
                }
            },
            dismissButton = { TextButton(onClick = { confirming = false }) { Text("Cancel") } },
        )
    }
}
