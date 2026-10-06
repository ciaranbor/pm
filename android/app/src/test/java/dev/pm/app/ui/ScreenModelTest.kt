package dev.pm.app.ui

import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.eventually
import dev.pm.app.model.Pairing
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
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
import org.junit.Assert.assertFalse
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ScreenModelTest {
    private val server = MockWebServer()
    private lateinit var model: ScreenModel

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        model = ScreenModel(client, "app", "login", "implementer")
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

    @Test
    fun a_key_press_is_sent_then_the_screen_read_again() = modelTest {
        reply(200, "{}")
        reply(200, "Trust this folder?\n  1. Yes\n❯ 2. No")

        model.press("Down")

        eventually { model.screen.value == "Trust this folder?\n  1. Yes\n❯ 2. No" }
        val pressed = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/keys", pressed.url.encodedPath)
        assertEquals("""{"keys":["Down"]}""", pressed.body?.utf8())
        assertEquals(
            "/v1/agents/app/login/implementer/screen",
            server.takeRequest().url.encodedPath,
        )
    }

    @Test
    fun text_a_server_cannot_type_asks_for_an_upgrade() = modelTest {
        reply(404, """{"error":"no such endpoint"}""")
        reply(200, "Paste code here if prompted >")

        var typed = true
        val job = model.viewModelScope.launch { typed = model.type("4821") }

        eventually { job.isCompleted }
        assertFalse(typed)
        assertEquals(
            "pm on the server is older than this app; update it to type here.",
            model.notice.value,
        )
        assertEquals("Paste code here if prompted >", model.screen.value)
    }
}
