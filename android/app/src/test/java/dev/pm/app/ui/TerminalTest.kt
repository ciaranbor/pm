package dev.pm.app.ui

import androidx.activity.ComponentActivity
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.doubleClick
import androidx.compose.ui.test.getBoundsInRoot
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.pinch
import androidx.compose.ui.test.swipeDown
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
class TerminalTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    /** The drawn screen's width in pixels, unclipped by the view, which holds the given text. */
    private fun gridWidth(text: String): Float =
        compose
            .onNode(SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf(text.asText())))
            .fetchSemanticsNode()
            .size
            .width
            .toFloat()

    private fun String.asText() = androidx.compose.ui.text.AnnotatedString(this)

    /** At a phone's density, not the 1× that screenshots render at. */
    @Test
    @Config(qualifiers = "w360dp-h640dp-440dpi")
    @GraphicsMode(GraphicsMode.Mode.NATIVE)
    fun an_80_column_pane_fits_the_width_and_a_wide_character_takes_two_cells() {
        val ascii = "a".repeat(80)
        var text by mutableStateOf(ascii)
        compose.setContent { PmTheme { TerminalView(listOf(text)) } }
        val screen = compose.onRoot().fetchSemanticsNode().size.width
        val fitted = gridWidth(ascii)
        assertTrue("$fitted fills $screen", fitted <= screen && fitted > screen * 0.95f)

        val wide = "字".repeat(30) + "🙂".repeat(10)
        text = wide
        assertEquals(fitted, gridWidth(wide), 2f)
    }

    @Test
    @Config(qualifiers = "w360dp-h640dp-440dpi")
    @GraphicsMode(GraphicsMode.Mode.NATIVE)
    fun pinching_or_a_double_tap_zooms_from_the_fit_and_never_below_it() {
        val ascii = "a".repeat(80)
        compose.setContent { PmTheme { TerminalView(listOf(ascii)) } }
        val fitted = gridWidth(ascii)
        val view = compose.onRoot()
        fun pinch(from: Float, to: Float) = view.performTouchInput {
            pinch(
                center - Offset(from, 0f),
                center - Offset(to, 0f),
                center + Offset(from, 0f),
                center + Offset(to, 0f),
            )
        }

        pinch(50f, 200f)
        assertTrue("${gridWidth(ascii)} grew past $fitted", gridWidth(ascii) > fitted * 1.5f)

        pinch(300f, 20f)
        pinch(300f, 20f)
        assertEquals(fitted, gridWidth(ascii), 2f)

        view.performTouchInput { doubleClick(center) }
        assertTrue("double tap zooms in", gridWidth(ascii) > fitted * 1.2f)
        view.performTouchInput { doubleClick(center) }
        assertEquals(fitted, gridWidth(ascii), 2f)
    }

    @Test
    @Config(qualifiers = "w360dp-h640dp-440dpi")
    @GraphicsMode(GraphicsMode.Mode.NATIVE)
    fun a_double_tap_zooms_about_the_tapped_point() {
        val rows = List(100) { "%03d ".format(it) + "x".repeat(76) }
        compose.setContent { PmTheme { TerminalView(rows) } }
        val grid =
            compose.onNode(
                SemanticsMatcher.expectValue(
                    SemanticsProperties.Text,
                    listOf(rows.joinToString("\n").asText()),
                )
            )
        val view = compose.onRoot().fetchSemanticsNode().size
        val tap = Offset(view.width * 0.75f, view.height * 0.4f)
        val padding = with(compose.density) { Spacing.s.toPx() }
        /** Where the tapped point is on the grid, as a fraction of its width and height. */
        fun under(): Offset {
            val node = grid.fetchSemanticsNode()
            val at = tap - node.positionInRoot - Offset(padding, padding)
            return Offset(
                at.x / (node.size.width - 2 * padding),
                at.y / (node.size.height - 2 * padding),
            )
        }
        val before = under()
        val width = grid.fetchSemanticsNode().size.width

        compose.onRoot().performTouchInput { doubleClick(tap) }
        compose.waitForIdle()

        assertTrue("zoomed in", grid.fetchSemanticsNode().size.width > width * 1.2f)
        val after = under()
        assertEquals("column under the tap", before.x * 80, after.x * 80, 1.5f)
        assertEquals("row under the tap", before.y * 100, after.y * 100, 1.5f)
    }

    @Test
    fun the_screen_drops_the_blank_rows_below_it_and_collapses_those_between() {
        assertEquals(
            listOf("menu", "", "> 1. Yes", "", "input"),
            screenRows("menu\n   \n\n> 1. Yes   \n\n\n\ninput\n\n  \n"),
        )
    }

    @Test
    fun cells_place_wide_characters_on_two_columns_and_marks_on_none() {
        assertEquals(
            listOf(Cell(0, "a", 1), Cell(1, "字", 2), Cell(3, "é", 1), Cell(4, "🙂", 2)),
            cells("a字é🙂"),
        )
        assertEquals(6, columns("a字é🙂"))
    }

    @Test
    fun a_link_the_pane_wrapped_is_joined_and_a_box_border_left_out() {
        val rows =
            listOf(
                "╭──────────────────────────╮",
                "│ Use the url below:       │",
                "│ https://claude.ai/oauth/a│",
                "│ uthorize?code=true&state=│",
                "│ abc123                   │",
                "│ Paste code here >        │",
                "╰──────────────────────────╯",
                "See https://example.com/docs. and https://pm.dev/x",
                "next words",
                "─".repeat(60),
            )
        assertEquals(
            listOf(
                "https://claude.ai/oauth/authorize?code=true&state=abc123",
                "https://example.com/docs",
                "https://pm.dev/x",
            ),
            screenLinks(rows),
        )
    }

    @Test
    fun a_link_ending_inside_a_box_as_wide_as_the_screen_stops_there() {
        val rows =
            listOf(
                "╭──────────────────────────╮",
                "│ https://claude.ai/oauth/a│",
                "│ uthorize?code=abc123     │",
                "│ Paste code here >        │",
                "╰──────────────────────────╯",
            )
        assertEquals(listOf("https://claude.ai/oauth/authorize?code=abc123"), screenLinks(rows))
    }

    @Test
    fun a_wrapped_link_ends_where_its_last_row_goes_on_in_words() {
        val link = "https://example.com/path?one=" + "1234567890".repeat(10)
        val other = "https://pm.dev/" + "y".repeat(20)
        val text = "⏺ $link and $other too"
        val rows = listOf(text.take(79)) + text.drop(79).chunked(77).map { "  $it" }
        assertEquals(
            listOf(link, "https://pm.dev/" + "y".repeat(20)),
            screenLinks(rows + "─".repeat(80)),
        )
    }

    @OptIn(ExperimentalMaterial3Api::class)
    @Test
    fun a_drag_down_on_the_screen_leaves_the_sheet_open() {
        var open by mutableStateOf(true)
        compose.setContent {
            PmTheme {
                if (open) {
                    ModalBottomSheet(
                        onDismissRequest = { open = false },
                        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
                    ) {
                        ScreenPanel("❯ 1. Yes", null, emptyList(), press = {}, type = { true })
                    }
                }
            }
        }
        val screen =
            compose.onNode(
                SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf("❯ 1. Yes".asText()))
            )
        screen.performTouchInput { swipeDown(durationMillis = 300) }
        compose.waitForIdle()
        assertTrue("the sheet closed", open)
        screen.assertIsDisplayed()
    }

    private fun panel(pressed: MutableList<String>) = compose.setContent {
        PmTheme {
            ScreenPanel("❯ 1. Yes", null, emptyList(), press = { pressed += it }, type = { true })
        }
    }

    @Test
    fun ctrl_holds_for_the_next_key_on_a_tap_and_until_tapped_again_on_a_long_press() {
        val pressed = mutableListOf<String>()
        panel(pressed)
        val ctrl = compose.onNodeWithContentDescription("Control")
        val up = compose.onNodeWithContentDescription("Up")

        ctrl.performClick()
        up.performClick()
        up.performClick()
        ctrl.performClick()
        compose.onNodeWithContentDescription("Escape").performClick()
        up.performClick()
        assertEquals(listOf("C-Up", "Up", "Escape", "Up"), pressed)

        pressed.clear()
        ctrl.performTouchInput { longClick() }
        compose.onNodeWithText("Ctrl + a letter").performTextInput("r")
        up.performClick()
        ctrl.performClick()
        up.performClick()
        assertEquals(listOf("C-r", "C-Up", "Up"), pressed)
    }

    @Test
    fun digits_sit_behind_123_on_the_one_row_of_keys() {
        val pressed = mutableListOf<String>()
        panel(pressed)
        compose.onNodeWithContentDescription("1").assertDoesNotExist()
        val row = compose.onNodeWithContentDescription("Escape").getBoundsInRoot().top
        assertEquals(row, compose.onNodeWithContentDescription("Enter").getBoundsInRoot().top)

        compose.onNodeWithContentDescription("Digits").performClick()
        compose.onNodeWithContentDescription("1").assertIsDisplayed().performClick()
        assertEquals(row, compose.onNodeWithContentDescription("1").getBoundsInRoot().top)
        assertEquals(listOf("1"), pressed)
    }
}
