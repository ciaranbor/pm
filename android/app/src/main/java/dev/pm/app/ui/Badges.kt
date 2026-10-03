package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Help
import androidx.compose.material.icons.filled.Autorenew
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.CleaningServices
import androidx.compose.material.icons.filled.Dangerous
import androidx.compose.material.icons.filled.HourglassBottom
import androidx.compose.material.icons.filled.Mail
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.PanTool
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Circle
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.pm.app.model.Activity
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.Glyph
import dev.pm.app.model.Mark
import dev.pm.app.model.Marks
import dev.pm.app.model.Tone

/** The Material icon closest to each Nerd Font glyph pm's tmux badges use. */
private val Glyph.icon: ImageVector
    get() = when (this) {
        Glyph.Gear -> Icons.Filled.Settings
        Glyph.QuestionCircle -> Icons.AutoMirrored.Filled.Help
        Glyph.BellSlash -> Icons.Filled.NotificationsOff
        Glyph.Spinner -> Icons.Filled.Autorenew
        Glyph.Hourglass -> Icons.Filled.HourglassBottom
        Glyph.Skull -> Icons.Filled.Dangerous
        Glyph.Stop -> Icons.Filled.Stop
        Glyph.Hand -> Icons.Filled.PanTool
        Glyph.Broom -> Icons.Filled.CleaningServices
        Glyph.CheckCircle -> Icons.Filled.CheckCircle
        Glyph.Pause -> Icons.Filled.Pause
        Glyph.Unknown -> Icons.Outlined.Circle
    }

@Composable
fun MarkIcon(mark: Mark, description: String, modifier: Modifier = Modifier) {
    Icon(mark.glyph.icon, contentDescription = description, tint = mark.tone.color(), modifier = modifier.size(18.dp))
}

/** A scope's attention: its glyph and kind; nothing for `none`. */
@Composable
fun AttentionBadge(kind: AttentionKind, wire: String) {
    val mark = Marks.attention(kind) ?: return
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
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
fun AgentBadge(agent: AgentSnapshot, showName: Boolean = true) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(2.dp)) {
        MarkIcon(Marks.agent(agent.stateOf), "${agent.name} ${agent.state}")
        if (showName) Text(agent.name, style = MaterialTheme.typography.labelMedium)
        if (agent.unread > 0) {
            Icon(Icons.Filled.Mail, "${agent.unread} unread", tint = Tone.Yellow.color(), modifier = Modifier.size(14.dp))
            Text("${agent.unread}", style = MaterialTheme.typography.labelSmall, color = Tone.Yellow.color())
        }
    }
}

@Composable
fun ActivityLabel(activity: Activity?) {
    when (activity) {
        Activity.Working -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(2.dp)) {
            MarkIcon(Marks.agent(AgentState.Busy), "working", Modifier.size(14.dp))
            Text("working", style = MaterialTheme.typography.labelSmall, color = Tone.Green.color())
        }
        is Activity.Quiet -> Text("quiet ${activity.span}", style = MaterialTheme.typography.labelSmall, color = Tone.Grey.color())
        null -> {}
    }
}
