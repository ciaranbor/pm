package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import java.time.Instant
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class DocsUiTest {
    @get:Rule val compose = createComposeRule()

    private val server = MockWebServer()
    private lateinit var client: PmClient

    @Before
    fun start() {
        server.start()
        client = PmClient(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
    }

    @After
    fun stop() {
        server.close()
    }

    private fun projectPage(docsReply: MockResponse) {
        server.enqueue(docsReply)
        val snapshot = Snapshot.parse(SNAPSHOT)
        compose.setContent {
            ScopesList(
                snapshot,
                "app",
                Instant.now(),
                open = {},
                openNotes = {},
                openDocs = if (rememberDocsServed(client, "app")) ({}) else null,
            )
        }
        compose.waitUntil(5_000) { server.requestCount > 0 }
        compose.waitForIdle()
    }

    @Test
    fun a_server_with_docs_offers_them_beside_the_notes() {
        projectPage(MockResponse.Builder().code(200).body("""{"docs":[]}""").build())
        compose.onNodeWithText("Docs").assertExists()
    }

    @Test
    fun a_server_that_predates_docs_offers_only_the_notes() {
        projectPage(
            MockResponse.Builder().code(404).body("""{"error":"no such endpoint"}""").build()
        )
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("Docs").fetchSemanticsNodes().isEmpty()
        }
        compose.onNodeWithText("Notes").assertExists()
    }

    @Test
    fun a_long_doc_draws_what_is_in_view_and_jumps_to_a_section() {
        val doc =
            (1..200).joinToString("\n\n") { n ->
                "## Section $n\n\n" + "Body of section $n. ".repeat(40)
            }
        val topBar = TopBarSlot()
        compose.setContent {
            Column {
                Row { topBar.actions?.invoke(this) }
                DocScreen(
                    viewModel { ReadModel(client) { markdownParts(doc) } },
                    topBar,
                    Modifier.weight(1f),
                )
            }
        }
        compose.waitUntil(10_000) {
            compose.onAllNodesWithText("Section 1").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithText("Section 150").assertDoesNotExist()

        compose.onNodeWithText("Sections").performClick()
        compose.onNodeWithText("Section 150").performScrollTo().performClick()
        compose.waitUntil(10_000) {
            compose
                .onAllNodesWithText("Body of section 150.", substring = true)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    @Test
    fun the_list_reads_again_when_shown_again() {
        val listed = { size: Int ->
            MockResponse.Builder()
                .code(200)
                .body("""{"docs":[{"filename":"todo.md","description":"Tasks.","size":$size}]}""")
                .build()
        }
        server.enqueue(listed(2048))
        server.enqueue(listed(5120))
        var shown by mutableStateOf(true)
        compose.setContent {
            val model = viewModel { ReadModel(client) { docs("app") } }
            if (shown) DocsScreen(model, Instant.now(), open = {})
        }
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("2 KB").fetchSemanticsNodes().isNotEmpty()
        }

        shown = false
        compose.waitForIdle()
        shown = true
        compose.waitUntil(5_000) {
            compose.onAllNodesWithText("5 KB").fetchSemanticsNodes().isNotEmpty()
        }
    }
}
