package dev.pm.app.ui

import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.eventually
import dev.pm.app.model.Pairing
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.job
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestResult
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ModeModelTest {
    private val server = MockWebServer()
    private lateinit var model: ModeModel

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        model = ModeModel(client, "app", "login", "implementer")
    }

    @After
    fun tearDown() {
        server.close()
        Dispatchers.resetMain()
    }

    private fun modelTest(body: suspend TestScope.() -> Unit): TestResult = runTest {
        body()
        model.viewModelScope.cancel()
        eventually { model.viewModelScope.coroutineContext.job.isCompleted }
    }

    private fun reply(code: Int, body: String) {
        server.enqueue(MockResponse.Builder().code(code).body(body).build())
    }

    private val modes =
        """[{"id":"default","label":"Manual"},{"id":"plan","label":"Plan"},""" +
            """{"id":"auto","label":"Auto"}]"""

    @Test
    fun a_switch_is_posted_then_the_mode_read_again() = modelTest {
        reply(200, """{"mode":"default","modes":$modes}""")
        model.refresh()
        eventually { model.mode.value?.label == "Manual" }

        reply(200, """{"mode":"plan"}""")
        reply(200, """{"mode":"plan","modes":$modes}""")
        model.switch("plan")

        assertEquals(ModeSwitched.Done("Plan"), model.switched.first())
        assertEquals("plan", model.mode.value?.mode)
        assertNull(model.switching.value)
        server.takeRequest()
        val posted = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/mode", posted.url.encodedPath)
        assertEquals("""{"mode":"plan"}""", posted.body?.utf8())
    }

    @Test
    fun a_refused_switch_says_why_in_the_users_words() = modelTest {
        reply(200, """{"mode":"default","modes":$modes}""")
        model.refresh()
        eventually { model.mode.value != null }

        reply(409, """{"error":"that mode isn't available","refused":"unavailable"}""")
        reply(200, """{"mode":"default","modes":$modes}""")
        model.switch("auto")

        assertEquals(
            ModeSwitched.Refused("Auto isn't available for implementer"),
            model.switched.first(),
        )
    }

    @Test
    fun an_agent_whose_mode_pm_does_not_switch_offers_none() = modelTest {
        reply(200, """{"mode":"default","modes":$modes}""")
        model.refresh()
        eventually { model.mode.value != null }

        reply(404, """{"error":"the agent's harness has no modes to switch"}""")
        model.refresh()

        eventually { model.mode.value == null }
    }

    @Test
    fun a_failed_read_keeps_the_mode_read_before() = modelTest {
        reply(200, """{"mode":"plan","modes":$modes}""")
        model.refresh()
        eventually { model.mode.value?.mode == "plan" }

        reply(500, """{"error":"boom"}""")
        model.refresh()

        eventually {
            server.requestCount == 2 &&
                model.viewModelScope.coroutineContext.job.children.none { it.isActive }
        }
        assertEquals("plan", model.mode.value?.mode)
    }
}
