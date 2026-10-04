package dev.pm.app.data

import android.content.Context
import androidx.core.content.edit
import dev.pm.app.model.Pairing
import java.io.File

/**
 * What the app keeps on the phone: the pairing, the last snapshot (shown while the server can't be
 * reached), and the push subscription, kept until the server has it.
 */
class Store(context: Context) {
    private val prefs = context.getSharedPreferences("pm", Context.MODE_PRIVATE)
    private val snapshotFile = File(context.filesDir, "snapshot.json")

    var pairing: Pairing?
        get() = prefs.getString(PAIRING, null)?.let(Pairing::parse)
        set(value) {
            prefs.edit {
                if (value == null) remove(PAIRING) else putString(PAIRING, Pairing.encode(value))
            }
        }

    /** The last snapshot's JSON and when it was read (epoch ms). */
    fun cachedSnapshot(): Pair<String, Long>? =
        snapshotFile
            .takeIf { it.exists() }
            ?.let { runCatching { it.readText() to it.lastModified() }.getOrNull() }

    fun cacheSnapshot(json: String) {
        val tmp = File(snapshotFile.parentFile, "${snapshotFile.name}.tmp")
        tmp.writeText(json)
        tmp.renameTo(snapshotFile)
    }

    fun forgetSnapshot() {
        snapshotFile.delete()
    }

    /** The push subscription the distributor gave, and whether the server has it. */
    var subscription: Subscription?
        get() {
            val endpoint = prefs.getString(ENDPOINT, null) ?: return null
            return Subscription(
                endpoint,
                prefs.getString(P256DH, null) ?: return null,
                prefs.getString(AUTH, null) ?: return null,
                prefs.getBoolean(SENT, false),
            )
        }
        set(value) {
            prefs.edit {
                if (value == null) {
                    remove(ENDPOINT)
                    remove(P256DH)
                    remove(AUTH)
                    remove(SENT)
                } else {
                    putString(ENDPOINT, value.endpoint)
                    putString(P256DH, value.p256dh)
                    putString(AUTH, value.auth)
                    putBoolean(SENT, value.sent)
                }
            }
        }

    private companion object {
        const val PAIRING = "pairing"
        const val ENDPOINT = "push_endpoint"
        const val P256DH = "push_p256dh"
        const val AUTH = "push_auth"
        const val SENT = "push_sent"
    }
}

data class Subscription(
    val endpoint: String,
    val p256dh: String,
    val auth: String,
    val sent: Boolean,
)
