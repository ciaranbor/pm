package dev.pm.app.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import dev.pm.app.model.Tone

/**
 * The badge tones: the hues of pm's tmux badges, fixed whatever the wallpaper so a glyph reads the
 * same as in tmux, each dark or light enough for text on the theme's surfaces: checked against the
 * baseline schemes, whose surface tones the wallpaper's schemes share.
 */
@Immutable
data class PmColors(val red: Color, val green: Color, val magenta: Color, val yellow: Color) {
    companion object {
        val Light =
            PmColors(
                red = Color(0xFFC0143C),
                green = Color(0xFF1E6B14),
                magenta = Color(0xFFA0307F),
                yellow = Color(0xFF7A5000),
            )
        val Dark =
            PmColors(
                red = Color(0xFFF38BA8),
                green = Color(0xFFA6E3A1),
                magenta = Color(0xFFF5C2E7),
                yellow = Color(0xFFF9E2AF),
            )
    }
}

// Theme tokens, provided by PmTheme as MaterialTheme provides its own.
@Suppress("ComposeCompositionLocalUsage")
val LocalPmColors = staticCompositionLocalOf { PmColors.Light }

/** The app's theme: the wallpaper's colours where Android offers them (12+), for all but badges. */
@Composable
fun PmTheme(
    dark: Boolean = isSystemInDarkTheme(),
    dynamic: Boolean = true,
    content: @Composable () -> Unit,
) {
    val scheme =
        when {
            dynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S ->
                if (dark) dynamicDarkColorScheme(LocalContext.current)
                else dynamicLightColorScheme(LocalContext.current)
            dark -> darkColorScheme()
            else -> lightColorScheme()
        }
    CompositionLocalProvider(LocalPmColors provides if (dark) PmColors.Dark else PmColors.Light) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}

@Composable
@ReadOnlyComposable
fun Tone.color(): Color {
    val colors = LocalPmColors.current
    return when (this) {
        Tone.Red -> colors.red
        Tone.Green -> colors.green
        Tone.Magenta -> colors.magenta
        Tone.Yellow -> colors.yellow
        Tone.Grey -> MaterialTheme.colorScheme.onSurfaceVariant
    }
}
