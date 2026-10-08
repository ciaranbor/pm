package dev.pm.app.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onLast
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.OpenStream
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.model.Pairing
import dev.pm.app.push.Target
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class LifecycleUiTest {
    @get:Rule val compose = createComposeRule()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val server = MockWebServer()
    private val posted = mutableListOf<String>()

    /**
     * How many POSTs to refuse, as `pm serve` refuses an agent it can't act on, before obliging.
     */
    @Volatile private var refusals = 0

    /** The event stream, if the server has one; else the app shows the cached snapshot. */
    @Volatile private var events: (() -> MockResponse)? = null

    private val stream = OpenStream()

    /** What a GET of a feature's merge check gets; a server without the check, by default. */
    @Volatile
    private var mergeCheck: MockResponse =
        MockResponse.Builder().code(404).body("""{"error":"no such endpoint"}""").build()

    /** Each POST waits for this before it is answered. */
    @Volatile private var held = CountDownLatch(0)

    @Before
    fun start() {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.url.encodedPath
                    val reply = { code: Int, body: String ->
                        MockResponse.Builder().code(code).body(body).build()
                    }
                    return when {
                        request.method == "POST" -> {
                            synchronized(posted) { posted += path }
                            held.await(5, TimeUnit.SECONDS)
                            if (refusals > 0) {
                                refusals--
                                reply(409, """{"error":"no harness","refused":"no-harness"}""")
                            } else reply(200, "{}")
                        }
                        path == "/v1/events" -> events?.invoke() ?: reply(503, "{}")
                        path.endsWith("/merge") -> mergeCheck
                        // Nothing else to read: the app shows the cached snapshot.
                        else -> reply(503, "{}")
                    }
                }
            }
        server.start()
    }

    @After
    fun stop() {
        held.countDown()
        stream.release()
        scope.cancel()
        server.close()
    }

    private lateinit var model: AppViewModel

    /** The notification target the app is given; setting it again is another tap. */
    private var shown by mutableStateOf<Target?>(null)

    private fun open(target: Target) {
        val store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        model = AppViewModel(Repository(store, OkHttpClient(), scope)) {}
        shown = target
        compose.setContent { PmTheme { App(model, shown, targetShown = {}) } }
        compose.waitUntil(WAIT) { model.snapshot.value != null }
    }

    /** Open the project page's menu, reached by Up from one of its workspaces. */
    private fun openProjectMenu() {
        open(Target("app", "login", null))
        compose.onNodeWithContentDescription("Navigate up").performClick()
        compose.onNodeWithText("Notes").assertIsDisplayed()
        compose.onNodeWithContentDescription("More actions").performClick()
    }

    @Test
    fun a_project_is_deleted_only_once_its_name_is_typed_and_then_left() {
        openProjectMenu()
        compose.onNodeWithText("Open").assertIsDisplayed()
        compose.onNodeWithText("Delete").performClick()

        compose.onNodeWithText("Delete app?").assertIsDisplayed()
        compose.onNodeWithText("Delete").assertIsNotEnabled()
        compose.onNode(hasSetTextAction()).performTextInput("ap")
        compose.onNodeWithText("Delete").assertIsNotEnabled()
        compose.onNode(hasSetTextAction()).performTextInput("p ")
        compose.onNodeWithText("Delete").performClick()

        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Deleted app").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(listOf("/v1/projects/app/delete"), synchronized(posted) { posted.toList() })
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun a_close_says_how_many_agents_it_interrupts_and_stays_on_the_project() {
        openProjectMenu()
        compose.onNodeWithText("Close").performClick()

        compose
            .onNodeWithText(
                "1 agent is working, asking, or waiting on background work.",
                substring = true,
            )
            .assertIsDisplayed()
        compose.onNodeWithText("Close").performClick()

        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Closed app").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(listOf("/v1/projects/app/close"), synchronized(posted) { posted.toList() })
        compose.onNodeWithText("Notes").assertIsDisplayed()
    }

    @Test
    fun a_confirmed_merge_leaves_the_feature_for_where_it_was_opened_from() {
        open(Target("app", "login", null))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.onNodeWithText("Merge").performClick()
        compose.onNodeWithText("Merge login?").assertIsDisplayed()
        assertEquals(emptyList<String>(), synchronized(posted) { posted.toList() })
        compose.onNodeWithText("Merge").performClick()

        compose.waitUntil(WAIT) {
            compose.onAllNodesWithText("Merged login").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            listOf("/v1/features/app/login/merge"),
            synchronized(posted) { posted.toList() },
        )
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun a_ready_feature_merges_from_its_summary() {
        open(Target("app", "search", null))

        compose.onNodeWithText("Merge").performClick()
        compose.onNodeWithText("Merge search?").assertIsDisplayed()
        compose.onAllNodesWithText("Merge").onLast().performClick()
        compose.waitUntil(WAIT) {
            compose.onAllNodesWithText("Merged search").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            listOf("/v1/features/app/search/merge"),
            synchronized(posted) { posted.toList() },
        )
    }

    @Test
    fun merge_is_off_with_the_servers_reason_while_it_would_not_go_through() {
        mergeCheck =
            MockResponse.Builder()
                .code(200)
                .body("""{"mergeable":false,"reason":"Behind main: rebase first"}""")
                .build()
        open(Target("app", "search", null))

        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("Behind main: rebase first")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Merge").assertIsNotEnabled()
        assertEquals(emptyList<String>(), synchronized(posted) { posted.toList() })
    }

    @Test
    fun a_chats_menu_merge_is_off_with_the_servers_reason() {
        mergeCheck =
            MockResponse.Builder()
                .code(200)
                .body("""{"mergeable":false,"reason":"Behind main: rebase first"}""")
                .build()
        open(Target("app", "login", "implementer"))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("Behind main: rebase first")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose
            .onNode(hasText("Merge") and hasText("Behind main: rebase first"))
            .assertIsNotEnabled()
    }

    @Test
    fun a_feature_page_says_why_it_cant_merge_and_notices_once_it_can() {
        mergeCheck =
            MockResponse.Builder()
                .code(200)
                .body("""{"mergeable":false,"reason":"Behind main: rebase first"}""")
                .build()
        open(Target("app", "login", "implementer"))

        compose.onNodeWithContentDescription("Feature info").performClick()
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("Can't merge: behind main: rebase first")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithContentDescription("Merge", substring = true).assertIsNotEnabled()

        mergeCheck = MockResponse.Builder().code(200).body("""{"mergeable":true}""").build()
        compose.mainClock.advanceTimeBy(30_000)
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("Can't merge: behind main: rebase first")
                .fetchSemanticsNodes()
                .isEmpty()
        }
        compose.onNodeWithContentDescription("Merge").assertIsEnabled()
    }

    @Test
    fun a_feature_newer_than_any_snapshot_yet_stays_open() {
        events = { stream.response("snapshot" to SNAPSHOT) }
        open(Target("app", "fresh", "implementer"))
        compose.waitUntil(WAIT) { model.connection.value == Connection.Live }
        compose.waitForIdle()

        compose.onNodeWithText("fresh").assertIsDisplayed()
        compose.onNodeWithText("fresh was merged or deleted").assertDoesNotExist()
    }

    @Test
    fun a_scope_merged_elsewhere_is_left_with_word_of_why() {
        val without = SNAPSHOT.replace(""""name": "login"""", """"name": "login-gone"""")
        events = { stream.response("snapshot" to SNAPSHOT, later = listOf("snapshot" to without)) }
        open(Target("app", "login", null))
        compose.waitUntil(WAIT) { model.connection.value == Connection.Live }
        compose.onNodeWithText("Message the agent").assertIsDisplayed()

        stream.sendLater()
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("login was merged or deleted")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Needs you").assertIsDisplayed()
    }

    @Test
    fun a_scope_opened_after_the_last_snapshot_is_still_left_once_one_drops_it() {
        val without = SNAPSHOT.replace(""""name": "search"""", """"name": "search-gone"""")
        events = { stream.response("snapshot" to SNAPSHOT, later = listOf("snapshot" to without)) }
        open(Target("app", "login", null))
        compose.waitUntil(WAIT) { model.connection.value == Connection.Live }
        shown = Target("app", "search", null)
        compose.onNodeWithText("Merge").assertIsDisplayed()

        stream.sendLater()
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("search was merged or deleted")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Message the agent").assertIsDisplayed()
    }

    @Test
    fun an_agent_restarts_from_its_page_without_confirming() {
        open(Target("app", "login", "implementer"))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.onNodeWithText("Restart implementer").performClick()

        compose.waitUntil(WAIT) {
            compose.onAllNodesWithText("Restarted implementer").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            listOf("/v1/agents/app/login/implementer/restart"),
            synchronized(posted) { posted.toList() },
        )
    }

    @Test
    fun a_merge_on_its_way_shows_on_its_confirm_button_and_cant_be_sent_twice_or_cancelled() {
        held = CountDownLatch(1)
        open(Target("app", "login", null))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.onNodeWithText("Merge").performClick()
        compose.onNodeWithText("Merge").performClick()
        compose.waitUntil(WAIT) { synchronized(posted) { posted.isNotEmpty() } }

        compose
            .onNode(hasText("Merge") and hasStateDescription("In progress"))
            .assertIsDisplayed()
            .performClick()
        compose.onNodeWithText("Cancel").assertIsNotEnabled()
        held.countDown()
        compose.waitUntil(WAIT) {
            compose.onAllNodesWithText("Merged login").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            listOf("/v1/features/app/login/merge"),
            synchronized(posted) { posted.toList() },
        )
    }

    @Test
    fun an_interrupt_says_why_it_failed_and_retry_sends_it_again() {
        refusals = 1
        open(Target("app", "login", "implementer"))

        compose.onNodeWithContentDescription("Interrupt").performClick()
        compose.waitUntil(WAIT) {
            compose
                .onAllNodesWithText("Couldn't interrupt: no harness")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        // The snackbar's, drawn after the offline strip's.
        compose.onAllNodesWithText("Retry").onLast().performClick()
        compose.waitUntil(WAIT) {
            compose.onAllNodesWithText("Interrupted implementer").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            List(2) { "/v1/agents/app/login/implementer/interrupt" },
            synchronized(posted) { posted.toList() },
        )
    }
}

/** How long a wait on the local server may take in a full, loaded test run. */
private const val WAIT = 15_000L
