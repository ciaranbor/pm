package dev.pm.app.data

import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlin.time.Duration.Companion.seconds

/** How the app stands with `pm serve`. */
sealed interface Connection {
    data object Unpaired : Connection
    data object Connecting : Connection
    data object Live : Connection

    /** Not reachable: a normal state off the tailnet, shown with the cached snapshot. */
    data class Unreachable(val reason: String) : Connection

    /** The server refused the token: the device was revoked. */
    data object Unauthorized : Connection
}

/**
 * The snapshot and how fresh it is. While the app is in the foreground it
 * holds the event stream open, reconnecting with backoff; each snapshot it
 * receives is cached, so an unreachable server shows the last one known.
 */
class Repository(private val store: Store) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    private val _pairing = MutableStateFlow(store.pairing)
    val pairing: StateFlow<Pairing?> = _pairing.asStateFlow()

    private val _snapshot = MutableStateFlow<Snapshot?>(null)
    val snapshot: StateFlow<Snapshot?> = _snapshot.asStateFlow()

    /** When the shown snapshot was read, epoch ms. */
    private val _readAt = MutableStateFlow<Long?>(null)
    val readAt: StateFlow<Long?> = _readAt.asStateFlow()

    private val _connection = MutableStateFlow<Connection>(if (store.pairing == null) Connection.Unpaired else Connection.Connecting)
    val connection: StateFlow<Connection> = _connection.asStateFlow()

    private var stream: Job? = null

    init {
        store.cachedSnapshot()?.let { (json, at) ->
            runCatching { Snapshot.parse(json) }.getOrNull()?.let {
                _snapshot.value = it
                _readAt.value = at
            }
        }
    }

    fun client(): PmClient? = _pairing.value?.let(::PmClient)

    /** Hold the event stream open until [stop]. */
    fun start() {
        if (stream?.isActive == true) return
        val client = client() ?: return
        stream = scope.launch {
            var backoff = 2.seconds
            while (true) {
                try {
                    client.events().collect { event ->
                        if (event.name == "snapshot") {
                            accept(event.data)
                            if (_connection.value != Connection.Live) {
                                _connection.value = Connection.Live
                                backoff = 2.seconds
                                sendSubscription()
                            }
                        }
                    }
                } catch (e: PmError.Unauthorized) {
                    _connection.value = Connection.Unauthorized
                    return@launch
                } catch (e: PmError) {
                    _connection.value = Connection.Unreachable(e.message ?: "unreachable")
                }
                delay(backoff)
                backoff = (backoff * 2).coerceAtMost(30.seconds)
            }
        }
    }

    fun stop() {
        stream?.cancel()
        stream = null
    }

    /** Reconnect now rather than after the backoff. */
    fun retry() {
        stop()
        start()
    }

    private fun accept(json: String) {
        val snapshot = runCatching { Snapshot.parse(json) }.getOrNull() ?: return
        _snapshot.value = snapshot
        _readAt.value = System.currentTimeMillis()
        scope.launch(Dispatchers.IO) { store.cacheSnapshot(json) }
    }

    fun pair(pairing: Pairing) {
        stop()
        store.pairing = pairing
        store.subscription = null
        store.forgetSnapshot()
        _snapshot.value = null
        _readAt.value = null
        _pairing.value = pairing
        _connection.value = Connection.Connecting
        start()
    }

    /** Forget the pairing; the server is asked to drop this device's push subscription first. */
    fun unpair() {
        val client = client()
        stop()
        scope.launch {
            runCatching { client?.unregisterPush() }
        }
        store.pairing = null
        store.subscription = null
        store.forgetSnapshot()
        _pairing.value = null
        _snapshot.value = null
        _readAt.value = null
        _connection.value = Connection.Unpaired
    }

    /** A new subscription from the push distributor: kept, then sent once the server is reachable. */
    fun subscribed(endpoint: String, p256dh: String, auth: String) {
        store.subscription = Subscription(endpoint, p256dh, auth, sent = false)
        scope.launch { sendSubscription() }
    }

    /** Whether no distributor has given a subscription yet. */
    val needsSubscription: Boolean get() = store.subscription == null

    fun unsubscribed() {
        store.subscription = null
        val client = client() ?: return
        scope.launch { runCatching { client.unregisterPush() } }
    }

    private suspend fun sendSubscription() {
        val sub = store.subscription?.takeIf { !it.sent } ?: return
        val client = client() ?: return
        runCatching { client.registerPush(sub.endpoint, sub.p256dh, sub.auth) }
            .onSuccess { if (store.subscription == sub) store.subscription = sub.copy(sent = true) }
    }
}
