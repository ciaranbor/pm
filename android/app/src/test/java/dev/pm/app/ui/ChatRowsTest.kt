package dev.pm.app.ui

import dev.pm.app.model.Item
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
}
