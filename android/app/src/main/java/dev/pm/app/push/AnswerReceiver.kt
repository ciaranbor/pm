package dev.pm.app.push

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.container
import dev.pm.app.model.Alert
import dev.pm.app.model.Dialog
import dev.pm.app.model.DialogAnswer
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Answers an alert's permission prompt with the choice its Allow or Deny names, as the dialog card
 * does: Deny, with no message, also stops the agent's turn. Then the alert shows how that went and
 * the agent's next open dialog, if any. Within a broadcast's 10 s: 7 for the answer, 2 for the
 * next.
 */
class AnswerReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val alert = AlertNotification.of(intent) ?: return
        val choice = intent.getStringExtra(EXTRA_CHOICE) ?: return
        val said = intent.getStringExtra(EXTRA_SAID) ?: choice
        val app = context.applicationContext
        val pending = goAsync()
        CoroutineScope(Dispatchers.IO).launch {
            try {
                val client = app.container.repository.loadedClient()
                Notifications.acted(app, answer(client, alert, choice, said))
            } finally {
                pending.finish()
            }
        }
    }

    companion object {
        const val EXTRA_CHOICE = "dev.pm.app.choice"
        const val EXTRA_SAID = "dev.pm.app.said"

        /**
         * Answer `alert`'s prompt with `choice`, which the user knows as `said`: the alert showing
         * what came of it. A prompt already answered or closed shows why, and the next; one that
         * couldn't be answered can be again.
         */
        suspend fun answer(
            client: PmClient?,
            alert: Alert,
            choice: String,
            said: String,
            within: Duration = 7.seconds,
            settle: Duration = 2.seconds,
        ): Alert {
            val prompt = alert.prompt ?: return alert
            val transition = alert.transition
            val agent = transition.agent ?: return alert
            client ?: return alert.failed(said, "not paired")
            val sent =
                Sending.attempt<Sent>(within, failed = { Sent.Failed(it) }) {
                    try {
                        client.answerDialog(
                            transition.project,
                            transition.scope,
                            agent,
                            DialogAnswer(prompt.id, choice),
                        )
                        Sent.Answered
                    } catch (e: PmError.Refused) {
                        when (e.code) {
                            "answered" -> Sent.Closed("Answered elsewhere")
                            "gone" ->
                                Sent.Closed(
                                    "It can no longer be answered here; answer it at the terminal"
                                )
                            else -> throw e
                        }
                    } catch (e: PmError.Status) {
                        if (e.code != 404) throw e
                        Sent.Closed("It is no longer open")
                    }
                }
            return when (sent) {
                is Sent.Failed -> alert.failed(said, sent.why)
                Sent.Answered -> alert.answered(said, next(client, alert, agent, prompt.id, settle))
                is Sent.Closed ->
                    alert.closed(sent.note, next(client, alert, agent, prompt.id, settle))
            }
        }

        /** What came of sending an answer. */
        private sealed interface Sent {
            data object Answered : Sent

            /** The prompt was already closed, as `note` says. */
            data class Closed(val note: String) : Sent

            data class Failed(val why: String) : Sent
        }

        /**
         * The agent's open dialogs but `answered`, oldest first, waiting up to `settle` for one to
         * open: Claude Code shows the next of parallel tool calls' prompts only once the one before
         * is answered, so it opens just after.
         */
        private suspend fun next(
            client: PmClient,
            alert: Alert,
            agent: String,
            answered: String,
            settle: Duration,
        ): List<Dialog> {
            var open = emptyList<Dialog>()
            withTimeoutOrNull(settle) {
                while (true) {
                    open =
                        try {
                            client
                                .dialogs(alert.transition.project, alert.transition.scope, agent)
                                .filter { it.id != answered }
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Exception) {
                            open
                        }
                    if (open.isNotEmpty()) break
                    delay(400.milliseconds)
                }
            }
            return open
        }
    }
}
