package dev.pm.app.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import dev.pm.app.data.Connection
import dev.pm.app.model.Pairing
import dev.pm.app.update.Update
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class SettingsScreenTest {
    @get:Rule val compose = createComposeRule()

    private fun show(
        updates: UpdateControls? = null,
        pairing: Pairing? = null,
        unpair: () -> Unit = {},
    ) = compose.setContent {
        PmTheme {
            val scope = rememberCoroutineScope()
            val feedback = remember(scope) { Feedback(SnackbarHostState(), scope) }
            CompositionLocalProvider(LocalFeedback provides feedback) {
                Box {
                    SettingsScreen(
                        pairing = pairing,
                        connection = Connection.Live,
                        versions = Versions("0.2.0", "0.2.0"),
                        vapid = { null },
                        updates = updates,
                        pair = {},
                        unpair = unpair,
                    )
                    FeedbackHost(feedback, Modifier.align(Alignment.BottomCenter))
                }
            }
        }
    }

    @Test
    fun a_build_that_updates_itself_offers_the_check_and_one_that_does_not_only_its_version() {
        show(null)
        compose.onNodeWithText("App 0.2.0").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Check now").assertDoesNotExist()
        compose.onNodeWithText("Check for updates").assertDoesNotExist()
    }

    @Test
    fun forgetting_the_server_waits_for_confirmation() {
        var unpaired = 0
        show(pairing = Pairing("http://host:7764", "phone", "token"), unpair = { unpaired++ })

        compose.onNodeWithText("Forget this server").performScrollTo().performClick()
        compose.onNodeWithText("Cancel").performClick()
        assertEquals(0, unpaired)

        compose.onNodeWithText("Forget this server").performScrollTo().performClick()
        compose.onNodeWithText("Forget").performClick()
        assertEquals(1, unpaired)
    }

    @Test
    fun check_now_reports_what_it_found() {
        var checks = 0
        show(
            UpdateControls(
                enabled = true,
                setEnabled = {},
                checkNow = {
                    checks++
                    Update("9.9.9", "https://example.com/pm.apk")
                },
            )
        )
        compose.onNodeWithText("Check now").performScrollTo().performClick()
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("pm 9.9.9 is available").fetchSemanticsNodes().isNotEmpty()
        }
        assertEquals(1, checks)
    }

    @Test
    fun a_failed_check_offers_retry_which_checks_again() {
        var checks = 0
        show(
            UpdateControls(
                enabled = true,
                setEnabled = {},
                checkNow = {
                    checks++
                    throw IOException("GitHub answered 503")
                },
            )
        )
        compose.onNodeWithText("Check now").performScrollTo().performClick()
        compose.waitUntil(5_000) {
            compose
                .onAllNodesWithText("Couldn't check: GitHub answered 503")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Checking…").assertDoesNotExist()

        compose.onNodeWithText("Retry").performClick()
        compose.waitUntil(5_000) { checks == 2 }
    }
}
