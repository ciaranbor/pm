package dev.pm.app.ui

import androidx.activity.ComponentActivity
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasScrollToIndexAction
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToIndex
import androidx.compose.ui.test.performTouchInput
import dev.pm.app.model.Conversation
import dev.pm.app.model.Item
import dev.pm.app.model.ToolResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class ChatUiTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val user = { n: Int -> Item.User("u$n", null, "message $n") }

    private fun bash(id: String, command: String, output: String?, error: Boolean = false) =
        Item.Tool(
            id,
            null,
            "Bash",
            command,
            output?.let { ToolResult(it, error, truncated = false, full = null) },
        )

    /** The chat, with what its reader was asked to show standing in for the reader. */
    @Composable
    private fun Chat(conversation: Conversation, older: () -> Unit = {}) {
        val scope = rememberCoroutineScope()
        val feedback = remember(scope) { Feedback(SnackbarHostState(), scope) }
        CompositionLocalProvider(LocalFeedback provides feedback) {
            PmTheme {
                ChatView(
                    conversation,
                    live = true,
                    older = older,
                    reader = { reading, _ ->
                        Text("Reading ${reading.title}: ${reading.input} → ${reading.text}")
                    },
                )
            }
        }
    }

    @Test
    fun a_run_of_tool_calls_is_one_line_that_opens_to_each_and_a_tap_reads_one_whole() {
        val conversation =
            Conversation(
                listOf(
                    user(0),
                    bash("t1", "cargo build", "ok"),
                    Item.Thinking("th", null, "Now the tests."),
                    bash("t2", "cargo test", "1 failed", error = true),
                    bash("t3", "cargo fmt", null),
                )
            )
        compose.setContent { Chat(conversation) }
        compose.onNodeWithText("cargo test").assertDoesNotExist()
        compose.onNodeWithText("1 failed").assertIsDisplayed()
        compose.onNodeWithText("Ran 3 commands").performClick()
        compose.onNodeWithText("Now the tests.").assertIsDisplayed()
        compose.onNodeWithText("cargo test").performClick()
        compose.onNodeWithText("Reading Bash: cargo test → 1 failed").assertIsDisplayed()
    }

    @Test
    fun a_long_press_on_a_message_offers_its_actions_and_select_text_reads_it_whole() {
        compose.setContent { Chat(Conversation(listOf(user(0)))) }
        compose.onNodeWithText("message 0").performTouchInput { longClick() }
        compose.onNodeWithText("Copy").assertIsDisplayed()
        compose.onNodeWithText("Share").assertIsDisplayed()
        compose.onNodeWithText("Select text").performClick()
        compose.onNodeWithText("Reading Your message: null → message 0").assertIsDisplayed()
    }

    @Test
    fun a_short_first_page_keeps_asking_for_older_ones_until_the_list_fills() {
        var conversation by mutableStateOf(Conversation(listOf(user(100)), before = "c100"))
        var asked = 0
        val older = {
            asked++
            val held = conversation
            val n = held.items.size
            val page = (100 - n - 2 until 100 - n).map(user)
            conversation = held.prepended(page, before = "c${100 - n - 2}")
        }
        compose.setContent { Chat(conversation, older) }
        compose.waitForIdle()
        assertTrue("stopped asking after one page", asked > 1)
        val list = compose.onNode(hasScrollToIndexAction()).fetchSemanticsNode()
        val range = list.config[SemanticsProperties.VerticalScrollAxisRange]
        assertTrue("the list never filled", range.maxValue() > 0f)
    }

    @Test
    fun the_chat_follows_its_end_until_scrolled_up_then_counts_what_came_since() {
        var conversation by mutableStateOf(Conversation((0 until 40).map(user)))
        compose.setContent { Chat(conversation) }
        compose.onNodeWithText("message 39").assertIsDisplayed()

        conversation = conversation.appended(listOf(user(40)), null)
        compose.onNodeWithText("message 40").assertIsDisplayed()

        compose.onNode(hasScrollToIndexAction()).performScrollToIndex(0)
        conversation = conversation.appended(listOf(user(41), user(42)), null)
        compose.onNodeWithContentDescription("Go to the end, 2 new").performClick()
        compose.onNodeWithText("message 42").assertIsDisplayed()
        compose.onNodeWithContentDescription("Go to the end, 2 new").assertDoesNotExist()
    }

    @Test
    fun the_jump_to_the_users_last_message_counts_what_came_after_it() {
        val replies = (0 until 30).map { Item.Assistant("a$it", null, "reply $it") }
        compose.setContent { Chat(Conversation(listOf(user(0)) + replies)) }
        compose.onNodeWithText("message 0").assertDoesNotExist()
        compose.onNodeWithContentDescription("Go to your last message, 30 after it").performClick()
        compose.onNodeWithText("message 0").assertIsDisplayed()
        compose
            .onNodeWithContentDescription("Go to your last message, 30 after it")
            .assertDoesNotExist()
    }

    @Test
    fun each_jump_back_goes_to_the_users_message_before_the_last_one_it_went_to() {
        val replies = { n: Int -> (0 until 20).map { Item.Assistant("a$n-$it", null, "reply") } }
        val items = (0 until 4).flatMap { listOf(user(it)) + replies(it) }
        compose.setContent { Chat(Conversation(items)) }
        for ((n, after) in listOf(3 to 20, 2 to 41, 1 to 62, 0 to 83)) {
            val which = if (n == 3) "last" else "earlier"
            compose.onNodeWithText("message $n").assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Go to your $which message, $after after it")
                .performClick()
            compose.onNodeWithText("message $n").assertIsDisplayed()
        }
        compose.onNodeWithContentDescription("message,", substring = true).assertDoesNotExist()
    }

    @Test
    fun a_scroll_by_hand_mid_walk_goes_on_from_there_and_back_at_the_end_the_walk_restarts() {
        val replies = { n: Int -> (0 until 20).map { Item.Assistant("a$n-$it", null, "reply") } }
        compose.setContent {
            Chat(Conversation((0 until 4).flatMap { listOf(user(it)) + replies(it) }))
        }
        val list = compose.onNode(hasScrollToIndexAction())
        // List items: the start of the conversation, then message n at 21n + 1.
        compose.onNodeWithContentDescription("Go to your last message, 20 after it").performClick()
        compose
            .onNodeWithContentDescription("Go to your earlier message, 41 after it")
            .performClick()
        list.performScrollToIndex(21 + 6)
        compose.onNodeWithText("message 2").assertDoesNotExist()
        compose
            .onNodeWithContentDescription("Go to your earlier message, 62 after it")
            .performClick()
        compose.onNodeWithText("message 1").assertIsDisplayed()

        compose.onNodeWithContentDescription("Go to the end").performClick()
        compose.waitForIdle()
        list.performScrollToIndex(21 + 1)
        compose.onNodeWithContentDescription("Go to your last message, 20 after it").assertExists()
    }

    @Test
    fun a_scroll_away_from_where_a_jump_left_the_end_restarts_the_walk_once_back() {
        val replies = { n: Int -> (0 until 20).map { Item.Assistant("a$n-$it", null, "reply") } }
        val items =
            (0 until 3).flatMap { listOf(user(it)) + replies(it) } +
                listOf(user(3), Item.Assistant("a3", null, "reply"))
        compose.setContent { Chat(Conversation(items)) }
        val list = compose.onNode(hasScrollToIndexAction())
        val last = compose.onNodeWithContentDescription("Go to your last message, 1 after it")
        // List items: the start of the conversation, then message n at 21n + 1; 65 is the end.
        list.performScrollToIndex(43)
        last.performClick()
        compose.onNodeWithText("message 3").assertIsDisplayed()
        list.performScrollToIndex(55)
        list.performScrollToIndex(65)
        compose.waitForIdle()
        list.performScrollToIndex(43)
        last.assertExists()
    }

    @Test
    fun a_scroll_back_by_hand_mid_walk_goes_on_from_the_top_though_the_last_stop_still_shows() {
        val items =
            (0 until 10).flatMap { listOf(user(it), Item.Assistant("a$it", null, "reply $it")) } +
                (0 until 20).map { Item.Assistant("b$it", null, "more") }
        compose.setContent { Chat(Conversation(items)) }
        val back = { compose.onNodeWithContentDescription("Go to your", substring = true) }
        repeat(5) { back().performClick() }
        compose.onNodeWithText("message 5").assertIsDisplayed()
        compose.onNodeWithText("message 4").assertDoesNotExist()
        // List items: the start of the conversation, then message n at 2n + 1.
        compose.onNode(hasScrollToIndexAction()).performScrollToIndex(5)
        compose.onNodeWithText("message 5").assertIsDisplayed()
        back().performClick()
        compose.onNodeWithText("message 1").assertIsDisplayed()
    }

    @Test
    fun past_the_users_oldest_loaded_message_the_jump_back_loads_older_pages_until_it_finds_one() {
        // All on one day: each page opens under that day's divider.
        val at = "2026-10-06T12:00:00Z"
        val user = { n: Int -> user(n).copy(at = at) }
        val replies = { n: Int -> (0 until 20).map { Item.Assistant("a$n-$it", at, "reply") } }
        val page = replies(9) + listOf(user(10)) + replies(10) + listOf(user(11)) + replies(11)
        var conversation by mutableStateOf(Conversation(page, before = "c"))
        var asked = false
        compose.setContent { Chat(conversation, older = { asked = true }) }
        compose
            .onNodeWithContentDescription("Go to your last message", substring = true)
            .performClick()
        compose
            .onNodeWithContentDescription("Go to your earlier message", substring = true)
            .performClick()
        compose.onNodeWithText("message 10").assertIsDisplayed()
        compose.waitForIdle()
        assertFalse("asked for older before reaching the top", asked)

        // A page with none of the user's messages, then one with them.
        for ((older, cursor) in
            listOf(
                replies(5) + replies(6) + replies(7) to "c5",
                listOf(user(0), Item.Assistant("a0", at, "reply"), user(1)) to null,
            )) {
            asked = false
            compose.onNodeWithContentDescription("Go to older messages").performClick()
            compose.waitForIdle()
            assertTrue("never asked for older", asked)
            conversation = conversation.prepended(older, before = cursor)
            compose.waitForIdle()
        }
        for (n in listOf(1, 0)) {
            compose
                .onNodeWithContentDescription("Go to your earlier message", substring = true)
                .performClick()
            compose.onNodeWithText("message $n").assertIsDisplayed()
        }
        compose.onNodeWithContentDescription("message,", substring = true).assertDoesNotExist()
    }

    @Test
    fun the_chat_follows_again_once_scrolled_back_and_stays_at_its_end_as_the_last_row_grows() {
        val running = bash("t", "cargo test", null)
        val tools = (0 until 30).map { bash("b$it", "step $it", "ok") }
        var conversation by mutableStateOf(Conversation((0 until 40).map(user) + tools + running))
        compose.setContent { Chat(conversation) }
        val list = compose.onNode(hasScrollToIndexAction())
        list.performScrollToIndex(0)
        compose.onNodeWithContentDescription("Go to the end").assertIsDisplayed()
        list.performScrollToIndex(41)
        compose.onNodeWithContentDescription("Go to the end").assertDoesNotExist()

        conversation =
            conversation.appended(
                listOf(running.copy(result = ToolResult("ok", false, false, null))),
                null,
            )
        compose.onNodeWithText("Ran 31 commands").performClick()
        compose.waitForIdle()
        val range = list.fetchSemanticsNode().config[SemanticsProperties.VerticalScrollAxisRange]
        assertEquals("scrolled to the end", range.maxValue(), range.value())
        compose.onNodeWithContentDescription("Go to the end").assertDoesNotExist()
    }

    @Test
    fun a_newest_page_that_is_one_long_run_stays_at_its_end_as_older_pages_extend_the_run() {
        // All on one day: the newest page opens under that day's divider.
        val at = "2026-10-06T12:00:00Z"
        val run = { from: Int, to: Int ->
            (from until to).map { bash("t$it", "read $it", "ok").copy(at = at) }
        }
        val reply = Item.Assistant("a", at, "Huge run done.")
        var conversation by mutableStateOf(Conversation(run(250, 300) + reply, before = "c250"))
        // Each older page extends the run at its start; the last also brings what came before it.
        // Pages arrive after the list has been laid out, as from the server.
        var asked = false
        compose.setContent { Chat(conversation, older = { asked = true }) }
        compose.waitForIdle()
        while (asked) {
            asked = false
            conversation =
                when (conversation.before) {
                    "c250" -> conversation.prepended(run(200, 250), before = "c200")
                    else ->
                        conversation.prepended(
                            (0 until 20).map { user(it).copy(at = at) } + run(0, 200),
                            before = null,
                        )
                }
            compose.waitForIdle()
        }
        assertEquals(null, conversation.before)
        compose.onNodeWithText("Huge run done.").assertIsDisplayed()
        compose.onNodeWithContentDescription("Go to the end").assertDoesNotExist()
    }

    @Test
    fun paging_older_items_in_while_reading_at_the_top_keeps_the_message_read_in_place() {
        val at = "2026-10-06T12:00:00Z"
        val message = { n: Int -> user(n).copy(at = at) }
        var conversation by mutableStateOf(Conversation((50 until 90).map(message), before = "c50"))
        var asked = false
        compose.setContent { Chat(conversation, older = { asked = true }) }
        // The very top, under the day's divider, where older items of that day arrive.
        compose.onNode(hasScrollToIndexAction()).performScrollToIndex(0)
        compose.waitForIdle()
        compose.onNodeWithText("message 50").assertIsDisplayed()
        assertTrue("asked for older", asked)
        conversation = conversation.prepended((0 until 50).map(message), before = null)
        compose.waitForIdle()
        compose.onNodeWithText("message 50").assertIsDisplayed()
        compose.onNodeWithText("message 0").assertDoesNotExist()
    }

    @Test
    fun a_long_press_on_a_tool_call_reads_it_whole_rather_than_offering_a_copy() {
        compose.setContent { Chat(Conversation(listOf(bash("t", "cargo test", "cut short")))) }
        compose.onNodeWithText("cargo test").performTouchInput { longClick() }
        compose.onNodeWithText("Reading Bash: cargo test → cut short").assertIsDisplayed()
        compose.onNodeWithText("Copy").assertDoesNotExist()
    }

    @Test
    fun a_tap_on_a_message_shows_when_it_was_sent_and_another_hides_it() {
        compose.setContent { Chat(Conversation(listOf(user(0)))) }
        compose.onNodeWithText("message 0").performClick()
        compose.onNodeWithText("Time unknown").assertIsDisplayed()
        compose.onNodeWithText("message 0").performClick()
        compose.onNodeWithText("Time unknown").assertDoesNotExist()
    }
}
