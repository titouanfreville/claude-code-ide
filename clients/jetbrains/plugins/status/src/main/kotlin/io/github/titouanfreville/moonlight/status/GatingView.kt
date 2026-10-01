package io.github.titouanfreville.moonlight.status

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.HookStatusEntry
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.core.ActiveSession
import io.github.titouanfreville.moonlight.core.describePanelKey
import io.github.titouanfreville.moonlight.core.isInside

/**
 * What the gating indicator says — the at-a-glance answer to "is anything actually
 * governed?". A port of the render in `moonlight-status/src/extension.ts`.
 *
 * This is the fail-open visibility problem the whole client family started from: the
 * hooks can be uninstalled, or the backend can be down, and Claude Code carries on with
 * nothing gating it. Silence looks identical to safety, so this says which one you are in.
 *
 * Pure, so every state is testable; the widget only draws what this returns.
 */

enum class GatingIcon { NoBackend, Unlocked, Question, Frozen, Governed }

/**
 * What clicking the indicator does. It does the thing the bar is complaining about, so it
 * is chosen per state rather than fixed.
 */
enum class GatingAction(val actionId: String?) {
    SetPhase("Moonlight.SessionControl.SetPhase"),
    Adopt("Moonlight.SessionControl.Adopt"),
    SetActive("Moonlight.SessionControl.SetActive"),
    Refresh(null),
}

data class GatingView(
    val icon: GatingIcon,
    val text: String,
    /** Draw on the warning background. */
    val warn: Boolean,
    val tooltip: List<String>,
    val action: GatingAction,
)

fun gatingView(
    backendError: String?,
    hooks: List<HookStatusEntry>,
    sessions: List<DiscoverableSession>,
    active: ActiveSession?,
    panelKey: String?,
    folders: List<String>,
): GatingView {
    if (backendError != null) {
        return GatingView(
            GatingIcon.NoBackend,
            "MoonlightCode",
            warn = true,
            tooltip = listOf("No MoonlightCode backend reachable, so nothing is gated.", backendError, "", "Click to retry now."),
            action = GatingAction.Refresh,
        )
    }

    val hooksOn = hooks.isNotEmpty() && hooks.all { it.installed }
    val session = active?.let { a -> sessions.firstOrNull { it.sessionId == a.sessionId } }
    val view = when {
        // Necessary but not sufficient — and the most misleading thing this bar can say,
        // because a phase means nothing without hooks to enforce it.
        !hooksOn -> GatingView(
            GatingIcon.Unlocked, "Not gated", true,
            listOf("Hooks are NOT installed — no session is gated, whatever its phase.", "Run `moonlightd hooks install`."),
            GatingAction.SetPhase,
        )

        // Nothing is adopted anywhere, so "which session?" would be the wrong question.
        sessions.none { it.adopted } -> GatingView(
            GatingIcon.Unlocked, "No adopted session", true,
            listOf(
                "Hooks are installed, but no session is adopted — so nothing is gated.",
                "",
                "An unadopted session is never denied anything, whatever phase it reports.",
                "Click here to adopt one.",
            ),
            GatingAction.Adopt,
        )

        session == null -> GatingView(
            GatingIcon.Question, "Which session?", false,
            listOf(
                "Sessions are adopted, but MoonlightCode cannot tell which one you are working in.",
                "",
                "Claude Code does not expose a session id to other plugins, so this is only certain",
                "for a session MoonlightCode started or one you linked to a terminal or AI Chat tab.",
                "Click here to say which it is.",
            ),
            GatingAction.SetActive,
        )

        // The trap: hooks on, so the bar could read "gated", while this session never is.
        !session.adopted -> GatingView(
            GatingIcon.Unlocked, "Ungoverned", true,
            listOf(
                "\"${session.title ?: session.sessionId}\" is NOT adopted, so it is never gated.",
                "Click here to adopt it.",
            ),
            GatingAction.Adopt,
        )

        else -> governedView(session, sessions, active, folders)
    }

    return view.copy(tooltip = view.tooltip + identifiedBy(active, panelKey))
}

private fun governedView(
    session: DiscoverableSession,
    sessions: List<DiscoverableSession>,
    active: ActiveSession?,
    folders: List<String>,
): GatingView {
    val frozen = session.phase.frozen
    // Another session here is mid-turn while the one we report on is not — positive
    // evidence we are describing the wrong session. "Plan" shown for a governed session you
    // are not talking to reads as "this conversation is governed", which is the opposite of
    // the truth when the one answering you is unadopted.
    val busyElsewhere = workingElsewhere(sessions, session, folders)
    if (busyElsewhere.isNotEmpty()) {
        return GatingView(
            GatingIcon.Question, "${session.phase} · which session?", true,
            listOf(
                "Showing \"${session.title ?: session.sessionId}\" (${session.status}), but",
                "${busyElsewhere.size} other session(s) here are mid-turn:",
            ) + busyElsewhere.map { s ->
                "  • ${sessionLabel(s, sessions)} — ${s.status}${if (s.adopted) "" else ", NOT adopted"}"
            } + listOf(
                "",
                "The phase above may describe a session you are not talking to. Click to say which one you are in.",
            ),
            GatingAction.SetActive,
        )
    }
    // The phase *is* the headline: it decides whether the agent in front of you can write.
    // A guess is labelled in the bar itself, not only in a tooltip nobody hovers.
    val guessed = active?.how == ActiveSession.How.Sole
    return GatingView(
        if (frozen) GatingIcon.Frozen else GatingIcon.Governed,
        if (guessed) "${session.phase} · guess" else "${session.phase}",
        warn = frozen,
        tooltip = listOf(
            "\"${session.title ?: session.sessionId}\"",
            if (frozen) "${session.phase} — project writes DENIED (AI-workspace dirs stay writable)"
            else "${session.phase} — project writes allowed",
            "status: ${session.status}",
            "",
            "Click to change phase.",
        ),
        action = GatingAction.SetPhase,
    )
}

/** How the session was identified, so a guess never reads as certainty. */
private fun identifiedBy(active: ActiveSession?, panelKey: String?): List<String> {
    active ?: return emptyList()
    val how = when (active.how) {
        ActiveSession.How.Panel ->
            "Session identified: linked to ${panelKey?.let(::describePanelKey) ?: "the tab in front of you"}."
        ActiveSession.How.Owned -> "Session identified: MoonlightCode started it."
        ActiveSession.How.Pinned -> "Session identified: pinned for this project."
        ActiveSession.How.Sole -> "Session identified: only one running here — a guess. Pin one to be sure."
    }
    return listOf("", how)
}

/** Sessions in this project that are mid-turn while `shown` is not. */
fun workingElsewhere(
    sessions: List<DiscoverableSession>,
    shown: DiscoverableSession,
    folders: List<String>,
): List<DiscoverableSession> {
    if (shown.status == SessionStatus.Running) return emptyList()
    return sessions.filter { s ->
        s.sessionId != shown.sessionId &&
            s.status == SessionStatus.Running &&
            s.root?.let { root -> folders.any { isInside(root, it) } } == true
    }
}
