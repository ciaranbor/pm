package dev.pm.app.push

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import dev.pm.app.container
import dev.pm.app.model.Poll
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException

/**
 * Notifies by polling the snapshot while the app holds no push subscription: no distributor gave it
 * an endpoint, or the one it registered with failed. Android runs it every 15 minutes at best, and
 * far less often once the phone dozes or pm goes unused; it does nothing off the tailnet.
 */
class PollWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val container = applicationContext.container
        val client = container.repository.client.value ?: return Result.success()
        if (container.store.subscription != null) {
            // Pushes arrive; a later poll starts afresh rather than from what pushes superseded.
            container.store.polled = null
            return Result.success()
        }
        val snapshot =
            try {
                client.snapshot()
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                return Result.success()
            }
        if (!snapshot.understood) return Result.success()
        val (kept, made) = Poll.judge(container.store.polled, snapshot)
        container.store.polled = kept
        made.forEach { Notifications.show(applicationContext, it) }
        Notifications.reconcile(applicationContext, snapshot)
        return Result.success()
    }

    companion object {
        private const val NAME = "poll"

        fun schedule(context: Context) {
            val request =
                PeriodicWorkRequestBuilder<PollWorker>(15, TimeUnit.MINUTES)
                    .setConstraints(
                        Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build()
                    )
                    .build()
            WorkManager.getInstance(context)
                .enqueueUniquePeriodicWork(NAME, ExistingPeriodicWorkPolicy.KEEP, request)
        }

        fun cancel(context: Context) {
            WorkManager.getInstance(context).cancelUniqueWork(NAME)
        }
    }
}
