package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Surface
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onRoot
import androidx.lifecycle.viewmodel.compose.viewModel
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import java.time.Instant
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A Markdown page and a ready feature's workspace, compared with the images in
 * `src/test/screenshots` on every test run: headings at title sizes, inline code without wide
 * padding. `gradlew recordRoborazziDebug` records anew.
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

    /**
     * A ready feature's workspace: its agents' tabs, then its pages, open on Summary with Merge.
     */
    private fun ready() {
        val snapshot = Snapshot.parse(SNAPSHOT)
        val agents =
            listOf(
                AgentSnapshot("implementer", "idle"),
                AgentSnapshot("reviewer", "busy", unread = 1),
            )
        val feature =
            snapshot
                .feature("app", "search")!!
                .copy(summary = "Adds search", pr = "12", agents = agents, working = false)
        compose.setContent {
            PmTheme(dynamic = false) {
                Surface {
                    Column {
                        WorkspaceTabs(tabsOf("search", agents, null), Tab.Summary, agents, {})
                        SummaryTab(
                            feature,
                            viewModel { ReadModel(client) { summary } },
                            Instant.parse("2026-10-02T10:00:00Z"),
                            stale = false,
                            merging = false,
                            busy = false,
                            merge = {},
                        )
                    }
                }
            }
        }
        compose.waitUntil(10_000) {
            compose.onAllNodesWithText("Gaps").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onRoot().captureRoboImage()
    }

    @Test @Config(qualifiers = "w360dp-h640dp") fun workspace_ready_light() = ready()

    @Test @Config(qualifiers = "w360dp-h640dp-night") fun workspace_ready_dark() = ready()
}
