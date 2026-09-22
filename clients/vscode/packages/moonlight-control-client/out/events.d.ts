/**
 * The `EngineEvent` variants this client consumes, in serde's externally-tagged
 * shape (`{"PlanProposed": {...}}`).
 *
 * Deliberately partial: `crates/engine/src/lib.rs` carries more variants, and a
 * client that enumerated all of them would need editing every time the engine grew
 * one. Unknown variants fall through {@link parseEngineEvent} as `undefined`.
 */
export type EngineEvent = {
    kind: 'PlanProposed';
    session: string;
    plan: string;
} | {
    kind: 'ApprovalRequested';
    session: string;
    what: string;
    authorizeTool: string | undefined;
} | {
    kind: 'SessionStateChanged';
    session: string;
    status: string;
} | {
    kind: 'PhaseTransitioned';
    session: string;
    phase: string;
} | {
    kind: 'SessionRemoved';
    session: string;
};
/**
 * Whether a status means the session is waiting on us rather than working.
 *
 * A hold is disarmed when a session reports any other status, because the hook it was
 * holding has been released by someone — possibly another client. Mirrors
 * `status_is_waiting` in `apps/zed_based_desktop/crates/moonlight_ui/src/plan_review.rs`,
 * which is `WaitingInput` alone: `Running` is precisely what the server publishes when
 * a verdict resolves a hold.
 */
export declare function statusIsWaiting(status: string): boolean;
/**
 * Turn one decoded SSE `data:` payload into an event, or `undefined` when it is a
 * variant we do not model.
 *
 * Tolerant on purpose: an engine that adds a variant, or renames a field on one we
 * ignore, must not break the stream for the variants we do handle.
 */
export declare function parseEngineEvent(json: string): EngineEvent | undefined;
/** One parsed SSE frame: its `event:` name (default `message`) and its data. */
export interface SseFrame {
    event: string;
    data: string;
}
/**
 * Split an SSE byte stream into frames, keeping whatever trailing partial frame the
 * socket has not finished sending.
 *
 * Written as a pure function over an accumulated buffer so the framing can be tested
 * without a server: the interesting cases (a frame split mid-line, a multi-line
 * `data:`) are exactly the ones a live test would only hit by luck.
 */
export declare function readFrames(buffer: string): {
    frames: SseFrame[];
    rest: string;
};
export interface EventStreamHandlers {
    onEvent(event: EngineEvent): void;
    /**
     * The folded view can no longer be trusted — the stream lagged, dropped, or the
     * daemon went away. The caller re-reads the authoritative endpoints.
     *
     * Reported for a reconnect as well as a lag: a client that reconnected silently
     * would show a plan gate that was answered while it was disconnected.
     */
    onDesync(reason: string): void;
}
/** Stop listening. Safe to call twice. */
export interface EventSubscription {
    dispose(): void;
}
/**
 * Follow the engine event stream, reconnecting for as long as the subscription lives.
 *
 * Reconnection is the normal case, not an error path: the daemon is autostarted and
 * restarted under running windows, so a stream that gave up on the first failure
 * would leave the editor permanently blind after an ordinary daemon upgrade.
 */
export declare function subscribeEvents(handlers: EventStreamHandlers): EventSubscription;
