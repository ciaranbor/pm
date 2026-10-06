package dev.pm.app.ui

import androidx.annotation.DrawableRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.model.Activity
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.Glyph
import dev.pm.app.model.Mark
import dev.pm.app.model.Marks
import dev.pm.app.model.Tone

/** The Material icon closest to each Nerd Font glyph pm's tmux badges use. */
@get:DrawableRes
private val Glyph.icon: Int
    get() =
        when (this) {
            Glyph.Gear -> R.drawable.ic_settings
            Glyph.QuestionCircle -> R.drawable.ic_help
            Glyph.BellSlash -> R.drawable.ic_notifications_off
            Glyph.Spinner -> R.drawable.ic_autorenew
            Glyph.Hourglass -> R.drawable.ic_hourglass_bottom
            Glyph.Skull -> R.drawable.ic_dangerous
            Glyph.Stop -> R.drawable.ic_stop
            Glyph.Hand -> R.drawable.ic_pan_tool
            Glyph.Broom -> R.drawable.ic_cleaning_services
            Glyph.CheckCircle -> R.drawable.ic_check_circle
            Glyph.Pause -> R.drawable.ic_pause
            Glyph.Unknown -> R.drawable.ic_circle
        }

/** A badge's glyph; `description` is `null` where text beside it already says what it means. */
@Composable
fun MarkIcon(mark: Mark, description: String?, modifier: Modifier = Modifier) {
    Icon(
        painterResource(mark.glyph.icon),
        contentDescription = description,
        tint = mark.tone.color(),
        modifier = modifier.size(18.dp),
    )
}

/** A scope's attention: its glyph and kind; nothing for `none`. */
@Composable
fun AttentionBadge(kind: AttentionKind, modifier: Modifier = Modifier) {
    val mark = Marks.attention(kind) ?: return
    Row(
        modifier = modifier,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
    ) {
        MarkIcon(mark, null)
        Text(
            kind.label,
            color = mark.tone.color(),
            fontWeight = if (mark.strong) FontWeight.Bold else FontWeight.Normal,
            style = MaterialTheme.typography.labelLarge,
        )
    }
}

/** What TalkBack reads for an agent: its name, state, and unread messages. */
fun describe(agent: AgentSnapshot): String =
    listOfNotNull(
            "${agent.name} ${agent.stateOf.label}",
            agent.unread.takeIf { it > 0 }?.let { "$it unread" },
        )
        .joinToString(", ")

/** An agent's state glyph, and an envelope when it has unread messages. */
@Composable
fun AgentBadge(agent: AgentSnapshot, modifier: Modifier = Modifier, showName: Boolean = true) {
    Row(
        modifier = modifier.clearAndSetSemantics { contentDescription = describe(agent) },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xxs),
    ) {
        MarkIcon(Marks.agent(agent.stateOf), null)
        if (showName) Text(agent.name, style = MaterialTheme.typography.labelMedium)
        if (agent.unread > 0) {
            Icon(
                painterResource(R.drawable.ic_mail),
                null,
                tint = Tone.Caution.color(),
                modifier = Modifier.size(14.dp),
            )
            Text(
                "${agent.unread}",
                style = MaterialTheme.typography.labelSmall,
                color = Tone.Caution.color(),
            )
        }
    }
}

/**
 * What an [ActivityLabel] says, for a row's composed description. While `stale` (`pm serve` isn't
 * live, so the snapshot may be out of date) working reads as what it was.
 */
fun describe(activity: Activity?, stale: Boolean): String? =
    when (activity) {
        Activity.Working -> if (stale) "was ${AgentState.Busy.label}" else AgentState.Busy.label
        is Activity.Background -> "${AgentState.Background.label} ${activity.span}"
        is Activity.Idle -> "${AgentState.Idle.label} ${activity.span}"
        null -> null
    }

@Composable
fun ActivityLabel(activity: Activity?, stale: Boolean, modifier: Modifier = Modifier) {
    val text = describe(activity, stale) ?: return
    Row(
        modifier = modifier,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xxs),
    ) {
        val state =
            when (activity) {
                Activity.Working -> AgentState.Busy
                is Activity.Background -> AgentState.Background
                else -> null
            }
        if (state != null) {
            val mark = Marks.agent(state).let { if (stale) it.copy(tone = Tone.Neutral) else it }
            MarkIcon(mark, null, Modifier.size(14.dp))
        }
        Text(
            text,
            style = MaterialTheme.typography.labelSmall,
            color =
                if (activity == Activity.Working && !stale) Tone.Positive.color()
                else Tone.Neutral.color(),
        )
    }
}
