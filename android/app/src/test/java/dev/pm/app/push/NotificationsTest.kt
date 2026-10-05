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
import org.junit.Assert.assertNotNull
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
                "app-update" to NotificationManager.IMPORTANCE_DEFAULT,
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
        assertEquals("app/login: implementer", blocked.extras.getString(Notification.EXTRA_TITLE))
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

    private fun replyAction(notification: Notification) =
        NotificationCompat.getActionCount(notification).let { count ->
            (0 until count)
                .map { NotificationCompat.getAction(notification, it)!! }
                .singleOrNull { it.remoteInputs?.isNotEmpty() == true }
        }

    @Test
    fun an_agent_waiting_on_the_user_is_answered_inline_but_a_dialog_is_not() {
        Notifications.createChannels(context)
        Notifications.show(
            context,
            PushedTransition("app", "login", "blocked", "implementer"),
            now = 1,
        )
        Notifications.show(context, PushedTransition("app", "search", "asking", "qa"), now = 2)
        Notifications.show(context, PushedTransition("app", "auth", "ready"), now = 3)

        val byScope =
            alerts().associate { PushedTransition.parse(it.tag)!!.where to it.notification }
        val blocked = byScope.getValue("app/login")
        val reply = replyAction(blocked)!!
        assertEquals(ReplyReceiver.KEY_TEXT, reply.remoteInputs!!.single().resultKey)
        assertEquals(NotificationCompat.Action.SEMANTIC_ACTION_REPLY, reply.semanticAction)
        val style =
            NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(blocked)!!
        assertEquals("implementer", style.messages.single().person!!.name)
        assertNull(replyAction(byScope.getValue("app/search")))
        assertNull("a ready scope names no agent", replyAction(byScope.getValue("app/auth")))
    }

    @Test
    fun a_reply_that_failed_shows_its_text_and_can_be_sent_again() {
        Notifications.createChannels(context)
        val blocked = PushedTransition("app", "login", "blocked", "implementer")
        Notifications.show(context, blocked, now = 1)

        Notifications.replied(context, blocked, "use postgres", "tailnet unreachable", now = 2)
        val failed = alerts().single().notification
        val lines =
            NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(failed)!!
                .messages
                .map { it.text.toString() }
        assertEquals(
            listOf("implementer is blocked on you", "Not sent (tailnet unreachable): use postgres"),
            lines,
        )
        assertNotNull(replyAction(failed))

        Notifications.replied(context, blocked, "use postgres", null, now = 3)
        val sent = alerts().single().notification
        assertNull(replyAction(sent))
    }
}
