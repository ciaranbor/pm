package dev.pm.app.model

import dev.pm.app.SNAPSHOT
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PushTest {
    private fun push(kind: String, scope: String = "login", agent: String? = null) =
        PushedTransition("app", scope, kind, agent)

    private val snapshot = Snapshot.parse(SNAPSHOT)

    @Test
    fun a_push_names_the_agent_and_kind() {
        fun text(json: String) = PushedTransition.parse(json.toByteArray())!!.text
        assertEquals(
            "implementer is blocked on you",
            text("""{"project":"app","scope":"login","kind":"blocked","agent":"implementer"}"""),
        )
        assertEquals(
            "main is asking",
            text("""{"project":"app","scope":"main","kind":"asking","agent":"main"}"""),
        )
        assertEquals(
            "reviewer stopped running",
            text("""{"project":"app","scope":"login","kind":"dead","agent":"reviewer"}"""),
        )
        assertEquals("Ready for review", text("""{"project":"app","scope":"s","kind":"ready"}"""))
        assertEquals(
            "summoning",
            text("""{"project":"app","scope":"login","kind":"summoning","extra":1}"""),
        )
        assertNull(PushedTransition.parse("garbage".toByteArray()))
    }

    @Test
    fun an_end_push_parses_only_as_an_end_and_ends_its_kind_for_any_agent_or_the_one_it_names() {
        val end =
            """{"project":"app","scope":"login","ended":"asking","agent":null}""".toByteArray()
        assertNull(PushedTransition.parse(end))
        assertNull(
            PushedEnd.parse("""{"project":"app","scope":"login","kind":"asking"}""".toByteArray())
        )
        val asking = PushedEnd.parse(end)!!
        assertTrue(asking.ends(push("asking", agent = "a")))
        assertTrue(asking.ends(push("asking", agent = "b")))
        assertFalse(asking.ends(push("blocked")))
        assertFalse(asking.ends(push("asking", scope = "search", agent = "a")))
        val dead = PushedEnd("app", "login", "dead", "a")
        assertTrue(dead.ends(push("dead", agent = "a")))
        assertFalse(dead.ends(push("dead", agent = "b")))
    }

    @Test
    fun a_scope_keeps_one_blocked_or_ready_alert_but_one_per_agent_asking_or_dying() {
        assertEquals(push("blocked", agent = "a").key, push("blocked", agent = "b").key)
        assertEquals(push("ready", agent = "a").key, push("ready").key)
        assertNotEquals(push("asking", agent = "a").key, push("asking", agent = "b").key)
        assertNotEquals(push("dead", agent = "a").key, push("dead", agent = "b").key)
    }

    @Test
    fun a_kind_holds_while_its_condition_does_whatever_outranks_it() {
        // login: progress blocked, implementer asking; search: progress ready.
        assertTrue(push("blocked").holds(snapshot))
        assertTrue(push("asking", agent = "implementer").holds(snapshot))
        assertFalse(push("asking", agent = "reviewer").holds(snapshot))
        assertFalse(push("ready").holds(snapshot))
        assertTrue(push("ready", scope = "search").holds(snapshot))
        assertFalse(push("blocked", scope = "search").holds(snapshot))
        assertFalse(push("dead", agent = "implementer").holds(snapshot))
        val approved =
            snapshot.copy(
                features =
                    snapshot.features.map { it.copy(progress = "wip", lifecycle = "approved") }
            )
        assertTrue(push("ready", scope = "search").holds(approved))
        // main's agent hibernates, a state this app doesn't know: not asking.
        assertFalse(push("asking", scope = "main", agent = "main").holds(snapshot))
    }

    @Test
    fun a_vanished_scope_ends_its_alert_but_an_unreadable_project_keeps_it() {
        assertFalse(push("blocked", scope = "gone").holds(snapshot))
        assertFalse(PushedTransition("other", "login", "blocked").holds(snapshot))
        val skipped =
            snapshot.copy(projects = listOf(ProjectSnapshot("app", skipped = "unreadable")))
        assertTrue(push("blocked", scope = "gone").holds(skipped))
    }
}
