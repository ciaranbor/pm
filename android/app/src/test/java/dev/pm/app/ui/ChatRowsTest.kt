package dev.pm.app.ui

import dev.pm.app.model.Item
import dev.pm.app.model.ToolResult
import java.time.LocalDate
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Test

class ChatRowsTest {
    private val zone = ZoneId.of("Europe/Dublin")

    private fun user(id: String, at: String? = null) = Item.User(id, at, id)

    private fun wake(id: String, at: String? = null) = Item.Continuation(id, at, "wake $id")

    @Test
    fun consecutive_wakes_are_one_row_and_a_day_divider_starts_each_later_day() {
        val rows =
            chatRows(
                listOf(
                    user("u1", "2026-10-03T22:00:00Z"),
                    wake("w1", "2026-10-03T22:10:00Z"),
                    wake("w2", "2026-10-03T22:20:00Z"),
                    // 23:30Z is past midnight in Dublin (UTC+1): a new day.
                    wake("w3", "2026-10-03T23:30:00Z"),
                    wake("w4"),
                    user("u2"),
                    wake("w5", "2026-10-04T09:00:00Z"),
                ),
                zone,
            )
        assertEquals(
            listOf(
                ChatRow.Day(LocalDate.of(2026, 10, 3)),
                ChatRow.Single(user("u1", "2026-10-03T22:00:00Z")),
                ChatRow.Wakes(
                    listOf(wake("w1", "2026-10-03T22:10:00Z"), wake("w2", "2026-10-03T22:20:00Z"))
                ),
                ChatRow.Day(LocalDate.of(2026, 10, 4)),
                ChatRow.Wakes(listOf(wake("w3", "2026-10-03T23:30:00Z"), wake("w4"))),
                ChatRow.Single(user("u2")),
                ChatRow.Wakes(listOf(wake("w5", "2026-10-04T09:00:00Z"))),
            ),
            rows,
        )
    }

    @Test
    fun a_day_out_of_order_starts_no_second_divider() {
        val rows =
            chatRows(
                listOf(
                    user("a", "2026-10-04T09:00:00Z"),
                    user("b", "2026-10-03T09:00:00Z"),
                    user("c", "2026-10-04T10:00:00Z"),
                ),
                zone,
            )
        assertEquals(listOf("day:2026-10-04", "a", "b", "c"), rows.map { it.key })
    }

    @Test
    fun what_is_new_counts_the_items_after_the_last_one_seen() {
        val items = listOf(user("a"), user("b"), user("c"))
        assertEquals(2, newSince(items, "a"))
        assertEquals(0, newSince(items, "c"))
        assertEquals(0, newSince(items, "gone"))
    }

    private fun tool(id: String, name: String = "Bash", result: ToolResult? = ok) =
        Item.Tool(id, null, name, id, result)

    private val ok = ToolResult("", error = false, truncated = false, full = null)

    @Test
    fun tool_calls_and_the_thinking_between_them_are_one_row_and_thinking_alone_is_not() {
        val think = Item.Thinking("th", null, "hmm")
        val alone = Item.Thinking("th2", null, "hmm")
        val rows =
            chatRows(
                listOf(tool("t1"), think, tool("t2"), user("u"), alone, wake("w"), tool("t3")),
                zone,
            )
        assertEquals(
            listOf(
                ChatRow.Work(listOf(tool("t1"), think, tool("t2"))),
                ChatRow.Single(user("u")),
                ChatRow.Single(alone),
                ChatRow.Wakes(listOf(wake("w"))),
                ChatRow.Work(listOf(tool("t3"))),
            ),
            rows,
        )
    }

    @Test
    fun a_run_says_what_its_calls_did() {
        assertEquals(
            "Ran 2 commands, read 1 file, used 1 tool",
            workSummary(
                listOf(
                    tool("a"),
                    tool("b", "bash", null),
                    tool("c", "Read"),
                    tool("d", "mcp__x", null),
                )
            ),
        )
        assertEquals("Edited 2 files", workSummary(listOf(tool("e", "Edit"), tool("f", "Write"))))
    }

    @Test
    fun a_call_lasts_from_its_own_time_to_its_results() {
        val timed =
            Item.Tool(
                "t",
                "2026-10-03T22:00:00Z",
                "Bash",
                "ls",
                ok.copy(at = "2026-10-03T22:03:04.500Z"),
            )
        assertEquals("3m 4s", duration(timed)?.let(::durationLabel))
        assertEquals(null, duration(timed.copy(at = null)))
        assertEquals("0.4s", durationLabel(java.time.Duration.ofMillis(400)))
        assertEquals("12s", durationLabel(java.time.Duration.ofSeconds(12)))
        assertEquals("1h 2m", durationLabel(java.time.Duration.ofMinutes(62)))
    }

    @Test
    fun a_run_extended_at_its_start_by_an_older_page_keeps_its_key() {
        val known = HashMap<String, String>()
        val first = keepRunKeys(chatRows(listOf(tool("t2"), tool("t3"), user("u")), zone), known)
        val paged =
            keepRunKeys(
                chatRows(listOf(user("old"), tool("t1"), tool("t2"), tool("t3"), user("u")), zone),
                known,
            )
        assertEquals(first.first().key, paged[1].key)
        assertEquals(listOf("old", "work:t2", "u"), paged.map { it.key })
    }
}
