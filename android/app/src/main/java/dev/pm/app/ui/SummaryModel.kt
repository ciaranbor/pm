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

sealed interface SummaryState {
    data object Loading : SummaryState

    data class Shown(val markdown: String) : SummaryState

    data object Missing : SummaryState

    data object Unreachable : SummaryState

    data class Failed(val reason: String) : SummaryState
}

/** A feature's summary, read once and again on [retry]. */
class SummaryModel(
    private val client: PmClient?,
    private val project: String,
    private val feature: String,
) : ViewModel() {
    private val _uiState = MutableStateFlow<SummaryState>(SummaryState.Loading)
    val uiState: StateFlow<SummaryState> = _uiState.asStateFlow()

    private var reading: Job? = null

    init {
        retry()
    }

    fun retry() {
        if (reading?.isActive == true) return
        _uiState.value = SummaryState.Loading
        reading = viewModelScope.launch {
            _uiState.value =
                try {
                    val client = client ?: throw IllegalStateException("not paired")
                    SummaryState.Shown(client.summary(project, feature))
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Status) {
                    if (e.code == 404) SummaryState.Missing
                    else SummaryState.Failed(e.message ?: "HTTP ${e.code}")
                } catch (e: PmError.Unreachable) {
                    SummaryState.Unreachable
                } catch (e: Exception) {
                    SummaryState.Failed(e.message ?: e.javaClass.simpleName)
                }
        }
    }
}
