package io.github.titouanfreville.moonlight.client

import java.io.InputStream
import java.io.InputStreamReader
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.charset.StandardCharsets
import java.time.Duration

/**
 * The engine event stream — `GET /control/events`, served as SSE by
 * `crates/mcp-server/src/control_api.rs`. A port of the VS Code client's `events.ts`.
 *
 * Polling answers "what is true now" on a five-second cadence, which is fine for a
 * session list and useless for a hold: a session that proposes a plan is *stopped* until
 * someone answers.
 *
 * The stream is a change notification, not the record. A client that misses events — a
 * lag, a reconnect — re-reads the authoritative endpoints instead of assuming its folded
 * view is still complete. That is why [EventStreamHandlers.onDesync] is reported as
 * loudly as events are.
 */

/**
 * The `EngineEvent` variants this client consumes, from serde's externally-tagged shape
 * (`{"PlanProposed": {...}}`).
 *
 * Deliberately partial: the engine carries more variants, and a client that enumerated
 * all of them would need editing every time the engine grew one.
 */
sealed interface EngineEvent {
    val session: String

    data class PlanProposed(override val session: String, val plan: String) : EngineEvent
    data class ApprovalRequested(override val session: String, val what: String, val authorizeTool: String?) : EngineEvent
    data class SessionStateChanged(override val session: String, val status: String) : EngineEvent
    data class PhaseTransitioned(override val session: String, val phase: String) : EngineEvent
    data class SessionRemoved(override val session: String) : EngineEvent

    /**
     * The session's attention overlay changed. `alert` is the `AttentionKind` name —
     * `Incomplete` when detection saw a turn stall mid-work — or `null` when it cleared.
     */
    data class SessionAlert(override val session: String, val alert: String?) : EngineEvent
}

/**
 * Whether a status means the session is waiting on us rather than working.
 *
 * A hold is disarmed when a session reports any other status, because the hook it was
 * holding has been released by someone — possibly another client. `Running` is exactly
 * what the server publishes when a verdict resolves a hold.
 */
fun statusIsWaiting(status: String): Boolean = status == "WaitingInput"

/**
 * Turn one decoded SSE `data:` payload into an event, or `null` when it is a variant we
 * do not model. Tolerant on purpose: an engine that adds a variant must not break the
 * stream for the ones we handle.
 */
fun parseEngineEvent(json: String): EngineEvent? {
    val parsed = try {
        Json.parse(json)
    } catch (_: JsonException) {
        return null
    } as? Map<*, *> ?: return null
    if (parsed.size != 1) return null
    val (variant, rawBody) = parsed.entries.first()
    val body = rawBody as? Map<*, *> ?: emptyMap<String, Any?>()
    val session = body["session"] as? String
    if (session.isNullOrEmpty()) return null

    return when (variant) {
        "PlanProposed" -> (body["plan"] as? String)?.let { EngineEvent.PlanProposed(session, it) }
        "ApprovalRequested" -> EngineEvent.ApprovalRequested(
            session,
            body["what"] as? String ?: "",
            body["authorize_tool"] as? String,
        )
        "SessionStateChanged" -> EngineEvent.SessionStateChanged(session, body["status"] as? String ?: "")
        "PhaseTransitioned" -> EngineEvent.PhaseTransitioned(session, body["phase"] as? String ?: "")
        "SessionRemoved" -> EngineEvent.SessionRemoved(session)
        "SessionAlert" -> EngineEvent.SessionAlert(session, body["alert"] as? String)
        else -> null
    }
}

/** One parsed SSE frame: its `event:` name (default `message`) and its data. */
data class SseFrame(val event: String, val data: String)

/**
 * Split an SSE stream into frames, keeping whatever trailing partial frame the socket has
 * not finished sending.
 *
 * A pure function over an accumulated buffer so the framing can be tested without a
 * server: the interesting cases (a frame split mid-line, a multi-line `data:`) are
 * exactly the ones a live test would only hit by luck.
 */
fun readFrames(buffer: String): Pair<List<SseFrame>, String> {
    val frames = mutableListOf<SseFrame>()
    val blocks = buffer.split("\n\n").toMutableList()
    // The last block has no terminator yet — it may be a whole frame whose blank line is
    // still in flight, so it stays in the buffer.
    val rest = blocks.removeAt(blocks.size - 1)

    for (block in blocks) {
        var event = "message"
        val data = mutableListOf<String>()
        for (rawLine in block.split("\n")) {
            val line = rawLine.removeSuffix("\r")
            if (line.isEmpty() || line.startsWith(":")) continue // keep-alive comment
            if (line.startsWith("event:")) {
                event = line.removePrefix("event:").trim()
            } else if (line.startsWith("data:")) {
                data += line.removePrefix("data:").trimStart()
            }
        }
        if (data.isNotEmpty()) frames += SseFrame(event, data.joinToString("\n"))
    }
    return frames to rest
}

interface EventStreamHandlers {
    fun onEvent(event: EngineEvent)

    /**
     * The folded view can no longer be trusted — the stream lagged, dropped, or the daemon
     * went away. The caller re-reads the authoritative endpoints.
     *
     * Reported for a reconnect as well as a lag: a client that reconnected silently would
     * show a plan gate that was answered while it was disconnected.
     */
    fun onDesync(reason: String)
}

/**
 * Follows the engine event stream, reconnecting for as long as it lives.
 *
 * Reconnection is the normal case, not an error path: the daemon is autostarted and
 * restarted under running IDEs, so a stream that gave up on the first failure would
 * leave the IDE permanently blind after an ordinary daemon upgrade.
 *
 * Runs on one daemon thread of its own. The read blocks for as long as the connection
 * is open — that is what a stream is — so it cannot borrow a pooled worker without
 * pinning it for the life of the IDE.
 */
class EventStream(
    private val handlers: EventStreamHandlers,
    private val baseUrl: () -> String? = { controlBaseUrl() },
) : AutoCloseable {
    @Volatile
    private var disposed = false

    @Volatile
    private var current: InputStream? = null

    private val http: HttpClient = HttpClient.newBuilder()
        .proxy(HttpClient.Builder.NO_PROXY)
        .version(HttpClient.Version.HTTP_1_1)
        .connectTimeout(Duration.ofMillis(ControlApi.REQUEST_TIMEOUT_MS))
        .build()

    private val thread = Thread(::run, "MoonlightCode event stream").apply { isDaemon = true }

    fun start(): EventStream {
        thread.start()
        return this
    }

    private fun run() {
        var retryMs = BASE_RETRY_MS
        while (!disposed) {
            val reason = try {
                connectOnce { retryMs = BASE_RETRY_MS }
            } catch (e: InterruptedException) {
                break
            } catch (e: Exception) {
                e.message ?: e.javaClass.simpleName
            }
            if (disposed) break
            handlers.onDesync(reason)
            try {
                Thread.sleep(retryMs)
            } catch (_: InterruptedException) {
                break
            }
            retryMs = minOf(retryMs * 2, MAX_RETRY_MS)
        }
    }

    /** One connection, read to its end. Returns why it ended. */
    private fun connectOnce(onConnected: () -> Unit): String {
        val base = baseUrl() ?: return "no control API to stream from"
        val request = HttpRequest.newBuilder(URI.create("$base/control/events"))
            .header("Accept", "text/event-stream")
            .GET()
            .build()
        val response = http.send(request, HttpResponse.BodyHandlers.ofInputStream())
        val status = response.statusCode()
        if (status !in 200..299) {
            response.body().close()
            return "event stream → HTTP $status"
        }
        // Connected: the next outage should retry promptly rather than inherit the backoff
        // this one earned.
        onConnected()
        val body = response.body()
        current = body
        body.use { stream ->
            val reader = InputStreamReader(stream, StandardCharsets.UTF_8)
            val chunk = CharArray(8192)
            var buffer = ""
            while (!disposed) {
                val n = reader.read(chunk)
                if (n < 0) break
                buffer += String(chunk, 0, n)
                val (frames, rest) = readFrames(buffer)
                buffer = rest
                for (frame in frames) {
                    if (frame.event == "lagged") {
                        handlers.onDesync("event stream lagged (${frame.data} missed)")
                        continue
                    }
                    parseEngineEvent(frame.data)?.let(handlers::onEvent)
                }
            }
        }
        current = null
        return "event stream ended"
    }

    /** Stop listening. Safe to call twice. */
    override fun close() {
        disposed = true
        try {
            current?.close()
        } catch (_: Exception) {
            // Already closed, or closing mid-read — either way it is done.
        }
        thread.interrupt()
    }

    private companion object {
        const val BASE_RETRY_MS = 1_000L
        const val MAX_RETRY_MS = 30_000L
    }
}
