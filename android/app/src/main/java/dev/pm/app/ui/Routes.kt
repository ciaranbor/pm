package dev.pm.app.ui

import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import dev.pm.app.model.Snapshot
import dev.pm.app.push.Target
import kotlinx.serialization.Serializable

/**
 * Where the app can be. Each is a back stack entry, kept across process death. Back follows the
 * stack; Up goes to the [parent], whatever the stack holds.
 */
@Serializable
sealed interface Route : NavKey {
    @Serializable data object Pair : Route

    @Serializable data object Home : Route

    @Serializable data class Project(val project: String) : Route

    /** A scope's workspace, a feature's or `main`'s, opened on `tab`, else on its default. */
    @Serializable
    data class Scope(val project: String, val scope: String, val tab: Tab? = null) : Route

    @Serializable data class Notes(val project: String) : Route

    @Serializable data object Settings : Route

    /** The top bar's title, and the line under it that says where it is. */
    val heading: kotlin.Pair<String, String?>
        get() =
            when (this) {
                Pair -> "Pair with pm serve" to null
                Home -> "pm" to null
                Settings -> "Settings" to null
                is Project -> project to null
                is Scope -> scope to project
                is Notes -> "Notes" to project
            }

    /** Where Up leads: the page this one belongs to. */
    val parent: Route?
        get() =
            when (this) {
                Pair,
                Home -> null
                Settings,
                is Project -> Home
                is Scope -> Project(project)
                is Notes -> Project(project)
            }

    /** Whether this is the page `other` names: a workspace is one page whichever tab it is on. */
    fun samePage(other: Route): Boolean =
        if (this is Scope && other is Scope) project == other.project && scope == other.scope
        else this == other
}

/** A workspace's tab: one per agent, then a feature's pages. */
@Serializable
sealed interface Tab {
    @Serializable data class Agent(val name: String) : Tab

    @Serializable data object Summary : Tab

    @Serializable data object Brief : Tab

    @Serializable data object Details : Tab
}

/** The workspace a notification's target opens, on the agent it names. */
fun Target.route(): Route.Scope = Route.Scope(project, scope, agent?.let(Tab::Agent))

internal fun NavBackStack<NavKey>.replaceWith(routes: List<Route>) {
    clear()
    addAll(routes)
}

/**
 * Open `route` over what is shown, so Back returns there; the workspace shown already switches to
 * its tab instead.
 */
internal fun NavBackStack<NavKey>.open(route: Route) {
    val top = lastOrNull() as? Route
    when {
        top == route -> Unit
        top != null && top.samePage(route) ->
            if ((route as? Route.Scope)?.tab != null) set(lastIndex, route)
        else -> add(route)
    }
}

/**
 * Go to the shown page's parent: back to it where it is just below, else to a stack that leads from
 * the start screen to it. A page without one goes back.
 */
internal fun NavBackStack<NavKey>.up() {
    val parent = (lastOrNull() as? Route)?.parent
    if (parent == null) {
        if (size > 1) removeLastOrNull()
        return
    }
    removeLastOrNull()
    val below = lastOrNull() as? Route
    if (below != null && below.samePage(parent)) return
    replaceWith(generateSequence(parent) { it.parent }.toList().asReversed())
}

/**
 * Drop the pages of `scope` in `project`, which is gone, and every page opened from them; the start
 * screen if nothing is left. `main` goes only with its project, so leaving it drops all the
 * project's pages.
 */
internal fun NavBackStack<NavKey>.leave(project: String, scope: String) {
    val at = indexOfFirst {
        (it as? Route)?.scopeOf()?.let { shown -> (project to scope).covers(shown) } == true
    }
    if (at < 0) return
    repeat(size - at) { removeLastOrNull() }
    if (isEmpty()) add(Route.Home)
}

/** Whether this scope's going takes `other` with it: itself, or any of a project's when `main`. */
internal fun kotlin.Pair<String, String>.covers(other: kotlin.Pair<String, String>): Boolean =
    this == other || (second == Snapshot.MAIN && first == other.first)

/**
 * The scope a page shows, as project and scope: a project's own pages are its `main`'s, which is
 * there as long as the project is.
 */
internal fun Route.scopeOf(): kotlin.Pair<String, String>? =
    when (this) {
        is Route.Scope -> project to scope
        is Route.Project -> project to Snapshot.MAIN
        is Route.Notes -> project to Snapshot.MAIN
        else -> null
    }

/**
 * Whether the snapshot has `scope` of `project`: null when it can't say, as for a project it can't
 * read, whose scopes it leaves out.
 */
internal fun Snapshot.has(project: String, scope: String): Boolean? {
    val known = project(project)
    return when {
        known?.skipped != null -> null
        scope == Snapshot.MAIN -> known != null
        else -> feature(project, scope) != null
    }
}

/** Where a workspace page sits in the stack's state: the same whichever tab it shows. */
internal fun Route.Scope.contentKey(): String = "scope/$project/$scope"

/**
 * The scopes the back stack shows that this snapshot no longer has, of those an earlier snapshot
 * had: merged or deleted. One not seen yet may be newer than the snapshots so far, as a feature a
 * push announced can be. A gone project stands for all its scopes. Adds those it has to `seen`.
 */
internal fun Snapshot.dropped(
    stack: List<NavKey>,
    seen: MutableSet<kotlin.Pair<String, String>>,
): List<kotlin.Pair<String, String>> =
    stack
        .mapNotNull { (it as? Route)?.scopeOf() }
        .distinct()
        .filter { shown ->
            when (has(shown.first, shown.second)) {
                true -> seen.add(shown).let { false }
                false -> shown in seen
                null -> false
            }
        }
        .let { gone -> gone.filter { scope -> gone.none { it != scope && it.covers(scope) } } }
