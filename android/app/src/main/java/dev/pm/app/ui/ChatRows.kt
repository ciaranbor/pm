package dev.pm.app.ui

import dev.pm.app.model.Item
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** One row of the chat list: a day's divider, an item, or a run of pm's wake-ups. */
sealed interface ChatRow {
    /** Unique within one list, and stable while the conversation grows. */
    val key: String

    data class Day(val date: LocalDate) : ChatRow {
        override val key = "day:$date"
    }

    data class Single(val item: Item) : ChatRow {
        override val key = item.id
    }

    data class Wakes(val items: List<Item.Continuation>) : ChatRow {
        override val key = "wakes:${items.first().id}"
    }
}

/**
 * The rows `items` show in `zone`: consecutive continuations as one, and a divider before the first
 * item of each later day. An item's day comes from its `at`; one without stays under the divider
 * before it, and a day earlier than one already shown starts none, so no day appears twice.
 */
fun chatRows(items: List<Item>, zone: ZoneId): List<ChatRow> {
    val rows = ArrayList<ChatRow>()
    var day: LocalDate? = null
    var wakes = ArrayList<Item.Continuation>()
    fun endWakes() {
        if (wakes.isNotEmpty()) rows.add(ChatRow.Wakes(wakes))
        wakes = ArrayList()
    }
    for (item in items) {
        val date = item.at?.let { runCatching { Instant.parse(it) }.getOrNull() }?.atZone(zone)
        val itemDay = date?.toLocalDate()
        if (itemDay != null && (day == null || itemDay > day)) {
            endWakes()
            rows.add(ChatRow.Day(itemDay))
            day = itemDay
        }
        if (item is Item.Continuation) {
            wakes.add(item)
        } else {
            endWakes()
            rows.add(ChatRow.Single(item))
        }
    }
    endWakes()
    return rows
}

/** How a day's divider names it, seen on `today`. */
fun dayLabel(date: LocalDate, today: LocalDate): String =
    when (date) {
        today -> "Today"
        today.minusDays(1) -> "Yesterday"
        else -> date.format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM))
    }

/** The items after the one with id `seen`; none when `seen` is not held. */
fun newSince(items: List<Item>, seen: String?): Int {
    val at = items.indexOfLast { it.id == seen }
    return if (at < 0) 0 else items.size - 1 - at
}
