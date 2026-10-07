package dev.pm.app.ui

import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import dev.pm.app.SNAPSHOT
import dev.pm.app.model.AgentSnapshot
import dev.pm.app.model.Attention
import dev.pm.app.model.Need
import dev.pm.app.model.Snapshot
import org.junit.Assert.assertEquals
import org.junit.Test

class RoutesTest {
    private val login = Route.Scope("app", "login")

    private fun stack(vararg routes: Route) = NavBackStack<NavKey>(*routes)

    @Test
    fun a_target_opens_over_what_is_shown_or_switches_the_tab_of_the_workspace_shown() {
        val notes = stack(Route.Home, Route.Notes("app"))
        notes.open(Route.Scope("app", "login", Tab.Agent("qa")))
        assertEquals(
            listOf(Route.Home, Route.Notes("app"), Route.Scope("app", "login", Tab.Agent("qa"))),
            notes.toList(),
        )

        notes.open(Route.Scope("app", "login", Tab.Agent("implementer")))
        assertEquals(Route.Scope("app", "login", Tab.Agent("implementer")), notes.last())
        assertEquals(3, notes.size)

        notes.open(login)
        assertEquals(Route.Scope("app", "login", Tab.Agent("implementer")), notes.last())
    }

    @Test
    fun up_goes_back_to_the_parent_just_below_else_to_a_stack_leading_to_it() {
        val fromProject = stack(Route.Home, Route.Project("app"), login)
        fromProject.up()
        assertEquals(listOf(Route.Home, Route.Project("app")), fromProject.toList())

        val fromHome = stack(Route.Home, login)
        fromHome.up()
        assertEquals(listOf(Route.Home, Route.Project("app")), fromHome.toList())

        val repair = stack(Route.Home, Route.Settings, Route.Pair)
        repair.up()
        assertEquals(listOf(Route.Home, Route.Settings), repair.toList())
    }

    @Test
    fun leaving_a_scope_drops_its_pages_and_those_opened_from_them() {
        val shown =
            stack(
                Route.Home,
                Route.Project("app"),
                login,
                Route.Settings,
            )
        shown.leave("app", "search")
        assertEquals(4, shown.size)
        shown.leave("app", "login")
        assertEquals(listOf(Route.Home, Route.Project("app")), shown.toList())
    }

    @Test
    fun a_scope_is_dropped_once_a_snapshot_that_had_it_no_longer_does() {
        val snapshot = Snapshot.parse(SNAPSHOT)
        val shown = listOf(Route.Home, login, Route.Scope("app", "new"), Route.Scope("app", "main"))
        val seen = mutableSetOf<Pair<String, String>>()
        assertEquals(emptyList<Pair<String, String>>(), snapshot.dropped(shown, seen))
        assertEquals(setOf("app" to "login", "app" to "main"), seen)

        val without = snapshot.copy(features = snapshot.features.filter { it.name != "login" })
        assertEquals(listOf("app" to "login"), without.dropped(shown, seen))

        val unreadable = without.copy(projects = without.projects.map { it.copy(skipped = "bad") })
        assertEquals(emptyList<Pair<String, String>>(), unreadable.dropped(shown, seen))
    }
}

class WorkspaceTabsTest {
    private val snapshot = Snapshot.parse(SNAPSHOT)

    @Test
    fun a_workspace_opens_where_it_is_needed() {
        val login = snapshot.feature("app", "login")!!
        assertEquals(Tab.Agent("implementer"), defaultTab(login, null, login.agents))

        val search = snapshot.feature("app", "search")!!
        assertEquals(Tab.Summary, defaultTab(search, null, emptyList()))

        val asking = AgentSnapshot("qa", "asking")
        val agents = listOf(AgentSnapshot("implementer", "busy"), asking)
        assertEquals(Tab.Agent("qa"), defaultTab(null, Attention(), agents))
        assertEquals(null, defaultTab(null, Attention(), emptyList()))
    }

    @Test
    fun a_target_agent_the_snapshot_lacks_still_gets_a_tab_and_main_has_no_pages() {
        assertEquals(
            listOf(Tab.Agent("qa"), Tab.Summary, Tab.Brief, Tab.Details),
            tabsOf("login", emptyList(), Tab.Agent("qa")),
        )
        assertEquals(
            listOf(Tab.Agent("main")),
            tabsOf(Snapshot.MAIN, listOf(AgentSnapshot("main")), null),
        )
    }
}

class MarkdownTextTest {
    @Test
    fun a_briefs_lines_stay_lines_except_in_code() {
        assertEquals(
            "one  \ntwo\n\n```\nx\ny\n```\nlast",
            keepLineBreaks("one\ntwo\n\n```\nx\ny\n```\nlast"),
        )
    }

    @Test
    fun an_excerpt_drops_markdown_but_not_the_words() {
        assertEquals(
            "Adds search to pm, see docs",
            plainExcerpt("## Adds **search** to `pm`, see [docs](https://x)"),
        )
        assertEquals(
            "keeps snake_case_names and 2 * 3",
            plainExcerpt("keeps snake_case_names and 2 * 3"),
        )
    }
}

class WorkingLineTest {
    @Test
    fun a_working_feature_its_team_marked_ready_says_so_before_its_agents() {
        val agents = listOf(AgentSnapshot("plain", "busy"), AgentSnapshot("qa", "idle"))
        val need = { progress: String ->
            Need("app", "f", Attention(), agents, null, working = true, progress = progress)
        }
        assertEquals("ready · plain", workingLine(need("ready")))
        assertEquals("plain", workingLine(need("wip")))
    }
}
