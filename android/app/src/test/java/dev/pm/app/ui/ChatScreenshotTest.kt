package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Surface
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.model.AgentState
import dev.pm.app.model.Conversation
import dev.pm.app.model.Item
import dev.pm.app.model.ToolResult
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A run of tool calls opened to its lines, and the reader, compared with the images in
 * `src/test/screenshots` on every run; `gradlew recordRoborazziGoogleDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h640dp")
class ChatScreenshotTest {
    @get:Rule val compose = createComposeRule()

    private fun tool(
        id: String,
        name: String,
        input: String,
        secs: Int,
        output: String?,
        error: Boolean = false,
    ) =
        Item.Tool(
            id,
            "2026-10-01T12:00:00Z",
            name,
            input,
            output?.let {
                ToolResult(it, error, false, null, at = "2026-10-01T12:00:%02dZ".format(secs))
            },
        )

    private val conversation =
        Conversation(
            listOf(
                Item.User("u1", "2026-10-01T12:00:00Z", "Add Stripe to the checkout flow."),
                Item.Thinking("th", "2026-10-01T12:00:00Z", "Find the checkout handler first."),
                tool("t1", "Grep", "createCheckout", 1, "src/checkout.ts:12"),
                tool("t2", "Read", "src/checkout.ts", 1, "export function createCheckout() {}"),
                tool("t3", "Bash", "npm install stripe@^17", 14, "added 1 package"),
                tool("t4", "Edit", "src/checkout.ts", 2, "ok"),
                tool("t5", "Bash", "npm test", 41, "1 failing", error = true),
                tool("t6", "Bash", "npm test -- --watch=false", 0, null),
                Item.Event(
                    "e1",
                    "2026-10-01T12:01:00Z",
                    "API Error: 529 Overloaded",
                    failure = true,
                ),
                Item.Assistant(
                    "a1",
                    "2026-10-01T12:02:00Z",
                    "Checkout now creates a **Stripe** session.\nOne test still fails: `refund`.",
                ),
            ),
            before = null,
        )

    private fun chat() {
        compose.setContent {
            PmTheme(dynamic = false) {
                Surface {
                    Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                        StatePill(AgentState.Asking, stale = false)
                        ChatView(conversation, live = true, older = {})
                    }
                }
            }
        }
        compose
            .onNodeWithText("Ran 3 commands, read 1 file, edited 1 file, searched once")
            .performClick()
        compose.onRoot().captureRoboImage()
    }

    @Test fun tools_light() = chat()

    @Test @Config(qualifiers = "+night") fun tools_dark() = chat()

    private val output =
        Reading(
            "Bash",
            (1..40).joinToString("\n") {
                "test checkout::case_$it ... ok   (a line long enough to run past the screen's edge)"
            },
            ReadStyle.Output,
            input = "npm test -- --reporter=spec",
        )

    private fun reader() {
        compose.setContent {
            PmTheme(dynamic = false) {
                Surface { ReaderView(output, ReaderBody.Shown(output.text), close = {}) }
            }
        }
        compose.onRoot().captureRoboImage()
    }

    @Test fun reader_light() = reader()

    @Test @Config(qualifiers = "+night") fun reader_dark() = reader()
}
