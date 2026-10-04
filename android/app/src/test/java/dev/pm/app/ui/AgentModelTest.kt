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
}
