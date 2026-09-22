import { type PlanVerdict } from './feedback';
/** What happened, in words the caller can show. */
export type GateOutcome = {
    ok: true;
    summary: string;
} | {
    ok: false;
    error: string;
};
/**
 * Apply a plan verdict to a held session.
 *
 * Reports failure rather than throwing: a hold that could not be answered is
 * information the operator needs immediately — the session is still stopped, and it
 * is still their move.
 */
export declare function decidePlan(sessionId: string, plan: string, comments: ReadonlyMap<number, string>, verdict: PlanVerdict): Promise<GateOutcome>;
/**
 * Answer a danger-zone or MCP-authorize hold — no plan, just allow or refuse.
 */
export declare function decideAction(sessionId: string, allow: boolean, reason: string): Promise<GateOutcome>;
