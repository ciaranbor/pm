package dev.pm.app.ui

import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import dev.pm.app.model.AgentDefinition
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class AgentFormsTest {
    @get:Rule val compose = createComposeRule()

    private val definitions =
        listOf(
            AgentDefinition("plain", "no definition: a vanilla session", "claude-code"),
            AgentDefinition("reviewer", "Reviews code", "codex", "gpt-5"),
        )

    @Test
    fun the_name_follows_the_definition_until_typed_and_an_existing_name_is_explained() {
        var submitted: Action.Spawn? = null
        compose.setContent {
            PmTheme {
                AddAgentForm(
                    ReadState.Shown(definitions),
                    retry = {},
                    "app",
                    "login",
                    existing = listOf("implementer"),
                    submit = { submitted = it },
                    dismiss = {},
                )
            }
        }

        compose.onNodeWithText("codex · gpt-5").assertExists()
        compose.onNode(hasSetTextAction() and hasText("plain")).assertExists()
        compose.onNodeWithText("reviewer").performClick()
        val name = hasSetTextAction() and hasText("Name")
        compose.onNode(hasSetTextAction() and hasText("reviewer")).assertExists()

        compose.onNode(name).performTextReplacement("bad name")
        compose.onNodeWithText("Letters, digits, - and _ only").assertExists()
        compose.onNodeWithText("Add").assertIsNotEnabled()

        compose.onNode(name).performTextReplacement("implementer")
        compose.onNodeWithText("implementer exists", substring = true).assertExists()
        compose.onNodeWithText("Add").performClick()
        assertEquals(
            Action.Spawn("app", "login", "implementer", null, null, existing = true),
            submitted,
        )

        compose.onNode(name).performTextReplacement("second")
        compose.onNodeWithText("Add").performClick()
        assertEquals(Action.Spawn("app", "login", "second", "reviewer", null), submitted)
    }

    @Test
    fun a_definition_config_cant_resolve_is_shown_but_cant_be_picked() {
        val broken = AgentDefinition("broken", error = "harness 'x' is not supported yet")
        compose.setContent {
            PmTheme {
                AddAgentForm(
                    ReadState.Shown(listOf(broken) + definitions),
                    {},
                    "app",
                    "login",
                    emptyList(),
                    {},
                    {},
                )
            }
        }

        compose.onNodeWithText("harness 'x' is not supported yet").assertExists()
        compose.onNode(hasSetTextAction() and hasText("plain")).assertExists()
        compose.onNodeWithText("broken").performClick()
        compose.onNode(hasSetTextAction() and hasText("plain")).assertExists()
    }

    @Test
    fun a_server_without_definitions_asks_for_an_update_with_no_retry() {
        compose.setContent {
            PmTheme { AddAgentForm(ReadState.Shown(null), {}, "app", "login", emptyList(), {}, {}) }
        }

        compose.onNodeWithText("older than this app", substring = true).assertExists()
        compose.onNodeWithText("Retry").assertDoesNotExist()
        compose.onNodeWithText("Add").assertIsNotEnabled()
    }

    @Test
    fun a_fork_name_skips_taken_ones_and_a_taken_one_is_refused_inline() {
        compose.setContent {
            PmTheme {
                ForkDialog(
                    "app",
                    "login",
                    "implementer",
                    existing = listOf("implementer", "implementer-fork", "reviewer"),
                    submit = {},
                    dismiss = {},
                )
            }
        }

        compose.onNode(hasSetTextAction() and hasText("implementer-fork-2")).assertExists()
        compose.onNode(hasSetTextAction()).performTextReplacement("reviewer")
        compose.onNodeWithText("reviewer exists").assertExists()
        compose.onNodeWithText("Fork").assertIsNotEnabled()
    }
}
