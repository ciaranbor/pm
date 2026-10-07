package dev.pm.app.ui

import androidx.lifecycle.SavedStateHandle
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
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch

/** Why an edit went wrong, and whether saving again is the way to retry it. */
data class NotesFailure(val message: String, val saveAgain: Boolean)

sealed interface NotesState {
    data object Loading : NotesState

    data class Viewing(val notes: Notes) : NotesState {
        /** Too long to edit or render here. */
        val tooLong: Boolean = overNotesLimit(notes.text)
    }

    /** An edit, in [NotesModel.text], of `base`; `changed` says whether it differs. */
    data class Editing(
        val base: Notes,
        val changed: Boolean = false,
        val saving: Boolean = false,
    ) : NotesState

    /** A save was refused: the notes became `theirs` while `mine` was edited. */
    data class Conflict(val mine: String, val theirs: Notes) : NotesState {
        /** Whether both texts merged fit in a save. */
        val mergeable: Boolean = !overNotesLimit(NotesModel.merged(mine, theirs.text))
    }

    data object Unreachable : NotesState

    data class Failed(val reason: String) : NotesState
}

/**
 * A project's notes, read and edited. The text being edited is the editor's own, which [edited]
 * hands here, so a keystroke never passes through [state] or copies the text. It is kept in
 * `drafts` a moment after each edit, off the main thread and in order, until saved or discarded, so
 * leaving the screen, the process dying, or failing to reach the server loses nothing; opening the
 * notes again resumes it. Writes run on `writes`, by default one dispatcher every model shares, so
 * they land one at a time and in order across models, and outlive the model so the last one lands.
 * A failed write is sent on [failures].
 */
class NotesModel(
    private val client: PmClient?,
    private val project: String,
    private val drafts: NotesDrafts,
    private val saved: SavedStateHandle,
    writes: CoroutineDispatcher = DRAFT_WRITES,
) : ViewModel() {
    private val _state = MutableStateFlow<NotesState>(NotesState.Loading)
    val state: StateFlow<NotesState> = _state.asStateFlow()

    private val _failures = Channel<NotesFailure>(Channel.BUFFERED)

    /** Each edit's error once, as it happens: a save failing the same way twice is sent twice. */
    val failures: Flow<NotesFailure> = _failures.receiveAsFlow()

    private fun fail(error: String, saveAgain: Boolean = true) {
        _failures.trySend(NotesFailure(error, saveAgain))
    }

    /** The text being edited, while [state] is [NotesState.Editing]. */
    var text: CharSequence = ""
        private set

    /**
     * The offsets of the text at the top of the editor and of its cursor: where it opens, and where
     * they were when last shown, so a rotation, a look at the preview, or the process dying returns
     * to the same place. A kept draft holds them too, for opening the notes again after leaving.
     */
    var top: Int
        get() = saved[TOP] ?: 0
        set(value) {
            saved[TOP] = value
        }

    var cursor: Int
        get() = saved[CURSOR] ?: top
        set(value) {
            saved[CURSOR] = value
        }

    /** Whether the editor had the keyboard when last shown, to give it back after a rotation. */
    var focused: Boolean
        get() = saved[FOCUSED] ?: false
        set(value) {
            saved[FOCUSED] = value
        }

    private val writer = CoroutineScope(SupervisorJob() + writes)
    private var reading: Job? = null
    private var pending: Job? = null
    private var unkept = false
    private var keptAt = 0 to 0

    init {
        val draft = drafts.draft(project)
        if (draft != null) {
            open(draft, saved[TOP] ?: draft.top, saved[CURSOR] ?: draft.cursor)
            keptAt = top to cursor
        } else reload()
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

    /** Edit the notes with the cursor, and the top of the editor, at offset `at`. */
    fun edit(at: Int = 0) {
        val now = _state.value as? NotesState.Viewing ?: return
        if (now.tooLong) return
        reading?.cancel()
        open(NotesDraft(now.notes, now.notes.text), at)
    }

    /** The editor's text is now `text`: keep it once typing pauses. */
    fun edited(text: CharSequence) {
        val now = _state.value as? NotesState.Editing ?: return
        this.text = text
        val changed = text.length != now.base.text.length || !text.contentEquals(now.base.text)
        if (changed != now.changed) _state.value = now.copy(changed = changed)
        unkept = true
        pending?.cancel()
        pending = viewModelScope.launch {
            delay(KEEP_AFTER_MS)
            keep()
        }
    }

    /** Keep the edit now if it, or where the editor is in it, changed since it was last kept. */
    fun keep() {
        pending?.cancel()
        val now = _state.value as? NotesState.Editing ?: return
        if (!unkept && (!now.changed || keptAt == top to cursor)) return
        unkept = false
        keptAt = top to cursor
        val text = text.toString()
        write(if (text == now.base.text) null else NotesDraft(now.base, text, top, cursor))
    }

    /** Drop the edit and show the notes as they are now. */
    fun discard() {
        val now = _state.value as? NotesState.Editing ?: return
        if (now.saving) return
        forget()
        _state.value = NotesState.Viewing(now.base)
        reload()
    }

    fun save() {
        val now = _state.value as? NotesState.Editing ?: return
        if (now.saving) return
        val draft = NotesDraft(now.base, text.toString())
        if (overNotesLimit(draft.text)) {
            fail(TOO_LONG, saveAgain = false)
            return
        }
        pending?.cancel()
        unkept = false
        write(draft)
        _state.value = now.copy(saving = true)
        viewModelScope.launch {
            _state.value =
                try {
                    val version = paired().saveNotes(project, draft.text, draft.base.version)
                    forget()
                    NotesState.Viewing(Notes(draft.text, version))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.NotesChanged) {
                    conflict(draft.text, e.current)
                } catch (e: PmError.Unreachable) {
                    now.also { fail("Can't reach pm serve. The edit is kept on this phone.") }
                } catch (e: Exception) {
                    now.also { fail(e.message ?: e.javaClass.simpleName) }
                }
        }
    }

    /** Settle a conflict with the notes as they are on the server, dropping the edit. */
    fun keepTheirs() {
        val now = _state.value as? NotesState.Conflict ?: return
        forget()
        _state.value = NotesState.Viewing(now.theirs)
    }

    /** Settle a conflict by saving the edit over the notes as they are on the server. */
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
        unkept = true
        keep()
    }

    override fun onCleared() {
        keep()
    }

    private fun open(draft: NotesDraft, top: Int = 0, cursor: Int = top) {
        text = draft.text
        this.top = top.coerceIn(0, draft.text.length)
        this.cursor = cursor.coerceIn(0, draft.text.length)
        unkept = false
        _state.value = NotesState.Editing(draft.base, changed = draft.text != draft.base.text)
    }

    private fun forget() {
        pending?.cancel()
        unkept = false
        write(null)
    }

    private fun write(draft: NotesDraft?) {
        writer.launch {
            try {
                drafts.keep(project, draft)
            } catch (e: Exception) {
                if (_state.value is NotesState.Editing)
                    fail(
                        "Couldn't keep the edit on this phone: " +
                            (e.message ?: e.javaClass.simpleName),
                        saveAgain = false,
                    )
            }
        }
    }

    /**
     * Where the notes went while `mine` was edited; nothing to settle if they agree. The kept draft
     * still names the version the edit started from, so resuming it meets the conflict again.
     */
    private fun conflict(mine: String, theirs: Notes): NotesState =
        if (mine == theirs.text) {
            forget()
            NotesState.Viewing(theirs)
        } else {
            NotesState.Conflict(mine, theirs)
        }

    private fun paired(): PmClient = client ?: throw IllegalStateException("not paired")

    companion object {
        const val TOO_LONG =
            "Notes over ${MAX_NOTES_BYTES / 1024} KB are edited with pm notes on the server."

        private const val TOP = "top"
        private const val CURSOR = "cursor"
        private const val FOCUSED = "focused"

        private val DRAFT_WRITES = Dispatchers.IO.limitedParallelism(1)

        /** How long typing pauses before the edit is kept. */
        const val KEEP_AFTER_MS = 500L

        /** Both texts in one, each between conflict markers, as git writes them. */
        fun merged(mine: String, theirs: String): String =
            "<<<<<<< this phone\n${mine.withNewline()}=======\n" +
                "${theirs.withNewline()}>>>>>>> the server\n"

        private fun String.withNewline() = if (isEmpty() || endsWith("\n")) this else "$this\n"
    }
}
