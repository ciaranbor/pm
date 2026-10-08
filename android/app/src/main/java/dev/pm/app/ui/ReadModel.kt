package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

sealed interface ReadState<out T> {
    data object Loading : ReadState<Nothing>

    data class Shown<T>(val value: T) : ReadState<T>

    /** The server has no such thing: a 404 other than an unknown endpoint. */
    data object Missing : ReadState<Nothing>

    data object Unreachable : ReadState<Nothing>

    data class Failed(val reason: String) : ReadState<Nothing>
}

/** What `read` asks of the server, read once and again on [retry]. */
class ReadModel<T>(private val client: PmClient?, private val read: suspend PmClient.() -> T) :
    ViewModel() {
    private val _uiState = MutableStateFlow<ReadState<T>>(ReadState.Loading)
    val uiState: StateFlow<ReadState<T>> = _uiState.asStateFlow()

    private var reading: Job? = null

    init {
        retry()
    }

    private var shownBefore = false

    /**
     * Its screen is shown: the first time, as read at creation; after that, read again behind what
     * is shown, since it may have changed while another page was.
     */
    fun shown() {
        if (shownBefore) refresh()
        shownBefore = true
    }

    /** Read again behind what is shown, replacing it once read; a failure leaves it. */
    private fun refresh() {
        if (reading?.isActive == true || _uiState.value !is ReadState.Shown) return
        val client = client ?: return
        reading = viewModelScope.launch {
            try {
                _uiState.value = ReadState.Shown(client.read())
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {}
        }
    }

    fun retry() {
        if (reading?.isActive == true) return
        _uiState.value = ReadState.Loading
        reading = viewModelScope.launch {
            _uiState.value =
                try {
                    val client = client ?: throw IllegalStateException("not paired")
                    ReadState.Shown(client.read())
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Status) {
                    if (e.code == 404) ReadState.Missing
                    else ReadState.Failed(e.message ?: "HTTP ${e.code}")
                } catch (e: PmError.Unreachable) {
                    ReadState.Unreachable
                } catch (e: Exception) {
                    ReadState.Failed(e.message ?: e.javaClass.simpleName)
                }
        }
    }
}
