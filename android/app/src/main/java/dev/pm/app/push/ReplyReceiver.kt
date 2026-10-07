package dev.pm.app.push

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import dev.pm.app.api.PmClient
import dev.pm.app.container
import dev.pm.app.model.Alert
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch

/**
 * Sends an inline reply from an alert to the agent it names, then updates the alert with how that
 * went. A broadcast has 10 s before Android may kill the process, so the send gets 9.
 */
class ReplyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val text =
            RemoteInput.getResultsFromIntent(intent)
                ?.getCharSequence(KEY_TEXT)
                ?.toString()
                ?.takeIf { it.isNotBlank() } ?: return
        val alert = AlertNotification.of(intent) ?: return
        val app = context.applicationContext
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                Notifications.acted(
                    app,
                    reply(app.container.repository.loadedClient(), alert, text),
                )
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val KEY_TEXT = "dev.pm.app.reply"

        /** Send `text` to the agent `alert` names: the alert showing what came of it. */
        suspend fun reply(
            client: PmClient?,
            alert: Alert,
            text: String,
            within: Duration = 9.seconds,
        ): Alert {
            val transition = alert.transition
            val agent = transition.agent ?: return alert.failed(text, "no agent to send to")
            client ?: return alert.failed(text, "not paired")
            return Sending.attempt(within, failed = { alert.failed(text, it) }) {
                client.sendText(transition.project, transition.scope, agent, text)
                alert.replied(text)
            }
        }
    }
}
