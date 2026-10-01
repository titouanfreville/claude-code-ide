package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.EngineEvent
import io.github.titouanfreville.moonlight.client.PLAN_HOLD
import io.github.titouanfreville.moonlight.client.PendingApproval
import io.github.titouanfreville.moonlight.client.statusIsWaiting

/**
 * What is held at the gate, folded from the engine event stream — a port of
 * `gate-state.ts`.
 *
 * The rules here decide whether an operator is shown a live verdict or a dead button, and
 * both mistakes are expensive: a dead button gets clicked and believed, and a missing one
 * leaves a session stopped.
 *
 * Synchronized throughout: the event stream folds on its own thread while the UI reads on
 * the EDT.
 */
class GateState {
    private val held = LinkedHashMap<String, HeldApproval>()
    private val plans = HashMap<String, String>()

    /**
     * Replace the held set with a server snapshot.
     *
     * Wholesale, not merged: a stream that dropped may have carried both the arrival and
     * the answering of a hold, and a leftover entry would offer a verdict on a hook nobody
     * is holding any more.
     *
     * Plans are kept rather than replaced — the snapshot only knows about holds still
     * open, while a plan stays worth reading after its verdict.
     */
    @Synchronized
    fun resync(rows: List<PendingApproval>) {
        held.clear()
        for (row in rows) {
            held[row.sessionId] = HeldApproval(row.sessionId, row.what, row.plan, row.mcpTool, row.sinceMs)
            row.plan?.let { plans[row.sessionId] = it }
        }
    }

    /** Fold one event in. Returns whether anything a surface renders actually changed. */
    @Synchronized
    fun apply(event: EngineEvent, now: Long = System.currentTimeMillis()): Boolean = when (event) {
        // Text only. `PlanProposed` also fires from transcript detection for sessions that
        // finished hours ago, so arming a verdict on it would put an approve button in
        // front of an operator with nothing behind it.
        is EngineEvent.PlanProposed -> {
            // A plan hold folded *before* its plan arrived would otherwise keep no plan for
            // good — and the panel would open on a genuine hold with nothing in it. Only a
            // hold that is itself the plan proposal is patched: filling in any waiting hold
            // would put a plan back on a command approval.
            val hold = held[event.session]
            val patch = hold != null && hold.what == PLAN_HOLD && hold.plan != event.plan
            if (plans[event.session] == event.plan && !patch) {
                false
            } else {
                plans[event.session] = event.plan
                if (patch) held[event.session] = hold.copy(plan = event.plan)
                true
            }
        }

        // A hook is holding: this session is the one that needs an answer.
        is EngineEvent.ApprovalRequested -> {
            held[event.session] = HeldApproval(
                sessionId = event.session,
                what = event.what,
                // Only when *this* hold is the plan proposal. The plan cache outlives any one
                // hold, and reading it unconditionally attached a long-settled plan to a
                // later `Bash` approval — which then opened the plan panel instead of asking
                // about the command.
                plan = if (event.what == PLAN_HOLD) plans[event.session] else null,
                mcpTool = event.authorizeTool,
                sinceMs = now,
            )
            true
        }

        // Anything but "waiting" means the hook was released — by us, by the cockpit, or
        // by a timeout. Either way there is nothing left to answer.
        is EngineEvent.SessionStateChanged ->
            if (statusIsWaiting(event.status)) false else held.remove(event.session) != null

        is EngineEvent.SessionRemoved -> {
            val had = held.remove(event.session) != null
            plans.remove(event.session)
            had
        }

        is EngineEvent.PhaseTransitioned -> false

        // Attention, not the gate — auto-resume listens for it separately.
        is EngineEvent.SessionAlert -> false
    }

    @Synchronized
    fun approval(sessionId: String): HeldApproval? = held[sessionId]

    /** Every outstanding hold, longest-waiting first. */
    @Synchronized
    fun approvals(): List<HeldApproval> = held.values.sortedBy { it.sinceMs }

    @Synchronized
    fun plan(sessionId: String): String? = plans[sessionId]
}
