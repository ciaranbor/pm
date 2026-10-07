package dev.pm.app.ui

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.data.NotesDrafts
import dev.pm.app.eventually
import dev.pm.app.model.MAX_NOTES_BYTES
import dev.pm.app.model.Notes
import dev.pm.app.model.NotesDraft
import dev.pm.app.model.Pairing
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestResult
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
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

    private fun model() =
        NotesModel(client, "app", drafts, SavedStateHandle(), Dispatchers.Unconfined).also {
            models += it
        }

    private fun modelTest(body: suspend TestScope.() -> Unit): TestResult = runTest {
        body()
        models.forEach { it.viewModelScope.cancel() }
        eventually { models.all { it.viewModelScope.coroutineContext.job.isCompleted } }
    }

    /** What `model` reports on [NotesModel.failures] from now on. */
    private fun TestScope.failuresOf(model: NotesModel): List<String> =
        mutableListOf<String>().also { seen ->
            backgroundScope.launch { model.failures.collect { seen += it.message } }
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
        model.edited("old\nfrom the phone\n")

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
        assertEquals("v2", merging.base.version)
        assertEquals(
            NotesModel.merged("old\nfrom the phone\n", "old\nfrom the Mac\n"),
            model.text.toString(),
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
        first.edited("unsaved\n")
        val failures = failuresOf(first)
        server.close()
        first.save()
        eventually { failures.isNotEmpty() }

        val again = model()
        assertEquals(NotesState.Editing(Notes("old\n", "v1"), changed = true), again.state.value)
        assertEquals("unsaved\n", again.text.toString())
    }

    @Test
    fun an_edit_left_reopens_where_the_editor_was() = modelTest {
        server.enqueue(notes("one\ntwo\nthree\n", "v1"))
        val first = model()
        eventually { first.state.value is NotesState.Viewing }
        first.edit()
        first.edited("one\ntwo\nthree\nfour\n")
        advanceTimeBy(NotesModel.KEEP_AFTER_MS + 1)
        first.top = 4
        first.cursor = 9
        first.keep()

        val again = model()
        assertEquals(4, again.top)
        assertEquals(9, again.cursor)
    }

    @Test
    fun text_the_server_would_refuse_is_neither_merged_nor_sent() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val model = model()
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        model.edited("phone\n")
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
        val fresh =
            NotesModel(client, "other", drafts, SavedStateHandle(), Dispatchers.Unconfined).also {
                models += it
            }
        eventually { fresh.state.value is NotesState.Viewing }
        fresh.edit()
        fresh.edited(grown)
        val failures = failuresOf(fresh)
        fresh.save()
        eventually { failures.isNotEmpty() }
        assertEquals(listOf(NotesModel.TOO_LONG), failures)
        assertEquals(3, server.requestCount)
    }

    @Test
    fun an_edit_is_kept_once_typing_pauses_and_typing_it_back_forgets_it() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val model = model()
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        model.edited("old\nnew\n")
        advanceTimeBy(NotesModel.KEEP_AFTER_MS - 1)
        assertNull("kept only once typing pauses", drafts.kept["app"])
        advanceTimeBy(2)
        assertEquals(NotesDraft(Notes("old\n", "v1"), "old\nnew\n"), drafts.kept["app"])

        model.edited("old\n")
        advanceTimeBy(NotesModel.KEEP_AFTER_MS + 1)
        assertNull(drafts.kept["app"])
    }

    @Test
    fun an_edit_the_phone_cannot_keep_says_so_in_the_editor() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val full =
            object : NotesDrafts {
                override fun draft(project: String): NotesDraft? = null

                override fun keep(project: String, draft: NotesDraft?) =
                    throw IOException("No space left on device")
            }
        val model =
            NotesModel(client, "app", full, SavedStateHandle(), Dispatchers.Unconfined).also {
                models += it
            }
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        val failures = mutableListOf<NotesFailure>()
        backgroundScope.launch { model.failures.collect { failures += it } }
        model.edited("new\n")
        advanceTimeBy(NotesModel.KEEP_AFTER_MS + 1)
        eventually { failures.isNotEmpty() }
        val failure = failures.single()
        assertTrue(failure.message, failure.message.contains("No space left on device"))
        assertFalse("saving again is no retry for keeping it here", failure.saveAgain)
    }

    @Test
    fun a_save_that_fails_the_same_way_twice_reports_both() = modelTest {
        server.enqueue(notes("old\n", "v1"))
        val model = model()
        eventually { model.state.value is NotesState.Viewing }
        model.edit()
        model.edited("unsaved\n")
        val failures = failuresOf(model)
        server.close()

        model.save()
        eventually { failures.size == 1 }
        model.save()
        eventually { failures.size == 2 }

        assertEquals(List(2) { "Can't reach pm serve. The edit is kept on this phone." }, failures)
    }
}
