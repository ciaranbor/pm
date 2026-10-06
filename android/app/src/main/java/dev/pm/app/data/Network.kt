package dev.pm.app.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow

/**
 * Emits each time a different default network is validated, such as a switch from one VPN to
 * another, and each time the validated one is lost or loses validation. A stream opened over the
 * previous network may be dead without having failed yet: reopening it either moves it to the new
 * network or fails at once, so the loss shows without waiting out a read timeout.
 */
fun Context.defaultNetworkChanges(): Flow<Unit> = callbackFlow {
    val connectivity = getSystemService(ConnectivityManager::class.java)
    var last: Network? =
        connectivity.activeNetwork?.takeIf { network ->
            connectivity
                .getNetworkCapabilities(network)
                ?.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED) == true
        }
    val callback =
        object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(
                network: Network,
                capabilities: NetworkCapabilities,
            ) {
                val validated =
                    capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
                when {
                    validated && network != last -> last = network
                    !validated && network == last -> last = null
                    else -> return
                }
                trySend(Unit)
            }

            override fun onLost(network: Network) {
                if (network != last) return
                last = null
                trySend(Unit)
            }
        }
    connectivity.registerDefaultNetworkCallback(callback)
    awaitClose { connectivity.unregisterNetworkCallback(callback) }
}
