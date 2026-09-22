import type { MoonlightApi } from 'moonlight-core';
export declare class PlanReviewPanel {
    private readonly panel;
    private readonly core;
    private static current;
    private readonly disposables;
    private comments;
    private sessionId;
    private plan;
    /** How many comment editors are open — a refresh while one is would discard it. */
    private editing;
    /** A refresh that arrived while the operator was typing, owed once they stop. */
    private renderPending;
    static show(core: MoonlightApi, sessionId: string | undefined): void;
    private constructor();
    /**
     * Point the panel at a session.
     *
     * Passing `undefined` means "whichever session is blocked", which is the common
     * case: an operator opening this is answering the thing that stopped, and asking
     * them which session that is would be asking a question the editor can answer.
     */
    private bind;
    private onMessage;
    private render;
    private html;
    private dispose;
}
