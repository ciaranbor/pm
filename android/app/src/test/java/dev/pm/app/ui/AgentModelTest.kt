package dev.pm.app.ui

import androidx.lifecycle.viewModelScope
import dev.pm.app.OpenStream
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.eventually
import dev.pm.app.model.Item
import dev.pm.app.model.Pairing
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableSharedFlow
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
class AgentModelTest {
    private val server = MockWebServer()
    private val stream = OpenStream()
    private lateinit var model: AgentModel
    private val network = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        model = AgentModel(client, "app", "login", "implementer", network)
    }

    @After
    fun tearDown() {
        stream.release()
        server.close()
        Dispatchers.resetMain()
    }

    /**
     * Runs `body`, then ends the model's coroutines, waiting out each call in flight to resume on
     * Main.
     */
    private fun modelTest(body: suspend TestScope.() -> Unit): TestResult = runTest {
        body()
        model.viewModelScope.cancel()
        eventually { model.viewModelScope.coroutineContext.job.isCompleted }
    }

    private fun page(ids: List<String>, before: String?, after: String?): MockResponse {
        val items = ids.joinToString(",") { """{"id":"$it","kind":"user","text":"$it"}""" }
        val cursors =
            listOf("before" to before, "after" to after).joinToString(",") { (k, v) ->
                "\"$k\":${v?.let { "\"$it\"" } ?: "null"}"
            }
        return MockResponse.Builder().body("""{"items":[$items],$cursors}""").build()
    }

    private val shown
        get() = model.chat.value as? ChatState.Shown

    private val ids
        get() = shown?.conversation?.items?.map { it.id }

    @Test
    fun a_conversation_that_starts_as_the_watch_opens_is_read_again() = modelTest {
        server.enqueue(
            MockResponse.Builder()
                .code(404)
                .body("""{"error":"the agent has no conversation yet"}""")
                .build()
        )
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        server.enqueue(page(listOf("1"), before = null, after = "c1"))
        model.start()
        eventually { ids == listOf("1") }
        assertEquals("c1", shown!!.conversation.after)
    }

    @Test
    fun paging_back_continues_past_an_empty_page() = modelTest {
        server.enqueue(page(listOf("3"), before = "c2", after = "c3"))
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        model.start()
        eventually { shown?.live == true }

        server.enqueue(page(emptyList(), before = "c1", after = null))
        server.enqueue(page(listOf("1"), before = null, after = null))
        model.older()
        eventually { ids == listOf("1", "3") }
        assertNull(shown!!.conversation.before)
    }

    @Test
    fun a_reset_replaces_the_conversation() = modelTest {
        server.enqueue(page(listOf("1", "2"), before = null, after = "c2"))
        val reset =
            """{"project":"app","scope":"login","agent":"implementer","reset":true,
            "items":[{"id":"a","kind":"assistant","text":"fresh"}],"before":null,"after":"x1"}"""
        server.enqueue(stream.response("snapshot" to SNAPSHOT, "transcript" to reset))
        model.start()
        eventually { ids == listOf("a") }
        assertEquals(Item.Assistant("a", null, "fresh"), shown!!.conversation.items.single())
        assertEquals("x1", shown!!.conversation.after)
    }

    @Test
    fun an_unreadable_page_fails_the_chat_rather_than_the_app() = modelTest {
        server.enqueue(MockResponse.Builder().body("not json").build())
        model.start()
        eventually { model.chat.value is ChatState.Failed }
    }

    @Test
    fun a_network_change_reopens_the_watch_from_where_it_was() = modelTest {
        server.enqueue(page(listOf("3"), before = "c2", after = "c3"))
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        model.start()
        eventually { shown?.live == true }

        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        network.tryEmit(Unit)
        eventually { server.requestCount == 3 }
        server.takeRequest()
        server.takeRequest()
        val reopened = server.takeRequest()
        assertEquals("/v1/events", reopened.url.encodedPath)
        assertEquals("c3", reopened.url.queryParameter("after"))
        assertEquals(listOf("3"), ids)
    }

    @Test
    fun queued_text_is_seen_once_the_conversation_holds_it() = modelTest {
        server.enqueue(MockResponse.Builder().body("""{"delivery":"queued"}""").build())
        model.send("deploy it\n")
        eventually { model.outbox.value == Outbox.Queued("deploy it\n") }
        val posted = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/input", posted.url.encodedPath)
        assertEquals("""{"text":"deploy it\n"}""", posted.body?.utf8())

        server.enqueue(page(listOf("1"), before = null, after = "c1"))
        val typed =
            """{"project":"app","scope":"login","agent":"implementer",
            "items":[{"id":"u","kind":"user","text":"deploy it"}],"after":"c2"}"""
        server.enqueue(stream.response("snapshot" to SNAPSHOT, "transcript" to typed))
        model.start()
        eventually { model.outbox.value == Outbox.Seen("deploy it\n") }
    }

    @Test
    fun a_refused_send_says_why_and_keeps_the_text() = modelTest {
        server.enqueue(
            MockResponse.Builder()
                .code(409)
                .body("""{"error":"a dialog is up (permission prompt)","refused":"asking"}""")
                .build()
        )
        model.send("yes")
        eventually { model.outbox.value is Outbox.Failed }
        assertEquals(
            Outbox.Failed("yes", "a dialog is up (permission prompt)"),
            model.outbox.value,
        )
    }

    @Test
    fun the_same_words_said_before_the_send_are_not_taken_for_it() = modelTest {
        server.enqueue(page(listOf("yes"), before = null, after = "c1"))
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        model.start()
        eventually { shown?.live == true }

        server.enqueue(
            MockResponse.Builder().body("""{"delivery":"sent","confirmed":false}""").build()
        )
        model.send("yes")
        eventually { model.outbox.value !is Outbox.Sending }
        assertEquals(Outbox.Sent("yes"), model.outbox.value)
    }

    @Test
    fun a_page_read_after_the_send_is_not_taken_for_it() = modelTest {
        server.enqueue(
            MockResponse.Builder().body("""{"delivery":"sent","confirmed":false}""").build()
        )
        model.send("yes")
        eventually { model.outbox.value == Outbox.Sent("yes") }

        server.enqueue(page(listOf("yes"), before = null, after = "c1"))
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        model.start()
        eventually { shown?.live == true }
        assertEquals(Outbox.Sent("yes"), model.outbox.value)
    }

    private val permission =
        """{"id":"d1","kind":"permission","tool":"Bash","detail":"cargo publish",
        "choices":[{"id":"allow","label":"Yes"},{"id":"deny","label":"No","takes_message":true}]}"""

    @Test
    fun a_dialog_the_snapshot_names_is_read_once_and_answered() = modelTest {
        server.enqueue(MockResponse.Builder().body(permission).build())
        model.dialogNamed("d1")
        eventually { model.dialog.value?.id == "d1" }
        model.dialogNamed("d1")
        assertEquals(
            "/v1/agents/app/login/implementer/dialog",
            server.takeRequest().url.encodedPath,
        )

        server.enqueue(MockResponse.Builder().body("""{"answered":true}""").build())
        model.answer("deny", message = "dry-run first")
        eventually { model.dialog.value == null && !model.answering.value }
        val posted = server.takeRequest()
        assertEquals("POST", posted.method)
        assertEquals(
            """{"id":"d1","choice":"deny","message":"dry-run first"}""",
            posted.body?.utf8(),
        )
        assertEquals(2, server.requestCount)
        assertNull(model.notice.value)
    }

    @Test
    fun a_dialog_answered_at_the_terminal_first_says_so_and_is_read_again() = modelTest {
        server.enqueue(MockResponse.Builder().body(permission).build())
        model.dialogNamed("d1")
        eventually { model.dialog.value?.id == "d1" }

        server.enqueue(
            MockResponse.Builder()
                .code(409)
                .body("""{"error":"the dialog is no longer up","refused":"answered"}""")
                .build()
        )
        server.enqueue(
            MockResponse.Builder()
                .code(404)
                .body("""{"error":"no dialog of the agent's can be answered"}""")
                .build()
        )
        model.answer("allow")
        eventually { server.requestCount == 3 && model.dialog.value == null }
        assertEquals("Answered elsewhere", model.notice.value)

        model.dialogNamed(null)
        assertNull(model.dialog.value)
    }
}
