package dev.pm.app.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import dev.pm.app.model.Tone
import org.junit.Assert.assertTrue
import org.junit.Test

class ThemeTest {
    private fun contrast(a: Color, b: Color): Double {
        val (hi, lo) = listOf(a.luminance(), b.luminance()).sortedDescending()
        return (hi + 0.05) / (lo + 0.05)
    }

    /** A brand seed of each hue family, so a later seed swap keeps the status tones readable. */
    private val seeds =
        listOf(Seed, Color(0xFF006A6A), Color(0xFFB3261E), Color(0xFF3D5AFE), Color(0xFF7A5900))

    @Test
    fun status_tones_meet_wcag_aa_for_text_on_every_surface_they_sit_on_whatever_the_seed() {
        for (seed in seeds) for (dark in listOf(false, true)) {
            val scheme = schemeOf(seed, dark)
            val status = StatusColors.of(dark)
            val tones = Tone.entries.associateWith { it.color(scheme, status) }
            val surfaces =
                mapOf(
                    "surface" to scheme.surface,
                    "surfaceContainer" to scheme.surfaceContainer,
                    "surfaceContainerHighest" to scheme.surfaceContainerHighest,
                )
            for ((tone, color) in tones) for ((surface, background) in surfaces) {
                val ratio = contrast(color, background)
                assertTrue("$tone on $surface ($seed, dark=$dark) is $ratio:1", ratio >= 4.5)
            }
        }
    }
}
