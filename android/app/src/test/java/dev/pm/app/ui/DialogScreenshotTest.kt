package dev.pm.app.ui

import androidx.compose.material3.Surface
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.model.Dialog
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A dialog card of each kind, compared with the images in `src/test/screenshots` on every test run;
 * `gradlew recordRoborazziDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h640dp")
class DialogScreenshotTest {
    @get:Rule val compose = createComposeRule()

    /** Once `shown` is on screen: a plan's Markdown is parsed off the main thread. */
    private fun dialog(dialog: Dialog, shown: String = dialog.choices.first().label) {
        compose.setContent {
            PmTheme {
                Surface {
                    DialogCard(
                        dialog,
                        answering = false,
                        notice = null,
                        answer = { _, _, _ -> },
                        {},
                    )
                }
            }
        }
        compose.waitUntil(10_000) {
            compose.onAllNodesWithText(shown, substring = true).fetchSemanticsNodes().isNotEmpty()
        }
        compose.onRoot().captureRoboImage()
    }

    @Test
    fun question() =
        dialog(
            Dialog(
                "d1",
                "question",
                questions =
                    listOf(
                        Dialog.Question(
                            "Which database should the cache use?",
                            "Cache",
                            listOf(
                                Dialog.Option("SQLite", "One file, no server"),
                                Dialog.Option("Postgres", "The app's own database"),
                            ),
                            custom = true,
                        ),
                        Dialog.Question(
                            "Which checks run on push?",
                            "CI",
                            listOf(Dialog.Option("Tests"), Dialog.Option("Clippy")),
                            multiSelect = true,
                        ),
                    ),
                choices =
                    listOf(
                        Dialog.Choice(Dialog.ANSWER, "Submit answers"),
                        Dialog.Choice("decline", "Decline to answer"),
                    ),
            )
        )

    @Test
    @Config(qualifiers = "+night")
    fun permission() =
        dialog(
            Dialog(
                "d2",
                "permission",
                tool = "Bash",
                detail = "cargo publish --dry-run",
                choices =
                    listOf(
                        Dialog.Choice("allow", "Yes"),
                        Dialog.Choice(
                            "always",
                            "Yes, and don't ask again for Bash(cargo publish:*)",
                        ),
                        Dialog.Choice("deny", "No", takesMessage = true),
                    ),
            )
        )

    @Test
    fun plan() =
        dialog(
            Dialog(
                "d3",
                "plan",
                detail = "Add login",
                plan = "# Add login\n\n1. A form at `/login`\n2. **Sessions** in a cookie",
                choices =
                    listOf(
                        Dialog.Choice("auto", "Yes, and use auto mode"),
                        Dialog.Choice("manual", "Yes, manually approve edits"),
                        Dialog.Choice("keep-planning", "No, keep planning", takesMessage = true),
                    ),
            ),
            shown = "A form at",
        )
}
