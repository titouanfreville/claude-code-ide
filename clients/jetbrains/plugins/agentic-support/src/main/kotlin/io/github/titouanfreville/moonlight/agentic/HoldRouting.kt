package io.github.titouanfreville.moonlight.agentic

import io.github.titouanfreville.moonlight.client.PLAN_HOLD
import io.github.titouanfreville.moonlight.core.HeldApproval

/**
 * Which surface answers a hold — a port of `hold-routing.ts`.
 *
 * The gate's own marker, not "does this hold carry plan text": the held-set fold caches a
 * session's plan, and a `plan != null` test once sent a `Bash` approval to the plan review
 * instead of asking about the command.
 */
fun isPlanHold(hold: HeldApproval): Boolean = isPlanHold(hold.what)

fun isPlanHold(what: String): Boolean = what == PLAN_HOLD

/**
 * Where a hold is announced. Core names the one window that owns the session; this window
 * acts if it is that one. When no window owns it, the focused window announces it — never
 * every window, the noise VS Code could not avoid with one process per window, and never
 * none, because a held agent nobody is told about stays held.
 */
fun <P> announcesHere(here: P, owner: P?, focused: P?, firstOpen: P?): Boolean =
    when {
        owner != null -> owner == here
        focused != null -> focused == here
        else -> firstOpen == here
    }
