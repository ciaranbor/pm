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
    fun a_features_details_parse_with_their_optional_fields_absent() = runBlocking {
        reply(
            200,
            """{"name":"login","progress":"wip","lifecycle":"wip","branch":"login","base":"main",
               "divergence":{"ahead":3,"behind":1},"workflow":{"name":"review"},"created":"2026-10-05T18:36:31.235464Z","extra":1}""",
        )
        val info = client.feature("app", "login")
        assertEquals("/v1/features/app/login", server.takeRequest().url.encodedPath)
        assertEquals(null, info.context)
        assertEquals(null, info.pr)
        assertEquals(
            listOf(
                "Status" to "wip",
                "Lifecycle" to "wip",
                "Branch" to "login",
                "Remote" to "none",
                "Base" to "main",
                "Divergence" to "3 ahead, 1 behind main",
                "Workflow" to "review",
                "Created" to "2026-10-05 18:36:31 UTC",
            ),
            info.rows,
        )
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

    @Test
    fun input_is_posted_and_its_refusals_told_apart() = runBlocking {
        reply(200, """{"delivery":"sent","confirmed":true}""")
        reply(200, "{}")
        reply(409, """{"error":"the agent's input line is not empty","refused":"not-at-prompt"}""")

        assertEquals(
            Delivered("sent", confirmed = true),
            client.sendText("app", "login", "implementer", "hi"),
        )
        client.interrupt("app", "login", "implementer")
        val refused = runCatching {
            client.pressKeys("app", "login", "implementer", listOf("Down", "Enter"))
        }
            .exceptionOrNull()
        assertTrue("$refused", refused is PmError.Refused && refused.code == "not-at-prompt")

        val sent = server.takeRequest()
        assertEquals("POST", sent.method)
        assertEquals("""{"text":"hi"}""", sent.body?.utf8())
        assertEquals(
            "/v1/agents/app/login/implementer/interrupt",
            server.takeRequest().url.encodedPath,
        )
        assertEquals("""{"keys":["Down","Enter"]}""", server.takeRequest().body?.utf8())
    }
}
