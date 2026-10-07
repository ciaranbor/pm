package dev.pm.app.push

import dev.pm.app.api.PmClient
import dev.pm.app.model.Alert
import dev.pm.app.model.Pairing
import dev.pm.app.model.PushedTransition
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ReplyReceiverTest {
    private val blocked = Alert.bare(PushedTransition("app", "login", "blocked", "implementer"))

    @Test
    fun a_reply_reaches_the_agent_the_alert_names_or_says_why_not() = runBlocking {
        val server = MockWebServer()
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        server.enqueue(MockResponse.Builder().body("""{"delivery":"sent"}""").build())

        val sent = ReplyReceiver.reply(client, blocked, "use postgres")
        assertEquals(
            "/v1/agents/app/login/implementer/input",
            server.takeRequest().url.encodedPath,
        )
        assertEquals(Alert.Line("use postgres", Alert.By.You), sent.lines.last())
        assertTrue("a sent reply stays showing", sent.settled)

        server.close()
        val failed = ReplyReceiver.reply(client, blocked, "use postgres")
        assertEquals("pm serve unreachable", failed.lines.last().failure)
        assertEquals(
            "not paired",
            ReplyReceiver.reply(null, blocked, "use postgres").lines.last().failure,
        )
    }
}
