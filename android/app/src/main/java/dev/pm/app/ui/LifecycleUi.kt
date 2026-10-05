package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import dev.pm.app.R
import dev.pm.app.model.Snapshot

/** The lifecycle actions a page offers in its top bar's overflow: none for most. */
fun Route.actions(): List<Action> =
    when (this) {
        is Route.Scope ->
            if (scope == Snapshot.MAIN) emptyList()
            else listOf(Action.Merge(project, scope), Action.Delete(project, scope))
        is Route.Agent -> listOf(Action.Restart(project, scope, agent))
        else -> emptyList()
    }

/**
 * The top bar's overflow, holding `actions`; asks the server what this device was granted as it
 * opens, and says how to grant what it lacks.
 */
@Composable
fun ActionsMenu(
    actions: List<Action>,
    grants: Grants,
    refreshGrants: () -> Unit,
    ask: (Action) -> Unit,
    modifier: Modifier = Modifier,
) =
    Box(modifier) {
        var open by remember { mutableStateOf(false) }
        IconButton(
            onClick = {
                refreshGrants()
                open = true
            }
        ) {
            Icon(painterResource(R.drawable.ic_more_vert), "More actions")
        }
        val why =
            when (grants) {
                is Grants.NotGranted ->
                    "This phone may not merge, delete or restart. On the Mac, run " +
                        grantCommand(grants.device)
                Grants.Unsupported -> Lifecycle.UNSUPPORTED
                else -> null
            }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            actions.forEach { action ->
                if (action is Action.Delete) HorizontalDivider()
                DropdownMenuItem(
                    text = {
                        Text(
                            action.verb,
                            color =
                                if (action is Action.Delete && why == null)
                                    MaterialTheme.colorScheme.error
                                else Color.Unspecified,
                        )
                    },
                    enabled = why == null,
                    onClick = {
                        open = false
                        ask(action)
                    },
                )
            }
            if (why != null) {
                Text(
                    why,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier =
                        Modifier.widthIn(max = 280.dp).padding(horizontal = 12.dp, vertical = 8.dp),
                )
            }
        }
    }

private val Action.running: String
    get() =
        when (this) {
            is Action.Merge -> "Merging"
            is Action.Delete -> "Deleting"
            is Action.Restart -> "Restarting"
        }

/** What a finished action did, for the snackbar. */
val Action.done: String
    get() =
        when (this) {
            is Action.Merge -> "Merged $feature"
            is Action.Delete -> "Deleted $feature"
            is Action.Restart -> "Restarted $agent"
        }

/** The dialog an action's state calls for: its confirmation, its progress, or why it failed. */
@Composable
fun ActionDialog(state: ActionState, confirm: () -> Unit, dismiss: () -> Unit) {
    when (state) {
        ActionState.Idle -> Unit
        is ActionState.Confirming -> {
            val action = state.action
            val (title, text, verb) =
                when (action) {
                    is Action.Merge ->
                        Triple(
                            "Merge ${action.feature}?",
                            "Merges it into its base, then deletes its worktree, branch and " +
                                "session. The project's post-merge hook runs on the Mac.",
                            "Merge",
                        )
                    is Action.Delete ->
                        Triple(
                            "Delete ${action.feature}?",
                            "Deletes its worktree, branch and session. pm refuses while it " +
                                "holds uncommitted, unmerged or unpushed work.",
                            "Delete",
                        )
                    is Action.Restart ->
                        Triple(
                            "Restart ${action.agent} anyway?",
                            "${action.agent} is working, asking, or waiting on background " +
                                "work. Restarting interrupts it; once back, it is told to resume.",
                            "Restart anyway",
                        )
                }
            val destructive = action !is Action.Merge
            AlertDialog(
                onDismissRequest = dismiss,
                title = { Text(title) },
                text = { Text(text) },
                confirmButton = {
                    TextButton(onClick = confirm) {
                        Text(
                            verb,
                            color =
                                if (destructive) MaterialTheme.colorScheme.error
                                else MaterialTheme.colorScheme.primary,
                        )
                    }
                },
                dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
            )
        }
        is ActionState.Running ->
            AlertDialog(
                onDismissRequest = {},
                properties =
                    DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false),
                confirmButton = {},
                text = {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        CircularProgressIndicator(Modifier.size(24.dp))
                        Text("${state.action.running} ${state.action.subject}…")
                    }
                },
            )
        is ActionState.Failed ->
            AlertDialog(
                onDismissRequest = dismiss,
                title = {
                    val action = state.action
                    Text(
                        when (state.outcome) {
                            Outcome.Refused ->
                                "Couldn't ${action.verb.lowercase()} ${action.subject}"
                            Outcome.Lost ->
                                "Lost the server while ${action.running.lowercase()} ${action.subject}"
                            Outcome.Broken -> "${action.running} ${action.subject} failed"
                        }
                    )
                },
                text = {
                    SelectionContainer {
                        Text(
                            if (state.outcome == Outcome.Broken)
                                "${state.reason}\n\nIt may have got partway; the app shows how far."
                            else state.reason
                        )
                    }
                },
                confirmButton = { TextButton(onClick = dismiss) { Text("OK") } },
            )
    }
}
