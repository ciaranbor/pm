package dev.pm.app.ui

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.pm.app.api.PmClient
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.model.Pairing
import java.time.Instant
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/**
 * What every screen shares: the repository's state, the clock activity is measured by, and push
 * registration.
 */
class AppViewModel(private val repository: Repository, private val unsubscribePush: () -> Unit) :
    ViewModel() {
    val pairing: StateFlow<Pairing?> = repository.pairing
    val client: StateFlow<PmClient?> = repository.client
    val snapshot = repository.snapshot
    val connection: StateFlow<Connection> = repository.connection
    val readAt = repository.readAt

    val now: StateFlow<Instant> = flow {
        while (true) {
            emit(Instant.now())
            delay(30.seconds)
        }
    }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), Instant.now())

    private val _pushKey = MutableStateFlow<String?>(null)

    /**
     * The server's VAPID key to register with the push distributor against: once per pairing, after
     * the server is first reached, until [pushRegistered]. Registering again is harmless, and is
     * how a lost distributor is replaced and a subscription the server dropped is sent again.
     */
    val pushKey: StateFlow<String?> = _pushKey.asStateFlow()

    init {
        @OptIn(ExperimentalCoroutinesApi::class)
        viewModelScope.launch {
            repository.client
                .flatMapLatest { client ->
                    flow {
                        emit(null)
                        if (client == null) return@flow
                        while (true) {
                            connection.first { it == Connection.Live }
                            val key = vapidOf(client)
                            if (key != null) {
                                emit(key)
                                return@flow
                            }
                            connection.first { it != Connection.Live }
                        }
                    }
                }
                .collect { _pushKey.value = it }
        }
    }

    fun pushRegistered() {
        _pushKey.value = null
    }

    fun start() = repository.start()

    fun stop() = repository.stop()

    fun retry() = repository.retry()

    fun pair(pairing: Pairing) = repository.pair(pairing)

    fun unpair() {
        unsubscribePush()
        repository.unpair()
    }

    suspend fun vapid(): String? = client.value?.let { vapidOf(it) }

    private suspend fun vapidOf(client: PmClient): String? =
        try {
            client.vapidKey()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            null
        }
}
