package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.mikepenz.markdown.compose.components.MarkdownComponents
import com.mikepenz.markdown.compose.components.markdownComponents
import dev.pm.app.data.Connection
import dev.pm.app.model.FeatureInfo
import java.time.Instant

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
                .padding(start = Spacing.l, end = Spacing.s, top = Spacing.xs, bottom = Spacing.xs)
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
            manual.pending -> PendingButton("Retry", {}, pending = true, emphasis = Emphasis.Text)
            action != null -> TextButton(onClick = action.second) { Text(action.first) }
        }
    }
}

/** Why the server can't be reached; `again` after a retry the user asked for failed. */
internal fun unreachable(again: Boolean): String =
    if (again) "Still can't reach pm serve. $UNREACHABLE_HINT"
    else "Can't reach pm serve. $UNREACHABLE_HINT"

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
            ReadState.Missing -> EmptyState(missing)
            ReadState.Unreachable ->
                ErrorState("Can't reach pm serve", model::retry, hint = UNREACHABLE_HINT)
            is ReadState.Failed ->
                ErrorState("Couldn't load the $what", model::retry, hint = shown.reason)
        }
    }
}

@Composable
fun SummaryScreen(model: ReadModel<String>, modifier: Modifier = Modifier) {
    ReadScreen(model, "summary", "No summary yet.", modifier) { MarkdownPage(it) }
}

/** The brief `pm feat new --context` gave the feature, often plain text: its lines kept. */
@Composable
fun BriefScreen(model: ReadModel<FeatureInfo>, modifier: Modifier = Modifier) {
    ReadScreen(model, "brief", GONE, modifier) {
        val brief = it.context
        if (brief == null) EmptyState("No brief.") else MarkdownPage(keepLineBreaks(brief))
    }
}

@Composable
fun DetailsScreen(model: ReadModel<FeatureInfo>, modifier: Modifier = Modifier) {
    ReadScreen(model, "details", GONE, modifier) { DetailsPage(it) }
}

private const val GONE = "This feature is no longer there."

/**
 * `markdown`, selectable, under `actions`; `components` draws its parts, and `bottom` is room left
 * below it, as for a floating button.
 */
@Composable
internal fun MarkdownPage(
    markdown: String,
    modifier: Modifier = Modifier,
    components: MarkdownComponents = markdownComponents(),
    bottom: Dp = 0.dp,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(Spacing.gutter)
            .padding(bottom = bottom)
    ) {
        Row(Modifier.align(Alignment.End), content = actions)
        Selectable { PmMarkdown(markdown, components = components) }
    }
}

/** A feature's details, label by value, the values selectable. */
@Composable
private fun DetailsPage(info: FeatureInfo, modifier: Modifier = Modifier) {
    Column(modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(Spacing.gutter)) {
        Selectable {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.m)) {
                info.rows().forEach { (label, value) ->
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

/** What to check when pm serve can't be reached. */
const val UNREACHABLE_HINT = "Is Tailscale on, and pm serve running?"
