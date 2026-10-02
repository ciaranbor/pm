package dev.pm.app.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

class SnapshotTest {
    /** README's example, with a field, kind, state and version detail this app doesn't know. */
    private val json = """
        {
          "version": 1,
          "projects": [{
            "name": "app", "root": "/src/app", "skipped": null,
            "main": {
              "session": "app/main", "session_exists": true,
              "agents": [{"name": "main", "state": "hibernating", "unread": 0, "window": "app/main:1", "waiting": null}],
              "attention": {"kind": "summoning", "detail": "main: ?", "agent": "main"},
              "working": false, "last_activity": null, "colour": "teal"
            }
          }],
          "features": [{
            "project": "app", "name": "login",
            "attention": {"kind": "blocked", "detail": "which DB?", "agent": "implementer"},
            "progress": "blocked", "blocked_reason": "which DB?", "blocked_by": "implementer",
            "summary": null, "lifecycle": "wip", "pr": null,
            "session": "app/login", "session_exists": true,
            "agents": [{"name": "implementer", "state": "asking", "unread": 2, "window": "app/login:1",
                        "waiting": {"kind": "question", "detail": "Postgres or SQLite?"}}],
            "working": false, "last_activity": "2026-10-02T09:30:00Z"
          }, {
            "project": "app", "name": "search",
            "attention": {"kind": "ready", "detail": "Adds search", "agent": null},
            "progress": "ready", "lifecycle": "review", "session": "app/search", "session_exists": false,
            "agents": [], "working": true, "last_activity": null
          }]
        }
    """.trimIndent()

    @Test
    fun parses_the_snapshot_and_reads_values_it_does_not_know_as_unknown() {
        val snapshot = Snapshot.parse(json)
        assertTrue(snapshot.understood)
        val login = snapshot.feature("app", "login")!!
        assertEquals(AttentionKind.Blocked, login.attention.kindOf)
        assertEquals("implementer", login.blockedBy)
        assertEquals(AgentState.Asking, login.agents.single().stateOf)
        assertEquals("Postgres or SQLite?", login.agents.single().waiting?.detail)
        assertEquals(2, login.agents.single().unread)

        val main = snapshot.project("app")!!.main!!
        assertEquals(AttentionKind.Unknown, main.attention.kindOf)
        assertEquals(AgentState.Unknown, main.agents.single().stateOf)
        assertEquals(listOf("main"), snapshot.agents("app", Snapshot.MAIN).map { it.name })
        assertEquals(listOf("implementer"), snapshot.agents("app", "login").map { it.name })
    }

    @Test
    fun counts_each_projects_attention_most_urgent_first() {
        val counts = Snapshot.parse(json).attentionCounts("app")
        assertEquals(
            listOf(AttentionKind.Blocked to 1, AttentionKind.Ready to 1, AttentionKind.Unknown to 1),
            counts,
        )
    }

    @Test
    fun a_newer_version_is_flagged() {
        assertFalse(Snapshot.parse("""{"version": 2, "projects": [], "features": []}""").understood)
    }
}

class MarksTest {
    @Test
    fun an_attention_kind_that_means_an_agent_state_is_drawn_as_that_state() {
        assertEquals(Marks.agent(AgentState.Asking), Marks.attention(AttentionKind.Asking))
        assertEquals(Marks.agent(AgentState.Dead), Marks.attention(AttentionKind.Dead))
        assertEquals(Marks.agent(AgentState.Unarmed), Marks.attention(AttentionKind.Unarmed))
    }
}

class ActivityTest {
    private val now = Instant.parse("2026-10-02T12:00:00Z")

    @Test
    fun working_wins_and_quiet_shows_only_after_ten_minutes() {
        assertEquals(Activity.Working, activity(true, "2026-10-01T00:00:00Z", now))
        assertNull(activity(false, "2026-10-02T11:51:00Z", now))
        assertEquals(Activity.Quiet("10m"), activity(false, "2026-10-02T11:50:00Z", now))
        assertEquals(Activity.Quiet("3h"), activity(false, "2026-10-02T08:59:00Z", now))
        assertEquals(Activity.Quiet("2d"), activity(false, "2026-09-30T11:00:00Z", now))
        assertNull(activity(false, null, now))
        assertNull(activity(false, "yesterday", now))
    }
}
