package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession

/**
 * Adopting new sessions without being asked — a port of `auto-adopt.ts`.
 *
 * Adoption is what first lets the gate deny anything, so a fleet governed only when
 * someone remembers to click Adopt is governed by accident. What this deliberately does not
 * do is soften the landing: a freshly adopted session sits in `Plan`, which denies project
 * writes until the operator advances it. Adopting straight into a writing phase would grant
 * write access without the plan gate ever running.
 */

/**
 * The sessions this window should adopt now.
 *
 * Scoped by `owns` because every window sees the whole fleet: unscoped, every window would
 * race to adopt every session. `attempted` carries ids already tried — a failing adopt
 * retried on a five-second poll is a request storm.
 */
fun sessionsToAdopt(
    sessions: List<DiscoverableSession>,
    owns: (String) -> Boolean,
    attempted: Set<String>,
): List<DiscoverableSession> = sessions.filter { !it.adopted && it.sessionId !in attempted && owns(it.sessionId) }

/**
 * Ids worth forgetting, so a session adopted and later released can be adopted again
 * rather than being ignored forever by a set that only grows.
 */
fun staleAttempts(attempted: Set<String>, sessions: List<DiscoverableSession>): List<String> {
    val live = sessions.filter { !it.adopted }.mapTo(HashSet()) { it.sessionId }
    return attempted.filter { it !in live }
}
