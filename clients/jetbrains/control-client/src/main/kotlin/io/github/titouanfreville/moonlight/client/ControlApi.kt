package io.github.titouanfreville.moonlight.client

import java.net.URI
import java.net.URLEncoder
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.charset.StandardCharsets
import java.time.Duration

/**
 * Thin client over moonlightd's control API — see `crates/mcp-server/src/control_api.rs`
 * for the server side. A port of the VS Code client's `index.ts`, endpoint for endpoint.
 *
 * Every call is **blocking**. Callers run it off the UI thread (the IDE's pooled
 * executor); the request timeout bounds how long any one call can hold that thread.
 */
class ControlApi(
    /** Where the daemon is, read per call — see [controlBaseUrl]. */
    private val baseUrl: () -> String? = { controlBaseUrl() },
) {
    /**
     * Loopback only, and never through a proxy.
     *
     * The IDE installs its own default `ProxySelector` so that plugins honour the proxy
     * configured in Settings — which is right for the internet and wrong here. A proxy
     * rule without a `localhost` exclusion would route `127.0.0.1` through a corporate
     * proxy that cannot reach it, and the daemon would look down while running fine.
     *
     * HTTP/1.1 because the server speaks it, and Java's default of HTTP/2 sends an
     * upgrade offer on every plain-HTTP request for nothing.
     */
    private val http: HttpClient = HttpClient.newBuilder()
        .proxy(HttpClient.Builder.NO_PROXY)
        .version(HttpClient.Version.HTTP_1_1)
        .connectTimeout(Duration.ofMillis(REQUEST_TIMEOUT_MS))
        .build()

    private fun request(method: String, path: String, body: Any? = NO_BODY, timeoutMs: Long = REQUEST_TIMEOUT_MS): Any? {
        val base = baseUrl() ?: throw ControlApiException(
            "MoonlightCode control API not found (~/.moonlight/control.json missing) — is moonlightd or the desktop app running?"
        )
        val builder = HttpRequest.newBuilder(URI.create("$base$path"))
            // A daemon that accepts the socket and then stops answering would otherwise
            // hold the calling thread forever, which wedges the poll.
            .timeout(Duration.ofMillis(timeoutMs))
        if (body === NO_BODY) {
            builder.method(method, HttpRequest.BodyPublishers.noBody())
        } else {
            builder.header("Content-Type", "application/json")
            builder.method(method, HttpRequest.BodyPublishers.ofString(Json.write(body), StandardCharsets.UTF_8))
        }
        val response = try {
            http.send(builder.build(), HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8))
        } catch (e: java.net.http.HttpTimeoutException) {
            throw ControlApiException("$method $path → timed out after ${timeoutMs}ms")
        } catch (e: java.io.IOException) {
            throw ControlApiException("$method $path → ${e.message ?: e.javaClass.simpleName}")
        } catch (e: InterruptedException) {
            Thread.currentThread().interrupt()
            throw ControlApiException("$method $path → interrupted")
        }
        val status = response.statusCode()
        if (status !in 200..299) {
            throw ControlApiException("$method $path → HTTP $status", status)
        }
        val text = response.body()
        if (text.isNullOrEmpty()) return null
        return try {
            Json.parse(text)
        } catch (e: JsonException) {
            throw ControlApiException("$method $path → unreadable response: ${e.message}")
        }
    }

    /** [request], then decode — a decode failure is reported like an unparseable body. */
    private fun <T> call(method: String, path: String, body: Any? = NO_BODY, decode: (Any?) -> T): T =
        decoded(method, path, request(method, path, body), decode)

    private fun <T> decoded(method: String, path: String, raw: Any?, decode: (Any?) -> T): T {
        return try {
            decode(raw)
        } catch (e: DecodeException) {
            throw ControlApiException("$method $path → unreadable response: ${e.message}")
        }
    }

    private fun <T> list(raw: Any?, decode: (Any?) -> T): List<T> =
        (raw as? List<*> ?: throw DecodeException("expected a JSON array")).map(decode)

    fun gatingStatus(): List<HookStatusEntry> = call("GET", "/control/gating-status") { list(it, HookStatusEntry::decode) }

    /**
     * Account quota + per-session context/uptime. The server caches the quota (it is a
     * network round-trip to Anthropic), so polling this on a UI cadence is cheap.
     */
    fun usage(): UsageResponse = call("GET", "/control/usage", decode = UsageResponse::decode)

    /**
     * Unreviewed files, narrowed to one `session` and/or `path` when given. `diffs = false`
     * leaves each item's diff empty — the queue renders one per file, and across a fleet that
     * wrote build output it runs to tens of megabytes, so lists ask without and a preview asks
     * for the one file it shows.
     *
     * A daemon too old to know the parameters ignores them and answers with everything, so the
     * result is filtered here too, and the call is allowed [QUEUE_TIMEOUT_MS].
     */
    fun reviewQueue(session: String? = null, path: String? = null, diffs: Boolean = true): List<ReviewQueueItem> {
        val query = buildList {
            session?.let { add("session=${q(it)}") }
            path?.let { add("path=${q(it)}") }
            if (!diffs) add("diffs=false")
        }.joinToString("&")
        // Not `path`: that is the filter parameter, and shadowing it emptied every result.
        val url = "/control/review-queue" + if (query.isEmpty()) "" else "?$query"
        val all = decoded("GET", url, request("GET", url, timeoutMs = QUEUE_TIMEOUT_MS)) { list(it, ReviewQueueItem::decode) }
        return all.filter { (session == null || it.sessionId == session) && (path == null || it.filePath == path) }
    }

    fun accept(sessionId: String, filePath: String) {
        request("POST", "/control/review-queue/accept", mapOf("session_id" to sessionId, "path" to filePath))
    }

    fun reject(sessionId: String, filePath: String, message: String): RejectResponse =
        call(
            "POST",
            "/control/review-queue/reject",
            mapOf("session_id" to sessionId, "path" to filePath, "message" to message),
            RejectResponse::decode,
        )

    /**
     * Sessions detection has found — adopted or not. An unadopted one lives only in the
     * running backend's in-memory fleet, so this is the only way to see "here's a Claude
     * Code session you could adopt."
     */
    fun discoverableSessions(): List<DiscoverableSession> =
        call("GET", "/control/discoverable-sessions") { list(it, DiscoverableSession::decode) }

    /** Opt a discovered session into MoonlightCode governance. */
    fun adopt(sessionId: String) {
        request("POST", "/control/adopt", mapOf("session_id" to sessionId))
    }

    /**
     * Move a session to an explicit phase (an operator override — the engine pins it).
     * The escape hatch from `Plan`, where project writes are denied.
     */
    fun setPhase(sessionId: String, phase: Phase) {
        request("POST", "/control/phase", mapOf("session_id" to sessionId, "phase" to phase.name))
    }

    /** Advance to the next phase and return the session to auto (clears any pin). */
    fun advancePhase(sessionId: String) {
        request("POST", "/control/phase/advance", mapOf("session_id" to sessionId))
    }

    /**
     * Answer a held approval — a plan proposal, or an action a frozen phase stopped.
     *
     * A denial's `reason` is the feedback the agent acts on, so a bare refusal teaches it
     * nothing: callers are expected to say why.
     *
     * This releases the *hook*. Releasing a plan hold is only half of approving a plan —
     * see [approvePlan].
     */
    fun verdict(sessionId: String, approve: Boolean, reason: String? = null) {
        request("POST", "/control/verdict", mapOf("session_id" to sessionId, "approve" to approve, "reason" to reason))
    }

    /**
     * Approve a plan: release the held hook, then move the session out of `Plan`.
     *
     * Both halves are required and neither implies the other — the server swallows an
     * `ApproveAction` that resolved a held hook, so it never reaches the engine, and a
     * phase advance alone leaves the agent still blocked on its hook. Getting it wrong
     * looks like an approval that did nothing.
     */
    fun approvePlan(sessionId: String) {
        verdict(sessionId, true)
        advancePhase(sessionId)
    }

    /**
     * Bind this session's `moonlight` MCP endpoint and get its URL.
     *
     * Called *before* launching, because the URL goes into the launch arguments. Without
     * it the agent has no `moonlight` verbs at all — it is gated by the hooks but cannot
     * propose a plan or request a phase. A backend built without the MCP transport answers
     * 501; the caller launches anyway.
     */
    fun mcpEndpoint(sessionId: String): String =
        call("POST", "/control/mcp-endpoint", mapOf("session_id" to sessionId)) { Fields(it).string("url") }

    /**
     * Holds outstanding right now.
     *
     * The event stream announces a hold once, when it starts. An IDE opened after that
     * point would otherwise never learn about a session sitting blocked — and because the
     * daemon holds indefinitely waiting for a client, nothing would ever resolve it. This
     * is the catch-up read that makes the stream safe to miss.
     */
    fun pendingApprovals(): List<PendingApproval> =
        call("GET", "/control/pending-approvals") { list(it, PendingApproval::decode) }

    /** The before side of a reviewed file — the left pane of the diff. */
    fun baseline(sessionId: String, filePath: String): BaselineView =
        call("GET", "/control/review/baseline?session=${q(sessionId)}&path=${q(filePath)}", decode = BaselineView::decode)

    fun comments(sessionId: String): List<ReviewComment> =
        call("GET", "/control/review/comments?session=${q(sessionId)}") { list(it, ReviewComment::decode) }

    /**
     * Persist a comment immediately. Reviews survive closing the IDE because they live in
     * the shared tracker, not in this plugin's memory.
     *
     * `parentId` answers an existing comment instead of starting a thread; the reply
     * inherits that comment's file, side and line range server-side, so the anchor fields
     * are ignored for it.
     */
    fun addComment(
        sessionId: String,
        scope: CommentScope,
        path: String,
        side: DiffSide,
        startLine: Int,
        endLine: Int,
        body: String,
        anchorText: String? = null,
        parentId: String? = null,
        author: CommentAuthor? = null,
    ): ReviewComment = call(
        "POST",
        "/control/review/comments",
        mapOf(
            "session_id" to sessionId,
            "scope" to scope.name,
            "path" to path,
            "side" to side.name,
            "start_line" to startLine,
            "end_line" to endLine,
            "body" to body,
            "anchor_text" to anchorText,
            "parent_id" to parentId,
            "author" to author?.name,
        ),
        ReviewComment::decode,
    )

    /** Edit a comment's body and/or resolve it. Editing re-queues it for the next batch. */
    fun updateComment(sessionId: String, id: String, body: String? = null, resolved: Boolean? = null): ReviewComment =
        call(
            "POST",
            "/control/review/comments/update",
            mapOf("session_id" to sessionId, "id" to id, "body" to body, "resolved" to resolved),
            ReviewComment::decode,
        )

    fun deleteComment(sessionId: String, id: String) {
        request("POST", "/control/review/comments/delete", mapOf("session_id" to sessionId, "id" to id))
    }

    /** Deliver every unsent, unresolved comment to the session as one message; `null` when there was nothing to send. */
    fun submitReview(sessionId: String): SubmitReviewResponse? =
        call("POST", "/control/review/submit", mapOf("session_id" to sessionId)) { raw ->
            raw?.let(SubmitReviewResponse::decode)
        }

    /**
     * Record a delivery **we** performed. Only call this after actually writing the review
     * into the session — the server cannot observe an IDE's terminal, so this is a claim
     * it has to take on trust.
     */
    fun markDelivered(sessionId: String, commentIds: List<String>) {
        request("POST", "/control/review/delivered", mapOf("session_id" to sessionId, "comment_ids" to commentIds))
    }

    fun ignoredPaths(sessionId: String): List<String> =
        call("GET", "/control/review/ignore?session=${q(sessionId)}") { raw ->
            list(raw) { it as? String ?: throw DecodeException("ignored path is not a string") }
        }

    fun setIgnored(sessionId: String, filePath: String, ignored: Boolean) {
        request("POST", "/control/review/ignore", mapOf("session_id" to sessionId, "path" to filePath, "ignored" to ignored))
    }

    private fun q(value: String): String = URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20")

    companion object {
        /**
         * How long a control-API call may take before it is abandoned. Comfortably above a
         * loopback round-trip and well below the 5s poll interval, so a wedged daemon costs
         * one tick rather than every tick after it.
         */
        const val REQUEST_TIMEOUT_MS: Long = 3000

        /** The review queue reads every reviewed file from disk; it gets far longer than a status read. */
        const val QUEUE_TIMEOUT_MS: Long = 60_000

        private val NO_BODY = Any()
    }
}

/** A control-API call that failed; `status` is the HTTP status when there was one. */
class ControlApiException(message: String, val status: Int? = null) : Exception(message)
