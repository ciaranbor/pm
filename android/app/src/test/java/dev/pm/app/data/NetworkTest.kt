package dev.pm.app.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.shadows.ShadowNetwork
import org.robolectric.shadows.ShadowNetworkCapabilities

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class NetworkTest {
    private val context: Context = ApplicationProvider.getApplicationContext()
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)

    private fun capabilities(validated: Boolean): NetworkCapabilities = ShadowNetworkCapabilities.newInstance().also {
        if (validated) shadowOf(it).addCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }

    /** The app's own repository registers a callback too; this is the one the test's collector added. */
    private lateinit var callback: ConnectivityManager.NetworkCallback

    private fun TestScope.follow(): () -> Int {
        val before = shadowOf(connectivity).networkCallbacks.toSet()
        var changes = 0
        backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) { context.defaultNetworkChanges().collect { changes++ } }
        callback = (shadowOf(connectivity).networkCallbacks - before).single()
        return {
            runCurrent()
            changes
        }
    }

    private fun validated(network: Network) = callback.onCapabilitiesChanged(network, capabilities(validated = true))

    @Test
    fun emits_once_for_each_different_network_once_it_is_validated() = runTest {
        shadowOf(connectivity).setActiveNetworkInfo(null)
        val changes = follow()
        val vpn = ShadowNetwork.newInstance(1)
        val tailscale = ShadowNetwork.newInstance(2)

        callback.onCapabilitiesChanged(vpn, capabilities(validated = false))
        assertEquals(0, changes())
        validated(vpn)
        validated(vpn)
        assertEquals(1, changes())

        validated(tailscale)
        assertEquals(2, changes())

        callback.onLost(tailscale)
        validated(tailscale)
        assertEquals(3, changes())
    }

    @Test
    fun the_network_already_validated_at_the_start_is_no_change() = runTest {
        val active = connectivity.activeNetwork!!
        shadowOf(connectivity).setNetworkCapabilities(active, capabilities(validated = true))
        val changes = follow()

        validated(active)
        assertEquals(0, changes())
        validated(ShadowNetwork.newInstance(7))
        assertEquals(1, changes())
    }
}
