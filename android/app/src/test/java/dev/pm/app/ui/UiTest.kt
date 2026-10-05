package dev.pm.app.ui

import android.content.ClipboardManager
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasScrollToIndexAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
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
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
class UiTest {
    @get:Rule val compose = createComposeRule()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    @After
    fun tearDown() {
        scope.cancel()
    }

    @Test
    fun a_notification_opens_its_scope_over_the_projects() {
        val store = Store(ApplicationProvider.getApplicationContext())
        // Nothing listens on the discard port: the server is unreachable, as off the tailnet.
        store.pairing = Pairing("http://127.0.0.1:9", "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        val model = AppViewModel(Repository(store, OkHttpClient(), scope)) {}
        var shown = false
        compose.setContent {
            PmTheme { App(model, Target("app", "login", null), targetShown = { shown = true }) }
        }

        compose.waitUntil(5_000) { model.snapshot.value != null }
        compose.onNodeWithText("login").assertIsDisplayed()
        compose.onNodeWithText("app").assertIsDisplayed()
        compose
            .onNodeWithContentDescription("implementer, asking: Postgres or SQLite?, 2 unread")
            .assertIsDisplayed()
        assertTrue(shown)

        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithContentDescription("main", substring = true).assertIsDisplayed()
        compose.onNodeWithContentDescription("search", substring = true).assertIsDisplayed()
    }

    @Test
    fun a_need_on_the_start_screen_opens_the_agent_it_names_with_its_scope_behind() {
        val store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing("http://127.0.0.1:9", "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        val model = AppViewModel(Repository(store, OkHttpClient(), scope)) {}
        compose.setContent { PmTheme { App(model, null, targetShown = {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        compose
            .onNodeWithContentDescription(
                "app/login, blocked, which DB?, implementer asking, 2 unread"
            )
            .performClick()
        compose.onNodeWithText("app › login").assertIsDisplayed()
        compose.onNodeWithText("Chat").assertIsDisplayed()

        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithText("login").assertIsDisplayed()
    }

    @Test
    fun a_features_page_opens_its_summary_brief_and_details() {
        val store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing("http://127.0.0.1:9", "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        val model = AppViewModel(Repository(store, OkHttpClient(), scope)) {}
        compose.setContent { PmTheme { App(model, Target("app", "login", null), {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }

        for (page in listOf("Summary", "Brief", "Details")) {
            compose.onNodeWithText(page).performClick()
            compose.onNodeWithText(page).assertIsDisplayed()
            compose.onNodeWithText("app › login").assertIsDisplayed()
            compose
                .onNodeWithContentDescription("implementer", substring = true)
                .assertDoesNotExist()
            compose.onNodeWithContentDescription("Back").performClick()
            compose
                .onNodeWithContentDescription("implementer", substring = true)
                .assertIsDisplayed()
        }
    }

    @Test
    fun a_ready_features_header_says_so_rather_than_repeating_its_summary() {
        val snapshot = Snapshot.parse(SNAPSHOT)
        val ready =
            snapshot.copy(
                features =
                    snapshot.features.map {
                        if (it.name == "search") it.copy(summary = "Adds search", pr = "12") else it
                    }
            )
        val now = Instant.parse("2026-10-02T10:00:00Z")
        compose.setContent {
            PmTheme { AgentsList(ready, "app", "search", now, openAgent = {}, openPage = {}) }
        }
        compose.onNodeWithText("Ready for review").assertIsDisplayed()
        compose.onNodeWithText("Adds search").assertDoesNotExist()
        compose.onNodeWithText("Status ready · PR #12 open").assertIsDisplayed()
    }

    @Test
    fun copy_puts_the_brief_on_the_clipboard() {
        val brief = "# Login\n\nUse **OAuth**."
        val client = PmClient(Pairing("http://127.0.0.1:9", "pixel", "tok"))
        val model = ReadModel(client) { FeatureInfo(name = "login", context = brief) }
        compose.setContent { PmTheme { BriefScreen(model) } }
        compose.onNodeWithText("Copy").performClick()
        compose.waitForIdle()

        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        val clip = context.getSystemService(ClipboardManager::class.java).primaryClip!!
        assertEquals(brief, clip.getItemAt(0).text)
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
    fun the_status_strip_says_how_old_the_snapshot_is_and_offers_the_fix() {
        var retries = 0
        var pairings = 0
        val now = Instant.parse("2026-10-02T10:00:00Z")
        val connection = MutableStateFlow<Connection>(Connection.Unreachable("refused"))
        compose.setContent {
            PmTheme {
                val state by connection.collectAsState()
                StatusStrip(
                    state,
                    readAt = now.toEpochMilli() - 12 * 60_000,
                    now = now,
                    retry = { retries++ },
                    pairAgain = { pairings++ },
                )
            }
        }
        compose.onNodeWithText("Offline · last update 12 min ago").assertIsDisplayed()
        compose.onNodeWithText("Retry").performClick()
        assertEquals(1, retries)

        connection.value = Connection.Unauthorized
        compose.onNodeWithText("Pair again").performClick()
        assertEquals(1, pairings)

        connection.value = Connection.Live
        compose.onNodeWithText("last update", substring = true).assertDoesNotExist()
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
    fun the_screen_fits_its_widest_row_to_the_width() {
        val row = "x".repeat(100)
        compose.setContent { PmTheme { TerminalView("$row\nshort") } }
        val layouts = mutableListOf<TextLayoutResult>()
        compose
            .onNodeWithText(row, substring = true)
            .fetchSemanticsNode()
            .config[SemanticsActions.GetTextLayoutResult]
            .action!!(layouts)
        val text = layouts.single().size.width
        val padding = with(compose.density) { 16.dp.roundToPx() }
        val screen = compose.onRoot().fetchSemanticsNode().size.width - padding
        assertTrue("$text > $screen", text <= screen)
        assertTrue("$text < 0.97 × $screen", text >= screen * 0.97f)
    }
}
