package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.data.Backoff
import dev.pm.app.model.Conversation
import dev.pm.app.model.Dialog
import dev.pm.app.model.DialogAnswer
import dev.pm.app.model.Item
import dev.pm.app.model.TranscriptEvent
import dev.pm.app.model.Transcripts
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json

/** What the chat tab shows. */
sealed interface ChatState {
    data object Loading : ChatState

    data class Shown(val conversation: Conversation, val live: Boolean) : ChatState

    /** The server doesn't serve transcripts; `advice` says which side to update. */
    data class Unsupported(val advice: String) : ChatState

    data class Failed(val reason: String) : ChatState
}

/** A tool's whole output, by its result's `full` reference, as far as it has been read. */
data class WholeOutput(val ref: String, val body: ReaderBody)

/** Text the user sent the agent, and how far it got. */
sealed interface Outbox {
    val text: String

    data class Sending(override val text: String) : Outbox

    /** The agent is mid-turn: its harness holds the text until a step ends. */
    data class Queued(override val text: String) : Outbox

    /** Submitted, but not seen in the conversation yet. */
    data class Sent(override val text: String) : Outbox

    /** In the conversation. */
    data class Seen(override val text: String) : Outbox

    data class Failed(override val text: String, val reason: String) : Outbox
}

/**
 * One agent's conversation, kept current while its view is open: the latest page, then a watched
 * event stream from where that page ended, reopened with backoff, and at once when `networkChanges`
 * emits.
 */
class AgentModel(
    private val client: PmClient,
    private val project: String,
    private val scope: String,
    private val agent: String,
    private val networkChanges: Flow<Unit> = emptyFlow(),
) : ViewModel() {
    private val _chat = MutableStateFlow<ChatState>(ChatState.Loading)
    val chat: StateFlow<ChatState> = _chat.asStateFlow()

    /** The dialog the agent shows that can be answered here; null when none can. */
    private val _dialog = MutableStateFlow<Dialog?>(null)
    val dialog: StateFlow<Dialog?> = _dialog.asStateFlow()

    /** An answer to [dialog] is on its way. */
    private val _answering = MutableStateFlow(false)
    val answering: StateFlow<Boolean> = _answering.asStateFlow()

    private val _outbox = MutableStateFlow<Outbox?>(null)
    val outbox: StateFlow<Outbox?> = _outbox.asStateFlow()

    /** An interrupt is on its way. */
    private val _interrupting = MutableStateFlow(false)
    val interrupting: StateFlow<Boolean> = _interrupting.asStateFlow()

    private val _interrupted = Channel<String?>(Channel.BUFFERED)

    /** How each interrupt went, once: null once sent, else why it wasn't. */
    val interrupted: Flow<String?> = _interrupted.receiveAsFlow()

    /** Why the last answer failed; cleared by the next. */
    private val _notice = MutableStateFlow<String?>(null)
    val notice: StateFlow<String?> = _notice.asStateFlow()

    /**
     * Items the stream appended since the last send began: only these can be what was sent. A page
     * read, or a reset, may hold the same words said before ("yes").
     */
    private val arrivedSinceSend = mutableListOf<Item>()

    /**
     * The whole output the reader shows, one at a time: outputs can run to megabytes, so reading
     * another replaces it, and [closeWhole] lets it go.
     */
    private val _whole = MutableStateFlow<WholeOutput?>(null)
    val whole: StateFlow<WholeOutput?> = _whole.asStateFlow()
    private var readingWhole: Job? = null

    private var watching: Job? = null
    private val backoff = Backoff()
    private var reconnecting: Job? = null
    private var fetchingDialog: Job? = null
    private var paging = false

    private val conversation
        get() = (_chat.value as? ChatState.Shown)?.conversation

    /** Follow the conversation until [stop]. */
    fun start() {
        if (watching?.isActive != true) watching = viewModelScope.launch { follow() }
        if (reconnecting?.isActive != true) {
            reconnecting = viewModelScope.launch {
                networkChanges.collect {
                    watching?.cancel()
                    watching = viewModelScope.launch { follow() }
                }
            }
        }
    }

    private suspend fun follow() {
        while (true) {
            try {
                val held = conversation
                var unstarted = false
                if (held == null || held.after == null) {
                    val page =
                        try {
                            client.transcript(project, scope, agent)
                        } catch (e: PmError.NoConversation) {
                            null
                        }
                    unstarted = page == null
                    val conversation =
                        if (page == null) Conversation()
                        else
                            Conversation()
                                .replacedBy(Transcripts.items(page.items), page.before, page.after)
                    _chat.value = ChatState.Shown(conversation, live = false)
                }
                watch(unstarted)
            } catch (e: CancellationException) {
                throw e
            } catch (e: PmError.Unsupported) {
                _chat.value = ChatState.Unsupported(e.advice("see the conversation"))
                return
            } catch (e: Exception) {
                val shown = _chat.value
                _chat.value =
                    if (shown is ChatState.Shown) shown.copy(live = false)
                    else ChatState.Failed(e.message ?: e.javaClass.simpleName)
            }
            delay(backoff.next())
        }
    }

    /**
     * Follow the watched stream. With `unstarted` (the page read found no conversation), the
     * session may have started between that read and the watch's first, which then sends no reset:
     * once the stream's opening snapshot shows the watch has read, the page is read again.
     */
    private suspend fun watch(unstarted: Boolean) {
        val after = conversation?.after
        var recheck = unstarted
        client.events(watch = "$project/$scope/$agent", after = after).collect { event ->
            val shown = _chat.value as? ChatState.Shown ?: return@collect
            if (event.name != "transcript") {
                if (!shown.live) {
                    _chat.value = shown.copy(live = true)
                    backoff.reset()
                }
                if (recheck) {
                    recheck = false
                    viewModelScope.launch { recheckStarted() }
                }
                return@collect
            }
            val update =
                runCatching { json.decodeFromString(TranscriptEvent.serializer(), event.data) }
                    .getOrNull() ?: return@collect
            if (update.project != project || update.scope != scope || update.agent != agent)
                return@collect
            val items = Transcripts.items(update.items)
            val next =
                if (update.reset) {
                    Conversation().replacedBy(items, update.before, update.after)
                } else {
                    shown.conversation.appended(items, update.after)
                }
            _chat.value = ChatState.Shown(next, live = true)
            if (!update.reset) {
                arrivedSinceSend += items
                seen()
            }
        }
    }

    /** Type `text` into the agent; [outbox] follows it until it is seen in the conversation. */
    fun send(text: String) {
        if (_outbox.value is Outbox.Sending) return
        arrivedSinceSend.clear()
        _outbox.value = Outbox.Sending(text)
        viewModelScope.launch {
            _outbox.value =
                try {
                    val delivered = client.sendText(project, scope, agent, text)
                    when {
                        delivered.queued -> Outbox.Queued(text)
                        delivered.confirmed == true -> Outbox.Seen(text)
                        else -> Outbox.Sent(text)
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    Outbox.Failed(text, e.message ?: e.javaClass.simpleName)
                }
            seen()
        }
    }

    /** Mark what was sent seen once the stream has brought it since the send. */
    private fun seen() {
        val waiting = _outbox.value
        if (waiting !is Outbox.Queued && waiting !is Outbox.Sent) return
        val want = waiting.text.trim()
        if (arrivedSinceSend.any { it is Item.User && it.text.trim() == want }) {
            _outbox.value = Outbox.Seen(waiting.text)
        }
    }

    /** Forget a failed or finished send. */
    fun dismissOutbox() {
        if (_outbox.value !is Outbox.Sending) _outbox.value = null
    }

    fun interrupt() {
        if (_interrupting.value) return
        _interrupting.value = true
        viewModelScope.launch {
            val failure =
                try {
                    client.interrupt(project, scope, agent)
                    null
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    e.message ?: e.javaClass.simpleName
                } finally {
                    _interrupting.value = false
                }
            _interrupted.send(failure)
        }
    }

    /**
     * The snapshot names the agent's answerable dialog by `id`, null for none: read it when it is
     * one not held.
     */
    fun dialogNamed(id: String?) {
        if (id == null) {
            fetchingDialog?.cancel()
            _dialog.value = null
            return
        }
        if (_dialog.value?.id == id) return
        fetchDialog()
    }

    private fun fetchDialog() {
        fetchingDialog?.cancel()
        fetchingDialog = viewModelScope.launch {
            _dialog.value =
                try {
                    client.dialog(project, scope, agent)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    null
                }
        }
    }

    /**
     * Answer [dialog] with `choice`, the `answers` to its questions, and a `message` for the agent.
     * One refused (answered at the terminal first, or its hook gone) says why and is read again.
     */
    fun answer(
        choice: String,
        answers: Map<String, List<String>> = emptyMap(),
        message: String? = null,
    ) {
        val shown = _dialog.value ?: return
        if (_answering.value) return
        _answering.value = true
        _notice.value = null
        viewModelScope.launch {
            try {
                client.answerDialog(
                    project,
                    scope,
                    agent,
                    DialogAnswer(shown.id, choice, answers, message?.takeIf { it.isNotBlank() }),
                )
                if (_dialog.value?.id == shown.id) _dialog.value = null
            } catch (e: CancellationException) {
                throw e
            } catch (e: PmError.Refused) {
                _notice.value =
                    when (e.code) {
                        "answered" -> "Answered elsewhere"
                        "gone" ->
                            "The dialog can no longer be answered here; answer it at the terminal"
                        else -> e.message
                    }
                fetchDialog()
            } catch (e: Exception) {
                _notice.value = e.message ?: e.javaClass.simpleName
            } finally {
                _answering.value = false
            }
        }
    }

    private suspend fun recheckStarted() {
        val page =
            try {
                client.transcript(project, scope, agent)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                return
            }
        val shown = _chat.value as? ChatState.Shown ?: return
        if (shown.conversation.after != null) return
        val read = Conversation().replacedBy(Transcripts.items(page.items), page.before, page.after)
        _chat.value = shown.copy(conversation = read.appended(shown.conversation.items, page.after))
    }

    fun stop() {
        watching?.cancel()
        watching = null
        reconnecting?.cancel()
        reconnecting = null
    }

    /** Read the whole output `ref` names into [whole], unless it is held or being read. */
    fun readWhole(ref: String) {
        val held = _whole.value
        if (held?.ref == ref && held.body !is ReaderBody.Failed) return
        readingWhole?.cancel()
        _whole.value = WholeOutput(ref, ReaderBody.Loading)
        readingWhole = viewModelScope.launch {
            val body =
                try {
                    ReaderBody.Shown(client.toolResult(project, scope, agent, ref))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    ReaderBody.Failed(e.message ?: e.javaClass.simpleName) { readWhole(ref) }
                }
            _whole.value = WholeOutput(ref, body)
        }
    }

    fun closeWhole() {
        readingWhole?.cancel()
        _whole.value = null
    }

    /** Page the conversation back from its oldest item held. */
    fun older() {
        val held = conversation ?: return
        val before = held.before ?: return
        if (paging) return
        paging = true
        viewModelScope.launch {
            pageBack(before)
            paging = false
        }
    }

    /**
     * Prepend older pages from `before` until one adds an item or the conversation's start is
     * reached: a page can come back short, even empty, with older ones left.
     */
    private suspend fun pageBack(before: String) {
        var cursor = before
        repeat(MAX_EMPTY_PAGES) {
            val page =
                runCatching { client.transcript(project, scope, agent, cursor) }.getOrNull()
                    ?: return
            val shown = _chat.value as? ChatState.Shown ?: return
            if (shown.conversation.before != cursor) return
            val next = shown.conversation.prepended(Transcripts.items(page.items), page.before)
            _chat.value = shown.copy(conversation = next)
            if (next.items.size > shown.conversation.items.size) return
            cursor = page.before ?: return
        }
    }

    override fun onCleared() {
        stop()
    }

    private companion object {
        const val MAX_EMPTY_PAGES = 20
        val json = Json { ignoreUnknownKeys = true }
    }
}
