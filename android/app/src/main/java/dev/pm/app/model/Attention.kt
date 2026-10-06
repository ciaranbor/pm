package dev.pm.app.model

import java.time.Duration
import java.time.Instant

/** What a scope needs from the user, most urgent first, as pm ranks it. */
enum class AttentionKind(val wire: String) {
    Blocked("blocked"),
    Asking("asking"),
    Cleanup("cleanup"),
    Ready("ready"),
    Dead("dead"),
    Unarmed("unarmed"),
    Stalled("stalled"),
    None("none"),

    /** A kind newer than this app. */
    Unknown("");

    /** What the app calls it: its wire name, or `other` for a kind newer than the app. */
    val label: String
        get() = wire.ifEmpty { "other" }

    companion object {
        fun of(wire: String): AttentionKind =
            entries.find { it.wire == wire && it != Unknown } ?: Unknown
    }
}

/**
 * The states an agent can be in. [label] is the app's word for each, the same everywhere it shows:
 * an agent taking a turn is working, as is a scope with one that is, and one waiting is idle.
 */
enum class AgentState(val wire: String) {
    Idle("idle"),
    Busy("busy"),
    Asking("asking"),
    Unarmed("unarmed"),
    Background("background"),
    Dead("dead"),
    Stopped("stopped"),
    Closed("closed"),

    /** A state newer than this app. */
    Unknown("");

    val label: String
        get() =
            when (this) {
                Busy -> "working"
                Background -> "background work"
                Unknown -> "other"
                else -> wire
            }

    companion object {
        fun of(wire: String): AgentState =
            entries.find { it.wire == wire && it != Unknown } ?: Unknown
    }
}

/** The glyphs pm's tmux badges draw, by the Nerd Font icon each names. */
enum class Glyph {
    Gear,
    QuestionCircle,
    BellSlash,
    Spinner,
    Hourglass,
    Skull,
    Stop,
    Hand,
    Broom,
    CheckCircle,
    Pause,
    Unknown,
}

/**
 * What a badge's colour says. The glyphs match pm's tmux badges; the colours are the app's own, so
 * asking (a turn waiting on the user) reads apart from dead.
 */
enum class Tone {
    /** Waiting on the user: asking, blocked. */
    Attention,

    /** Something broke: dead. */
    Danger,

    /** Going well: working, ready. */
    Positive,

    /** Worth a look, nothing broken: stalled, unarmed, unread messages. */
    Caution,

    /** Nothing to act on. */
    Neutral,
}

/** A badge: a glyph in a tone; `strong` where tmux draws it bold. */
data class Mark(val glyph: Glyph, val tone: Tone, val strong: Boolean = false)

/**
 * The marks of pm's tmux badges (`tmux_refresh/badge.rs`), so a glyph means the same on the phone
 * as in tmux. An attention kind that means an agent state (asking, dead, unarmed) is drawn as that
 * state.
 */
object Marks {
    fun agent(state: AgentState): Mark =
        when (state) {
            AgentState.Busy -> Mark(Glyph.Gear, Tone.Positive)
            AgentState.Asking -> Mark(Glyph.QuestionCircle, Tone.Attention, strong = true)
            AgentState.Unarmed -> Mark(Glyph.BellSlash, Tone.Caution)
            AgentState.Background -> Mark(Glyph.Spinner, Tone.Positive)
            AgentState.Idle -> Mark(Glyph.Hourglass, Tone.Neutral)
            AgentState.Dead -> Mark(Glyph.Skull, Tone.Danger)
            AgentState.Stopped,
            AgentState.Closed -> Mark(Glyph.Stop, Tone.Neutral)
            AgentState.Unknown -> Mark(Glyph.Unknown, Tone.Neutral)
        }

    /** `null` for [AttentionKind.None]: nothing needed, no badge. */
    fun attention(kind: AttentionKind): Mark? =
        when (kind) {
            AttentionKind.Blocked -> Mark(Glyph.Hand, Tone.Attention, strong = true)
            AttentionKind.Asking -> agent(AgentState.Asking)
            AttentionKind.Cleanup -> Mark(Glyph.Broom, Tone.Neutral)
            AttentionKind.Ready -> Mark(Glyph.CheckCircle, Tone.Positive, strong = true)
            AttentionKind.Dead -> agent(AgentState.Dead)
            AttentionKind.Unarmed -> agent(AgentState.Unarmed)
            AttentionKind.Stalled -> Mark(Glyph.Pause, Tone.Caution)
            AttentionKind.Unknown -> Mark(Glyph.Unknown, Tone.Neutral)
            AttentionKind.None -> null
        }
}

/** A feature's team status (`pm feat status`) in the app's words; one newer than the app, as is. */
fun progressLabel(progress: String): String =
    when (progress) {
        "wip" -> "in progress"
        else -> progress
    }

/** What `pm feat sync` last saw of a feature's PR, from its lifecycle; null for none known. */
fun prLabel(lifecycle: String): String? =
    when (lifecycle) {
        "wip" -> "draft"
        "review" -> "open"
        "approved" -> "approved"
        "merged" -> "merged"
        "stale" -> "closed"
        else -> null
    }

/** What a scope's activity line shows. */
sealed interface Activity {
    data object Working : Activity

    /** Waiting on background work for `span`, as pm writes it. */
    data class Background(val span: String) : Activity

    /** Idle for `span`, as pm writes it: `12m`, `3h`, `2d`. */
    data class Idle(val span: String) : Activity
}

/**
 * pm's rule (`attention::activity`): working while an agent showed activity recently; else waiting
 * on background work since its oldest wait began; otherwise idle once that was [QUIET] or longer
 * ago, and nothing in between, so the gaps between turns don't flicker.
 */
fun activity(
    working: Boolean,
    backgroundSince: String?,
    lastActivity: String?,
    now: Instant,
): Activity? {
    if (working) return Activity.Working
    instant(backgroundSince)?.let {
        return Activity.Background(span(Duration.between(it, now).seconds.coerceAtLeast(0)))
    }
    val then = instant(lastActivity) ?: return null
    val secs = Duration.between(then, now).seconds.coerceAtLeast(0)
    if (secs < QUIET.seconds) return null
    return Activity.Idle(span(secs))
}

private fun instant(text: String?): Instant? = text?.let {
    runCatching { Instant.parse(it) }.getOrNull()
}

val QUIET: Duration = Duration.ofMinutes(10)

/** Seconds rounded down to their largest unit, as pm's `span`. */
fun span(secs: Long): String =
    when {
        secs < 3600 -> "${secs / 60}m"
        secs < 86400 -> "${secs / 3600}h"
        else -> "${secs / 86400}d"
    }
