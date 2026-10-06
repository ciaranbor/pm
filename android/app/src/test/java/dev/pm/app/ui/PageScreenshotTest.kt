package dev.pm.app.ui

import androidx.compose.material3.Surface
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onRoot
import androidx.lifecycle.viewmodel.compose.viewModel
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.api.PmClient
import dev.pm.app.model.Pairing
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A Markdown page, compared with the images in `src/test/screenshots` on every test run: headings
 * at title sizes, inline code without wide padding. `gradlew recordRoborazziDebug` records anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h400dp")
class PageScreenshotTest {
    @get:Rule val compose = createComposeRule()

    private val client = PmClient(Pairing("http://127.0.0.1:9", "pixel", "tok"))

    private val summary =
        "# Login\n\nAdds `pm login` and a `--token` flag, so a phone pairs once.\n\n" +
            "## Gaps\n\n- The token isn't refreshed.\n- `pm logout` is missing.\n\n" +
            "### Next\n\nRefresh tokens."

    /** Once the Markdown is parsed, off the main thread. */
    private fun page() {
        compose.setContent {
            PmTheme(dynamic = false) {
                Surface { SummaryScreen(viewModel { ReadModel(client) { summary } }) }
            }
        }
        compose.waitUntil(10_000) {
            compose.onAllNodesWithText("Gaps").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onRoot().captureRoboImage()
    }

    @Test fun summary_light() = page()

    @Test @Config(qualifiers = "+night") fun summary_dark() = page()
}
