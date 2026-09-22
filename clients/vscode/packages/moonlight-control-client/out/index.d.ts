export { controlBaseUrl, daemonReachable, discoveryPath, ensureDaemon, stateAnchor, } from './daemon';
export type { DaemonOptions, DaemonStartResult } from './daemon';
export { parseEngineEvent, readFrames, statusIsWaiting, subscribeEvents } from './events';
export type { EngineEvent, EventStreamHandlers, EventSubscription, SseFrame } from './events';
export interface HookStatusEntry {
    event: string;
    matcher: string;
    installed: boolean;
    command: string;
}
/** Mirrors `moonlight_domain::changes::ChangeTool`. */
export type ChangeTool = string;
export interface ReviewQueueItem {
    session_id: string;
    session_title: string | null;
    file_path: string;
    /** How many times the session wrote this file. */
    touches: number;
    /** The tool that wrote it most recently. */
    tool: ChangeTool;
    /** The session created the file — the before side is empty. */
    created: boolean;
    /**
     * The baseline came from VCS rather than an observed pre-image, so the before
     * side may include edits that were already uncommitted. Shown because reading an
     * inferred baseline as an exact one leads to wrong review conclusions.
     */
    from_head: boolean;
    diff: string;
}
/** The "before" side of a reviewed file — content, plus what kind of baseline it is. */
export type BaselineView = {
    kind: 'content';
    text: string;
} | {
    kind: 'from_head';
    text: string;
} | {
    kind: 'created';
} | {
    kind: 'unavailable';
    reason: string;
};
/** Mirrors `moonlight_domain::changes::CommentScope`. */
export type CommentScope = 'Line' | 'File' | 'Review';
/** Mirrors `moonlight_domain::changes::DiffSide`. */
export type DiffSide = 'Before' | 'After';
/**
 * Who wrote a comment. A review is a conversation; without this a client cannot tell
 * the reviewer's objection from the session's answer to it, and shows a review
 * talking to itself.
 */
export type CommentAuthor = 'Operator' | 'Agent';
export interface ReviewCommentView {
    id: string;
    scope: CommentScope;
    path: string;
    side: DiffSide;
    start_line: number;
    end_line: number;
    body: string;
    anchor_text: string | null;
    /** Delivered to the session in a submitted review. */
    sent: boolean;
    resolved: boolean;
    /**
     * The anchored line no longer reads as it did when the comment was written.
     * This is the review's expiry condition — not elapsed time.
     */
    outdated: boolean;
    author: CommentAuthor;
    /** The comment this one answers; null for a thread root. Group on it to render threads. */
    parent_id: string | null;
}
/** A comment and the answers to it, in the order they were written. */
export interface CommentThread {
    root: ReviewCommentView;
    replies: ReviewCommentView[];
}
/**
 * Group a flat comment list into threads, roots in their original order.
 *
 * A reply whose root is missing is dropped rather than shown as a root of its own:
 * an answer with nothing to answer reads as a fresh objection, which is worse than
 * not showing it.
 */
export declare function toThreads(comments: readonly ReviewCommentView[]): CommentThread[];
export interface SubmitReviewResponse {
    /** Comments in the batch — delivered only if `feedback.status === 'queued'`. */
    comment_count: number;
    /** The comments this batch carried, to confirm back if we deliver them ourselves. */
    comment_ids: string[];
    /** Where the full review was written. What gets delivered is a pointer to it. */
    review_path: string | null;
    feedback: FeedbackDelivery;
}
/**
 * Whether a reject's feedback could actually reach the agent. Rejecting always
 * drops the file from the queue; steering the session with the message is the part
 * that can silently fail (a headless backend has no terminal to write into), so the
 * server reports the two outcomes separately rather than a bare success.
 */
export type FeedbackDelivery = {
    status: 'queued';
} | {
    status: 'undeliverable';
    reason: string;
};
export interface RejectResponse {
    /** The file left the review queue. */
    reviewed: boolean;
    feedback: FeedbackDelivery;
}
/** Mirrors `moonlight_domain::phase::Phase`. */
export type Phase = 'Plan' | 'AutoImplement' | 'Test' | 'Review' | 'Commit';
/** Phases in which the PDP denies writes to project files. */
export declare const FROZEN_PHASES: readonly Phase[];
/** Mirrors `moonlight_domain::session::SessionStatus`. */
export type SessionStatus = 'Running' | 'WaitingInput' | 'Done' | 'Errored' | 'Idle' | 'Paused';
/**
 * A session that is mid-turn is still writing the files you would be reviewing, so
 * a review opened against it is a review of a moving target.
 */
export declare function isReviewable(status: SessionStatus): boolean;
export interface DiscoverableSession {
    session_id: string;
    title: string | null;
    root: string | null;
    adopted: boolean;
    /** Live status from detection — Claude Code exposes no idle/mid-turn signal. */
    status: SessionStatus;
    /** Only meaningful while adopted — an unadopted session is never gated. */
    phase: Phase;
    /**
     * Files this session wrote that nobody has reviewed yet. Carried here rather than
     * counted from `reviewQueue()`, which renders a unified diff per file — far too
     * much work to repeat every poll just to show a number.
     */
    unreviewed_files: number;
}
/** The leading characters of a session id — enough to tell two sessions apart by eye. */
export declare function shortId(sessionId: string): string;
/**
 * How to name a session in a list, given the others it sits beside.
 *
 * Titles are not unique — two sessions started the same way are both "Test session" —
 * and every picker here asks a question ("which one are you in?", "which one do you
 * want to govern?") whose whole value depends on the answer being distinguishable.
 * Two identical rows make that a coin flip, and picking the wrong one means pinning
 * the status bar to a session nobody is talking to.
 *
 * The id is appended only when the title actually collides: a suffix on every row is
 * noise that trains you to stop reading the row.
 */
export declare function sessionLabel(session: {
    session_id: string;
    title: string | null;
}, among: readonly {
    session_id: string;
    title: string | null;
}[]): string;
/**
 * Account-wide Claude usage — the rolling 5-hour window, the weekly window, and the
 * weekly Sonnet-specific one, each as a percentage consumed.
 *
 * Every field is nullable because every source is. A client must render `—` for a
 * missing figure rather than 0, which would read as "plenty left" at exactly the
 * moment the truth is unknown.
 */
export interface AccountUsage {
    five_hour_pct: number | null;
    /** Unix epoch **seconds** (UTC) — for running a live countdown client-side. */
    five_hour_resets_at: number | null;
    /** The same reset pre-rendered by the server (`2h14m`, `47m`, `<1m`). */
    five_hour_resets_in: string | null;
    weekly_pct: number | null;
    sonnet_pct: number | null;
}
/**
 * One session's context occupancy and uptime, from the statusline snapshots Claude
 * Code writes. Only sessions MoonlightCode registered its statusline with appear —
 * absence means "not observed", not "idle".
 */
export interface SessionUsage {
    session_id: string;
    title: string | null;
    model: string;
    persona: string;
    session_ms: number;
    /** `session_ms` pre-rendered compactly (`1h12m`). */
    session_uptime: string;
    /** Context-window occupancy %, or null when neither source has a figure. */
    ctx_pct: number | null;
    ctx_tokens: number;
    ctx_limit: number;
    cost_usd: number;
    /** When the snapshot was written (epoch ms) — old means the session went quiet. */
    updated_ms: number;
}
export interface UsageResponse {
    /** Null when no quota source could be read at all (no credentials, or offline). */
    quota: AccountUsage | null;
    /** Freshest-observed first. */
    sessions: SessionUsage[];
}
export declare function gatingStatus(): Promise<HookStatusEntry[]>;
/**
 * Account quota + per-session context/uptime. The server caches the quota (it is a
 * network round-trip to Anthropic), so polling this on a UI cadence is cheap.
 */
export declare function usage(): Promise<UsageResponse>;
export declare function reviewQueue(): Promise<ReviewQueueItem[]>;
export declare function accept(sessionId: string, filePath: string): Promise<void>;
export declare function reject(sessionId: string, filePath: string, message: string): Promise<RejectResponse>;
/**
 * Sessions detection has found — adopted or not. An unadopted one lives only in
 * the running backend's in-memory fleet (see `BusFleetView` on the Rust side), so
 * this is the only way to see "here's a Claude Code session you could adopt."
 */
export declare function discoverableSessions(): Promise<DiscoverableSession[]>;
/** Opt a discovered session into MoonlightCode governance. */
export declare function adopt(sessionId: string): Promise<void>;
/**
 * Move a session to an explicit phase (an operator override — the engine pins it).
 * This is the escape hatch from `Plan`, where project writes are denied: without it,
 * adopting a session from an editor freezes it with no way back.
 */
export declare function setPhase(sessionId: string, phase: Phase): Promise<void>;
/** Advance to the next phase and return the session to auto (clears any pin). */
export declare function advancePhase(sessionId: string): Promise<void>;
/**
 * Answer a held approval — a plan proposal, or an action a frozen phase stopped.
 *
 * Only the process holding the hook can release it, which is the whole reason this is
 * an endpoint rather than something a client resolves locally. A denial's `reason` is
 * the feedback the agent acts on, so a bare refusal teaches it nothing: callers are
 * expected to say why.
 *
 * This releases the *hook*. Releasing a plan hold is only half of approving a plan —
 * see {@link approvePlan}.
 */
export declare function verdict(sessionId: string, approve: boolean, reason?: string): Promise<void>;
/**
 * Approve a plan: release the held hook, then move the session out of `Plan`.
 *
 * Both halves are required and neither implies the other — the server swallows an
 * `ApproveAction` that resolved a held hook, so it never reaches the engine, and a
 * phase advance alone leaves the agent still blocked on its hook. The Zed fork does
 * exactly this pairing in `plan_review.rs`; getting it wrong looks like an approval
 * that did nothing.
 */
export declare function approvePlan(sessionId: string): Promise<void>;
/**
 * Bind this session's `moonlight` MCP endpoint and get its URL.
 *
 * Called *before* launching, because the URL goes into the launch arguments. The
 * daemon cannot bind it in advance: the endpoint belongs to a session id, and the id
 * is minted by whoever starts the session — here, us.
 *
 * Without it the agent launches with no `moonlight` verbs at all: no `present_plan`,
 * no `request_phase`, no `phase_status`, no `report_blocked`. The session is still
 * gated by the hooks, but it has no way to ask for a phase or propose a plan — which
 * reads as an agent that inexplicably cannot get out of `Plan`.
 *
 * A backend built without the MCP transport answers 501; the caller launches anyway
 * rather than refusing to start a session over a missing convenience.
 */
export declare function mcpEndpoint(sessionId: string): Promise<{
    url: string;
}>;
/**
 * A loopback MCP endpoint we are willing to put on a command line, or `undefined`.
 *
 * The URL is an HTTP response body from whatever is listening on the port in
 * `~/.moonlight/control.json` — a world-readable file in `$HOME` that the gated agent
 * is told to read. So it crosses a trust boundary and is validated like one: an
 * allowlist of scheme and host, not a scan for bad characters.
 */
export declare function safeEndpointUrl(url: string): string | undefined;
/** A session id we are willing to put on a command line — the shape Claude Code mints. */
export declare function isSafeSessionId(id: string): boolean;
/**
 * The `--mcp-config` fragment (leading space included) that wires `url` in as the
 * session's `moonlight` server. `undefined` when the URL is not one we will run.
 *
 * The previous version asserted "the JSON carries no single quotes, so the
 * single-quoted shell argument is safe". `JSON.stringify` escapes `"` and control
 * characters and never `'`, so a URL containing one closed the quoting and the rest of
 * it ran as shell — and this string is typed into the operator's terminal followed by
 * Enter. Validated first, then quoted properly.
 */
export declare function mcpConfigFlag(url: string): string | undefined;
/** A hold currently waiting on an operator, as `GET /control/pending-approvals` reports it. */
export interface PendingApproval {
    session_id: string;
    /** What is being asked, in the words the gate used. */
    what: string;
    /** The plan markdown, when the hold is a plan proposal. */
    plan: string | null;
    /** The full `mcp__server__tool` name for an external MCP tool held by a frozen phase. */
    mcp_tool: string | null;
    /** When the hold started (epoch ms) — a client shows how long it has been waiting. */
    since_ms: number;
}
/**
 * Holds outstanding right now.
 *
 * The event stream announces a hold once, when it starts. A window opened or reloaded
 * after that point would otherwise never learn about a session sitting blocked — and
 * because the daemon holds indefinitely waiting for a client, nothing would ever
 * resolve it. This is the catch-up read that makes the stream safe to miss.
 */
export declare function pendingApprovals(): Promise<PendingApproval[]>;
/** The before side of a reviewed file — the left pane of the diff editor. */
export declare function baseline(sessionId: string, filePath: string): Promise<BaselineView>;
export declare function comments(sessionId: string): Promise<ReviewCommentView[]>;
/**
 * Persist a comment immediately. Reviews survive closing the editor because they
 * live in the shared tracker, not in this extension's memory.
 */
export declare function addComment(input: {
    sessionId: string;
    scope: CommentScope;
    path: string;
    side: DiffSide;
    startLine: number;
    endLine: number;
    body: string;
    anchorText?: string;
    /**
     * Answer an existing comment instead of starting a thread. The reply inherits that
     * comment's file, side and line range server-side — the anchor fields above are
     * ignored — so a reply can never drift to different code than the thread it is in.
     */
    parentId?: string;
    /** Defaults to the operator server-side; an editor client is the reviewer. */
    author?: CommentAuthor;
}): Promise<ReviewCommentView>;
/** Edit a comment's body and/or resolve it. Editing re-queues it for the next batch. */
export declare function updateComment(sessionId: string, id: string, patch: {
    body?: string;
    resolved?: boolean;
}): Promise<ReviewCommentView>;
export declare function deleteComment(sessionId: string, id: string): Promise<void>;
/** Deliver every unsent, unresolved comment to the session as one message. */
export declare function submitReview(sessionId: string): Promise<SubmitReviewResponse | undefined>;
/**
 * Record a delivery **we** performed. Only call this after actually writing the
 * review into the session — the server cannot observe an editor's terminal, so this
 * is a claim it has to take on trust.
 */
export declare function markDelivered(sessionId: string, commentIds: string[]): Promise<void>;
export declare function ignoredPaths(sessionId: string): Promise<string[]>;
export declare function setIgnored(sessionId: string, filePath: string, ignored: boolean): Promise<void>;
