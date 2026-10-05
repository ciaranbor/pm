package dev.pm.app.ui

import androidx.compose.runtime.snapshotFlow

/**
 * Keep a scrolling view at its end while it follows. A change of `position` the view didn't make
 * itself (the user scrolled) decides whether it follows: only if that left it at the end. Any other
 * change that puts the end out of sight while following (content added or grown) `toEnd` undoes.
 *
 * `position` must come from the same layout pass as `behind`, and not change as content grows below
 * what is shown.
 */
suspend fun followEnd(
    position: () -> Any,
    behind: () -> Boolean,
    following: () -> Boolean,
    setFollowing: (Boolean) -> Unit,
    toEnd: suspend () -> Unit,
) {
    var last: Any? = null
    snapshotFlow { Triple(position(), behind(), following()) }
        .collect { (at, behindNow, followingNow) ->
            val moved = last != null && at != last
            last = at
            when {
                moved -> setFollowing(!behindNow)
                followingNow && behindNow -> {
                    toEnd()
                    last = position()
                }
            }
        }
}
