package dev.pm.app.ui

import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.model.Item
import dev.pm.app.model.Pairing
import dev.pm.app.model.ToolResult
import dev.pm.app.push.Target
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
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
    @get:Rule
    val compose = createComposeRule()

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
        compose.onNodeWithText("app/login").assertIsDisplayed()
        compose.onNodeWithText("implementer").assertIsDisplayed()
        assertTrue(shown)

        compose.onNodeWithContentDescription("Back").performClick()
        compose.onNodeWithText("main").assertIsDisplayed()
        compose.onNodeWithText("search").assertIsDisplayed()
    }

    @Test
    fun the_unreachable_banner_says_how_old_the_snapshot_is_and_retries_on_tap() {
        var retries = 0
        compose.setContent {
            PmTheme { ConnectionBanner(Connection.Unreachable("refused"), readAt = 0L, retry = { retries++ }) }
        }
        compose.onNodeWithText("Server unreachable", substring = true)
            .assertIsDisplayed()
            .assert(hasText("showing what was known at", substring = true))
            .performClick()
        assertEquals(1, retries)
    }

    @Test
    fun a_tool_card_opens_to_its_result_and_loads_the_rest_on_request() {
        val tool = Item.Tool("t", null, "Bash", "cargo test", ToolResult("first lines", error = false, truncated = true, full = "r1"))
        compose.setContent {
            PmTheme { ToolCard(tool) { ref -> Result.success("all of $ref") } }
        }
        compose.onNodeWithText("first lines").assertDoesNotExist()
        compose.onNodeWithText("Bash").performClick()
        compose.onNodeWithText("first lines").assertIsDisplayed()
        compose.onNodeWithText("Show all").performClick()
        compose.onNodeWithText("all of r1").assertIsDisplayed()
    }
}

