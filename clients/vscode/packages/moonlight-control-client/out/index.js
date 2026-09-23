"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.FROZEN_PHASES = exports.subscribeEvents = exports.statusIsWaiting = exports.readFrames = exports.parseEngineEvent = exports.stateAnchor = exports.ensureDaemon = exports.discoveryPath = exports.daemonReachable = exports.daemonBinaryName = exports.controlBaseUrl = void 0;
exports.toThreads = toThreads;
exports.isReviewable = isReviewable;
exports.shortId = shortId;
exports.sessionLabel = sessionLabel;
exports.gatingStatus = gatingStatus;
exports.usage = usage;
exports.reviewQueue = reviewQueue;
exports.accept = accept;
exports.reject = reject;
exports.discoverableSessions = discoverableSessions;
exports.adopt = adopt;
exports.setPhase = setPhase;
exports.advancePhase = advancePhase;
exports.verdict = verdict;
exports.approvePlan = approvePlan;
exports.mcpEndpoint = mcpEndpoint;
exports.safeEndpointUrl = safeEndpointUrl;
exports.isSafeSessionId = isSafeSessionId;
exports.mcpConfigFlag = mcpConfigFlag;
exports.pendingApprovals = pendingApprovals;
exports.baseline = baseline;
exports.comments = comments;
exports.addComment = addComment;
exports.updateComment = updateComment;
exports.deleteComment = deleteComment;
exports.submitReview = submitReview;
exports.markDelivered = markDelivered;
exports.ignoredPaths = ignoredPaths;
exports.setIgnored = setIgnored;
/**
 * Thin client over moonlightd's control API — see
 * `crates/mcp-server/src/control_api.rs` for the server side and
 * `apps/daemon/src/main.rs` for the headless binary. Deliberately dependency-free
 * (Node's built-in `http`, no fetch/axios) so this stays a small, throwaway spike.
 */
const http = __importStar(require("http"));
const daemon_1 = require("./daemon");
var daemon_2 = require("./daemon");
Object.defineProperty(exports, "controlBaseUrl", { enumerable: true, get: function () { return daemon_2.controlBaseUrl; } });
Object.defineProperty(exports, "daemonBinaryName", { enumerable: true, get: function () { return daemon_2.daemonBinaryName; } });
Object.defineProperty(exports, "daemonReachable", { enumerable: true, get: function () { return daemon_2.daemonReachable; } });
Object.defineProperty(exports, "discoveryPath", { enumerable: true, get: function () { return daemon_2.discoveryPath; } });
Object.defineProperty(exports, "ensureDaemon", { enumerable: true, get: function () { return daemon_2.ensureDaemon; } });
Object.defineProperty(exports, "stateAnchor", { enumerable: true, get: function () { return daemon_2.stateAnchor; } });
var events_1 = require("./events");
Object.defineProperty(exports, "parseEngineEvent", { enumerable: true, get: function () { return events_1.parseEngineEvent; } });
Object.defineProperty(exports, "readFrames", { enumerable: true, get: function () { return events_1.readFrames; } });
Object.defineProperty(exports, "statusIsWaiting", { enumerable: true, get: function () { return events_1.statusIsWaiting; } });
Object.defineProperty(exports, "subscribeEvents", { enumerable: true, get: function () { return events_1.subscribeEvents; } });
/**
 * Group a flat comment list into threads, roots in their original order.
 *
 * A reply whose root is missing is dropped rather than shown as a root of its own:
 * an answer with nothing to answer reads as a fresh objection, which is worse than
 * not showing it.
 */
function toThreads(comments) {
    const roots = comments.filter((c) => c.parent_id === null);
    return roots.map((root) => ({
        root,
        replies: comments.filter((c) => c.parent_id === root.id),
    }));
}
/** Phases in which the PDP denies writes to project files. */
exports.FROZEN_PHASES = ['Plan', 'Commit'];
/**
 * A session that is mid-turn is still writing the files you would be reviewing, so
 * a review opened against it is a review of a moving target.
 */
function isReviewable(status) {
    return status !== 'Running';
}
/** The leading characters of a session id — enough to tell two sessions apart by eye. */
function shortId(sessionId) {
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
function sessionLabel(session, among) {
    const title = session.title ?? shortId(session.session_id);
    const collides = among.some((other) => other.session_id !== session.session_id && (other.title ?? '') === (session.title ?? ''));
    return collides ? `${title} (${shortId(session.session_id)})` : title;
}
/**
 * How long a control-API call may take before it is abandoned.
 *
 * Comfortably above a loopback round-trip, and well below the 5s poll interval, so a
 * wedged daemon costs one tick rather than every tick after it.
 */
const REQUEST_TIMEOUT_MS = 3000;
function request(method, urlPath, body) {
    return new Promise((resolve, reject) => {
        const base = (0, daemon_1.controlBaseUrl)();
        if (!base) {
            reject(new Error('MoonlightCode control API not found (~/.moonlight/control.json missing) — is moonlightd or the desktop app running?'));
            return;
        }
        const payload = body === undefined ? undefined : Buffer.from(JSON.stringify(body), 'utf8');
        const req = http.request(`${base}${urlPath}`, {
            method,
            headers: payload
                ? { 'Content-Type': 'application/json', 'Content-Length': payload.length }
                : undefined,
        }, (res) => {
            const chunks = [];
            res.on('data', (chunk) => chunks.push(chunk));
            res.on('end', () => {
                const status = res.statusCode ?? 0;
                if (status >= 200 && status < 300) {
                    const text = Buffer.concat(chunks).toString('utf8');
                    // Guarded: an unparseable 2xx body used to throw *inside* this handler, so
                    // the promise settled neither way and the caller's 5s poll stopped for the
                    // life of the window — with no error surfaced, because nothing rejected.
                    try {
                        resolve((text ? JSON.parse(text) : undefined));
                    }
                    catch (err) {
                        reject(new Error(`${method} ${urlPath} → unreadable response: ${String(err)}`));
                    }
                }
                else {
                    reject(new Error(`${method} ${urlPath} → HTTP ${status}`));
                }
            });
        });
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
function gatingStatus() {
    return request('GET', '/control/gating-status');
}
/**
 * Account quota + per-session context/uptime. The server caches the quota (it is a
 * network round-trip to Anthropic), so polling this on a UI cadence is cheap.
 */
function usage() {
    return request('GET', '/control/usage');
}
function reviewQueue() {
    return request('GET', '/control/review-queue');
}
function accept(sessionId, filePath) {
    return request('POST', '/control/review-queue/accept', { session_id: sessionId, path: filePath });
}
function reject(sessionId, filePath, message) {
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
function discoverableSessions() {
    return request('GET', '/control/discoverable-sessions');
}
/** Opt a discovered session into MoonlightCode governance. */
function adopt(sessionId) {
    return request('POST', '/control/adopt', { session_id: sessionId });
}
/**
 * Move a session to an explicit phase (an operator override — the engine pins it).
 * This is the escape hatch from `Plan`, where project writes are denied: without it,
 * adopting a session from an editor freezes it with no way back.
 */
function setPhase(sessionId, phase) {
    return request('POST', '/control/phase', { session_id: sessionId, phase });
}
/** Advance to the next phase and return the session to auto (clears any pin). */
function advancePhase(sessionId) {
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
function verdict(sessionId, approve, reason) {
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
async function approvePlan(sessionId) {
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
function mcpEndpoint(sessionId) {
    return request('POST', '/control/mcp-endpoint', { session_id: sessionId });
}
/**
 * Single-quote a value for a POSIX shell.
 *
 * `'` cannot be escaped inside single quotes, so the quoting is closed, an escaped
 * quote is emitted, and quoting reopens — the standard `'\''` dance.
 */
function shellQuote(value) {
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
function safeEndpointUrl(url) {
    let parsed;
    try {
        parsed = new URL(url);
    }
    catch {
        return undefined;
    }
    const loopback = parsed.hostname === '127.0.0.1' || parsed.hostname === 'localhost' || parsed.hostname === '[::1]';
    return (parsed.protocol === 'http:' || parsed.protocol === 'https:') && loopback
        ? parsed.toString()
        : undefined;
}
/** A session id we are willing to put on a command line — the shape Claude Code mints. */
function isSafeSessionId(id) {
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
function mcpConfigFlag(url) {
    const safe = safeEndpointUrl(url);
    if (safe === undefined) {
        return undefined;
    }
    const json = JSON.stringify({ mcpServers: { moonlight: { type: 'http', url: safe } } });
    return ` --mcp-config ${shellQuote(json)}`;
}
/**
 * Holds outstanding right now.
 *
 * The event stream announces a hold once, when it starts. A window opened or reloaded
 * after that point would otherwise never learn about a session sitting blocked — and
 * because the daemon holds indefinitely waiting for a client, nothing would ever
 * resolve it. This is the catch-up read that makes the stream safe to miss.
 */
function pendingApprovals() {
    return request('GET', '/control/pending-approvals');
}
const q = encodeURIComponent;
/** The before side of a reviewed file — the left pane of the diff editor. */
function baseline(sessionId, filePath) {
    return request('GET', `/control/review/baseline?session=${q(sessionId)}&path=${q(filePath)}`);
}
function comments(sessionId) {
    return request('GET', `/control/review/comments?session=${q(sessionId)}`);
}
/**
 * Persist a comment immediately. Reviews survive closing the editor because they
 * live in the shared tracker, not in this extension's memory.
 */
function addComment(input) {
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
function updateComment(sessionId, id, patch) {
    return request('POST', '/control/review/comments/update', {
        session_id: sessionId,
        id,
        body: patch.body ?? null,
        resolved: patch.resolved ?? null,
    });
}
function deleteComment(sessionId, id) {
    return request('POST', '/control/review/comments/delete', { session_id: sessionId, id });
}
/** Deliver every unsent, unresolved comment to the session as one message. */
function submitReview(sessionId) {
    return request('POST', '/control/review/submit', { session_id: sessionId });
}
/**
 * Record a delivery **we** performed. Only call this after actually writing the
 * review into the session — the server cannot observe an editor's terminal, so this
 * is a claim it has to take on trust.
 */
function markDelivered(sessionId, commentIds) {
    return request('POST', '/control/review/delivered', {
        session_id: sessionId,
        comment_ids: commentIds,
    });
}
function ignoredPaths(sessionId) {
    return request('GET', `/control/review/ignore?session=${q(sessionId)}`);
}
function setIgnored(sessionId, filePath, ignored) {
    return request('POST', '/control/review/ignore', {
        session_id: sessionId,
        path: filePath,
        ignored,
    });
}
//# sourceMappingURL=index.js.map