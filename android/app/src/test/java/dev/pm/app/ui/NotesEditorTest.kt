package dev.pm.app.ui

import android.os.SystemClock
import android.view.MotionEvent
import android.widget.EditText
import androidx.activity.ComponentActivity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.SavedStateHandle
import dev.pm.app.api.PmClient
import dev.pm.app.data.NotesDrafts
import dev.pm.app.model.Notes
import dev.pm.app.model.NotesDraft
import dev.pm.app.model.Pairing
import kotlinx.coroutines.Dispatchers
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NotesEditorTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val text =
        (1..8).joinToString("\n\n") { s ->
            "## Section $s\n\n" + (1..25).joinToString("\n\n") { "- Item $s.$it: a note." }
        }
    private val base = Notes(text, "v1")
    private val drafts =
        object : NotesDrafts {
            val kept = mutableMapOf<String, NotesDraft>()

            override fun draft(project: String) = kept[project]

            override fun keep(project: String, draft: NotesDraft?) {
                if (draft == null) kept.remove(project) else kept[project] = draft
            }
        }
    private val server = MockWebServer()

    @After
    fun stop() {
        server.close()
    }

    /** The notes screen, editing `base` unchanged, with the editor's place as `saved` holds it. */
    private fun editing(saved: SavedStateHandle = SavedStateHandle()): EditText {
        drafts.kept["app"] = NotesDraft(base, base.text)
        val model = NotesModel(null, "app", drafts, saved, Dispatchers.Unconfined)
        compose.setContent { NotesScreen(model, TopBarSlot()) }
        return field()
    }

    private fun field(): EditText {
        compose.waitForIdle()
        return compose.activity.window.decorView.findViewWithTag(EDITOR_TAG)
    }

    private fun EditText.topOf(offset: Int) = layout.getLineTop(layout.getLineForOffset(offset))

    @Test
    fun a_tap_after_scrolling_puts_the_cursor_where_tapped_and_leaves_the_text_still() {
        val field = editing()
        val scrolled = compose.runOnIdle {
            field.scrollTo(0, field.layout.getLineTop(150))
            field.scrollY
        }
        val x = 120f
        val y = field.height / 3f
        compose.runOnIdle {
            val at = SystemClock.uptimeMillis()
            for (action in listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP)) {
                val event = MotionEvent.obtain(at, at, action, x, y, 0)
                field.dispatchTouchEvent(event)
                event.recycle()
            }
        }
        compose.runOnIdle {
            assertEquals(scrolled, field.scrollY)
            assertTrue(field.hasFocus())
            val layout = field.layout
            val line = layout.getLineForVertical(scrolled + y.toInt() - field.totalPaddingTop)
            assertEquals(
                layout.getOffsetForHorizontal(line, x - field.totalPaddingLeft),
                field.selectionStart,
            )
        }
    }

    @Test
    fun stopping_mid_edit_keeps_the_edit_at_once() {
        val field = editing()
        compose.runOnIdle {
            field.setSelection(0)
            field.text.insert(0, "typed ")
        }
        compose.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        compose.runOnIdle { assertEquals("typed $text", drafts.kept.getValue("app").text) }
    }

    @Test
    fun the_editor_reopens_where_it_was_after_rotation_or_process_death() {
        val top = text.indexOf("- Item 5.3")
        val cursor = text.indexOf("- Item 5.7")
        val field = editing(SavedStateHandle(mapOf("top" to top, "cursor" to cursor)))
        compose.runOnIdle {
            assertEquals(field.topOf(top), field.scrollY)
            assertEquals(cursor, field.selectionStart)
        }
    }

    @Test
    fun a_look_at_the_preview_returns_to_the_same_place() {
        val first = editing()
        val (scrolled, cursor) =
            compose.runOnIdle {
                first.scrollTo(0, first.topOf(text.indexOf("- Item 6.1")))
                first.setSelection(text.indexOf("- Item 6.4"))
                first.scrollY to first.selectionStart
            }
        compose.onNodeWithText("Preview").performClick()
        compose.onNodeWithText("Edit").performClick()
        val again = field()
        compose.runOnIdle {
            assertTrue("the field is made again", again !== first)
            assertEquals(scrolled, again.scrollY)
            assertEquals(cursor, again.selectionStart)
        }
    }

    @Test
    fun edit_opens_at_the_section_being_read() {
        server.enqueue(MockResponse.Builder().body(text).addHeader("ETag", "\"v1\"").build())
        server.start()
        val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        val model = NotesModel(client, "app", drafts, SavedStateHandle(), Dispatchers.Unconfined)
        compose.setContent { NotesScreen(model, TopBarSlot()) }
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Section 4").fetchSemanticsNodes().isNotEmpty()
        }

        val page = compose.onNode(hasScrollAction())
        val heading = compose.onNodeWithText("Section 4").fetchSemanticsNode().positionInRoot.y
        val pageTop = page.fetchSemanticsNode().positionInRoot.y
        // Into the section, past its heading.
        page.performSemanticsAction(SemanticsActions.ScrollBy) { it(0f, heading - pageTop + 200f) }
        compose.onNodeWithText("Edit").performClick()

        val field = field()
        compose.runOnIdle {
            assertEquals(text.indexOf("## Section 4"), field.selectionStart)
            assertEquals(field.topOf(text.indexOf("## Section 4")), field.scrollY)
        }
    }
}
