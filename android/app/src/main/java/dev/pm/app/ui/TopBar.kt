package dev.pm.app.ui

import androidx.compose.foundation.layout.RowScope
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue

/**
 * The top bar's actions for the screen shown, in place of the app's own. A screen fills it with
 * [TopBarActions]; while a transition composes two screens, the last to claim it holds it.
 */
class TopBarSlot {
    var actions: (@Composable RowScope.() -> Unit)? by mutableStateOf(null)
        private set

    private var owner: Any? = null

    internal fun claim(by: Any, content: @Composable RowScope.() -> Unit) {
        owner = by
        actions = content
    }

    internal fun release(by: Any) {
        if (owner !== by) return
        owner = null
        actions = null
    }
}

/** Put `content` in `slot` while this is composed. */
@Composable
fun TopBarActions(slot: TopBarSlot, content: @Composable RowScope.() -> Unit) {
    val latest by rememberUpdatedState(content)
    val owner = remember { Any() }
    DisposableEffect(slot, owner) {
        slot.claim(owner) { latest() }
        onDispose { slot.release(owner) }
    }
}
