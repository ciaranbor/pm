package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.mikepenz.markdown.m3.Markdown
import dev.pm.app.data.Connection
import dev.pm.app.model.Pairing
import dev.pm.app.push.Notifications
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
 * nothing while live.
 */
@Composable
fun StatusStrip(
    connection: Connection,
    readAt: Long?,
    now: Instant,
    retry: () -> Unit,
    pairAgain: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val updated = readAt?.let { " · last update ${ago(it, now)}" }.orEmpty()
    val (title, hint, action) =
        when (connection) {
            Connection.Live,
            Connection.Unpaired -> return
            Connection.Connecting -> Triple("Connecting…$updated", null, null)
            is Connection.Unreachable ->
                Triple("Offline$updated", "Can't reach pm serve. Tailscale off?", "Retry" to retry)
            Connection.Unauthorized ->
                Triple(
                    "Not paired$updated",
                    "The Mac revoked this phone's token.",
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
        if (action != null) TextButton(onClick = action.second) { Text(action.first) }
    }
}

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

@Composable
fun SummaryScreen(model: SummaryModel, modifier: Modifier = Modifier) {
    val state by model.uiState.collectAsStateWithLifecycle()
    when (val shown = state) {
        SummaryState.Loading -> Centered(modifier) { CircularProgressIndicator() }
        is SummaryState.Shown ->
            Column(modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
                Markdown(shown.markdown)
            }
        SummaryState.Missing ->
            Centered(modifier) { Text("No summary yet.", textAlign = TextAlign.Center) }
        SummaryState.Unreachable ->
            Retryable(
                "Can't reach pm serve. Tailscale off?",
                model::retry,
                modifier,
            )
        is SummaryState.Failed ->
            Retryable("Couldn't load the summary: ${shown.reason}", model::retry, modifier)
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

@Composable
fun SettingsScreen(
    pairing: Pairing?,
    connection: Connection,
    vapid: suspend () -> String?,
    pair: () -> Unit,
    unpair: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var distributor by remember { mutableStateOf(Notifications.current(context)) }
    var problem by remember { mutableStateOf<String?>(null) }
    var allowed by remember { mutableStateOf(Notifications.allowed(context)) }
    LifecycleResumeEffect(context) {
        allowed = Notifications.allowed(context)
        onPauseOrDispose {}
    }
    var choosing by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(choosing) {
        val chosen = choosing ?: return@LaunchedEffect
        val key = vapid()
        if (key == null) {
            problem =
                "Connect to the server first: it gives the key a subscription is made against."
        } else {
            Notifications.use(context, chosen, key)
            distributor = chosen
            problem = null
        }
        choosing = null
    }
    Column(
        modifier.verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Server", style = MaterialTheme.typography.titleMedium)
        if (pairing == null) {
            Text("Not paired.")
            OutlinedButton(onClick = pair) { Text("Pair") }
        } else {
            Text(pairing.url)
            Text(
                "This phone is “${pairing.device}”. " +
                    when (connection) {
                        Connection.Live -> "Connected."
                        Connection.Connecting -> "Connecting…"
                        is Connection.Unreachable -> "Unreachable: ${connection.reason}"
                        Connection.Unauthorized -> "Its token was revoked."
                        Connection.Unpaired -> ""
                    }
            )
            OutlinedButton(onClick = pair) { Text("Pair again") }
            OutlinedButton(onClick = unpair) { Text("Forget this server") }
        }

        Text("Notifications", style = MaterialTheme.typography.titleMedium)
        Text(
            "pm serve pushes through a UnifiedPush distributor, which reaches the phone without Tailscale. " +
                "Install ntfy (set to use ntfy.sh) to receive them without Google; otherwise Google's push service is used.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (!allowed) {
            Text(
                "Notifications are off for pm in Android's settings.",
                color = MaterialTheme.colorScheme.error,
            )
            OutlinedButton(onClick = { context.startActivity(Notifications.settings(context)) }) {
                Text("Open notification settings")
            }
        }
        val all = Notifications.distributors(context)
        if (all.isEmpty()) Text("No distributor is available on this phone.")
        Column(Modifier.selectableGroup()) {
            all.forEach { name ->
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier =
                        Modifier.fillMaxWidth()
                            .heightIn(min = 48.dp)
                            .selectable(
                                selected = name == distributor,
                                enabled = pairing != null,
                                role = Role.RadioButton,
                                onClick = { choosing = name },
                            ),
                ) {
                    RadioButton(
                        selected = name == distributor,
                        onClick = null,
                        enabled = pairing != null,
                    )
                    Text(
                        if (name == context.packageName) "Google (built in)" else name,
                        modifier = Modifier.padding(start = 8.dp),
                    )
                }
            }
        }
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}
