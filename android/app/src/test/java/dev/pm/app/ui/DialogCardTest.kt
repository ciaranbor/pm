package dev.pm.app.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import dev.pm.app.model.Dialog
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
    private val form = Dialog("d1", "question", questions = listOf(colour, pets))

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
        val sent = mutableListOf<Triple<String, Map<String, List<String>>, String?>>()
        val prompt =
            Dialog(
                "d2",
                "permission",
                tool = "Bash",
                detail = "rm -rf build",
                choices = listOf(Dialog.Choice("allow", "Yes"), Dialog.Choice("deny", "No", true)),
            )
        compose.setContent {
            PmTheme {
                DialogCard(
                    prompt,
                    false,
                    false,
                    null,
                    answer = { c, a, m -> sent += Triple(c, a, m) },
                    {},
                    {},
                )
            }
        }
        compose.onNodeWithText("No").performClick()
        assertEquals(emptyList<Any>(), sent)
        compose
            .onNodeWithText("Tell the agent what to do instead (optional)")
            .performTextInput("clean with cargo")
        compose.onNodeWithText("No").performClick()
        assertEquals(
            listOf(Triple("deny", emptyMap<String, List<String>>(), "clean with cargo")),
            sent,
        )
    }

    @Test
    fun only_the_tapped_choice_shows_its_answer_on_the_way_and_nothing_more_is_sent() {
        val sent = mutableListOf<String>()
        val prompt =
            Dialog(
                "d3",
                "permission",
                tool = "Bash",
                choices =
                    listOf(
                        Dialog.Choice("allow", "Yes"),
                        Dialog.Choice("always", "Always"),
                        Dialog.Choice("deny", "No"),
                    ),
            )
        compose.setContent {
            var answering by remember { mutableStateOf(false) }
            PmTheme {
                DialogCard(
                    prompt,
                    answering,
                    false,
                    null,
                    answer = { c, _, _ ->
                        sent += c
                        answering = true
                    },
                    {},
                    {},
                )
            }
        }
        compose.onNodeWithText("Yes").performClick()

        compose.onNode(hasText("Yes") and hasStateDescription("In progress")).assertExists()
        compose.onNodeWithText("Always").assertIsNotEnabled()
        compose.onNodeWithText("No").assertIsNotEnabled()
        compose.onNodeWithContentDescription("Interrupt").assertIsNotEnabled()
        compose.onNodeWithText("Yes").performClick()
        assertEquals(listOf("allow"), sent)
    }
}
