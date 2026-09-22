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
            case 'PlanProposed':
                if (this.plans.get(event.session) === event.plan) {
                    return false;
                }
                this.plans.set(event.session, event.plan);
                return true;
            // A hook is holding: this session is the one that needs an answer.
            case 'ApprovalRequested':
                this.held.set(event.session, {
                    sessionId: event.session,
                    what: event.what,
                    plan: this.plans.get(event.session),
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