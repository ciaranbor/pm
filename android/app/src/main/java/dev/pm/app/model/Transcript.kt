package dev.pm.app.model

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * An agent's conversation as `pm serve` reads it from the harness's transcript (`GET
 * /v1/agents/{p}/{scope}/{agent}/transcript`, and `transcript` events on a watched event stream).
 * Items are oldest first, keyed by a stable id: one that comes again replaces the one held.
 */
sealed interface Item {
    val id: String
    val at: String?

    data class User(override val id: String, override val at: String?, val text: String) : Item

    data class Assistant(override val id: String, override val at: String?, val text: String) : Item

    data class Thinking(override val id: String, override val at: String?, val text: String) : Item

    data class Tool(
        override val id: String,
        override val at: String?,
        val name: String,
        val input: String,
        val result: ToolResult?,
        /** No result, and the conversation moved on: it will not finish. */
        val unfinished: Boolean = false,
    ) : Item

    /** pm waking the agent (a Stop-hook continuation), not the human. */
    data class Continuation(override val id: String, override val at: String?, val text: String) :
        Item

    data class Compaction(override val id: String, override val at: String?, val summary: String?) :
        Item

    /** Anything else the harness logged: an interrupt, an API error; `failure` when it failed. */
    data class Event(
        override val id: String,
        override val at: String?,
        val text: String,
        val failure: Boolean = false,
    ) : Item
}

/** `full` names the whole output when `text` was cut short; `at` is when the call returned. */
data class ToolResult(
    val text: String,
    val error: Boolean,
    val truncated: Boolean,
    val full: String?,
    val at: String? = null,
)

@Serializable
data class TranscriptPage(
    val version: Int = 1,
    val harness: String? = null,
    val items: List<JsonObject> = emptyList(),
    val before: String? = null,
    val after: String? = null,
)

@Serializable
data class TranscriptEvent(
    val project: String,
    val scope: String,
    val agent: String,
    val reset: Boolean = false,
    val items: List<JsonObject> = emptyList(),
    val after: String? = null,
    val before: String? = null,
)

object Transcripts {
    /** The item `raw` describes; `null` for a kind this app doesn't know. */
    fun item(raw: JsonObject): Item? {
        fun str(key: String, from: JsonObject = raw) =
            from[key]?.let { runCatching { it.jsonPrimitive.contentOrNull }.getOrNull() }
        val id = str("id") ?: return null
        val at = str("at")
        val text = str("text").orEmpty()
        fun flag(key: String, from: JsonObject = raw) =
            from[key]?.let { runCatching { it.jsonPrimitive.booleanOrNull }.getOrNull() } ?: false
        return when (str("kind")) {
            "user" -> Item.User(id, at, text)
            "assistant" -> Item.Assistant(id, at, text)
            "thinking" -> Item.Thinking(id, at, text)
            "continuation" -> Item.Continuation(id, at, text)
            "event" -> Item.Event(id, at, text, flag("failure"))
            "compaction" -> Item.Compaction(id, at, str("summary"))
            "tool" -> {
                val result =
                    raw["result"]
                        ?.let { runCatching { it.jsonObject }.getOrNull() }
                        ?.let { r ->
                            ToolResult(
                                str("text", r).orEmpty(),
                                flag("error", r),
                                flag("truncated", r),
                                str("full", r),
                                str("at", r),
                            )
                        }
                Item.Tool(
                    id,
                    at,
                    str("name").orEmpty(),
                    str("input").orEmpty(),
                    result,
                    flag("unfinished"),
                )
            }
            else -> null
        }
    }

    fun items(raw: List<JsonObject>): List<Item> = raw.mapNotNull(::item)
}

/**
 * The items held for one agent, and the cursor to page older ones from. Applying a page or event
 * upserts by id, keeping the order items first arrived in, except that an older page goes before
 * everything held.
 */
data class Conversation(
    val items: List<Item> = emptyList(),
    val before: String? = null,
    val after: String? = null,
) {
    /** A first page, or a reset: what it holds replaces everything. */
    fun replacedBy(items: List<Item>, before: String?, after: String?) =
        Conversation(dedupe(items), before, after)

    /** New items at the end, or old ones re-sent: each replaces its id in place. */
    fun appended(items: List<Item>, after: String?): Conversation {
        val incoming = dedupe(items)
        val byId = incoming.associateBy { it.id }
        val updated = this.items.map { byId[it.id] ?: it }
        val held = this.items.mapTo(HashSet()) { it.id }
        return copy(
            items = updated + incoming.filter { it.id !in held },
            after = after ?: this.after,
        )
    }

    /** An older page, placed before what is held. */
    fun prepended(items: List<Item>, before: String?): Conversation {
        val held = this.items.mapTo(HashSet()) { it.id }
        return copy(items = dedupe(items).filter { it.id !in held } + this.items, before = before)
    }

    private fun dedupe(items: List<Item>): List<Item> {
        val last = items.associateBy { it.id }
        val seen = HashSet<String>()
        return items.mapNotNull { if (seen.add(it.id)) last.getValue(it.id) else null }
    }
}
