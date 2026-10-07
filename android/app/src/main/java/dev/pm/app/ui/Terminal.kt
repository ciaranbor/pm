package dev.pm.app.ui

import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.Velocity
import androidx.compose.ui.unit.sp
import dev.pm.app.R
import kotlin.math.abs
import kotlin.math.roundToInt

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
 * An agent's pane, as [screenRows] gives it, drawn cell for cell at its own width, so a TUI keeps
 * its grid: it opens fitted to the view's width, pinching or a double tap zooms about the fingers,
 * and a drag pans. It opens at the bottom, where a dialog's choices or the prompt are, and stays
 * there as the pane changes unless scrolled away.
 */
@Composable
internal fun TerminalView(rows: List<String>, modifier: Modifier = Modifier) {
    val measurer = rememberTextMeasurer()
    val grid = remember(rows, measurer) { Grid(rows, measurer) }
    val color = MaterialTheme.colorScheme.onSurface
    val density = LocalDensity.current
    val padding = with(density) { Spacing.s.toPx() }
    var zoom by rememberSaveable { mutableFloatStateOf(1f) }
    val vertical = rememberScrollState()
    val horizontal = rememberScrollState()
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
    BoxWithConstraints(modifier.clipToBounds()) {
        val fit =
            minOf(
                (constraints.maxWidth - 2 * padding) / grid.width,
                FIT_MAX_SP / BASE_SP,
            )
        val most = maxOf(1f, MAX_SP / (BASE_SP * fit))
        val scale = fit * zoom.coerceIn(1f, most)
        // Where the scroll should be once a zoom is laid out, to keep the point under the fingers.
        var anchored by remember { mutableStateOf<Offset?>(null) }
        fun zoomTo(target: Float, about: Offset) {
            val before = fit * zoom.coerceIn(1f, most)
            zoom = target.coerceIn(1f, most)
            val change = fit * zoom / before
            val from = anchored ?: Offset(horizontal.value.toFloat(), vertical.value.toFloat())
            anchored = (from + about) * change - about
        }
        LaunchedEffect(anchored) {
            val to = anchored ?: return@LaunchedEffect
            withFrameNanos {}
            horizontal.scrollTo(to.x.roundToInt())
            vertical.scrollTo(to.y.roundToInt())
            anchored = null
        }
        val pinched by rememberUpdatedState { centroid: Offset, change: Float ->
            zoomTo(zoom * change, centroid)
        }
        Box(
            Modifier.fillMaxSize()
                .pinchZoom { centroid, change -> pinched(centroid, change) }
                .nestedScroll(KeepScroll)
                .pointerInput(fit, most) {
                    detectTapGestures(
                        onDoubleTap = { at ->
                            val read = READ_SP / (BASE_SP * fit)
                            zoomTo(if (zoom < read * 0.9f) read else 1f, at)
                        }
                    )
                }
                .verticalScroll(vertical)
                .horizontalScroll(horizontal)
        ) {
            val shown = remember(rows) { rows.joinToString("\n") }
            Spacer(
                Modifier.size(
                        with(density) { (grid.width * scale + 2 * padding).toDp() },
                        with(density) { (grid.height * scale + 2 * padding).toDp() },
                    )
                    .semantics { this.text = AnnotatedString(shown) }
                    .drawBehind {
                        withTransform({
                            translate(padding, padding)
                            scale(scale, scale, Offset.Zero)
                        }) {
                            grid.draw(this, color)
                        }
                    }
            )
        }
    }
}

/**
 * Keeps what the view's scrolling leaves over, so a drag on the screen pans it and never drags the
 * sheet it sits in down.
 */
private object KeepScroll : NestedScrollConnection {
    override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource) =
        available

    override suspend fun onPostFling(consumed: Velocity, available: Velocity) = available
}

/**
 * Two fingers zoom, about their centre. The gesture is taken before the scrolling inside sees it,
 * else a zoomed view's scroll claims the fingers' movement and a pinch can't zoom back out.
 */
private fun Modifier.pinchZoom(zoom: (centroid: Offset, change: Float) -> Unit) =
    pointerInput(Unit) {
        awaitEachGesture {
            awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
            do {
                val event = awaitPointerEvent(PointerEventPass.Initial)
                if (event.changes.count { it.pressed } >= 2) {
                    val change = event.calculateZoom()
                    if (change != 1f) zoom(event.calculateCentroid(useCurrent = true), change)
                    event.changes.forEach { if (it.positionChanged()) it.consume() }
                }
            } while (event.changes.any { it.pressed })
        }
    }

/**
 * [rows] laid out on a grid of [TerminalFont] cells at [BASE_SP]. A run of characters the font
 * draws one cell wide is laid out as one text; any other character alone, centred on its cells and
 * shrunk to fit them.
 */
private class Grid(rows: List<String>, measurer: TextMeasurer) {
    private class Glyph(val at: Offset, val text: TextLayoutResult, val scale: Float)

    private val style = terminalStyle(BASE_SP.sp)
    private val cell: Float
    private val line: Float
    private val glyphs = mutableListOf<Glyph>()
    val width: Float
    val height: Float

    init {
        val sample = 100
        val zeros = measurer.measure("0".repeat(sample), style, softWrap = false)
        cell = zeros.size.width.toFloat() / sample
        line = zeros.size.height.toFloat()
        val widths = HashMap<String, Int>()
        fun fits(c: Cell) =
            c.width == 1 &&
                (c.text.length == 1 && c.text[0].code in 0x20..0x7E ||
                    abs(widths.getOrPut(c.text) { measure(measurer, c.text).size.width } - cell) <=
                        1f)
        var columns = 1
        rows.forEachIndexed { y, row ->
            val cells = cells(row)
            columns = maxOf(columns, cells.lastOrNull()?.let { it.column + it.width } ?: 0)
            var i = 0
            while (i < cells.size) {
                val start = cells[i]
                val top = y * line
                if (fits(start)) {
                    var end = i + 1
                    while (end < cells.size && fits(cells[end])) end++
                    val run = cells.subList(i, end).joinToString("") { it.text }
                    if (run.isNotBlank()) {
                        glyphs +=
                            Glyph(Offset(start.column * cell, top), measure(measurer, run), 1f)
                    }
                    i = end
                } else {
                    val text = measure(measurer, start.text)
                    val room = start.width * cell
                    val scale = minOf(1f, room / text.size.width.coerceAtLeast(1))
                    val left = start.column * cell + (room - text.size.width * scale) / 2
                    val down = top + (line - text.size.height * scale) / 2
                    glyphs += Glyph(Offset(left, down), text, scale)
                    i++
                }
            }
        }
        width = columns * cell
        height = maxOf(1, rows.size) * line
    }

    private fun measure(measurer: TextMeasurer, text: String) =
        measurer.measure(text, style, softWrap = false, maxLines = 1)

    fun draw(scope: DrawScope, color: Color) =
        with(scope) {
            for (glyph in glyphs) {
                if (glyph.scale == 1f) {
                    drawText(glyph.text, color, glyph.at)
                } else {
                    scale(glyph.scale, glyph.at) { drawText(glyph.text, color, glyph.at) }
                }
            }
        }
}

/** The size the grid is laid out at, before it is scaled to the view. */
private const val BASE_SP = 12f

/** The largest a fitted pane draws, so a narrow one isn't blown up. */
private const val FIT_MAX_SP = 14f

/** The size a double tap zooms to. */
private const val READ_SP = 12f

private const val MAX_SP = 32f
