package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.AgentState
import dev.pm.app.model.Waiting

/**
 * Where the user writes to the agent: a draft, sent as one prompt, and how the last send went. A
 * dialog up, or an agent not running, takes no text; the draft of a failed send comes back.
 */
@Composable
internal fun Composer(
    state: AgentState?,
    waiting: Waiting?,
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
            AgentState.Asking ->
                "A dialog is up at the terminal" + (waiting?.detail?.let { ": $it" } ?: ".")
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
