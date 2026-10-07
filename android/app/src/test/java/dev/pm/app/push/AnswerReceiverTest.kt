package dev.pm.app.push

import dev.pm.app.api.PmClient
import dev.pm.app.model.Alert
import dev.pm.app.model.Dialog
import dev.pm.app.model.Pairing
import dev.pm.app.model.PushedTransition
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AnswerReceiverTest {
    private val server = MockWebServer().apply { start() }
    private val client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))

    private fun permission(id: String, detail: String) =
        Dialog(
            id,
            "permission",
            tool = "Bash",
            detail = detail,
            choices =
                listOf(
                    Dialog.Choice("yes", "Yes"),
                    Dialog.Choice("always", "Yes, and don't ask again"),
                    Dialog.Choice("no", "No", takesMessage = true),
                ),
        )

    private val first = permission("d1", "npm install stripe")
    private val alert =
        Alert.of(PushedTransition("app", "login", "asking", "implementer"), null, listOf(first))

    private fun dialogs(vararg open: Dialog): MockResponse =
        MockResponse.Builder()
            .body(
                """{"dialogs": [${open.joinToString(",") { """{"id":"${it.id}","kind":"permission","tool":"Bash","detail":"${it.detail}","choices":[{"id":"yes","label":"Yes"},{"id":"no","label":"No"}]}""" }}]}"""
            )
            .build()

    @After
    fun stop() {
        server.close()
    }

    @Test
    fun allow_answers_the_prompt_the_alert_shows_and_leaves_what_was_sent() = runBlocking {
        server.enqueue(MockResponse.Builder().body("""{"answered":true}""").build())
        server.enqueue(dialogs())

        val after =
            AnswerReceiver.answer(client, alert, "yes", "Allowed", settle = 300.milliseconds)
        val posted = server.takeRequest()
        assertEquals("/v1/agents/app/login/implementer/dialog", posted.url.encodedPath)
        assertEquals("""{"id":"d1","choice":"yes"}""", posted.body!!.utf8())
        assertEquals(
            listOf(
                Alert.Line("Allow Bash? npm install stripe"),
                Alert.Line("Allowed", Alert.By.You),
            ),
            after.lines,
        )
        assertNull("nothing left to allow", after.prompt)
        assertTrue(after.settled)
    }

    @Test
    fun an_answer_moves_on_to_the_agents_next_open_prompt() = runBlocking {
        server.enqueue(MockResponse.Builder().body("""{"answered":true}""").build())
        // The answered prompt may still be listed as its record closes; the next opens just after.
        server.enqueue(dialogs(first))
        server.enqueue(dialogs(permission("d2", "rm -rf build")))

        val after = AnswerReceiver.answer(client, alert, "no", "Denied")
        assertEquals("Allow Bash? rm -rf build", after.lines.last().text)
        assertEquals("d2", after.prompt!!.id)
        assertFalse("a prompt still waits", after.settled)
    }

    @Test
    fun a_prompt_answered_elsewhere_or_unknown_says_so_and_one_unanswered_can_be_retried() =
        runBlocking {
            server.enqueue(
                MockResponse.Builder()
                    .code(409)
                    .body("""{"error":"already answered","refused":"answered"}""")
                    .build()
            )
            server.enqueue(dialogs())
            val elsewhere =
                AnswerReceiver.answer(client, alert, "yes", "Allowed", settle = 300.milliseconds)
            assertEquals(Alert.Line("Answered elsewhere", Alert.By.Pm), elsewhere.lines.last())
            assertNull(elsewhere.prompt)
            assertFalse("withdrawn once the server shows it over", elsewhere.settled)

            server.enqueue(
                MockResponse.Builder().code(404).body("""{"error":"no such dialog"}""").build()
            )
            server.enqueue(dialogs())
            val unknown =
                AnswerReceiver.answer(client, alert, "yes", "Allowed", settle = 300.milliseconds)
            assertEquals("It is no longer open", unknown.lines.last().text)

            server.enqueue(
                MockResponse.Builder()
                    .code(409)
                    .body("""{"error":"hook ended","refused":"gone"}""")
                    .build()
            )
            server.enqueue(dialogs())
            val gone =
                AnswerReceiver.answer(client, alert, "yes", "Allowed", settle = 300.milliseconds)
            assertEquals(
                "It can no longer be answered here; answer it at the terminal",
                gone.lines.last().text,
            )
            assertNull(gone.prompt)

            server.enqueue(MockResponse.Builder().code(500).body("""{"error":"boom"}""").build())
            val erred = AnswerReceiver.answer(client, alert, "yes", "Allowed")
            assertEquals(Alert.Line("Allowed", Alert.By.You, "boom"), erred.lines.last())
            assertEquals("still answerable", first, erred.prompt)

            server.close()
            val failed =
                AnswerReceiver.answer(client, alert, "yes", "Allowed", settle = 300.milliseconds)
            assertEquals(
                Alert.Line("Allowed", Alert.By.You, "pm serve unreachable"),
                failed.lines.last(),
            )
            assertEquals("still answerable", first, failed.prompt)
        }
}
