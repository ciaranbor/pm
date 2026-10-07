package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.Dialog

/**
 * A dialog's choices in one row: Review, then the declining choice, then the main one as a split
 * button whose menu holds the rest ("don't ask again", a mode switch) and the main one's whole
 * label where the button shows it cut. A question form's main choice opens the review until it is
 * filled.
 */
@Composable
internal fun ChoiceRow(
    dialog: Dialog,
    pendingOn: String?,
    busy: Boolean,
    review: () -> Unit,
    onChoice: (Dialog.Choice) -> Unit,
    modifier: Modifier = Modifier,
) {
    val groups = dialog.groups ?: return
    Row(
        modifier.fillMaxWidth().padding(top = Spacing.xs),
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        val primary = groups.primary
        if (primary.id != Dialog.ANSWER) TextButton(onClick = review) { Text("Review") }
        Spacer(Modifier.weight(1f))
        groups.negative?.let { no ->
            PendingButton(
                shortLabel(no.label),
                onClick = { onChoice(no) },
                pending = pendingOn == no.id,
                enabled = pendingOn == no.id || !busy,
                emphasis = Emphasis.Outlined,
            )
        }
        val label = if (primary.id == Dialog.ANSWER) "Answer" else shortLabel(primary.label)
        val menu = buildList {
            if (label != primary.label && primary.id != Dialog.ANSWER) add(primary)
            addAll(groups.more)
        }
        SplitButton(label, primary, menu, pendingOn, busy, onChoice)
    }
}

/**
 * A filled button for the `main` choice, labelled `label`, with a trailing arrow opening `menu`
 * when it has entries; the side holding the choice `pendingOn` shows progress.
 */
@Composable
private fun SplitButton(
    label: String,
    main: Dialog.Choice,
    menu: List<Dialog.Choice>,
    pendingOn: String?,
    busy: Boolean,
    onChoice: (Dialog.Choice) -> Unit,
) {
    val mainPending = pendingOn == main.id
    if (menu.isEmpty()) {
        PendingButton(
            label,
            onClick = { onChoice(main) },
            pending = mainPending,
            enabled = mainPending || !busy,
        )
        return
    }
    val menuPending = !mainPending && menu.any { it.id == pendingOn }
    var open by remember { mutableStateOf(false) }
    Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
        Button(
            onClick = { if (!mainPending) onChoice(main) },
            enabled = mainPending || !busy,
            shape =
                RoundedCornerShape(
                    topStart = 20.dp,
                    bottomStart = 20.dp,
                    topEnd = 4.dp,
                    bottomEnd = 4.dp,
                ),
            modifier =
                Modifier.semantics {
                    if (mainPending) {
                        contentDescription = label
                        stateDescription = "In progress"
                    }
                },
        ) {
            Pending(mainPending) { Text(label, Modifier.alpha(if (mainPending) 0f else 1f)) }
        }
        Box {
            Button(
                onClick = { open = true },
                enabled = menuPending || !busy,
                shape =
                    RoundedCornerShape(
                        topStart = 4.dp,
                        bottomStart = 4.dp,
                        topEnd = 20.dp,
                        bottomEnd = 20.dp,
                    ),
                contentPadding = PaddingValues(horizontal = Spacing.s),
                modifier = Modifier.semantics { if (menuPending) stateDescription = "In progress" },
            ) {
                Pending(menuPending) {
                    Icon(
                        painterResource(R.drawable.ic_expand_more),
                        "More choices",
                        Modifier.alpha(if (menuPending) 0f else 1f),
                    )
                }
            }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                menu.forEach { choice ->
                    DropdownMenuItem(
                        text = { Text(choice.label) },
                        onClick = {
                            open = false
                            onChoice(choice)
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun Pending(pending: Boolean, content: @Composable () -> Unit) {
    Box(contentAlignment = Alignment.Center) {
        content()
        if (pending) {
            CircularProgressIndicator(
                Modifier.size(18.dp),
                color = LocalContentColor.current,
                strokeWidth = 2.dp,
            )
        }
    }
}

/** The message a choice takes for the agent, then Cancel or the choice itself to send it. */
@Composable
internal fun MessageField(
    form: DialogForm,
    label: String,
    pending: Boolean,
    enabled: Boolean,
    send: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier) {
        OutlinedTextField(
            value = form.message,
            onValueChange = { form.message = it },
            placeholder = { Text("Tell the agent what to do instead (optional)") },
            maxLines = 4,
            modifier = Modifier.fillMaxWidth().padding(top = Spacing.s),
        )
        Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
            TextButton(onClick = { form.messaging = null }, enabled = !pending) { Text("Cancel") }
            PendingButton(label, onClick = send, pending = pending, enabled = pending || enabled)
        }
    }
}
