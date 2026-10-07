package dev.pm.app.ui

import android.Manifest
import android.app.Application
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTextReplacement
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.model.Pairing
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

@RunWith(RobolectricTestRunner::class)
class PairScreenTest {
    @get:Rule val compose = createComposeRule()

    private val pairs = mutableListOf<Pairing>()

    private fun show(hasCamera: Boolean) = compose.setContent {
        PmTheme { PairScreen(paired = { pairs += it }, hasCamera = hasCamera) }
    }

    private fun field() = compose.onNode(hasSetTextAction())

    @Test
    fun a_phone_without_a_camera_pastes_without_being_offered_the_scanner() {
        show(hasCamera = false)
        compose.onNodeWithText("Scan QR").assertDoesNotExist()
        field().performTextInput("""{"url":"http://host:7764","device":"phone","token":"t"}""")
        compose.onNodeWithText("Pair").performClick()
        assertEquals(listOf(Pairing("http://host:7764", "phone", "t")), pairs)
    }

    @Test
    fun a_paste_that_is_not_a_pairing_is_flagged_on_the_field_until_edited() {
        show(hasCamera = true)
        compose.onNodeWithText("Paste").performClick()
        field().performTextInput("hello")
        compose.onNodeWithText("Pair").performClick()
        val error = "That is not a pairing: paste all that pm serve pair prints."
        compose.onNodeWithText(error).assertIsDisplayed()
        assertEquals(emptyList<Pairing>(), pairs)

        field().performTextReplacement("hello again")
        compose.onNodeWithText(error).assertDoesNotExist()
    }

    @Test
    fun a_camera_that_cannot_be_opened_falls_back_to_paste() {
        shadowOf(ApplicationProvider.getApplicationContext<Application>())
            .grantPermissions(Manifest.permission.CAMERA)
        show(hasCamera = true)
        compose.onNodeWithText("Scan QR").performClick()
        compose.mainClock.advanceTimeBy(11_000)
        compose.waitUntil(5_000) {
            compose
                .onAllNodesWithText("The camera isn't available, so paste the pairing instead.")
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        field().assertIsDisplayed()
    }
}
