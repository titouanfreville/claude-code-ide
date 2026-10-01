package io.github.titouanfreville.moonlight.client

/** The leading characters of a session id — enough to tell two sessions apart by eye. */
fun shortId(sessionId: String): String = sessionId.take(8)

/**
 * How to name a session in a list, given the others it sits beside.
 *
 * Titles are not unique — two sessions started the same way are both "Test session" —
 * and every picker here asks a question ("which one are you in?", "which one do you
 * want to govern?") whose whole value depends on the answer being distinguishable.
 *
 * The id is appended only when the title actually collides: a suffix on every row is
 * noise that trains you to stop reading the row.
 */
fun sessionLabel(session: DiscoverableSession, among: List<DiscoverableSession>): String =
    sessionLabel(session.sessionId, session.title, among.map { it.sessionId to it.title })

/** The same rule over bare (id, title) pairs, for callers that have no full session. */
fun sessionLabel(sessionId: String, title: String?, among: List<Pair<String, String?>>): String {
    val shown = title ?: shortId(sessionId)
    val collides = among.any { (otherId, otherTitle) -> otherId != sessionId && (otherTitle ?: "") == (title ?: "") }
    return if (collides) "$shown (${shortId(sessionId)})" else shown
}

/**
 * Single-quote a value for a POSIX shell.
 *
 * `'` cannot be escaped inside single quotes, so the quoting is closed, an escaped quote
 * is emitted, and quoting reopens — the standard `'\''` dance.
 */
fun shellQuote(value: String): String = "'" + value.replace("'", "'\\''") + "'"

/**
 * A loopback MCP endpoint we are willing to put on a command line, or `null`.
 *
 * The URL is an HTTP response body from whatever is listening on the port in
 * `~/.moonlight/control.json` — a world-readable file in `$HOME` that the gated agent is
 * told to read. So it crosses a trust boundary and is validated like one: an allowlist of
 * scheme and host, not a scan for bad characters.
 *
 * Stricter than the VS Code client in one way: `java.net.URI` refuses characters such as
 * spaces that a WHATWG `URL` would percent-encode and accept. A daemon never hands out
 * such a URL, so refusing it costs nothing.
 */
fun safeEndpointUrl(url: String): String? {
    val parsed = try {
        java.net.URI(url)
    } catch (_: java.net.URISyntaxException) {
        return null
    }
    val scheme = parsed.scheme?.lowercase()
    val host = parsed.host
    val loopback = host == "127.0.0.1" || host == "localhost" || host == "[::1]"
    return if ((scheme == "http" || scheme == "https") && loopback) parsed.toString() else null
}

/** A session id we are willing to put on a command line — the shape Claude Code mints. */
fun isSafeSessionId(id: String): Boolean = Regex("^[0-9a-fA-F-]{8,64}$").matches(id)

/** The environment variable that ties a launched process back to its launch — see the daemon's `launches`. */
const val LAUNCH_ENV: String = "MOONLIGHT_LAUNCH_ID"

/**
 * The line typed into a terminal to start a governed session.
 *
 * `env MOONLIGHT_LAUNCH_ID=<id>` rather than a shell-specific `VAR=value` prefix: `env` reads
 * the same in bash, zsh and fish. The variable is what keeps the session's MCP verbs working
 * after `/resume` or `/clear` change its conversation: Claude Code hands it to every hook it
 * runs, and the daemon follows the launch to wherever the hooks say it is now.
 *
 * Returns `null` for an id that is not the shape we mint — it is interpolated into a shell
 * line, so it is checked rather than trusted. `mcpFlag` comes from [mcpConfigFlag], already
 * validated and quoted (or empty, to launch without the verbs).
 */
fun launchCommand(sessionId: String, mcpFlag: String): String? {
    if (!isSafeSessionId(sessionId)) return null
    return "env $LAUNCH_ENV=$sessionId claude --session-id $sessionId$mcpFlag"
}

/**
 * The line that brings a launch back on the conversation it was in — after the IDE
 * restarted, or the process died.
 *
 * Keeps the **same launch id**, so the daemon's launch registry, the session's MCP endpoint
 * and its phase all carry over. `conversationId` is `null` for a launch the daemon never saw
 * write a transcript (a "ghost"): `claude --resume` aborts on a conversation it cannot find,
 * so that one starts fresh, pinned to the launch id, as it was first launched.
 *
 * `null` when either id is not the shape we mint — both are interpolated into a shell line.
 */
fun resumeCommand(launchId: String, conversationId: String?, mcpFlag: String): String? {
    if (conversationId == null) return launchCommand(launchId, mcpFlag)
    if (!isSafeSessionId(launchId) || !isSafeSessionId(conversationId)) return null
    return "env $LAUNCH_ENV=$launchId claude --resume $conversationId$mcpFlag"
}

/**
 * The `--mcp-config` fragment (leading space included) that wires `url` in as the
 * session's `moonlight` server, or `null` when the URL is not one we will run.
 *
 * Validated first, then quoted properly: JSON escapes `"` and control characters and
 * never `'`, so a URL containing one would otherwise close the quoting and run the rest
 * as shell — and this string is typed into the operator's terminal followed by Enter.
 */
fun mcpConfigFlag(url: String): String? {
    val safe = safeEndpointUrl(url) ?: return null
    val json = Json.write(mapOf("mcpServers" to mapOf("moonlight" to mapOf("type" to "http", "url" to safe))))
    return " --mcp-config ${shellQuote(json)}"
}
