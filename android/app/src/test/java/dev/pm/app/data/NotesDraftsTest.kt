package dev.pm.app.data

import android.content.Context
import androidx.core.content.edit
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.model.Notes
import dev.pm.app.model.NotesDraft
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class NotesDraftsTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val prefs = context.getSharedPreferences("pm", Context.MODE_PRIVATE)
    private val draft = NotesDraft(Notes("old\n", "v1"), "new\n")

    @Test
    fun a_draft_is_kept_per_project_until_forgotten() {
        val store = Store(context)
        store.keep("web/shop", draft)
        store.keep("app", NotesDraft(Notes("", "v9"), "other"))

        assertEquals("another store reads it back", draft, Store(context).draft("web/shop"))
        store.keep("web/shop", null)
        assertNull(store.draft("web/shop"))
        assertEquals("other", store.draft("app")?.text)
    }

    @Test
    fun a_draft_kept_before_drafts_had_files_is_read_then_moved_on_the_next_keep() {
        prefs.edit {
            putString(
                "notes_draft/app",
                """{"base":{"text":"old\n","version":"v1"},"text":"new\n"}""",
            )
        }
        val store = Store(context)
        assertEquals(draft, store.draft("app"))

        val edited = draft.copy(text = "newer\n")
        store.keep("app", edited)
        assertFalse(prefs.contains("notes_draft/app"))
        assertEquals(edited, Store(context).draft("app"))
    }
}
