package dev.pm.app.api

import dev.pm.app.model.Pairing
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class PmClientTest {
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
    fun errors_are_told_apart_by_status_and_body() = runBlocking {
        reply(404, """{"error":"no such endpoint"}""")
        reply(404, """{"error":"no such agent"}""")
        reply(401, """{"error":"a paired device's bearer token is required"}""")
        reply(404, """{"error":"the agent has no conversation yet"}""")

        val unsupported = runCatching {
            client.transcript("app", "login", "implementer")
        }
            .exceptionOrNull()
        assertTrue("$unsupported", unsupported is PmError.Unsupported)
        val missing = runCatching { client.transcript("app", "login", "ghost") }.exceptionOrNull()
        assertTrue(
            "$missing",
            missing is PmError.Status && missing.code == 404 && missing.message == "no such agent",
        )
        val revoked = runCatching { client.snapshot() }.exceptionOrNull()
        assertTrue("$revoked", revoked is PmError.Unauthorized)
        val unstarted = runCatching {
            client.transcript("app", "login", "reviewer")
        }
            .exceptionOrNull()
        assertTrue("$unstarted", unstarted is PmError.NoConversation)

        val first = server.takeRequest()
        assertEquals("Bearer tok", first.headers["Authorization"])
        assertEquals("/v1/agents/app/login/implementer/transcript", first.url.encodedPath)
    }

    @Test
    fun a_subscription_is_sent_in_the_web_push_shape() = runBlocking {
        reply(204, "")
        client.registerPush("https://ntfy.sh/upX?up=1", "BKey", "auth")
        val request = server.takeRequest()
        assertEquals("PUT", request.method)
        assertEquals("/v1/push", request.url.encodedPath)
        assertEquals(
            """{"endpoint":"https://ntfy.sh/upX?up=1","keys":{"p256dh":"BKey","auth":"auth"}}""",
            request.body?.utf8(),
        )
    }

    @Test
    fun the_event_stream_yields_named_events_and_asks_to_watch() = runBlocking {
        server.enqueue(
            MockResponse.Builder()
                .addHeader("Content-Type", "text/event-stream")
                .body(
                    "event: snapshot\ndata: {\"version\":1}\n\n: heartbeat\n\nevent: transcript\ndata: {\"items\":[]}\n\n"
                )
                .build()
        )
        val events = client.events(watch = "app/login/implementer", after = "c9").take(2).toList()
        assertEquals(
            listOf(
                ServerEvent("snapshot", "{\"version\":1}"),
                ServerEvent("transcript", "{\"items\":[]}"),
            ),
            events,
        )
        val request = server.takeRequest()
        assertEquals("app/login/implementer", request.url.queryParameter("watch"))
        assertEquals("c9", request.url.queryParameter("after"))
    }
}
