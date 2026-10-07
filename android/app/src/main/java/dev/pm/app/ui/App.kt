package dev.pm.app.ui

import android.Manifest
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.TextAutoSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PlainTooltip
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TooltipAnchorPosition
import androidx.compose.material3.TooltipBox
import androidx.compose.material3.TooltipDefaults
import androidx.compose.material3.TooltipState
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberTooltipState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.createSavedStateHandle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
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
import kotlinx.coroutines.launch

/**
 * The app's frame and navigation. While shown, it holds the event stream open, and registers for
 * pushes once the server is reached. A `target` from a notification opens over what is shown once
 * paired, then `targetShown` is called. A scope the live snapshot no longer has is left, with word
 * of why.
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
    val loaded by model.loaded.collectAsStateWithLifecycle()
    if (!loaded) return
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
        backStack.open(to.route())
        targetShown()
    }

    val acting by model.lifecycle.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val feedback = remember(scope) { Feedback(SnackbarHostState(), scope) }
    val topBar = remember { TopBarSlot() }
    val fullName = rememberTooltipState(isPersistent = true)
    LaunchedEffect(top) { fullName.dismiss() }
    LaunchedEffect(model) {
        model.lifecycle.finished.collect { action ->
            if (action !is Action.Restart) backStack.leave(action.project, action.subject)
            feedback.done(action.done)
        }
    }
    val seen = remember { mutableSetOf<kotlin.Pair<String, String>>() }
    val shownScopes = backStack.mapNotNull { (it as? Route)?.scopeOf() }.distinct()
    LaunchedEffect(snapshot, connection, acting, shownScopes) {
        val shown = snapshot ?: return@LaunchedEffect
        // The app's own merge or delete reports itself once it finishes.
        val own =
            (acting as? ActionState.Running)
                ?.action
                ?.takeIf { it !is Action.Restart }
                ?.let { it.project to it.subject }
        val gone = shown.dropped(backStack, seen)
        if (connection != Connection.Live) return@LaunchedEffect
        gone
            .filter { it != own }
            .forEach { (project, scope) ->
                backStack.leave(project, scope)
                feedback.done(
                    if (scope == Snapshot.MAIN) "$project is no longer a pm project"
                    else "$scope was merged or deleted"
                )
            }
    }
    ActionDialog(acting, model.lifecycle::confirm, model.lifecycle::dismiss)

    val pairAgain: () -> Unit = { backStack.add(Route.Pair) }
    val (title, subtitle) = top?.heading ?: ("pm" to null)
    val stale = connection != Connection.Live && connection != Connection.Unpaired
    val updated =
        readAt
            ?.takeIf { stale && top != Route.Pair && top != Route.Settings }
            ?.let { "Updated ${ago(it, now)}" }
    CompositionLocalProvider(LocalFeedback provides feedback, LocalConnection provides connection) {
        Scaffold(
            modifier = modifier,
            snackbarHost = { FeedbackHost(feedback, Modifier.imePadding()) },
            topBar = {
                TopAppBar(
                    title = { Title(title, subtitle, updated, fullName) },
                    navigationIcon = {
                        if (top?.parent != null || backStack.size > 1) {
                            IconButton(onClick = { backStack.up() }) {
                                Icon(painterResource(R.drawable.ic_arrow_back), "Navigate up")
                            }
                        }
                    },
                    actions = {
                        val screen = topBar.actions
                        when {
                            screen != null -> screen()
                            top == Route.Home ->
                                ActionMenu(
                                    listOf(
                                        MenuItem("Settings", R.drawable.ic_settings) {
                                            backStack.add(Route.Settings)
                                        }
                                    )
                                )
                        }
                    },
                )
            },
        ) { padding ->
            Column(Modifier.padding(padding).consumeWindowInsets(padding).fillMaxSize()) {
                if (snapshot != null && top != Route.Pair && top != Route.Settings)
                    StatusStrip(connection, model::retry, pairAgain)
                if (snapshot?.understood == false) {
                    Text(
                        "pm serve sends a newer snapshot than this app knows; update the app.",
                        color = MaterialTheme.colorScheme.error,
                        textAlign = TextAlign.Center,
                        modifier = Modifier.padding(Spacing.s),
                    )
                }
                NavDisplay(
                    backStack = backStack,
                    onBack = {
                        if (fullName.isVisible) fullName.dismiss() else backStack.removeLastOrNull()
                    },
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
                                        openScope = { p, s -> backStack.add(Route.Scope(p, s)) },
                                        openProject = { p -> backStack.add(Route.Project(p)) },
                                        stale = stale,
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
                                        stale = stale,
                                    )
                                }
                            }
                            entry<Route.Scope>(clazzContentKey = { it.contentKey() }) { key ->
                                Shown(snapshot, connection, model::retry, pairAgain) {
                                    Workspace(
                                        it,
                                        client,
                                        key,
                                        select = { tab ->
                                            val at = backStack.indexOf(key)
                                            if (at >= 0) backStack[at] = key.copy(tab = tab)
                                        },
                                        now,
                                        networkChanges,
                                        topBar,
                                        acting,
                                        model.lifecycle::ask,
                                        stale = stale,
                                        drafts = model.drafts,
                                    )
                                }
                            }
                            entry<Route.Notes> { key ->
                                val store = LocalContext.current.container.store
                                NotesScreen(
                                    viewModel {
                                        NotesModel(
                                            client,
                                            key.project,
                                            store,
                                            createSavedStateHandle(),
                                        )
                                    },
                                    topBar,
                                )
                            }
                            entry<Route.Settings> {
                                val context = LocalContext.current
                                val container = context.container
                                var checkUpdates by remember {
                                    mutableStateOf(container.store.checkUpdates)
                                }
                                val serverVersion by
                                    remember(client) {
                                            client?.serverVersion ?: MutableStateFlow(null)
                                        }
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
                                                checkNow = {
                                                    UpdateWorker.check(container.http)?.also {
                                                        UpdateWorker.offer(context, it)
                                                    }
                                                },
                                            )
                                            .takeIf { BuildConfig.SELF_UPDATE },
                                    pair = { backStack.add(Route.Pair) },
                                    unpair = {
                                        model.unpair().also {
                                            backStack.replaceWith(listOf(Route.Pair))
                                        }
                                    },
                                )
                            }
                        },
                )
            }
        }
    }
}

/**
 * The top bar's title, shrunk to fit before it is cut short, with a tap showing it whole; under it,
 * the heading's second line, which gives way first, then how old the snapshot is. `tip` shows the
 * full name, and Back hides it first: NavDisplay's onBack does that while the stack can go back,
 * the handler here otherwise.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Title(title: String, heading: String?, updated: String?, tip: TooltipState) {
    val scope = rememberCoroutineScope()
    BackHandler(enabled = tip.isVisible) { tip.dismiss() }
    TooltipBox(
        positionProvider =
            TooltipDefaults.rememberTooltipPositionProvider(TooltipAnchorPosition.Below),
        tooltip = { PlainTooltip { Text(listOfNotNull(title, heading).joinToString("\n")) } },
        state = tip,
    ) {
        Column(
            Modifier.clickable(onClickLabel = "Show the full name") { scope.launch { tip.show() } }
        ) {
            Text(
                title,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                autoSize =
                    TextAutoSize.StepBased(
                        minFontSize = MaterialTheme.typography.titleSmall.fontSize,
                        maxFontSize = MaterialTheme.typography.titleLarge.fontSize,
                    ),
            )
            if (heading != null || updated != null) Subtitle(heading, updated)
        }
    }
}

/**
 * The top bar's second line: the heading's, which gives way first, then how old the snapshot is.
 */
@Composable
private fun Subtitle(heading: String?, updated: String?) {
    val style = MaterialTheme.typography.bodySmall
    val color = MaterialTheme.colorScheme.onSurfaceVariant
    Row {
        if (heading != null) {
            Text(
                heading,
                style = style,
                color = color,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
        }
        if (updated != null) {
            Text(
                if (heading != null) " · $updated" else updated,
                style = style,
                color = color,
                maxLines = 1,
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
    val manual = rememberRetry(connection, retry)
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
        manual.pending || connection is Connection.Unreachable ->
            ErrorState(
                if (manual.failedAgain) "Still can't reach pm serve" else "Can't reach pm serve",
                manual::start,
                hint = UNREACHABLE_HINT,
                pending = manual.pending,
            )
        connection == Connection.Unauthorized ->
            ErrorState(
                "Not paired",
                pairAgain,
                hint = "The server revoked this phone's token.",
                action = "Pair again",
            )
        else -> Centered { CircularProgressIndicator() }
    }
}
