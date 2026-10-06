package dev.pm.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import dev.pm.app.data.Connection
import kotlin.time.Duration.Companion.milliseconds
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first

/** A reconnect the user asked for, as the control they asked with shows it. */
@Stable
class ManualRetry internal constructor(private val retry: () -> Unit) {
    /** Asked for, and the connection hasn't settled yet. */
    var pending by mutableStateOf(false)
        private set

    /** The last one asked for ended unreachable; cleared once live. */
    var failedAgain by mutableStateOf(false)
        internal set

    fun start() {
        if (pending) return
        pending = true
        failedAgain = false
        retry()
    }

    internal fun settle(failed: Boolean) {
        pending = false
        failedAgain = failed
    }
}

/**
 * A [ManualRetry] of `retry`, pending until `connection` leaves [Connection.Connecting], and for at
 * least half a second: a server that refuses at once must still show that it was tried.
 */
@Composable
fun rememberRetry(connection: Connection, retry: () -> Unit): ManualRetry {
    val current = rememberUpdatedState(connection)
    val latest by rememberUpdatedState(retry)
    val state = remember { ManualRetry { latest() } }
    LaunchedEffect(state.pending) {
        if (!state.pending) return@LaunchedEffect
        delay(500.milliseconds)
        val settled = snapshotFlow { current.value }.first { it != Connection.Connecting }
        state.settle(failed = settled !is Connection.Live)
    }
    LaunchedEffect(connection) { if (connection == Connection.Live) state.failedAgain = false }
    return state
}
