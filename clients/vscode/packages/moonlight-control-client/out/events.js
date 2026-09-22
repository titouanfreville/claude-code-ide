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
exports.statusIsWaiting = statusIsWaiting;
exports.parseEngineEvent = parseEngineEvent;
exports.readFrames = readFrames;
exports.subscribeEvents = subscribeEvents;
/**
 * The engine event stream — `GET /control/events`, served as SSE by
 * `crates/mcp-server/src/control_api.rs`.
 *
 * This is what lets an editor stop guessing. Polling answers "what is true now" on a
 * five-second cadence, which is fine for a session list and useless for a hold: a
 * session that proposes a plan is *stopped* until someone answers, and an operator
 * watching a status bar catch up four seconds later has already started wondering
 * whether the thing is broken.
 *
 * The stream is a change notification, not the record. A client that misses events —
 * a lag, a reconnect — re-reads the authoritative endpoints instead of assuming its
 * folded view is still complete. That is why {@link subscribeEvents} reports
 * `onDesync` as loudly as it reports events.
 */
const http = __importStar(require("http"));
const daemon_1 = require("./daemon");
/**
 * Whether a status means the session is waiting on us rather than working.
 *
 * A hold is disarmed when a session reports any other status, because the hook it was
 * holding has been released by someone — possibly another client. Mirrors
 * `status_is_waiting` in `apps/zed_based_desktop/crates/moonlight_ui/src/plan_review.rs`,
 * which is `WaitingInput` alone: `Running` is precisely what the server publishes when
 * a verdict resolves a hold.
 */
function statusIsWaiting(status) {
    return status === 'WaitingInput';
}
/**
 * Turn one decoded SSE `data:` payload into an event, or `undefined` when it is a
 * variant we do not model.
 *
 * Tolerant on purpose: an engine that adds a variant, or renames a field on one we
 * ignore, must not break the stream for the variants we do handle.
 */
function parseEngineEvent(json) {
    let parsed;
    try {
        parsed = JSON.parse(json);
    }
    catch {
        return undefined;
    }
    if (typeof parsed !== 'object' || parsed === null) {
        return undefined;
    }
    const entries = Object.entries(parsed);
    if (entries.length !== 1) {
        return undefined;
    }
    const [variant, rawBody] = entries[0];
    const body = (typeof rawBody === 'object' && rawBody !== null ? rawBody : {});
    const session = typeof body.session === 'string' ? body.session : undefined;
    if (!session) {
        return undefined;
    }
    switch (variant) {
        case 'PlanProposed':
            return typeof body.plan === 'string'
                ? { kind: 'PlanProposed', session, plan: body.plan }
                : undefined;
        case 'ApprovalRequested':
            return {
                kind: 'ApprovalRequested',
                session,
                what: typeof body.what === 'string' ? body.what : '',
                authorizeTool: typeof body.authorize_tool === 'string' ? body.authorize_tool : undefined,
            };
        case 'SessionStateChanged':
            return {
                kind: 'SessionStateChanged',
                session,
                status: typeof body.status === 'string' ? body.status : '',
            };
        case 'PhaseTransitioned':
            return {
                kind: 'PhaseTransitioned',
                session,
                phase: typeof body.phase === 'string' ? body.phase : '',
            };
        case 'SessionRemoved':
            return { kind: 'SessionRemoved', session };
        default:
            return undefined;
    }
}
/**
 * Split an SSE byte stream into frames, keeping whatever trailing partial frame the
 * socket has not finished sending.
 *
 * Written as a pure function over an accumulated buffer so the framing can be tested
 * without a server: the interesting cases (a frame split mid-line, a multi-line
 * `data:`) are exactly the ones a live test would only hit by luck.
 */
function readFrames(buffer) {
    const frames = [];
    const blocks = buffer.split('\n\n');
    // The last block has no terminator yet — it may be a whole frame whose blank line
    // is still in flight, so it stays in the buffer.
    const rest = blocks.pop() ?? '';
    for (const block of blocks) {
        let event = 'message';
        const data = [];
        for (const rawLine of block.split('\n')) {
            const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
            if (line.startsWith(':') || line.length === 0) {
                continue; // keep-alive comment
            }
            if (line.startsWith('event:')) {
                event = line.slice('event:'.length).trim();
            }
            else if (line.startsWith('data:')) {
                data.push(line.slice('data:'.length).trimStart());
            }
        }
        if (data.length > 0) {
            frames.push({ event, data: data.join('\n') });
        }
    }
    return { frames, rest };
}
/** How long to wait before reconnecting, growing to {@link MAX_RETRY_MS}. */
const BASE_RETRY_MS = 1_000;
const MAX_RETRY_MS = 30_000;
/**
 * Follow the engine event stream, reconnecting for as long as the subscription lives.
 *
 * Reconnection is the normal case, not an error path: the daemon is autostarted and
 * restarted under running windows, so a stream that gave up on the first failure
 * would leave the editor permanently blind after an ordinary daemon upgrade.
 */
function subscribeEvents(handlers) {
    let disposed = false;
    let retryMs = BASE_RETRY_MS;
    let request;
    let timer;
    const scheduleReconnect = (reason) => {
        if (disposed) {
            return;
        }
        handlers.onDesync(reason);
        timer = setTimeout(connect, retryMs);
        retryMs = Math.min(retryMs * 2, MAX_RETRY_MS);
    };
    const connect = () => {
        if (disposed) {
            return;
        }
        const base = (0, daemon_1.controlBaseUrl)();
        if (!base) {
            scheduleReconnect('no control API to stream from');
            return;
        }
        request = http.request(`${base}/control/events`, { method: 'GET', headers: { Accept: 'text/event-stream' } }, (res) => {
            const status = res.statusCode ?? 0;
            if (status < 200 || status >= 300) {
                res.resume();
                scheduleReconnect(`event stream → HTTP ${status}`);
                return;
            }
            // Connected: the next outage should retry promptly rather than inherit the
            // backoff this one earned.
            retryMs = BASE_RETRY_MS;
            res.setEncoding('utf8');
            let buffer = '';
            res.on('data', (chunk) => {
                buffer += chunk;
                const { frames, rest } = readFrames(buffer);
                buffer = rest;
                for (const frame of frames) {
                    if (frame.event === 'lagged') {
                        handlers.onDesync(`event stream lagged (${frame.data} missed)`);
                        continue;
                    }
                    const event = parseEngineEvent(frame.data);
                    if (event) {
                        handlers.onEvent(event);
                    }
                }
            });
            res.on('end', () => scheduleReconnect('event stream ended'));
            res.on('error', (err) => scheduleReconnect(err.message));
        });
        request.on('error', (err) => scheduleReconnect(err.message));
        request.end();
    };
    connect();
    return {
        dispose() {
            disposed = true;
            if (timer) {
                clearTimeout(timer);
            }
            request?.destroy();
        },
    };
}
//# sourceMappingURL=events.js.map