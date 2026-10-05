package dev.pm.app

import android.content.Context
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.data.defaultNetworkChanges
import dev.pm.app.push.Notifications
import dev.pm.app.push.PollWorker
import dev.pm.app.update.UpdateWorker
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.shareIn
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient

/** The app's long-lived objects, made once per process. */
class AppContainer(context: Context) {
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

    init {
        val app = context.applicationContext
        scope.launch {
            repository.pairing.collect {
                if (it == null) PollWorker.cancel(app) else PollWorker.schedule(app)
            }
        }
        if (BuildConfig.SELF_UPDATE) UpdateWorker.schedule(app, store.checkUpdates)
        scope.launch {
            repository.received.collect {
                if (it.understood) Notifications.reconcile(context.applicationContext, it)
            }
        }
    }
}
