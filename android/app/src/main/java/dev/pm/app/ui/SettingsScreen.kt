package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import dev.pm.app.data.Connection
import dev.pm.app.model.Pairing
import dev.pm.app.push.Notifications
import dev.pm.app.update.Update
import dev.pm.app.update.Updates
import kotlinx.coroutines.CancellationException

/** The app's version and the server's, and whether they are of one release. */
data class Versions(val app: String, val server: String?) {
    /** Whether the server runs another release than the app; a build from source counts as its. */
    val mismatched: Boolean
        get() = server != null && Updates.compare(app, server) != 0
}

/** The app's own update check: whether it runs, and checking now. */
class UpdateControls(
    val enabled: Boolean,
    val setEnabled: (Boolean) -> Unit,
    val checkNow: suspend () -> Update?,
)

/** What "Check now" last found. */
private sealed interface Checked {
    data object Checking : Checked

    data object Current : Checked

    data class Available(val update: Update) : Checked

    data class Failed(val reason: String) : Checked
}

@Composable
fun SettingsScreen(
    pairing: Pairing?,
    connection: Connection,
    versions: Versions,
    vapid: suspend () -> String?,
    /** `null` in a build that never updates itself. */
    updates: UpdateControls?,
    pair: () -> Unit,
    unpair: () -> Unit,
    modifier: Modifier = Modifier,
) {
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
        NotificationSettings(pairing, vapid)
        UpdateSettings(versions, updates)
    }
}

@Composable
private fun NotificationSettings(
    pairing: Pairing?,
    vapid: suspend () -> String?,
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
    Column(modifier, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Notifications", style = MaterialTheme.typography.titleMedium)
        Text(
            "pm serve pushes through a UnifiedPush distributor, which reaches the phone without " +
                "Tailscale: ntfy (set to use ntfy.sh) if installed, else Google's push service " +
                "where this build has it. Without either, pm checks the server every 15 minutes " +
                "or so while the phone is on the tailnet, and less often while it sleeps.",
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
        if (all.isEmpty()) Text("No distributor is available: notifications come from polling.")
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

@Composable
private fun UpdateSettings(
    versions: Versions,
    updates: UpdateControls?,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var checked by remember { mutableStateOf<Checked?>(null) }
    LaunchedEffect(checked) {
        if (checked != Checked.Checking) return@LaunchedEffect
        checked =
            try {
                updates?.checkNow()?.let { Checked.Available(it) } ?: Checked.Current
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                Checked.Failed(e.message ?: e.javaClass.simpleName)
            }
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("About", style = MaterialTheme.typography.titleMedium)
        Text("App ${versions.app} · Server ${versions.server ?: "unknown"}")
        if (versions.mismatched) {
            Text(
                "The app and the server are of different pm releases; some of what one sends the " +
                    "other may not understand.",
                color = MaterialTheme.colorScheme.error,
            )
        }
        if (updates == null) return@Column
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier =
                Modifier.fillMaxWidth()
                    .heightIn(min = 48.dp)
                    .toggleable(
                        value = updates.enabled,
                        role = Role.Switch,
                        onValueChange = updates.setEnabled,
                    ),
        ) {
            Text("Check for updates", modifier = Modifier.weight(1f))
            Switch(checked = updates.enabled, onCheckedChange = null)
        }
        OutlinedButton(
            onClick = { checked = Checked.Checking },
            enabled = checked != Checked.Checking,
        ) {
            Text("Check now")
        }
        when (val shown = checked) {
            null,
            Checked.Checking -> {}
            Checked.Current -> Text("This is the latest release.")
            is Checked.Failed -> Text("Couldn't check: ${shown.reason}")
            is Checked.Available -> {
                Text("pm ${shown.update.version} is available.")
                OutlinedButton(
                    onClick = { context.startActivity(Notifications.download(shown.update)) }
                ) {
                    Text("Download")
                }
            }
        }
    }
}
