package dev.pm.app.ui

import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.data.NotesDrafts
import dev.pm.app.eventually
import dev.pm.app.model.MAX_NOTES_BYTES
import dev.pm.app.model.Notes
import dev.pm.app.model.NotesDraft
import dev.pm.app.model.Pairing
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
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
class NotesModelTest {
    private val server = MockWebServer()
    private val drafts =
        object : NotesDrafts {
            val kept = mutableMapOf<String, NotesDraft>()

            override fun draft(project: String) = kept[project]

            override fun keep(project: String, draft: NotesDraft?) {
                if (draft == null) kept.remove(project) else kept[project] = draft
            }
        }
    private lateinit var client: PmClient
    private val models = mutableListOf<NotesModel>()

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
        server.start()
        client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
    }

    @After
    fun tearDown() {
        server.close()
        Dispatchers.resetMain()
    }

    private fun model() = NotesModel(client, "app", drafts).also { models += it }

    private fun modelTest(body: suspend TestScope.() -> Unit): TestResult = runTest {
        body()
        models.forEach { it.viewModelScope.cancel() }
        eventually { models.all { it.viewModelScope.coroutineContext.job.isCompleted } }
    }

    private fun notes(text: String, version: String) =
        MockResponse.Builder().body(text).addHeader("ETag", "\"$version\"").build()

    private fun changed(text: String, version: String) =
        MockResponse.Builder()
            .code(409)
            .body(
                """{"error":"the notes changed since that version","refused":"changed",
                   "text":"$text","version":"$version"}"""
            )
            .build()

    @Test
    fun a_refused_save_shows_both_texts_and_a_merge_saves_against_the_macs_version() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val model = model()
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        model.type("old\nfrom the phone\n")

        server.enqueue(changed("old\\nfrom the Mac\\n", "v2"))
        model.save()
        eventually { model.state.value is NotesState.Conflict }
        assertEquals(
            NotesState.Conflict("old\nfrom the phone\n", Notes("old\nfrom the Mac\n", "v2")),
            model.state.value,
        )
        assertEquals(
            "a draft left in conflict meets it again when resumed",
            "v1",
            drafts.kept["app"]?.base?.version,
        )

        model.merge()
        val merging = model.state.value as NotesState.Editing
        assertEquals("v2", merging.draft.base.version)
        assertEquals(
            NotesModel.merged("old\nfrom the phone\n", "old\nfrom the Mac\n"),
            merging.draft.text,
        )

        server.enqueue(MockResponse.Builder().body("""{"version":"v3"}""").build())
        model.save()
        eventually { model.state.value is NotesState.Viewing }
        server.takeRequest()
        server.takeRequest()
        assertEquals("\"v2\"", server.takeRequest().headers["If-Match"])
        assertNull(drafts.kept["app"])
    }

    @Test
    fun an_edit_that_could_not_be_saved_is_resumed_when_the_notes_are_opened_again() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val first = model()
        eventually { first.state.value is NotesState.Viewing }
        first.edit()
        first.type("unsaved\n")
        server.close()
        first.save()
        eventually { (first.state.value as? NotesState.Editing)?.error != null }

        val again = model()
        assertEquals(
            NotesState.Editing(NotesDraft(Notes("old\n", "v1"), "unsaved\n")),
            again.state.value,
        )
    }

    @Test
    fun text_the_server_would_refuse_is_neither_merged_nor_sent() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val model = model()
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        model.type("phone\n")
        val grown = "x".repeat(MAX_NOTES_BYTES + 1)
        server.enqueue(changed(grown, "v2"))
        model.save()
        eventually { model.state.value is NotesState.Conflict }
        val conflict = model.state.value as NotesState.Conflict
        assertEquals(false, conflict.mergeable)
        model.merge()
        assertEquals(conflict, model.state.value)

        model.keepTheirs()
        assertEquals(true, (model.state.value as NotesState.Viewing).tooLong)
        model.edit()
        assertEquals(true, model.state.value is NotesState.Viewing)

        server.enqueue(notes("small\n", "v3"))
        val fresh = NotesModel(client, "other", drafts).also { models += it }
        eventually { fresh.state.value is NotesState.Viewing }
        fresh.edit()
        fresh.type(grown)
        fresh.save()
        assertEquals(NotesModel.TOO_LONG, (fresh.state.value as NotesState.Editing).error)
        assertEquals(3, server.requestCount)
    }
}
