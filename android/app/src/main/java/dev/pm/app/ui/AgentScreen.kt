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
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
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
import dev.pm.app.model.AgentState
import dev.pm.app.model.Conversation
import dev.pm.app.model.Waiting
import java.time.LocalDate
import java.time.ZoneId
import kotlinx.coroutines.flow.Flow

@Composable
fun AgentScreen(
    client: PmClient,
    project: String,
    scope: String,
    agent: String,
    state: AgentState?,
    waiting: Waiting?,
    networkChanges: Flow<Unit>,
    openResult: (tool: String, ref: String) -> Unit,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
    model: AgentModel =
        viewModel(key = "agent/$agent") {
            AgentModel(client, project, scope, agent, networkChanges)
        },
) {
    LifecycleStartEffect(model) {
        model.start()
        onStopOrDispose { model.stop() }
    }
    val asking = state == AgentState.Asking
    val dialogId = waiting?.dialog?.takeIf { asking }
    LaunchedEffect(dialogId) { model.dialogNamed(dialogId) }
    val outbox by model.outbox.collectAsStateWithLifecycle()
    val notice by model.notice.collectAsStateWithLifecycle()
    val dialog by model.dialog.collectAsStateWithLifecycle()
    val answering by model.answering.collectAsStateWithLifecycle()
    val interrupting by model.interrupting.collectAsStateWithLifecycle()
    val feedback = LocalFeedback.current
    LaunchedEffect(model) {
        model.interrupted.collect { failure ->
            if (failure == null) feedback.done("Interrupted $agent")
            else feedback.failed("Couldn't interrupt: $failure", model::interrupt)
        }
    }
    Column(modifier.fillMaxSize().imePadding()) {
        Box(Modifier.weight(1f)) { Chat(model, openResult) }
        val shown = dialog
        if (asking && shown != null) {
            DialogCard(
                shown,
                answering,
                interrupting,
                notice,
                answer = model::answer,
                interrupt = model::interrupt,
                openTerminal = openTerminal,
            )
        } else {
            Composer(
                state,
                waiting,
                outbox,
                notice,
                interrupting,
                send = model::send,
                interrupt = model::interrupt,
                openTerminal = openTerminal,
            )
        }
    }
}

@Composable
private fun Chat(model: AgentModel, openResult: (tool: String, ref: String) -> Unit) {
    val chat by model.chat.collectAsStateWithLifecycle()
    when (val state = chat) {
        ChatState.Loading -> Centered { CircularProgressIndicator() }
        is ChatState.Unsupported -> EmptyState("No conversation to show", hint = state.advice)
        is ChatState.Failed ->
            ErrorState(
                "Couldn't load the conversation",
                onAction = null,
                hint = "${state.reason}\nRetrying…",
            )
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

    Column(modifier.fillMaxSize()) {
        if (!live) {
            Text(
                "Not live: reconnecting",
                style = MaterialTheme.typography.labelSmall,
                modifier =
                    Modifier.fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surfaceVariant)
                        .padding(Spacing.xs),
                textAlign = TextAlign.Center,
            )
        }
        if (items.isEmpty()) {
            EmptyState("Nothing said yet.")
            return
        }
        Box(Modifier.weight(1f)) {
            LazyColumn(
                state = list,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
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
                val placed = Modifier.align(Alignment.BottomEnd).padding(Spacing.l)
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
