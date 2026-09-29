/**
 * Adopting new sessions without being asked.
 *
 * Adoption is what first lets the gate deny anything — an unadopted session is always
 * allowed — so a fleet governed only when someone remembers to click Adopt is governed
 * by accident. This closes that gap for the sessions belonging to this window.
 *
 * What it deliberately does not do is soften the landing. A freshly adopted session
 * sits in `Plan`, which is a frozen phase: project writes are denied until the
 * operator advances it. Adopting automatically therefore *stops* new sessions, and
 * that is the point — the alternative, adopting straight into a writing phase, grants
 * write access without the plan gate ever running, which is the check adoption exists
 * to impose. The notification has to say so, because a session that silently cannot
 * write reads as a broken agent rather than a governed one.
 *
 * The decision of which sessions to take is kept here as a pure function, so the rule
 * can be tested without a daemon or a window.
 */
import type { DiscoverableSession } from 'moonlight-control-client';

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
export function sessionsToAdopt(
  sessions: readonly DiscoverableSession[],
  owns: (sessionId: string) => boolean,
  attempted: ReadonlySet<string>
): readonly DiscoverableSession[] {
  return sessions.filter(
    (session) =>
      !session.adopted && !attempted.has(session.session_id) && owns(session.session_id)
  );
}

/**
 * Ids worth forgetting, so a session that is adopted and later released can be adopted
 * again rather than being ignored forever by a set that only grows.
 */
export function staleAttempts(
  attempted: ReadonlySet<string>,
  sessions: readonly DiscoverableSession[]
): readonly string[] {
  const live = new Set(sessions.filter((s) => !s.adopted).map((s) => s.session_id));
  return [...attempted].filter((id) => !live.has(id));
}
