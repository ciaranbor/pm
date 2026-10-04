package dev.pm.app

import android.app.Application
import android.content.Context
import dev.pm.app.push.Notifications

class PmApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        Notifications.createChannel(this)
    }
}

val Context.container: AppContainer get() = (applicationContext as PmApp).container
