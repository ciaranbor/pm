package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.R
import dev.pm.app.api.PmClient
import dev.pm.app.model.AgentState
import dev.pm.app.model.Conversation
import dev.pm.app.model.Item
import dev.pm.app.model.Waiting
import java.time.LocalDate
import java.time.ZoneId
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch

@Composable
fun AgentScreen(
    client: PmClient,
    project: String,
    scope: String,
    agent: String,
    state: AgentState?,
    waiting: Waiting?,
    networkChanges: Flow<Unit>,
    openTerminal: () -> Unit,
    modifier: Modifier = Modifier,
    drafts: Drafts = remember { Drafts() },
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
    val answeredIds by model.answeredIds.collectAsStateWithLifecycle()
    val dialogId = waiting?.dialog?.takeIf { asking }
    val shownState = composerState(state, dialogId, answeredIds)
    LaunchedEffect(dialogId) { model.dialogNamed(dialogId) }
    val outbox by model.outbox.collectAsStateWithLifecycle()
    val notice by model.notice.collectAsStateWithLifecycle()
    val dialogs by model.dialogs.collectAsStateWithLifecycle()
    val answering by model.answering.collectAsStateWithLifecycle()
    val answered by model.answered.collectAsStateWithLifecycle()
    val interrupting by model.interrupting.collectAsStateWithLifecycle()
    val feedback = LocalFeedback.current
    LaunchedEffect(model) {
        model.interrupted.collect { failure ->
            if (failure == null) feedback.done("Interrupted $agent")
            else feedback.failed("Couldn't interrupt: $failure", model::interrupt)
        }
    }
    LaunchedEffect(model) {
        model.answerFailed.collect { feedback.failed("Couldn't answer: $it", model::retryAnswer) }
    }
    val draftKey = "$project/$scope/$agent"
    Column(modifier.fillMaxSize().imePadding()) {
        Box(Modifier.weight(1f)) { Chat(model) }
        answered?.let { AnsweredRow(it) }
        val sent = outbox?.takeUnless { it is Outbox.Seen }
        if (sent != null) {
            OutboxBubble(
                sent,
                retry = model::retrySend,
                edit = {
                    drafts.restore(draftKey, sent.text)
                    model.dismissOutbox()
                },
            )
        }
        if (asking && dialogs.isNotEmpty()) {
            DialogCard(
                dialogs,
                answering,
                interrupting,
                notice,
                answer = model::answer,
                interrupt = model::interrupt,
                openTerminal = openTerminal,
            )
        } else {
            Composer(
                shownState,
                waiting,
                sending = outbox is Outbox.Sending,
                notice?.text,
                interrupting,
                drafts,
                draftKey,
                send = model::send,
                interrupt = model::interrupt,
                openTerminal = openTerminal,
            )
        }
    }
}

/**
 * The agent's state as the composer takes it: a dialog answered here stays the agent's state until
 * its harness moves on, and meanwhile the agent is as good as busy.
 */
internal fun composerState(state: AgentState?, dialog: String?, answered: Set<String>) =
    if (dialog != null && dialog in answered) AgentState.Busy else state

@Composable
private fun Chat(model: AgentModel) {
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
            ChatView(
                state.conversation,
                state.live,
                older = model::older,
                reader = { reading, close ->
                    val ref = reading.full
                    if (ref == null) ReaderDialog(reading, ReaderBody.Shown(reading.text), close)
                    else {
                        LaunchedEffect(ref) { model.readWhole(ref) }
                        val whole by model.whole.collectAsStateWithLifecycle()
                        ReaderDialog(
                            reading,
                            whole?.takeIf { it.ref == ref }?.body ?: ReaderBody.Loading,
                            close = {
                                model.closeWhole()
                                close()
                            },
                        )
                    }
                },
            )
    }
}

/**
 * The conversation, oldest first, kept at its end while the user is there; scrolled up, a button
 * counts what has come since and goes back down, and another goes to the user's last message. Near
 * the top, or while all of it fits, it asks for `older` items, again after each page arrives.
 *
 * A tap on a message shows its time, and a long press offers its actions. A tool call, a thought or
 * an event opens in `reader` on a tap; a tool call on a long press too, since only the reader holds
 * its whole output.
 */
@Composable
internal fun ChatView(
    conversation: Conversation,
    live: Boolean,
    older: () -> Unit,
    modifier: Modifier = Modifier,
    reader: @Composable (Reading, close: () -> Unit) -> Unit = { reading, close ->
        ReaderDialog(reading, ReaderBody.Shown(reading.text), close)
    },
) {
    val items = conversation.items
    val runKeys = remember { HashMap<String, String>() }
    val built = remember(items) { keepRunKeys(chatRows(items, ZoneId.systemDefault()), runKeys) }
    val today = LocalDate.now()
    val start = if (conversation.before == null) 1 else 0
    val list = rememberLazyListState(initialFirstVisibleItemIndex = built.size + start - 1)
    var follow by rememberSaveable { mutableStateOf(true) }
    val rows = remember(built, start) { placed(built, list, start, follow) }
    var seen by rememberSaveable { mutableStateOf(items.lastOrNull()?.id) }
    var reading by rememberSaveable { mutableStateOf<String?>(null) }
    var acting by rememberSaveable { mutableStateOf<String?>(null) }
    val latest = items.lastOrNull()?.id
    val askOlder by rememberUpdatedState(older)
    val before by rememberUpdatedState(conversation.before)
    val byIdNow = remember(items) { items.associateBy { it.id } }
    val byId by rememberUpdatedState(byIdNow)
    val touch = remember {
        ChatTouch(
            read = { reading = it },
            act = { id -> if (byId[id] is Item.Tool) reading = id else acting = id },
        )
    }

    LaunchedEffect(follow, latest) { if (follow) seen = latest }
    LaunchedEffect(list) {
        followEnd(
            // From the layout, as `canScrollForward` is: the state's index moves before it. By
            // key, which an older page put before it leaves as it was; its index does not.
            position = {
                list.layoutInfo.visibleItemsInfo.firstOrNull()?.let { it.key to it.offset } ?: 0
            },
            behind = { list.canScrollForward },
            following = { follow },
            setFollowing = { follow = it },
            toEnd = { list.scrollToEnd() },
        )
    }
    LaunchedEffect(list) {
        snapshotFlow {
            val near = list.firstVisibleItemIndex < PREFETCH || !list.canScrollBackward
            near to before
        }
            .collect { (near, cursor) -> if (near && cursor != null) askOlder() }
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
                    ChatRowView(row, today, touch)
                }
            }
            Jumps(
                list,
                rows,
                start,
                behind = !follow,
                unseen = newSince(items, seen),
                toEnd = { follow = true },
                modifier = Modifier.align(Alignment.BottomEnd).padding(Spacing.l),
            )
        }
    }
    val read = reading?.let { byIdNow[it] }?.let(::reading)
    if (read != null) reader(read) { reading = null }
    val act = acting?.let { byIdNow[it] }?.let(::reading)
    if (act != null) {
        val id = acting
        MessageActions(act, select = { reading = id }, dismiss = { acting = null })
    }
}

/**
 * `rows`, the list's new rows, with its place kept across the change: at its end while `following`;
 * else at the first message shown, older rows put before it included. Compose instead keeps a list
 * at its top when it was there, as a short first page leaves it, and a day's divider stays first as
 * older items of that day arrive under it. Called during composition, before the list lays out the
 * new rows; `start` is how many items come before them.
 */
private fun placed(
    rows: List<ChatRow>,
    list: LazyListState,
    start: Int,
    following: Boolean,
): List<ChatRow> {
    if (following) {
        // Past the end, which the list clamps to: its end, however tall the last row.
        if (rows.isNotEmpty()) list.requestScrollToItem(rows.size + start - 1, Int.MAX_VALUE / 2)
        return rows
    }
    val first =
        Snapshot.withoutReadObservation {
            list.layoutInfo.visibleItemsInfo.firstOrNull {
                (it.key as? String)?.isMessage() == true
            }
        } ?: return rows
    val at = rows.indexOfFirst { it.key == first.key }
    if (at >= 0 && at + start != first.index) {
        list.requestScrollToItem(at + start, (-first.offset).coerceAtLeast(0))
    }
    return rows
}

private fun String.isMessage() = this != "start" && !startsWith("day:")

/**
 * The jumps a scrolled chat offers: to the user's last message while it is out of view, with how
 * many rows came after it; and, while `behind`, to the end, with how many are `unseen`. `start` is
 * how many list items come before the rows.
 */
@Composable
private fun Jumps(
    list: LazyListState,
    rows: List<ChatRow>,
    start: Int,
    behind: Boolean,
    unseen: Int,
    toEnd: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = rememberCoroutineScope()
    val mine = rows.indexOfLast { (it as? ChatRow.Single)?.item is Item.User }
    val since = remember(rows, mine) { rowsAfter(rows, mine) }
    val mineOut by
        remember(mine, start) {
            derivedStateOf {
                mine >= 0 && list.layoutInfo.visibleItemsInfo.none { it.index == mine + start }
            }
        }
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        if (mineOut) {
            Jump(
                R.drawable.ic_arrow_upward,
                "Go to your last message, $since after it",
                since,
            ) {
                scope.launch { list.animateScrollToItem(mine + start) }
            }
        }
        if (behind) {
            Jump(
                R.drawable.ic_arrow_down,
                if (unseen > 0) "Go to the end, $unseen new" else "Go to the end",
                unseen,
                toEnd,
            )
        }
    }
}

@Composable
private fun Jump(icon: Int, description: String, count: Int, onClick: () -> Unit) {
    BadgedBox(badge = { if (count > 0) Badge { Text("$count") } }) {
        SmallFloatingActionButton(
            onClick = onClick,
            modifier = Modifier.semantics(mergeDescendants = true) {},
        ) {
            Icon(painterResource(icon), description)
        }
    }
}

/** How many rows, day dividers aside, come after the one at `at`. */
private fun rowsAfter(rows: List<ChatRow>, at: Int): Int =
    if (at < 0) 0 else rows.drop(at + 1).count { it !is ChatRow.Day }

private val ChatRow.contentType: Any
    get() =
        when (this) {
            is ChatRow.Day -> "day"
            is ChatRow.Wakes -> "wakes"
            is ChatRow.Work -> "work"
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

/** How near the top, in rows, the chat asks for older items. */
private const val PREFETCH = 3
