package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * An agent's pane, for answering at the phone a dialog no hook reports: its screen, read every
 * second until [stop] (the server has no stream of it) and again after each send, and keys and text
 * typed into it, in the order they were asked for.
 */
class ScreenModel(
    private val client: PmClient,
    private val project: String,
    private val scope: String,
    private val agent: String,
) : ViewModel() {
    private val _screen = MutableStateFlow<String?>(null)
    val screen: StateFlow<String?> = _screen.asStateFlow()

    /** Why the last read or send failed; cleared by the next send, or [start]. */
    private val _notice = MutableStateFlow<String?>(null)
    val notice: StateFlow<String?> = _notice.asStateFlow()

    private val sending = Mutex()
    private var polling: Job? = null

    fun start() {
        if (polling?.isActive == true) return
        _notice.value = null
        polling = viewModelScope.launch {
            while (true) {
                refresh()
                delay(1.seconds)
            }
        }
    }

    fun stop() {
        polling?.cancel()
        polling = null
    }

    /** The keys pressed and not yet sent, in order; a key pressed twice is here twice. */
    private val _pressing = MutableStateFlow<List<String>>(emptyList())
    val pressing: StateFlow<List<String>> = _pressing.asStateFlow()

    /** Press `key`, by tmux's name for it. */
    fun press(key: String) {
        _pressing.update { it + key }
        viewModelScope.launch {
            try {
                send { client.pressKeys(project, scope, agent, listOf(key)) }
            } finally {
                _pressing.update { it - key }
            }
        }
    }

    /** Type `text` with nothing pressed after it; whether it was typed. */
    suspend fun type(text: String): Boolean = send { client.typeText(project, scope, agent, text) }

    private suspend fun send(action: suspend () -> Unit): Boolean {
        val sent = sending.withLock {
            _notice.value = null
            try {
                action()
                true
            } catch (e: CancellationException) {
                throw e
            } catch (e: PmError.Unsupported) {
                _notice.value = e.advice("type here")
                false
            } catch (e: Exception) {
                _notice.value = e.message ?: e.javaClass.simpleName
                false
            }
        }
        refresh()
        return sent
    }

    private suspend fun refresh() {
        try {
            _screen.value = client.screen(project, scope, agent)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            if (_screen.value == null) _notice.value = "Couldn't read the screen: ${e.message}"
        }
    }

    override fun onCleared() {
        stop()
    }
}
