package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.Conversation
import dev.pm.app.model.TranscriptEvent
import dev.pm.app.model.Transcripts
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json

/** What the chat tab shows. */
sealed interface ChatState {
    data object Loading : ChatState

    data class Shown(val conversation: Conversation, val live: Boolean) : ChatState

    /** The server predates transcripts. */
    data object Unsupported : ChatState

    data class Failed(val reason: String) : ChatState
}

/**
 * One agent's conversation, kept current while its view is open: the latest page, then a watched
 * event stream from where that page ended, reopened at once when `networkChanges` emits.
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

    private val _screen = MutableStateFlow<Result<String>?>(null)
    val screen: StateFlow<Result<String>?> = _screen.asStateFlow()

    private var watching: Job? = null
    private var reconnecting: Job? = null
    private var screenPolling: Job? = null
    private var screenShown = false
    private var started = false
    private var paging = false

    private val conversation
        get() = (_chat.value as? ChatState.Shown)?.conversation

    /** Follow the conversation, and the screen if its tab shows, until [stop]. */
    fun start() {
        started = true
        if (screenShown && screenPolling?.isActive != true) pollScreen()
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
                _chat.value = ChatState.Unsupported
                return
            } catch (e: Exception) {
                val shown = _chat.value
                _chat.value =
                    if (shown is ChatState.Shown) shown.copy(live = false)
                    else ChatState.Failed(e.message ?: e.javaClass.simpleName)
            }
            delay(3.seconds)
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
                if (!shown.live) _chat.value = shown.copy(live = true)
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
        started = false
        watching?.cancel()
        watching = null
        reconnecting?.cancel()
        reconnecting = null
        screenPolling?.cancel()
        screenPolling = null
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

    suspend fun fullResult(ref: String): Result<String> = runCatching {
        client.toolResult(project, scope, agent, ref)
    }

    /** Read the agent's screen every few seconds while its tab shows. */
    fun watchScreen(on: Boolean) {
        screenShown = on
        screenPolling?.cancel()
        screenPolling = null
        if (on && started) pollScreen()
    }

    private fun pollScreen() {
        screenPolling = viewModelScope.launch {
            while (true) {
                _screen.value = runCatching { client.screen(project, scope, agent) }
                delay(3.seconds)
            }
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
