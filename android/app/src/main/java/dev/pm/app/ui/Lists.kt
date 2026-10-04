package dev.pm.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.model.Activity
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.Attention
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Marks
import dev.pm.app.model.Need
import dev.pm.app.model.ProjectSnapshot
import dev.pm.app.model.Snapshot
import dev.pm.app.model.activity
import java.time.Instant

/** `n` and the noun, plural unless `n` is one. */
fun count(n: Int, noun: String): String = if (n == 1) "1 $noun" else "$n ${noun}s"

/**
 * The start screen: every scope that needs the user, across projects, most urgent first; then the
 * projects. A need opens the agent it names, else its scope.
 */
@Composable
fun Home(
    snapshot: Snapshot,
    now: Instant,
    openNeed: (Need) -> Unit,
    openProject: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val needs = snapshot.needsYou()
    LazyColumn(modifier) {
        item(key = "needs") { Heading("Needs you") }
        if (needs.isEmpty()) {
            item(key = "nothing") {
                Text(
                    "Nothing needs you · ${count(snapshot.workingFeatures(), "feature")} working",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                )
            }
        }
        items(needs, key = { "need/${it.project}/${it.scope}" }) { need ->
            NeedRow(need, open = { openNeed(need) })
            HorizontalDivider()
        }
        item(key = "projects") { Heading("Projects") }
        items(snapshot.projectsByUrgency(), key = { "project/${it.name}" }) { project ->
            ProjectRow(snapshot, project, now, open = { openProject(project.name) })
            HorizontalDivider()
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier =
            Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp).semantics {
                heading()
            },
    )
}

/** What a scope needs, said in words: the attention's detail, else what its kind means. */
fun needLine(attention: Attention): String? =
    attention.detail
        ?: if (attention.kindOf == AttentionKind.Stalled) "every agent idle, no unread messages"
        else null

@Composable
private fun NeedRow(need: Need, open: () -> Unit) {
    val name = "${need.project}/${need.scope}"
    val line = needLine(need.attention)
    val agent = need.agent
    val description =
        listOfNotNull(
                name,
                need.attention.kind,
                line,
                agent?.let(::describe) ?: need.attention.agent,
            )
            .joinToString(", ")
    Column(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .clearAndSetSemantics { contentDescription = description }
            .padding(horizontal = 16.dp, vertical = 10.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                name,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            AttentionBadge(need.attention.kindOf, need.attention.kind)
        }
        if (line != null) {
            Text(
                line,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        when {
            agent != null -> AgentBadge(agent, Modifier.padding(top = 4.dp))
            need.attention.agent != null ->
                Text(
                    need.attention.agent,
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(top = 4.dp),
                )
        }
    }
}

@Composable
private fun ProjectRow(
    snapshot: Snapshot,
    project: ProjectSnapshot,
    now: Instant,
    open: () -> Unit,
) {
    val counts = snapshot.attentionCounts(project.name)
    val features = snapshot.featuresOf(project.name)
    val working =
        project.main?.let { activity(it.working || features.any { f -> f.working }, null, now) }
    val status =
        when {
            project.skipped != null -> "unreadable: ${project.skipped}"
            counts.isEmpty() -> "${count(features.size, "feature")}, nothing needs you"
            else -> counts.joinToString(", ") { (kind, n) -> "$n ${kind.label}" }
        }
    ListItem(
        modifier =
            Modifier.clickable(onClick = open).clearAndSetSemantics {
                contentDescription =
                    listOfNotNull(project.name, status, describe(working)).joinToString(", ")
            },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        headlineContent = { Text(project.name) },
        supportingContent = {
            when {
                project.skipped != null -> Text(status, color = MaterialTheme.colorScheme.error)
                counts.isEmpty() -> Text(status)
                else ->
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        counts.forEach { (kind, n) ->
                            val mark = Marks.attention(kind) ?: return@forEach
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                MarkIcon(mark, null)
                                Text(
                                    " $n ${kind.label}",
                                    style = MaterialTheme.typography.labelMedium,
                                )
                            }
                        }
                    }
            }
        },
        trailingContent = { ActivityLabel(working) },
    )
}

/** A project's main scope, then its features, most urgent first. */
@Composable
fun ScopesList(
    snapshot: Snapshot,
    project: String,
    now: Instant,
    open: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val main = snapshot.project(project)?.main
    LazyColumn(modifier) {
        if (main != null) {
            item(key = "main") {
                ScopeRow(
                    name = Snapshot.MAIN,
                    attention = main.attention,
                    line = null,
                    agents = main.agents,
                    closed = !main.sessionExists,
                    activity = activity(main.working, main.lastActivity, now),
                    open = { open(Snapshot.MAIN) },
                )
                HorizontalDivider()
            }
        }
        items(snapshot.featuresOf(project), key = { it.name }) { feature ->
            ScopeRow(
                name = feature.name,
                attention = feature.attention,
                line = featureLine(feature),
                agents = feature.agents,
                closed = !feature.sessionExists,
                activity = activity(feature.working, feature.lastActivity, now),
                open = { open(feature.name) },
            )
            HorizontalDivider()
        }
    }
}

/** What a feature row says under its name: the attention's detail, else its status. */
fun featureLine(feature: FeatureSnapshot): String? =
    needLine(feature.attention) ?: feature.progress.takeIf { it.isNotEmpty() }

@Composable
private fun ScopeRow(
    name: String,
    attention: Attention,
    line: String?,
    agents: List<AgentSnapshot>,
    closed: Boolean,
    activity: Activity?,
    open: () -> Unit,
) {
    val detail = attention.detail?.takeIf { attention.kindOf != AttentionKind.None } ?: line
    val description =
        listOfNotNull(
                name,
                attention.kind.takeIf { attention.kindOf != AttentionKind.None },
                detail,
                if (closed) "no session" else agents.joinToString(", ", transform = ::describe),
                describe(activity),
            )
            .filter { it.isNotEmpty() }
            .joinToString(", ")
    Column(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .clearAndSetSemantics { contentDescription = description }
            .padding(horizontal = 16.dp, vertical = 10.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(name, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            AttentionBadge(attention.kindOf, attention.kind)
        }
        if (detail != null) {
            Text(
                detail,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Spacer(Modifier.padding(top = 4.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                modifier = Modifier.weight(1f),
            ) {
                if (closed) Text("no session", style = MaterialTheme.typography.labelMedium)
                else agents.forEach { AgentBadge(it) }
            }
            ActivityLabel(activity)
        }
    }
}

/** A scope's header and agents. */
@Composable
fun AgentsList(
    snapshot: Snapshot,
    project: String,
    scope: String,
    now: Instant,
    openAgent: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val feature = snapshot.feature(project, scope)
    val main = snapshot.project(project)?.main?.takeIf { scope == Snapshot.MAIN }
    val agents = snapshot.agents(project, scope)
    LazyColumn(modifier) {
        item(key = "header") {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                val attention = feature?.attention ?: main?.attention
                if (attention != null) AttentionBadge(attention.kindOf, attention.kind)
                attention?.let(::needLine)?.let {
                    Text(it, style = MaterialTheme.typography.bodyLarge)
                }
                if (feature != null) {
                    Text(
                        listOfNotNull(
                                "status ${feature.progress}",
                                feature.lifecycle
                                    .takeIf { it.isNotEmpty() }
                                    ?.let { "lifecycle $it" },
                                feature.pr?.let { "PR $it" },
                            )
                            .joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    feature.summary?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                    ActivityLabel(activity(feature.working, feature.lastActivity, now))
                } else if (main != null) {
                    ActivityLabel(activity(main.working, main.lastActivity, now))
                }
                if (feature == null && main == null)
                    Text("This scope is no longer in the snapshot.")
            }
            HorizontalDivider()
        }
        items(agents, key = { it.name }) { agent ->
            val line = listOfNotNull(agent.state, agent.waiting?.detail).joinToString(": ")
            ListItem(
                modifier =
                    Modifier.clickable { openAgent(agent.name) }
                        .clearAndSetSemantics {
                            contentDescription =
                                listOfNotNull(
                                        agent.name,
                                        line,
                                        agent.unread.takeIf { it > 0 }?.let { "$it unread" },
                                    )
                                    .joinToString(", ")
                        },
                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                leadingContent = { MarkIcon(Marks.agent(agent.stateOf), null) },
                headlineContent = { Text(agent.name) },
                supportingContent = { Text(line, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                trailingContent = { if (agent.unread > 0) AgentBadge(agent, showName = false) },
            )
            HorizontalDivider()
        }
    }
}
