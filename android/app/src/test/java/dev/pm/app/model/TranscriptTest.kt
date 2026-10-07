package dev.pm.app.model

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TranscriptTest {
    private fun page(json: String) = Json {
        ignoreUnknownKeys = true
    }
        .decodeFromString(TranscriptPage.serializer(), json)

    @Test
    fun parses_each_kind_and_skips_unknown_ones() {
        val page =
            page(
                """
            {"version":1,"harness":"claude-code","before":"c0","after":"c9","items":[
              {"id":"1","at":"2026-10-02T09:00:00Z","kind":"user","text":"hi"},
              {"id":"2","at":null,"kind":"assistant","text":"**hello**"},
              {"id":"3","kind":"thinking","text":"hmm"},
              {"id":"4","kind":"tool","name":"Bash","input":"cargo test",
               "result":{"text":"ok","error":true,"truncated":true,"full":"r4"}},
              {"id":"5","kind":"tool","name":"Read","input":"src/lib.rs","result":null},
              {"id":"12","kind":"tool","name":"Bash","input":"sleep 99","result":null,"unfinished":true},
              {"id":"6","kind":"continuation","text":"You have new messages"},
              {"id":"7","kind":"compaction","summary":null},
              {"id":"8","kind":"event","text":"interrupted"},
              {"id":"10","kind":"event","text":"API Error: 529","failure":true},
              {"id":"11","kind":"tool","name":"Bash","input":"ls",
               "result":{"text":"","error":false,"truncated":false,"at":"2026-10-02T09:00:04Z"}},
              {"id":"9","kind":"hologram","text":"?"}
            ]}
            """
            )
        val items = Transcripts.items(page.items)
        assertEquals(
            listOf(
                Item.User("1", "2026-10-02T09:00:00Z", "hi"),
                Item.Assistant("2", null, "**hello**"),
                Item.Thinking("3", null, "hmm"),
                Item.Tool(
                    "4",
                    null,
                    "Bash",
                    "cargo test",
                    ToolResult("ok", error = true, truncated = true, full = "r4"),
                ),
                Item.Tool("5", null, "Read", "src/lib.rs", null),
                Item.Tool("12", null, "Bash", "sleep 99", null, unfinished = true),
                Item.Continuation("6", null, "You have new messages"),
                Item.Compaction("7", null, null),
                Item.Event("8", null, "interrupted"),
                Item.Event("10", null, "API Error: 529", failure = true),
                Item.Tool(
                    "11",
                    null,
                    "Bash",
                    "ls",
                    ToolResult("", false, false, null, at = "2026-10-02T09:00:04Z"),
                ),
            ),
            items,
        )
        assertEquals("c0", page.before)
        assertEquals("c9", page.after)
    }

    private fun user(id: String, text: String = id): Item = Item.User(id, null, text)

    @Test
    fun an_item_sent_again_replaces_the_one_held_in_place() {
        val running = Item.Tool("t", null, "Bash", "ls", null)
        val done = Item.Tool("t", null, "Bash", "ls", ToolResult("a b", false, false, null))
        val held =
            Conversation().replacedBy(listOf(user("1"), running), before = "c0", after = "c1")
        val next = held.appended(listOf(done, user("2")), after = "c2")
        assertEquals(listOf(user("1"), done, user("2")), next.items)
        assertEquals("c2", next.after)
        assertEquals("c0", next.before)
    }

    @Test
    fun an_older_page_goes_first_and_a_reset_replaces_everything() {
        val held =
            Conversation().replacedBy(listOf(user("3"), user("4")), before = "c2", after = "c4")
        val paged = held.prepended(listOf(user("1"), user("2"), user("3", "stale")), before = null)
        assertEquals(listOf(user("1"), user("2"), user("3"), user("4")), paged.items)
        assertNull(paged.before)

        val reset = paged.replacedBy(listOf(user("a")), before = "x", after = "y")
        assertEquals(listOf(user("a")), reset.items)
        assertEquals("x", reset.before)
    }

    @Test
    fun an_item_without_an_id_is_skipped() {
        assertNull(
            Transcripts.item(
                Json.decodeFromString(JsonObject.serializer(), """{"kind":"user","text":"x"}""")
            )
        )
    }
}

class PairingTest {
    @Test
    fun reads_the_qr_codes_json_and_the_printed_lines() {
        val expected = Pairing("https://mac.tail.ts.net", "pixel", "abc123")
        assertEquals(
            expected,
            Pairing.parse(
                """{"url":"https://mac.tail.ts.net/","device":"pixel","token":"abc123"}"""
            ),
        )
        assertEquals(
            expected,
            Pairing.parse(
                "url:    https://mac.tail.ts.net\ndevice: pixel\ntoken:  abc123\nThe token is shown only now"
            ),
        )
        assertNull(Pairing.parse("""{"url":"ftp://x","device":"pixel","token":"t"}"""))
        assertNull(Pairing.parse("""{"url":"https://x","device":"","token":"t"}"""))
        assertNull(Pairing.parse("WIFI:S:home;;"))
    }
}
