package dev.pm.app.ui

import dev.pm.app.model.Item
import java.time.Duration
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/**
 * One row of the chat list: a day's divider, an item, a run of pm's wake-ups, or a run of the
 * agent's work (tool calls, with the thinking between them).
 */
sealed interface ChatRow {
    /** Unique within one list, and stable while the conversation grows. */
    val key: String

    data class Day(val date: LocalDate) : ChatRow {
        override val key = "day:$date"
    }

    data class Single(val item: Item) : ChatRow {
        override val key = item.id
    }

    data class Wakes(
        val items: List<Item.Continuation>,
        override val key: String = "wakes:${items.first().id}",
    ) : ChatRow

    /** Tool calls and thinking, in order; at least one tool call. */
    data class Work(val items: List<Item>, override val key: String = "work:${items.first().id}") :
        ChatRow {

        val tools: List<Item.Tool>
            get() = items.filterIsInstance<Item.Tool>()
    }
}

/**
 * The rows `items` show in `zone`: consecutive continuations as one; consecutive tool calls and
 * thinking as one, where the run holds a tool call; and a divider before the first item of each
 * later day. An item's day comes from its `at`; one without stays under the divider before it, and
 * a day earlier than one already shown starts none, so no day appears twice.
 */
fun chatRows(items: List<Item>, zone: ZoneId): List<ChatRow> {
    val rows = ArrayList<ChatRow>()
    var day: LocalDate? = null
    var wakes = ArrayList<Item.Continuation>()
    var work = ArrayList<Item>()
    fun endRuns() {
        if (wakes.isNotEmpty()) rows.add(ChatRow.Wakes(wakes))
        wakes = ArrayList()
        if (work.any { it is Item.Tool }) rows.add(ChatRow.Work(work))
        else work.forEach { rows.add(ChatRow.Single(it)) }
        work = ArrayList()
    }
    for (item in items) {
        val itemDay = instant(item.at)?.atZone(zone)?.toLocalDate()
        if (itemDay != null && (day == null || itemDay > day)) {
            endRuns()
            rows.add(ChatRow.Day(itemDay))
            day = itemDay
        }
        when (item) {
            is Item.Continuation -> {
                if (work.isNotEmpty()) endRuns()
                wakes.add(item)
            }
            is Item.Tool,
            is Item.Thinking -> {
                if (wakes.isNotEmpty()) endRuns()
                work.add(item)
            }
            else -> {
                endRuns()
                rows.add(ChatRow.Single(item))
            }
        }
    }
    endRuns()
    return rows
}

/**
 * `rows` with each run keyed as it was when first shown, by `known` (item id to run key, kept
 * across calls): a run an older page extends at its start keeps its key, so the list stays anchored
 * on it. A key already taken in `rows` is not reused.
 */
fun keepRunKeys(rows: List<ChatRow>, known: MutableMap<String, String>): List<ChatRow> {
    val used = HashSet<String>()
    return rows.map { row ->
        val items =
            when (row) {
                is ChatRow.Wakes -> row.items
                is ChatRow.Work -> row.items
                else -> return@map row.also { used.add(it.key) }
            }
        val key = items.firstNotNullOfOrNull { known[it.id] }?.takeIf { it !in used } ?: row.key
        used.add(key)
        items.forEach { known[it.id] = key }
        when (row) {
            is ChatRow.Wakes -> row.copy(key = key)
            is ChatRow.Work -> row.copy(key = key)
        }
    }
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

/** What a run of tool calls did, in words: `Ran 3 commands, read 2 files`. */
fun workSummary(tools: List<Item.Tool>): String {
    val counts = tools.groupingBy { kindOf(it.name) }.eachCount()
    return ToolKind.entries
        .mapNotNull { kind -> counts[kind]?.let { kind.phrase(it) } }
        .joinToString(", ")
        .replaceFirstChar { it.uppercase() }
}

private enum class ToolKind(val one: String, val many: String) {
    Command("ran 1 command", "ran %d commands"),
    Read("read 1 file", "read %d files"),
    Edit("edited 1 file", "edited %d files"),
    Search("searched once", "searched %d times"),
    Fetch("fetched 1 page", "fetched %d pages"),
    Agent("ran 1 subagent", "ran %d subagents"),
    Other("used 1 tool", "used %d tools");

    fun phrase(count: Int) = if (count == 1) one else many.replace("%d", "$count")
}

/** The kind of a tool by its name, as Claude Code, codex and opencode name theirs. */
private fun kindOf(name: String): ToolKind =
    when (name.lowercase()) {
        "bash",
        "shell",
        "local_shell",
        "exec",
        "exec_command",
        "powershell" -> ToolKind.Command
        "read",
        "view" -> ToolKind.Read
        "edit",
        "multiedit",
        "write",
        "notebookedit",
        "apply_patch",
        "patch" -> ToolKind.Edit
        "grep",
        "glob",
        "list",
        "ls",
        "websearch",
        "codesearch" -> ToolKind.Search
        "webfetch" -> ToolKind.Fetch
        "task",
        "agent" -> ToolKind.Agent
        else -> ToolKind.Other
    }

/** How long a tool call took, where both its call and its result are timed. */
fun duration(tool: Item.Tool): Duration? {
    val start = instant(tool.at) ?: return null
    val end = instant(tool.result?.at) ?: return null
    return Duration.between(start, end).takeIf { !it.isNegative }
}

/** A duration as a tool line shows it: `0.4s`, `12s`, `3m 4s`, `1h 2m`. */
fun durationLabel(duration: Duration): String {
    val millis = duration.toMillis()
    val secs = duration.seconds
    return when {
        millis < 10_000 -> String.format(Locale.ROOT, "%.1fs", millis / 1000.0)
        secs < 60 -> "${secs}s"
        secs < 3600 -> "${secs / 60}m ${secs % 60}s"
        else -> "${secs / 3600}h ${secs % 3600 / 60}m"
    }
}

internal fun instant(at: String?): Instant? = at?.let {
    runCatching { Instant.parse(it) }.getOrNull()
}
