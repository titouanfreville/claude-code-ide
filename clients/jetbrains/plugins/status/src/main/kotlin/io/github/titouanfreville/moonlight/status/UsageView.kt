package io.github.titouanfreville.moonlight.status

import io.github.titouanfreville.moonlight.client.AccountUsage
import io.github.titouanfreville.moonlight.client.SessionUsage
import io.github.titouanfreville.moonlight.client.UsageResponse
import kotlin.math.roundToLong

/**
 * The Claude usage readout — how much allowance is spent, how long until it refills, and
 * how full the current session's context is. A port of `moonlight-status/src/usage.ts`.
 *
 * The one rule this follows everywhere: an unknown figure renders as `—`, never as 0. A
 * meter that reads zero when it simply could not fetch is worse than no meter, because
 * zero is read as headroom.
 */

/** Above this, the window is close enough to exhausted to take the warning background. */
const val PRESSURE_PCT = 90.0

/**
 * A snapshot older than this no longer describes what the session is doing: Claude Code
 * rewrites it on every render, so it only goes quiet when the session does.
 */
const val STALE_MS = 5 * 60 * 1000L

data class UsageView(val text: String, val warn: Boolean, val tooltip: List<String>)

/** `42%`, `42.5%`, or `—` when the figure is genuinely unknown. */
fun pct(value: Double?): String = when {
    value == null -> "—"
    value == Math.rint(value) -> "${value.toLong()}%"
    else -> "$value%"
}

/** Whether any shown window is close enough to its limit to warrant the warning color. */
fun underPressure(quota: AccountUsage?, session: SessionUsage?): Boolean =
    listOfNotNull(quota?.fiveHourPct, quota?.weeklyPct, session?.ctxPct).any { it >= PRESSURE_PCT }

/**
 * What the usage widget shows, or `null` to hide it — with no backend the gating widget is
 * already saying so loudly, and two alarms for one fault is noise.
 */
fun usageView(backendError: String?, usage: UsageResponse?, sessionId: String?, now: Long): UsageView? {
    if (backendError != null) return null
    // Before the first reading lands: say we don't know yet rather than flash a figure.
    usage ?: return UsageView("usage —", false, listOf("Claude usage: waiting for the first reading from MoonlightCode."))
    val session = sessionId?.let { id -> usage.sessions.firstOrNull { it.sessionId == id } }
    return UsageView(
        renderText(usage.quota, session),
        underPressure(usage.quota, session),
        renderTooltip(usage.quota, session, sessionId != null, now),
    )
}

/**
 * Deliberately two segments: the account windows are true whatever you are looking at,
 * while context and uptime only mean anything once we know which session you are in.
 */
private fun renderText(quota: AccountUsage?, session: SessionUsage?): String {
    // The countdown rides with the 5-hour figure: "52% spent" and "refills in 40m" mean
    // opposite things about whether to start something big.
    val resets = quota?.fiveHourResetsIn?.let { " ↻ $it" } ?: ""
    val parts = mutableListOf("5h ${pct(quota?.fiveHourPct)}$resets", "wk ${pct(quota?.weeklyPct)}")
    if (session != null) {
        parts += "ctx ${pct(session.ctxPct)}"
        parts += session.sessionUptime
    }
    return parts.joinToString(" · ")
}

private fun renderTooltip(quota: AccountUsage?, session: SessionUsage?, sessionKnown: Boolean, now: Long): List<String> {
    val lines = mutableListOf("Claude usage")
    if (quota != null) {
        val resets = quota.fiveHourResetsIn?.let { " — resets in $it" } ?: ""
        lines += "5-hour window: ${pct(quota.fiveHourPct)}$resets"
        lines += "Weekly window: ${pct(quota.weeklyPct)}"
        lines += "Weekly Sonnet: ${pct(quota.sonnetPct)}"
    } else {
        lines += "Account quota unavailable — MoonlightCode could not read your Claude"
        lines += "credentials or reach the usage endpoint. The figures are unknown, not zero."
    }
    lines += ""
    when {
        session != null -> {
            val window = if (session.ctxLimit > 0) {
                "${(session.ctxTokens / 1000.0).roundToLong()}k of ${(session.ctxLimit / 1000.0).roundToLong()}k tokens"
            } else {
                "window size unknown"
            }
            lines += "Session: ${session.title ?: session.sessionId}"
            lines += "Model: ${session.model.ifEmpty { "—" }}${if (session.persona.isNotEmpty()) " · ${session.persona}" else ""}"
            lines += "Context: ${pct(session.ctxPct)} ($window)"
            lines += "Uptime: ${session.sessionUptime}"
            // A snapshot only refreshes while Claude Code renders, so an old one means the
            // session went quiet — and its context figure is a memory, not a reading.
            val age = now - session.updatedMs
            if (age > STALE_MS) {
                lines += ""
                lines += "Last observed ${(age / 60000.0).roundToLong()}m ago — this session has gone quiet,"
                lines += "so its context and uptime are the last known values, not live ones."
            }
        }
        // Known session, but no snapshot — normal for one MoonlightCode did not launch.
        sessionKnown -> {
            lines += "No context or uptime for this session: Claude Code reports those through its"
            lines += "status line, which MoonlightCode only registers for sessions it starts."
        }
        else -> lines += "Context and uptime need a known session — pin one to see them."
    }
    lines += ""
    lines += "Click to refresh now."
    return lines
}
