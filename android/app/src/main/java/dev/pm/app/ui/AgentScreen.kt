package dev.pm.app.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleStartEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.mikepenz.markdown.m3.Markdown
import dev.pm.app.api.PmClient
import dev.pm.app.model.Item
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.launch

@Composable
fun AgentScreen(
    client: PmClient,
    project: String,
    scope: String,
    agent: String,
    networkChanges: Flow<Unit>,
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
            0 -> Chat(model)
            else -> Screen(model)
        }
    }
}

@Composable
private fun Chat(model: AgentModel) {
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
        is ChatState.Shown -> Conversation(model, state)
    }
}

@Composable
private fun Conversation(model: AgentModel, state: ChatState.Shown) {
    val items = state.conversation.items
    val list = rememberLazyListState()
    LaunchedEffect(Unit) { if (items.isNotEmpty()) list.scrollToItem(items.size - 1) }
    LaunchedEffect(items.lastOrNull()?.id) {
        val atEnd =
            list.layoutInfo.visibleItemsInfo.lastOrNull()?.index?.let { it >= items.size - 3 }
                ?: true
        if (atEnd && items.isNotEmpty()) list.animateScrollToItem(items.size - 1)
    }
    LaunchedEffect(list) {
        snapshotFlow { list.firstVisibleItemIndex }.collect { if (it == 0) model.older() }
    }
    Column(Modifier.fillMaxSize()) {
        if (!state.live) {
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
        LazyColumn(
            state = list,
            modifier = Modifier.fillMaxSize(),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (state.conversation.before == null) {
                item(key = "start") { SystemRow("start of the conversation") }
            }
            items(items, key = { it.id }) { item -> ItemRow(model, item) }
        }
    }
}

@Composable
private fun ItemRow(model: AgentModel, item: Item) {
    when (item) {
        is Item.User ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Box(
                    Modifier.widthIn(max = 320.dp)
                        .background(
                            MaterialTheme.colorScheme.primaryContainer,
                            RoundedCornerShape(12.dp),
                        )
                        .padding(10.dp)
                ) {
                    SelectionContainer {
                        Text(item.text, color = MaterialTheme.colorScheme.onPrimaryContainer)
                    }
                }
            }
        is Item.Assistant -> SelectionContainer { Markdown(item.text) }
        is Item.Thinking -> Collapsible(title = "thinking", body = item.text, italic = true)
        is Item.Tool -> ToolCard(item, model::fullResult)
        is Item.Continuation -> SystemRow("pm: ${item.text.lineSequence().firstOrNull().orEmpty()}")
        is Item.Compaction -> SystemRow("context compacted")
        is Item.Event -> SystemRow(item.text)
    }
}

@Composable
private fun SystemRow(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun Collapsible(title: String, body: String, italic: Boolean = false) {
    var open by rememberSaveable { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().clickable { open = !open }.animateContentSize()) {
        Text(
            if (open) "▾ $title" else "▸ $title",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (open) {
            Text(
                body,
                style = MaterialTheme.typography.bodySmall,
                fontStyle = if (italic) FontStyle.Italic else null,
            )
        }
    }
}

/** A tool call, collapsed to its name and input; open, it shows the result. */
@Composable
internal fun ToolCard(tool: Item.Tool, fullResult: suspend (ref: String) -> Result<String>) {
    var open by rememberSaveable(tool.id) { mutableStateOf(false) }
    var full by remember(tool.id) { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val result = tool.result
    Card(Modifier.fillMaxWidth().clickable { open = !open }.animateContentSize()) {
        Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(tool.name, style = MaterialTheme.typography.labelLarge)
                Text(
                    when {
                        result == null -> "  running"
                        result.error -> "  failed"
                        else -> ""
                    },
                    style = MaterialTheme.typography.labelSmall,
                    color =
                        if (result?.error == true) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                tool.input,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
                maxLines = if (open) Int.MAX_VALUE else 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (open && result != null) {
                SelectionContainer {
                    Text(
                        full ?: result.text,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        modifier = Modifier.horizontalScroll(rememberScrollState()),
                    )
                }
                val ref = result.full
                if (result.truncated && ref != null && full == null) {
                    TextButton(
                        onClick = {
                            scope.launch {
                                full =
                                    fullResult(ref).getOrElse { "Couldn't load it: ${it.message}" }
                            }
                        }
                    ) {
                        Text("Show all")
                    }
                }
            }
        }
    }
}

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
        else ->
            SelectionContainer {
                Text(
                    shown.getOrThrow(),
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                    softWrap = false,
                    modifier =
                        Modifier.fillMaxSize()
                            .verticalScroll(rememberScrollState())
                            .horizontalScroll(rememberScrollState())
                            .padding(8.dp),
                )
            }
    }
}

@Composable
fun Centered(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Box(modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) { content() }
}
