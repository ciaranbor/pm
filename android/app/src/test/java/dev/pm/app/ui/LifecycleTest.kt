package dev.pm.app.ui

import dev.pm.app.api.PmClient
import dev.pm.app.eventually
import dev.pm.app.model.Pairing
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class LifecycleTest {
    private val server = MockWebServer()
    private lateinit var client: PmClient

    @Before
    fun start() {
        server.start()
        client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
    }

    @After
    fun stop() {
        server.close()
    }

    private fun reply(code: Int, body: String) {
        server.enqueue(MockResponse.Builder().code(code).body(body).build())
    }

    @Test
    fun a_merge_waits_for_confirmation_and_a_refusal_keeps_the_servers_words() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val merge = Action.Merge("app", "login")
        val refusal = "feature 'login' has uncommitted changes — commit or stash before merging"
        reply(409, """{"error":"$refusal","refused":"unsafe"}""")

        lifecycle.ask(merge)
        assertEquals(ActionState.Confirming(merge), lifecycle.state.value)
        assertEquals("nothing is sent before confirming", 0, server.requestCount)

        lifecycle.confirm()
        eventually { lifecycle.state.value is ActionState.Failed }
        assertEquals(ActionState.Failed(merge, refusal), lifecycle.state.value)
        assertEquals("/v1/features/app/login/merge", server.takeRequest().url.encodedPath)
    }

    @Test
    fun a_mid_turn_restart_asks_to_restart_anyway_then_forces_it() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val restart = Action.Restart("app", "login", "implementer")
        reply(409, """{"error":"agent 'implementer' is mid-turn","refused":"mid-turn"}""")
        reply(200, """{"restarted":"Restarted agent 'implementer'"}""")
        val finished = backgroundScope.launch { lifecycle.finished.first() }

        lifecycle.ask(restart)
        eventually { lifecycle.state.value is ActionState.Confirming }
        val forced = restart.copy(force = true)
        assertEquals(ActionState.Confirming(forced), lifecycle.state.value)

        lifecycle.confirm()
        eventually { finished.isCompleted }
        assertEquals(ActionState.Idle, lifecycle.state.value)
        assertEquals("""{"force":false}""", server.takeRequest().body?.utf8())
        assertEquals("""{"force":true}""", server.takeRequest().body?.utf8())
    }

    @Test
    fun a_failure_that_is_not_a_refusal_is_told_apart_as_possibly_partway() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val delete = Action.Delete("app", "login")
        reply(500, """{"error":"Git error: cannot remove worktree"}""")

        lifecycle.ask(delete)
        lifecycle.confirm()
        eventually { lifecycle.state.value is ActionState.Failed }
        assertEquals(
            ActionState.Failed(delete, "Git error: cannot remove worktree", Outcome.Broken),
            lifecycle.state.value,
        )
    }
}
