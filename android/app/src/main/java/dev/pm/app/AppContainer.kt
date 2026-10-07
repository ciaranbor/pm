package dev.pm.app

import android.content.Context
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.data.defaultNetworkChanges
import dev.pm.app.model.PushedTransition
import dev.pm.app.push.Details
import dev.pm.app.push.Notifications
import dev.pm.app.push.PollWorker
import dev.pm.app.update.UpdateWorker
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.shareIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.OkHttpClient

/** The app's long-lived objects, made once per process. */
class AppContainer(context: Context) {
    private val app = context.applicationContext

    /** Every request shares its connection pool and threads. */
    val http = OkHttpClient()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    /** One network callback, however many streams follow it. */
    val networkChanges: Flow<Unit> =
        context.applicationContext
            .defaultNetworkChanges()
            .shareIn(scope, SharingStarted.WhileSubscribed())

    val store = Store(context.applicationContext)

    val repository = Repository(store, http, scope, networkChanges)

    /**
     * Fill in `transition`'s alert, posted as its push alone told it, with what the server says of
     * it, if the server answers within 10 s; and withdraw the alerts its snapshot shows are over,
     * which no push announces.
     */
    fun detail(transition: PushedTransition) {
        scope.launch(Dispatchers.IO) {
            withTimeoutOrNull(10.seconds) {
                val client = repository.loadedClient() ?: return@withTimeoutOrNull
                val snapshot = Details.snapshot(client)
                snapshot?.let { Notifications.reconcile(app, it) }
                Details.of(client, transition, snapshot)?.let { Notifications.detailed(app, it) }
            }
        }
    }

    init {
        scope.launch {
            repository.loaded.first { it }
            repository.pairing.collect {
                if (it == null) PollWorker.cancel(app) else PollWorker.schedule(app)
            }
        }
        if (BuildConfig.SELF_UPDATE) UpdateWorker.schedule(app, store.checkUpdates)
        scope.launch {
            repository.received.collect { if (it.understood) Notifications.reconcile(app, it) }
        }
    }
}
