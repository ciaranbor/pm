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

    /** A scope's workspace, a feature's or `main`'s, showing `agent`, else its default. */
    @Serializable
    data class Scope(val project: String, val scope: String, val agent: String? = null) : Route

    /** A feature's own page: where it stands, with its summary, brief or details. */
    @Serializable
    data class Feature(val project: String, val scope: String, val page: Page = Page.Summary) :
        Route

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
                is Feature -> scope to "$project · Feature"
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
                is Feature -> Scope(project, scope)
                is Notes -> Project(project)
            }

    /**
     * Whether this is the page `other` names: a workspace is one page whichever agent it shows, and
     * a feature's page one whichever of its pages.
     */
    fun samePage(other: Route): Boolean =
        when {
            this is Scope && other is Scope -> project == other.project && scope == other.scope
            this is Feature && other is Feature -> project == other.project && scope == other.scope
            else -> this == other
        }
}

/** What a feature's page shows under where the feature stands. */
@Serializable
enum class Page {
    Summary,
    Brief,
    Details,
}

/** Where a notification's target opens: a ready alert, the feature's page; else its workspace. */
fun Target.route(): Route =
    if (ready) Route.Feature(project, scope) else Route.Scope(project, scope, agent)

internal fun NavBackStack<NavKey>.replaceWith(routes: List<Route>) {
    clear()
    addAll(routes)
}

/**
 * Open `route` over what is shown, so Back returns there. A shown page that is `route`'s page
 * switches to what it names instead; so does the page below when the shown page is its child, which
 * closes.
 */
internal fun NavBackStack<NavKey>.open(route: Route) {
    val below = getOrNull(lastIndex - 1) as? Route
    val shownParent = (lastOrNull() as? Route)?.parent
    if (below != null && below.samePage(route) && shownParent?.samePage(below) == true)
        removeLastOrNull()
    val top = lastOrNull() as? Route
    when {
        top == route -> Unit
        top != null && top.samePage(route) ->
            if (route !is Route.Scope || route.agent != null) set(lastIndex, route)
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
        is Route.Feature -> project to scope
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

/** Where a workspace sits in the stack's state: the same whichever agent it shows. */
internal fun Route.Scope.contentKey(): String = "scope/$project/$scope"

/** Where a feature's page sits in the stack's state: the same whichever page it shows. */
internal fun Route.Feature.contentKey(): String = "feature/$project/$scope"

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
