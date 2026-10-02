package dev.pm.app

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Settings
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
import androidx.compose.runtime.produceState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import dev.pm.app.data.Connection
import dev.pm.app.model.Snapshot
import dev.pm.app.push.Notifications
import dev.pm.app.push.Target
import dev.pm.app.ui.AgentModel
import dev.pm.app.ui.AgentScreen
import dev.pm.app.ui.AgentsList
import dev.pm.app.ui.Centered
import dev.pm.app.ui.ConnectionBanner
import dev.pm.app.ui.PairScreen
import dev.pm.app.ui.PmTheme
import dev.pm.app.ui.ProjectsList
import dev.pm.app.ui.ScopesList
import dev.pm.app.ui.SettingsScreen
import dev.pm.app.ui.SummaryScreen
import kotlinx.coroutines.delay
import java.time.Instant
import kotlin.time.Duration.Companion.seconds

class MainActivity : ComponentActivity() {
    /** Where a tapped notification leads, until navigation has gone there. */
    private var target by mutableStateOf<Target?>(null)

    /** Whether this run already asked a distributor for a subscription. */
    private var subscribing = false

    private val askNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        target = Target.from(intent)
        setContent { PmTheme { App() } }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        target = Target.from(intent)
    }

    override fun onStart() {
        super.onStart()
        repository.start()
    }

    override fun onStop() {
        repository.stop()
        super.onStop()
    }

    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !Notifications.allowed(this)) {
            askNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    /** Once connected, subscribe for pushes if no distributor has given a subscription yet. */
    private suspend fun subscribe() {
        if (subscribing || !repository.needsSubscription) return
        subscribing = true
        val vapid = runCatching { repository.client()?.vapidKey() }.getOrNull()
        if (vapid == null) {
            subscribing = false
            return
        }
        Notifications.subscribe(this, vapid)
    }

    @OptIn(ExperimentalMaterial3Api::class)
    @Composable
    private fun App() {
        val nav = rememberNavController()
        val pairing by repository.pairing.collectAsStateWithLifecycle()
        val snapshot by repository.snapshot.collectAsStateWithLifecycle()
        val connection by repository.connection.collectAsStateWithLifecycle()
        val readAt by repository.readAt.collectAsStateWithLifecycle()
        val now by produceState(Instant.now()) {
            while (true) {
                delay(30.seconds)
                value = Instant.now()
            }
        }
        val entry by nav.currentBackStackEntryAsState()
        val route = entry?.destination?.route

        LaunchedEffect(connection) {
            if (connection == Connection.Live) subscribe()
        }
        LaunchedEffect(target, pairing) {
            val to = target ?: return@LaunchedEffect
            if (pairing == null) return@LaunchedEffect
            nav.popBackStack(PROJECTS, inclusive = false)
            nav.navigate("project/${e(to.project)}")
            nav.navigate("scope/${e(to.project)}/${e(to.scope)}")
            if (to.agent != null) nav.navigate("agent/${e(to.project)}/${e(to.scope)}/${e(to.agent)}")
            target = null
        }

        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(title(entry?.arguments, route)) },
                    navigationIcon = {
                        if (nav.previousBackStackEntry != null) {
                            IconButton(onClick = { nav.popBackStack() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                        }
                    },
                    actions = {
                        if (route != SETTINGS && route != PAIR) {
                            IconButton(onClick = { nav.navigate(SETTINGS) }) { Icon(Icons.Filled.Settings, "Settings") }
                        }
                    },
                )
            },
        ) { padding ->
            Column(Modifier.padding(padding).fillMaxSize()) {
                if (route != PAIR && route != SETTINGS) ConnectionBanner(connection, readAt, repository::retry)
                if (snapshot?.understood == false) {
                    Text(
                        "pm serve sends a newer snapshot than this app knows; update the app.",
                        color = MaterialTheme.colorScheme.error,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.padding(8.dp),
                    )
                }
                NavHost(nav, startDestination = if (pairing == null) PAIR else PROJECTS) {
                    composable(PAIR) {
                        PairScreen { paired ->
                            repository.pair(paired)
                            askForNotifications()
                            nav.navigate(PROJECTS) { popUpTo(0) }
                        }
                    }
                    composable(PROJECTS) {
                        Shown(snapshot, connection) { ProjectsList(it, now) { p -> nav.navigate("project/${e(p)}") } }
                    }
                    composable("project/{project}") { back ->
                        val project = back.arguments?.getString("project").orEmpty()
                        Shown(snapshot, connection) {
                            ScopesList(it, project, now) { s -> nav.navigate("scope/${e(project)}/${e(s)}") }
                        }
                    }
                    composable("scope/{project}/{scope}") { back ->
                        val project = back.arguments?.getString("project").orEmpty()
                        val scope = back.arguments?.getString("scope").orEmpty()
                        Shown(snapshot, connection) {
                            AgentsList(
                                it, project, scope, now,
                                openAgent = { a -> nav.navigate("agent/${e(project)}/${e(scope)}/${e(a)}") },
                                openSummary = { nav.navigate("summary/${e(project)}/${e(scope)}") },
                            )
                        }
                    }
                    composable("agent/{project}/{scope}/{agent}") { back ->
                        val args = back.arguments
                        val project = args?.getString("project").orEmpty()
                        val scope = args?.getString("scope").orEmpty()
                        val agent = args?.getString("agent").orEmpty()
                        val client = repository.client()
                        if (client == null) {
                            Centered { Text("Not paired.") }
                        } else {
                            val model = viewModel(key = "$project/$scope/$agent") { AgentModel(client, project, scope, agent) }
                            LifecycleStartEffect(model) {
                                model.start()
                                onStopOrDispose { model.stop() }
                            }
                            AgentScreen(model)
                        }
                    }
                    composable("summary/{project}/{feature}") { back ->
                        SummaryScreen(
                            repository.client(),
                            back.arguments?.getString("project").orEmpty(),
                            back.arguments?.getString("feature").orEmpty(),
                        )
                    }
                    composable(SETTINGS) {
                        SettingsScreen(
                            pairing = pairing,
                            connection = connection,
                            vapid = { runCatching { repository.client()?.vapidKey() }.getOrNull() },
                            pair = { nav.navigate(PAIR) },
                            unpair = {
                                Notifications.unsubscribe(this@MainActivity)
                                repository.unpair()
                                subscribing = false
                                nav.navigate(PAIR) { popUpTo(0) }
                            },
                        )
                    }
                }
            }
        }
    }

    @Composable
    private fun Shown(snapshot: Snapshot?, connection: Connection, content: @Composable (Snapshot) -> Unit) {
        when {
            snapshot != null -> content(snapshot)
            connection is Connection.Unreachable -> Centered {
                Text("Server unreachable: connect Tailscale to open.", textAlign = TextAlign.Center)
            }
            else -> Centered { androidx.compose.material3.CircularProgressIndicator() }
        }
    }

    private fun title(args: Bundle?, route: String?): String = when (route) {
        PAIR -> "Pair"
        SETTINGS -> "Settings"
        "summary/{project}/{feature}" -> "${args?.getString("feature")} summary"
        "project/{project}" -> args?.getString("project").orEmpty()
        "scope/{project}/{scope}" -> "${args?.getString("project")}/${args?.getString("scope")}"
        "agent/{project}/{scope}/{agent}" -> "${args?.getString("scope")}: ${args?.getString("agent")}"
        else -> "pm"
    }

    private companion object {
        const val PAIR = "pair"
        const val PROJECTS = "projects"
        const val SETTINGS = "settings"

        fun e(segment: String): String = Uri.encode(segment)
    }
}
