package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.key
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.FeatureSnapshot
import dev.pm.app.model.Snapshot
import java.time.Instant
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.delay

/** A feature's page, on the route's page; `select` switches it. */
@Composable
fun FeatureRoute(
    snapshot: Snapshot,
    client: PmClient?,
    route: Route.Feature,
    select: (Page) -> Unit,
    now: Instant,
    topBar: TopBarSlot,
    acting: ActionState,
    ask: (Action) -> Unit,
    modifier: Modifier = Modifier,
    stale: Boolean = false,
) {
    val feature = snapshot.feature(route.project, route.scope)
    val blocker = feature?.let {
        mergeBlocker(client, route.project, route.scope, it, acting, recheck = true)
    }
    if (feature != null) FeatureActions(topBar, feature, blocker, ask) else TopBarActions(topBar) {}
    FeatureScreen(
        route.project,
        route.scope,
        feature,
        route.page,
        select,
        client,
        now,
        stale,
        acting,
        blocker,
        ask,
        modifier,
    )
}

/**
 * Why `feature`'s merge would not land now, or null; asked again as the feature or `acting`
 * changes, and while `recheck`, every [RECHECK] the screen is started: a commit at a terminal, the
 * usual cure, changes nothing the snapshot shows.
 */
@Composable
internal fun mergeBlocker(
    client: PmClient?,
    project: String,
    name: String,
    feature: FeatureSnapshot,
    acting: ActionState,
    recheck: Boolean,
    check: MergeCheckModel = viewModel(key = "merge") { MergeCheckModel(client, project, name) },
): String? {
    LaunchedEffect(feature, acting) { check.refresh() }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(check, recheck, lifecycle) {
        if (!recheck) return@LaunchedEffect
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                delay(RECHECK)
                check.refresh()
            }
        }
    }
    return check.blocker.collectAsState().value
}

private val RECHECK = 25.seconds

/** [FeaturePage] reading what it shows from the server. */
@Composable
internal fun FeatureScreen(
    project: String,
    name: String,
    feature: FeatureSnapshot?,
    page: Page,
    select: (Page) -> Unit,
    client: PmClient?,
    now: Instant,
    stale: Boolean,
    acting: ActionState,
    mergeBlocker: String?,
    ask: (Action) -> Unit,
    modifier: Modifier = Modifier,
) {
    val merge = Action.Merge(project, name)
    FeaturePage(
        feature,
        page,
        select,
        now,
        stale,
        merging = (acting as? ActionState.Running)?.action == merge,
        busy = acting is ActionState.Running,
        merge = { ask(merge) },
        modifier = modifier,
        mergeBlocker = mergeBlocker,
    ) { shown ->
        when (shown) {
            Page.Summary ->
                SummaryScreen(
                    viewModel(key = "summary") { ReadModel(client) { summary(project, name) } }
                )
            Page.Brief -> BriefScreen(infoModel(client, project, name))
            Page.Details -> DetailsScreen(infoModel(client, project, name))
        }
    }
}

@Composable
private fun infoModel(client: PmClient?, project: String, feature: String): ReadModel<FeatureInfo> =
    viewModel(key = "info") { ReadModel(client) { feature(project, feature) } }

/**
 * Where a feature stands, then a choice of its pages, `page` shown by `content`; a ready feature's
 * Merge, its next step, below whichever. `merging` while this feature's merge runs; `busy` while
 * any action does. A `mergeBlocker` turns Merge off and says why: above a ready feature's Merge,
 * else under where the feature stands.
 */
@Composable
fun FeaturePage(
    feature: FeatureSnapshot?,
    page: Page,
    select: (Page) -> Unit,
    now: Instant,
    stale: Boolean,
    merging: Boolean,
    busy: Boolean,
    merge: () -> Unit,
    modifier: Modifier = Modifier,
    mergeBlocker: String? = null,
    content: @Composable (Page) -> Unit,
) {
    val pages = rememberSaveableStateHolder()
    Column(modifier.fillMaxSize()) {
        if (feature != null) StatusHeader(feature, now, stale)
        if (feature != null && !isReady(feature) && mergeBlocker != null) {
            Text(
                "Can't merge: ${mergeBlocker.replaceFirstChar { it.lowercase() }}",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = Spacing.gutter),
            )
        }
        SingleChoiceSegmentedButtonRow(
            Modifier.fillMaxWidth().padding(horizontal = Spacing.gutter, vertical = Spacing.s)
        ) {
            Page.entries.forEachIndexed { i, each ->
                SegmentedButton(
                    selected = each == page,
                    onClick = { select(each) },
                    shape = SegmentedButtonDefaults.itemShape(i, Page.entries.size),
                ) {
                    Text(each.name)
                }
            }
        }
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        Box(Modifier.weight(1f)) {
            key(page) { pages.SaveableStateProvider(page.name) { content(page) } }
        }
        if (feature != null && isReady(feature)) {
            Surface(color = MaterialTheme.colorScheme.surfaceContainer) {
                Column(
                    Modifier.padding(horizontal = Spacing.gutter, vertical = Spacing.s),
                    verticalArrangement = Arrangement.spacedBy(Spacing.xs),
                ) {
                    if (mergeBlocker != null && !merging) {
                        Text(
                            mergeBlocker,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                    PendingButton(
                        "Merge",
                        merge,
                        Modifier.fillMaxWidth(),
                        pending = merging,
                        enabled = merging || (!busy && mergeBlocker == null),
                    )
                }
            }
        }
    }
}

/** A feature page's top bar: Merge while the feature isn't ready (its page has it then), Delete. */
@Composable
internal fun FeatureActions(
    topBar: TopBarSlot,
    feature: FeatureSnapshot,
    mergeBlocker: String?,
    ask: (Action) -> Unit,
) {
    val items = buildList {
        if (!isReady(feature)) add(mergeItem(feature, mergeBlocker, ask))
        add(deleteItem(feature, ask))
    }
    TopBarActions(topBar) { ActionMenu(items) }
}

internal fun mergeItem(feature: FeatureSnapshot, blocker: String?, ask: (Action) -> Unit) =
    MenuItem("Merge", R.drawable.ic_merge, blocker = blocker) {
        ask(Action.Merge(feature.project, feature.name))
    }

internal fun deleteItem(feature: FeatureSnapshot, ask: (Action) -> Unit) =
    MenuItem("Delete", R.drawable.ic_delete, destructive = true) {
        ask(Action.Delete(feature.project, feature.name))
    }
