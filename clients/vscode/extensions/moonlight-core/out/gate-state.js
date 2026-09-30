"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.GateState = void 0;
const moonlight_control_client_1 = require("moonlight-control-client");
class GateState {
    held = new Map();
    plans = new Map();
    /**
     * Replace the held set with a server snapshot.
     *
     * Wholesale, not merged: a stream that dropped may have carried both the arrival
     * and the answering of a hold, and a leftover entry would offer a verdict on a hook
     * nobody is holding any more — the operator answers, nothing happens, and the
     * editor says it worked.
     *
     * Plans are kept rather than replaced, because the snapshot only knows about holds
     * that are still open, while a plan stays worth reading after its verdict.
     */
    resync(rows) {
        this.held.clear();
        for (const row of rows) {
            this.held.set(row.session_id, {
                sessionId: row.session_id,
                what: row.what,
                plan: row.plan ?? undefined,
                mcpTool: row.mcp_tool ?? undefined,
                sinceMs: row.since_ms,
            });
            if (row.plan) {
                this.plans.set(row.session_id, row.plan);
            }
        }
    }
    /** Fold one event in. Returns whether anything a surface renders actually changed. */
    apply(event, now = Date.now()) {
        switch (event.kind) {
            // Text only. `PlanProposed` has two sources and only one is actionable: the
            // notifier publishes it when a hook is genuinely holding, and the supervisor
            // also publishes it from transcript detection — for sessions that finished
            // hours ago. Arming a verdict on this would put an approve button in front of
            // an operator with nothing behind it.
            case 'PlanProposed': {
                // A plan hold folded *before* its plan arrived keeps `plan: undefined` for
                // good: `ApprovalRequested` carries no plan of its own, and nothing else ever
                // revisits an entry once it is in the map. The notifier does publish the plan
                // first, but both travel the same stream and a resync can land between them —
                // and the result is the panel opening on a genuine hold with nothing in it,
                // which reads as the plan having been lost rather than not yet delivered.
                //
                // Only a hold that is *itself* the plan proposal is patched. Filling in any
                // waiting hold would put a plan back on the command approvals this fold was
                // just fixed to keep clear of one.
                const held = this.held.get(event.session);
                const patch = held !== undefined && held.what === moonlight_control_client_1.PLAN_HOLD && held.plan !== event.plan;
                // The dedup has to account for the patch: an identical `PlanProposed` repeated
                // while the hold is still missing its plan is exactly the case worth acting on,
                // and returning early on "same text" would skip it.
                if (this.plans.get(event.session) === event.plan && !patch) {
                    return false;
                }
                this.plans.set(event.session, event.plan);
                if (patch && held !== undefined) {
                    this.held.set(event.session, { ...held, plan: event.plan });
                }
                return true;
            }
            // A hook is holding: this session is the one that needs an answer.
            case 'ApprovalRequested':
                this.held.set(event.session, {
                    sessionId: event.session,
                    what: event.what,
                    // Only when *this* hold is the plan proposal. `ApprovalRequested` carries no
                    // plan of its own — the notifier publishes `PlanProposed` first and this
                    // correlates the two through `plans` — but that map is per session and
                    // deliberately outlives any one hold, so reading it unconditionally attached
                    // a long-settled plan to every later hold. A `Bash` approval then arrived
                    // carrying a plan, and the surface reading `plan !== undefined` opened the
                    // plan review panel for it: the operator was shown a plan they had already
                    // approved instead of the command they were being asked about.
                    //
                    // The server already draws this line (`HoldKind::Plan` is the only kind that
                    // carries a plan); this makes the folded event agree with it.
                    plan: event.what === moonlight_control_client_1.PLAN_HOLD ? this.plans.get(event.session) : undefined,
                    mcpTool: event.authorizeTool,
                    sinceMs: now,
                });
                return true;
            // Anything but "waiting" means the hook was released — by us, by the cockpit,
            // or by a timeout. Either way there is nothing left to answer.
            case 'SessionStateChanged':
                return (0, moonlight_control_client_1.statusIsWaiting)(event.status) ? false : this.held.delete(event.session);
            case 'SessionRemoved': {
                const had = this.held.delete(event.session);
                this.plans.delete(event.session);
                return had;
            }
            default:
                return false;
        }
    }
    approval(sessionId) {
        return this.held.get(sessionId);
    }
    /** Every outstanding hold, longest-waiting first. */
    approvals() {
        return [...this.held.values()].sort((a, b) => a.sinceMs - b.sinceMs);
    }
    plan(sessionId) {
        return this.plans.get(sessionId);
    }
}
exports.GateState = GateState;
//# sourceMappingURL=gate-state.js.map