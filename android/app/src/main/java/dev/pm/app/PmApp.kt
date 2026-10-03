package dev.pm.app

import android.app.Application
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.push.Notifications

class PmApp : Application() {
    lateinit var repository: Repository
        private set

    override fun onCreate() {
        super.onCreate()
        repository = Repository(Store(this))
        Notifications.createChannel(this)
    }
}

val android.content.Context.repository: Repository get() = (applicationContext as PmApp).repository
