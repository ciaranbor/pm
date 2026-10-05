package dev.pm.app.model

import dev.pm.app.SNAPSHOT
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PollTest {
    // login: blocked, its implementer asking; search: ready, its team working.
    private val snapshot = Snapshot.parse(SNAPSHOT)

    private fun push(kind: String, scope: String = "login", agent: String? = null) =
        PushedTransition("app", scope, kind, agent)

    private fun Snapshot.withFeature(name: String, change: (FeatureSnapshot) -> FeatureSnapshot) =
        copy(features = features.map { if (it.name == name) change(it) else it })

    @Test
    fun the_first_poll_alerts_only_a_dialog_up_and_takes_the_rest_as_alerted() {
        val (kept, made) = Poll.judge(null, snapshot)
        assertEquals(listOf(push("asking", agent = "implementer")), made)
        assertTrue(Poll.judge(kept, snapshot).second.isEmpty())
    }

    @Test
    fun an_alert_is_made_once_while_its_condition_holds_and_again_once_it_has_lapsed() {
        val quiet =
            snapshot.withFeature("login") { it.copy(progress = "wip", agents = emptyList()) }
        val (calm, _) = Poll.judge(null, quiet)

        val (blocked, made) = Poll.judge(calm, snapshot)
        assertEquals(
            setOf(push("blocked", agent = "implementer"), push("asking", agent = "implementer")),
            made.toSet(),
        )
        assertTrue(Poll.judge(blocked, snapshot).second.isEmpty())

        val (lapsed, none) = Poll.judge(blocked, quiet)
        assertTrue(none.isEmpty())
        assertEquals(made.toSet(), Poll.judge(lapsed, snapshot).second.toSet())
    }

    @Test
    fun a_ready_feature_alerts_once_its_team_is_quiet() {
        val (owed, none) = Poll.judge(Poll.judge(null, snapshot).first, snapshot)
        assertTrue(none.isEmpty())
        val quiet = snapshot.withFeature("search") { it.copy(working = false) }
        assertEquals(listOf(push("ready", scope = "search")), Poll.judge(owed, quiet).second)
    }
}
