package dev.pm.app.api

import dev.pm.app.model.Dialog
import dev.pm.app.model.DialogAnswer
import dev.pm.app.model.DocCategory
import dev.pm.app.model.FeatureInfo
import dev.pm.app.model.MergeCheck
import dev.pm.app.model.Notes
import dev.pm.app.model.Pairing
import dev.pm.app.model.Snapshot
import dev.pm.app.model.TranscriptPage
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.channels.trySendBlocking
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources

/** Why a request to `pm serve` failed. */
sealed class PmError(message: String) : Exception(message) {
    /** The server can't be reached: the phone or the server is off the tailnet. */
    class Unreachable(cause: IOException) : PmError(cause.message ?: "unreachable")

    /** The token was refused: the device was revoked. */
    class Unauthorized : PmError("this device's token was refused")

    /**
     * The server doesn't serve what was asked: it predates it, or, when `retired`, it served it
     * once and dropped it, so this app predates the server.
     */
    class Unsupported(val retired: Boolean = false) :
        PmError(
            if (retired) "the server no longer serves this" else "the server doesn't serve this yet"
        )

    /** The agent's session hasn't started, or its transcript is gone. */
    class NoConversation : PmError("the agent has no conversation yet")

    /**
     * The agent can't take this now; `code` says why (`asking`, `not-at-prompt`, `answered`, …).
     */
    class Refused(val code: String, message: String) : PmError(message)

    /** A save of the notes was refused: they changed since the version it started from. */
    class NotesChanged(val current: Notes) : PmError("the notes changed since they were read")

    class Status(val code: Int, message: String) : PmError(message)
}

/** How typed text reached an agent (`POST …/input`). */
@Serializable
data class Delivered(
    /** `sent`, submitted as a prompt; `queued`, held until a step of the running turn ends. */
    val delivery: String,
    /** For `sent`: whether the server saw it in the conversation. */
    val confirmed: Boolean? = null,
) {
    val queued: Boolean
        get() = delivery == "queued"
}

/** One server-sent event. */
data class ServerEvent(val name: String, val data: String)

/** A client of `pm serve`'s API (docs/remote-api.md), as one paired device. */
class PmClient(private val pairing: Pairing, base: OkHttpClient = OkHttpClient()) {
    private val _serverVersion = MutableStateFlow<String?>(null)

    /** The version of pm the server last answered as (its `Pm-Version` header). */
    val serverVersion: StateFlow<String?> = _serverVersion.asStateFlow()

    private val http =
        base
            .newBuilder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .addInterceptor { chain ->
                chain
                    .proceed(
                        chain
                            .request()
                            .newBuilder()
                            .header("Authorization", "Bearer ${pairing.token}")
                            .build()
                    )
                    .also { response ->
                        response.header("Pm-Version")?.let { _serverVersion.value = it }
                    }
            }
            .build()

    /**
     * A merge runs a fetch, and a restart waits for the harness to start, so their requests wait
     * longer than a read.
     */
    private val lifecycle = http.newBuilder().readTimeout(2, TimeUnit.MINUTES).build()

    /**
     * Longer than the server's 25 s heartbeat, so a silent stream is a dead one, and shorter than
     * two of them, so one missed heartbeat is enough to tell.
     */
    private val streaming = http.newBuilder().readTimeout(35, TimeUnit.SECONDS).build()

    private val api: HttpUrl = "${pairing.url}/v1/".toHttpUrl()

    private fun url(vararg segments: String, query: Map<String, String?> = emptyMap()): HttpUrl {
        val builder = api.newBuilder()
        segments.forEach { builder.addPathSegment(it) }
        query.forEach { (k, v) -> if (v != null) builder.addQueryParameter(k, v) }
        return builder.build()
    }

    suspend fun snapshot(): Snapshot = Snapshot.parse(get(url("snapshot")))

    suspend fun feature(project: String, feature: String): FeatureInfo =
        json.decodeFromString(FeatureInfo.serializer(), get(url("features", project, feature)))

    suspend fun mergeCheck(project: String, feature: String): MergeCheck =
        json.decodeFromString(
            MergeCheck.serializer(),
            get(url("features", project, feature, "merge")),
        )

    suspend fun summary(project: String, feature: String): String =
        get(url("features", project, feature, "summary"))

    /** The project's notes, with the version a save names. */
    suspend fun notes(project: String): Notes =
        call(Request.Builder().url(url("projects", project, "notes")).build()) {
            Notes(it.body.string(), it.header("ETag").orEmpty().trim('"'))
        }

    /**
     * Save `text` as the project's notes if they are still at version `base`; returns the new
     * version. Throws [PmError.NotesChanged] when they are not.
     */
    suspend fun saveNotes(project: String, text: String, base: String): String =
        call(
            Request.Builder()
                .url(url("projects", project, "notes"))
                .header("If-Match", "\"$base\"")
                .put(text.toRequestBody(MARKDOWN))
                .build(),
            refuse = { response ->
                if (response.code != 409) return@call null
                // Read whole, not peeked: it carries the notes, which the terminal can grow
                // to any length.
                val body = response.body.string()
                val changed = runCatching {
                    json.decodeFromString(Changed.serializer(), body)
                }
                    .getOrNull()
                if (changed?.refused == "changed")
                    PmError.NotesChanged(Notes(changed.text, changed.version))
                else PmError.Status(409, changed?.error ?: "HTTP 409")
            },
        ) {
            json.decodeFromString(Saved.serializer(), it.body.string()).version
        }

    /**
     * The categories of the project's information store; [PmError.Unsupported] from a server that
     * predates it.
     */
    suspend fun docs(project: String): List<DocCategory> =
        json.decodeFromString(DocList.serializer(), get(url("projects", project, "docs"))).docs

    /** The doc `filename` of the project's information store, Markdown. */
    suspend fun doc(project: String, filename: String): String =
        get(url("projects", project, "docs", filename))

    suspend fun screen(project: String, scope: String, agent: String): String =
        get(url("agents", project, scope, agent, "screen"))

    suspend fun transcript(
        project: String,
        scope: String,
        agent: String,
        before: String? = null,
    ): TranscriptPage =
        json.decodeFromString(
            TranscriptPage.serializer(),
            get(
                url(
                    "agents",
                    project,
                    scope,
                    agent,
                    "transcript",
                    query = mapOf("before" to before),
                )
            ),
        )

    /** A tool result's whole output, when the transcript cut it short. */
    suspend fun toolResult(project: String, scope: String, agent: String, ref: String): String =
        get(
            url(
                "agents",
                project,
                scope,
                agent,
                "transcript",
                "result",
                query = mapOf("ref" to ref),
            )
        )

    /** Type `text` into the agent's input line and submit it. */
    suspend fun sendText(project: String, scope: String, agent: String, text: String): Delivered =
        json.decodeFromString(
            Delivered.serializer(),
            post(
                url("agents", project, scope, agent, "input"),
                json.encodeToString(TextBody.serializer(), TextBody(text)),
            ),
        )

    /** Press Escape in the agent's pane, ending its turn. */
    suspend fun interrupt(project: String, scope: String, agent: String) {
        post(url("agents", project, scope, agent, "interrupt"), "{}")
    }

    /** Press `keys`, by tmux's names for them, in the agent's pane. */
    suspend fun pressKeys(project: String, scope: String, agent: String, keys: List<String>) {
        post(
            url("agents", project, scope, agent, "keys"),
            json.encodeToString(KeysBody.serializer(), KeysBody(keys)),
        )
    }

    /**
     * Type `text` into the agent's pane as keys, pressing nothing after it: for a dialog that takes
     * text. A server that predates it throws [PmError.Unsupported].
     */
    suspend fun typeText(project: String, scope: String, agent: String, text: String) {
        post(
            url("agents", project, scope, agent, "type"),
            json.encodeToString(TextBody.serializer(), TextBody(text)),
        )
    }

    /** The dialog on the agent's screen that can be answered from here; null when there is none. */
    suspend fun dialog(project: String, scope: String, agent: String): Dialog? =
        try {
            json.decodeFromString(
                Dialog.serializer(),
                get(url("agents", project, scope, agent, "dialog")),
            )
        } catch (e: PmError.Status) {
            if (e.code == 404) null else throw e
        } catch (e: PmError.Unsupported) {
            null
        }

    /**
     * Every dialog of the agent's that can be answered from here, oldest first. A server that
     * predates the list serves only the oldest.
     */
    suspend fun dialogs(project: String, scope: String, agent: String): List<Dialog> =
        try {
            json
                .decodeFromString(
                    DialogList.serializer(),
                    get(url("agents", project, scope, agent, "dialogs")),
                )
                .dialogs
        } catch (e: PmError.Unsupported) {
            listOfNotNull(dialog(project, scope, agent))
        }

    /**
     * Answer the agent's dialog; returns once its harness has the answer. A dialog already answered
     * at the terminal is refused with `answered`, one whose hook has ended with `gone`.
     */
    suspend fun answerDialog(project: String, scope: String, agent: String, answer: DialogAnswer) {
        post(
            url("agents", project, scope, agent, "dialog"),
            json.encodeToString(DialogAnswer.serializer(), answer),
        )
    }

    /**
     * Merge the feature into its base and delete it, as `pm feat merge` does. Refused (`unsafe`,
     * `git`) with the CLI's own words. Returns what the CLI would warn of.
     */
    suspend fun merge(project: String, feature: String): List<String> =
        warnings(send(postRequest(url("features", project, feature, "merge"), "{}"), lifecycle))

    /**
     * Delete the feature, as `pm feat delete` does; refused (`unsafe`) when work would be lost.
     * Returns what the CLI would warn of, such as the untracked files deleted with it.
     */
    suspend fun delete(project: String, feature: String): List<String> =
        warnings(send(postRequest(url("features", project, feature, "delete"), "{}"), lifecycle))

    /**
     * Open the project, as `pm open` does: recreate its missing sessions and respawn their agents.
     * Returns what it skipped and the agents that didn't come up.
     */
    suspend fun openProject(project: String): List<String> =
        warnings(send(postRequest(url("projects", project, "open"), "{}"), lifecycle))

    /** Close the project, as `pm close` does: end its sessions, keeping all its state. */
    suspend fun closeProject(project: String) {
        send(postRequest(url("projects", project, "close"), "{}"), lifecycle)
    }

    /**
     * Delete the project, as `pm delete` without `--force` does; refused (`unsafe`) while a feature
     * holds work that would be lost. Returns what the CLI would warn of.
     */
    suspend fun deleteProject(project: String): List<String> =
        warnings(send(postRequest(url("projects", project, "delete"), "{}"), lifecycle))

    private fun warnings(reply: String): List<String> =
        json.decodeFromString(Ended.serializer(), reply).warnings

    /**
     * Restart the agent, resuming its session. Refused with `mid-turn` while it is busy, asking or
     * waiting on background work, unless `force`.
     */
    suspend fun restart(project: String, scope: String, agent: String, force: Boolean) {
        send(
            postRequest(
                url("agents", project, scope, agent, "restart"),
                json.encodeToString(RestartBody.serializer(), RestartBody(force)),
            ),
            lifecycle,
        )
    }

    /** The server's VAPID public key, which push subscriptions are made against. */
    suspend fun vapidKey(): String =
        json
            .decodeFromString(JsonObject.serializer(), get(url("push")))["vapid"]!!
            .jsonPrimitive
            .content

    suspend fun registerPush(endpoint: String, p256dh: String, auth: String) {
        val body =
            json.encodeToString(
                Subscription.serializer(),
                Subscription(endpoint, Keys(p256dh, auth)),
            )
        send(Request.Builder().url(url("push")).put(body.toRequestBody(JSON)).build())
    }

    suspend fun unregisterPush() {
        send(Request.Builder().url(url("push")).delete().build())
    }

    /** Unpair this device on the server: its token and push subscription are dropped. */
    suspend fun unpair() {
        send(Request.Builder().url(url("pairing")).delete().build())
    }

    /**
     * The event stream, until it fails or the collector stops; it fails with [PmError.Unauthorized]
     * once the device is unpaired. With `watch` (`project/scope/agent`), it also carries that
     * agent's `transcript` events from `after` on.
     */
    fun events(watch: String? = null, after: String? = null): Flow<ServerEvent> = callbackFlow {
        val request =
            Request.Builder()
                .url(url("events", query = mapOf("watch" to watch, "after" to after)))
                .header("Accept", "text/event-stream")
                .build()
        val source =
            EventSources.createFactory(streaming)
                .newEventSource(
                    request,
                    object : EventSourceListener() {
                        override fun onEvent(
                            eventSource: EventSource,
                            id: String?,
                            type: String?,
                            data: String,
                        ) {
                            if (type == "revoked") {
                                close(PmError.Unauthorized())
                                return
                            }
                            // Blocks OkHttp's reader thread, never drops: a lost
                            // transcript event would be lost for good.
                            trySendBlocking(ServerEvent(type ?: "message", data))
                        }

                        override fun onClosed(eventSource: EventSource) {
                            close(PmError.Unreachable(IOException("the server closed the stream")))
                        }

                        override fun onFailure(
                            eventSource: EventSource,
                            t: Throwable?,
                            response: Response?,
                        ) {
                            close(
                                response?.let(::failure)
                                    ?: PmError.Unreachable(t as? IOException ?: IOException(t))
                            )
                        }
                    },
                )
        awaitClose { source.cancel() }
    }

    private suspend fun get(url: HttpUrl): String = send(Request.Builder().url(url).build())

    private suspend fun post(url: HttpUrl, body: String): String = send(postRequest(url, body))

    private fun postRequest(url: HttpUrl, body: String): Request =
        Request.Builder().url(url).post(body.toRequestBody(JSON)).build()

    private suspend fun send(request: Request, client: OkHttpClient = http): String =
        call(request, client) { it.body.string() }

    /**
     * `read` from the response to `request`, made with `client`, once it succeeds; else the error
     * `refuse` makes of it, or the general one.
     */
    private suspend fun <T> call(
        request: Request,
        client: OkHttpClient = http,
        refuse: (Response) -> PmError? = { null },
        read: (Response) -> T,
    ): T =
        withContext(Dispatchers.IO) {
            val response =
                try {
                    client.newCall(request).await()
                } catch (e: IOException) {
                    throw PmError.Unreachable(e)
                }
            response.use { if (it.isSuccessful) read(it) else throw refuse(it) ?: failure(it) }
        }

    private fun failure(response: Response): PmError {
        val body = runCatching {
            json.decodeFromString(JsonObject.serializer(), response.peekBody(4096).string())
        }
            .getOrNull()
        fun field(key: String) =
            body?.get(key)?.let { runCatching { it.jsonPrimitive.content }.getOrNull() }
        val message = field("error")
        val refused = field("refused")
        return when {
            response.code == 401 -> PmError.Unauthorized()
            response.code == 409 && refused != null ->
                PmError.Refused(refused, message ?: "the agent can't take input now")
            response.code == 404 && message == NO_SUCH_ENDPOINT -> PmError.Unsupported()
            response.code == 405 && message == NO_SUCH_METHOD -> PmError.Unsupported()
            response.code == 410 -> PmError.Unsupported(retired = true)
            response.code == 404 && message == NO_CONVERSATION -> PmError.NoConversation()
            else -> PmError.Status(response.code, message ?: "HTTP ${response.code}")
        }
    }

    @Serializable private data class TextBody(val text: String)

    @Serializable private data class DialogList(val dialogs: List<Dialog>)

    @Serializable private data class KeysBody(val keys: List<String>)

    @Serializable private data class RestartBody(val force: Boolean)

    @Serializable private data class Keys(val p256dh: String, val auth: String)

    @Serializable private data class Subscription(val endpoint: String, val keys: Keys)

    @Serializable private data class Saved(val version: String)

    @Serializable private data class DocList(val docs: List<DocCategory>)

    @Serializable private data class Ended(val warnings: List<String> = emptyList())

    @Serializable
    private data class Changed(
        val error: String? = null,
        val refused: String? = null,
        val text: String = "",
        val version: String = "",
    )

    private companion object {
        val JSON = "application/json".toMediaType()
        val MARKDOWN = "text/markdown; charset=utf-8".toMediaType()
        const val NO_SUCH_ENDPOINT = "no such endpoint"
        const val NO_SUCH_METHOD = "no such endpoint for this method"
        const val NO_CONVERSATION = "the agent has no conversation yet"
        val json = Json { ignoreUnknownKeys = true }
    }
}

private suspend fun Call.await(): Response = suspendCancellableCoroutine { cont ->
    enqueue(
        object : Callback {
            override fun onResponse(call: Call, response: Response) = cont.resume(response)

            override fun onFailure(call: Call, e: IOException) = cont.resumeWithException(e)
        }
    )
    cont.invokeOnCancellation { cancel() }
}
