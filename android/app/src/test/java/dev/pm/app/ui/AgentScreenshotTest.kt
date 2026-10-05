package dev.pm.app.ui

import androidx.compose.material3.Surface
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.model.Conversation
import dev.pm.app.model.Item
import dev.pm.app.model.ToolResult
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The chat's rows and the screen tab, compared with the images in `src/test/screenshots` on every
 * test run; `gradlew recordRoborazziDebug` records them anew. Times are at noon UTC so the day is
 * the same in any zone the run is in.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h640dp")
class AgentScreenshotTest {
    private val conversation =
        Conversation(
            listOf(
                Item.User("u1", "2026-10-01T12:00:00Z", "Run the tests and fix what fails."),
                Item.Thinking("th", "2026-10-01T12:00:01Z", "Start with the failing crate."),
                Item.Tool(
                    "t1",
                    "2026-10-01T12:00:02Z",
                    "Bash",
                    "cargo test --workspace",
                    ToolResult("test result: ok", error = false, truncated = false, full = null),
                ),
                Item.Tool(
                    "t2",
                    "2026-10-01T12:00:03Z",
                    "Bash",
                    "cargo clippy --all-targets",
                    ToolResult("error: unused import", error = true, truncated = false, null),
                ),
                Item.Continuation("c1", "2026-10-02T12:00:00Z", "You have new messages."),
                Item.Continuation("c2", "2026-10-02T12:01:00Z", "You have new messages."),
                Item.Continuation("c3", "2026-10-02T12:02:00Z", "You have new messages."),
                Item.Event("e1", "2026-10-02T12:03:00Z", "interrupted"),
                Item.Tool("t3", "2026-10-02T12:04:00Z", "Read", "src/lib.rs", null),
            ),
            before = null,
        )

    private fun capture() = captureRoboImage {
        PmTheme { Surface { ChatView(conversation, live = true, older = {}, openResult = {}) } }
    }

    @Test fun chat_light() = capture()

    @Test @Config(qualifiers = "+night") fun chat_dark() = capture()

    /** Box drawing and pm's badge glyphs keep to the grid, and the widest row fits the width. */
    @Test
    fun screen() = captureRoboImage {
        PmTheme {
            Surface {
                TerminalView(
                    listOf(
                            "╭${"─".repeat(66)}╮",
                            "│ > fix the failing test${" ".repeat(43)}│",
                            "╰${"─".repeat(66)}╯",
                            "  \uf013 busy  \uf059 asking  \uf1f6 unarmed  \uf252 idle  \uf0e0 2",
                            "  \uf256 blocked  \uf058 ready  \uf04c stalled  \udb81\ude8c dead",
                            "",
                            "",
                        )
                        .joinToString("\n")
                )
            }
        }
    }
}
