/**
 * Thin client over moonlightd's control API — see
 * `crates/mcp-server/src/control_api.rs` for the server side and
 * `apps/daemon/src/main.rs` for the headless binary. Deliberately dependency-free
 * (Node's built-in `http`, no fetch/axios) so this stays a small, throwaway spike.
 */
import * as http from 'http';

import { controlBaseUrl } from './daemon';

export {
  controlBaseUrl,
  daemonReachable,
  discoveryPath,
  ensureDaemon,
  stateAnchor,
} from './daemon';
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
export type BaselineView =
  | { kind: 'content'; text: string }
  | { kind: 'from_head'; text: string }
  | { kind: 'created' }
  | { kind: 'unavailable'; reason: string };

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
export function toThreads(comments: readonly ReviewCommentView[]): CommentThread[] {
  const roots = comments.filter((c) => c.parent_id === null);
  return roots.map((root) => ({
    root,
    replies: comments.filter((c) => c.parent_id === root.id),
  }));
}

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
export type FeedbackDelivery =
  | { status: 'queued' }
  | { status: 'undeliverable'; reason: string };

export interface RejectResponse {
  /** The file left the review queue. */
  reviewed: boolean;
  feedback: FeedbackDelivery;
}

/** Mirrors `moonlight_domain::phase::Phase`. */
export type Phase = 'Plan' | 'AutoImplement' | 'Test' | 'Review' | 'Commit';

/** Phases in which the PDP denies writes to project files. */
export const FROZEN_PHASES: readonly Phase[] = ['Plan', 'Commit'];

/** Mirrors `moonlight_domain::session::SessionStatus`. */
export type SessionStatus =
  | 'Running'
  | 'WaitingInput'
  | 'Done'
  | 'Errored'
  | 'Idle'
  | 'Paused';

/**
 * A session that is mid-turn is still writing the files you would be reviewing, so
 * a review opened against it is a review of a moving target.
 */
export function isReviewable(status: SessionStatus): boolean {
  return status !== 'Running';
}

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
export function shortId(sessionId: string): string {
  return sessionId.slice(0, 8);
}

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
export function sessionLabel(
  session: { session_id: string; title: string | null },
  among: readonly { session_id: string; title: string | null }[]
): string {
  const title = session.title ?? shortId(session.session_id);
  const collides = among.some(
    (other) => other.session_id !== session.session_id && (other.title ?? '') === (session.title ?? '')
  );
  return collides ? `${title} (${shortId(session.session_id)})` : title;
}

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

/**
 * How long a control-API call may take before it is abandoned.
 *
 * Comfortably above a loopback round-trip, and well below the 5s poll interval, so a
 * wedged daemon costs one tick rather than every tick after it.
 */
const REQUEST_TIMEOUT_MS = 3000;

function request<T>(method: string, urlPath: string, body?: unknown): Promise<T> {
  return new Promise((resolve, reject) => {
    const base = controlBaseUrl();
    if (!base) {
      reject(
        new Error(
          'MoonlightCode control API not found (~/.moonlight/control.json missing) — is moonlightd or the desktop app running?'
        )
      );
      return;
    }
    const payload = body === undefined ? undefined : Buffer.from(JSON.stringify(body), 'utf8');
    const req = http.request(
      `${base}${urlPath}`,
      {
        method,
        headers: payload
          ? { 'Content-Type': 'application/json', 'Content-Length': payload.length }
          : undefined,
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on('data', (chunk) => chunks.push(chunk));
        res.on('end', () => {
          const status = res.statusCode ?? 0;
          if (status >= 200 && status < 300) {
            const text = Buffer.concat(chunks).toString('utf8');
            // Guarded: an unparseable 2xx body used to throw *inside* this handler, so
            // the promise settled neither way and the caller's 5s poll stopped for the
            // life of the window — with no error surfaced, because nothing rejected.
            try {
              resolve((text ? JSON.parse(text) : undefined) as T);
            } catch (err) {
              reject(new Error(`${method} ${urlPath} → unreadable response: ${String(err)}`));
            }
          } else {
            reject(new Error(`${method} ${urlPath} → HTTP ${status}`));
          }
        });
      }
    );
    req.on('error', reject);
    // A daemon that accepts the socket and then stops answering would otherwise hold
    // this promise open forever, which wedges the poll exactly as the parse throw did.
    req.setTimeout(REQUEST_TIMEOUT_MS, () => {
      req.destroy(new Error(`${method} ${urlPath} → timed out after ${REQUEST_TIMEOUT_MS}ms`));
    });
    if (payload) {
      req.write(payload);
    }
    req.end();
  });
}

export function gatingStatus(): Promise<HookStatusEntry[]> {
  return request('GET', '/control/gating-status');
}

/**
 * Account quota + per-session context/uptime. The server caches the quota (it is a
 * network round-trip to Anthropic), so polling this on a UI cadence is cheap.
 */
export function usage(): Promise<UsageResponse> {
  return request('GET', '/control/usage');
}

export function reviewQueue(): Promise<ReviewQueueItem[]> {
  return request('GET', '/control/review-queue');
}

export function accept(sessionId: string, filePath: string): Promise<void> {
  return request('POST', '/control/review-queue/accept', { session_id: sessionId, path: filePath });
}

export function reject(
  sessionId: string,
  filePath: string,
  message: string
): Promise<RejectResponse> {
  return request('POST', '/control/review-queue/reject', {
    session_id: sessionId,
    path: filePath,
    message,
  });
}

/**
 * Sessions detection has found — adopted or not. An unadopted one lives only in
 * the running backend's in-memory fleet (see `BusFleetView` on the Rust side), so
 * this is the only way to see "here's a Claude Code session you could adopt."
 */
export function discoverableSessions(): Promise<DiscoverableSession[]> {
  return request('GET', '/control/discoverable-sessions');
}

/** Opt a discovered session into MoonlightCode governance. */
export function adopt(sessionId: string): Promise<void> {
  return request('POST', '/control/adopt', { session_id: sessionId });
}

/**
 * Move a session to an explicit phase (an operator override — the engine pins it).
 * This is the escape hatch from `Plan`, where project writes are denied: without it,
 * adopting a session from an editor freezes it with no way back.
 */
export function setPhase(sessionId: string, phase: Phase): Promise<void> {
  return request('POST', '/control/phase', { session_id: sessionId, phase });
}

/** Advance to the next phase and return the session to auto (clears any pin). */
export function advancePhase(sessionId: string): Promise<void> {
  return request('POST', '/control/phase/advance', { session_id: sessionId });
}

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
export function verdict(sessionId: string, approve: boolean, reason?: string): Promise<void> {
  return request('POST', '/control/verdict', {
    session_id: sessionId,
    approve,
    reason: reason ?? null,
  });
}

/**
 * Approve a plan: release the held hook, then move the session out of `Plan`.
 *
 * Both halves are required and neither implies the other — the server swallows an
 * `ApproveAction` that resolved a held hook, so it never reaches the engine, and a
 * phase advance alone leaves the agent still blocked on its hook. The Zed fork does
 * exactly this pairing in `plan_review.rs`; getting it wrong looks like an approval
 * that did nothing.
 */
export async function approvePlan(sessionId: string): Promise<void> {
  await verdict(sessionId, true);
  await advancePhase(sessionId);
}

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
export function mcpEndpoint(sessionId: string): Promise<{ url: string }> {
  return request('POST', '/control/mcp-endpoint', { session_id: sessionId });
}

/**
 * Single-quote a value for a POSIX shell.
 *
 * `'` cannot be escaped inside single quotes, so the quoting is closed, an escaped
 * quote is emitted, and quoting reopens — the standard `'\''` dance.
 */
function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * A loopback MCP endpoint we are willing to put on a command line, or `undefined`.
 *
 * The URL is an HTTP response body from whatever is listening on the port in
 * `~/.moonlight/control.json` — a world-readable file in `$HOME` that the gated agent
 * is told to read. So it crosses a trust boundary and is validated like one: an
 * allowlist of scheme and host, not a scan for bad characters.
 */
export function safeEndpointUrl(url: string): string | undefined {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return undefined;
  }
  const loopback = parsed.hostname === '127.0.0.1' || parsed.hostname === 'localhost' || parsed.hostname === '[::1]';
  return (parsed.protocol === 'http:' || parsed.protocol === 'https:') && loopback
    ? parsed.toString()
    : undefined;
}

/** A session id we are willing to put on a command line — the shape Claude Code mints. */
export function isSafeSessionId(id: string): boolean {
  return /^[0-9a-fA-F-]{8,64}$/.test(id);
}

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
export function mcpConfigFlag(url: string): string | undefined {
  const safe = safeEndpointUrl(url);
  if (safe === undefined) {
    return undefined;
  }
  const json = JSON.stringify({ mcpServers: { moonlight: { type: 'http', url: safe } } });
  return ` --mcp-config ${shellQuote(json)}`;
}

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
export function pendingApprovals(): Promise<PendingApproval[]> {
  return request('GET', '/control/pending-approvals');
}

const q = encodeURIComponent;

/** The before side of a reviewed file — the left pane of the diff editor. */
export function baseline(sessionId: string, filePath: string): Promise<BaselineView> {
  return request('GET', `/control/review/baseline?session=${q(sessionId)}&path=${q(filePath)}`);
}

export function comments(sessionId: string): Promise<ReviewCommentView[]> {
  return request('GET', `/control/review/comments?session=${q(sessionId)}`);
}

/**
 * Persist a comment immediately. Reviews survive closing the editor because they
 * live in the shared tracker, not in this extension's memory.
 */
export function addComment(input: {
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
}): Promise<ReviewCommentView> {
  return request('POST', '/control/review/comments', {
    session_id: input.sessionId,
    scope: input.scope,
    path: input.path,
    side: input.side,
    start_line: input.startLine,
    end_line: input.endLine,
    body: input.body,
    anchor_text: input.anchorText ?? null,
    parent_id: input.parentId ?? null,
    author: input.author ?? null,
  });
}

/** Edit a comment's body and/or resolve it. Editing re-queues it for the next batch. */
export function updateComment(
  sessionId: string,
  id: string,
  patch: { body?: string; resolved?: boolean }
): Promise<ReviewCommentView> {
  return request('POST', '/control/review/comments/update', {
    session_id: sessionId,
    id,
    body: patch.body ?? null,
    resolved: patch.resolved ?? null,
  });
}

export function deleteComment(sessionId: string, id: string): Promise<void> {
  return request('POST', '/control/review/comments/delete', { session_id: sessionId, id });
}

/** Deliver every unsent, unresolved comment to the session as one message. */
export function submitReview(sessionId: string): Promise<SubmitReviewResponse | undefined> {
  return request('POST', '/control/review/submit', { session_id: sessionId });
}

/**
 * Record a delivery **we** performed. Only call this after actually writing the
 * review into the session — the server cannot observe an editor's terminal, so this
 * is a claim it has to take on trust.
 */
export function markDelivered(sessionId: string, commentIds: string[]): Promise<void> {
  return request('POST', '/control/review/delivered', {
    session_id: sessionId,
    comment_ids: commentIds,
  });
}

export function ignoredPaths(sessionId: string): Promise<string[]> {
  return request('GET', `/control/review/ignore?session=${q(sessionId)}`);
}

export function setIgnored(sessionId: string, filePath: string, ignored: boolean): Promise<void> {
  return request('POST', '/control/review/ignore', {
    session_id: sessionId,
    path: filePath,
    ignored,
  });
}
