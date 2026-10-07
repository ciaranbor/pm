package dev.pm.app.api

import dev.pm.app.model.DialogAnswer
import dev.pm.app.model.Pairing
import java.time.ZoneId
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
    fun a_server_without_an_endpoint_is_told_apart_from_one_that_retired_it() = runBlocking {
        reply(405, """{"error":"no such endpoint for this method"}""")
        reply(410, """{"error":"this endpoint was retired"}""")
        reply(405, """{"error":"something else"}""")

        val older = runCatching { client.merge("app", "login") }.exceptionOrNull()
        assertTrue("$older", older is PmError.Unsupported && !older.retired)
        val newer = runCatching { client.merge("app", "login") }.exceptionOrNull()
        assertTrue("$newer", newer is PmError.Unsupported && newer.retired)
        val other = runCatching { client.merge("app", "login") }.exceptionOrNull()
        assertTrue("$other", other is PmError.Status && other.code == 405)
    }

    @Test
    fun notes_are_saved_against_the_version_read_and_a_stale_save_returns_the_current_notes() =
        runBlocking {
            server.enqueue(
                MockResponse.Builder()
                    .code(200)
                    .body("# Ideas\n")
                    .addHeader("ETag", "\"v1\"")
                    .build()
            )
            reply(200, """{"version":"v2"}""")
            // Longer than any limit on a save: the terminal can grow the notes past one.
            val grown = "from the Mac\n".repeat(200_000)
            reply(
                409,
                """{"error":"the notes changed since that version","refused":"changed",
                   "text":"${grown.replace("\n", "\\n")}","version":"v3"}""",
            )

            val notes = client.notes("app")
            assertEquals(dev.pm.app.model.Notes("# Ideas\n", "v1"), notes)
            assertEquals("v2", client.saveNotes("app", "# Ideas\nmore\n", notes.version))
            val stale = runCatching { client.saveNotes("app", "stale\n", "v2") }.exceptionOrNull()
            assertTrue("$stale", stale is PmError.NotesChanged)
            assertEquals(
                dev.pm.app.model.Notes(grown, "v3"),
                (stale as PmError.NotesChanged).current,
            )

            assertEquals("/v1/projects/app/notes", server.takeRequest().url.encodedPath)
            val save = server.takeRequest()
            assertEquals("PUT", save.method)
            assertEquals("\"v1\"", save.headers["If-Match"])
            assertEquals("# Ideas\nmore\n", save.body?.utf8())
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
        val rows = info.rows(ZoneId.of("Europe/Dublin"))
        assertEquals(
            listOf(
                "Status" to "in progress",
                "Branch" to "login",
                "Remote" to "none",
                "Base" to "main",
                "Divergence" to "3 ahead, 1 behind main",
                "Workflow" to "review",
            ),
            rows.filter { it.first != "Created" },
        )
        // 18:36 UTC, in the phone's zone: Dublin is an hour ahead in October.
        val created = rows.toMap().getValue("Created")
        assertTrue(created, Regex("""\b(19|7):36\b""").containsMatchIn(created))
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

    @Test
    fun typed_text_is_posted_and_a_server_without_it_is_unsupported() = runBlocking {
        reply(200, "{}")
        reply(404, """{"error":"no such endpoint"}""")

        client.typeText("app", "login", "implementer", "4821")
        val old = runCatching { client.typeText("app", "login", "implementer", "4821") }

        val sent = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/type", sent.url.encodedPath)
        assertEquals("""{"text":"4821"}""", sent.body?.utf8())
        assertTrue("$old", old.exceptionOrNull() is PmError.Unsupported)
    }

    @Test
    fun every_open_dialog_is_listed_and_a_server_without_the_list_gives_its_oldest() = runBlocking {
        val dialog = """{"id":"%s","kind":"permission","choices":[]}"""
        reply(200, """{"dialogs":[${dialog.format("d1")},${dialog.format("d2")}]}""")
        reply(404, """{"error":"no such endpoint"}""")
        reply(200, dialog.format("d1"))

        assertEquals(
            listOf("d1", "d2"),
            client.dialogs("app", "login", "implementer").map { it.id },
        )
        assertEquals(listOf("d1"), client.dialogs("app", "login", "implementer").map { it.id })
        assertEquals(
            listOf("dialogs", "dialogs", "dialog"),
            List(3) { server.takeRequest().url.encodedPath.substringAfterLast('/') },
        )
    }

    @Test
    fun a_dialog_is_read_and_answered_and_none_is_null() = runBlocking {
        reply(
            200,
            """{"id":"d1","kind":"question","since":"2026-10-05T12:00:00Z","questions":[
                {"question":"Which DB?","header":"DB","options":[{"label":"SQLite","description":""}],
                 "multi_select":true,"custom":true}],
                "choices":[{"id":"answer","label":"Submit answers","takes_message":false}]}""",
        )
        reply(200, """{"answered":true}""")
        reply(409, """{"error":"the dialog is no longer up","refused":"answered"}""")
        reply(404, """{"error":"no dialog of the agent's can be answered"}""")

        val dialog = client.dialog("app", "login", "implementer")!!
        assertEquals("d1", dialog.id)
        assertTrue(dialog.questions.single().multiSelect)
        val answer = DialogAnswer("d1", "answer", mapOf("Which DB?" to listOf("SQLite")))
        client.answerDialog("app", "login", "implementer", answer)
        val late = runCatching {
            client.answerDialog("app", "login", "implementer", answer)
        }
            .exceptionOrNull()
        assertTrue("$late", late is PmError.Refused && late.code == "answered")
        assertNull(client.dialog("app", "login", "implementer"))

        assertEquals(
            "/v1/agents/app/login/implementer/dialog",
            server.takeRequest().url.encodedPath,
        )
        val posted = server.takeRequest()
        assertEquals("POST", posted.method)
        assertEquals(
            """{"id":"d1","choice":"answer","answers":{"Which DB?":["SQLite"]}}""",
            posted.body?.utf8(),
        )
    }

    @Test
    fun lifecycle_actions_are_posted_and_a_refusal_is_told_apart() = runBlocking {
        reply(200, """{"merged":true}""")
        reply(200, """{"deleted":true}""")
        reply(409, """{"error":"agent 'implementer' is mid-turn","refused":"mid-turn"}""")

        client.merge("app", "login")
        client.delete("app", "login")
        val busy = runCatching {
            client.restart("app", "login", "implementer", force = true)
        }
            .exceptionOrNull()
        assertTrue("$busy", busy is PmError.Refused && busy.code == "mid-turn")

        val merged = server.takeRequest()
        assertEquals("POST", merged.method)
        assertEquals("/v1/features/app/login/merge", merged.url.encodedPath)
        assertEquals("/v1/features/app/login/delete", server.takeRequest().url.encodedPath)
        val restarted = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/restart", restarted.url.encodedPath)
        assertEquals("""{"force":true}""", restarted.body?.utf8())
    }

    @Test
    fun project_actions_are_posted_and_an_older_server_is_told_apart() = runBlocking {
        reply(200, """{"opened":true,"sessions":2,"agents":1,"warnings":["skipping 'x'"]}""")
        reply(200, """{"closed":true,"sessions":2}""")
        reply(409, """{"error":"feature 'login' has unpushed commits","refused":"unsafe"}""")
        reply(404, """{"error":"no such endpoint"}""")

        assertEquals(listOf("skipping 'x'"), client.openProject("app"))
        client.closeProject("app")
        val refused = runCatching { client.deleteProject("app") }.exceptionOrNull()
        assertTrue("$refused", refused is PmError.Refused && refused.code == "unsafe")
        val older = runCatching { client.openProject("app") }.exceptionOrNull()
        assertTrue("$older", older is PmError.Unsupported && !older.retired)

        val opened = server.takeRequest()
        assertEquals("POST", opened.method)
        assertEquals("/v1/projects/app/open", opened.url.encodedPath)
        assertEquals("/v1/projects/app/close", server.takeRequest().url.encodedPath)
        assertEquals("/v1/projects/app/delete", server.takeRequest().url.encodedPath)
    }
}
