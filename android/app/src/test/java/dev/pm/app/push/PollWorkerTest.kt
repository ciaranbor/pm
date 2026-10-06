package dev.pm.app.push

import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.testing.TestListenableWorkerBuilder
import dev.pm.app.SNAPSHOT
import dev.pm.app.container
import dev.pm.app.data.Subscription
import dev.pm.app.model.Pairing
import java.util.Collections
import kotlinx.coroutines.runBlocking
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.unifiedpush.android.connector.FailedReason

@RunWith(RobolectricTestRunner::class)
class PollWorkerTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val server = MockWebServer()
    private val snapshots = Collections.synchronizedList(mutableListOf<String>())

    @Before
    fun start() {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse =
                    if (request.target.startsWith("/v1/snapshot")) {
                        snapshots += request.target
                        MockResponse.Builder().body(SNAPSHOT).build()
                    } else MockResponse.Builder().code(404).build()
            }
        server.start()
        val repository = context.container.repository
        repository.pair(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok"))
        repository.stop()
    }

    @After
    fun stop() {
        server.close()
    }

    private fun poll() = runBlocking {
        TestListenableWorkerBuilder<PollWorker>(context).build().doWork()
    }

    private fun alerts() =
        context.getSystemService(NotificationManager::class.java).activeNotifications.toList()

    @Test
    fun polling_waits_while_a_push_subscription_is_held_and_takes_over_once_registration_fails() {
        context.container.store.subscription =
            Subscription("https://fcm.googleapis.com/x", "p256dh", "auth", sent = true)
        poll()
        assertEquals(emptyList<String>(), snapshots.toList())

        Robolectric.buildService(PushService::class.java)
            .create()
            .get()
            .onRegistrationFailed(FailedReason.INTERNAL_ERROR, "default")
        // A poll that has read before, so login's blocked and search's ready alerts are new to it.
        context.container.store.polled = emptySet()
        poll()
        assertEquals(listOf("/v1/snapshot"), snapshots.toList())
        val shown = alerts().filter { it.tag != null }
        assertEquals(3, shown.size)
        val blocked = shown.single {
            it.notification.channelId == "needs-input" && it.notification.actions != null
        }
        assertEquals(
            "a polled alert of an agent blocked on the user is answered inline, as a pushed one is",
            1,
            blocked.notification.actions.count { it.remoteInputs?.isNotEmpty() == true },
        )
    }
}
