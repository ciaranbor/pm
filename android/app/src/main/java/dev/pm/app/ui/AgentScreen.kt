package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import dev.pm.app.model.Conversation
import java.time.LocalDate
import java.time.ZoneId
import kotlinx.coroutines.flow.Flow

@Composable
fun AgentScreen(
    client: PmClient,
    project: String,
    scope: String,
    agent: String,
    networkChanges: Flow<Unit>,
    openResult: (tool: String, ref: String) -> Unit,
    modifier: Modifier = Modifier,
    model: AgentModel = viewModel { AgentModel(client, project, scope, agent, networkChanges) },
) {
    LifecycleStartEffect(model) {
        model.start()
        onStopOrDispose { model.stop() }
    }
    var tab by rememberSaveable { mutableIntStateOf(0) }
    LaunchedEffect(tab) { model.watchScreen(tab == 1) }
    Column(modifier.fillMaxSize()) {
        PrimaryTabRow(selectedTabIndex = tab) {
            Tab(selected = tab == 0, onClick = { tab = 0 }, text = { Text("Chat") })
            Tab(selected = tab == 1, onClick = { tab = 1 }, text = { Text("Screen") })
        }
        when (tab) {
            0 -> Chat(model, openResult)
            else -> Screen(model)
        }
    }
}

@Composable
private fun Chat(model: AgentModel, openResult: (tool: String, ref: String) -> Unit) {
    val chat by model.chat.collectAsStateWithLifecycle()
    when (val state = chat) {
        ChatState.Loading -> Centered { CircularProgressIndicator() }
        ChatState.Unsupported ->
            Centered {
                Text(
                    "This pm serve has no transcripts yet. Upgrade pm on the Mac; the Screen tab shows the agent meanwhile.",
                    textAlign = TextAlign.Center,
                )
            }
        is ChatState.Failed ->
            Centered {
                Text(
                    "Couldn't load the conversation: ${state.reason}",
                    textAlign = TextAlign.Center,
                )
            }
        is ChatState.Shown ->
            ChatView(state.conversation, state.live, older = model::older, openResult = openResult)
    }
}

/**
 * The conversation, oldest first, kept at its end while the user is there; scrolled up, a button
 * counts what has come since and goes back down. Reaching the top asks for `older` items.
 */
@Composable
internal fun ChatView(
    conversation: Conversation,
    live: Boolean,
    older: () -> Unit,
    openResult: (tool: String, ref: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val items = conversation.items
    val rows = remember(items) { chatRows(items, ZoneId.systemDefault()) }
    val today = LocalDate.now()
    val list =
        rememberLazyListState(
            initialFirstVisibleItemIndex = rows.size - if (conversation.before == null) 0 else 1
        )
    var follow by rememberSaveable { mutableStateOf(true) }
    var seen by rememberSaveable { mutableStateOf(items.lastOrNull()?.id) }
    val latest = items.lastOrNull()?.id
    val askOlder by rememberUpdatedState(older)

    LaunchedEffect(follow, latest) { if (follow) seen = latest }
    LaunchedEffect(list) {
        followEnd(
            // From the layout, as `canScrollForward` is: the state's index moves before it.
            position = {
                list.layoutInfo.visibleItemsInfo.firstOrNull()?.let { it.index to it.offset } ?: 0
            },
            behind = { list.canScrollForward },
            following = { follow },
            setFollowing = { follow = it },
            toEnd = { list.scrollToEnd() },
        )
    }
    LaunchedEffect(list) {
        snapshotFlow { list.firstVisibleItemIndex }.collect { if (it == 0) askOlder() }
    }

    Column(modifier.fillMaxSize().imePadding()) {
        if (!live) {
            Text(
                "Not live: reconnecting",
                style = MaterialTheme.typography.labelSmall,
                modifier =
                    Modifier.fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surfaceVariant)
                        .padding(4.dp),
                textAlign = TextAlign.Center,
            )
        }
        if (items.isEmpty()) {
            Centered { Text("Nothing said yet.") }
            return
        }
        Box(Modifier.weight(1f)) {
            LazyColumn(
                state = list,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (conversation.before == null) {
                    item(key = "start", contentType = "start") {
                        SystemRow("start of the conversation")
                    }
                }
                items(rows, key = { it.key }, contentType = { it.contentType }) { row ->
                    ChatRowView(row, today, openResult)
                }
            }
            if (!follow) {
                val unseen = newSince(items, seen)
                val down = { follow = true }
                val placed = Modifier.align(Alignment.BottomEnd).padding(16.dp)
                val arrow = painterResource(R.drawable.ic_arrow_down)
                if (unseen > 0) {
                    ExtendedFloatingActionButton(
                        onClick = down,
                        icon = { Icon(arrow, null) },
                        text = { Text("$unseen new") },
                        modifier = placed,
                    )
                } else {
                    SmallFloatingActionButton(onClick = down, modifier = placed) {
                        Icon(arrow, "Go to the end")
                    }
                }
            }
        }
    }
}

private val ChatRow.contentType: Any
    get() =
        when (this) {
            is ChatRow.Day -> "day"
            is ChatRow.Wakes -> "wakes"
            is ChatRow.Single -> item::class
        }

/** Scroll to the very end: the bottom of the last row, however tall. */
private suspend fun LazyListState.scrollToEnd() {
    val last = layoutInfo.totalItemsCount - 1
    if (last < 0) return
    scrollToItem(last)
    repeat(MAX_END_STEPS) {
        if (!canScrollForward) return
        scrollBy(layoutInfo.viewportSize.height.toFloat().coerceAtLeast(1f))
    }
}

private const val MAX_END_STEPS = 50

@Composable
private fun Screen(model: AgentModel) {
    val screen by model.screen.collectAsStateWithLifecycle()
    val shown = screen
    when {
        shown == null -> Centered { CircularProgressIndicator() }
        shown.isFailure ->
            Centered {
                Text(
                    "Couldn't read the screen: ${shown.exceptionOrNull()?.message}",
                    textAlign = TextAlign.Center,
                )
            }
        else -> TerminalView(shown.getOrThrow())
    }
}

@Composable
fun Centered(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Box(modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) { content() }
}
