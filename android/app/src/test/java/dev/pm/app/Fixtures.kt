package dev.pm.app

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import mockwebserver3.MockResponse
import mockwebserver3.MockResponseBody
import mockwebserver3.SocketEffect
import okio.BufferedSink
import java.util.concurrent.CountDownLatch

/** README's example snapshot, with a field, kind, state and version detail this app doesn't know. */
val SNAPSHOT = """
    {
      "version": 1,
      "projects": [{
        "name": "app", "root": "/src/app", "skipped": null,
        "main": {
          "session": "app/main", "session_exists": true,
          "agents": [{"name": "main", "state": "hibernating", "unread": 0, "window": "app/main:1", "waiting": null}],
          "attention": {"kind": "summoning", "detail": "main: ?", "agent": "main"},
          "working": false, "last_activity": null, "colour": "teal"
        }
      }],
      "features": [{
        "project": "app", "name": "login",
        "attention": {"kind": "blocked", "detail": "which DB?", "agent": "implementer"},
        "progress": "blocked", "blocked_reason": "which DB?", "blocked_by": "implementer",
        "summary": null, "lifecycle": "wip", "pr": null,
        "session": "app/login", "session_exists": true,
        "agents": [{"name": "implementer", "state": "asking", "unread": 2, "window": "app/login:1",
                    "waiting": {"kind": "question", "detail": "Postgres or SQLite?"}}],
        "working": false, "last_activity": "2026-10-02T09:30:00Z"
      }, {
        "project": "app", "name": "search",
        "attention": {"kind": "ready", "detail": "Adds search", "agent": null},
        "progress": "ready", "lifecycle": "review", "session": "app/search", "session_exists": false,
        "agents": [], "working": true, "last_activity": null
      }]
    }
""".trimIndent()

/**
 * Server-sent events that stay open after `events` until [release], as
 * `pm serve`'s stream does between heartbeats.
 */
class OpenStream {
    private val held = CountDownLatch(1)

    fun response(vararg events: Pair<String, String>): MockResponse = MockResponse.Builder()
        .addHeader("Content-Type", "text/event-stream")
        .body(object : MockResponseBody {
            override val contentLength = -1L

            override fun writeTo(sink: BufferedSink) {
                events.forEach { (name, data) -> sink.writeUtf8("event: $name\ndata: ${data.replace("\n", "")}\n\n") }
                sink.flush()
                held.await()
            }
        })
        .onResponseEnd(SocketEffect.CloseSocket())
        .build()

    /** End the stream: the server closes it, as `pm serve` stopping would. */
    fun release() = held.countDown()
}

/**
 * Run what is due on the test scheduler, without advancing its clock,
 * until `condition` holds: real I/O resumes coroutines on it from other threads.
 */
@OptIn(ExperimentalCoroutinesApi::class)
fun TestScope.eventually(condition: () -> Boolean) {
    val deadline = System.nanoTime() + 5_000_000_000
    while (true) {
        runCurrent()
        if (condition()) return
        check(System.nanoTime() < deadline) { "timed out waiting" }
        Thread.sleep(5)
    }
}
