package dev.pm.app.data

import android.content.Context
import androidx.core.content.edit
import dev.pm.app.model.NotesDraft
import dev.pm.app.model.Pairing
import dev.pm.app.model.PushedTransition
import java.io.File
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/**
 * What the app keeps on the phone: the pairing, the last snapshot (shown while the server can't be
 * reached), the push subscription, what polling last alerted, the update check's settings, and
 * unsaved edits of each project's notes.
 */
class Store(context: Context) : NotesDrafts {
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

    /** The alerts polling made whose condition held as it last read; `null` before it has read. */
    var polled: Set<PushedTransition>?
        get() =
            prefs.getString(POLLED, null)?.let {
                runCatching { json.decodeFromString(alerts, it).toSet() }.getOrNull()
            }
        set(value) {
            prefs.edit {
                if (value == null) remove(POLLED)
                else putString(POLLED, json.encodeToString(alerts, value.toList()))
            }
        }

    /** Whether to check GitHub for a newer app. */
    var checkUpdates: Boolean
        get() = prefs.getBoolean(CHECK_UPDATES, true)
        set(value) = prefs.edit { putBoolean(CHECK_UPDATES, value) }

    /** The newest release already notified of. */
    var notifiedUpdate: String?
        get() = prefs.getString(NOTIFIED_UPDATE, null)
        set(value) = prefs.edit { putString(NOTIFIED_UPDATE, value) }

    override fun draft(project: String): NotesDraft? =
        prefs.getString(DRAFT + project, null)?.let {
            runCatching { json.decodeFromString(NotesDraft.serializer(), it) }.getOrNull()
        }

    override fun keep(project: String, draft: NotesDraft?) {
        prefs.edit {
            if (draft == null) remove(DRAFT + project)
            else putString(DRAFT + project, json.encodeToString(NotesDraft.serializer(), draft))
        }
    }

    private companion object {
        const val DRAFT = "notes_draft/"
        val json = Json { ignoreUnknownKeys = true }
        val alerts = ListSerializer(PushedTransition.serializer())
        const val POLLED = "polled"
        const val CHECK_UPDATES = "check_updates"
        const val NOTIFIED_UPDATE = "notified_update"
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

/** Unsaved edits of projects' notes, kept until saved or discarded. */
interface NotesDrafts {
    fun draft(project: String): NotesDraft?

    /** Keep `draft` as `project`'s, or forget it with null. */
    fun keep(project: String, draft: NotesDraft?)
}
