/**
 * What is held at the gate, folded from the engine event stream.
 *
 * Kept apart from the activation closure so the folding can be tested: the rules here
 * are the ones that decide whether an operator is shown a live verdict or a dead
 * button, and both mistakes are expensive — a dead button gets clicked and believed,
 * and a missing one leaves a session stopped.
 */
import type { EngineEvent, PendingApproval } from 'moonlight-control-client';
import type { HeldApproval } from './api';
export declare class GateState {
    private readonly held;
    private readonly plans;
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
    resync(rows: readonly PendingApproval[]): void;
    /** Fold one event in. Returns whether anything a surface renders actually changed. */
    apply(event: EngineEvent, now?: number): boolean;
    approval(sessionId: string): HeldApproval | undefined;
    /** Every outstanding hold, longest-waiting first. */
    approvals(): readonly HeldApproval[];
    plan(sessionId: string): string | undefined;
}
