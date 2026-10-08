package dev.pm.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.Activity
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.Attention
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Mark
import dev.pm.app.model.Marks
import dev.pm.app.model.Need
import dev.pm.app.model.ProjectSnapshot
import dev.pm.app.model.Snapshot
import dev.pm.app.model.Tone
import dev.pm.app.model.activity
import dev.pm.app.model.progressLabel
import dev.pm.app.model.span
import java.time.Duration
import java.time.Instant

/** `n` and the noun, plural unless `n` is one. */
fun count(n: Int, noun: String): String = if (n == 1) "1 $noun" else "$n ${noun}s"

/**
 * The start screen: every scope that needs the user, across projects, most urgent first; then those
 * at work; then the projects. A scope opens its workspace.
 */
@Composable
fun Home(
    snapshot: Snapshot,
    now: Instant,
    openScope: (project: String, scope: String) -> Unit,
    openProject: (String) -> Unit,
    modifier: Modifier = Modifier,
    stale: Boolean = false,
) {
    val needs = snapshot.needsYou()
    val atWork = snapshot.atWork()
    // A project with no session up is closed, and listed apart, after the rest.
    val (shown, closed) =
        snapshot.projectsByUrgency().partition { it.skipped != null || snapshot.anyOpen(it.name) }
    LazyColumn(modifier) {
        item(key = "needs") { Heading("Needs you") }
        if (needs.isEmpty()) {
            item(key = "nothing") {
                Text(
                    "Nothing needs you",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(Rows.padding),
                )
            }
        }
        needRows(needs, now) { openScope(it.project, it.scope) }
        if (atWork.isNotEmpty()) {
            item(key = "working") { Heading("Working") }
            items(atWork, key = { "working/${it.project}/${it.scope}" }) { scope ->
                WorkingRow(scope, now, stale) { openScope(scope.project, scope.scope) }
                RowDivider()
            }
        }
        if (shown.isNotEmpty()) item(key = "projects") { Heading("Projects") }
        items(shown, key = { "project/${it.name}" }) { project ->
            ProjectRow(snapshot, project, now, stale, open = { openProject(project.name) })
            RowDivider()
        }
        if (closed.isNotEmpty()) item(key = "closed") { Heading("Closed projects") }
        items(closed.sortedBy { it.name }, key = { "closed/${it.name}" }) { project ->
            ProjectRow(snapshot, project, now, stale, open = { openProject(project.name) })
            RowDivider()
        }
    }
}

private fun LazyListScope.needRows(
    needs: List<Need>,
    now: Instant,
    open: (Need) -> Unit,
) =
    items(needs, key = { "need/${it.project}/${it.scope}" }) { need ->
        val kind = need.attention.kindOf
        val age = need.since?.let { span(Duration.between(it, now).seconds.coerceAtLeast(0)) }
        ScopeRow(
            name = need.scope,
            place = need.project,
            mark = Marks.attention(kind),
            status = listOfNotNull(kind.label, age).joinToString(" · "),
            line = preview(need),
            unread = need.agents.sumOf { it.unread },
            open = { open(need) },
        )
        RowDivider()
    }

/**
 * What a need's row says it is about: the attention's detail, else the question or command its
 * agent waits on.
 */
fun preview(need: Need): String? =
    needLine(need.attention) ?: need.agent?.waiting?.detail?.takeIf { it.isNotBlank() }

@Composable
private fun WorkingRow(scope: Need, now: Instant, stale: Boolean, open: () -> Unit) {
    val activity = activity(scope.working, scope.backgroundSince, null, now)
    val state = if (activity == Activity.Working) AgentState.Busy else AgentState.Background
    val mark = Marks.agent(state).let { if (stale) it.copy(tone = Tone.Neutral) else it }
    ScopeRow(
        name = scope.scope,
        place = scope.project,
        mark = mark,
        status = describe(activity, stale),
        line = workingLine(scope),
        unread = scope.agents.sumOf { it.unread },
        open = open,
    )
}

/**
 * What a working row says: its agents at work, after `ready` for a feature its team marked so while
 * an agent still works, which pm doesn't rank as needing the user.
 */
fun workingLine(scope: Need): String? =
    listOfNotNull(
            scope.progress.takeIf { it == AttentionKind.Ready.wire }?.let(::progressLabel),
            scope.agents
                .filter { it.stateOf == AgentState.Busy || it.stateOf == AgentState.Background }
                .joinToString(", ") { it.name }
                .ifEmpty { null },
        )
        .joinToString(" · ")
        .ifEmpty { null }

/** Between list rows: inset, so the rows read as one list rather than boxes. */
@Composable
internal fun RowDivider() =
    HorizontalDivider(
        Modifier.padding(start = Rows.dividerInset + MARK_SLOT),
        color = MaterialTheme.colorScheme.outlineVariant,
    )

/** The width a row's leading mark takes, with its gap. */
private val MARK_SLOT = 18.dp + Spacing.m

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier =
            Modifier.padding(
                    start = Spacing.gutter,
                    end = Spacing.gutter,
                    top = Spacing.l,
                    bottom = Spacing.xs,
                )
                .semantics { heading() },
    )
}

/** What a scope needs, said in words: the attention's detail, else what its kind means. */
fun needLine(attention: Attention): String? =
    when (attention.kindOf) {
        AttentionKind.Stalled -> attention.detail ?: "every agent idle, no unread messages"
        AttentionKind.Ready -> attention.detail?.let(::plainExcerpt)
        else -> attention.detail
    }

/**
 * A list row for a scope: its mark, name and where it is, then a line of what it is about; on the
 * right, its status in the mark's tone, and unread messages.
 */
@Composable
private fun ScopeRow(
    name: String,
    place: String?,
    mark: Mark?,
    status: String?,
    line: String?,
    unread: Int,
    open: () -> Unit,
) {
    val description =
        listOfNotNull(
                name,
                place,
                status,
                line,
                unread.takeIf { it > 0 }?.let { "$it unread" },
            )
            .joinToString(", ")
    Row(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .clearAndSetSemantics { contentDescription = description }
            .heightIn(min = 56.dp)
            .padding(Rows.padding),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.width(MARK_SLOT)) { if (mark != null) MarkIcon(mark, null) }
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        name,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    if (place != null) {
                        Text(
                            "  $place",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                        )
                    }
                }
                if (status != null) {
                    Text(
                        status,
                        style = MaterialTheme.typography.labelMedium,
                        color = mark?.tone?.color() ?: Tone.Neutral.color(),
                        fontWeight = if (mark?.strong == true) FontWeight.Bold else null,
                        maxLines = 1,
                        modifier = Modifier.padding(start = Spacing.s),
                    )
                }
            }
            if (line != null || unread > 0) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        line.orEmpty(),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                    if (unread > 0) Unread(unread)
                }
            }
        }
    }
}

/** A count of unread messages: an envelope and the number. */
@Composable
private fun Unread(n: Int) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xxs),
        modifier = Modifier.padding(start = Spacing.s),
    ) {
        Icon(
            painterResource(R.drawable.ic_mail),
            null,
            tint = Tone.Caution.color(),
            modifier = Modifier.size(14.dp),
        )
        Text("$n", style = MaterialTheme.typography.labelMedium, color = Tone.Caution.color())
    }
}

@Composable
private fun ProjectRow(
    snapshot: Snapshot,
    project: ProjectSnapshot,
    now: Instant,
    stale: Boolean,
    open: () -> Unit,
) {
    val counts = snapshot.attentionCounts(project.name)
    val features = snapshot.featuresOf(project.name)
    val working =
        activity(project.main?.working == true || features.any { it.working }, null, null, now)
    val status =
        when {
            project.skipped != null -> "unreadable: ${project.skipped}"
            counts.isEmpty() -> count(features.size, "feature")
            else ->
                count(features.size, "feature") +
                    " · " +
                    counts.joinToString(", ") { (kind, n) -> "$n ${kind.label}" }
        }
    Row(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .clearAndSetSemantics {
                contentDescription =
                    listOfNotNull(project.name, status, describe(working, stale)).joinToString(", ")
            }
            .heightIn(min = 56.dp)
            .padding(Rows.padding),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.width(MARK_SLOT))
        Column(Modifier.weight(1f)) {
            Text(project.name, style = MaterialTheme.typography.titleSmall)
            Text(
                status,
                style = MaterialTheme.typography.bodyMedium,
                color =
                    if (project.skipped != null) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        ActivityLabel(working, stale)
    }
}

/** A project's main scope, then its features, most urgent first. */
@Composable
fun ScopesList(
    snapshot: Snapshot,
    project: String,
    now: Instant,
    open: (String) -> Unit,
    openNotes: () -> Unit,
    /** Null where the server has no docs to serve. */
    openDocs: (() -> Unit)?,
    modifier: Modifier = Modifier,
    stale: Boolean = false,
) {
    val main = snapshot.project(project)?.main
    LazyColumn(modifier) {
        item(key = "header") {
            Row(
                Modifier.padding(horizontal = Spacing.gutter, vertical = Spacing.s),
                horizontalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                OutlinedButton(openNotes) { Text("Notes") }
                if (openDocs != null) OutlinedButton(openDocs) { Text("Docs") }
            }
        }
        if (main != null) {
            item(key = "main") {
                ProjectScopeRow(
                    name = Snapshot.MAIN,
                    attention = main.attention,
                    line = needLine(main.attention),
                    agents = main.agents,
                    closed = !main.sessionExists,
                    activity = activity(main.working, main.backgroundSince, main.lastActivity, now),
                    stale = stale,
                    open = { open(Snapshot.MAIN) },
                )
                RowDivider()
            }
        }
        items(snapshot.featuresOf(project), key = { it.name }) { feature ->
            ProjectScopeRow(
                name = feature.name,
                attention = feature.attention,
                line = featureLine(feature),
                agents = feature.agents,
                closed = !feature.sessionExists,
                activity =
                    activity(feature.working, feature.backgroundSince, feature.lastActivity, now),
                stale = stale,
                open = { open(feature.name) },
            )
            RowDivider()
        }
    }
}

/** What a feature row says under its name: the attention's detail, else its status. */
fun featureLine(feature: FeatureSnapshot): String? =
    needLine(feature.attention) ?: progressLabel(feature.progress).takeIf { it.isNotEmpty() }

/**
 * A scope on its project's page: its attention where it needs the user, else what it is doing; a
 * scope without a session says so.
 */
@Composable
private fun ProjectScopeRow(
    name: String,
    attention: Attention,
    line: String?,
    agents: List<AgentSnapshot>,
    closed: Boolean,
    activity: Activity?,
    stale: Boolean,
    open: () -> Unit,
) {
    val kind = attention.kindOf
    val needs = kind != AttentionKind.None
    val state =
        when (activity) {
            Activity.Working -> AgentState.Busy
            is Activity.Background -> AgentState.Background
            else -> null
        }
    val mark =
        Marks.attention(kind)
            ?: state?.let { s ->
                Marks.agent(s).let { if (stale) it.copy(tone = Tone.Neutral) else it }
            }
    val status =
        when {
            needs -> listOfNotNull(kind.label, describe(activity, stale)).joinToString(" · ")
            closed -> "no session"
            else -> describe(activity, stale)
        }
    ScopeRow(
        name = name,
        place = null,
        mark = mark,
        status = status,
        line = line,
        unread = agents.sumOf { it.unread },
        open = open,
    )
}
