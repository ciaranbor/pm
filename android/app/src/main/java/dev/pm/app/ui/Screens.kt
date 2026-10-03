package dev.pm.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.layout.Row
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.mikepenz.markdown.m3.Markdown
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.data.Connection
import dev.pm.app.model.Pairing
import dev.pm.app.push.Notifications
import java.text.DateFormat
import java.util.Date

/** How the app stands with the server, over the content it qualifies. */
@Composable
fun ConnectionBanner(connection: Connection, readAt: Long?, retry: () -> Unit) {
    val text = when (connection) {
        Connection.Live, Connection.Unpaired -> return
        Connection.Connecting -> "Connecting…"
        is Connection.Unreachable -> {
            val age = readAt?.let { " · showing what was known at ${DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(it))}" }.orEmpty()
            "Server unreachable: connect Tailscale to open$age"
        }
        Connection.Unauthorized -> "This phone's token was revoked; pair again in Settings."
    }
    Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        textAlign = TextAlign.Center,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .clickable(onClick = retry)
            .padding(8.dp),
    )
}

@Composable
fun SummaryScreen(client: PmClient?, project: String, feature: String) {
    var summary by remember { mutableStateOf<Result<String>?>(null) }
    LaunchedEffect(project, feature) {
        summary = client?.let { runCatching { it.summary(project, feature) } }
            ?: Result.failure(IllegalStateException("not paired"))
    }
    val shown = summary
    when {
        shown == null -> Centered { CircularProgressIndicator() }
        shown.isSuccess -> Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp)) {
            Markdown(shown.getOrThrow())
        }
        else -> Centered {
            val e = shown.exceptionOrNull()
            Text(
                when {
                    e is PmError.Status && e.code == 404 -> "No summary yet."
                    e is PmError.Unreachable -> "Server unreachable: connect Tailscale to open the summary."
                    else -> "Couldn't load the summary: ${e?.message}"
                },
                textAlign = TextAlign.Center,
            )
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
) {
    val context = LocalContext.current
    var distributor by remember { mutableStateOf(Notifications.current(context)) }
    var problem by remember { mutableStateOf<String?>(null) }
    var choosing by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(choosing) {
        val chosen = choosing ?: return@LaunchedEffect
        val key = vapid()
        if (key == null) {
            problem = "Connect to the server first: it gives the key a subscription is made against."
        } else {
            Notifications.use(context, chosen, key)
            distributor = chosen
            problem = null
        }
        choosing = null
    }
    Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Server", style = MaterialTheme.typography.titleMedium)
        if (pairing == null) {
            Text("Not paired.")
            OutlinedButton(onClick = pair) { Text("Pair") }
        } else {
            Text(pairing.url)
            Text("This phone is “${pairing.device}”. " + when (connection) {
                Connection.Live -> "Connected."
                Connection.Connecting -> "Connecting…"
                is Connection.Unreachable -> "Unreachable: ${connection.reason}"
                Connection.Unauthorized -> "Its token was revoked."
                Connection.Unpaired -> ""
            })
            OutlinedButton(onClick = pair) { Text("Pair again") }
            OutlinedButton(onClick = unpair) { Text("Forget this server") }
        }

        Text("Notifications", style = MaterialTheme.typography.titleMedium)
        Text(
            "pm serve pushes through a UnifiedPush distributor, which reaches the phone without Tailscale. " +
                "Install ntfy (set to use ntfy.sh) to receive them without Google; otherwise Google's push service is used.",
            style = MaterialTheme.typography.bodySmall,
        )
        if (!Notifications.allowed(context)) Text("Notifications are off for pm in Android's settings.", color = MaterialTheme.colorScheme.error)
        val all = Notifications.distributors(context)
        if (all.isEmpty()) Text("No distributor is available on this phone.")
        all.forEach { name ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().clickable(enabled = pairing != null) { choosing = name },
            ) {
                RadioButton(selected = name == distributor, onClick = null)
                Text(if (name == context.packageName) "Google (built in)" else name, modifier = Modifier.padding(start = 8.dp))
            }
        }
        problem?.let { Text(it, color = MaterialTheme.colorScheme.error) }
    }
}
