package dev.pm.app.ui

import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.pm.app.R

/**
 * A monospace font for terminal text: a subset of FiraCode Nerd Font Mono, so box drawing and the
 * Nerd Font glyphs pm's badges use draw in the same cell width as letters (system fonts lack the
 * glyphs and fall back to other widths). Its licence ships in `assets/terminal-font-license.txt`.
 */
val TerminalFont = FontFamily(Font(R.font.terminal))

/** The style of terminal text at `size`. */
fun terminalStyle(size: TextUnit) =
    TextStyle(fontFamily = TerminalFont, fontSize = size, lineHeight = size * 1.2f)

/** One cell's width in [TerminalFont], in pixels per sp of font size. */
@Composable
fun rememberCellWidth(): Float {
    val measurer = rememberTextMeasurer()
    val density = LocalDensity.current
    return remember(measurer, density) {
        val sample = 100
        val size = 10f
        measurer.measure("0".repeat(sample), terminalStyle(size.sp)).size.width.toFloat() /
            sample /
            size
    }
}

/** A pane's text as the screen tab shows it: without the blank rows below what was drawn. */
fun screenLines(text: String): List<String> = text.lines().dropLastWhile { it.isBlank() }

/** The columns `lines` fill, trailing spaces aside, and never fewer than [MIN_COLUMNS]. */
fun screenColumns(lines: List<String>): Int =
    maxOf(
        MIN_COLUMNS,
        lines.maxOfOrNull { it.trimEnd().let { l -> l.codePointCount(0, l.length) } } ?: 0,
    )

private const val MIN_COLUMNS = 20

/**
 * An agent's pane: sized so its widest row fits the width, then pinch to zoom. It opens at the
 * bottom, where the prompt is, and stays there as the pane changes unless scrolled away.
 */
@Composable
internal fun TerminalView(text: String, modifier: Modifier = Modifier) {
    val lines = remember(text) { screenLines(text) }
    val columns = remember(lines) { screenColumns(lines) }
    val cell = rememberCellWidth()
    val padding = 8.dp
    var zoom by rememberSaveable { mutableFloatStateOf(1f) }
    val zooming = rememberTransformableState { change, _, _ ->
        zoom = (zoom * change).coerceIn(MIN_ZOOM, MAX_ZOOM)
    }
    val vertical = rememberScrollState()
    var follow by rememberSaveable { mutableStateOf(true) }
    LaunchedEffect(vertical) {
        followEnd(
            position = { vertical.value },
            behind = { vertical.canScrollForward },
            following = { follow },
            setFollowing = { follow = it },
            toEnd = { vertical.scrollTo(vertical.maxValue) },
        )
    }
    BoxWithConstraints(modifier.fillMaxSize().transformable(zooming, canPan = { false })) {
        val width = constraints.maxWidth - with(LocalDensity.current) { (padding * 2).toPx() }
        val fit = width / (columns * cell)
        val size = (fit * zoom).coerceIn(MIN_SP, MAX_SP).sp
        SelectionContainer {
            Text(
                lines.joinToString("\n"),
                style = terminalStyle(size),
                softWrap = false,
                modifier =
                    Modifier.fillMaxSize()
                        .verticalScroll(vertical)
                        .horizontalScroll(rememberScrollState())
                        .padding(padding),
            )
        }
    }
}

private const val MIN_ZOOM = 0.5f
private const val MAX_ZOOM = 4f
private const val MIN_SP = 4f
private const val MAX_SP = 32f
