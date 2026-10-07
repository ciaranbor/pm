package dev.pm.app.push

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.model.Alert
import dev.pm.app.model.Dialog
import dev.pm.app.model.PushedEnd
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
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

    private fun summaries() =
        showing().filter { it.notification.flags and Notification.FLAG_GROUP_SUMMARY != 0 }

    private fun Notification.text() = extras.getCharSequence(Notification.EXTRA_TEXT).toString()

    private fun Notification.lines() =
        NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(this)!!
            .messages
            .map { it.text.toString() }

    private fun Notification.conversationTitle() =
        NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(this)!!
            .conversationTitle
            .toString()

    private fun Notification.actionLabels() =
        (0 until NotificationCompat.getActionCount(this)).map {
            NotificationCompat.getAction(this, it)!!.title.toString()
        }

    private fun Notification.action(label: String) =
        (0 until NotificationCompat.getActionCount(this))
            .map { NotificationCompat.getAction(this, it)!! }
            .single { it.title.toString() == label }

    private fun byScope() =
        alerts().associate { PushedTransition.parse(it.tag)!!.where to it.notification }

    private val blocked = PushedTransition("app", "login", "blocked", "implementer")

    private val permission =
        Dialog(
            "d1",
            "permission",
            tool = "Bash",
            detail = "npm install stripe@^17",
            choices = listOf(Dialog.Choice("yes", "Yes"), Dialog.Choice("no", "No")),
        )

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
    fun alerts_are_titled_by_feature_and_grouped_per_project_under_a_summary_that_alerts() {
        Notifications.createChannels(context)
        Notifications.show(context, blocked, now = 1)
        Notifications.show(context, PushedTransition("app", "search", "ready"), now = 2)
        Notifications.show(context, PushedTransition("web", "main", "asking", "main"), now = 3)

        val login = byScope().getValue("app/login")
        assertEquals("login", login.conversationTitle())
        assertEquals("app", login.extras.getString(Notification.EXTRA_SUB_TEXT))
        assertEquals("needs-input", login.channelId)
        assertEquals(1L, login.`when`)
        assertEquals("web", byScope().getValue("web/main").conversationTitle())
        alerts().forEach {
            assertEquals(NotificationCompat.GROUP_ALERT_SUMMARY, it.notification.groupAlertBehavior)
        }

        val summaries = summaries().associateBy { it.tag }
        assertEquals(setOf("app", "web"), summaries.keys)
        val app = summaries.getValue("app").notification
        assertEquals("ready", app.channelId)
        assertEquals(login.group, app.group)
        assertEquals("search", app.extras.getString(Notification.EXTRA_TITLE))
        assertEquals(2, app.number)
        assertEquals(1, summaries.getValue("web").notification.number)
    }

    @Test
    fun a_later_push_of_the_same_alert_replaces_it() {
        Notifications.createChannels(context)
        Notifications.show(context, blocked, now = 1)
        Notifications.show(context, blocked.copy(agent = "reviewer"), now = 2)
        Notifications.show(context, PushedTransition("app", "login", "asking", "reviewer"), now = 3)

        assertEquals(2, alerts().size)
        assertEquals(
            setOf(listOf("Blocked on you"), listOf("Waiting for your answer")),
            alerts().map { it.notification.lines() }.toSet(),
        )
    }

    @Test
    fun a_snapshot_withdraws_the_alerts_it_shows_are_over_answered_or_not() {
        Notifications.createChannels(context)
        val snapshot = Snapshot.parse(SNAPSHOT)
        Notifications.show(context, PushedTransition("app", "login", "blocked"), now = 1)
        Notifications.show(context, PushedTransition("app", "login", "ready"), now = 2)
        val answered = Alert.bare(PushedTransition("web", "x", "blocked", "a"))
        Notifications.show(context, answered, now = 3)
        Notifications.acted(context, answered.replied("ok"))

        Notifications.reconcile(context, snapshot)
        assertEquals(setOf("app/login"), byScope().keys)
        assertEquals(1, summaries().single { it.tag == "app" }.notification.number)
        assertEquals(listOf("app"), summaries().map { it.tag })

        Notifications.reconcile(context, snapshot.copy(features = emptyList()))
        assertTrue(showing().isEmpty())
    }

    @Test
    fun an_end_push_withdraws_the_alerts_whose_need_it_ends_answered_or_not() {
        Notifications.createChannels(context)
        val asking = Alert.bare(PushedTransition("app", "login", "asking", "implementer"))
        Notifications.show(context, asking, now = 1)
        Notifications.show(context, PushedTransition("app", "login", "asking", "reviewer"), now = 2)
        Notifications.show(context, PushedTransition("app", "login", "dead", "reviewer"), now = 3)
        val answered = Alert.bare(blocked)
        Notifications.show(context, answered, now = 4)
        Notifications.acted(context, answered.replied("use postgres"), now = 5)
        Notifications.show(context, PushedTransition("app", "search", "ready"), now = 6)

        Notifications.withdraw(context, PushedEnd("app", "login", "asking"))
        fun kinds() =
            alerts().map { PushedTransition.parse(it.tag)!!.let { "${it.scope} ${it.kind}" } }
        assertEquals(setOf("login dead", "login blocked", "search ready"), kinds().toSet())
        assertEquals(3, summaries().single().notification.number)

        Notifications.acted(context, asking.answered("Allowed", emptyList()), now = 7)
        assertEquals(
            "an action's outcome doesn't bring back what was withdrawn",
            setOf("login dead", "login blocked", "search ready"),
            kinds().toSet(),
        )

        Notifications.withdraw(context, PushedEnd("app", "login", "blocked"))
        Notifications.withdraw(context, PushedEnd("app", "login", "dead", "reviewer"))
        Notifications.withdraw(context, PushedEnd("app", "search", "ready"))
        assertTrue(showing().isEmpty())
    }

    @Test
    fun a_snapshot_brings_the_summary_up_to_date_after_a_dismissal() {
        Notifications.createChannels(context)
        Notifications.show(context, PushedTransition("app", "login", "blocked"), now = 1)
        Notifications.show(context, PushedTransition("app", "search", "ready"), now = 2)
        manager.cancel(PushedTransition.encode(blocked.key), alerts().first().id)

        Notifications.reconcile(context, Snapshot.parse(SNAPSHOT))
        assertEquals(1, summaries().single().notification.number)
    }

    @Test
    fun every_alert_opens_its_agent_and_only_a_permission_prompt_is_allowed_or_denied() {
        Notifications.createChannels(context)
        val asking = PushedTransition("app", "search", "asking", "qa")
        Notifications.show(context, Alert.of(asking, null, listOf(permission)), now = 1)
        Notifications.show(
            context,
            Alert.of(asking.copy(scope = "auth"), null, listOf(permission.copy(kind = "plan"))),
            now = 2,
        )
        Notifications.show(context, blocked, now = 3)
        Notifications.show(context, PushedTransition("app", "docs", "ready"), now = 4)

        val alerts = byScope()
        val prompt = alerts.getValue("app/search")
        assertEquals(listOf("Allow Bash? npm install stripe@^17"), prompt.lines())
        assertEquals(listOf("Allow", "Deny", "Open"), prompt.actionLabels())
        assertTrue(prompt.action("Allow").isAuthenticationRequired)
        assertTrue(prompt.action("Deny").isAuthenticationRequired)
        assertEquals(listOf("Open"), alerts.getValue("app/auth").actionLabels())
        assertEquals(listOf("Open"), alerts.getValue("app/docs").actionLabels())

        val waiting = alerts.getValue("app/login")
        assertEquals(listOf("Reply", "Open"), waiting.actionLabels())
        val reply = waiting.action("Reply")
        assertEquals(ReplyReceiver.KEY_TEXT, reply.remoteInputs!!.single().resultKey)
        assertEquals(NotificationCompat.Action.SEMANTIC_ACTION_REPLY, reply.semanticAction)
        assertTrue(reply.isAuthenticationRequired)
        val style =
            NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(waiting)!!
        assertEquals("implementer", style.messages.single().person!!.name)
        assertEquals("login", style.conversationTitle)
    }

    @Test
    fun a_reply_shows_in_the_alert_with_the_reply_action_again_only_if_it_failed() {
        Notifications.createChannels(context)
        val alert = Alert.bare(blocked)
        Notifications.show(context, alert, now = 1)

        Notifications.acted(context, alert.failed("use postgres", "tailnet unreachable"), now = 2)
        val failed = alerts().single().notification
        assertEquals(
            listOf("Blocked on you", "Not sent (tailnet unreachable): use postgres"),
            failed.lines(),
        )
        assertEquals(listOf("Reply", "Open"), failed.actionLabels())

        Notifications.acted(context, alert.replied("use postgres"), now = 3)
        val sent = alerts().single().notification
        assertEquals(listOf("Blocked on you", "use postgres"), sent.lines())
        assertEquals(listOf("Open"), sent.actionLabels())
    }

    @Test
    fun the_servers_words_replace_the_pushs_until_the_user_acts_on_the_alert() {
        Notifications.createChannels(context)
        val asking = PushedTransition("app", "search", "asking", "qa")
        val detailed = Alert.of(asking, null, listOf(permission))
        Notifications.show(context, asking, now = 1)

        Notifications.detailed(context, detailed)
        val shown = alerts().single().notification
        assertEquals(listOf("Allow Bash? npm install stripe@^17"), shown.lines())
        assertEquals("the alert keeps its time", 1L, shown.`when`)

        Notifications.acted(context, detailed.answered("Allowed", emptyList()), now = 2)
        Notifications.detailed(context, detailed)
        assertEquals(
            listOf("Allow Bash? npm install stripe@^17", "Allowed"),
            alerts().single().notification.lines(),
        )

        Notifications.reconcile(context, Snapshot.parse(SNAPSHOT))
        assertTrue("an answered alert goes once its need is over", showing().isEmpty())
    }
}
