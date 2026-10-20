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
    fun an_action_the_server_lacks_asks_to_update_the_server_and_one_it_retired_the_app() =
        runTest {
            val lifecycle = Lifecycle(backgroundScope) { client }
            val merge = Action.Merge("app", "login")
            reply(404, """{"error":"no such endpoint"}""")
            reply(410, """{"error":"this endpoint was retired"}""")

            lifecycle.ask(merge)
            lifecycle.confirm()
            eventually { lifecycle.state.value is ActionState.Failed }
            assertEquals(
                ActionState.Failed(
                    merge,
                    "pm on the server is older than this app; update it to do this here.",
                ),
                lifecycle.state.value,
            )

            lifecycle.ask(merge)
            lifecycle.confirm()
            eventually {
                (lifecycle.state.value as? ActionState.Failed)?.reason?.startsWith("This app") ==
                    true
            }
            assertEquals(
                ActionState.Failed(
                    merge,
                    "This app is older than pm on the server; update the app to do this here.",
                ),
                lifecycle.state.value,
            )
        }

    @Test
    fun a_delete_that_went_through_with_warnings_shows_them() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val delete = Action.Delete("app", "login")
        val warning = "feature 'login' had 1 untracked file(s), deleted with it: notes.txt"
        reply(200, """{"deleted":true,"warnings":["$warning"]}""")
        reply(200, """{"deleted":true}""")
        val finished = backgroundScope.launch { lifecycle.finished.first() }

        lifecycle.ask(delete)
        lifecycle.confirm()
        eventually { lifecycle.state.value is ActionState.Warned }
        assertEquals(ActionState.Warned(delete, listOf(warning)), lifecycle.state.value)
        eventually { finished.isCompleted }

        lifecycle.dismiss()
        lifecycle.ask(delete)
        lifecycle.confirm()
        eventually { server.requestCount == 2 && lifecycle.state.value == ActionState.Idle }
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

    @Test
    fun a_project_opens_at_once_and_its_delete_waits_for_confirmation() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val open = Action.OpenProject("app")
        val delete = Action.DeleteProject("app")
        val skipped = "skipping 'login': worktree missing at /src/app/login"
        val refusal = "Cannot delete project — feature 'login' has unpushed commits"
        reply(200, """{"opened":true,"sessions":1,"agents":0,"warnings":["$skipped"]}""")
        reply(409, """{"error":"$refusal","refused":"unsafe"}""")

        lifecycle.ask(open)
        eventually { lifecycle.state.value is ActionState.Warned }
        assertEquals(ActionState.Warned(open, listOf(skipped)), lifecycle.state.value)
        assertEquals("/v1/projects/app/open", server.takeRequest().url.encodedPath)
        lifecycle.dismiss()

        lifecycle.ask(delete)
        assertEquals(ActionState.Confirming(delete), lifecycle.state.value)
        assertEquals("nothing is sent before confirming", 1, server.requestCount)
        lifecycle.confirm()
        eventually { lifecycle.state.value is ActionState.Failed }
        assertEquals(ActionState.Failed(delete, refusal), lifecycle.state.value)
        assertEquals("/v1/projects/app/delete", server.takeRequest().url.encodedPath)
    }

    @Test
    fun a_spawn_and_a_start_run_at_once_and_an_agent_already_running_says_so() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val spawn = Action.Spawn("app", "login", "second", "reviewer", "look at login")
        val start = Action.Start("app", "login", "implementer")
        reply(200, """{"spawned":"Agent 'second' already active","outcome":"already-active"}""")
        val finished = mutableListOf<Finished>()
        backgroundScope.launch { lifecycle.finished.collect { finished += it } }

        lifecycle.ask(spawn)
        eventually { finished.size == 1 }
        assertEquals(
            Finished(spawn, "second was already running; sent it the context"),
            finished[0],
        )
        val spawned = server.takeRequest()
        assertEquals("/v1/agents/app/login/second/spawn", spawned.url.encodedPath)
        assertEquals(
            """{"definition":"reviewer","context":"look at login"}""",
            spawned.body?.utf8(),
        )

        val restarted = spawn.copy(agent = "implementer", definition = null, existing = true)
        reply(200, """{"spawned":"Spawned agent 'implementer'","outcome":"spawned"}""")
        lifecycle.ask(restarted)
        eventually { finished.size == 2 }
        assertEquals(
            Finished(restarted, "Started implementer; sent it the context"),
            finished[1],
        )
        server.takeRequest()

        reply(200, """{"spawned":"Resumed agent 'implementer'","outcome":"resumed"}""")
        lifecycle.ask(start)
        eventually { finished.size == 3 }
        assertEquals(Finished(start, "Started implementer"), finished[2])
        val started = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/spawn", started.url.encodedPath)
        assertEquals("{}", started.body?.utf8())
    }

    @Test
    fun a_stop_and_an_agents_delete_wait_for_confirmation() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val stop = Action.Stop("app", "login", "implementer", working = true)
        val delete = Action.DeleteAgent("app", "login", "implementer")
        reply(200, """{"stopped":"Stopped agent 'implementer' in login"}""")
        reply(200, """{"deleted":"Deleted agent 'implementer' from login"}""")

        for ((action, path) in listOf(stop to "stop", delete to "delete")) {
            lifecycle.ask(action)
            assertEquals(ActionState.Confirming(action), lifecycle.state.value)
            lifecycle.confirm()
            eventually { lifecycle.state.value == ActionState.Idle }
            assertEquals(
                "/v1/agents/app/login/implementer/$path",
                server.takeRequest().url.encodedPath,
            )
        }
    }

    @Test
    fun a_fork_onto_a_taken_name_fails_in_the_servers_words() = runTest {
        val lifecycle = Lifecycle(backgroundScope) { client }
        val fork = Action.Fork("app", "login", "implementer", "reviewer")
        val refusal = "agent 'reviewer' already exists in scope 'login'"
        reply(409, """{"error":"$refusal","refused":"fork"}""")

        lifecycle.ask(fork)
        eventually { lifecycle.state.value is ActionState.Failed }
        assertEquals(ActionState.Failed(fork, refusal), lifecycle.state.value)
        val sent = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/fork", sent.url.encodedPath)
        assertEquals("""{"name":"reviewer"}""", sent.body?.utf8())
    }
}
