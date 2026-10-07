package dev.pm.app.ui

import androidx.annotation.DrawableRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LocalContentColor
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

/**
 * An action for [ActionMenu]: `icon` stands for it when shown as a button; `destructive` ones show
 * in the error colour, after a divider in a menu.
 */
data class MenuItem(
    val label: String,
    @DrawableRes val icon: Int,
    val destructive: Boolean = false,
    val onClick: () -> Unit,
)

/**
 * `items` as icon buttons, or behind a ⋮ once there are [MENU_FROM] of them: a menu around one or
 * two entries costs a tap and draws an oversized box.
 */
@Composable
fun ActionMenu(
    items: List<MenuItem>,
    modifier: Modifier = Modifier,
) {
    if (items.size < MENU_FROM) {
        Row(modifier) {
            items.forEach { item ->
                IconButton(onClick = item.onClick) {
                    Icon(
                        painterResource(item.icon),
                        item.label,
                        tint =
                            if (item.destructive) MaterialTheme.colorScheme.error
                            else LocalContentColor.current,
                    )
                }
            }
        }
        return
    }
    Box(modifier) {
        var open by remember { mutableStateOf(false) }
        IconButton(onClick = { open = true }) {
            Icon(painterResource(R.drawable.ic_more_vert), "More actions")
        }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            items.forEachIndexed { i, item ->
                if (item.destructive && i > 0 && !items[i - 1].destructive) HorizontalDivider()
                DropdownMenuItem(
                    text = {
                        Text(
                            item.label,
                            color =
                                if (item.destructive) MaterialTheme.colorScheme.error
                                else Color.Unspecified,
                        )
                    },
                    onClick = {
                        open = false
                        item.onClick()
                    },
                )
            }
        }
    }
}

private const val MENU_FROM = 3

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

/**
 * The dialog an action's state calls for: its confirmation, its progress, what it warned of, or why
 * it failed.
 */
@Composable
fun ActionDialog(state: ActionState, confirm: () -> Unit, dismiss: () -> Unit) {
    when (state) {
        ActionState.Idle -> Unit
        is ActionState.Confirming -> Confirmation(state.action, false, confirm, dismiss)
        is ActionState.Running ->
            if (state.confirmed) Confirmation(state.action, true, confirm, dismiss)
            else
                AlertDialog(
                    onDismissRequest = {},
                    properties =
                        DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false),
                    confirmButton = {},
                    text = {
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(Spacing.l),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            CircularProgressIndicator(Modifier.size(24.dp))
                            Text("${state.action.running} ${state.action.subject}…")
                        }
                    },
                )
        is ActionState.Warned ->
            AlertDialog(
                onDismissRequest = dismiss,
                title = { Text("${state.action.done}, with warnings") },
                text = { Selectable { Text(state.warnings.joinToString("\n\n")) } },
                confirmButton = { TextButton(onClick = dismiss) { Text("OK") } },
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
                    Selectable {
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

/** Asks to confirm `action`; once confirmed, its button shows it `running` until it ends. */
@Composable
private fun Confirmation(
    action: Action,
    running: Boolean,
    confirm: () -> Unit,
    dismiss: () -> Unit,
) {
    val (title, text, verb) =
        when (action) {
            is Action.Merge ->
                Triple(
                    "Merge ${action.feature}?",
                    "Merges it into its base, then deletes its worktree, branch and " +
                        "session. The project's post-merge hook runs on the server.",
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
        properties =
            DialogProperties(dismissOnBackPress = !running, dismissOnClickOutside = !running),
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = {
            PendingButton(
                verb,
                onClick = confirm,
                pending = running,
                emphasis = Emphasis.Text,
                color =
                    if (destructive) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.primary,
            )
        },
        dismissButton = { TextButton(onClick = dismiss, enabled = !running) { Text("Cancel") } },
    )
}
