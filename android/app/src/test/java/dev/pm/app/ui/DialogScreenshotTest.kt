package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.model.AgentState
import dev.pm.app.model.Dialog
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * A dialog card of each kind, compared with the images in `src/test/screenshots` on every test run;
 * `gradlew recordRoborazziGoogleDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h640dp")
class DialogScreenshotTest {
    @get:Rule val compose = createComposeRule()

    /**
     * The card over `dialogs`, once `shown` is on screen. `tap`, if given, is tapped first, and its
     * answer left on its way.
     */
    private fun card(
        vararg dialogs: Dialog,
        shown: String = dialogs.first().title,
        tap: String? = null,
    ) {
        compose.setContent {
            var answering by remember { mutableStateOf<Answering?>(null) }
            PmTheme(dynamic = false) {
                Surface {
                    DialogCard(
                        dialogs.toList(),
                        answering = answering,
                        interrupting = false,
                        notice = null,
                        answer = { d, c, _, _ -> answering = Answering(d.id, c) },
                        interrupt = {},
                        openTerminal = {},
                    )
                }
            }
        }
        capture(shown, tap)
    }

    private fun capture(shown: String, tap: String? = null) {
        compose.waitUntil(10_000) {
            compose.onAllNodesWithText(shown, substring = true).fetchSemanticsNodes().isNotEmpty()
        }
        if (tap != null) {
            compose.mainClock.autoAdvance = false
            compose.onNodeWithText(tap).performClick()
            compose.mainClock.advanceTimeBy(100)
        }
        compose.onRoot().captureRoboImage()
    }

    /** The review over the whole screen; a plan's Markdown is parsed off the main thread. */
    private fun review(dialog: Dialog, shown: String) {
        compose.setContent {
            PmTheme(dynamic = false) {
                DialogReviewView(
                    remember { DialogForm(dialog) },
                    pendingOn = null,
                    busy = false,
                    notice = null,
                    onChoice = {},
                    sendMessage = {},
                    close = {},
                )
            }
        }
        capture(shown)
    }

    private val question =
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

    private val permission =
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

    private val plan =
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
        )

    private val lone = question.copy(id = "d4", questions = question.questions.take(1))

    @Test fun question_light() = card(question)

    @Test @Config(qualifiers = "+night") fun question_dark() = card(question)

    /** One tap on an option answers a lone question. */
    @Test fun lone_question_light() = card(lone)

    @Test @Config(qualifiers = "+night") fun lone_question_dark() = card(lone)

    @Test fun permission_light() = card(permission)

    @Test @Config(qualifiers = "+night") fun permission_dark() = card(permission)

    /** The tapped choice shows its answer on the way; the others can't be tapped meanwhile. */
    @Test fun permission_answering_light() = card(permission, tap = "Yes")

    @Test fun plan_light() = card(plan)

    @Test @Config(qualifiers = "+night") fun plan_dark() = card(plan)

    /** Several open dialogs: one shown, with steps to the others. */
    @Test fun several_light() = card(permission, plan, lone)

    @Test @Config(qualifiers = "+night") fun several_dark() = card(permission, plan, lone)

    @Test fun plan_review_light() = review(plan, "A form at /login")

    @Test @Config(qualifiers = "+night") fun plan_review_dark() = review(plan, "A form at /login")

    @Test fun form_review_light() = review(question, "Which checks run on push?")

    /** What the user answered, a send that failed, and the composer below them. */
    private fun composer() {
        compose.setContent {
            PmTheme(dynamic = false) {
                Surface {
                    Column {
                        AnsweredRow("You answered: Postgres")
                        OutboxBubble(
                            Outbox.Failed(
                                "Use the cache from main, then run the whole test suite " +
                                    "again before you open the pull request.",
                                "unexpected end of stream on http://127.0.0.1:7843/v1/agents",
                            ),
                            retry = {},
                            edit = {},
                        )
                        Composer(
                            AgentState.Idle,
                            null,
                            sending = false,
                            notice = null,
                            interrupting = false,
                            remember { Drafts() },
                            "app/login/implementer",
                            send = {},
                            interrupt = {},
                            openTerminal = {},
                        )
                    }
                }
            }
        }
        capture("Retry")
    }

    @Test fun composer_light() = composer()

    @Test @Config(qualifiers = "+night") fun composer_dark() = composer()
}
