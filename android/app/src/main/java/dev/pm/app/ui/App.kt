package dev.pm.app.ui

import android.Manifest
import android.os.Build
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
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
import dev.pm.app.R
import dev.pm.app.data.Connection
import dev.pm.app.model.Snapshot
import dev.pm.app.push.Notifications
import dev.pm.app.push.Target
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.serialization.Serializable

/** Where the app can be. Each is a back stack entry, kept across process death. */
@Serializable
sealed interface Route : NavKey {
    @Serializable data object Pair : Route

    @Serializable data object Projects : Route

    @Serializable data class Project(val project: String) : Route

    @Serializable data class Scope(val project: String, val scope: String) : Route

    @Serializable
    data class Agent(val project: String, val scope: String, val agent: String) : Route

    @Serializable data class Summary(val project: String, val feature: String) : Route

    @Serializable data object Settings : Route

    val title: String
        get() =
            when (this) {
                Pair -> "Pair"
                Projects -> "pm"
                Settings -> "Settings"
                is Project -> project
                is Scope -> "$project/$scope"
                is Agent -> "$scope: $agent"
                is Summary -> "$feature summary"
            }
}

/** The back stack a notification's target opens: down from the projects to its scope or agent. */
fun Target.route(): List<Route> =
    listOfNotNull(
        Route.Projects,
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
    val backStack = rememberNavBackStack(if (pairing == null) Route.Pair else Route.Projects)
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

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(top?.title ?: "pm") },
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
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (top != Route.Pair && top != Route.Settings)
                ConnectionBanner(connection, readAt, model::retry)
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
                                    backStack.replaceWith(listOf(Route.Projects))
                                }
                            )
                        }
                        entry<Route.Projects> {
                            Shown(snapshot, connection) {
                                ProjectsList(
                                    it,
                                    now,
                                    open = { p -> backStack.add(Route.Project(p)) },
                                )
                            }
                        }
                        entry<Route.Project> { key ->
                            Shown(snapshot, connection) {
                                ScopesList(
                                    it,
                                    key.project,
                                    now,
                                    open = { s -> backStack.add(Route.Scope(key.project, s)) },
                                )
                            }
                        }
                        entry<Route.Scope> { key ->
                            Shown(snapshot, connection) {
                                AgentsList(
                                    it,
                                    key.project,
                                    key.scope,
                                    now,
                                    openAgent = { a ->
                                        backStack.add(Route.Agent(key.project, key.scope, a))
                                    },
                                    openSummary = {
                                        backStack.add(Route.Summary(key.project, key.scope))
                                    },
                                )
                            }
                        }
                        entry<Route.Agent> { key ->
                            val paired = client
                            if (paired == null) {
                                Centered { Text("Not paired.") }
                            } else {
                                AgentScreen(
                                    paired,
                                    key.project,
                                    key.scope,
                                    key.agent,
                                    networkChanges,
                                )
                            }
                        }
                        entry<Route.Summary> { key ->
                            SummaryScreen(
                                viewModel { SummaryModel(client, key.project, key.feature) }
                            )
                        }
                        entry<Route.Settings> {
                            SettingsScreen(
                                pairing = pairing,
                                connection = connection,
                                vapid = model::vapid,
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

@Composable
private fun Shown(
    snapshot: Snapshot?,
    connection: Connection,
    content: @Composable (Snapshot) -> Unit,
) {
    when {
        snapshot != null -> content(snapshot)
        connection is Connection.Unreachable ->
            Centered {
                Text("Server unreachable: connect Tailscale to open.", textAlign = TextAlign.Center)
            }
        else -> Centered { CircularProgressIndicator() }
    }
}
