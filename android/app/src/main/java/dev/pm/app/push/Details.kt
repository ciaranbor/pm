package dev.pm.app.push

import dev.pm.app.api.PmClient
import dev.pm.app.model.Alert
import dev.pm.app.model.AttentionKind
import dev.pm.app.model.PushedTransition
import dev.pm.app.model.Snapshot
import kotlinx.coroutines.CancellationException

/** What the server says of an alert, read over the tailnet: what its push leaves out. */
internal object Details {
    /**
     * `transition`'s alert in the words of `snapshot` (read if not given) and, for an agent asking,
     * of its open dialogs; null when nothing could be read, or the snapshot shows its need over.
     */
    suspend fun of(
        client: PmClient,
        transition: PushedTransition,
        snapshot: Snapshot? = null,
    ): Alert? {
        val read = snapshot ?: snapshot(client)
        if (read != null && !transition.holds(read)) return null
        val agent = transition.agent
        val dialogs =
            if (transition.kindOf == AttentionKind.Asking && agent != null) {
                attempt { client.dialogs(transition.project, transition.scope, agent) }
            } else null
        if (read == null && dialogs == null) return null
        return Alert.of(transition, read, dialogs.orEmpty())
    }

    /** The server's snapshot, if it can be read and understood. */
    suspend fun snapshot(client: PmClient): Snapshot? = attempt {
        client.snapshot()
    }
        ?.takeIf { it.understood }

    private suspend fun <T> attempt(read: suspend () -> T): T? =
        try {
            read()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            null
        }
}
