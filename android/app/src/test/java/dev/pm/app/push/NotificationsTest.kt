package dev.pm.app.push

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class NotificationsTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val manager = context.getSystemService(NotificationManager::class.java)

    private fun showing() = manager.activeNotifications.toList()

    private fun alerts() =
        showing().filter { it.notification.flags and Notification.FLAG_GROUP_SUMMARY == 0 }

    private fun summary() =
        showing().singleOrNull { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 }

    @Test
    fun upgrading_replaces_the_single_channel_with_one_per_kind() {
        manager.createNotificationChannel(
            NotificationChannel("attention", "Needs you", NotificationManager.IMPORTANCE_HIGH)
        )
        Notifications.createChannels(context)

        assertNull(manager.getNotificationChannel("attention"))
        assertEquals(
            mapOf(
                "needs-input" to NotificationManager.IMPORTANCE_HIGH,
                "ready" to NotificationManager.IMPORTANCE_DEFAULT,
                "agent-died" to NotificationManager.IMPORTANCE_HIGH,
            ),
            manager.notificationChannels.associate { it.id to it.importance },
        )
    }

    @Test
    fun alerts_group_under_a_summary_that_alerts_on_the_newest_ones_channel() {
        Notifications.createChannels(context)
        Notifications.show(
            context,
            PushedTransition("app", "login", "blocked", "implementer"),
            now = 1,
        )
        Notifications.show(context, PushedTransition("app", "search", "ready"), now = 2)

        val byChannel = alerts().associate { it.notification.channelId to it.notification }
        assertEquals(setOf("needs-input", "ready"), byChannel.keys)
        val blocked = byChannel.getValue("needs-input")
        assertEquals("app/login", blocked.extras.getString(Notification.EXTRA_TITLE))
        assertEquals(
            "implementer is blocked on you",
            blocked.extras.getCharSequence(Notification.EXTRA_TEXT).toString(),
        )
        assertEquals(1L, blocked.`when`)
        alerts().forEach {
            assertEquals(NotificationCompat.GROUP_ALERT_SUMMARY, it.notification.groupAlertBehavior)
        }

        val summary = summary()!!.notification
        assertEquals("ready", summary.channelId)
        assertEquals(alerts().first().notification.group, summary.group)
        assertEquals("app/search", summary.extras.getString(Notification.EXTRA_TITLE))
        assertEquals(2, summary.number)
    }

    @Test
    fun a_later_push_of_the_same_alert_replaces_it() {
        Notifications.createChannels(context)
        Notifications.show(
            context,
            PushedTransition("app", "login", "blocked", "implementer"),
            now = 1,
        )
        Notifications.show(
            context,
            PushedTransition("app", "login", "blocked", "reviewer"),
            now = 2,
        )
        Notifications.show(context, PushedTransition("app", "login", "asking", "reviewer"), now = 3)

        assertEquals(2, alerts().size)
        assertEquals(
            setOf("reviewer is blocked on you", "reviewer is asking"),
            alerts()
                .map { it.notification.extras.getCharSequence(Notification.EXTRA_TEXT).toString() }
                .toSet(),
        )
    }

    @Test
    fun a_snapshot_withdraws_the_alerts_it_shows_are_over() {
        Notifications.createChannels(context)
        val snapshot = Snapshot.parse(SNAPSHOT)
        Notifications.show(context, PushedTransition("app", "login", "blocked"), now = 1)
        Notifications.show(context, PushedTransition("app", "login", "ready"), now = 2)

        Notifications.reconcile(context, snapshot)
        assertEquals(listOf("needs-input"), alerts().map { it.notification.channelId })
        assertEquals(1, summary()!!.notification.number)
        assertEquals("ready", summary()!!.notification.channelId)

        Notifications.reconcile(context, snapshot.copy(features = emptyList()))
        assertEquals(emptyList<Any>(), showing())
    }

    @Test
    fun a_snapshot_brings_the_summary_up_to_date_after_a_dismissal() {
        Notifications.createChannels(context)
        val snapshot = Snapshot.parse(SNAPSHOT)
        val blocked = PushedTransition("app", "login", "blocked")
        Notifications.show(context, blocked, now = 1)
        Notifications.show(context, PushedTransition("app", "search", "ready"), now = 2)
        manager.cancel(PushedTransition.encode(blocked.key), alerts().first().id)

        Notifications.reconcile(context, snapshot)
        assertEquals(1, summary()!!.notification.number)
    }
}
