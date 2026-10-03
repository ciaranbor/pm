package dev.pm.app.push

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import dev.pm.app.MainActivity
import dev.pm.app.R
import dev.pm.app.model.PushedTransition
import org.unifiedpush.android.connector.UnifiedPush

/** Where a notification leads: the scope, and the agent when one is named. */
data class Target(val project: String, val scope: String, val agent: String?) {
    fun into(intent: Intent): Intent = intent
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
    private const val CHANNEL = "attention"

    fun createChannel(context: Context) {
        val channel = NotificationChannel(
            CHANNEL,
            context.getString(R.string.channel_attention),
            NotificationManager.IMPORTANCE_HIGH,
        ).apply { description = context.getString(R.string.channel_attention_description) }
        context.getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
    }

    /** A notification of `transition`; a later one for the same scope replaces it. */
    fun show(context: Context, transition: PushedTransition) {
        if (!allowed(context)) return
        val target = Target(transition.project, transition.scope, transition.agent)
        val intent = target.into(Intent(context, MainActivity::class.java))
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val id = "${transition.project}/${transition.scope}".hashCode()
        val open = PendingIntent.getActivity(
            context,
            id,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(transition.title)
            .setContentText("Open to see the details over your tailnet")
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        @Suppress("MissingPermission")
        NotificationManagerCompat.from(context).notify(id, notification)
    }

    fun allowed(context: Context): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED

    /**
     * Subscribe through a distributor against the server's `vapid` key: the
     * user's default one, else an installed one (ntfy), else the embedded
     * FCM distributor, which needs Google Play services. Registration is
     * asynchronous; the endpoint arrives at [PushService.onNewEndpoint].
     */
    fun subscribe(activity: Activity, vapid: String) {
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { found ->
            if (!found) {
                val installed = UnifiedPush.getDistributors(activity)
                val chosen = installed.firstOrNull { it != activity.packageName } ?: installed.firstOrNull()
                chosen?.let { UnifiedPush.saveDistributor(activity, it) }
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
