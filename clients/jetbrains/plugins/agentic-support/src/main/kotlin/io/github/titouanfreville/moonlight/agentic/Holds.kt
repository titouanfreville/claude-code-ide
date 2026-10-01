package io.github.titouanfreville.moonlight.agentic

import com.intellij.icons.AllIcons
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.startup.ProjectActivity
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.ui.popup.PopupStep
import com.intellij.openapi.ui.popup.util.BaseListPopupStep
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.client.shortId
import io.github.titouanfreville.moonlight.core.HeldApproval
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightDataKeys
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.core.MoonlightService

/**
 * Held approvals: telling the operator a session is blocked, and taking their answer — the
 * counterpart of `moonlight-agentic-support/src/extension.ts`.
 *
 * A held session is *stopped* until someone answers. A bounded hold with no answer resolves
 * to a deny; an unbounded one (what the daemon uses when a client is expected) waits
 * forever. Both look to the operator like an agent that mysteriously stopped working — so
 * a hold is announced where it will be seen, and stays announced until it is answered.
 */
object Holds {
    private val core: MoonlightApi get() = MoonlightApi.getInstance()

    /** A plan goes to the review tab; any other hold is one question, answered in place. */
    fun answer(project: Project, hold: HeldApproval) {
        if (isPlanHold(hold)) {
            PlanReviews.of(project).open(hold.sessionId, focus = true)
            return
        }
        val choice = Messages.showDialog(
            project,
            "${describe(hold)}\n\nAllow it, or refuse and tell the agent why?",
            "MoonlightCode — ${who(hold.sessionId)} is held",
            arrayOf("Allow", "Refuse…", "Not Now"),
            2,
            AllIcons.General.WarningDialog,
        )
        when (choice) {
            0 -> applyActionVerdict(project, hold, allow = true)
            1 -> applyActionVerdict(project, hold, allow = false)
            // Not answering is not an answer: the hold stays open.
            else -> Unit
        }
    }

    /**
     * Refusing asks for a reason first. The reason is the hook's deny message — the only
     * thing the session learns from being refused — so a silent "no" leaves it to guess what
     * it did wrong and try something similar.
     */
    fun applyActionVerdict(project: Project, hold: HeldApproval, allow: Boolean) {
        var reason = "Refused by the operator."
        if (!allow) {
            val typed = Messages.showInputDialog(
                project,
                "Why is this refused? The agent is told, and acts on it.",
                "Refuse — ${describe(hold)}",
                null,
                reason,
                null,
            ) ?: return // cancelled the refusal; the hold stays open
            reason = typed.trim().ifEmpty { reason }
        }
        ApplicationManager.getApplication().executeOnPooledThread {
            when (val outcome = decideAction(core.control, hold.sessionId, allow, reason)) {
                is GateOutcome.Ok -> info(project, outcome.summary)
                is GateOutcome.Failed -> error(project, "The verdict did not reach the session (${outcome.error}). It is still waiting.")
            }
            core.refresh()
        }
    }

    /** Answer whichever hold is outstanding — on demand, rather than from its announcement. */
    fun answerAny(project: Project) {
        val holds = core.heldApprovals()
        when (holds.size) {
            0 -> info(project, "Nothing is waiting on you.")
            1 -> answer(project, holds[0])
            else -> {
                val step = object : BaseListPopupStep<HeldApproval>("Which hold do you want to answer?", holds) {
                    override fun getTextFor(value: HeldApproval): String =
                        "${who(value.sessionId)} — ${describe(value)}    (waiting ${waited(value)})"

                    override fun onChosen(selectedValue: HeldApproval, finalChoice: Boolean): PopupStep<*>? =
                        doFinalStep { answer(project, selectedValue) }
                }
                JBPopupFactory.getInstance().createListPopup(step).showCenteredInCurrentWindow(project)
            }
        }
    }

    fun describe(hold: HeldApproval): String = hold.what + (hold.mcpTool?.let { " ($it)" } ?: "")

    fun who(sessionId: String): String {
        val all = core.sessions()
        return all.firstOrNull { it.sessionId == sessionId }?.let { sessionLabel(it, all) } ?: shortId(sessionId)
    }

    fun waited(hold: HeldApproval): String {
        val s = ((System.currentTimeMillis() - hold.sinceMs) / 1000).coerceAtLeast(0)
        return if (s < 120) "${s}s" else "${s / 60}m"
    }

    fun info(project: Project, message: String) = notify(project, message, NotificationType.INFORMATION)
    fun error(project: Project, message: String) = notify(project, message, NotificationType.ERROR)

    private fun notify(project: Project, message: String, type: NotificationType) {
        NotificationGroupManager.getInstance().getNotificationGroup("MoonlightCode").createNotification(message, type).notify(project)
    }
}

/**
 * Announces new holds in this window — if this is the window that should.
 *
 * Exactly one window announces each hold (see [announcesHere]): the owner, else the focused
 * one. The announcement is a sticky notification, expired the moment the hold is answered
 * from anywhere — this window, another IDE, the cockpit — so nothing lingers offering a
 * verdict on a hook nobody holds.
 */
@Service(Service.Level.PROJECT)
class HoldAnnouncer(private val project: Project) : Disposable {
    private val announced = HashMap<String, Notification>()

    fun start() {
        ApplicationManager.getApplication().messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun holdsChanged() = onHolds()
            // Which window owns a session can change with the session list.
            override fun sessionsChanged() = onHolds()
        })
        onHolds()
    }

    private fun onHolds() {
        if (project.isDisposed) return
        val core = MoonlightApi.getInstance()
        val holds = core.heldApprovals()
        val live = holds.associateBy { it.sessionId }

        // Answered — here or anywhere else. Forgotten too, so the same session holding again
        // is announced again rather than mistaken for the notification still open.
        for ((sessionId, notification) in announced.entries.toList()) {
            val hold = live[sessionId]
            if (hold == null) {
                notification.expire()
                announced.remove(sessionId)
            }
        }

        val focused = MoonlightService.getInstance().focusedProject()
        val firstOpen = ProjectManager.getInstance().openProjects.firstOrNull { !it.isDisposed }
        for (hold in holds) {
            if (hold.sessionId in announced) continue
            if (!announcesHere(project, core.owningProject(hold.sessionId), focused, firstOpen)) continue
            announced[hold.sessionId] = announce(hold)
        }
    }

    private fun announce(hold: HeldApproval): Notification {
        val group = NotificationGroupManager.getInstance().getNotificationGroup("MoonlightCode Holds")
        val who = Holds.who(hold.sessionId)
        if (isPlanHold(hold)) {
            // The review opens by itself — without stealing focus from whatever is being
            // typed — and the notification says why a tab just appeared.
            PlanReviews.of(project).open(hold.sessionId, focus = false)
            return group.createNotification("$who is waiting on a plan review.", NotificationType.WARNING)
                .setTitle("MoonlightCode")
                .addAction(NotificationAction.createSimple("Review plan") { PlanReviews.of(project).open(hold.sessionId, focus = true) })
                .also { it.notify(project) }
        }
        return group.createNotification("$who is held — ${Holds.describe(hold)}", NotificationType.WARNING)
            .setTitle("MoonlightCode")
            .addAction(NotificationAction.createSimpleExpiring("Allow") { Holds.applyActionVerdict(project, hold, allow = true) })
            .addAction(NotificationAction.createSimpleExpiring("Refuse…") { Holds.applyActionVerdict(project, hold, allow = false) })
            .also { it.notify(project) }
    }

    override fun dispose() {
        announced.values.forEach(Notification::expire)
        announced.clear()
    }
}

class AgenticStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        ApplicationManager.getApplication().invokeLater({ project.service<HoldAnnouncer>().start() }, project.disposed)
    }
}

/**
 * Answer the hold of the session in front of you — the selected row when there is one —
 * else whichever is blocked. The id Session Control's row double-click invokes.
 */
class AnswerSessionHoldAction : AnAction(), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.EDT

    override fun update(e: AnActionEvent) {
        e.presentation.isEnabledAndVisible = e.project != null
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val core = MoonlightApi.getInstance()
        val sessionId = e.getData(MoonlightDataKeys.SESSION_ID)
        val hold = sessionId?.let(core::heldApproval)
        when {
            hold != null -> Holds.answer(project, hold)
            // A named session with no hold but a plan: show it, for reading.
            sessionId != null && core.proposedPlan(sessionId) != null -> PlanReviews.of(project).open(sessionId, focus = true)
            else -> Holds.answerAny(project)
        }
    }
}

class AnswerAnyHoldAction : AnAction(), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.EDT

    override fun actionPerformed(e: AnActionEvent) {
        Holds.answerAny(e.project ?: return)
    }
}

/** Open the plan of the session you are in — or the blocked one — whether or not it is held. */
class OpenPlanAction : AnAction(), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.EDT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val core = MoonlightApi.getInstance()
        val sessionId = e.getData(MoonlightDataKeys.SESSION_ID)
            ?: core.heldApprovals().firstOrNull(::isPlanHold)?.sessionId
            ?: core.activeSession(project)?.sessionId
        if (sessionId == null) {
            Holds.info(project, "No session to show a plan for — pick one in the MoonlightCode tool window.")
            return
        }
        PlanReviews.of(project).open(sessionId, focus = true)
    }
}
