package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.AgentMode
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.api.reason
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch

/** How a switch of mode went. */
sealed interface ModeSwitched {
    data class Done(val label: String) : ModeSwitched

    /** The server refused it; `text` says why, for the user. */
    data class Refused(val text: String) : ModeSwitched

    /** It didn't reach the server; [ModeModel.switch] to `id` again may. */
    data class Failed(val id: String, val reason: String) : ModeSwitched
}

/**
 * An agent's permission mode: read when [refresh] asks, and again after each switch. [mode] stays
 * null for an agent whose mode pm doesn't switch, so nothing offers to.
 */
class ModeModel(
    private val client: PmClient,
    private val project: String,
    private val scope: String,
    private val agent: String,
) : ViewModel() {
    private val _mode = MutableStateFlow<AgentMode?>(null)
    val mode: StateFlow<AgentMode?> = _mode.asStateFlow()

    /** The id of the mode a switch is on its way to. */
    private val _switching = MutableStateFlow<String?>(null)
    val switching: StateFlow<String?> = _switching.asStateFlow()

    private val _switched = Channel<ModeSwitched>(Channel.BUFFERED)

    /** How each switch went, once. */
    val switched: Flow<ModeSwitched> = _switched.receiveAsFlow()

    private var reading: Job? = null

    fun refresh() {
        if (_switching.value != null) return
        reading?.cancel()
        reading = viewModelScope.launch { read() }
    }

    private suspend fun read() {
        try {
            _mode.value = client.mode(project, scope, agent)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            // Keep what was read: a failed read hides nothing that was offered.
        }
    }

    /** Put the agent in the mode `id`. */
    fun switch(id: String) {
        if (_switching.value != null) return
        reading?.cancel()
        _switching.value = id
        val label = _mode.value?.modes?.find { it.id == id }?.label ?: id
        viewModelScope.launch {
            val outcome =
                try {
                    client.setMode(project, scope, agent, id)
                    ModeSwitched.Done(label)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Refused) {
                    ModeSwitched.Refused(refusal(e, label))
                } catch (e: Exception) {
                    ModeSwitched.Failed(id, e.reason())
                }
            read()
            _switching.value = null
            _switched.send(outcome)
        }
    }

    private fun refusal(e: PmError.Refused, label: String): String =
        when (e.code) {
            "asking" -> "Answer the dialog first"
            "unavailable" -> "$label isn't available for $agent"
            "mode-unknown" -> "Can't see $agent's mode; check its terminal"
            "unchanged" -> "$agent's mode didn't change"
            "not-running",
            "no-window",
            "inactive" -> "$agent isn't running"
            else -> e.message ?: e.code
        }
}
