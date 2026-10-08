package dev.pm.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.DocCategory
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** A project's information store: each category, what it holds, and how much and how recent. */
@Composable
fun DocsScreen(
    model: ReadModel<List<DocCategory>>,
    now: Instant,
    open: (DocCategory) -> Unit,
    modifier: Modifier = Modifier,
) {
    // Shown again on the way back from a doc, which may have changed meanwhile.
    DisposableEffect(model) {
        if (model.left) model.refresh()
        onDispose { model.left = true }
    }
    ReadScreen(model, "docs", "This project is no longer there.", modifier) { docs ->
        if (docs.isEmpty()) {
            EmptyState("No docs", hint = "The project's categories.toml lists none.")
        } else {
            LazyColumn(Modifier.fillMaxSize()) {
                items(docs, key = { it.filename }) { doc ->
                    DocRow(doc, now) { open(doc) }
                    RowDivider()
                }
            }
        }
    }
}

@Composable
private fun DocRow(doc: DocCategory, now: Instant, open: () -> Unit) {
    Column(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .heightIn(min = 56.dp)
            .padding(horizontal = Spacing.gutter, vertical = Spacing.m)
    ) {
        Text(doc.title, style = MaterialTheme.typography.titleSmall)
        if (doc.description.isNotBlank()) {
            Text(
                doc.description,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 3,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Text(
            docStatus(doc, now),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** How much a doc holds and when it last changed, or that it holds nothing. */
internal fun docStatus(doc: DocCategory, now: Instant): String {
    if (doc.size == 0L) return "Empty"
    val modified = doc.modified?.let { runCatching { Instant.parse(it) }.getOrNull() }
    val size = if (doc.size < 1024) "${doc.size} B" else "${(doc.size + 512) / 1024} KB"
    return listOfNotNull(size, modified?.let { "updated ${ago(it.toEpochMilli(), now)}" })
        .joinToString(" · ")
}

/**
 * A doc, read-only, as its [markdownParts], each drawn only as it scrolls into view so a doc of
 * thousands of lines opens at once; Sections jumps to a heading.
 */
@Composable
fun DocScreen(model: ReadModel<List<String>>, topBar: TopBarSlot, modifier: Modifier = Modifier) {
    ReadScreen(model, "doc", "This doc is no longer listed.", modifier) { parts ->
        if (parts.isEmpty()) {
            EmptyState("Nothing written yet")
        } else {
            val list = rememberLazyListState()
            val headings = remember(parts) { sectionHeadings(parts) }
            if (headings.size > 1) {
                val scope = rememberCoroutineScope()
                TopBarActions(topBar) {
                    SectionsMenu(headings) { at -> scope.launch { list.scrollToItem(at) } }
                }
            }
            LazyColumn(
                Modifier.fillMaxSize(),
                state = list,
                contentPadding = PaddingValues(Spacing.gutter),
            ) {
                items(parts) { part ->
                    Selectable { PmMarkdown(part, Modifier.padding(bottom = Spacing.m)) }
                }
            }
        }
    }
}

@Composable
private fun SectionsMenu(headings: List<Section>, jump: (Int) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { open = true }) { Text("Sections") }
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            headings.forEach { section ->
                DropdownMenuItem(
                    text = {
                        Text(
                            section.title,
                            Modifier.padding(start = Spacing.l * (section.level - 1)),
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    },
                    onClick = {
                        open = false
                        jump(section.at)
                    },
                )
            }
        }
    }
}

private val TOP_HEADING = Regex("""^ {0,3}(#{1,2})\s+(.*?)\s*#*\s*$""")

/** A heading of a doc: the part it opens, its level, and its text. */
internal data class Section(val at: Int, val level: Int, val title: String)

/** The parts that open with a first- or second-level heading. */
internal fun sectionHeadings(parts: List<String>): List<Section> =
    parts.mapIndexedNotNull { i, part ->
        TOP_HEADING.find(part.lineSequence().first())?.let { heading ->
            val (marks, text) = heading.destructured
            Section(i, marks.length, plainExcerpt(text))
        }
    }

/**
 * Whether the server serves `project`'s docs: so until it answers that it predates them, so a
 * server that has them never shows the entry late.
 */
@Composable
internal fun rememberDocsServed(client: PmClient?, project: String): Boolean {
    val served by
        produceState(true, client, project) {
            value =
                try {
                    client?.docs(project)
                    true
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Unsupported) {
                    false
                } catch (e: Exception) {
                    true
                }
        }
    return served
}
