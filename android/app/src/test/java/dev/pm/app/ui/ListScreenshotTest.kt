package dev.pm.app.ui

import androidx.compose.runtime.Composable
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.SNAPSHOT
import dev.pm.app.model.Snapshot
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import java.time.Instant

/**
 * The list screens' rows, compared with the images in `src/test/screenshots`
 * on every test run; `gradlew recordRoborazziDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h400dp")
class ListScreenshotTest {
    private val snapshot = Snapshot.parse(SNAPSHOT)
    private val now = Instant.parse("2026-10-02T10:00:00Z")

    private fun capture(content: @Composable () -> Unit) = captureRoboImage { PmTheme { Surface(content) } }

    @Test
    fun projects_light() = capture { ProjectsList(snapshot, now) {} }

    @Test
    @Config(qualifiers = "+night")
    fun projects_dark() = capture { ProjectsList(snapshot, now) {} }

    @Test
    fun scopes_light() = capture { ScopesList(snapshot, "app", now) {} }

    @Test
    @Config(qualifiers = "+night")
    fun scopes_dark() = capture { ScopesList(snapshot, "app", now) {} }

    @Test
    fun agents_light() = capture { AgentsList(snapshot, "app", "login", now, openAgent = {}, openSummary = {}) }

    @Test
    @Config(qualifiers = "+night")
    fun agents_dark() = capture { AgentsList(snapshot, "app", "login", now, openAgent = {}, openSummary = {}) }
}

@Composable
private fun Surface(content: @Composable () -> Unit) = androidx.compose.material3.Surface(content = content)
