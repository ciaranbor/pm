package dev.pm.app.push

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.provider.Settings
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.net.toUri
import dev.pm.app.R
import dev.pm.app.model.Alert
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.PushedEnd
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot
import dev.pm.app.update.Update
import org.unifiedpush.android.connector.UnifiedPush

/**
 * Where a notification leads: the scope, and the agent when one is named; `ready` when it says the
 * feature is ready for review.
 */
data class Target(
    val project: String,
    val scope: String,
    val agent: String?,
    val ready: Boolean = false,
) {
    fun into(intent: Intent): Intent =
        intent
            .putExtra(EXTRA_PROJECT, project)
            .putExtra(EXTRA_SCOPE, scope)
            .putExtra(EXTRA_AGENT, agent)
            .putExtra(EXTRA_READY, ready)

    companion object {
        private const val EXTRA_PROJECT = "dev.pm.app.project"
        private const val EXTRA_SCOPE = "dev.pm.app.scope"
        private const val EXTRA_AGENT = "dev.pm.app.agent"
        private const val EXTRA_READY = "dev.pm.app.ready"

        fun from(intent: Intent?): Target? {
            val project = intent?.getStringExtra(EXTRA_PROJECT) ?: return null
            val scope = intent.getStringExtra(EXTRA_SCOPE) ?: return null
            return Target(
                project,
                scope,
                intent.getStringExtra(EXTRA_AGENT),
                intent.getBooleanExtra(EXTRA_READY, false),
            )
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
    /** The alerts' group, suffixed per project. */
    private const val GROUP = "dev.pm.app.attention"
    /** Each project's summary's id; its tag is the project. */
    private const val SUMMARY_ID = 0
    /** Every alert's id; its tag, the encoded [PushedTransition.key], tells them apart. */
    private const val ALERT_ID = 1
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
     * Alert `alert`, replacing one with the same [PushedTransition.key], under its project's
     * summary, which alerts for the group on `alert`'s channel unless `silent`.
     */
    fun show(
        context: Context,
        alert: Alert,
        now: Long = System.currentTimeMillis(),
        silent: Boolean = false,
    ) {
        if (!allowed(context)) return
        val transition = alert.transition
        val tag = PushedTransition.encode(transition.key)
        val channel = Channel.of(transition.kindOf)
        val built =
            AlertNotification.build(context, alert, channel.id, group(transition.project), now)
                .setSilent(silent)
                .build()
        // What's showing is read before posting: a post reaches the active list asynchronously.
        val others =
            newestFirst(
                alerts(context).filter { (sbn, shown) ->
                    sbn.tag != tag && shown.transition.project == transition.project
                }
            )
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(tag, ALERT_ID, built)
        summarize(context, transition.project, channel.id, listOf(alert) + others, silent)
    }

    /** Alert `transition` as its push alone tells it. */
    fun show(
        context: Context,
        transition: PushedTransition,
        now: Long = System.currentTimeMillis(),
    ) = show(context, Alert.bare(transition), now)

    /**
     * Show `alert`, the detailed form of one showing as its push alone told it, silently; not if
     * that one is gone, or the user has acted on it.
     */
    fun detailed(context: Context, alert: Alert) {
        val tag = PushedTransition.encode(alert.transition.key)
        val (sbn, shown) = alerts(context).find { (sbn, _) -> sbn.tag == tag } ?: return
        if (shown != Alert.bare(alert.transition)) return
        show(context, alert, sbn.notification.`when`, silent = true)
    }

    /**
     * Show what came of the user acting on an alert, silently, as `alert` now tells it; not if the
     * alert was withdrawn meanwhile, its need over.
     */
    fun acted(context: Context, alert: Alert, now: Long = System.currentTimeMillis()) =
        synchronized(this) {
            val tag = PushedTransition.encode(alert.transition.key)
            if (alerts(context).any { (sbn, _) -> sbn.tag == tag }) {
                show(context, alert, now, silent = true)
            }
        }

    private fun group(project: String) = "$GROUP/$project"

    /**
     * Offer `update`: tapped, the browser downloads its APK, which Android installs over pm.
     * Whether it was posted: not while pm's notifications are off.
     */
    fun update(context: Context, update: Update): Boolean {
        if (!allowed(context)) return false
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
        return true
    }

    /** What opens `update`'s APK in the browser. */
    fun download(update: Update): Intent =
        Intent(Intent.ACTION_VIEW, update.apk.toUri()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    /** Withdraw the alerts whose need `end` says is over. */
    fun withdraw(context: Context, end: PushedEnd) =
        synchronized(this) {
            val alerts = alerts(context)
            withdraw(context, alerts, alerts.filter { (_, alert) -> end.ends(alert.transition) })
        }

    /**
     * Withdraw each alert `snapshot` shows is over, and bring each project's summary up to date.
     */
    fun reconcile(context: Context, snapshot: Snapshot) =
        synchronized(this) {
            val alerts = alerts(context)
            withdraw(
                context,
                alerts,
                alerts.filterNot { (_, alert) -> alert.transition.holds(snapshot) },
            )
        }

    /**
     * Withdraw `over` of the `alerts` showing, silently, and bring each project's summary up to
     * date.
     */
    private fun withdraw(
        context: Context,
        alerts: List<Pair<StatusBarNotification, Alert>>,
        over: List<Pair<StatusBarNotification, Alert>>,
    ) {
        val manager = NotificationManagerCompat.from(context)
        over.forEach { (sbn, _) ->
            // Android ignores an app cancelling a notification a direct reply keeps up, until the
            // app posts it again.
            if (keptUpByReply(sbn.notification)) {
                @Suppress("MissingPermission") manager.notify(sbn.tag, ALERT_ID, sbn.notification)
            }
            manager.cancel(sbn.tag, ALERT_ID)
        }
        val left = newestFirst(alerts - over.toSet()).groupBy { it.transition.project }
        for (summary in active(context).filter { it.id == SUMMARY_ID && it.tag != null }) {
            val project = left[summary.tag].orEmpty()
            when {
                project.isEmpty() -> manager.cancel(summary.tag, SUMMARY_ID)
                // A dismissed alert leaves the summary counting it.
                project.size != summary.notification.number ->
                    summarize(
                        context,
                        summary.tag,
                        summary.notification.channelId,
                        project,
                        silent = true,
                    )
            }
        }
    }

    /** `project`'s summary, led by its newest alert: what the group's alert shows. */
    private fun summarize(
        context: Context,
        project: String,
        channel: String,
        alerts: List<Alert>,
        silent: Boolean,
    ) {
        val newest = alerts.first()
        val style = NotificationCompat.InboxStyle()
        alerts.forEach { style.addLine("${it.transition.title}: ${said(it)}") }
        val summary =
            NotificationCompat.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(newest.transition.title)
                .setContentText(said(newest))
                .setSubText(project)
                .setStyle(style.setSummaryText(project))
                .setNumber(alerts.size)
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setAutoCancel(true)
                .setGroup(group(project))
                .setGroupSummary(true)
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_SUMMARY)
                .setSilent(silent)
                .build()
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(project, SUMMARY_ID, summary)
    }

    private fun said(alert: Alert): String = alert.lines.lastOrNull()?.text ?: alert.transition.text

    /** The alerts showing, with what each shows. */
    private fun alerts(context: Context): List<Pair<StatusBarNotification, Alert>> =
        active(context).mapNotNull { sbn ->
            if (sbn.id != ALERT_ID) return@mapNotNull null
            val encoded =
                sbn.notification.extras.getString(AlertNotification.EXTRA_ALERT)
                    ?: return@mapNotNull null
            Alert.parse(encoded)?.let { sbn to it }
        }

    /** `Notification.FLAG_LIFETIME_EXTENDED_BY_DIRECT_REPLY`, hidden from the SDK (Android 15). */
    private const val FLAG_KEPT_UP_BY_REPLY = 0x00010000

    private fun keptUpByReply(notification: Notification): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM &&
            notification.flags and FLAG_KEPT_UP_BY_REPLY != 0

    private fun newestFirst(alerts: List<Pair<StatusBarNotification, Alert>>): List<Alert> =
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
