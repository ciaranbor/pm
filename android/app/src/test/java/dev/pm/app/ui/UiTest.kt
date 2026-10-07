package dev.pm.app.ui

import androidx.activity.ComponentActivity
import androidx.compose.foundation.text.selection.SelectionState
import androidx.compose.foundation.text.selection.rememberSelectionState
import androidx.compose.material3.Text
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertContentDescriptionContains
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
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
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
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
            // The page reads the server on Dispatchers.IO, which the test clock does not await.
            compose.waitUntil(5_000) {
                compose
                    .onAllNodesWithText("Can't reach pm serve")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
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
    fun a_send_or_an_interrupt_on_its_way_holds_the_other_and_a_draft_outlives_its_composer() {
        val drafts = Drafts()
        var sending by mutableStateOf(true)
        var interrupting by mutableStateOf(false)
        var shown by mutableStateOf("implementer")
        compose.setContent {
            PmTheme {
                key(shown) {
                    Composer(
                        AgentState.Busy,
                        null,
                        sending,
                        notice = null,
                        interrupting,
                        drafts,
                        "app/login/$shown",
                        send = {},
                        interrupt = {},
                        openTerminal = {},
                    )
                }
            }
        }
        compose.onNodeWithContentDescription("Agent actions").performClick()
        compose.onNodeWithText("Interrupt").assertDoesNotExist()
        compose.onNodeWithText("Show the terminal").performClick()

        sending = false
        interrupting = true
        compose.onNodeWithText("Message the agent").performTextInput("stop that")
        compose.onNodeWithContentDescription("Send").assertIsNotEnabled()
        compose.onNodeWithContentDescription("Interrupting").assertIsDisplayed()

        shown = "reviewer"
        compose.onNodeWithText("stop that").assertDoesNotExist()
        shown = "implementer"
        compose.onNodeWithText("stop that").assertIsDisplayed()
    }

    @Test
    fun a_failed_send_taken_back_goes_after_the_draft_already_there() {
        val drafts = Drafts()
        drafts["a"] = "first"
        drafts.restore("a", "second")
        drafts.restore("b", "only")
        assertEquals("first\nsecond", drafts["a"])
        assertEquals("only", drafts["b"])
    }

    @Test
    fun a_dialog_answered_here_leaves_the_composer_as_for_a_busy_agent() {
        assertEquals(AgentState.Busy, composerState(AgentState.Asking, "d1", setOf("d1")))
        assertEquals(AgentState.Asking, composerState(AgentState.Asking, "d2", setOf("d1")))
        assertEquals(AgentState.Idle, composerState(AgentState.Idle, null, setOf("d1")))
    }
}
