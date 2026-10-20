package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.res.painterResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.Attention
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Marks
import dev.pm.app.model.Snapshot
import dev.pm.app.model.activity
import dev.pm.app.model.prLabel
import dev.pm.app.model.progressLabel
import java.time.Instant
import kotlinx.coroutines.flow.Flow

/**
 * One scope's screen: a tab per agent once there are two, and the chat of the route's agent. It
 * shows the route's agent: `select` puts another in the route, as it does the one the scope most
 * needs looked at when the route names none, or names one the snapshot had and has no longer. A
 * feature's ⓘ calls `openFeature`, as does its ready strip; a feature without agents shows its page
 * here instead.
 */
@Composable
fun Workspace(
    snapshot: Snapshot,
    client: PmClient?,
    route: Route.Scope,
    select: (String?) -> Unit,
    openFeature: () -> Unit,
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
    val names = agents.map { it.name }
    // The route's agent was deleted once a snapshot has had it since the route named it, and the
    // latest doesn't: a name just added is kept until a snapshot has it.
    var arrived by remember(project, scope, route.agent) { mutableStateOf(false) }
    LaunchedEffect(route.agent, names) { if (route.agent in names) arrived = true }
    val gone = route.agent?.takeIf { arrived && it !in names }
    val tabs = tabsOf(agents, route.agent?.takeIf { it != gone })
    val fallback = defaultAgent(feature, main?.attention, agents)
    LaunchedEffect(route.agent == null) { if (route.agent == null) fallback?.let(select) }
    LaunchedEffect(gone) { if (gone != null) select(fallback) }
    val selected = route.agent?.takeIf { it in tabs } ?: fallback
    val mergeBlocker = feature?.let {
        mergeBlocker(client, project, scope, it, acting, recheck = selected == null)
    }
    val running = feature?.sessionExists ?: main?.sessionExists ?: false
    var adding by rememberSaveable { mutableStateOf(false) }
    var forking by rememberSaveable { mutableStateOf<String?>(null) }
    if (adding && client != null) {
        AddAgentDialog(
            client,
            project,
            scope,
            names,
            submit = {
                adding = false
                ask(it)
            },
            dismiss = { adding = false },
        )
    }
    forking?.let { source ->
        ForkDialog(
            project,
            scope,
            source,
            names,
            submit = {
                forking = null
                ask(it)
            },
            dismiss = { forking = null },
        )
    }
    val addAgent = { adding = true }.takeIf { client != null && running }

    if (selected == null) {
        if (feature == null)
            if (addAgent != null)
                EmptyState("No agents here", action = "Add agent", onAction = addAgent)
            else EmptyState("No agents here", hint = "Its session isn't running.")
        else {
            var page by rememberSaveable { mutableStateOf(Page.Summary) }
            FeatureActions(topBar, feature, mergeBlocker, ask, addAgent)
            FeatureScreen(
                project,
                scope,
                feature,
                page,
                { page = it },
                client,
                now,
                stale,
                acting,
                mergeBlocker,
                ask,
                modifier,
                addAgent,
            )
        }
        return
    }

    var terminal by rememberSaveable { mutableStateOf<String?>(null) }
    val modes = client?.let { modeModel(it, project, scope, selected) }
    modes?.let { FollowMode(it, selected, agents.find { a -> a.name == selected }?.stateOf) }
    val mode = modes?.mode?.collectAsStateWithLifecycle()?.value
    val switching = modes?.switching?.collectAsStateWithLifecycle()?.value
    TopBarActions(topBar) {
        if (modes != null && mode != null) {
            ModeChip(mode, switching, open = modes::refresh, switch = modes::switch)
        }
        agents.find { it.name == selected }?.let { StatePill(it.stateOf, stale) }
        if (feature != null) {
            IconButton(onClick = openFeature) {
                Icon(painterResource(R.drawable.ic_info), "Feature info")
            }
        }
        val state = agents.find { it.name == selected }?.stateOf
        val items = buildList {
            if (client != null) {
                add(MenuItem("Terminal", R.drawable.ic_terminal) { terminal = selected })
                add(
                    MenuItem("Restart $selected", R.drawable.ic_autorenew) {
                        ask(Action.Restart(project, scope, selected))
                    }
                )
                addAll(agentItems(project, scope, selected, state, running, ask))
                if (running) {
                    add(MenuItem("Fork $selected…", R.drawable.ic_fork) { forking = selected })
                    add(MenuItem("Add agent…", R.drawable.ic_add) { adding = true })
                }
            }
            if (feature != null) add(mergeItem(feature, mergeBlocker, ask))
            if (client != null)
                add(
                    MenuItem("Delete $selected", R.drawable.ic_delete, destructive = true) {
                        ask(Action.DeleteAgent(project, scope, selected))
                    }
                )
            if (feature != null) add(deleteItem(feature, ask))
        }
        ActionMenu(items)
    }

    val chats = rememberSaveableStateHolder()
    Column(modifier.fillMaxSize()) {
        if (tabs.size > 1) WorkspaceTabs(tabs, selected, agents, select = select, stale = stale)
        Box(Modifier.weight(1f)) {
            key(selected) {
                chats.SaveableStateProvider(selected) {
                    if (client == null) EmptyState("Not paired.")
                    else {
                        val shown = agents.find { it.name == selected }
                        AgentScreen(
                            client,
                            project,
                            scope,
                            selected,
                            shown?.stateOf,
                            shown?.waiting,
                            networkChanges,
                            openTerminal = { terminal = selected },
                            drafts = drafts,
                            banner =
                                if (feature != null && isReady(feature)) {
                                    { ReadyStrip(openFeature) }
                                } else null,
                        )
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
private fun modeModel(client: PmClient, project: String, scope: String, agent: String): ModeModel =
    viewModel(key = "mode/$project/$scope/$agent") { ModeModel(client, project, scope, agent) }

/**
 * Keep `model`, `agent`'s mode, read as it opens and again as its `state` changes, since a mode can
 * be switched at the keyboard; each switch's outcome goes to the snackbar.
 */
@Composable
private fun FollowMode(model: ModeModel, agent: String, state: AgentState?) {
    LaunchedEffect(model, state) { model.refresh() }
    val feedback = LocalFeedback.current
    LaunchedEffect(model) {
        model.switched.collect {
            when (it) {
                is ModeSwitched.Done -> feedback.done("$agent is in ${it.label}")
                is ModeSwitched.Refused -> feedback.failed(it.text)
                is ModeSwitched.Failed ->
                    feedback.failed("Couldn't switch mode: ${it.reason}") { model.switch(it.id) }
            }
        }
    }
}

/**
 * Stop for an agent whose session is up, Start for one stopped or dead while its scope's session
 * `running`: the CLI's spawn, which heals a dead one.
 */
internal fun agentItems(
    project: String,
    scope: String,
    agent: String,
    state: AgentState?,
    running: Boolean,
    ask: (Action) -> Unit,
): List<MenuItem> =
    when (state) {
        AgentState.Stopped,
        AgentState.Dead ->
            if (running)
                listOf(
                    MenuItem("Start $agent", R.drawable.ic_play_arrow) {
                        ask(Action.Start(project, scope, agent))
                    }
                )
            else emptyList()
        AgentState.Closed,
        null -> emptyList()
        else ->
            listOf(
                MenuItem("Stop $agent", R.drawable.ic_stop) {
                    val working =
                        state == AgentState.Busy ||
                            state == AgentState.Asking ||
                            state == AgentState.Background
                    ask(Action.Stop(project, scope, agent, working))
                }
            )
    }

/** The scope's agents, and the route's should the snapshot not have it yet. */
internal fun tabsOf(agents: List<AgentSnapshot>, asked: String?): List<String> {
    val named = agents.map { it.name }
    return named + listOfNotNull(asked?.takeIf { it !in named })
}

/** The agent a scope opens on: the one its attention names, else one asking, else its first. */
internal fun defaultAgent(
    feature: FeatureSnapshot?,
    attention: Attention?,
    agents: List<AgentSnapshot>,
): String? {
    val need = feature?.attention ?: attention
    return (agents.find { it.name == need?.agent }
            ?: agents.find { it.stateOf == AgentState.Asking }
            ?: agents.firstOrNull())
        ?.name
}

/** Over a ready feature's chat: that it is ready, and the way to its summary. */
@Composable
private fun ReadyStrip(openFeature: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
        Row(
            Modifier.fillMaxWidth().padding(start = Spacing.gutter, end = Spacing.xs),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
            Marks.attention(AttentionKind.Ready)?.let { MarkIcon(it, null) }
            Text(
                "Ready for review",
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = openFeature) { Text("Summary") }
        }
    }
}

internal fun isReady(feature: FeatureSnapshot): Boolean =
    feature.attention.kindOf == AttentionKind.Ready || feature.progress == AttentionKind.Ready.wire

/** Where a feature stands: its attention and what it says, its status and PR, its activity. */
@Composable
internal fun StatusHeader(feature: FeatureSnapshot, now: Instant, stale: Boolean) {
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
 * A feature's status and its PR's, in words: `in progress · PR #12 open`. A status its attention
 * badge already says (ready, blocked) is left out.
 */
fun statusLine(feature: FeatureSnapshot): String =
    listOfNotNull(
            feature.progress
                .takeIf { it.isNotEmpty() && it != feature.attention.kindOf.wire }
                ?.let(::progressLabel),
            feature.pr?.let { pr ->
                listOfNotNull("PR #$pr", prLabel(feature.lifecycle)).joinToString(" ")
            },
        )
        .joinToString(" · ")
