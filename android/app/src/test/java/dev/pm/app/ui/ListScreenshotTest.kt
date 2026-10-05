package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.Composable
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.model.Snapshot
import java.time.Instant
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The list screens' rows, compared with the images in `src/test/screenshots` on every test run;
 * `gradlew recordRoborazziDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h400dp")
class ListScreenshotTest {
    private val snapshot = Snapshot.parse(SNAPSHOT)
    private val calm = Snapshot.parse("""{"version": 1, "projects": [{"name": "app"}]}""")
    private val now = Instant.parse("2026-10-02T10:00:00Z")

    private fun capture(content: @Composable () -> Unit) = captureRoboImage {
        PmTheme(dynamic = false) { Surface(content) }
    }

    private val home = @Composable { Home(snapshot, now, openNeed = {}, openProject = {}) }
    private val offline =
        @Composable {
            Column {
                StatusStrip(
                    Connection.Unreachable("refused"),
                    readAt = now.toEpochMilli() - 12 * 60_000,
                    now = now,
                    retry = {},
                    pairAgain = {},
                )
                home()
            }
        }

    @Test fun home_light() = capture(home)

    @Test @Config(qualifiers = "+night") fun home_dark() = capture(home)

    @Test fun home_calm_light() = capture { Home(calm, now, openNeed = {}, openProject = {}) }

    @Test fun offline_light() = capture(offline)

    @Test @Config(qualifiers = "+night") fun offline_dark() = capture(offline)

    @Test fun scopes_light() = capture { ScopesList(snapshot, "app", now, open = {}) }

    @Test
    @Config(qualifiers = "+night")
    fun scopes_dark() = capture { ScopesList(snapshot, "app", now, open = {}) }

    @Test fun agents_light() = capture { AgentsList(snapshot, "app", "login", now, openAgent = {}) }

    @Test
    @Config(qualifiers = "+night")
    fun agents_dark() = capture { AgentsList(snapshot, "app", "login", now, openAgent = {}) }
}

@Composable
private fun Surface(content: @Composable () -> Unit) =
    androidx.compose.material3.Surface(content = content)
