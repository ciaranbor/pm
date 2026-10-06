package dev.pm.app.ui

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityCheckOptions
import com.github.takahirom.roborazzi.RoborazziATFAccessibilityChecker
import com.github.takahirom.roborazzi.checkRoboAccessibility
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.model.Snapshot
import java.time.Instant
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The start screen under an offline strip, a scope's screen, whose header shows a badge's text
 * unmerged, and the terminal sheet's keys, through Android's accessibility checks. Only errors
 * fail: its contrast warnings come from antialiased glyph edges on dark text; [ThemeTest] holds the
 * badge tones to 4.5:1.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h640dp")
class AccessibilityTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    private val snapshot = Snapshot.parse(SNAPSHOT)
    private val now = Instant.parse("2026-10-02T10:00:00Z")

    private fun check(content: @Composable () -> Unit) {
        compose.setContent { PmTheme(dynamic = false) { Surface(content = content) } }
        compose
            .onRoot()
            .checkRoboAccessibility(
                RoborazziATFAccessibilityCheckOptions(
                    failureLevel = RoborazziATFAccessibilityChecker.CheckLevel.Error
                )
            )
    }

    private val home =
        @Composable {
            Column {
                StatusStrip(
                    Connection.Unreachable("refused"),
                    readAt = now.toEpochMilli() - 12 * 60_000,
                    now = now,
                    retry = {},
                    pairAgain = {},
                )
                Home(snapshot, now, openNeed = {}, openProject = {})
            }
        }

    private val scope =
        @Composable { AgentsList(snapshot, "app", "login", now, openAgent = {}, openPage = {}) }

    private val terminal =
        @Composable {
            ScreenPanel(
                "Trust this folder?\n❯ 1. Yes\n  2. No",
                notice = null,
                press = {},
                type = { true },
            )
        }

    @Test fun home_light() = check(home)

    @Test @Config(qualifiers = "+night") fun home_dark() = check(home)

    @Test fun scope_light() = check(scope)

    @Test @Config(qualifiers = "+night") fun scope_dark() = check(scope)

    @Test fun terminal_light() = check(terminal)

    @Test @Config(qualifiers = "+night") fun terminal_dark() = check(terminal)
}
