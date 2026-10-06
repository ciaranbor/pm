package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.mikepenz.markdown.compose.components.CurrentComponentsBridge
import com.mikepenz.markdown.compose.components.MarkdownComponentModel
import com.mikepenz.markdown.compose.components.MarkdownComponents
import com.mikepenz.markdown.compose.components.markdownComponents
import dev.pm.app.R

/**
 * A project's notes: rendered to read, raw to edit, and a refused save's two texts to settle. Edit
 * opens at the section being read.
 */
@Composable
fun NotesScreen(model: NotesModel, topBar: TopBarSlot, modifier: Modifier = Modifier) {
    val state by model.state.collectAsStateWithLifecycle()
    LifecycleStartEffect(model) {
        model.reload()
        onStopOrDispose {}
    }
    Box(modifier.fillMaxSize()) {
        when (val shown = state) {
            NotesState.Loading -> Centered { CircularProgressIndicator() }
            is NotesState.Viewing -> NotesView(shown, model::edit)
            is NotesState.Editing -> NotesEditor(model, shown, topBar)
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
private fun NotesView(state: NotesState.Viewing, edit: (Int) -> Unit) {
    if (state.tooLong) {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("These notes are over 256 KB", style = MaterialTheme.typography.titleMedium)
            Text(
                "Too long to edit or render here: edit them with pm notes on the server.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Version("How they start", state.notes.text)
        }
    } else if (state.notes.text.isBlank()) {
        EmptyState(
            "No notes yet",
            "Notes written here or with pm notes show here.",
            "Edit",
            onAction = { edit(0) },
        )
    } else {
        val sections = remember(state.notes.text) { Sections() }
        val slack = with(LocalDensity.current) { 24.dp.toPx() }
        Box(Modifier.fillMaxSize()) {
            MarkdownPage(
                state.notes.text,
                "Notes",
                modifier = Modifier.onGloballyPositioned { sections.page = it },
                components = remember(sections) { sections.components() },
                bottom = 88.dp,
            )
            ExtendedFloatingActionButton(
                onClick = { edit(sections.reading(slack)) },
                modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
            ) {
                Icon(painterResource(R.drawable.ic_edit), null)
                Text("Edit", Modifier.padding(start = 12.dp))
            }
        }
    }
}

/** Where the rendered notes' headings are, to tell which section is being read. */
private class Sections {
    var page: LayoutCoordinates? = null
    private val headings = mutableMapOf<Int, LayoutCoordinates>()

    /** The source offset of the heading of the section at the top of the page, `slack` px down. */
    fun reading(slack: Float): Int {
        val page = page?.takeIf { it.isAttached } ?: return 0
        return headings
            .filterValues { it.isAttached }
            .mapValues { (_, at) -> page.localPositionOf(at, Offset.Zero).y }
            .filterValues { it <= slack }
            .maxByOrNull { it.value }
            ?.key ?: 0
    }

    fun components(): MarkdownComponents {
        val bridge = CurrentComponentsBridge
        return markdownComponents(
            heading1 = tracked(bridge.heading1),
            heading2 = tracked(bridge.heading2),
            heading3 = tracked(bridge.heading3),
            heading4 = tracked(bridge.heading4),
            heading5 = tracked(bridge.heading5),
            heading6 = tracked(bridge.heading6),
            setextHeading1 = tracked(bridge.setextHeading1),
            setextHeading2 = tracked(bridge.setextHeading2),
        )
    }

    private fun tracked(
        draw: @Composable (MarkdownComponentModel) -> Unit
    ): @Composable (MarkdownComponentModel) -> Unit = { model ->
        Box(Modifier.onGloballyPositioned { headings[model.node.startOffset] = it }) { draw(model) }
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
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Surface(
            color = MaterialTheme.colorScheme.tertiaryContainer,
            shape = MaterialTheme.shapes.medium,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "The notes changed on the server",
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onTertiaryContainer,
                )
                Text(
                    "They were saved elsewhere while you edited here, so your edit was not " +
                        "saved. Keep one version, or merge the two by hand.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onTertiaryContainer,
                )
                if (!state.mergeable) {
                    Text(
                        "Together they are over 256 KB, too long to merge here.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onTertiaryContainer,
                    )
                }
            }
        }
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(onClick = merge, enabled = state.mergeable) { Text("Merge") }
            OutlinedButton(onClick = keepMine) { Text("Keep mine") }
            OutlinedButton(onClick = keepTheirs) { Text("Keep the server's") }
        }
        Version("On the server", state.theirs.text)
        Version("Yours", state.mine)
    }
}

@Composable
private fun Version(label: String, text: String) {
    // Text this long stalls layout for seconds; the rest is read on the server.
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
