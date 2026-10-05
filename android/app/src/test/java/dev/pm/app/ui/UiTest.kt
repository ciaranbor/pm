package dev.pm.app.ui

import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasScrollToIndexAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.model.Conversation
import dev.pm.app.model.Item
import dev.pm.app.model.Pairing
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
        compose.onNodeWithContentDescription("More").performClick()
        compose.onNodeWithText("Summary").assertIsDisplayed()
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
        val opened = mutableListOf<Item.Tool>()
        compose.setContent { PmTheme { ToolCard(tool) { opened.add(it) } } }
        compose.onNodeWithText("first lines").assertDoesNotExist()
        compose.onNodeWithText("Bash").performClick()
        compose.onNodeWithText("first lines").assertIsDisplayed()
        compose.onNodeWithText("first lines").performClick()
        compose.onNodeWithText("first lines").assertIsDisplayed()
        compose.onNodeWithText("Show all").performClick()
        assertEquals(listOf(tool), opened)
    }

    @Test
    fun the_chat_follows_its_end_until_scrolled_up_then_counts_what_came_since() {
        val user = { n: Int -> Item.User("u$n", null, "message $n") }
        var conversation by mutableStateOf(Conversation((0 until 40).map(user)))
        compose.setContent {
            PmTheme { ChatView(conversation, live = true, older = {}, openResult = {}) }
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
}
