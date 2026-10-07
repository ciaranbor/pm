package dev.pm.app.ui

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import androidx.test.core.app.ApplicationProvider
import dev.pm.app.OpenStream
import dev.pm.app.SNAPSHOT
import dev.pm.app.data.Connection
import dev.pm.app.data.Repository
import dev.pm.app.data.Store
import dev.pm.app.eventually
import dev.pm.app.model.Pairing
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.cancel
import kotlinx.coroutines.job
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class AppViewModelTest {
    private val server = MockWebServer()
    private val first = OpenStream()
    private val rest = OpenStream()

    @Before
    fun setUp() {
        Dispatchers.setMain(StandardTestDispatcher())
        server.start()
    }

    @After
    fun tearDown() {
        first.release()
        rest.release()
        server.close()
        Dispatchers.resetMain()
    }

    private fun vapid(key: String) = MockResponse.Builder().body("""{"vapid":"$key"}""").build()

    @Test
    fun the_push_key_comes_once_per_pairing_retrying_on_the_next_connection() = runTest {
        val url = server.url("/").toString().trimEnd('/')
        val store = Store(ApplicationProvider.getApplicationContext())
        store.pairing = Pairing(url, "pixel", "tok")
        val repository = Repository(store, OkHttpClient(), backgroundScope)
        val model = AppViewModel(repository) {}

        server.enqueue(first.response("snapshot" to SNAPSHOT))
        server.enqueue(MockResponse.Builder().code(500).build())
        repository.start()
        eventually { server.requestCount == 2 }
        assertNull(model.pushKey.value)

        server.enqueue(rest.response("snapshot" to SNAPSHOT))
        server.enqueue(vapid("K1"))
        first.release()
        eventually { repository.connection.value is Connection.Unreachable }
        advanceTimeBy(2.seconds)
        eventually { model.pushKey.value == "K1" }
        model.pushRegistered()
        assertNull(model.pushKey.value)

        server.enqueue(rest.response("snapshot" to SNAPSHOT))
        server.enqueue(vapid("K2"))
        repository.pair(Pairing(url, "pixel", "tok2"))
        eventually { model.pushKey.value == "K2" }
        assertEquals(6, server.requestCount)

        model.viewModelScope.cancel()
        eventually { model.viewModelScope.coroutineContext.job.isCompleted }
    }

    @Test
    fun the_models_drafts_outlive_the_process_through_its_saved_state_unless_too_big() = runTest {
        val store = Store(ApplicationProvider.getApplicationContext())
        val repository = Repository(store, OkHttpClient(), backgroundScope)
        val saved = SavedStateHandle()
        val first = AppViewModel(repository, saved) {}
        first.drafts["app/login/implementer"] = "half a thought"
        first.drafts["app/login/reviewer"] = "x".repeat(Drafts.SAVED_MAX)

        val again = AppViewModel(repository, saved) {}
        assertEquals("half a thought", again.drafts["app/login/implementer"])
        assertEquals("too big to save: kept in memory only", "", again.drafts["app/login/reviewer"])
        assertEquals("x".repeat(Drafts.SAVED_MAX), first.drafts["app/login/reviewer"])

        listOf(first, again).forEach { it.viewModelScope.cancel() }
        eventually {
            listOf(first, again).all { it.viewModelScope.coroutineContext.job.isCompleted }
        }
    }
}
