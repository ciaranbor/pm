package dev.pm.app.ui

import android.content.ClipData
import android.os.Build
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.mikepenz.markdown.compose.components.MarkdownComponents
import com.mikepenz.markdown.compose.components.markdownComponents
import com.mikepenz.markdown.m3.Markdown
import com.mikepenz.markdown.model.rememberMarkdownState
import dev.pm.app.R
import dev.pm.app.data.Connection
import dev.pm.app.model.FeatureInfo
import java.time.Instant
import kotlinx.coroutines.launch

/** How long ago `then` (epoch ms) was, in words. */
fun ago(then: Long, now: Instant): String {
    val mins = (now.toEpochMilli() - then).coerceAtLeast(0) / 60_000
    return when {
        mins < 1 -> "just now"
        mins < 60 -> "$mins min ago"
        mins < 24 * 60 -> "${mins / 60} h ago"
        else -> "${mins / (24 * 60)} d ago"
    }
}

/**
 * How the app stands with the server, over the content it qualifies, with what to do about it;
 * nothing while live. A retry shows progress in place of its button, then the outcome.
 */
@Composable
fun StatusStrip(
    connection: Connection,
    retry: () -> Unit,
    pairAgain: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val manual = rememberRetry(connection, retry)
    val (title, hint, action) =
        if (manual.pending) Triple("Connecting…", "Trying pm serve again.", null)
        else
            when (connection) {
                Connection.Live,
                Connection.Unpaired -> return
                Connection.Connecting -> Triple("Connecting…", null, null)
                is Connection.Unreachable ->
                    Triple("Offline", unreachable(manual.failedAgain), "Retry" to manual::start)
                Connection.Unauthorized ->
                    Triple(
                        "Not paired",
                        "The server revoked this phone's token.",
                        "Pair again" to pairAgain,
                    )
            }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier =
            modifier
                .fillMaxWidth()
                .background(MaterialTheme.colorScheme.secondaryContainer)
                .padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp)
                .heightIn(min = 40.dp),
    ) {
        Column(
            Modifier.weight(1f).semantics(mergeDescendants = true) {
                liveRegion = LiveRegionMode.Polite
            }
        ) {
            Text(
                title,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
            )
            if (hint != null) {
                Text(
                    hint,
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
            }
        }
        when {
            manual.pending ->
                Box(Modifier.padding(horizontal = 12.dp).size(48.dp), Alignment.Center) {
                    CircularProgressIndicator(
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onSecondaryContainer,
                        modifier = Modifier.size(20.dp),
                    )
                }
            action != null -> TextButton(onClick = action.second) { Text(action.first) }
        }
    }
}

/** Why the server can't be reached; `again` after a retry the user asked for failed. */
internal fun unreachable(again: Boolean): String =
    if (again) "Still can't reach pm serve. Tailscale off?"
    else "Can't reach pm serve. Tailscale off?"

/** A screen with nothing to show yet: why, and the one thing to do about it. */
@Composable
fun EmptyState(
    title: String,
    hint: String,
    action: String,
    onAction: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Centered(modifier) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
            Text(
                hint,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            OutlinedButton(onClick = onAction, modifier = Modifier.padding(top = 8.dp)) {
                Text(action)
            }
        }
    }
}

/**
 * What `model` read, shown by `content`; else why not. `what` names it in a failure, `missing` is
 * what a 404 means.
 */
@Composable
fun <T> ReadScreen(
    model: ReadModel<T>,
    what: String,
    missing: String,
    modifier: Modifier = Modifier,
    content: @Composable (T) -> Unit,
) {
    val state by model.uiState.collectAsStateWithLifecycle()
    Box(modifier) {
        when (val shown = state) {
            ReadState.Loading -> Centered { CircularProgressIndicator() }
            is ReadState.Shown -> content(shown.value)
            ReadState.Missing -> Centered { Text(missing, textAlign = TextAlign.Center) }
            ReadState.Unreachable -> Retryable("Can't reach pm serve. Tailscale off?", model::retry)
            is ReadState.Failed ->
                Retryable("Couldn't load the $what: ${shown.reason}", model::retry)
        }
    }
}

@Composable
fun SummaryScreen(model: ReadModel<String>, modifier: Modifier = Modifier) {
    ReadScreen(model, "summary", "No summary yet.", modifier) { MarkdownPage(it, "Summary") }
}

/** The brief `pm feat new --context` gave the feature. */
@Composable
fun BriefScreen(model: ReadModel<FeatureInfo>, modifier: Modifier = Modifier) {
    ReadScreen(model, "brief", GONE, modifier) {
        val brief = it.context
        if (brief == null) Centered { Text("No brief.") } else MarkdownPage(brief, "Brief")
    }
}

@Composable
fun DetailsScreen(model: ReadModel<FeatureInfo>, modifier: Modifier = Modifier) {
    ReadScreen(model, "details", GONE, modifier) { DetailsPage(it) }
}

private const val GONE = "This feature is no longer there."

/**
 * `markdown`, selectable, under a button copying its source as `label`, after `actions`;
 * `components` draws its parts, and `bottom` is room left below it, as for a floating button.
 */
@Composable
internal fun MarkdownPage(
    markdown: String,
    label: String,
    modifier: Modifier = Modifier,
    components: MarkdownComponents = markdownComponents(),
    bottom: Dp = 0.dp,
    actions: @Composable RowScope.() -> Unit = {},
) {
    val state = rememberMarkdownState(markdown, retainState = true)
    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp)
            .padding(bottom = bottom)
    ) {
        Row(Modifier.align(Alignment.End)) {
            actions()
            CopyButton(label, markdown)
        }
        SelectionContainer {
            Markdown(
                markdownState = state,
                components = components,
                loading = { Text(markdown, it, style = MaterialTheme.typography.bodyMedium) },
            )
        }
    }
}

/** A feature's details, label by value, the values selectable. */
@Composable
private fun DetailsPage(info: FeatureInfo, modifier: Modifier = Modifier) {
    Column(modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
        CopyButton("Details", info.text, Modifier.align(Alignment.End))
        SelectionContainer {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                info.rows.forEach { (label, value) ->
                    Column {
                        Text(
                            label,
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(value, style = MaterialTheme.typography.bodyLarge)
                    }
                }
            }
        }
    }
}

/** Puts `text` on the clipboard as `label`, saying so where the system doesn't (before 13). */
@Composable
fun CopyButton(label: String, text: String, modifier: Modifier = Modifier) {
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    TextButton(
        onClick = {
            scope.launch {
                clipboard.setClipEntry(ClipEntry(ClipData.newPlainText(label, text)))
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU)
                    Toast.makeText(context, "Copied", Toast.LENGTH_SHORT).show()
            }
        },
        modifier = modifier,
    ) {
        Icon(painterResource(R.drawable.ic_content_copy), null, Modifier.size(18.dp))
        Spacer(Modifier.width(8.dp))
        Text("Copy")
    }
}

@Composable
internal fun Retryable(text: String, retry: () -> Unit, modifier: Modifier = Modifier) {
    Centered(modifier) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(text, textAlign = TextAlign.Center)
            OutlinedButton(onClick = retry) { Text("Retry") }
        }
    }
}
