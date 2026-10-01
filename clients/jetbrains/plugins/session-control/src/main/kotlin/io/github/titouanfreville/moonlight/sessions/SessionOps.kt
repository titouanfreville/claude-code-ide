package io.github.titouanfreville.moonlight.sessions

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.ui.popup.PopupStep
import com.intellij.openapi.ui.popup.util.BaseListPopupStep
import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.client.shortId
import io.github.titouanfreville.moonlight.core.ActiveSession
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.SessionLaunches
import io.github.titouanfreville.moonlight.core.describePanelKey
import io.github.titouanfreville.moonlight.core.isInside
import io.github.titouanfreville.moonlight.core.projectFolders
import java.util.UUID

/**
 * The operator actions behind every Session Control surface — tool window rows, the
 * toolbar, the status widget's click, the palette. One implementation each, so a session
 * adopted from the tree and one adopted from the status bar go through the same words.
 *
 * Control-API calls are blocking, so each runs on a pooled thread; popups and dialogs run
 * on the EDT, which is where every entry point here is called from.
 */
object SessionOps {
    private val core: MoonlightApi get() = MoonlightApi.getInstance()

    /**
     * How long to wait for detection to notice a session we just launched, before telling
     * the operator it is ungoverned. A fresh session writes nothing until it is prompted.
     */
    private const val ADOPT_POLL_MS = 750L
    private const val ADOPT_ATTEMPTS = 20

    /** Adopt a session, from a row or a picker of the unadopted ones. */
    fun adopt(project: Project, session: DiscoverableSession?) {
        if (session != null) {
            doAdopt(project, session, linkPanel = false)
            return
        }
        val all = core.sessions()
        val candidates = all.filter { !it.adopted }
        if (candidates.isEmpty()) {
            info(
                project,
                if (all.isEmpty()) "No Claude Code sessions detected yet. Run `claude` somewhere and wait a few seconds."
                else "Every detected session is already adopted.",
            )
            return
        }
        pick(project, "Adopt a Claude Code session into MoonlightCode governance", candidates.map { s ->
            Choice(sessionLabel(s, candidates), s.root ?: s.sessionId, s)
        }) { doAdopt(project, it, linkPanel = true) }
    }

    private fun doAdopt(project: Project, session: DiscoverableSession, linkPanel: Boolean) = background(project) {
        core.control.adopt(session.sessionId)
        onEdt {
            // Adopting while an agent tab is in front is a statement about that tab: it is
            // the session you are working in. Recording it lets several tabs each track
            // their own session instead of sharing one project-wide guess.
            val linked = linkPanel && core.pinToActivePanel(project, session.sessionId)
            core.refresh()
            info(
                project,
                "Adopted \"${sessionLabel(session, core.sessions())}\" — it starts in Plan, where project writes are denied." +
                    (if (linked) " Linked to the tab in front of you — the status bar now follows it." else "") +
                    " Gating applies from its next tool call.",
            )
        }
    }

    /**
     * Change a session's phase. With no session named, the active one — clicking the phase
     * indicator should change *that* phase, not ask which session it was about.
     */
    fun setPhase(project: Project, session: DiscoverableSession?) {
        val target = session ?: activeAdopted(project)
        if (target != null) {
            pickPhase(project, target)
            return
        }
        pickAdopted(project, "Set the phase of which session?") { pickPhase(project, it) }
    }

    fun pickPhase(project: Project, session: DiscoverableSession) {
        val label = sessionLabel(session, core.sessions())
        pick(project, "Move \"$label\" from ${session.phase} to…", Phase.ORDERED.map { phase ->
            Choice(
                phase.name + if (phase == session.phase) " (current)" else "",
                if (phase.frozen) "project writes denied" else "project writes allowed",
                phase,
            )
        }) { phase ->
            background(project) {
                core.control.setPhase(session.sessionId, phase)
                core.refresh()
                info(project, "\"$label\" → ${phase.name}. The phase is pinned now: a manual pick overrides auto-advance.")
            }
        }
    }

    /** Advance to the next phase and return the session to auto (clears any pin). */
    fun advance(project: Project, session: DiscoverableSession?) {
        val target = session ?: activeAdopted(project)
        if (target == null) {
            pickAdopted(project, "Advance which session?") { advance(project, it) }
            return
        }
        background(project) {
            core.control.advancePhase(target.sessionId)
            core.refresh()
        }
    }

    /**
     * Say which session you are working in. Needed because Claude Code exposes no session
     * id: MoonlightCode knows for certain only the sessions it started, so rather than infer
     * and risk showing the wrong session's phase, the operator can say.
     *
     * Links to the agent-host tab in front (terminal or AI Chat) when there is one — the more
     * specific statement, and it keeps several tabs independent — else pins for the project.
     */
    fun setActive(project: Project, session: DiscoverableSession?) {
        if (session != null) {
            follow(project, session.sessionId)
            return
        }
        val all = core.sessions()
        val folders = projectFolders(project)
        // This project's sessions first — the likely answer — but never hide the rest: a
        // session can report a root that doesn't match how the project was opened.
        val here = all.filter { s -> s.root?.let { r -> folders.any { isInside(r, it) } } == true }
        val current = core.activeSession(project)?.sessionId
        val choices = (here + (all - here.toSet())).map { s ->
            Choice<String?>(
                (if (s.sessionId == current) "✓ " else "") + sessionLabel(s, all),
                "${if (s.adopted) s.phase.name else "not governed"} · ${s.status} · ${s.root ?: s.sessionId}",
                s.sessionId,
            )
        } + Choice<String?>("Clear pin", "fall back to detecting it automatically", null)
        pick(project, "Which session are you working in?", choices) { follow(project, it) }
    }

    private fun follow(project: Project, sessionId: String?) {
        val linked = core.pinToActivePanel(project, sessionId)
        if (!linked) core.setPinnedSession(project, sessionId)
        val tab = core.activePanelKey(project)?.let(::describePanelKey)
        info(
            project,
            when {
                sessionId != null && linked -> "Linked to $tab — switching tabs switches the session shown."
                sessionId != null -> "Pinned for this project — the status bar now shows its phase."
                linked -> "Link cleared for $tab."
                else -> "Pin cleared — the active session will be detected automatically."
            },
        )
    }

    /**
     * Start Claude Code in a terminal MoonlightCode owns, then adopt it.
     *
     * Starting a session through MoonlightCode *is* the decision to govern it. Adoption has
     * to wait for detection to see the session, though — the engine drops an adopt for a
     * session not yet in the fleet — so this waits for it rather than firing into the void.
     */
    fun startSession(project: Project) {
        val sessionId = UUID.randomUUID().toString()
        core.terminals.start(project, sessionId, project.basePath)
        object : Task.Backgroundable(project, "Starting session ${shortId(sessionId)} — waiting for detection…", true) {
            override fun run(indicator: ProgressIndicator) {
                repeat(ADOPT_ATTEMPTS) {
                    if (indicator.isCanceled) return
                    Thread.sleep(ADOPT_POLL_MS)
                    core.refresh().get()
                    // The launch, wherever it is now: the operator may `/resume` another
                    // conversation before the minted one ever writes a line.
                    val current = core.sessions().firstOrNull { it.isOrContinues(sessionId) } ?: return@repeat
                    try {
                        if (!current.adopted) core.control.adopt(current.sessionId)
                        core.refresh()
                        info(
                            project,
                            "Session ${shortId(sessionId)} started and adopted — governed from its next tool call, and reviews can be delivered to its terminal.",
                        )
                    } catch (e: Exception) {
                        warn(project, "Session started, but adopting it failed: ${e.message}. Use \"Adopt a Claude Code Session\" to retry.")
                    }
                    return
                }
                // Detection works off the transcript, which only appears once the session
                // writes something. Say so rather than implying the session is governed.
                warn(
                    project,
                    "Session ${shortId(sessionId)} started, but detection has not seen it yet, so it is NOT governed. " +
                        "It is detected once it writes its first transcript line — send it a prompt, then adopt it.",
                )
            }
        }.queue()
    }

    /**
     * After an automatic adoption: say what adoption did, not only that it happened. A
     * session gone quiet because its writes are denied looks like a broken agent unless the
     * message that governed it also says it is frozen and how to release it.
     */
    fun announceAutoAdopted(project: Project, session: DiscoverableSession) {
        NotificationGroupManager.getInstance().getNotificationGroup("MoonlightCode")
            .createNotification(
                "Adopted \"${sessionLabel(session, core.sessions())}\" automatically. It is on Plan, where project writes are denied.",
                NotificationType.INFORMATION,
            )
            // The session this toast is about, carried explicitly — re-resolving "the active
            // session" would quietly set the phase of a different one.
            .addAction(NotificationAction.createSimpleExpiring("Set phase…") { pickPhase(project, session) })
            .notify(project)
    }

    /** Whether "Resume Session" applies to a row: nothing of ours is running it, and it is not mid-turn. */
    fun canResume(session: DiscoverableSession): Boolean =
        core.terminals.get(session.sessionId) == null && session.status != SessionStatus.Running

    /**
     * Bring a session back in a terminal this window owns, on the conversation it was in.
     *
     * A launch this window remembers resumes as itself. Any other ended session gets a launch
     * of its own — but `claude --resume` beside a process still holding the conversation
     * forks it, so one the daemon does not report finished is confirmed first.
     */
    fun resume(project: Project, session: DiscoverableSession?) {
        val launches = SessionLaunches.of(project)
        if (session == null) {
            val resumable = launches.resumable()
            if (resumable.isEmpty()) {
                info(project, "Nothing to resume — no session launched in this window has ended.")
                return
            }
            pick(project, "Resume which session?", resumable.map { r ->
                Choice(
                    r.title ?: shortId(r.conversation ?: r.launchId),
                    (if (r.wasWorking) "was working · " else "") + (r.cwd ?: ""),
                    r,
                )
            }) { launches.resume(it, nudge = false).show() }
            return
        }
        core.terminals.get(session.sessionId)?.let {
            it.show()
            return
        }
        val remembered = launches.records().firstOrNull {
            it.conversation == session.sessionId || it.launchId == session.sessionId || it.launchId == session.launchId
        }
        if (remembered != null) {
            launches.resume(remembered, nudge = false).show()
            return
        }
        if (session.status != SessionStatus.Done && session.status != SessionStatus.Errored) {
            val go = Messages.showOkCancelDialog(
                project,
                "\"${session.title ?: shortId(session.sessionId)}\" is ${session.status} and may still be open in another terminal. " +
                    "Resuming it here starts a second Claude process on the same conversation.",
                "Resume Session",
                "Resume Here",
                Messages.getCancelButton(),
                Messages.getWarningIcon(),
            )
            if (go != Messages.OK) return
        }
        launches.resumeConversation(session.sessionId, session.root ?: project.basePath).show()
    }

    fun promptGroupName(project: Project, title: String, initial: String = ""): String? =
        Messages.showInputDialog(project, "Name for the session group", title, null, initial, object : com.intellij.openapi.ui.InputValidator {
            override fun checkInput(inputString: String?): Boolean = !inputString.isNullOrBlank()
            override fun canClose(inputString: String?): Boolean = checkInput(inputString)
        })?.trim()

    /**
     * Switch the view to custom grouping after the operator makes or fills a group. Without
     * this, a group made while grouping by project is filed somewhere the tree does not
     * render — which reads as the group not having been created.
     */
    fun ensureCustomMode(project: Project) {
        val settings = SessionControlSettings.of(project)
        if (settings.groupBy() != GroupBy.Custom) settings.setGroupBy(project, GroupBy.Custom)
    }

    private fun activeAdopted(project: Project): DiscoverableSession? {
        val active: ActiveSession = core.activeSession(project) ?: return null
        return core.sessions().firstOrNull { it.sessionId == active.sessionId && it.adopted }
    }

    private fun pickAdopted(project: Project, title: String, then: (DiscoverableSession) -> Unit) {
        val adopted = core.sessions().filter { it.adopted }
        if (adopted.isEmpty()) {
            info(project, "No adopted sessions — phase only governs sessions under MoonlightCode governance.")
            return
        }
        pick(project, title, adopted.map { s ->
            Choice(
                sessionLabel(s, adopted),
                "phase: ${s.phase}${if (s.phase.frozen) " (project writes DENIED)" else ""} · ${s.root ?: s.sessionId}",
                s,
            )
        }, then)
    }

    /** One row of a picker: what it says, the fact beside it, and the value it stands for. */
    data class Choice<T>(val label: String, val description: String, val value: T)

    fun <T> pick(project: Project, title: String, choices: List<Choice<T>>, then: (T) -> Unit) {
        val step = object : BaseListPopupStep<Choice<T>>(title, choices) {
            override fun getTextFor(value: Choice<T>): String = "${value.label}    —    ${value.description}"
            override fun onChosen(selectedValue: Choice<T>, finalChoice: Boolean): PopupStep<*>? =
                doFinalStep { then(selectedValue.value) }
        }
        JBPopupFactory.getInstance().createListPopup(step).showCenteredInCurrentWindow(project)
    }

    /** Run a control-API call off the EDT, reporting a failure where the operator asked. */
    fun background(project: Project, block: () -> Unit) {
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                block()
            } catch (e: Exception) {
                error(project, e.message ?: e.toString())
            }
        }
    }

    fun onEdt(action: () -> Unit) = ApplicationManager.getApplication().invokeLater(action)

    fun info(project: Project, message: String) = notify(project, message, NotificationType.INFORMATION)
    fun warn(project: Project, message: String) = notify(project, message, NotificationType.WARNING)
    fun error(project: Project, message: String) = notify(project, message, NotificationType.ERROR)

    private fun notify(project: Project, message: String, type: NotificationType) {
        NotificationGroupManager.getInstance().getNotificationGroup("MoonlightCode").createNotification(message, type).notify(project)
    }
}
