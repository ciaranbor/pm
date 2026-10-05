package dev.pm.app.ui

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import org.junit.Assert.assertTrue
import org.junit.Test

class ThemeTest {
    private fun contrast(a: Color, b: Color): Double {
        val (hi, lo) = listOf(a.luminance(), b.luminance()).sortedDescending()
        return (hi + 0.05) / (lo + 0.05)
    }

    private fun assertReadable(tones: PmColors, scheme: ColorScheme) {
        val surfaces =
            mapOf(
                "surface" to scheme.surface,
                "surfaceContainer" to scheme.surfaceContainer,
                "surfaceContainerHighest" to scheme.surfaceContainerHighest,
            )
        val colors =
            mapOf(
                "red" to tones.red,
                "green" to tones.green,
                "magenta" to tones.magenta,
                "yellow" to tones.yellow,
            )
        for ((tone, color) in colors) for ((surface, background) in surfaces) {
            val ratio = contrast(color, background)
            assertTrue("$tone on $surface is $ratio:1", ratio >= 4.5)
        }
    }

    @Test
    fun badge_tones_meet_wcag_aa_for_text_on_every_surface_they_sit_on() {
        assertReadable(PmColors.Light, lightColorScheme())
        assertReadable(PmColors.Dark, darkColorScheme())
    }
}
