package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryScrollableTabRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.api.PmClient
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.Attention
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Marks
import dev.pm.app.model.Snapshot
import dev.pm.app.model.Tone
import dev.pm.app.model.activity
import dev.pm.app.model.prLabel
import dev.pm.app.model.progressLabel
import java.time.Instant
import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.json.Json

/**
 * One scope's screen: a tab per agent, its chat; a feature's then Summary, Brief and Details. It
 * shows the route's tab: `select` puts another in the route, as it does the one the scope most
 * needs looked at (a ready feature's Summary, with Merge) when the route names none.
 */
@Composable
fun Workspace(
    snapshot: Snapshot,
    client: PmClient?,
    route: Route.Scope,
    select: (Tab) -> Unit,
    now: Instant,
    networkChanges: Flow<Unit>,
    topBar: TopBarSlot,
    acting: ActionState,
    ask: (Action) -> Unit,
    modifier: Modifier = Modifier,
    stale: Boolean = false,
    drafts: Drafts = remember { Drafts() },
) {
    val project = route.project
    val scope = route.scope
    val feature = snapshot.feature(project, scope)
    val main = snapshot.project(project)?.main?.takeIf { scope == Snapshot.MAIN }
    val agents = snapshot.agents(project, scope)
    val tabs = tabsOf(scope, agents, route.tab)
    val fallback = defaultTab(feature, main?.attention, agents)
    LaunchedEffect(route.tab == null) { if (route.tab == null) fallback?.let(select) }
    val selected = route.tab?.takeIf { it in tabs } ?: fallback
    var terminal by rememberSaveable { mutableStateOf<String?>(null) }

    val shownAgent = (selected as? Tab.Agent)?.name
    TopBarActions(topBar) {
        agents.find { it.name == shownAgent }?.let { StatePill(it.stateOf, stale) }
        val items = buildList {
            if (shownAgent != null && client != null) {
                add(MenuItem("Terminal") { terminal = shownAgent })
                add(
                    MenuItem("Restart $shownAgent") {
                        ask(Action.Restart(project, scope, shownAgent))
                    }
                )
            }
            if (feature != null) {
                add(MenuItem("Merge") { ask(Action.Merge(project, scope)) })
                add(MenuItem("Delete", destructive = true) { ask(Action.Delete(project, scope)) })
            }
        }
        if (items.isNotEmpty()) OverflowMenu(items)
    }

    val pages = rememberSaveableStateHolder()
    Column(modifier.fillMaxSize()) {
        if (tabs.size > 1) WorkspaceTabs(tabs, selected, agents, select = select)
        Box(Modifier.weight(1f)) {
            val tab = selected
            if (tab == null) {
                EmptyState("No agents here", hint = "Its session isn't running.")
                return@Box
            }
            key(tab) {
                pages.SaveableStateProvider(pageKey(tab)) {
                    when (tab) {
                        is Tab.Agent -> {
                            if (client == null) EmptyState("Not paired.")
                            else {
                                val shown = agents.find { it.name == tab.name }
                                AgentScreen(
                                    client,
                                    project,
                                    scope,
                                    tab.name,
                                    shown?.stateOf,
                                    shown?.waiting,
                                    networkChanges,
                                    openTerminal = { terminal = tab.name },
                                    drafts = drafts,
                                )
                            }
                        }
                        Tab.Summary ->
                            SummaryTab(
                                feature,
                                viewModel(key = "summary") {
                                    ReadModel(client) { summary(project, scope) }
                                },
                                now,
                                stale,
                                merging =
                                    (acting as? ActionState.Running)?.action ==
                                        Action.Merge(project, scope),
                                busy = acting is ActionState.Running,
                                merge = { ask(Action.Merge(project, scope)) },
                            )
                        Tab.Brief -> BriefScreen(infoModel(client, project, scope))
                        Tab.Details -> DetailsScreen(infoModel(client, project, scope))
                    }
                }
            }
        }
    }
    val sheet = terminal
    if (sheet != null && client != null) {
        ScreenSheet(client, project, scope, sheet, dismiss = { terminal = null })
    }
}

@Composable
private fun infoModel(client: PmClient?, project: String, feature: String): ReadModel<FeatureInfo> =
    viewModel(key = "info") { ReadModel(client) { feature(project, feature) } }

/**
 * The scope's tabs: its agents, and the route's should the snapshot not have it yet; then pages.
 */
internal fun tabsOf(scope: String, agents: List<AgentSnapshot>, asked: Tab?): List<Tab> {
    val named = agents.map { Tab.Agent(it.name) }
    val extra = (asked as? Tab.Agent)?.takeIf { it !in named }
    val pages =
        if (scope == Snapshot.MAIN) emptyList() else listOf(Tab.Summary, Tab.Brief, Tab.Details)
    return named + listOfNotNull(extra) + pages
}

/**
 * The tab a scope opens on: a ready feature's Summary; else the agent its attention names, one
 * asking, or its first; a feature without agents, its Summary.
 */
internal fun defaultTab(
    feature: FeatureSnapshot?,
    attention: Attention?,
    agents: List<AgentSnapshot>,
): Tab? {
    val need = feature?.attention ?: attention
    if (feature != null && isReady(feature)) return Tab.Summary
    val agent =
        agents.find { it.name == need?.agent }
            ?: agents.find { it.stateOf == AgentState.Asking }
            ?: agents.firstOrNull()
    return agent?.let { Tab.Agent(it.name) } ?: feature?.let { Tab.Summary }
}

/** The workspace's tabs, `selected` marked; an agent's with its state and unread messages. */
@Composable
fun WorkspaceTabs(
    tabs: List<Tab>,
    selected: Tab?,
    agents: List<AgentSnapshot>,
    select: (Tab) -> Unit,
    modifier: Modifier = Modifier,
) {
    PrimaryScrollableTabRow(
        selectedTabIndex = tabs.indexOf(selected).coerceAtLeast(0),
        modifier = modifier,
        edgePadding = 0.dp,
        minTabWidth = 64.dp,
    ) {
        tabs.forEach { tab ->
            Tab(
                selected = tab == selected,
                onClick = { select(tab) },
                text = { TabLabel(tab, agents) },
            )
        }
    }
}

@Composable
private fun TabLabel(tab: Tab, agents: List<AgentSnapshot>) {
    when (tab) {
        is Tab.Agent -> {
            val agent = agents.find { it.name == tab.name }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
                modifier =
                    Modifier.clearAndSetSemantics {
                        contentDescription = agent?.let(::describe) ?: tab.name
                    },
            ) {
                MarkIcon(Marks.agent(agent?.stateOf ?: AgentState.Unknown), null)
                Text(tab.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (agent != null && agent.unread > 0) {
                    Text(
                        "${agent.unread}",
                        style = MaterialTheme.typography.labelSmall,
                        color = Tone.Caution.color(),
                    )
                }
            }
        }
        Tab.Summary -> Text("Summary")
        Tab.Brief -> Text("Brief")
        Tab.Details -> Text("Details")
    }
}

/**
 * A feature's summary under where it stands; a ready feature's with Merge, its next step, below.
 * `merging` while this feature's merge runs; `busy` while any action does.
 */
@Composable
fun SummaryTab(
    feature: FeatureSnapshot?,
    model: ReadModel<String>,
    now: Instant,
    stale: Boolean,
    merging: Boolean,
    busy: Boolean,
    merge: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier.fillMaxSize()) {
        if (feature != null) {
            StatusHeader(feature, now, stale)
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        }
        SummaryScreen(model, Modifier.weight(1f))
        if (feature != null && isReady(feature)) {
            Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
                PendingButton(
                    "Merge",
                    merge,
                    Modifier.fillMaxWidth()
                        .padding(horizontal = Spacing.gutter, vertical = Spacing.s),
                    pending = merging,
                    enabled = !busy || merging,
                )
            }
        }
    }
}

private fun isReady(feature: FeatureSnapshot): Boolean =
    feature.attention.kindOf == AttentionKind.Ready || feature.progress == AttentionKind.Ready.wire

/** Where a feature stands: its attention and what it says, its status and PR, its activity. */
@Composable
private fun StatusHeader(feature: FeatureSnapshot, now: Instant, stale: Boolean) {
    Selectable {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = Spacing.gutter, vertical = Spacing.s),
            verticalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Spacing.m),
            ) {
                AttentionBadge(feature.attention.kindOf)
                val status = statusLine(feature)
                if (status.isEmpty()) Box(Modifier.weight(1f))
                else
                    Text(
                        status,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                ActivityLabel(
                    activity(feature.working, feature.backgroundSince, feature.lastActivity, now),
                    stale,
                )
            }
            headerNeedLine(feature.attention, feature)?.let {
                Text(it, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }
}

/**
 * The header's need line: as [needLine], except a ready feature's, which is its summary's first
 * line, says what it is waiting for instead.
 */
fun headerNeedLine(attention: Attention, feature: FeatureSnapshot?): String? =
    if (
        attention.kindOf == AttentionKind.Ready &&
            feature != null &&
            attention.detail == feature.summary
    )
        "Ready for review"
    else needLine(attention)

/**
 * A feature's status and its PR's, in words: `In progress · PR #12 open`. A status its attention
 * badge already says (ready, blocked) is left out.
 */
fun statusLine(feature: FeatureSnapshot): String =
    listOfNotNull(
            feature.progress
                .takeIf { it.isNotEmpty() && it != feature.attention.kindOf.wire }
                ?.let { progressLabel(it).replaceFirstChar { c -> c.uppercase() } },
            feature.pr?.let { pr ->
                listOfNotNull("PR #$pr", prLabel(feature.lifecycle)).joinToString(" ")
            },
        )
        .joinToString(" · ")

/** The key a tab's page state is kept under. */
private fun pageKey(tab: Tab): String = Json.encodeToString(Tab.serializer(), tab)
