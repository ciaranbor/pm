package dev.pm.app.ui

import android.content.ClipData
import android.os.Build
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconToggleButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.Clipboard
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import dev.pm.app.R
import dev.pm.app.model.Item
import kotlinx.coroutines.launch

/** How the reader sets its text: tool output in monospace, with wrapping a choice, or prose. */
enum class ReadStyle {
    Output,
    Plain,
    Markdown,
}

/**
 * Something to read whole: a message, or a tool's output under the call that made it (`input`).
 * `full` names a tool's whole output when `text` was cut short.
 */
data class Reading(
    val title: String,
    val text: String,
    val style: ReadStyle,
    val input: String? = null,
    val full: String? = null,
)

/** What the reader has of its text. */
sealed interface ReaderBody {
    data object Loading : ReaderBody

    data class Shown(val text: String) : ReaderBody

    data class Failed(val reason: String, val retry: () -> Unit) : ReaderBody
}

/** What a chat item reads as, whole; `null` for one with nothing to read. */
fun reading(item: Item): Reading? =
    when (item) {
        is Item.User -> Reading("Your message", item.text, ReadStyle.Plain)
        is Item.Assistant -> Reading("Message", item.text, ReadStyle.Markdown)
        is Item.Thinking -> Reading("Thinking", item.text, ReadStyle.Plain)
        is Item.Tool ->
            Reading(
                item.name,
                item.result?.text.orEmpty(),
                ReadStyle.Output,
                input = item.input,
                full = item.result?.full?.takeIf { item.result.truncated },
            )
        is Item.Continuation -> Reading("pm woke the agent", item.text, ReadStyle.Plain)
        is Item.Compaction ->
            item.summary?.let { Reading("Context compacted", it, ReadStyle.Markdown) }
        is Item.Event -> Reading("Event", item.text, ReadStyle.Plain)
    }

/** The reader over the whole screen; Back or Close leaves it. */
@Composable
fun ReaderDialog(
    reading: Reading,
    body: ReaderBody,
    close: () -> Unit,
    modifier: Modifier = Modifier,
) {
    FullScreen(close) { Surface(modifier.fillMaxSize()) { ReaderView(reading, body, close) } }
}

/**
 * `content` over the whole screen, edge to edge, its system bars' icons set for the theme; Back
 * calls `close`.
 */
@Composable
fun FullScreen(close: () -> Unit, content: @Composable () -> Unit) {
    Dialog(
        onDismissRequest = close,
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        val light = MaterialTheme.colorScheme.surface.luminance() > 0.5f
        val view = LocalView.current
        SideEffect {
            val window = (view.parent as? DialogWindowProvider)?.window ?: return@SideEffect
            WindowCompat.getInsetsController(window, view).apply {
                isAppearanceLightStatusBars = light
                isAppearanceLightNavigationBars = light
            }
        }
        content()
    }
}

/**
 * One text, scrolled as a whole, so any of it can be selected and all of it copied. Tool output is
 * monospace, unwrapped until the wrap toggle is on. Text past [LARGE] is shown wrapped a line at a
 * time, which keeps it smooth at the cost of selecting across lines; Copy all still takes the
 * whole.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ReaderView(
    reading: Reading,
    body: ReaderBody,
    close: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var wrap by rememberSaveable { mutableStateOf(false) }
    val snackbar = remember { SnackbarHostState() }
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    val text = (body as? ReaderBody.Shown)?.text
    Scaffold(
        modifier = modifier,
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Text(reading.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = {
                    IconButton(onClick = close) {
                        Icon(painterResource(R.drawable.ic_close), "Close")
                    }
                },
                actions = {
                    if (reading.style == ReadStyle.Output && (text?.length ?: 0) <= LARGE) {
                        IconToggleButton(checked = wrap, onCheckedChange = { wrap = it }) {
                            Icon(painterResource(R.drawable.ic_wrap_text), "Wrap lines")
                        }
                    }
                    IconButton(
                        onClick = {
                            val all = text ?: return@IconButton
                            scope.launch {
                                clipboard.copy(reading.title, all) {
                                    snackbar.showSnackbar("Copied")
                                }
                            }
                        },
                        enabled = text != null,
                    ) {
                        Icon(painterResource(R.drawable.ic_content_copy), "Copy all")
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            reading.input?.let { input ->
                Selectable {
                    Text(
                        input,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 6,
                        overflow = TextOverflow.Ellipsis,
                        modifier =
                            Modifier.fillMaxWidth()
                                .padding(horizontal = Spacing.gutter, vertical = Spacing.s),
                    )
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            }
            when (body) {
                ReaderBody.Loading -> Centered { CircularProgressIndicator() }
                is ReaderBody.Failed ->
                    ErrorState("Couldn't load the output", body.retry, hint = body.reason)
                is ReaderBody.Shown ->
                    if (body.text.isEmpty()) EmptyState("No output")
                    else ReaderText(body.text, reading.style, wrap)
            }
        }
    }
}

@Composable
private fun ReaderText(text: String, style: ReadStyle, wrap: Boolean) {
    val output = style == ReadStyle.Output
    val type =
        if (output) MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace)
        else MaterialTheme.typography.bodyLarge
    if (text.length > LARGE) {
        val lines = remember(text) { outputLines(text) }
        Selectable {
            LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(Spacing.gutter)) {
                items(lines) { Text(it, style = type) }
            }
        }
        return
    }
    Selectable(Modifier.fillMaxSize()) {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(Spacing.gutter),
            verticalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
            when {
                style == ReadStyle.Markdown -> PmMarkdown(keepLineBreaks(text), text = type)
                output && !wrap ->
                    Text(
                        text,
                        style = type,
                        softWrap = false,
                        modifier = Modifier.horizontalScroll(rememberScrollState()),
                    )
                else -> Text(text, style = type)
            }
        }
    }
}

/** Put `text` on the clipboard; `confirm` says so where Android doesn't, before Android 13. */
suspend fun Clipboard.copy(label: String, text: String, confirm: suspend () -> Unit) {
    setClipEntry(ClipEntry(ClipData.newPlainText(label, text)))
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) confirm()
}

/** The longest text the reader lays out as one. */
private const val LARGE = 200_000
