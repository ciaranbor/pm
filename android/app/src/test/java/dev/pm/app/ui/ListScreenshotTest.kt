package dev.pm.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.runtime.Composable
import androidx.lifecycle.viewmodel.compose.viewModel
import com.github.takahirom.roborazzi.captureRoboImage
import dev.pm.app.SNAPSHOT
import dev.pm.app.api.PmClient
import dev.pm.app.data.Connection
import dev.pm.app.model.Divergence
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import dev.pm.app.model.WorkflowInfo
import java.time.Instant
import java.util.TimeZone
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The list screens' rows, compared with the images in `src/test/screenshots` on every test run;
 * `gradlew recordRoborazziGoogleDebug` records them anew.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w360dp-h400dp")
class ListScreenshotTest {
    private val snapshot = Snapshot.parse(SNAPSHOT)
    private val calm = Snapshot.parse("""{"version": 1, "projects": [{"name": "app"}]}""")
    private val now = Instant.parse("2026-10-02T10:00:00Z")
    private val client = PmClient(Pairing("http://127.0.0.1:9", "pixel", "tok"))

    private fun capture(content: @Composable () -> Unit) = captureRoboImage {
        PmTheme(dynamic = false) { Surface(content) }
    }

    private val home =
        @Composable { Home(snapshot, now, openScope = { _, _ -> }, openProject = {}) }
    private val offline =
        @Composable {
            Column {
                StatusStrip(
                    Connection.Unreachable("refused"),
                    retry = {},
                    pairAgain = {},
                )
                Home(snapshot, now, openScope = { _, _ -> }, openProject = {}, stale = true)
            }
        }

    @Test fun home_light() = capture(home)

    @Test @Config(qualifiers = "+night") fun home_dark() = capture(home)

    @Test
    fun home_calm_light() = capture { Home(calm, now, openScope = { _, _ -> }, openProject = {}) }

    @Test fun offline_light() = capture(offline)

    @Test @Config(qualifiers = "+night") fun offline_dark() = capture(offline)

    @Test
    fun scopes_light() = capture { ScopesList(snapshot, "app", now, open = {}, openNotes = {}) }

    @Test
    @Config(qualifiers = "+night")
    fun scopes_dark() = capture { ScopesList(snapshot, "app", now, open = {}, openNotes = {}) }

    private val details =
        @Composable {
            val info =
                FeatureInfo(
                    name = "login",
                    progress = "wip",
                    lifecycle = "review",
                    branch = "login",
                    base = "main",
                    divergence = Divergence(ahead = 3, behind = 1),
                    remote = "origin/login",
                    pr = "42",
                    workflow = WorkflowInfo("research-implement-qa-review", "Researcher briefs."),
                    created = "2026-10-01T09:00:00Z",
                    lastActive = "2026-10-02T09:30:00Z",
                )
            DetailsScreen(viewModel { ReadModel(client) { info } })
        }

    @Test fun details_light() = utc { capture(details) }

    @Test @Config(qualifiers = "+night") fun details_dark() = utc { capture(details) }

    /** Run `block` with the phone in UTC, which details' times are shown in. */
    private fun utc(block: () -> Unit) {
        val zone = TimeZone.getDefault()
        TimeZone.setDefault(TimeZone.getTimeZone("UTC"))
        try {
            block()
        } finally {
            TimeZone.setDefault(zone)
        }
    }
}

@Composable
private fun Surface(content: @Composable () -> Unit) =
    androidx.compose.material3.Surface(content = content)
