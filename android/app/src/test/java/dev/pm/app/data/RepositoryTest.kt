package dev.pm.app.data

import androidx.test.core.app.ApplicationProvider
import dev.pm.app.OpenStream
import dev.pm.app.SNAPSHOT
import dev.pm.app.eventually
import dev.pm.app.model.Pairing
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.minutes
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class RepositoryTest {
    private val server = MockWebServer()
    private val stream = OpenStream()
    private lateinit var store: Store
    private val network = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    @Before
    fun setUp() {
        server.start()
        store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok")
    }

    @After
    fun tearDown() {
        stream.release()
        server.close()
    }

    private fun TestScope.repository(
        io: CoroutineDispatcher = Dispatchers.IO,
        prefs: CoroutineDispatcher = Dispatchers.IO,
    ) = Repository(store, OkHttpClient(), backgroundScope, network, io, prefs)

    /** A disk that runs nothing until opened, then everything in order. */
    private class HeldDisk {
        private val thread = Executors.newSingleThreadExecutor()
        private val held = CountDownLatch(1)
        val dispatcher = thread.asCoroutineDispatcher()

        init {
            thread.execute { held.await() }
        }

        /** Open it, and return once what was queued has run. */
        fun drain() {
            held.countDown()
            thread.submit {}.get()
        }
    }

    private fun eventsRequests() = server.requestCount

    @Test
    fun the_cached_snapshot_shows_until_the_server_sends_one_which_is_cached() = runTest {
        store.cacheSnapshot(SNAPSHOT)
        val repository = repository()
        eventually { repository.snapshot.value != null }
        assertEquals(listOf("app"), repository.snapshot.value!!.projects.map { it.name })
        assertNotNull(repository.readAt.value)

        val fresh = """{"version":1,"projects":[],"features":[]}"""
        server.enqueue(stream.response("snapshot" to fresh))
        repository.start()
        eventually { repository.connection.value == Connection.Live }
        assertEquals(emptyList<Any>(), repository.snapshot.value!!.projects)
        eventually { store.cachedSnapshot()?.first == fresh }
    }

    @Test
    fun a_refused_token_stops_reconnecting() = runTest {
        server.enqueue(MockResponse.Builder().code(401).body("""{"error":"revoked"}""").build())
        val repository = repository()
        repository.start()
        eventually { repository.connection.value == Connection.Unauthorized }
        advanceTimeBy(5.minutes)
        runCurrent()
        assertEquals(1, eventsRequests())
    }

    @Test
    fun a_network_change_reconnects_without_waiting_out_the_backoff() = runTest {
        server.enqueue(MockResponse.Builder().code(502).build())
        val repository = repository()
        repository.start()
        eventually { repository.connection.value is Connection.Unreachable }

        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        advanceTimeBy(900.milliseconds)
        runCurrent()
        assertEquals(1, eventsRequests())
        network.tryEmit(Unit)
        eventually { repository.connection.value == Connection.Live }
        assertEquals(2, eventsRequests())
    }

    @Test
    fun losing_the_network_while_live_shows_offline_without_waiting_for_the_stream_to_time_out() =
        runTest {
            server.enqueue(stream.response("snapshot" to SNAPSHOT))
            val repository = repository()
            repository.start()
            eventually { repository.connection.value == Connection.Live }

            server.enqueue(MockResponse.Builder().code(502).build())
            network.tryEmit(Unit)
            eventually { repository.connection.value is Connection.Unreachable }
        }

    @Test
    fun a_retry_asked_for_while_unreachable_is_connecting_until_it_settles() = runTest {
        server.enqueue(MockResponse.Builder().code(502).build())
        val repository = repository()
        repository.start()
        eventually { repository.connection.value is Connection.Unreachable }

        server.enqueue(MockResponse.Builder().code(502).build())
        repository.retry()
        assertEquals(Connection.Connecting, repository.connection.value)
        eventually { repository.connection.value is Connection.Unreachable }

        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        repository.retry()
        eventually { repository.connection.value == Connection.Live }
        assertEquals(3, eventsRequests())
    }

    @Test
    fun the_pairing_is_read_off_the_callers_thread_and_a_start_asked_for_meanwhile_holds() =
        runTest {
            server.enqueue(stream.response("snapshot" to SNAPSHOT))
            val disk = HeldDisk()
            val repository = repository(prefs = disk.dispatcher)
            repository.start()
            runCurrent()
            assertEquals(false, repository.loaded.value)
            assertNull(repository.pairing.value)
            val client = async { repository.loadedClient() }
            runCurrent()
            assertFalse(client.isCompleted)

            disk.drain()
            eventually { client.isCompleted }
            assertNotNull(client.await())
            eventually { repository.connection.value == Connection.Live }
            assertEquals("tok", repository.pairing.value?.token)
        }

    @Test
    fun a_failure_is_retried_after_the_backoff() = runTest {
        server.enqueue(MockResponse.Builder().code(502).build())
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        val repository = repository()
        repository.start()
        eventually { repository.connection.value is Connection.Unreachable }
        advanceTimeBy(2_001.milliseconds)
        eventually { repository.connection.value == Connection.Live }
    }

    @Test
    fun a_subscription_is_sent_once_the_server_is_reached() = runTest {
        store.subscription = Subscription("https://ntfy.sh/upX", "BKey", "auth", sent = false)
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        server.enqueue(MockResponse.Builder().code(204).build())
        val repository = repository()
        repository.start()
        eventually { store.subscription?.sent == true }
        assertEquals("/v1/events", server.takeRequest().url.encodedPath)
        val put = server.takeRequest()
        assertEquals("PUT", put.method)
        assertTrue(put.body!!.utf8().contains("https://ntfy.sh/upX"))
    }

    @Test
    fun pairing_again_drops_the_cached_snapshot_still_being_read() = runTest {
        store.cacheSnapshot(SNAPSHOT)
        val disk = HeldDisk()
        val repository = repository(disk.dispatcher)
        runCurrent()
        repository.pair(Pairing(server.url("/").toString().trimEnd('/'), "pixel", "tok2"))
        disk.drain()
        runCurrent()
        assertNull(repository.snapshot.value)
        assertNull(store.cachedSnapshot())
    }

    @Test
    fun a_snapshot_still_being_cached_as_the_phone_unpairs_is_not_kept() = runTest {
        val disk = HeldDisk()
        server.enqueue(stream.response("snapshot" to SNAPSHOT))
        val repository = repository(disk.dispatcher)
        repository.start()
        eventually { repository.connection.value == Connection.Live }
        repository.unpair()
        disk.drain()
        assertNull(store.cachedSnapshot())
    }
}
