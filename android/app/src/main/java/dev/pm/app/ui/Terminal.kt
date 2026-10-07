package dev.pm.app.ui

import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.ui.text.style.TextOverflow
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

/**
 * A pane's rows as the terminal sheet shows them: without the blank rows below what was drawn, and
 * each run of blank rows between parts of the screen as one.
 */
fun screenRows(text: String): List<String> {
    val rows = text.lines().map { it.trimEnd() }.dropLastWhile { it.isEmpty() }
    return rows.filterIndexed { i, row -> row.isNotEmpty() || i == 0 || rows[i - 1].isNotEmpty() }
}

/**
 * Whether `row` is drawn mostly in box-drawing and block characters: a rule, perhaps labelled
 * (`──── main ─`), which wrapping repeats.
 */
fun isRule(row: String): Boolean {
    val drawn = row.codePoints().filter { it != ' '.code }.toArray()
    val box = drawn.count { it in 0x2500..0x259F }
    return drawn.isNotEmpty() && box >= drawn.size * 4 / 5
}

/**
 * [rule] in at most `columns` cells: its longest run of one character shortened, so a label on it
 * stays in view.
 */
fun fitRule(rule: String, columns: Int): String {
    val excess = rule.codePointCount(0, rule.length) - columns
    if (excess <= 0) return rule
    val runs = Regex("""(.)\1*""").findAll(rule)
    val longest = runs.filter { it.value[0] != ' ' }.maxByOrNull { it.value.length } ?: return rule
    val keep = maxOf(1, longest.value.length - excess)
    return rule.replaceRange(longest.range, longest.value.take(keep))
}

/**
 * An agent's pane at a readable size, each row wrapped to the width but a rule kept to one line;
 * pinch to zoom. It opens at the bottom, where a dialog's choices or the prompt are, and stays
 * there as the pane changes unless scrolled away.
 */
@Composable
internal fun TerminalView(text: String, modifier: Modifier = Modifier) {
    val rows = remember(text) { screenRows(text) }
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
    val size = TEXT_SP * zoom
    val style = terminalStyle(size.sp)
    val cell = rememberCellWidth()
    val padding = 8.dp
    BoxWithConstraints(modifier.transformable(zooming, canPan = { false })) {
        val width = constraints.maxWidth - with(LocalDensity.current) { (padding * 2).toPx() }
        val columns = (width / (cell * size)).toInt()
        Selectable {
            Column(Modifier.fillMaxSize().verticalScroll(vertical).padding(padding)) {
                for (row in rows) {
                    val rule = isRule(row)
                    Text(
                        if (rule) fitRule(row, columns) else row,
                        style = style,
                        softWrap = !rule,
                        maxLines = if (rule) 1 else Int.MAX_VALUE,
                        overflow = TextOverflow.Clip,
                    )
                }
            }
        }
    }
}

private const val TEXT_SP = 12f
private const val MIN_ZOOM = 0.5f
private const val MAX_ZOOM = 2.5f
