package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.data.NotesDrafts
import dev.pm.app.model.MAX_NOTES_BYTES
import dev.pm.app.model.Notes
import dev.pm.app.model.NotesDraft
import dev.pm.app.model.overNotesLimit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

sealed interface NotesState {
    data object Loading : NotesState

    data class Viewing(val notes: Notes) : NotesState {
        /** Too long to edit or render here. */
        val tooLong: Boolean = overNotesLimit(notes.text)
    }

    /** An edit of `draft.base`; `error` says why the last save failed. */
    data class Editing(
        val draft: NotesDraft,
        val saving: Boolean = false,
        val error: String? = null,
    ) : NotesState {
        val changed: Boolean
            get() = draft.text != draft.base.text
    }

    /** A save was refused: the notes became `theirs` while `mine` was edited. */
    data class Conflict(val mine: String, val theirs: Notes) : NotesState {
        /** Whether both texts merged fit in a save. */
        val mergeable: Boolean = !overNotesLimit(NotesModel.merged(mine, theirs.text))
    }

    data object Unreachable : NotesState

    data class Failed(val reason: String) : NotesState
}

/**
 * A project's notes, read and edited. An edit is kept in `drafts` from its first keystroke until it
 * is saved or discarded, so leaving the screen or failing to reach the server loses nothing;
 * opening the notes again resumes it.
 */
class NotesModel(
    private val client: PmClient?,
    private val project: String,
    private val drafts: NotesDrafts,
) : ViewModel() {
    private val _state = MutableStateFlow<NotesState>(NotesState.Loading)
    val state: StateFlow<NotesState> = _state.asStateFlow()

    private var reading: Job? = null

    init {
        val draft = drafts.draft(project)
        if (draft != null) _state.value = NotesState.Editing(draft) else reload()
    }

    /** Read the notes again, unless an edit is open. */
    fun reload() {
        val now = _state.value
        if (now is NotesState.Editing || now is NotesState.Conflict) return
        if (reading?.isActive == true) return
        if (now !is NotesState.Viewing) _state.value = NotesState.Loading
        reading = viewModelScope.launch {
            _state.value =
                try {
                    NotesState.Viewing(paired().notes(project))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Unreachable) {
                    if (now is NotesState.Viewing) now else NotesState.Unreachable
                } catch (e: Exception) {
                    NotesState.Failed(e.message ?: e.javaClass.simpleName)
                }
        }
    }

    fun edit() {
        val now = _state.value as? NotesState.Viewing ?: return
        if (now.tooLong) return
        reading?.cancel()
        open(NotesDraft(now.notes, now.notes.text))
    }

    fun type(text: String) {
        val now = _state.value as? NotesState.Editing ?: return
        if (now.saving) return
        open(now.draft.copy(text = text), now.error)
    }

    /** Drop the edit and show the notes as they are now. */
    fun discard() {
        val now = _state.value as? NotesState.Editing ?: return
        if (now.saving) return
        drafts.keep(project, null)
        _state.value = NotesState.Viewing(now.draft.base)
        reload()
    }

    fun save() {
        val now = _state.value as? NotesState.Editing ?: return
        if (now.saving) return
        val draft = now.draft
        if (overNotesLimit(draft.text)) {
            _state.value = now.copy(error = TOO_LONG)
            return
        }
        _state.value = now.copy(saving = true, error = null)
        viewModelScope.launch {
            _state.value =
                try {
                    val version = paired().saveNotes(project, draft.text, draft.base.version)
                    drafts.keep(project, null)
                    NotesState.Viewing(Notes(draft.text, version))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.NotesChanged) {
                    conflict(draft.text, e.current)
                } catch (e: PmError.Unreachable) {
                    now.copy(error = "Can't reach pm serve. The edit is kept on this phone.")
                } catch (e: Exception) {
                    now.copy(error = e.message ?: e.javaClass.simpleName)
                }
        }
    }

    /** Settle a conflict with the notes as they are on the Mac, dropping the edit. */
    fun keepTheirs() {
        val now = _state.value as? NotesState.Conflict ?: return
        drafts.keep(project, null)
        _state.value = NotesState.Viewing(now.theirs)
    }

    /** Settle a conflict by saving the edit over the notes as they are on the Mac. */
    fun keepMine() {
        val now = _state.value as? NotesState.Conflict ?: return
        open(NotesDraft(now.theirs, now.mine))
        save()
    }

    /** Settle a conflict by editing both texts, marked, into one. */
    fun merge() {
        val now = _state.value as? NotesState.Conflict ?: return
        if (!now.mergeable) return
        open(NotesDraft(now.theirs, merged(now.mine, now.theirs.text)))
    }

    private fun open(draft: NotesDraft, error: String? = null) {
        drafts.keep(project, draft)
        _state.value = NotesState.Editing(draft, error = error)
    }

    /**
     * Where the notes went while `mine` was edited; nothing to settle if they agree. The kept draft
     * still names the version the edit started from, so resuming it meets the conflict again.
     */
    private fun conflict(mine: String, theirs: Notes): NotesState =
        if (mine == theirs.text) {
            drafts.keep(project, null)
            NotesState.Viewing(theirs)
        } else {
            NotesState.Conflict(mine, theirs)
        }

    private fun paired(): PmClient = client ?: throw IllegalStateException("not paired")

    companion object {
        const val TOO_LONG =
            "Notes over ${MAX_NOTES_BYTES / 1024} KB are edited with pm notes on the Mac."

        /** Both texts in one, each between conflict markers, as git writes them. */
        fun merged(mine: String, theirs: String): String =
            "<<<<<<< this phone\n${mine.withNewline()}=======\n" +
                "${theirs.withNewline()}>>>>>>> the Mac\n"

        private fun String.withNewline() = if (isEmpty() || endsWith("\n")) this else "$this\n"
    }
}
