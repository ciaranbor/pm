package dev.pm.app

import android.content.Context
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.data.defaultNetworkChanges
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.shareIn
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

    val repository = Repository(Store(context.applicationContext), http, scope, networkChanges)
}
