package dev.pm.app.update

import android.content.Context
import android.os.Build
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import dev.pm.app.BuildConfig
import dev.pm.app.container
import dev.pm.app.push.Notifications
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException

/** Checks for a newer app as it starts and daily, notifying of each new release once. */
class UpdateWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val container = applicationContext.container
        if (!container.store.checkUpdates) return Result.success()
        val update =
            try {
                check(container.http)
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                return Result.success()
            }
        if (update != null && update.version != container.store.notifiedUpdate) {
            offer(applicationContext, update)
        }
        return Result.success()
    }

    companion object {
        private const val NOW = "update-check"
        private const val DAILY = "update-check-daily"

        /** This app's update, if there is one. */
        suspend fun check(http: okhttp3.OkHttpClient): Update? =
            Updates(http).check(BuildConfig.VERSION_NAME, Build.SUPPORTED_ABIS.toList())

        /**
         * Post `update`'s notification, which the daily check then doesn't repeat; one that
         * couldn't be posted, it posts once notifications are on.
         */
        fun offer(context: Context, update: Update) {
            if (Notifications.update(context, update)) {
                context.container.store.notifiedUpdate = update.version
            }
        }

        /** Check now and daily while `enabled`; otherwise stop checking. */
        fun schedule(context: Context, enabled: Boolean) {
            val work = WorkManager.getInstance(context)
            if (!enabled) {
                work.cancelUniqueWork(NOW)
                work.cancelUniqueWork(DAILY)
                return
            }
            val online = Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
            work.enqueueUniqueWork(
                NOW,
                ExistingWorkPolicy.KEEP,
                OneTimeWorkRequestBuilder<UpdateWorker>().setConstraints(online).build(),
            )
            work.enqueueUniquePeriodicWork(
                DAILY,
                ExistingPeriodicWorkPolicy.KEEP,
                PeriodicWorkRequestBuilder<UpdateWorker>(1, TimeUnit.DAYS)
                    .setConstraints(online)
                    .build(),
            )
        }
    }
}
