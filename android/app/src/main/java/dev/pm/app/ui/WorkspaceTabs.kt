package dev.pm.app.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.relocation.BringIntoViewRequester
import androidx.compose.foundation.relocation.bringIntoViewRequester
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.AgentState
import dev.pm.app.model.Marks
import dev.pm.app.model.Tone

/**
 * The workspace's agent tabs, `selected` marked; each with its agent's state, neutral while
 * `stale`, and unread messages.
 *
 * Material's scrollable tab row centres the selected tab, which on a team opened at its last agent
 * pushes its first ones off-screen; this row scrolls only as far as brings the selected tab into
 * view.
 */
@Composable
fun WorkspaceTabs(
    tabs: List<String>,
    selected: String?,
    agents: List<AgentSnapshot>,
    select: (String) -> Unit,
    modifier: Modifier = Modifier,
    stale: Boolean = false,
) {
    val indicator = MaterialTheme.colorScheme.primary
    Column(modifier.fillMaxWidth()) {
        Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).selectableGroup()) {
            tabs.forEach { tab ->
                val chosen = tab == selected
                val view = remember { BringIntoViewRequester() }
                LaunchedEffect(chosen) { if (chosen) view.bringIntoView() }
                Tab(
                    selected = chosen,
                    onClick = { select(tab) },
                    text = { TabLabel(tab, agents, stale) },
                    selectedContentColor = indicator,
                    unselectedContentColor = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier =
                        Modifier.widthIn(min = 64.dp).bringIntoViewRequester(view).drawWithContent {
                            drawContent()
                            if (chosen) {
                                val inset = TAB_PADDING.toPx()
                                val height = INDICATOR.toPx()
                                drawRoundRect(
                                    indicator,
                                    topLeft = Offset(inset, size.height - height),
                                    size = Size(size.width - 2 * inset, height),
                                    cornerRadius = CornerRadius(height),
                                )
                            }
                        },
                )
            }
        }
        HorizontalDivider()
    }
}

/** Material's horizontal padding inside a tab, which the indicator spans the width within. */
private val TAB_PADDING = 16.dp
private val INDICATOR = 3.dp

@Composable
private fun TabLabel(name: String, agents: List<AgentSnapshot>, stale: Boolean) {
    val agent = agents.find { it.name == name }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
        modifier =
            Modifier.clearAndSetSemantics {
                contentDescription = agent?.let { describe(it, stale) } ?: name
            },
    ) {
        val mark = Marks.agent(agent?.stateOf ?: AgentState.Unknown)
        MarkIcon(if (stale) mark.copy(tone = Tone.Neutral) else mark, null)
        Text(name, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (agent != null && agent.unread > 0) {
            Text(
                "${agent.unread}",
                style = MaterialTheme.typography.labelSmall,
                color = Tone.Caution.color(),
            )
        }
    }
}
