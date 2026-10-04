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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.Attention
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Marks
import dev.pm.app.model.Snapshot
import dev.pm.app.model.activity
import java.time.Instant

@Composable
fun ProjectsList(
    snapshot: Snapshot,
    now: Instant,
    open: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier) {
        items(snapshot.projects.sortedBy { it.name }, key = { it.name }) { project ->
            ListItem(
                modifier = Modifier.clickable { open(project.name) },
                headlineContent = { Text(project.name) },
                supportingContent = {
                    val counts = snapshot.attentionCounts(project.name)
                    when {
                        project.skipped != null ->
                            Text(
                                "unreadable: ${project.skipped}",
                                color = MaterialTheme.colorScheme.error,
                            )
                        counts.isEmpty() ->
                            Text(
                                "${snapshot.featuresOf(project.name).size} features, nothing needs you"
                            )
                        else ->
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                counts.forEach { (kind, count) ->
                                    val mark = Marks.attention(kind) ?: return@forEach
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        MarkIcon(mark, kind.wire)
                                        Text(
                                            " $count ${kind.wire}",
                                            style = MaterialTheme.typography.labelMedium,
                                        )
                                    }
                                }
                            }
                    }
                },
                trailingContent = {
                    val main = project.main
                    if (main != null)
                        ActivityLabel(
                            activity(
                                main.working ||
                                    snapshot.featuresOf(project.name).any { it.working },
                                null,
                                now,
                            )
                        )
                },
            )
            HorizontalDivider()
        }
    }
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
                    activity = { ActivityLabel(activity(main.working, main.lastActivity, now)) },
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
                activity = { ActivityLabel(activity(feature.working, feature.lastActivity, now)) },
                open = { open(feature.name) },
            )
            HorizontalDivider()
        }
    }
}

/** What a feature row says under its name: the attention's detail, else its status. */
fun featureLine(feature: FeatureSnapshot): String? =
    feature.attention.detail ?: feature.progress.takeIf { it.isNotEmpty() }

@Composable
private fun ScopeRow(
    name: String,
    attention: Attention,
    line: String?,
    agents: List<AgentSnapshot>,
    closed: Boolean,
    activity: @Composable () -> Unit,
    open: () -> Unit,
) {
    Column(
        Modifier.fillMaxWidth()
            .clickable(onClick = open)
            .padding(horizontal = 16.dp, vertical = 10.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(name, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            AttentionBadge(attention.kindOf, attention.kind)
        }
        val detail = attention.detail?.takeIf { attention.kind != "none" } ?: line
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
            activity()
        }
    }
}

/** A scope's header and agents; a feature also offers its summary. */
@Composable
fun AgentsList(
    snapshot: Snapshot,
    project: String,
    scope: String,
    now: Instant,
    openAgent: (String) -> Unit,
    openSummary: () -> Unit,
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
                attention?.detail?.let { Text(it, style = MaterialTheme.typography.bodyLarge) }
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
                    OutlinedButton(onClick = openSummary) { Text("Summary") }
                } else if (main != null) {
                    ActivityLabel(activity(main.working, main.lastActivity, now))
                }
                if (feature == null && main == null)
                    Text("This scope is no longer in the snapshot.")
            }
            HorizontalDivider()
        }
        items(agents, key = { it.name }) { agent ->
            ListItem(
                modifier = Modifier.clickable { openAgent(agent.name) },
                leadingContent = { MarkIcon(Marks.agent(agent.stateOf), agent.state) },
                headlineContent = { Text(agent.name) },
                supportingContent = {
                    Text(
                        listOfNotNull(agent.state, agent.waiting?.detail).joinToString(": "),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
                trailingContent = { if (agent.unread > 0) AgentBadge(agent, showName = false) },
            )
            HorizontalDivider()
        }
    }
}
