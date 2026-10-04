package dev.pm.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.graphics.Color
import dev.pm.app.model.Tone

@Composable
fun PmTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme(),
        content = content,
    )
}

/** A badge tone as a colour that reads on the current background. */
@Composable
@ReadOnlyComposable
fun Tone.color(): Color {
    val dark = isSystemInDarkTheme()
    return when (this) {
        Tone.Red -> if (dark) Color(0xFFF38BA8) else Color(0xFFD20F39)
        Tone.Green -> if (dark) Color(0xFFA6E3A1) else Color(0xFF40A02B)
        Tone.Magenta -> if (dark) Color(0xFFF5C2E7) else Color(0xFFEA76CB)
        Tone.Yellow -> if (dark) Color(0xFFF9E2AF) else Color(0xFFDF8E1D)
        Tone.Grey -> MaterialTheme.colorScheme.onSurfaceVariant
    }
}
