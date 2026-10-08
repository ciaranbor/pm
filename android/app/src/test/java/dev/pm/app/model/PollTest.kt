package dev.pm.app.model

import dev.pm.app.SNAPSHOT
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PollTest {
    // login: blocked, its implementer asking; search: ready.
    private val snapshot = Snapshot.parse(SNAPSHOT)

    private fun push(kind: String, scope: String = "login", agent: String? = null) =
        PushedTransition("app", scope, kind, agent)

    private fun Snapshot.withFeature(name: String, change: (FeatureSnapshot) -> FeatureSnapshot) =
        copy(features = features.map { if (it.name == name) change(it) else it })

    private fun FeatureSnapshot.showing(kind: String, agent: String? = null) =
        copy(attention = Attention(kind, agent = agent))

    /** login no longer blocked: its implementer's question is its attention. */
    private val asking =
        snapshot.withFeature("login") { it.copy(progress = "wip").showing("asking", "implementer") }

    /** login with nothing going on. */
    private val quiet =
        snapshot.withFeature("login") {
            it.copy(progress = "wip", agents = emptyList()).showing("none")
        }

    @Test
    fun the_first_poll_alerts_only_a_dialog_up_that_is_its_scopes_attention() {
        val (kept, made) = Poll.judge(null, snapshot)
        assertTrue("blocked outranks the question", made.isEmpty())
        assertTrue(Poll.judge(kept, snapshot).second.isEmpty())

        assertEquals(listOf(push("asking", agent = "implementer")), Poll.judge(null, asking).second)
    }

    @Test
    fun a_scope_alerts_its_top_kind_and_an_outranked_one_once_it_is_on_top() {
        val (calm, _) = Poll.judge(null, quiet)

        val (blocked, made) = Poll.judge(calm, snapshot)
        assertEquals(listOf(push("blocked", agent = "implementer")), made)
        assertTrue(Poll.judge(blocked, snapshot).second.isEmpty())

        val (answered, next) = Poll.judge(blocked, asking)
        assertEquals(listOf(push("asking", agent = "implementer")), next)
        assertTrue(Poll.judge(answered, asking).second.isEmpty())

        val (lapsed, none) = Poll.judge(answered, quiet)
        assertTrue(none.isEmpty())
        assertEquals(made, Poll.judge(lapsed, snapshot).second)
    }

    @Test
    fun a_blocked_main_alerts_once_but_not_when_first_seen() {
        fun main(progress: String, kind: String) =
            quiet.copy(
                projects =
                    quiet.projects.map {
                        it.copy(
                            main =
                                it.main!!.copy(
                                    progress = progress,
                                    attention = Attention(kind, agent = "main"),
                                )
                        )
                    }
            )
        val blocked = main("blocked", "blocked")

        val (standing, none) = Poll.judge(null, blocked)
        assertTrue(none.isEmpty())
        assertTrue(Poll.judge(standing, blocked).second.isEmpty())

        val (calm, _) = Poll.judge(null, main("wip", "none"))
        val (alerted, made) = Poll.judge(calm, blocked)
        assertEquals(listOf(push("blocked", scope = "main", agent = "main")), made)
        assertTrue(Poll.judge(alerted, blocked).second.isEmpty())
    }

    @Test
    fun a_ready_feature_whose_agent_asks_alerts_the_question_then_ready_once_answered() {
        val ready =
            quiet.withFeature("login") {
                it.copy(progress = "ready", agents = listOf(AgentSnapshot("implementer", "idle")))
                    .showing("ready")
            }
        val askingToo =
            ready.withFeature("login") {
                it.copy(agents = listOf(AgentSnapshot("implementer", "asking")))
                    .showing("asking", "implementer")
            }
        val (calm, _) = Poll.judge(null, quiet)

        val (asked, made) = Poll.judge(calm, askingToo)
        assertEquals(listOf(push("asking", agent = "implementer")), made)
        assertEquals(listOf(push("ready")), Poll.judge(asked, ready).second)
    }

    @Test
    fun a_ready_feature_alerts_once_no_agent_is_busy_however_quiet_its_team() {
        val reviewer = AgentSnapshot("reviewer", state = "busy")
        // A busy agent holds the attention back: the server shows none.
        val busy =
            snapshot.withFeature("search") {
                it.copy(agents = listOf(reviewer), working = false).showing("none")
            }
        val idle =
            busy.withFeature("search") {
                it.copy(agents = listOf(reviewer.copy(state = "idle"))).showing("ready")
            }

        val (calm, _) = Poll.judge(null, busy.withFeature("search") { it.copy(progress = "wip") })
        val (owed, none) = Poll.judge(calm, busy)
        assertTrue("held back while its agent is busy", none.isEmpty())
        assertEquals(listOf(push("ready", scope = "search")), Poll.judge(owed, idle).second)

        val (standing, _) = Poll.judge(null, busy)
        assertTrue("a standing ready, held back", Poll.judge(standing, idle).second.isEmpty())
    }

    @Test
    fun an_agent_alerts_as_it_dies_but_not_when_first_seen_dead() {
        val dead =
            quiet.withFeature("login") { it.copy(agents = listOf(AgentSnapshot("qa", "dead"))) }
        val (standing, none) = Poll.judge(null, dead)
        assertTrue(none.isEmpty())
        assertTrue(Poll.judge(standing, dead).second.isEmpty())

        val (calm, _) = Poll.judge(null, quiet)
        assertEquals(listOf(push("dead", agent = "qa")), Poll.judge(calm, dead).second)
    }
}
