package dev.pm.app.model

import dev.pm.app.SNAPSHOT
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SnapshotTest {
    private val json = SNAPSHOT

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
            listOf(
                AttentionKind.Blocked to 1,
                AttentionKind.Ready to 1,
                AttentionKind.Unknown to 1,
            ),
            counts,
        )
    }

    @Test
    fun needs_rank_every_projects_scopes_by_kind_then_longest_quiet() {
        fun feature(project: String, name: String, kind: String, last: String?) =
            """{"project": "$project", "name": "$name", "attention": {"kind": "$kind"},
                "last_activity": ${last?.let { "\"$it\"" } ?: "null"}}"""
        val snapshot =
            Snapshot.parse(
                """{"version": 1,
                    "projects": [
                      {"name": "a", "main": {"attention": {"kind": "dead", "agent": "main"}}},
                      {"name": "b", "main": {"attention": {"kind": "none"}}}],
                    "features": [
                      ${feature("a", "fresh", "ready", "2026-10-02T11:00:00Z")},
                      ${feature("b", "unknown-age", "ready", null)},
                      ${feature("b", "old", "ready", "2026-10-01T11:00:00Z")},
                      ${feature("b", "idle", "none", null)},
                      ${feature("b", "q", "asking", null)}]}"""
            )
        assertEquals(
            listOf("b/q", "b/old", "a/fresh", "b/unknown-age", "a/main"),
            snapshot.needsYou().map { "${it.project}/${it.scope}" },
        )
        assertEquals(listOf("b", "a"), snapshot.projectsByUrgency().map { it.name })
    }

    @Test
    fun a_need_names_its_agent_only_while_the_scope_still_has_it() {
        val needs = Snapshot.parse(json).needsYou()
        assertEquals("implementer", needs.first { it.scope == "login" }.agent?.name)
        val gone =
            Snapshot.parse(
                    """{"version": 1, "features": [{"project": "a", "name": "f",
                        "attention": {"kind": "blocked", "agent": "qa"}, "agents": []}]}"""
                )
                .needsYou()
                .single()
        assertNull(gone.agent)
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
