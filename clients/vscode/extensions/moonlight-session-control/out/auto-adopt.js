"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.sessionsToAdopt = sessionsToAdopt;
exports.staleAttempts = staleAttempts;
/**
 * The sessions this window should adopt now.
 *
 * `owns` is `MoonlightApi.ownsSession`. Scoping by it is not a nicety: every open
 * window polls the same daemon and sees the same sessions, so an unscoped rule has
 * every window racing to adopt every session, including ones from projects it has
 * nothing to do with — the same fleet-wide-state bug that had the plan panel opening
 * everywhere at once.
 *
 * `attempted` carries the ids already tried. A session that failed to adopt — or that
 * the daemon has not yet reported back as adopted — must not be retried on every poll,
 * which for a five-second loop would be a request storm against a failing endpoint.
 */
function sessionsToAdopt(sessions, owns, attempted) {
    return sessions.filter((session) => !session.adopted && !attempted.has(session.session_id) && owns(session.session_id));
}
/**
 * Ids worth forgetting, so a session that is adopted and later released can be adopted
 * again rather than being ignored forever by a set that only grows.
 */
function staleAttempts(attempted, sessions) {
    const live = new Set(sessions.filter((s) => !s.adopted).map((s) => s.session_id));
    return [...attempted].filter((id) => !live.has(id));
}
//# sourceMappingURL=auto-adopt.js.map