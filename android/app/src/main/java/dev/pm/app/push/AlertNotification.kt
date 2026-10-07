package dev.pm.app.push

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import dev.pm.app.MainActivity
import dev.pm.app.R
import dev.pm.app.model.Alert
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot

/**
 * One alert's notification: titled by its feature, under its project, with what was said in it as a
 * conversation with the agent it names. Every alert opens its agent; a permission prompt also has
 * Allow and Deny ([AnswerReceiver]) and an agent waiting on the user an inline reply
 * ([ReplyReceiver]), each only on an unlocked phone.
 */
internal object AlertNotification {
    const val EXTRA_ALERT = "dev.pm.app.alert"

    fun build(
        context: Context,
        alert: Alert,
        channel: String,
        group: String,
        now: Long,
    ): NotificationCompat.Builder {
        val transition = alert.transition
        val tag = PushedTransition.encode(transition.key)
        val encoded = Alert.encode(alert)
        val open = open(context, transition, tag)
        val builder =
            NotificationCompat.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(transition.title)
                .setContentText(alert.lines.lastOrNull()?.let { text(context, it) })
                .setWhen(now)
                .setShowWhen(true)
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setContentIntent(open)
                .setAutoCancel(true)
                .setGroup(group)
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_SUMMARY)
                .addExtras(Bundle().apply { putString(EXTRA_ALERT, encoded) })
        if (transition.scope != Snapshot.MAIN) builder.setSubText(transition.project)
        builder.setStyle(style(context, alert, now))
        alert.prompt?.let { prompt ->
            val groups = prompt.groups ?: return@let
            val deny = groups.negative ?: return@let
            builder.addAction(answer(context, tag, encoded, groups.primary.id, Answer.Allow))
            builder.addAction(answer(context, tag, encoded, deny.id, Answer.Deny))
        }
        if (alert.replyable && !alert.settled) {
            builder.addAction(reply(context, transition.agent!!, tag, encoded))
        }
        return builder.addAction(
            NotificationCompat.Action.Builder(
                    R.drawable.ic_open_in_new,
                    context.getString(R.string.open_action),
                    open,
                )
                .build()
        )
    }

    /**
     * A conversation with the agent the alert names, under the feature's title; with no agent, the
     * lines as one text.
     */
    private fun style(context: Context, alert: Alert, now: Long): NotificationCompat.Style {
        val agent = alert.transition.agent
        if (agent == null || alert.transition.kindOf == AttentionKind.Dead) {
            return NotificationCompat.BigTextStyle()
                .bigText(alert.lines.joinToString("\n") { text(context, it) })
        }
        val them = Person.Builder().setName(agent).setKey(agent).build()
        val pm = Person.Builder().setName(context.getString(R.string.app_name)).setKey("pm").build()
        val you = Person.Builder().setName(context.getString(R.string.reply_you)).build()
        val style =
            NotificationCompat.MessagingStyle(you)
                .setConversationTitle(alert.transition.title)
                .setGroupConversation(true)
        alert.lines.forEach {
            val by =
                when (it.by) {
                    Alert.By.Agent -> them
                    Alert.By.Pm -> pm
                    Alert.By.You -> null
                }
            style.addMessage(text(context, it), now, by)
        }
        return style
    }

    private fun text(context: Context, line: Alert.Line): String =
        line.failure?.let { context.getString(R.string.reply_not_sent, it, line.text) } ?: line.text

    private fun open(context: Context, transition: PushedTransition, tag: String): PendingIntent {
        val intent =
            Target(transition.project, transition.scope, transition.agent)
                .into(Intent(context, MainActivity::class.java))
                .setData(Uri.fromParts("pm", tag, null))
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun answer(
        context: Context,
        tag: String,
        encoded: String,
        choice: String,
        answer: Answer,
    ): NotificationCompat.Action {
        val intent =
            Intent(context, AnswerReceiver::class.java)
                .setData(Uri.fromParts("pm-answer", tag, choice))
                .putExtra(EXTRA_ALERT, encoded)
                .putExtra(AnswerReceiver.EXTRA_CHOICE, choice)
                .putExtra(AnswerReceiver.EXTRA_SAID, context.getString(answer.said))
        val pending =
            PendingIntent.getBroadcast(
                context,
                0,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        return NotificationCompat.Action.Builder(
                answer.icon,
                context.getString(answer.label),
                pending,
            )
            .setAuthenticationRequired(true)
            .setShowsUserInterface(false)
            .build()
    }

    private fun reply(
        context: Context,
        agent: String,
        tag: String,
        encoded: String,
    ): NotificationCompat.Action {
        val remote =
            RemoteInput.Builder(ReplyReceiver.KEY_TEXT)
                .setLabel(context.getString(R.string.reply_label, agent))
                .build()
        val send =
            PendingIntent.getBroadcast(
                context,
                0,
                Intent(context, ReplyReceiver::class.java)
                    .setData(Uri.fromParts("pm-reply", tag, null))
                    .putExtra(EXTRA_ALERT, encoded),
                // Mutable, the only one: RemoteInput writes the reply into it.
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
            )
        return NotificationCompat.Action.Builder(
                R.drawable.ic_send,
                context.getString(R.string.reply_action),
                send,
            )
            .addRemoteInput(remote)
            .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
            .setAuthenticationRequired(true)
            .setShowsUserInterface(false)
            .build()
    }

    private enum class Answer(val icon: Int, val label: Int, val said: Int) {
        Allow(R.drawable.ic_check, R.string.answer_allow, R.string.answered_allow),
        Deny(R.drawable.ic_close, R.string.answer_deny, R.string.answered_deny),
    }

    /** The alert an intent from one of an alert's actions carries. */
    fun of(intent: Intent): Alert? = intent.getStringExtra(EXTRA_ALERT)?.let(Alert::parse)
}
