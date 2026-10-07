package dev.pm.app.ui

import androidx.activity.ComponentActivity
import androidx.compose.foundation.text.selection.SelectionState
import androidx.compose.foundation.text.selection.rememberSelectionState
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertContentDescriptionContains
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasScrollToIndexAction
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.text.TextLayoutResult
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.ui.NavDisplay
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.model.AgentState
import dev.pm.app.model.Conversation
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.Item
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import dev.pm.app.model.ToolResult
import dev.pm.app.push.Target
import java.time.Instant
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
class UiTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    @After
    fun tearDown() {
        scope.cancel()
    }

    private fun model(): AppViewModel {
        val store = Store(ApplicationProvider.getApplicationContext())
        // Nothing listens on the discard port: the server is unreachable, as off the tailnet.
        store.pairing = Pairing("http://127.0.0.1:9", "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        return AppViewModel(Repository(store, OkHttpClient(), scope)) {}
    }

    private fun back() = compose.runOnUiThread {
        compose.activity.onBackPressedDispatcher.onBackPressed()
    }

    @Test
    @Config(qualifiers = "w360dp-h640dp-440dpi")
    fun a_notification_opens_its_workspace_over_what_was_shown_and_back_returns_there() {
        val model = model()
        var target by mutableStateOf<Target?>(null)
        compose.setContent { PmTheme { App(model, target, targetShown = { target = null }) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }
        compose.onNodeWithContentDescription("app, 2 features", substring = true).performClick()
        compose.onNodeWithText("Notes").assertIsDisplayed()

        target = Target("app", "login", "implementer")
        compose.onNodeWithText("login").assertIsDisplayed()
        compose.onNodeWithText("Updated just now", substring = true).assertIsDisplayed()
        compose.onNodeWithContentDescription("implementer asking, 2 unread").assertIsDisplayed()
        compose
            .onNodeWithText("A dialog is up at the terminal: Postgres or SQLite?")
            .assertIsDisplayed()
        assertEquals(null, target)

        back()
        compose.onNodeWithText("Notes").assertIsDisplayed()
    }

    @Test
    fun a_need_on_the_start_screen_opens_its_workspace_and_back_returns_to_the_start() {
        val model = model()
        compose.setContent { PmTheme { App(model, null, targetShown = {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        compose
            .onNodeWithContentDescription("login, app, blocked · ", substring = true)
            .assertContentDescriptionContains("which DB?", substring = true)
            .assertContentDescriptionContains("2 unread", substring = true)
            .performClick()
        compose.onNodeWithText("login").assertIsDisplayed()
        compose
            .onNodeWithText("A dialog is up at the terminal: Postgres or SQLite?")
            .assertIsDisplayed()

        back()
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun up_from_a_workspace_opened_from_the_start_screen_goes_to_its_project() {
        val model = model()
        compose.setContent { PmTheme { App(model, Target("app", "login", null), {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        compose.onNodeWithContentDescription("Navigate up").performClick()
        compose.onNodeWithText("Notes").assertIsDisplayed()
        back()
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun a_workspace_has_a_tab_per_agent_then_its_features_pages() {
        val model = model()
        compose.setContent { PmTheme { App(model, Target("app", "login", null), {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        compose.onNodeWithText("Message the agent").assertIsDisplayed()
        compose.onNodeWithText("Summary").performClick()
        compose.onNodeWithText("which DB?").assertIsDisplayed()
        compose.onNodeWithText("Message the agent").assertDoesNotExist()
        for (page in listOf("Brief", "Details")) {
            compose.onNodeWithText(page).performClick()
            compose.onNodeWithText("Can't reach pm serve").assertIsDisplayed()
        }
        compose.onNodeWithContentDescription("implementer", substring = true).performClick()
        compose.onNodeWithText("Message the agent").assertIsDisplayed()
    }

    @Test
    fun short_markdown_shows_formatted_from_its_first_frame() {
        compose.setContent { PmTheme { PmMarkdown("# Title\n\n- one\n- two") } }
        compose.onNodeWithText("Title").assertIsDisplayed()
        compose.onNodeWithText("# Title", substring = true).assertDoesNotExist()
    }

    @Test
    fun a_plain_text_brief_keeps_its_line_breaks() {
        val client = PmClient(Pairing("http://127.0.0.1:9", "pixel", "tok"))
        val brief = FeatureInfo(context = "Goal: search\nConstraint: no new deps")
        compose.setContent { PmTheme { BriefScreen(viewModel { ReadModel(client) { brief } }) } }
        compose.waitUntil(10_000) {
            compose
                .onAllNodesWithText("Goal: search\nConstraint: no new deps")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    @Test
    fun a_ready_features_header_says_so_rather_than_repeating_its_summary() {
        val ready =
            Snapshot.parse(SNAPSHOT)
                .feature("app", "search")!!
                .copy(summary = "Adds search", pr = "12")
        val client = PmClient(Pairing("http://127.0.0.1:9", "pixel", "tok"))
        compose.setContent {
            PmTheme {
                SummaryTab(
                    ready,
                    viewModel { ReadModel(client) { "# Search" } },
                    Instant.parse("2026-10-02T10:00:00Z"),
                    stale = false,
                    merging = false,
                    busy = false,
                    merge = {},
                )
            }
        }
        compose.onNodeWithText("Ready for review").assertIsDisplayed()
        compose.onNodeWithText("Adds search").assertDoesNotExist()
        compose.onNodeWithText("PR #12 open").assertIsDisplayed()
    }

    @Test
    fun a_notification_for_the_agent_shown_before_switches_back_to_it() {
        val model = model()
        var target by mutableStateOf<Target?>(Target("app", "login", "implementer"))
        compose.setContent { PmTheme { App(model, target, targetShown = { target = null }) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }
        compose.onNodeWithText("Message the agent").assertIsDisplayed()

        compose.onNodeWithText("Summary").performClick()
        compose.onNodeWithText("Message the agent").assertDoesNotExist()
        target = Target("app", "login", "implementer")
        compose.onNodeWithText("Message the agent").assertIsDisplayed()
        back()
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun back_hides_the_full_name_before_it_leaves() {
        val model = model()
        compose.setContent { PmTheme { App(model, Target("app", "login", null), {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        compose.onNodeWithText("login").performClick()
        compose.onNodeWithText("login\napp").assertIsDisplayed()
        back()
        compose.onNodeWithText("login\napp").assertDoesNotExist()
        compose.onNodeWithText("Message the agent").assertIsDisplayed()

        // On the start screen, with nothing to go back to, the title's own handler hides it.
        back()
        compose.onNodeWithText("pm").performClick()
        assertEquals(2, compose.onAllNodesWithText("pm").fetchSemanticsNodes().size)
        back()
        assertEquals(1, compose.onAllNodesWithText("pm").fetchSemanticsNodes().size)
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun with_text_selected_back_clears_the_selection_and_nothing_else() {
        val stack = NavBackStack<NavKey>(Route.Home, Route.Settings)
        lateinit var selection: SelectionState
        compose.setContent {
            PmTheme {
                NavDisplay(
                    stack,
                    entryProvider =
                        entryProvider {
                            entry<Route.Home> { Text("home") }
                            entry<Route.Settings> {
                                selection = rememberSelectionState()
                                Selectable(state = selection) { Text("select these words") }
                            }
                        },
                )
            }
        }
        compose.runOnUiThread { selection.selectAll() }
        compose.waitForIdle()
        assertTrue(selection.selectedTexts.any { it.isNotEmpty() })
        back()
        compose.waitForIdle()
        assertEquals(2, stack.size)
        assertFalse(selection.selectedTexts.any { it.isNotEmpty() })
        back()
        compose.waitForIdle()
        assertEquals(1, stack.size)
    }

    @Test
    fun a_revoked_phone_with_nothing_cached_is_offered_pairing_rather_than_a_spinner() {
        val connection = MutableStateFlow<Connection>(Connection.Unauthorized)
        compose.setContent {
            PmTheme {
                val state by connection.collectAsState()
                Shown(null, state, retry = {}, pairAgain = {}) {}
            }
        }
        compose.onNodeWithText("Pair again").assertIsDisplayed()
        connection.value = Connection.Unreachable("refused")
        compose.onNodeWithText("Retry").assertIsDisplayed()
    }

    @Test
    fun a_retry_from_the_status_strip_shows_progress_then_the_outcome() {
        var pairings = 0
        val connection = MutableStateFlow<Connection>(Connection.Unreachable("refused"))
        val retry = { connection.value = Connection.Connecting }
        compose.mainClock.autoAdvance = false
        compose.setContent {
            PmTheme {
                val state by connection.collectAsState()
                StatusStrip(state, retry = retry, pairAgain = { pairings++ })
            }
        }
        compose.mainClock.advanceTimeByFrame()
        compose.onNodeWithText("Offline").assertIsDisplayed()

        compose.onNodeWithText("Retry").performClick()
        connection.value = Connection.Unreachable("refused")
        compose.mainClock.advanceTimeBy(100)
        compose.onNodeWithText("Connecting…").assertIsDisplayed()
        compose.onNode(hasText("Retry") and hasStateDescription("In progress")).assertIsDisplayed()
        compose.mainClock.advanceTimeBy(1_000)
        compose.onNodeWithText("Still can't reach pm serve. $UNREACHABLE_HINT").assertIsDisplayed()

        compose.onNodeWithText("Retry").performClick()
        compose.mainClock.advanceTimeBy(1_000)
        compose.onNodeWithText("Connecting…").assertIsDisplayed()
        connection.value = Connection.Live
        compose.mainClock.advanceTimeBy(100)
        compose.onNodeWithText("Offline", substring = true).assertDoesNotExist()
        compose.onNodeWithText("Connecting…").assertDoesNotExist()

        connection.value = Connection.Unreachable("refused")
        compose.mainClock.advanceTimeBy(100)
        compose.onNodeWithText("Can't reach pm serve. $UNREACHABLE_HINT").assertIsDisplayed()

        connection.value = Connection.Unauthorized
        compose.mainClock.advanceTimeBy(100)
        compose.onNodeWithText("Pair again").performClick()
        assertEquals(1, pairings)
    }

    @Test
    fun a_tool_card_opens_from_its_header_and_asks_for_the_whole_output() {
        val tool =
            Item.Tool(
                "t",
                null,
                "Bash",
                "cargo test",
                ToolResult("first lines", error = false, truncated = true, full = "r1"),
            )
        val opened = mutableListOf<Pair<String, String>>()
        compose.setContent { PmTheme { ToolCard(tool) { name, ref -> opened.add(name to ref) } } }
        compose.onNodeWithText("first lines").assertDoesNotExist()
        compose.onNodeWithText("Bash").performClick()
        compose.onNodeWithText("first lines").assertIsDisplayed()
        compose.onNodeWithText("first lines").performClick()
        compose.onNodeWithText("first lines").assertIsDisplayed()
        compose.onNodeWithText("Show all").performClick()
        assertEquals(listOf("Bash" to "r1"), opened)
    }

    @Test
    fun the_chat_follows_its_end_until_scrolled_up_then_counts_what_came_since() {
        val user = { n: Int -> Item.User("u$n", null, "message $n") }
        var conversation by mutableStateOf(Conversation((0 until 40).map(user)))
        compose.setContent {
            PmTheme { ChatView(conversation, live = true, older = {}, openResult = { _, _ -> }) }
        }
        compose.onNodeWithText("message 39").assertIsDisplayed()

        conversation = conversation.appended(listOf(user(40)), null)
        compose.onNodeWithText("message 40").assertIsDisplayed()

        compose.onNode(hasScrollToIndexAction()).performScrollToIndex(0)
        conversation = conversation.appended(listOf(user(41), user(42)), null)
        compose.onNodeWithText("2 new", useUnmergedTree = true).performClick()
        compose.onNodeWithText("message 42").assertIsDisplayed()
        compose.onNodeWithText("2 new", useUnmergedTree = true).assertDoesNotExist()
    }

    @Test
    fun the_chat_follows_again_once_scrolled_back_and_stays_at_its_end_as_the_last_row_grows() {
        val user = { n: Int -> Item.User("u$n", null, "message $n") }
        val running = Item.Tool("t", null, "Bash", "cargo test", null)
        var conversation by mutableStateOf(Conversation((0 until 40).map(user) + running))
        compose.setContent {
            PmTheme { ChatView(conversation, live = true, older = {}, openResult = { _, _ -> }) }
        }
        val list = compose.onNode(hasScrollToIndexAction())
        list.performScrollToIndex(0)
        compose.onNodeWithContentDescription("Go to the end").assertIsDisplayed()
        list.performScrollToIndex(conversation.items.size)
        compose.onNodeWithContentDescription("Go to the end").assertDoesNotExist()

        val output = (0 until 60).joinToString("\n") { "line $it" }
        conversation =
            conversation.appended(
                listOf(running.copy(result = ToolResult(output, false, false, null))),
                null,
            )
        compose.onNodeWithText("Bash").performClick()
        compose.waitForIdle()
        val range = list.fetchSemanticsNode().config[SemanticsProperties.VerticalScrollAxisRange]
        assertEquals("scrolled to the end", range.maxValue(), range.value())
        compose.onNodeWithContentDescription("Go to the end").assertDoesNotExist()
    }

    /** At a phone's density, not the 1× that screenshots render at. */
    @Test
    @Config(qualifiers = "w360dp-h640dp-440dpi")
    @GraphicsMode(GraphicsMode.Mode.NATIVE)
    fun the_screen_wraps_its_rows_and_fits_its_rules() {
        val prose = (1..40).joinToString(" ") { "word$it" }
        val rule = "─".repeat(150) + " main ─"
        compose.setContent { PmTheme { TerminalView("$rule\n$prose") } }
        val screen = compose.onRoot().fetchSemanticsNode().size.width
        fun layout(text: String): TextLayoutResult {
            val layouts = mutableListOf<TextLayoutResult>()
            compose
                .onNodeWithText(text, substring = true)
                .fetchSemanticsNode()
                .config[SemanticsActions.GetTextLayoutResult]
                .action!!(layouts)
            return layouts.single()
        }

        val wrapped = layout(prose)
        assertTrue("${wrapped.lineCount} lines", wrapped.lineCount > 1)
        assertTrue("${wrapped.size.width} > $screen", wrapped.size.width <= screen)
        val ruled = layout(" main ─")
        assertEquals(1, ruled.lineCount)
        assertFalse("the label is clipped", ruled.hasVisualOverflow)
    }

    @Test
    fun a_rule_too_wide_for_the_screen_keeps_its_label() {
        assertEquals("──── main ─", fitRule("─".repeat(70) + " main ─", 11))
        assertEquals("╭─ Title ─╮", fitRule("╭─ Title " + "─".repeat(50) + "╮", 11))
        assertEquals("─── a ─", fitRule("─── a ─", 40))
        assertEquals("─── \udb81\ude8c ─", fitRule("─".repeat(40) + " \udb81\ude8c ─", 7))
    }

    @Test
    fun the_screen_drops_the_blank_rows_below_it_and_collapses_those_between() {
        assertEquals(
            listOf("menu", "", "> 1. Yes", "", "input"),
            screenRows("menu\n   \n\n> 1. Yes   \n\n\n\ninput\n\n  \n"),
        )
    }

    @Test
    fun a_send_or_an_interrupt_on_its_way_disables_the_other() {
        var outbox by mutableStateOf<Outbox?>(Outbox.Sending("hi"))
        var interrupting by mutableStateOf(false)
        compose.setContent {
            PmTheme {
                Composer(
                    AgentState.Busy,
                    null,
                    outbox,
                    null,
                    interrupting,
                    send = {},
                    interrupt = {},
                    openTerminal = {},
                )
            }
        }
        compose.onNodeWithContentDescription("Interrupt").assertIsNotEnabled()

        outbox = null
        interrupting = true
        compose.onNodeWithText("Message the agent").performTextInput("stop that")
        compose.onNodeWithContentDescription("Send").assertIsNotEnabled()
    }
}
