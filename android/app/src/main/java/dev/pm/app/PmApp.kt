package dev.pm.app

import android.app.Application
import android.content.Context
import androidx.work.Configuration
import dev.pm.app.push.Notifications

/** WorkManager starts on first use, from [workManagerConfiguration], rather than at launch. */
class PmApp : Application(), Configuration.Provider {
    override val workManagerConfiguration: Configuration
        get() = Configuration.Builder().build()

    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        Notifications.createChannels(this)
    }
}

val Context.container: AppContainer
    get() = (applicationContext as PmApp).container
