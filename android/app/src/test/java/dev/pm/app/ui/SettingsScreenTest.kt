package dev.pm.app.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performScrollTo
import dev.pm.app.data.Connection
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class SettingsScreenTest {
    @get:Rule val compose = createComposeRule()

    private fun show(updates: UpdateControls?) = compose.setContent {
        PmTheme {
            SettingsScreen(
                pairing = null,
                connection = Connection.Unpaired,
                versions = Versions("0.2.0", "0.2.0"),
                vapid = { null },
                updates = updates,
                pair = {},
                unpair = {},
            )
        }
    }

    @Test
    fun a_build_that_updates_itself_offers_the_check_and_one_that_does_not_only_its_version() {
        show(null)
        compose.onNodeWithText("App 0.2.0 · Server 0.2.0").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Check now").assertDoesNotExist()
        compose.onNodeWithText("Check for updates").assertDoesNotExist()
    }

    @Test
    fun the_update_controls_show_where_the_build_updates_itself() {
        show(UpdateControls(enabled = true, setEnabled = {}, checkNow = { null }))
        compose.onNodeWithText("Check now").performScrollTo().assertIsDisplayed()
    }
}
