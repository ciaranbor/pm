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

/** The badge colours pm's tmux badges use. */
enum class Tone {
    Red,
    Green,
    Magenta,
    Yellow,
    Grey,
}

/** A badge: a glyph in a colour; `strong` where tmux draws it bold. */
data class Mark(val glyph: Glyph, val tone: Tone, val strong: Boolean = false)

/**
 * The marks of pm's tmux badges (`tmux_refresh/badge.rs`), so a glyph means the same on the phone
 * as in tmux. An attention kind that means an agent state (asking, dead, unarmed) is drawn as that
 * state.
 */
object Marks {
    fun agent(state: AgentState): Mark =
        when (state) {
            AgentState.Busy -> Mark(Glyph.Gear, Tone.Green)
            AgentState.Asking -> Mark(Glyph.QuestionCircle, Tone.Red, strong = true)
            AgentState.Unarmed -> Mark(Glyph.BellSlash, Tone.Magenta)
            AgentState.Background -> Mark(Glyph.Spinner, Tone.Green)
            AgentState.Idle -> Mark(Glyph.Hourglass, Tone.Grey)
            AgentState.Dead -> Mark(Glyph.Skull, Tone.Red)
            AgentState.Stopped,
            AgentState.Closed -> Mark(Glyph.Stop, Tone.Grey)
            AgentState.Unknown -> Mark(Glyph.Unknown, Tone.Grey)
        }

    /** `null` for [AttentionKind.None]: nothing needed, no badge. */
    fun attention(kind: AttentionKind): Mark? =
        when (kind) {
            AttentionKind.Blocked -> Mark(Glyph.Hand, Tone.Red, strong = true)
            AttentionKind.Asking -> agent(AgentState.Asking)
            AttentionKind.Cleanup -> Mark(Glyph.Broom, Tone.Grey)
            AttentionKind.Ready -> Mark(Glyph.CheckCircle, Tone.Green, strong = true)
            AttentionKind.Dead -> agent(AgentState.Dead)
            AttentionKind.Unarmed -> agent(AgentState.Unarmed)
            AttentionKind.Stalled -> Mark(Glyph.Pause, Tone.Yellow)
            AttentionKind.Unknown -> Mark(Glyph.Unknown, Tone.Grey)
            AttentionKind.None -> null
        }
}

/** What a scope's activity line shows. */
sealed interface Activity {
    data object Working : Activity

    /** Quiet for `span`, as pm writes it: `12m`, `3h`, `2d`. */
    data class Quiet(val span: String) : Activity
}

/**
 * pm's rule (`attention::quiet_since`): working while an agent showed activity recently; otherwise
 * quiet once that was [QUIET] or longer ago, and nothing in between, so the gaps between turns
 * don't flicker.
 */
fun activity(working: Boolean, lastActivity: String?, now: Instant): Activity? {
    if (working) return Activity.Working
    val then = lastActivity?.let { runCatching { Instant.parse(it) }.getOrNull() } ?: return null
    val secs = Duration.between(then, now).seconds.coerceAtLeast(0)
    if (secs < QUIET.seconds) return null
    return Activity.Quiet(span(secs))
}

val QUIET: Duration = Duration.ofMinutes(10)

/** Seconds rounded down to their largest unit, as pm's `span`. */
fun span(secs: Long): String =
    when {
        secs < 3600 -> "${secs / 60}m"
        secs < 86400 -> "${secs / 3600}h"
        else -> "${secs / 86400}d"
    }
