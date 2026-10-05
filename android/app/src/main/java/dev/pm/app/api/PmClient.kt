package dev.pm.app.api

import dev.pm.app.model.Dialog
import dev.pm.app.model.DialogAnswer
import dev.pm.app.model.FeatureInfo
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
    /** The server can't be reached: the phone is off the tailnet, or the Mac is. */
    class Unreachable(cause: IOException) : PmError(cause.message ?: "unreachable")

    /** The token was refused: the device was revoked. */
    class Unauthorized : PmError("this device's token was refused")

    /** The server has no such endpoint: it predates what was asked of it. */
    class Unsupported : PmError("the server doesn't serve this yet")

    /** The agent's session hasn't started, or its transcript is gone. */
    class NoConversation : PmError("the agent has no conversation yet")

    /**
     * The agent can't take this now; `code` says why (`asking`, `not-at-prompt`, `answered`, …).
     */
    class Refused(val code: String, message: String) : PmError(message)

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

    /** Longer than the server's 25 s heartbeat, so a silent stream is a dead one. */
    private val streaming = http.newBuilder().readTimeout(60, TimeUnit.SECONDS).build()

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

    suspend fun summary(project: String, feature: String): String =
        get(url("features", project, feature, "summary"))

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
     * Answer the agent's dialog; returns once its harness has the answer. A dialog already answered
     * at the terminal is refused with `answered`, one whose hook has ended with `gone`.
     */
    suspend fun answerDialog(project: String, scope: String, agent: String, answer: DialogAnswer) {
        post(
            url("agents", project, scope, agent, "dialog"),
            json.encodeToString(DialogAnswer.serializer(), answer),
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

    /**
     * The event stream, until it fails or the collector stops. With `watch`
     * (`project/scope/agent`), it also carries that agent's `transcript` events from `after` on.
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

    private suspend fun post(url: HttpUrl, body: String): String =
        send(Request.Builder().url(url).post(body.toRequestBody(JSON)).build())

    private suspend fun send(request: Request): String =
        withContext(Dispatchers.IO) {
            val response =
                try {
                    http.newCall(request).await()
                } catch (e: IOException) {
                    throw PmError.Unreachable(e)
                }
            response.use { if (it.isSuccessful) it.body.string() else throw failure(it) }
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
            response.code == 404 && message == NO_CONVERSATION -> PmError.NoConversation()
            else -> PmError.Status(response.code, message ?: "HTTP ${response.code}")
        }
    }

    @Serializable private data class TextBody(val text: String)

    @Serializable private data class KeysBody(val keys: List<String>)

    @Serializable private data class Keys(val p256dh: String, val auth: String)

    @Serializable private data class Subscription(val endpoint: String, val keys: Keys)

    private companion object {
        val JSON = "application/json".toMediaType()
        const val NO_SUCH_ENDPOINT = "no such endpoint"
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
