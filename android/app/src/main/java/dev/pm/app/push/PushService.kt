package dev.pm.app.push

import android.util.Log
import dev.pm.app.container
import dev.pm.app.model.PushedTransition
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService as UnifiedPushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/** Receives what the UnifiedPush distributor delivers: endpoints, and pushes from `pm serve`. */
class PushService : UnifiedPushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val keys =
            endpoint.pubKeySet
                ?: run {
                    Log.w(
                        TAG,
                        "the distributor gave no Web Push keys; pm serve only sends encrypted pushes",
                    )
                    return
                }
        container.repository.subscribed(endpoint.url, keys.pubKey, keys.auth)
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) {
            Log.w(TAG, "dropped a push that didn't decrypt")
            return
        }
        val transition = PushedTransition.parse(message.content) ?: return
        Notifications.show(this, transition)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Log.w(TAG, "push registration failed: $reason")
    }

    override fun onUnregistered(instance: String) {
        container.repository.unsubscribed()
    }

    private companion object {
        const val TAG = "pm-push"
    }
}
