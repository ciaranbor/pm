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
 * The chat's rows and the terminal sheet, compared with the images in `src/test/screenshots` on
 * every test run; `gradlew recordRoborazziGoogleDebug` records them anew. Times are at noon UTC so
 * the day is the same in any zone the run is in.
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
                // Not running: a spinner never settles under this composable-only capture.
                Item.Tool(
                    "t3",
                    "2026-10-02T12:04:00Z",
                    "Read",
                    "src/lib.rs",
                    ToolResult("pub fn run() {}", error = false, truncated = false, full = null),
                ),
            ),
            before = null,
        )

    private fun capture() = captureRoboImage {
        PmTheme { Surface { ChatView(conversation, live = true, older = {}) } }
    }

    @Test fun chat_light() = capture()

    @Test @Config(qualifiers = "+night") fun chat_dark() = capture()

    /** Claude Code's folder trust dialog, as `screen` returns it from an 80-column pane. */
    private val trust =
        listOf(
                "",
                "─".repeat(80),
                " Accessing workspace:",
                "",
                " /Users/me/Projects/应用 🙂",
                "",
                " Quick safety check: Is this a project you created or one you trust? (Like your",
                " own code, a well-known open source project, or work from your team). If not,",
                " take a moment to review what's in this folder first.",
                "",
                " Claude Code'll be able to read, edit, and execute files here.",
                "",
                " \uf059 Security guide: https://code.claude.com/docs/en/security",
                "",
                " ❯ 1. Yes, I trust this folder",
                "   2. No, exit",
                "",
                " Enter to confirm · Esc to cancel",
            )
            .plus(List(6) { "" })
            .joinToString("\n")

    private fun terminal() = captureRoboImage {
        PmTheme { Surface { ScreenPanel(trust, null, emptyList(), press = {}, type = { true }) } }
    }

    /** The pane's grid at the sheet's width, wide characters on two cells, and one row of keys. */
    @Test fun terminal_light() = terminal()

    @Test @Config(qualifiers = "+night") fun terminal_dark() = terminal()
}
