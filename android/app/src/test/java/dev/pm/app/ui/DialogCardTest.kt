package dev.pm.app.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasContentDescription
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import dev.pm.app.model.Dialog
import dev.pm.app.model.DialogAnswer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class DialogCardTest {
    @get:Rule val compose = createComposeRule()

    private val colour =
        Dialog.Question(
            "Colour?",
            options = listOf(Dialog.Option("Red"), Dialog.Option("Blue")),
            custom = true,
        )
    private val pets =
        Dialog.Question(
            "Pets?",
            options = listOf(Dialog.Option("Cat"), Dialog.Option("Dog"), Dialog.Option("Fish")),
            multiSelect = true,
            custom = true,
        )
    private val questionChoices =
        listOf(Dialog.Choice(Dialog.ANSWER, "Submit answers"), Dialog.Choice("decline", "Decline"))
    private val form = Dialog("d1", "question", questions = listOf(colour, pets))

    private val prompt =
        Dialog(
            "d2",
            "permission",
            tool = "Bash",
            detail = "rm -rf build",
            choices =
                listOf(
                    Dialog.Choice("allow", "Yes"),
                    Dialog.Choice("always", "Yes, and don't ask again for Bash(rm:*)"),
                    Dialog.Choice("deny", "No", true),
                ),
        )

    private val plan =
        Dialog(
            "d3",
            "plan",
            detail = "Add login",
            plan = "# Add login\n\nA form at the login page",
            choices =
                listOf(
                    Dialog.Choice("auto", "Yes, and use auto mode"),
                    Dialog.Choice("manual", "Yes, manually approve edits"),
                    Dialog.Choice("keep-planning", "No, keep planning", true),
                ),
        )

    private val lone =
        Dialog("q1", "question", questions = listOf(colour), choices = questionChoices)

    private val sent = mutableListOf<DialogAnswer>()

    /** The card over `dialogs`, its answers left on their way once sent. */
    private fun card(vararg dialogs: Dialog, notice: DialogNotice? = null) {
        compose.setContent {
            var answering by remember { mutableStateOf<Answering?>(null) }
            PmTheme {
                DialogCard(
                    dialogs.toList(),
                    answering,
                    interrupting = false,
                    notice = notice,
                    answer = { d, c, a, m ->
                        sent += DialogAnswer(d.id, c, a, m)
                        answering = Answering(d.id, c)
                    },
                    interrupt = {},
                    openTerminal = {},
                )
            }
        }
    }

    @Test
    fun answers_need_every_question_and_own_words_replace_or_join_the_picks() {
        assertNull(answersOf(form, mapOf("Colour?" to setOf("Red")), emptyMap()))
        assertEquals(
            mapOf("Colour?" to listOf("Teal"), "Pets?" to listOf("Cat", "Fish", "Hamster")),
            answersOf(
                form,
                mapOf("Colour?" to setOf("Red"), "Pets?" to setOf("Fish", "Cat")),
                mapOf("Colour?" to " Teal ", "Pets?" to "Hamster"),
            ),
        )
        assertNull(
            "blank words answer nothing",
            answersOf(form, mapOf("Pets?" to setOf("Cat")), mapOf("Colour?" to "  ")),
        )
        val closed = form.copy(questions = listOf(colour.copy(custom = false)))
        assertNull("no own words", answersOf(closed, emptyMap(), mapOf("Colour?" to "Teal")))
    }

    @Test
    fun a_choice_that_takes_a_message_asks_for_it_before_answering() {
        card(prompt)
        compose.onNodeWithText("No").performClick()
        assertEquals(emptyList<DialogAnswer>(), sent)
        compose
            .onNodeWithText("Tell the agent what to do instead (optional)")
            .performTextInput("clean with cargo")
        compose.onNodeWithText("No").performClick()
        assertEquals(listOf(DialogAnswer("d2", "deny", emptyMap(), "clean with cargo")), sent)
    }

    @Test
    fun only_the_tapped_choice_shows_its_answer_on_the_way_and_nothing_more_is_sent() {
        card(prompt)
        compose.onNodeWithText("Yes").performClick()

        compose
            .onNode(hasContentDescription("Yes") and hasStateDescription("In progress"))
            .assertExists()
        compose.onNodeWithContentDescription("More choices").assertIsNotEnabled()
        compose.onNodeWithText("No").assertIsNotEnabled()
        compose.onNodeWithContentDescription("Yes").performClick()
        assertEquals(listOf("allow"), sent.map { it.choice })
    }

    @Test
    fun the_menu_holds_the_main_choice_whole_and_the_choices_between() {
        card(plan)
        compose.onNodeWithText("Yes").assertExists()
        compose.onNodeWithContentDescription("More choices").performClick()
        compose.onNodeWithText("Yes, and use auto mode").assertExists()
        compose.onNodeWithText("Yes, manually approve edits").performClick()
        assertEquals(listOf("manual"), sent.map { it.choice })
    }

    @Test
    fun a_lone_question_is_answered_by_a_tap_on_an_option() {
        card(Dialog("q1", "question", questions = listOf(colour), choices = questionChoices))
        compose.onNodeWithText("Blue").performClick()
        assertEquals(
            listOf(DialogAnswer("q1", Dialog.ANSWER, mapOf("Colour?" to listOf("Blue")))),
            sent,
        )
    }

    @Test
    fun a_lone_question_is_answered_in_own_words_through_its_review() {
        card(lone)
        compose.onNodeWithText("Other answer").performClick()
        compose.onNodeWithText("Something else").performTextInput("Teal")
        compose.onNodeWithText("Submit answers").performClick()
        assertEquals(
            listOf(DialogAnswer("q1", Dialog.ANSWER, mapOf("Colour?" to listOf("Teal")))),
            sent,
        )
    }

    @Test
    fun a_lone_question_can_be_declined() {
        card(lone)
        compose.onNodeWithText("Decline").performClick()
        assertEquals(listOf(DialogAnswer("q1", "decline")), sent)
    }

    @Test
    fun a_refusal_shows_on_its_own_dialog_only() {
        card(prompt, plan, notice = DialogNotice("d3", "Answered elsewhere"))
        compose.onNodeWithText("Answered elsewhere").assertDoesNotExist()
        compose.onNodeWithContentDescription("Next dialog").performClick()
        compose.onNodeWithText("Answered elsewhere").assertExists()
    }

    @Test
    fun a_refusal_whose_dialog_is_gone_shows_on_the_one_shown() {
        card(prompt, notice = DialogNotice("gone", "Answered elsewhere"))
        compose.onNodeWithText("Answered elsewhere").assertExists()
    }

    @Test
    fun a_form_is_filled_in_its_review_and_submitted_once_complete() {
        card(form.copy(choices = questionChoices))
        compose.onNodeWithText("Answer").performClick()
        compose.onNodeWithText("Submit answers").assertIsNotEnabled()
        compose.onNodeWithText("Red").performClick()
        compose.onNodeWithText("Dog").performClick()
        compose.onNodeWithText("Submit answers").assertIsEnabled().performClick()
        assertEquals(
            listOf(
                DialogAnswer(
                    "d1",
                    Dialog.ANSWER,
                    mapOf("Colour?" to listOf("Red"), "Pets?" to listOf("Dog")),
                )
            ),
            sent,
        )
    }

    @Test
    fun a_plan_is_read_whole_and_approved_from_its_review() {
        card(plan)
        compose.onNodeWithText("Review").performClick()
        compose.waitUntil(5_000) {
            compose
                .onAllNodesWithText("A form at the login page", substring = true)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
        compose.onNodeWithText("Yes, and use auto mode").performClick()
        assertEquals(listOf("auto"), sent.map { it.choice })
    }

    @Test
    fun each_of_several_dialogs_is_reached_and_answered_as_itself() {
        card(prompt, plan)
        compose.onNodeWithContentDescription("Dialog 1 of 2").assertExists()
        compose.onNodeWithText("Allow Bash?").assertExists()
        compose.onNodeWithContentDescription("Next dialog").performClick()
        compose.onNodeWithText("Approve the plan?").assertExists()
        compose.onNodeWithText("Yes").performClick()
        assertEquals(listOf(DialogAnswer("d3", "auto")), sent)
    }
}
