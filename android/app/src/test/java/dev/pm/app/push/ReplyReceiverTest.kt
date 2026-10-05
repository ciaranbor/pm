package dev.pm.app.push

import dev.pm.app.api.PmClient
import dev.pm.app.model.Pairing
import dev.pm.app.model.PushedTransition
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReplyReceiverTest {
    private val blocked = PushedTransition("app", "login", "blocked", "implementer")

    @Test
    fun a_reply_reaches_the_agent_the_alert_names_or_says_why_not() = runBlocking {
        val server = MockWebServer()
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        server.enqueue(MockResponse.Builder().body("""{"delivery":"sent"}""").build())

        assertNull(ReplyReceiver.reply(client, blocked, "use postgres"))
        assertEquals(
            "/v1/agents/app/login/implementer/input",
            server.takeRequest().url.encodedPath,
        )

        server.close()
        assertEquals("tailnet unreachable", ReplyReceiver.reply(client, blocked, "use postgres"))
        assertEquals("not paired", ReplyReceiver.reply(null, blocked, "use postgres"))
    }
}
