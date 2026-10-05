package dev.pm.app.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

sealed interface OutputState {
    data object Loading : OutputState

    data class Shown(val lines: List<String>, val columns: Int) : OutputState

    data class Failed(val reason: String) : OutputState
}

/** A tool's whole output (`ref`, a truncated result's `full`), read once and again on [retry]. */
class ToolOutputModel(
    private val client: PmClient?,
    private val project: String,
    private val scope: String,
    private val agent: String,
    private val ref: String,
) : ViewModel() {
    private val _uiState = MutableStateFlow<OutputState>(OutputState.Loading)
    val uiState: StateFlow<OutputState> = _uiState.asStateFlow()

    private var reading: Job? = null

    init {
        retry()
    }

    fun retry() {
        if (reading?.isActive == true) return
        _uiState.value = OutputState.Loading
        reading = viewModelScope.launch {
            _uiState.value =
                try {
                    val client = client ?: throw IllegalStateException("not paired")
                    val text = client.toolResult(project, scope, agent, ref)
                    withContext(Dispatchers.Default) {
                        val lines = outputLines(text)
                        OutputState.Shown(lines, lines.maxOfOrNull { it.length } ?: 0)
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    OutputState.Failed(e.message ?: e.javaClass.simpleName)
                }
        }
    }
}

/**
 * `text` as rows of at most `width` columns, tabs expanded to stops of eight: a longer line goes on
 * over the rows after it, so no row is wider than a screen can be laid out.
 */
fun outputLines(text: String, width: Int = MAX_OUTPUT_COLUMNS): List<String> =
    text.lines().flatMap { line ->
        val expanded =
            if ('\t' !in line) line
            else
                buildString {
                    line.forEach { c ->
                        if (c == '\t') repeat(TAB - length % TAB) { append(' ') } else append(c)
                    }
                }
        if (expanded.length <= width) listOf(expanded) else expanded.chunked(width)
    }

private const val TAB = 8
private const val MAX_OUTPUT_COLUMNS = 1000

/** The output a tool card's "Show all" opens: a line at a time, scrolled across together. */
@Composable
fun ToolOutputScreen(model: ToolOutputModel, modifier: Modifier = Modifier) {
    val state by model.uiState.collectAsStateWithLifecycle()
    when (val shown = state) {
        OutputState.Loading -> Centered(modifier) { CircularProgressIndicator() }
        is OutputState.Failed ->
            Retryable("Couldn't load the output: ${shown.reason}", model::retry, modifier)
        is OutputState.Shown -> {
            val size = 12.sp
            val cell = rememberCellWidth()
            val padding = 12.dp
            val width =
                with(LocalDensity.current) { (shown.columns * cell * size.value).toDp() } +
                    padding * 2
            Box(modifier.fillMaxSize().horizontalScroll(rememberScrollState())) {
                SelectionContainer {
                    LazyColumn(
                        Modifier.width(width).fillMaxSize(),
                        contentPadding = PaddingValues(padding),
                    ) {
                        items(shown.lines) { line ->
                            Text(line, style = terminalStyle(size), softWrap = false, maxLines = 1)
                        }
                    }
                }
            }
        }
    }
}
