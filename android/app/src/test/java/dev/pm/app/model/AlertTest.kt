package dev.pm.app.model

import dev.pm.app.SNAPSHOT
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AlertTest {
    private val snapshot = Snapshot.parse(SNAPSHOT)

    private fun push(kind: String, scope: String = "login", agent: String? = "implementer") =
        PushedTransition("app", scope, kind, agent)

    private fun said(alert: Alert) = alert.lines.map { it.text }

    @Test
    fun an_asking_alert_says_what_the_oldest_open_dialog_asks() {
        val permission =
            Dialog(
                "d1",
                "permission",
                tool = "Bash",
                detail = "npm install stripe@^17",
                choices = listOf(Dialog.Choice("yes", "Yes"), Dialog.Choice("no", "No")),
            )
        val question =
            Dialog(
                "d2",
                "question",
                questions = listOf(Dialog.Question("Postgres or SQLite?"), Dialog.Question("ORM?")),
            )
        val plan = Dialog("d3", "plan", detail = "Add search", choices = permission.choices)

        val asking = Alert.of(push("asking"), snapshot, listOf(permission, question))
        assertEquals(listOf("Allow Bash? npm install stripe@^17 (+1 waiting)"), said(asking))
        assertEquals(
            "only a permission prompt is answered from the alert",
            permission,
            asking.prompt,
        )

        val questioned = Alert.of(push("asking"), snapshot, listOf(question))
        assertEquals(listOf("Postgres or SQLite? (+1 more)"), said(questioned))
        assertNull(questioned.prompt)
        assertNull(Alert.of(push("asking"), snapshot, listOf(plan)).prompt)
        assertEquals(
            listOf("Approve the plan? Add search"),
            said(Alert.of(push("asking"), snapshot, listOf(plan))),
        )
    }

    @Test
    fun without_its_dialogs_an_alert_says_what_the_snapshot_does_or_what_the_push_did() {
        assertEquals(
            "the agent's state",
            listOf("Postgres or SQLite?"),
            said(Alert.of(push("asking"), snapshot, emptyList())),
        )
        assertEquals(listOf("which DB?"), said(Alert.of(push("blocked"), snapshot, emptyList())))
        assertEquals(
            listOf("Ready for review"),
            said(Alert.of(push("ready", "search", null), snapshot, emptyList())),
        )
        assertEquals(
            listOf("Ready for review: Adds search"),
            said(
                Alert.of(
                    push("ready", "search", null),
                    snapshot.copy(
                        features = snapshot.features.map { it.copy(summary = "Adds search") }
                    ),
                    emptyList(),
                )
            ),
        )
        assertEquals(
            listOf("Waiting for your answer"),
            said(Alert.of(push("asking"), null, emptyList())),
        )
        assertEquals(
            listOf("main stopped running"),
            said(Alert.of(push("dead", "main", "main"), snapshot, emptyList())),
        )
    }

    @Test
    fun a_reply_sent_after_a_failure_replaces_the_failed_line() {
        val blocked = Alert.bare(push("blocked"))
        val sent = blocked.failed("use postgres", "tailnet unreachable").replied("use postgres")
        assertEquals(blocked.lines + Alert.Line("use postgres", Alert.By.You), sent.lines)
    }
}
