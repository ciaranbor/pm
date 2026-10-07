package dev.pm.app.ui

import android.content.ClipData
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.res.painterResource
import dev.pm.app.R
import dev.pm.app.data.Connection
import dev.pm.app.model.Pairing
import dev.pm.app.model.Tone
import dev.pm.app.update.Update
import dev.pm.app.update.Updates
import kotlinx.coroutines.launch

/** The app's version and the server's, and whether they are of one release. */
data class Versions(val app: String, val server: String?) {
    /** Whether the server runs another release than the app; a build from source counts as its. */
    val mismatched: Boolean
        get() = server != null && Updates.compare(app, server) != 0

    /** Both versions, as copied to report a problem. */
    val text: String
        get() = "pm app $app, server ${server ?: "unknown"}"
}

/**
 * The app's own update check: whether it runs, and checking now, which posts the update's
 * notification when there is one.
 */
class UpdateControls(
    val enabled: Boolean,
    val setEnabled: (Boolean) -> Unit,
    val checkNow: suspend () -> Update?,
)

/** All of the screen's text is selectable; Forget asks first. */
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
    var forgetting by rememberSaveable { mutableStateOf(false) }
    Selectable(modifier) {
        Column(Modifier.verticalScroll(rememberScrollState()).padding(bottom = Spacing.l)) {
            Section("Server")
            ServerCard(pairing, connection)
            if (pairing == null) {
                SettingRow("Pair", "Scan or paste a pairing from pm serve", onClick = pair)
            } else {
                SettingRow("Pair again", "Replace this pairing with a new one", onClick = pair)
                SettingRow(
                    "Forget this server",
                    "Unpair this phone and stop its notifications",
                    color = MaterialTheme.colorScheme.error,
                    onClick = { forgetting = true },
                )
            }
            Section("Notifications")
            NotificationRows(pairing, vapid)
            if (updates != null) {
                Section("Updates")
                UpdateRows(updates)
            }
            Section("About")
            AboutRows(versions)
        }
    }
    if (forgetting && pairing != null) {
        AlertDialog(
            onDismissRequest = { forgetting = false },
            title = { Text("Forget this server?") },
            text = {
                Text(
                    "This phone stops reaching ${pairing.url} and gets no more notifications " +
                        "from it. To reconnect, pair again with a new code from pm serve pair."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        forgetting = false
                        unpair()
                    }
                ) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = { TextButton(onClick = { forgetting = false }) { Text("Cancel") } },
        )
    }
}

@Composable
internal fun Section(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier =
            Modifier.padding(
                start = Spacing.gutter,
                end = Spacing.gutter,
                top = Spacing.xl,
                bottom = Spacing.s,
            ),
    )
}

/** A settings row; with no `onClick` it only shows. */
@Composable
internal fun SettingRow(
    headline: String,
    supporting: String?,
    modifier: Modifier = Modifier,
    color: Color = MaterialTheme.colorScheme.onSurface,
    supportingColor: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    enabled: Boolean = true,
    onClick: (() -> Unit)? = null,
    trailing: (@Composable () -> Unit)? = null,
) {
    val alpha = if (enabled) 1f else DISABLED
    ListItem(
        headlineContent = { Text(headline) },
        supportingContent = supporting?.let { { Text(it) } },
        trailingContent = trailing,
        colors =
            ListItemDefaults.colors(
                headlineColor = color.copy(alpha = color.alpha * alpha),
                supportingColor = supportingColor.copy(alpha = supportingColor.alpha * alpha),
            ),
        modifier =
            if (onClick != null) modifier.clickable(enabled = enabled, onClick = onClick)
            else modifier,
    )
}

/** M3's opacity for disabled content. */
private const val DISABLED = 0.38f

@Composable
private fun ServerCard(pairing: Pairing?, connection: Connection) {
    Card(
        colors =
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.gutter),
    ) {
        Column(
            Modifier.padding(Spacing.l),
            verticalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            if (pairing == null) {
                Text("Not paired", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Pair with pm serve to see your projects.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                return@Column
            }
            val (status, tone) = statusOf(connection)
            Text(pairing.url, style = MaterialTheme.typography.titleMedium)
            Text(
                "This phone is “${pairing.device}”",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(status, color = tone.color(), style = MaterialTheme.typography.labelLarge)
        }
    }
}

private fun statusOf(connection: Connection): Pair<String, Tone> =
    when (connection) {
        Connection.Live -> "Connected" to Tone.Positive
        Connection.Connecting,
        Connection.Unpaired -> "Connecting…" to Tone.Neutral
        is Connection.Unreachable -> "Offline: ${connection.reason}" to Tone.Danger
        Connection.Unauthorized -> "The server revoked this phone's token" to Tone.Danger
    }

@Composable
private fun AboutRows(versions: Versions) {
    val clipboard = LocalClipboard.current
    val feedback = LocalFeedback.current
    val scope = rememberCoroutineScope()
    ListItem(
        headlineContent = { Text("App ${versions.app}", softWrap = false) },
        supportingContent = {
            Column {
                Text("Server ${versions.server ?: "unknown"}", softWrap = false)
                if (versions.mismatched) {
                    Text(
                        "Another release than the app: some of what one sends, the other may " +
                            "not understand.",
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        },
        trailingContent = {
            IconButton(
                onClick = {
                    scope.launch {
                        clipboard.setClipEntry(
                            ClipEntry(ClipData.newPlainText("pm versions", versions.text))
                        )
                        feedback.done("Versions copied")
                    }
                }
            ) {
                Icon(painterResource(R.drawable.ic_content_copy), "Copy versions")
            }
        },
    )
}
