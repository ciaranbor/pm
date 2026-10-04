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

@Composable
fun MarkIcon(mark: Mark, description: String, modifier: Modifier = Modifier) {
    Icon(
        painterResource(mark.glyph.icon),
        contentDescription = description,
        tint = mark.tone.color(),
        modifier = modifier.size(18.dp),
    )
}

/** A scope's attention: its glyph and kind; nothing for `none`. */
@Composable
fun AttentionBadge(kind: AttentionKind, wire: String, modifier: Modifier = Modifier) {
    val mark = Marks.attention(kind) ?: return
    Row(
        modifier = modifier,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        MarkIcon(mark, wire)
        Text(
            wire,
            color = mark.tone.color(),
            fontWeight = if (mark.strong) FontWeight.Bold else FontWeight.Normal,
            style = MaterialTheme.typography.labelLarge,
        )
    }
}

/** An agent's state glyph, and an envelope when it has unread messages. */
@Composable
fun AgentBadge(agent: AgentSnapshot, modifier: Modifier = Modifier, showName: Boolean = true) {
    Row(
        modifier = modifier,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        MarkIcon(Marks.agent(agent.stateOf), "${agent.name} ${agent.state}")
        if (showName) Text(agent.name, style = MaterialTheme.typography.labelMedium)
        if (agent.unread > 0) {
            Icon(
                painterResource(R.drawable.ic_mail),
                "${agent.unread} unread",
                tint = Tone.Yellow.color(),
                modifier = Modifier.size(14.dp),
            )
            Text(
                "${agent.unread}",
                style = MaterialTheme.typography.labelSmall,
                color = Tone.Yellow.color(),
            )
        }
    }
}

@Composable
fun ActivityLabel(activity: Activity?, modifier: Modifier = Modifier) {
    when (activity) {
        Activity.Working ->
            Row(
                modifier = modifier,
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                MarkIcon(Marks.agent(AgentState.Busy), "working", Modifier.size(14.dp))
                Text(
                    "working",
                    style = MaterialTheme.typography.labelSmall,
                    color = Tone.Green.color(),
                )
            }
        is Activity.Quiet ->
            Text(
                "quiet ${activity.span}",
                modifier = modifier,
                style = MaterialTheme.typography.labelSmall,
                color = Tone.Grey.color(),
            )
        null -> {}
    }
}
