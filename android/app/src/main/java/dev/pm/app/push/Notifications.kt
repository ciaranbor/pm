package dev.pm.app.push

import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.core.net.toUri
import dev.pm.app.MainActivity
import dev.pm.app.R
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot
import dev.pm.app.update.Update
import org.unifiedpush.android.connector.UnifiedPush

/** Where a notification leads: the scope, and the agent when one is named. */
data class Target(val project: String, val scope: String, val agent: String?) {
    fun into(intent: Intent): Intent =
        intent
            .putExtra(EXTRA_PROJECT, project)
            .putExtra(EXTRA_SCOPE, scope)
            .putExtra(EXTRA_AGENT, agent)

    companion object {
        private const val EXTRA_PROJECT = "dev.pm.app.project"
        private const val EXTRA_SCOPE = "dev.pm.app.scope"
        private const val EXTRA_AGENT = "dev.pm.app.agent"

        fun from(intent: Intent?): Target? {
            val project = intent?.getStringExtra(EXTRA_PROJECT) ?: return null
            val scope = intent.getStringExtra(EXTRA_SCOPE) ?: return null
            return Target(project, scope, intent.getStringExtra(EXTRA_AGENT))
        }
    }
}

object Notifications {
    /** Channel ids are permanent: once created, only the user changes a channel's importance. */
    private enum class Channel(
        val id: String,
        val title: Int,
        val description: Int,
        val importance: Int,
    ) {
        NeedsInput(
            "needs-input",
            R.string.channel_needs_input,
            R.string.channel_needs_input_description,
            NotificationManager.IMPORTANCE_HIGH,
        ),
        Ready(
            "ready",
            R.string.channel_ready,
            R.string.channel_ready_description,
            NotificationManager.IMPORTANCE_DEFAULT,
        ),
        Died(
            "agent-died",
            R.string.channel_died,
            R.string.channel_died_description,
            NotificationManager.IMPORTANCE_HIGH,
        ),
        AppUpdate(
            "app-update",
            R.string.channel_update,
            R.string.channel_update_description,
            NotificationManager.IMPORTANCE_DEFAULT,
        );

        companion object {
            fun of(kind: AttentionKind): Channel =
                when (kind) {
                    AttentionKind.Blocked,
                    AttentionKind.Asking -> NeedsInput
                    AttentionKind.Dead -> Died
                    else -> Ready
                }
        }
    }

    /** The single channel of earlier versions. */
    private const val RETIRED_CHANNEL = "attention"
    private const val GROUP = "dev.pm.app.attention"
    private const val SUMMARY_ID = 0
    /** Every alert's id; its tag, the encoded [PushedTransition.key], tells them apart. */
    private const val ALERT_ID = 1
    private const val EXTRA_TRANSITION = "dev.pm.app.transition"
    private const val UPDATE_ID = 2

    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.deleteNotificationChannel(RETIRED_CHANNEL)
        manager.createNotificationChannels(
            Channel.entries.map {
                NotificationChannel(it.id, context.getString(it.title), it.importance).apply {
                    description = context.getString(it.description)
                }
            }
        )
    }

    /**
     * An alert of `transition`, replacing one with the same [PushedTransition.key], under a summary
     * that alerts for the group on `transition`'s channel.
     */
    fun show(
        context: Context,
        transition: PushedTransition,
        now: Long = System.currentTimeMillis(),
    ) {
        if (!allowed(context)) return
        val tag = PushedTransition.encode(transition.key)
        val channel = Channel.of(transition.kindOf)
        val alert = alert(context, transition, now).build()
        // What's showing is read before posting: a post reaches the active list asynchronously.
        val others = newestFirst(alerts(context).filter { (sbn, _) -> sbn.tag != tag })
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(tag, ALERT_ID, alert)
        summarize(context, channel.id, listOf(transition) + others, silent = false)
    }

    /**
     * Show what came of the user's inline reply `text` to `transition`'s alert, silently: the reply
     * under the agent's message, or, when `failure` says why it wasn't sent, the text with the
     * reason, and the reply action again.
     */
    fun replied(
        context: Context,
        transition: PushedTransition,
        text: String,
        failure: String?,
        now: Long = System.currentTimeMillis(),
    ) {
        if (!allowed(context)) return
        val alert = alert(context, transition, now, reply = Reply(text, failure))
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context)
            .notify(
                PushedTransition.encode(transition.key),
                ALERT_ID,
                alert.setSilent(true).build(),
            )
    }

    /** The user's inline reply, and why it wasn't sent, if it wasn't. */
    private data class Reply(val text: String, val failure: String?)

    /**
     * `transition`'s alert. One naming an agent that waits on the user — blocked on them, or ready
     * for review — is a conversation with that agent, answered inline ([ReplyReceiver]). An agent
     * asking has a dialog up, which typed text can't answer, so it gets none.
     */
    private fun alert(
        context: Context,
        transition: PushedTransition,
        now: Long,
        reply: Reply? = null,
    ): NotificationCompat.Builder {
        val tag = PushedTransition.encode(transition.key)
        val channel = Channel.of(transition.kindOf)
        val intent =
            Target(transition.project, transition.scope, transition.agent)
                .into(Intent(context, MainActivity::class.java))
                .setData(Uri.fromParts("pm", tag, null))
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val open =
            PendingIntent.getActivity(
                context,
                0,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        val encoded = PushedTransition.encode(transition)
        val alert =
            NotificationCompat.Builder(context, channel.id)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(transition.where)
                .setContentText(transition.text)
                .setWhen(now)
                .setShowWhen(true)
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setContentIntent(open)
                .setAutoCancel(true)
                .setGroup(GROUP)
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_SUMMARY)
                .addExtras(Bundle().apply { putString(EXTRA_TRANSITION, encoded) })
        val agent = transition.agent
        val waiting =
            transition.kindOf == AttentionKind.Blocked || transition.kindOf == AttentionKind.Ready
        if (agent == null || !waiting) return alert
        val them = Person.Builder().setName(agent).setKey(agent).build()
        val you = Person.Builder().setName(context.getString(R.string.reply_you)).build()
        val style =
            NotificationCompat.MessagingStyle(you)
                .setConversationTitle(transition.where)
                .setGroupConversation(false)
                .addMessage(transition.text, now, them)
        if (reply != null) {
            val line =
                reply.failure?.let { context.getString(R.string.reply_not_sent, it, reply.text) }
                    ?: reply.text
            style.addMessage(line, now, null as Person?)
        }
        alert.setStyle(style)
        if (reply != null && reply.failure == null) return alert
        val remote =
            RemoteInput.Builder(ReplyReceiver.KEY_TEXT)
                .setLabel(context.getString(R.string.reply_label, agent))
                .build()
        val send =
            PendingIntent.getBroadcast(
                context,
                tag.hashCode(),
                Intent(context, ReplyReceiver::class.java)
                    .setData(Uri.fromParts("pm-reply", tag, null))
                    .putExtra(EXTRA_TRANSITION, encoded),
                // Mutable, the only one: RemoteInput writes the reply into it.
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
            )
        return alert.addAction(
            NotificationCompat.Action.Builder(
                    R.drawable.ic_send,
                    context.getString(R.string.reply_action),
                    send,
                )
                .addRemoteInput(remote)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
                .setShowsUserInterface(false)
                .build()
        )
    }

    /** The transition a reply intent answers. */
    fun replyingTo(intent: Intent): PushedTransition? =
        intent.getStringExtra(EXTRA_TRANSITION)?.let(PushedTransition::parse)

    /** Offer `update`: tapped, the browser downloads its APK, which Android installs over pm. */
    fun update(context: Context, update: Update) {
        if (!allowed(context)) return
        val open =
            PendingIntent.getActivity(
                context,
                0,
                download(update),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        val notification =
            NotificationCompat.Builder(context, Channel.AppUpdate.id)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(context.getString(R.string.update_title, update.version))
                .setContentText(context.getString(R.string.update_text))
                .setContentIntent(open)
                .setAutoCancel(true)
                .build()
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(UPDATE_ID, notification)
    }

    /** What opens `update`'s APK in the browser. */
    fun download(update: Update): Intent =
        Intent(Intent.ACTION_VIEW, update.apk.toUri()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    /** Withdraw each alert `snapshot` shows is over, and bring the summary up to date. */
    fun reconcile(context: Context, snapshot: Snapshot) {
        val alerts = alerts(context)
        val over = alerts.filter { (_, transition) -> !transition.holds(snapshot) }
        val manager = NotificationManagerCompat.from(context)
        over.forEach { (sbn, _) -> manager.cancel(sbn.tag, ALERT_ID) }
        val left = newestFirst(alerts - over.toSet())
        val summary = active(context).find { it.id == SUMMARY_ID && it.tag == null } ?: return
        when {
            left.isEmpty() -> manager.cancel(SUMMARY_ID)
            // A dismissed alert leaves the summary counting it.
            left.size != summary.notification.number ->
                summarize(context, summary.notification.channelId, left, silent = true)
        }
    }

    /** The group's summary, led by the newest alert: what the group's alert shows. */
    private fun summarize(
        context: Context,
        channel: String,
        alerts: List<PushedTransition>,
        silent: Boolean,
    ) {
        val newest = alerts.first()
        val style = NotificationCompat.InboxStyle()
        alerts.forEach { style.addLine("${it.where}: ${it.text}") }
        val summary =
            NotificationCompat.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(newest.where)
                .setContentText(newest.text)
                .setStyle(style)
                .setNumber(alerts.size)
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setAutoCancel(true)
                .setGroup(GROUP)
                .setGroupSummary(true)
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_SUMMARY)
                .setSilent(silent)
                .build()
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(SUMMARY_ID, summary)
    }

    /** The alerts showing, with what each announced. */
    private fun alerts(context: Context): List<Pair<StatusBarNotification, PushedTransition>> =
        active(context).mapNotNull { sbn ->
            if (sbn.id != ALERT_ID) return@mapNotNull null
            val encoded =
                sbn.notification.extras.getString(EXTRA_TRANSITION) ?: return@mapNotNull null
            PushedTransition.parse(encoded)?.let { sbn to it }
        }

    private fun newestFirst(
        alerts: List<Pair<StatusBarNotification, PushedTransition>>
    ): List<PushedTransition> =
        alerts.sortedByDescending { (sbn, _) -> sbn.notification.`when` }.map { it.second }

    private fun active(context: Context): List<StatusBarNotification> =
        context.getSystemService(NotificationManager::class.java).activeNotifications.toList()

    /**
     * Whether Android lets pm post: the app's notification toggle, which POST_NOTIFICATIONS drives
     * on 13 and later.
     */
    fun allowed(context: Context): Boolean =
        NotificationManagerCompat.from(context).areNotificationsEnabled()

    /** Android's notification settings for pm, where a denied permission is granted. */
    fun settings(context: Context): Intent =
        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)

    /**
     * Subscribe through a distributor against the server's `vapid` key: the user's default one,
     * else an installed one (ntfy), else the google build's embedded one, Google's push service,
     * which needs Google Play services. With none, nothing is registered and [PollWorker] notifies
     * instead. Registration is asynchronous; the endpoint arrives at [PushService.onNewEndpoint].
     */
    fun subscribe(activity: Activity, vapid: String) {
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { found ->
            if (!found) {
                val installed = UnifiedPush.getDistributors(activity)
                val chosen =
                    installed.firstOrNull { it != activity.packageName }
                        ?: installed.firstOrNull()
                        ?: return@tryUseCurrentOrDefaultDistributor
                UnifiedPush.saveDistributor(activity, chosen)
            }
            UnifiedPush.register(activity, messageForDistributor = "pm", vapid = vapid)
        }
    }

    /** Subscribe through `distributor` (a package name) instead. */
    fun use(context: Context, distributor: String, vapid: String) {
        UnifiedPush.saveDistributor(context, distributor)
        UnifiedPush.register(context, messageForDistributor = "pm", vapid = vapid)
    }

    fun distributors(context: Context): List<String> = UnifiedPush.getDistributors(context)

    fun current(context: Context): String? = UnifiedPush.getAckDistributor(context)

    fun unsubscribe(context: Context) {
        UnifiedPush.unregister(context)
    }
}
