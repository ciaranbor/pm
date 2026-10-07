package dev.pm.app.ui

import android.content.Context
import android.content.pm.PackageManager
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import dev.pm.app.R
import dev.pm.app.model.Pairing
import dev.pm.app.push.Notifications
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@Composable
internal fun NotificationRows(pairing: Pairing?, vapid: suspend () -> String?) {
    val context = LocalContext.current
    val feedback = LocalFeedback.current
    val scope = rememberCoroutineScope()
    var distributor by remember { mutableStateOf(Notifications.current(context)) }
    var allowed by remember { mutableStateOf(Notifications.allowed(context)) }
    var choosing by rememberSaveable { mutableStateOf(false) }
    var switching by remember { mutableStateOf(false) }
    LifecycleResumeEffect(context) {
        allowed = Notifications.allowed(context)
        onPauseOrDispose {}
    }
    val all = Notifications.distributors(context)

    fun use(chosen: String) {
        switching = true
        scope.launch {
            val key = vapid()
            switching = false
            if (key == null) {
                feedback.failed("Connect to pm serve first: it gives the key to subscribe with") {
                    use(chosen)
                }
            } else {
                Notifications.use(context, chosen, key)
                distributor = chosen
                feedback.done("Notifications come through ${labelOf(context, chosen)}")
            }
        }
    }

    Column {
        SettingRow(
            "Delivered by",
            when {
                all.isEmpty() -> "Polling pm serve every 15 minutes or so; no push app is installed"
                else ->
                    distributor?.let { labelOf(context, it) }
                        ?: "Polling pm serve, until a push app is chosen"
            },
            enabled = pairing != null && all.isNotEmpty() && !switching,
            onClick = { choosing = true }.takeIf { all.isNotEmpty() },
            modifier = Modifier.semantics { if (switching) stateDescription = "In progress" },
            trailing =
                if (switching) {
                    { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
                } else null,
        )
        SettingRow(
            "System settings",
            if (allowed) "Which alerts sound, and how" else "Notifications are off for pm",
            supportingColor =
                if (allowed) MaterialTheme.colorScheme.onSurfaceVariant
                else MaterialTheme.colorScheme.error,
            onClick = { context.startActivity(Notifications.settings(context)) },
            trailing = { Icon(painterResource(R.drawable.ic_open_in_new), null) },
        )
        if (choosing) {
            AlertDialog(
                onDismissRequest = { choosing = false },
                title = { Text("Deliver notifications by") },
                text = {
                    Column(Modifier.selectableGroup()) {
                        Text(
                            "pm serve pushes through the app chosen here, which reaches the phone off the tailnet.",
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.padding(bottom = Spacing.s),
                        )
                        all.forEach { name ->
                            ListItem(
                                headlineContent = { Text(labelOf(context, name)) },
                                leadingContent = {
                                    RadioButton(selected = name == distributor, onClick = null)
                                },
                                colors =
                                    ListItemDefaults.colors(containerColor = Color.Transparent),
                                modifier =
                                    Modifier.selectable(
                                        selected = name == distributor,
                                        role = Role.RadioButton,
                                        onClick = {
                                            choosing = false
                                            if (name != distributor) use(name)
                                        },
                                    ),
                            )
                        }
                    }
                },
                confirmButton = {},
                dismissButton = { TextButton(onClick = { choosing = false }) { Text("Cancel") } },
            )
        }
    }
}

/** What the user calls distributor `name`, a package: its app's label. */
private fun labelOf(context: Context, name: String): String =
    if (name == context.packageName) "Google (built in)"
    else
        try {
            val pm = context.packageManager
            pm.getApplicationLabel(pm.getApplicationInfo(name, 0)).toString()
        } catch (_: PackageManager.NameNotFoundException) {
            name
        }

@Composable
internal fun UpdateRows(updates: UpdateControls) {
    val context = LocalContext.current
    val feedback = LocalFeedback.current
    val scope = rememberCoroutineScope()
    var checking by remember { mutableStateOf(false) }

    fun check() {
        checking = true
        scope.launch {
            val found =
                try {
                    Result.success(updates.checkNow())
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    Result.failure(e)
                } finally {
                    checking = false
                }
            found.fold(
                onSuccess = { update ->
                    when {
                        update == null -> feedback.done("This is the latest release")
                        Notifications.allowed(context) ->
                            feedback.done("pm ${update.version} is available")
                        else ->
                            feedback.done("pm ${update.version} is available", "Download") {
                                context.startActivity(Notifications.download(update))
                            }
                    }
                },
                onFailure = { e ->
                    feedback.failed("Couldn't check: ${e.message ?: e.javaClass.simpleName}") {
                        check()
                    }
                },
            )
        }
    }

    Column {
        SettingRow(
            "Check for updates",
            "Daily, with a notification for a new release",
            modifier =
                Modifier.toggleable(
                    value = updates.enabled,
                    role = Role.Switch,
                    onValueChange = updates.setEnabled,
                ),
            trailing = { Switch(checked = updates.enabled, onCheckedChange = null) },
        )
        SettingRow(
            "Check now",
            if (checking) "Checking…" else null,
            enabled = !checking,
            onClick = ::check,
            modifier = Modifier.semantics { if (checking) stateDescription = "In progress" },
            trailing =
                if (checking) {
                    { CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp) }
                } else null,
        )
    }
}
