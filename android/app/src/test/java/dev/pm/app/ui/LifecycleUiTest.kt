package dev.pm.app.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onLast
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
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
