package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** A project's notes: rendered to read, raw to edit, and a refused save's two texts to settle. */
@Composable
fun NotesScreen(model: NotesModel, modifier: Modifier = Modifier) {
    val state by model.state.collectAsStateWithLifecycle()
    LifecycleStartEffect(model) {
        model.reload()
        onStopOrDispose {}
    }
    Box(modifier.fillMaxSize()) {
        when (val shown = state) {
            NotesState.Loading -> Centered { CircularProgressIndicator() }
            is NotesState.Viewing -> NotesView(shown, model::edit)
            is NotesState.Editing -> NotesEditor(shown, model::type, model::save, model::discard)
            is NotesState.Conflict ->
                NotesConflict(shown, model::keepTheirs, model::keepMine, model::merge)
            NotesState.Unreachable ->
                Retryable("Can't reach pm serve. Tailscale off?", model::reload)
            is NotesState.Failed ->
                Retryable("Couldn't load the notes: ${shown.reason}", model::reload)
        }
    }
}

@Composable
private fun NotesView(state: NotesState.Viewing, edit: () -> Unit) {
    if (state.tooLong) {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("These notes are over 256 KB", style = MaterialTheme.typography.titleMedium)
            Text(
                "Too long to edit or render here: edit them with pm notes on the Mac.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Version("How they start", state.notes.text)
        }
    } else if (state.notes.text.isBlank()) {
        EmptyState("No notes yet", "Notes written here or with pm notes show here.", "Edit", edit)
    } else {
        MarkdownPage(state.notes.text, "Notes") { TextButton(onClick = edit) { Text("Edit") } }
    }
}

@Composable
private fun NotesEditor(
    state: NotesState.Editing,
    type: (String) -> Unit,
    save: () -> Unit,
    discard: () -> Unit,
) {
    var confirming by rememberSaveable { mutableStateOf(false) }
    Column(Modifier.fillMaxSize().imePadding()) {
        OutlinedTextField(
            value = state.draft.text,
            onValueChange = type,
            readOnly = state.saving,
            textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
            placeholder = { Text("Markdown") },
            modifier = Modifier.weight(1f).fillMaxWidth().padding(8.dp),
        )
        Surface(tonalElevation = 2.dp) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp)) {
                if (state.error != null) {
                    Text(
                        state.error,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(4.dp),
                    )
                }
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    if (state.saving) CircularProgressIndicator(Modifier.padding(4.dp))
                    TextButton(
                        onClick = { if (state.changed) confirming = true else discard() },
                        enabled = !state.saving,
                    ) {
                        Text(if (state.changed) "Discard" else "Cancel")
                    }
                    Button(onClick = save, enabled = !state.saving) { Text("Save") }
                }
            }
        }
    }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("Discard the edit?") },
            text = { Text("What you wrote here since the last save is lost.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirming = false
                        discard()
                    }
                ) {
                    Text("Discard")
                }
            },
            dismissButton = {
                TextButton(onClick = { confirming = false }) { Text("Keep editing") }
            },
        )
    }
}

@Composable
private fun NotesConflict(
    state: NotesState.Conflict,
    keepTheirs: () -> Unit,
    keepMine: () -> Unit,
    merge: () -> Unit,
) {
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("The notes changed on the Mac", style = MaterialTheme.typography.titleMedium)
        Text(
            "They were saved elsewhere while you edited here, so your edit was not saved. " +
                "Keep one version, or merge the two by hand.",
            style = MaterialTheme.typography.bodyMedium,
        )
        if (!state.mergeable) {
            Text(
                "Together they are over 256 KB, too long to merge here.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = merge, enabled = state.mergeable) { Text("Merge") }
            OutlinedButton(onClick = keepMine) { Text("Keep mine") }
            OutlinedButton(onClick = keepTheirs) { Text("Keep the Mac's") }
        }
        Version("On the Mac", state.theirs.text)
        Version("Yours", state.mine)
    }
}

@Composable
private fun Version(label: String, text: String) {
    // Text this long stalls layout for seconds; the rest is read on the Mac.
    val shown =
        remember(text) {
            if (text.length <= PREVIEW) text.ifEmpty { "(empty)" }
            else text.take(PREVIEW) + "\n… and ${text.length - PREVIEW} more characters"
        }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Surface(
            color = MaterialTheme.colorScheme.surfaceContainer,
            shape = MaterialTheme.shapes.small,
            modifier = Modifier.fillMaxWidth(),
        ) {
            SelectionContainer {
                Text(
                    shown,
                    style =
                        MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                    textAlign = TextAlign.Start,
                    modifier = Modifier.padding(8.dp),
                )
            }
        }
    }
}

/** The most of a text a notes page shows. */
private const val PREVIEW = 20_000
