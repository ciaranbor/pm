package dev.pm.app.data

import dev.pm.app.api.PmClient
import dev.pm.app.api.PmError
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.BufferOverflow.DROP_OLDEST
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient

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
 * The snapshot and how fresh it is. While the app is in the foreground it holds the event stream
 * open, reconnecting with backoff, and at once when `networkChanges` emits; each snapshot it
 * receives is cached, so an unreachable server shows the last one known. The pairing is read from
 * disk, not on the caller's thread: until it is, [loaded] is false and the pairing reads as none.
 */
class Repository(
    private val store: Store,
    private val http: OkHttpClient,
    private val scope: CoroutineScope,
    networkChanges: Flow<Unit> = emptyFlow(),
    io: CoroutineDispatcher = Dispatchers.IO,
    /**
     * Where the pairing is read: apart from the snapshot cache's `io`, so neither waits on the
     * other.
     */
    prefs: CoroutineDispatcher = Dispatchers.IO,
) {
    /** The snapshot cache's reads, writes and deletes, run in the order they're asked for. */
    private val disk = io.limitedParallelism(1)

    private val _pairing = MutableStateFlow<Pairing?>(null)
    val pairing: StateFlow<Pairing?> = _pairing.asStateFlow()

    private val _loaded = MutableStateFlow(false)

    /** Whether [pairing] has been read from disk, or set since. */
    val loaded: StateFlow<Boolean> = _loaded.asStateFlow()

    private val _client = MutableStateFlow<PmClient?>(null)

    /** The paired server's client, made once per pairing; null until [loaded]. */
    val client: StateFlow<PmClient?> = _client.asStateFlow()

    /** The paired server's client, once the pairing has been read: for readers outside the UI. */
    suspend fun loadedClient(): PmClient? {
        _loaded.first { it }
        return _client.value
    }

    private val _snapshot = MutableStateFlow<Snapshot?>(null)
    val snapshot: StateFlow<Snapshot?> = _snapshot.asStateFlow()

    private val _received =
        MutableSharedFlow<Snapshot>(extraBufferCapacity = 1, onBufferOverflow = DROP_OLDEST)

    /** Each snapshot read from the server as it arrives; never the cached one. */
    val received: SharedFlow<Snapshot> = _received.asSharedFlow()

    /** When the shown snapshot was read, epoch ms. */
    private val _readAt = MutableStateFlow<Long?>(null)
    val readAt: StateFlow<Long?> = _readAt.asStateFlow()

    private val _connection = MutableStateFlow<Connection>(Connection.Connecting)
    val connection: StateFlow<Connection> = _connection.asStateFlow()

    private var stream: Job? = null

    /** Between [start] and [stop]: the stream opens once the pairing is loaded. */
    private var started = false

    init {
        scope.launch {
            val stored = withContext(prefs) { store.pairing }
            if (_loaded.value) return@launch
            paired(stored)
            if (started) start()
        }
    }

    /** Reading the cached snapshot; a newer one, or a change of pairing, supersedes it. */
    private var cacheLoad: Job? = scope.launch {
        val cached =
            withContext(disk) {
                store.cachedSnapshot()?.let { (json, at) ->
                    runCatching { Snapshot.parse(json) }.getOrNull()?.let { it to at }
                }
            }
        if (cached != null && _snapshot.value == null) {
            _snapshot.value = cached.first
            _readAt.value = cached.second
        }
    }

    init {
        scope.launch { networkChanges.collect { if (stream != null) reconnect() } }
    }

    private fun paired(pairing: Pairing?) {
        _pairing.value = pairing
        _client.value = pairing?.let { PmClient(it, http) }
        _connection.value = if (pairing == null) Connection.Unpaired else Connection.Connecting
        _loaded.value = true
    }

    /** Hold the event stream open until [stop]. */
    fun start() {
        started = true
        if (stream?.isActive == true) return
        val client = _client.value ?: return
        stream = scope.launch {
            val backoff = Backoff()
            while (true) {
                try {
                    client.events().collect { event ->
                        if (event.name != "snapshot") return@collect
                        accept(event.data)
                        if (_connection.value != Connection.Live) {
                            _connection.value = Connection.Live
                            backoff.reset()
                            scope.launch { sendSubscription() }
                        }
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (e: PmError.Unauthorized) {
                    _connection.value = Connection.Unauthorized
                    return@launch
                } catch (e: Exception) {
                    _connection.value = Connection.Unreachable(e.message ?: e.javaClass.simpleName)
                }
                delay(backoff.next())
            }
        }
    }

    fun stop() {
        started = false
        stream?.cancel()
        stream = null
    }

    private fun reconnect() {
        stop()
        start()
    }

    /** Reconnect now rather than after the backoff, as [Connection.Connecting] until it settles. */
    fun retry() {
        if (_connection.value is Connection.Unreachable) _connection.value = Connection.Connecting
        reconnect()
    }

    private suspend fun accept(json: String) {
        val snapshot =
            withContext(Dispatchers.Default) { runCatching { Snapshot.parse(json) }.getOrNull() }
                ?: return
        cacheLoad?.cancel()
        _snapshot.value = snapshot
        _readAt.value = System.currentTimeMillis()
        _received.tryEmit(snapshot)
        scope.launch(disk) { store.cacheSnapshot(json) }
    }

    private fun forget() {
        stop()
        cacheLoad?.cancel()
        store.subscription = null
        scope.launch(disk) { store.forgetSnapshot() }
        _snapshot.value = null
        _readAt.value = null
    }

    fun pair(pairing: Pairing) {
        forget()
        store.pairing = pairing
        paired(pairing)
        start()
    }

    /** Forget the pairing; the server is asked to drop this device's push subscription first. */
    fun unpair() {
        val client = _client.value
        forget()
        scope.launch { runCatching { client?.unregisterPush() } }
        store.pairing = null
        paired(null)
    }

    /** A subscription from the push distributor: kept, then sent once the server is reachable. */
    fun subscribed(endpoint: String, p256dh: String, auth: String) {
        store.subscription = Subscription(endpoint, p256dh, auth, sent = false)
        scope.launch { sendSubscription() }
    }

    fun unsubscribed() {
        store.subscription = null
        val client = _client.value ?: return
        scope.launch { runCatching { client.unregisterPush() } }
    }

    private suspend fun sendSubscription() {
        val sub = store.subscription?.takeIf { !it.sent } ?: return
        val client = _client.value ?: return
        runCatching { client.registerPush(sub.endpoint, sub.p256dh, sub.auth) }
            .onSuccess { if (store.subscription == sub) store.subscription = sub.copy(sent = true) }
    }
}
