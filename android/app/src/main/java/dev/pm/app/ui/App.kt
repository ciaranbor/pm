package dev.pm.app.ui

import android.Manifest
import android.os.Build
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import dev.pm.app.BuildConfig
import dev.pm.app.R
import dev.pm.app.container
import dev.pm.app.data.Connection
import dev.pm.app.model.Snapshot
import dev.pm.app.push.Notifications
import dev.pm.app.push.Target
import dev.pm.app.update.UpdateWorker
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.serialization.Serializable

/** Where the app can be. Each is a back stack entry, kept across process death. */
@Serializable
sealed interface Route : NavKey {
    @Serializable data object Pair : Route

    @Serializable data object Home : Route

    @Serializable data class Project(val project: String) : Route

    @Serializable data class Scope(val project: String, val scope: String) : Route

    @Serializable
    data class Agent(val project: String, val scope: String, val agent: String) : Route

    @Serializable data class Summary(val project: String, val feature: String) : Route

    @Serializable data class Notes(val project: String) : Route

    /** The feature's `--context` brief. */
    @Serializable data class Brief(val project: String, val feature: String) : Route

    /** The feature's details: branch, base, PR, workflow. */
    @Serializable data class Details(val project: String, val feature: String) : Route

    /** A tool's whole output, by its result's `full` reference. */
    @Serializable
    data class Output(
        val project: String,
        val scope: String,
        val agent: String,
        val ref: String,
        val tool: String,
    ) : Route

    @Serializable data object Settings : Route

    /** The top bar's title, and the line under it that says where it is. */
    val heading: kotlin.Pair<String, String?>
        get() =
            when (this) {
                Pair -> "Pair" to null
                Home -> "pm" to null
                Settings -> "Settings" to null
                is Project -> project to null
                is Scope -> scope to project
                is Agent -> agent to "$project › $scope"
                is Summary -> "Summary" to "$project › $feature"
                is Notes -> "Notes" to project
                is Brief -> "Brief" to "$project › $feature"
                is Details -> "Details" to "$project › $feature"
                is Output -> "$tool output" to "$project › $scope › $agent"
            }
}

/** The back stack a notification's target opens: from the start screen to its scope or agent. */
fun Target.route(): List<Route> =
    listOfNotNull(
        Route.Home,
        Route.Project(project),
        Route.Scope(project, scope),
        agent?.let { Route.Agent(project, scope, it) },
    )

private fun NavBackStack<NavKey>.replaceWith(routes: List<Route>) {
    clear()
    addAll(routes)
}

/**
 * The app's frame and navigation. While shown, it holds the event stream open, and registers for
 * pushes once the server is reached. A `target` from a notification replaces the back stack once
 * paired, then `targetShown` is called.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun App(
    model: AppViewModel,
    target: Target?,
    targetShown: () -> Unit,
    modifier: Modifier = Modifier,
    networkChanges: Flow<Unit> = emptyFlow(),
) {
    val context = LocalContext.current
    val activity = LocalActivity.current
    val pairing by model.pairing.collectAsStateWithLifecycle()
    val client by model.client.collectAsStateWithLifecycle()
    val snapshot by model.snapshot.collectAsStateWithLifecycle()
    val connection by model.connection.collectAsStateWithLifecycle()
    val readAt by model.readAt.collectAsStateWithLifecycle()
    val now by model.now.collectAsStateWithLifecycle()
    val pushKey by model.pushKey.collectAsStateWithLifecycle()
    val backStack = rememberNavBackStack(if (pairing == null) Route.Pair else Route.Home)
    val top = backStack.lastOrNull() as? Route
    val askNotifications =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}

    // A key, not forwarding: the rule cannot tell the two apart.
    @Suppress("ComposeViewModelForwarding")
    LifecycleStartEffect(model) {
        model.start()
        onStopOrDispose { model.stop() }
    }
    LaunchedEffect(pushKey) {
        val key = pushKey ?: return@LaunchedEffect
        Notifications.subscribe(activity ?: return@LaunchedEffect, key)
        model.pushRegistered()
    }
    LaunchedEffect(target, pairing) {
        val to = target ?: return@LaunchedEffect
        if (pairing == null) return@LaunchedEffect
        backStack.replaceWith(to.route())
        targetShown()
    }

    val pairAgain: () -> Unit = { backStack.add(Route.Pair) }
    val (title, subtitle) = top?.heading ?: ("pm" to null)
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        if (subtitle != null) {
                            Text(
                                subtitle,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                },
                navigationIcon = {
                    if (backStack.size > 1) {
                        IconButton(onClick = { backStack.removeLastOrNull() }) {
                            Icon(painterResource(R.drawable.ic_arrow_back), "Back")
                        }
                    }
                },
                actions = {
                    if (top != Route.Settings && top != Route.Pair) {
                        IconButton(onClick = { backStack.add(Route.Settings) }) {
                            Icon(painterResource(R.drawable.ic_settings), "Settings")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).consumeWindowInsets(padding).fillMaxSize()) {
            if (snapshot != null && top != Route.Pair && top != Route.Settings)
                StatusStrip(connection, readAt, now, model::retry, pairAgain)
            if (snapshot?.understood == false) {
                Text(
                    "pm serve sends a newer snapshot than this app knows; update the app.",
                    color = MaterialTheme.colorScheme.error,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.padding(8.dp),
                )
            }
            NavDisplay(
                backStack = backStack,
                entryDecorators =
                    listOf(
                        rememberSaveableStateHolderNavEntryDecorator(),
                        rememberViewModelStoreNavEntryDecorator(),
                    ),
                entryProvider =
                    entryProvider {
                        entry<Route.Pair> {
                            PairScreen(
                                paired = { paired ->
                                    model.pair(paired)
                                    if (
                                        Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                                            !Notifications.allowed(context)
                                    ) {
                                        askNotifications.launch(
                                            Manifest.permission.POST_NOTIFICATIONS
                                        )
                                    }
                                    backStack.replaceWith(listOf(Route.Home))
                                }
                            )
                        }
                        entry<Route.Home> {
                            Shown(snapshot, connection, model::retry, pairAgain) {
                                Home(
                                    it,
                                    now,
                                    openNeed = { need ->
                                        backStack.addAll(
                                            Target(need.project, need.scope, need.agent?.name)
                                                .route()
                                                .drop(1)
                                        )
                                    },
                                    openProject = { p -> backStack.add(Route.Project(p)) },
                                )
                            }
                        }
                        entry<Route.Project> { key ->
                            Shown(snapshot, connection, model::retry, pairAgain) {
                                ScopesList(
                                    it,
                                    key.project,
                                    now,
                                    open = { s -> backStack.add(Route.Scope(key.project, s)) },
                                    openNotes = { backStack.add(Route.Notes(key.project)) },
                                )
                            }
                        }
                        entry<Route.Scope> { key ->
                            Shown(snapshot, connection, model::retry, pairAgain) {
                                AgentsList(
                                    it,
                                    key.project,
                                    key.scope,
                                    now,
                                    openAgent = { a ->
                                        backStack.add(Route.Agent(key.project, key.scope, a))
                                    },
                                    openPage = { backStack.add(it) },
                                )
                            }
                        }
                        entry<Route.Agent> { key ->
                            val paired = client
                            if (paired == null) {
                                Centered { Text("Not paired.") }
                            } else {
                                val shown =
                                    snapshot?.agents(key.project, key.scope)?.find {
                                        it.name == key.agent
                                    }
                                AgentScreen(
                                    paired,
                                    key.project,
                                    key.scope,
                                    key.agent,
                                    shown?.stateOf,
                                    shown?.waiting,
                                    networkChanges,
                                    openResult = { tool, ref ->
                                        backStack.add(
                                            Route.Output(
                                                key.project,
                                                key.scope,
                                                key.agent,
                                                ref,
                                                tool,
                                            )
                                        )
                                    },
                                )
                            }
                        }
                        entry<Route.Summary> { key ->
                            SummaryScreen(
                                viewModel {
                                    ReadModel(client) { summary(key.project, key.feature) }
                                }
                            )
                        }
                        entry<Route.Notes> { key ->
                            val store = LocalContext.current.container.store
                            NotesScreen(viewModel { NotesModel(client, key.project, store) })
                        }
                        entry<Route.Brief> { key ->
                            BriefScreen(
                                viewModel {
                                    ReadModel(client) { feature(key.project, key.feature) }
                                }
                            )
                        }
                        entry<Route.Details> { key ->
                            DetailsScreen(
                                viewModel {
                                    ReadModel(client) { feature(key.project, key.feature) }
                                }
                            )
                        }
                        entry<Route.Output> { key ->
                            ToolOutputScreen(
                                viewModel {
                                    ToolOutputModel(
                                        client,
                                        key.project,
                                        key.scope,
                                        key.agent,
                                        key.ref,
                                    )
                                }
                            )
                        }
                        entry<Route.Settings> {
                            val context = LocalContext.current
                            val container = context.container
                            var checkUpdates by remember {
                                mutableStateOf(container.store.checkUpdates)
                            }
                            val serverVersion by
                                remember(client) { client?.serverVersion ?: MutableStateFlow(null) }
                                    .collectAsStateWithLifecycle()
                            SettingsScreen(
                                pairing = pairing,
                                connection = connection,
                                versions = Versions(BuildConfig.VERSION_NAME, serverVersion),
                                vapid = model::vapid,
                                updates =
                                    UpdateControls(
                                            enabled = checkUpdates,
                                            setEnabled = {
                                                checkUpdates = it
                                                container.store.checkUpdates = it
                                                UpdateWorker.schedule(context, it)
                                            },
                                            checkNow = { UpdateWorker.check(container.http) },
                                        )
                                        .takeIf { BuildConfig.SELF_UPDATE },
                                pair = { backStack.add(Route.Pair) },
                                unpair = {
                                    model.unpair()
                                    backStack.replaceWith(listOf(Route.Pair))
                                },
                            )
                        }
                    },
            )
        }
    }
}

/**
 * The snapshot's content, on a tinted background while it may be stale; with none yet, why not and
 * what to do.
 */
@Composable
internal fun Shown(
    snapshot: Snapshot?,
    connection: Connection,
    retry: () -> Unit,
    pairAgain: () -> Unit,
    content: @Composable (Snapshot) -> Unit,
) {
    when {
        snapshot != null ->
            Box(
                Modifier.fillMaxSize()
                    .background(
                        if (connection == Connection.Live) MaterialTheme.colorScheme.surface
                        else MaterialTheme.colorScheme.surfaceContainer
                    )
            ) {
                content(snapshot)
            }
        connection is Connection.Unreachable ->
            EmptyState(
                "Can't reach pm serve",
                "Tailscale off, or pm serve not running on the Mac?",
                "Retry",
                retry,
            )
        connection == Connection.Unauthorized ->
            EmptyState(
                "Not paired",
                "The Mac revoked this phone's token.",
                "Pair again",
                pairAgain,
            )
        else -> Centered { CircularProgressIndicator() }
    }
}
