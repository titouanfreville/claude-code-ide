package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.util.messages.Topic
import io.github.titouanfreville.moonlight.client.ControlApi
import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.HookStatusEntry
import io.github.titouanfreville.moonlight.client.SessionUsage
import io.github.titouanfreville.moonlight.client.UsageResponse
import java.util.concurrent.CompletableFuture

/**
 * What `moonlight-core` offers the other MoonlightCode plugins — the JetBrains counterpart
 * of `api.ts`.
 *
 * Some state cannot be duplicated across the feature plugins: the terminals we started
 * (the review plugin delivers into a terminal session control launched), and the fleet's
 * status (the review surface must not believe a session idle while the status bar shows it
 * working). So core owns the backend connection, one poll, one event stream and the
 * terminal registry, and every feature plugin `<depends>` on it.
 *
 * Unlike VS Code there is one instance for the whole IDE, not one per window: every project
 * window lives in this process. Anything about "this window" therefore takes the
 * [Project].
 *
 * Change events arrive on [MoonlightListener.TOPIC], always on the EDT.
 */
interface MoonlightApi {
    /**
     * Bumped when members are added. A dependent checks it as a **minimum**, and members
     * added later carry default implementations, so a newer dependent still runs against an
     * older core — it feature-detects rather than demanding a version.
     */
    val version: Int

    /** The control-API client, shared so every plugin talks to the same daemon the same way. */
    val control: ControlApi

    /** Last known sessions, from the shared poll. */
    fun sessions(): List<DiscoverableSession>

    /** Last known hook-gating status, from the same poll. */
    fun gating(): List<HookStatusEntry>

    /**
     * Last known Claude usage, or `null` until the first successful read. A caller renders
     * both "null" and a null figure inside it as unknown — never as 0, which reads as
     * headroom.
     */
    fun usage(): UsageResponse?

    /** Usage for one session, or `null` when it has no statusline snapshot. */
    fun sessionUsage(sessionId: String): SessionUsage? = usage()?.sessions?.firstOrNull { it.sessionId == sessionId }

    /**
     * Why the backend is unreachable, or `null` when it is fine. Surfaced rather than
     * thrown: a missing daemon is an expected state, not an error.
     */
    fun backendError(): String?

    /** Refresh now instead of waiting for the next tick. Completes after the read. */
    fun refresh(): CompletableFuture<Unit>

    /** The hold outstanding for a session, or `null` when it is not blocked. */
    fun heldApproval(sessionId: String): HeldApproval?

    /** Every outstanding hold, longest-waiting first. */
    fun heldApprovals(): List<HeldApproval>

    /**
     * The plan a session last proposed, whether or not it is currently held. Supplies text
     * only — `PlanProposed` also fires for sessions that finished hours ago, so only a hold
     * arms a verdict.
     */
    fun proposedPlan(sessionId: String): String?

    val terminals: OwnedTerminals

    /**
     * The session the operator is working in, in this window, or `null` when it cannot be
     * told apart from the others. Ambiguity is reported, never guessed away.
     */
    fun activeSession(project: Project): ActiveSession?

    /** Pin a session for the whole project, or clear it with `null`. */
    fun setPinnedSession(project: Project, sessionId: String?)

    /** The key of the agent-host tab in front of the operator — see [panelKey]. */
    fun activePanelKey(project: Project): String?

    /**
     * Link a session to the agent-host tab in front of the operator. Returns false when no
     * agent host is in front.
     */
    fun pinToActivePanel(project: Project, sessionId: String?): Boolean

    /**
     * Whether `project` is the one window that should act on a session unprompted.
     *
     * Exactly one open window answers true per session (see [pickOwner]), so a hold opens
     * one plan panel, not one per window. A session no window claims answers false
     * everywhere; surfaces still *announce* such a hold, because a held agent nobody is told
     * about is the one outcome worth avoiding entirely.
     */
    fun ownsSession(project: Project, sessionId: String): Boolean = owningProject(sessionId) == project

    /**
     * The one open project window that acts on a session unprompted, or `null` when no
     * window claims it — its root is open nowhere and nobody is working in it. A surface
     * with nothing to act on still has to *announce* such a session's hold somewhere: a held
     * agent nobody is told about is the one outcome worth avoiding entirely.
     */
    fun owningProject(sessionId: String): Project?

    companion object {
        /** The oldest API version whose members every dependent may rely on. */
        const val VERSION: Int = 1

        fun getInstance(): MoonlightApi = ApplicationManager.getApplication().service<MoonlightService>()
    }
}

/**
 * Which session the operator is working in, and how confident we are.
 *
 * Claude Code exposes no session id to other plugins, so for a session in an agent panel
 * there is nothing to read. Rather than infer one and risk labelling the wrong session as
 * governed, the confidence travels with the answer.
 */
data class ActiveSession(val sessionId: String, val how: How) {
    enum class How {
        /** Linked to the agent-host tab in front of you (a terminal or AI Chat tab). Most specific. */
        Panel,

        /** MoonlightCode launched it, so the id is certain. */
        Owned,

        /** The operator pinned it for the whole project. */
        Pinned,

        /** The only session running in this project — a good guess, no more. */
        Sole,
    }
}

/**
 * A session stopped at the gate, waiting for an operator.
 *
 * Folded from the event stream and seeded from `/control/pending-approvals`, because
 * neither alone is enough: the stream announces a hold once, and the endpoint is a
 * snapshot.
 */
data class HeldApproval(
    val sessionId: String,
    /** What is being asked, in the gate's own words. */
    val what: String,
    /** The proposed plan markdown, when the hold is a plan proposal. */
    val plan: String?,
    /** The full `mcp__server__tool` name, when a frozen phase held an external tool. */
    val mcpTool: String?,
    /** When the hold started (epoch ms). */
    val sinceMs: Long,
)

/** A terminal MoonlightCode started for a session — the only place we can type a message. */
interface OwnedTerminal {
    val project: Project
    val name: String

    /** False once the terminal is closed; a closed terminal receives nothing. */
    val isAlive: Boolean

    /** Type one line and press Enter. */
    fun sendLine(text: String)

    fun show()
}

/**
 * Terminals this plugin family started, by session id.
 *
 * Only terminals we created are here. Finding the terminal hosting a session we did not
 * start would mean matching process trees, and a wrong match types a review into an
 * unrelated shell — so this only claims what it knows.
 */
interface OwnedTerminals {
    fun get(sessionId: String): OwnedTerminal?
    fun has(sessionId: String): Boolean = get(sessionId) != null

    /**
     * Start Claude Code for `sessionId` in a new terminal tab of `project`, and register it.
     * Call on the EDT.
     */
    fun start(project: Project, sessionId: String, cwd: String?): OwnedTerminal

    /** Register a terminal as a session's host, on the operator's say-so or because we launched it. */
    fun adopt(sessionId: String, terminal: OwnedTerminal)

    /** Sessions whose owned terminal lives in `project`. */
    fun ownedIn(project: Project): Set<String>
}

/**
 * Data keys the MoonlightCode plugins share. Defined in core so a surface in one plugin
 * (a session row) can name its subject to an action in another (answer this hold).
 */
object MoonlightDataKeys {
    /** The session a UI element is about — a selected row, for instance. */
    @JvmField
    val SESSION_ID: com.intellij.openapi.actionSystem.DataKey<String> =
        com.intellij.openapi.actionSystem.DataKey.create("moonlight.sessionId")
}

/** Change notifications from core. Delivered on the EDT. */
interface MoonlightListener {
    /** Sessions, gating, usage or backend reachability moved — or which session is active did. */
    fun sessionsChanged() {}

    /**
     * A hold appeared, changed, or was answered — including by another client. Separate
     * from [sessionsChanged] so a surface that only cares about the gate is not woken by
     * every poll.
     */
    fun holdsChanged() {}

    /**
     * A session's attention overlay changed — `alert` is the `AttentionKind` name
     * (`Incomplete`: a turn stalled mid-work), or `null` when it cleared.
     */
    fun sessionAlert(sessionId: String, alert: String?) {}

    companion object {
        @JvmField
        @Topic.AppLevel
        val TOPIC: Topic<MoonlightListener> = Topic(MoonlightListener::class.java, Topic.BroadcastDirection.NONE)
    }
}
