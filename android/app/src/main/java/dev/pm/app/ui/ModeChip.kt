package dev.pm.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.pm.app.R
import dev.pm.app.api.AgentMode

/**
 * An agent's permission mode as a pill that opens the modes it may be put in, the current one
 * checked. While `switching` names one, it shows that a switch is on its way and offers nothing.
 */
@Composable
fun ModeChip(
    mode: AgentMode,
    switching: String?,
    open: () -> Unit,
    switch: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var expanded by remember { mutableStateOf(false) }
    val label = mode.label ?: "Mode?"
    Box(modifier) {
        Surface(
            onClick = {
                open()
                expanded = true
            },
            enabled = switching == null,
            shape = CircleShape,
            color = MaterialTheme.colorScheme.surfaceContainerHigh,
            modifier = Modifier.semantics { contentDescription = "Permission mode: $label" },
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(Spacing.xxs),
                modifier =
                    Modifier.padding(
                        start = Spacing.s,
                        end = Spacing.xs,
                        top = Spacing.xxs,
                        bottom = Spacing.xxs,
                    ),
            ) {
                Text(label, style = MaterialTheme.typography.labelMedium, maxLines = 1)
                if (switching != null) {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                } else {
                    Icon(painterResource(R.drawable.ic_expand_more), null, Modifier.size(16.dp))
                }
            }
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            mode.modes.forEach { option ->
                val current = option.id == mode.mode
                DropdownMenuItem(
                    text = { Text(option.label) },
                    leadingIcon = {
                        if (current) Icon(painterResource(R.drawable.ic_check), "Current")
                        else Box(Modifier.size(24.dp))
                    },
                    onClick = {
                        expanded = false
                        if (!current) switch(option.id)
                    },
                )
            }
        }
    }
}
