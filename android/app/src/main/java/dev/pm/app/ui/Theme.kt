package dev.pm.app.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.sp
import com.materialkolor.hct.Hct
import com.materialkolor.palettes.TonalPalette
import com.materialkolor.scheme.DynamicScheme
import com.materialkolor.scheme.SchemeTonalSpot
import dev.pm.app.model.Tone

/** The colour the app's scheme grows from where the wallpaper's isn't used. */
val Seed = Color(0xFF6750A4)

/** A Material scheme grown from `seed`, as Android grows one from a wallpaper. */
fun schemeOf(seed: Color, dark: Boolean): ColorScheme {
    val s: DynamicScheme = SchemeTonalSpot(Hct.fromInt(seed.toArgb()), dark, 0.0)
    fun c(argb: Int) = Color(argb)
    return ColorScheme(
        primary = c(s.primary),
        onPrimary = c(s.onPrimary),
        primaryContainer = c(s.primaryContainer),
        onPrimaryContainer = c(s.onPrimaryContainer),
        inversePrimary = c(s.inversePrimary),
        secondary = c(s.secondary),
        onSecondary = c(s.onSecondary),
        secondaryContainer = c(s.secondaryContainer),
        onSecondaryContainer = c(s.onSecondaryContainer),
        tertiary = c(s.tertiary),
        onTertiary = c(s.onTertiary),
        tertiaryContainer = c(s.tertiaryContainer),
        onTertiaryContainer = c(s.onTertiaryContainer),
        background = c(s.background),
        onBackground = c(s.onBackground),
        surface = c(s.surface),
        onSurface = c(s.onSurface),
        surfaceVariant = c(s.surfaceVariant),
        onSurfaceVariant = c(s.onSurfaceVariant),
        surfaceTint = c(s.surfaceTint),
        inverseSurface = c(s.inverseSurface),
        inverseOnSurface = c(s.inverseOnSurface),
        error = c(s.error),
        onError = c(s.onError),
        errorContainer = c(s.errorContainer),
        onErrorContainer = c(s.onErrorContainer),
        outline = c(s.outline),
        outlineVariant = c(s.outlineVariant),
        scrim = c(s.scrim),
        surfaceBright = c(s.surfaceBright),
        surfaceDim = c(s.surfaceDim),
        surfaceContainer = c(s.surfaceContainer),
        surfaceContainerHigh = c(s.surfaceContainerHigh),
        surfaceContainerHighest = c(s.surfaceContainerHighest),
        surfaceContainerLow = c(s.surfaceContainerLow),
        surfaceContainerLowest = c(s.surfaceContainerLowest),
        primaryFixed = c(s.primaryFixed),
        primaryFixedDim = c(s.primaryFixedDim),
        onPrimaryFixed = c(s.onPrimaryFixed),
        onPrimaryFixedVariant = c(s.onPrimaryFixedVariant),
        secondaryFixed = c(s.secondaryFixed),
        secondaryFixedDim = c(s.secondaryFixedDim),
        onSecondaryFixed = c(s.onSecondaryFixed),
        onSecondaryFixedVariant = c(s.onSecondaryFixedVariant),
        tertiaryFixed = c(s.tertiaryFixed),
        tertiaryFixedDim = c(s.tertiaryFixedDim),
        onTertiaryFixed = c(s.onTertiaryFixed),
        onTertiaryFixedVariant = c(s.onTertiaryFixedVariant),
    )
}

/**
 * The status tones no scheme role carries, fixed in hue whatever the scheme: tone 40 in light and
 * 80 in dark, the tones Material gives text on its surfaces, so each reads on all of them.
 */
@Immutable
data class StatusColors(val positive: Color, val caution: Color) {
    companion object {
        private const val GREEN = 145.0
        private const val AMBER = 75.0
        private const val CHROMA = 48.0

        private fun tone(hue: Double, dark: Boolean) =
            Color(TonalPalette.fromHueAndChroma(hue, CHROMA).tone(if (dark) 80 else 40))

        fun of(dark: Boolean) = StatusColors(tone(GREEN, dark), tone(AMBER, dark))
    }
}

// Theme tokens, provided by PmTheme as MaterialTheme provides its own.
@Suppress("ComposeCompositionLocalUsage")
val LocalStatusColors = staticCompositionLocalOf { StatusColors.of(dark = false) }

/**
 * The type scale: Material's, with body text a little tighter so a phone screen holds more of a
 * conversation.
 */
private val PmTypography =
    Typography().run {
        copy(
            bodyLarge = bodyLarge.copy(lineHeight = 22.sp),
            bodyMedium = bodyMedium.copy(lineHeight = 20.sp, letterSpacing = 0.2.sp),
        )
    }

/**
 * The app's theme: the wallpaper's colours where Android offers them (12+) and `dynamic` is set,
 * else a scheme grown from `seed`; the status tones are the app's own either way.
 */
@Composable
fun PmTheme(
    dark: Boolean = isSystemInDarkTheme(),
    dynamic: Boolean = true,
    seed: Color = Seed,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val scheme =
        remember(dark, dynamic, seed) {
            if (dynamic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
            } else schemeOf(seed, dark)
        }
    val status = remember(dark) { StatusColors.of(dark) }
    CompositionLocalProvider(LocalStatusColors provides status) {
        MaterialTheme(
            colorScheme = scheme,
            typography = PmTypography,
            shapes = PmShapes,
            content = content,
        )
    }
}

/** The colour `this` tone takes in `scheme`, with the app's own `status` tones. */
fun Tone.color(scheme: ColorScheme, status: StatusColors): Color =
    when (this) {
        Tone.Attention -> scheme.tertiary
        Tone.Danger -> scheme.error
        Tone.Positive -> status.positive
        Tone.Caution -> status.caution
        Tone.Neutral -> scheme.onSurfaceVariant
    }

@Composable
@ReadOnlyComposable
fun Tone.color(): Color = color(MaterialTheme.colorScheme, LocalStatusColors.current)
