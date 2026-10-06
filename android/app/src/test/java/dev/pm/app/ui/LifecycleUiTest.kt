package dev.pm.app.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onLast
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
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
                        // No event stream: the app shows the cached snapshot.
                        else -> reply(503, "{}")
                    }
                }
            }
        server.start()
    }

    @After
    fun stop() {
        held.countDown()
        scope.cancel()
        server.close()
    }

    private fun open(target: Target) {
        val store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok")
        store.cacheSnapshot(SNAPSHOT)
        val model = AppViewModel(Repository(store, OkHttpClient(), scope)) {}
        compose.setContent { PmTheme { App(model, target, targetShown = {}) } }
        compose.waitUntil(5_000) { model.snapshot.value != null }
    }

    @Test
    fun a_confirmed_merge_leaves_the_feature_for_its_project() {
        open(Target("app", "login", null))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.onNodeWithText("Merge").performClick()
        compose.onNodeWithText("Merge login?").assertIsDisplayed()
        assertEquals(emptyList<String>(), synchronized(posted) { posted.toList() })
        compose.onNodeWithText("Merge").performClick()

        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Merged login").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            listOf("/v1/features/app/login/merge"),
            synchronized(posted) { posted.toList() },
        )
        compose.onNodeWithContentDescription("main", substring = true).assertIsDisplayed()
    }

    @Test
    fun an_agent_restarts_from_its_page_without_confirming() {
        open(Target("app", "login", "implementer"))

        compose.onNodeWithContentDescription("More actions").performClick()
        compose.onNodeWithText("Restart").performClick()

        compose.waitUntil(5_000) {
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
        compose.waitUntil(5_000) { synchronized(posted) { posted.isNotEmpty() } }

        compose
            .onNode(hasText("Merge") and hasStateDescription("In progress"))
            .assertIsDisplayed()
            .performClick()
        compose.onNodeWithText("Cancel").assertIsNotEnabled()
        held.countDown()
        compose.waitUntil(5_000) {
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
        compose.waitUntil(5_000) {
            compose
                .onAllNodesWithText("Couldn't interrupt: no harness")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        // The snackbar's, drawn after the offline strip's.
        compose.onAllNodesWithText("Retry").onLast().performClick()
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Interrupted implementer").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(
            List(2) { "/v1/agents/app/login/implementer/interrupt" },
            synchronized(posted) { posted.toList() },
        )
    }
}
