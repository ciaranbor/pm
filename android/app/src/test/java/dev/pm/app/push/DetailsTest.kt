package dev.pm.app.push

import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.model.Alert
import dev.pm.app.model.Pairing
import dev.pm.app.model.PushedTransition
import kotlinx.coroutines.runBlocking
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DetailsTest {
    private val server =
        MockWebServer().apply {
            dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse =
                        MockResponse.Builder().body(SNAPSHOT).build()
                }
            start()
        }
    private val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))

    @After
    fun stop() {
        server.close()
    }

    @Test
    fun an_alert_is_filled_in_only_while_the_server_shows_its_need() = runBlocking {
        // login is blocked; search is ready, not blocked.
        val blocked = PushedTransition("app", "login", "blocked", "implementer")
        assertEquals(
            listOf(Alert.Line("which DB?")),
            Details.of(client, blocked)!!.lines,
        )
        assertNull(
            "a need already over is not brought back",
            Details.of(client, blocked.copy(scope = "search")),
        )
    }
}
