package dev.pm.app.push

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.container
import dev.pm.app.model.PushedTransition
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout

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
        val transition = Notifications.replyingTo(intent) ?: return
        val app = context.applicationContext
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                val failure = reply(app.container.repository.loadedClient(), transition, text)
                Notifications.replied(app, transition, text, failure)
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val KEY_TEXT = "dev.pm.app.reply"

        /** Send `text` to the agent `transition` names: why it wasn't sent, or `null` if it was. */
        suspend fun reply(
            client: PmClient?,
            transition: PushedTransition,
            text: String,
            within: Duration = 9.seconds,
        ): String? {
            val agent = transition.agent ?: return "no agent to send to"
            client ?: return "not paired"
            return try {
                withTimeout(within) {
                    client.sendText(transition.project, transition.scope, agent, text)
                }
                null
            } catch (e: TimeoutCancellationException) {
                "no answer in time"
            } catch (e: CancellationException) {
                throw e
            } catch (e: PmError.Unreachable) {
                "tailnet unreachable"
            } catch (e: Exception) {
                e.message ?: e.javaClass.simpleName
            }
        }
    }
}
