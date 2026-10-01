package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.core.HeldApproval

/**
 * The row vocabulary of the session lists — the parts of `sessions-view.ts` that decide
 * what a row says, kept pure so the rules are tested rather than eyeballed.
 *
 * A row is `[icon] label   description   [badge]`. More facts than slots, so each slot
 * carries exactly one thing and everything else goes to the tooltip.
 */

/** Mirrors `SessionStatus::triage_rank`: blocked → errored → review-ready → active → idle. */
fun triageRank(status: SessionStatus): Int = when (status) {
    SessionStatus.WaitingInput -> 0
    SessionStatus.Errored -> 1
    SessionStatus.Done -> 2
    SessionStatus.Running -> 3
    SessionStatus.Idle -> 4
    SessionStatus.Paused -> 5
}

/** Mirrors `SessionStatus::badge` — the cockpit's glyph vocabulary. */
fun statusGlyph(status: SessionStatus): String = when (status) {
    SessionStatus.Running -> "●"
    SessionStatus.WaitingInput -> "◐"
    SessionStatus.Done -> "✓"
    SessionStatus.Errored -> "✕"
    SessionStatus.Idle -> "○"
    SessionStatus.Paused -> "‖"
}

/** Plain words for the status, for the one slot that has room for words. */
fun statusWords(status: SessionStatus): String = when (status) {
    SessionStatus.WaitingInput -> "waiting for input"
    SessionStatus.Running -> "working"
    SessionStatus.Done -> "finished"
    SessionStatus.Errored -> "errored"
    SessionStatus.Paused -> "paused"
    SessionStatus.Idle -> "idle"
}

/** The sessions a list shows, worst first. */
fun sessionsFor(scope: Scope, sessions: List<DiscoverableSession>): List<DiscoverableSession> =
    sessions.filter { if (scope == Scope.Governed) it.adopted else !it.adopted }.sortedBy { triageRank(it.status) }

/**
 * The description: the one fact you would act on.
 *
 * Governed rows lead with the phase — the thing an operator changes — and a frozen phase
 * says so in words, since "Plan" does not look like "cannot write files" to anyone who has
 * not read the docs. Ungoverned rows lead with the absence of a gate, stated as a
 * consequence: "not gated" tells you what is true, "unadopted" only names our bookkeeping.
 */
fun describe(session: DiscoverableSession, held: HeldApproval?): String = when {
    !session.adopted -> "not gated · ${statusWords(session.status)}"
    held != null -> if (held.plan != null) "PLAN WAITING ON YOU" else "WAITING ON YOU"
    session.phase.frozen -> "${session.phase} · writes denied"
    else -> "${session.phase} · ${statusWords(session.status)}"
}

enum class BadgeTone { Warning, Info, Error, Muted, Normal }

data class Badge(val text: String, val tone: BadgeTone)

/**
 * The badge at the end of a row. One glyph and a strict priority, because three things
 * want it: a hold beats an unreviewed count beats the resting status — "what would make
 * you click this row".
 */
fun badge(session: DiscoverableSession, held: Boolean): Badge = when {
    held -> Badge("!", BadgeTone.Warning)
    // Saturates rather than growing: a wide badge pushes the description off the row.
    session.unreviewedFiles > 0 -> Badge(if (session.unreviewedFiles > 99) "99+" else session.unreviewedFiles.toString(), BadgeTone.Info)
    !session.adopted -> Badge(statusGlyph(session.status), BadgeTone.Muted)
    session.status == SessionStatus.Errored -> Badge(statusGlyph(session.status), BadgeTone.Error)
    else -> Badge(statusGlyph(session.status), BadgeTone.Normal)
}
