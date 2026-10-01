package io.github.titouanfreville.moonlight.core

import com.intellij.ide.AppLifecycleListener
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectCloseListener
import com.intellij.util.concurrency.AppExecutorUtil
import io.github.titouanfreville.moonlight.client.shortId
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

/**
 * The Claude sessions this project window launched, remembered across IDE restarts, and
 * brought back when they go away — the IDE side of the desktop app's auto-resume.
 *
 * Two ways a session goes away:
 * - **The IDE closes.** Every terminal dies with it. On the next project open the launches
 *   that were running come back on the conversation each was in ([AutoResumeMode] says
 *   whether to ask first).
 * - **Claude exits.** Mid-work it is a crash, relaunched under `Automatic`; at the prompt it
 *   is the operator quitting, only offered back. See [decideOnExit].
 *
 * A relaunch keeps the **launch id**, so the daemon's launch registry, the MCP endpoint and
 * the phase carry over. A resumed session that was mid-work is nudged once to continue.
 *
 * Stored in the workspace file: which sessions this checkout was running is nobody else's.
 */
@Service(Service.Level.PROJECT)
@State(name = "MoonlightLaunches", storages = [Storage(StoragePathMacros.WORKSPACE_FILE)])
class SessionLaunches(private val project: Project) :
    SimplePersistentStateComponent<SessionLaunches.LaunchState>(LaunchState()), Disposable {

    class LaunchState : BaseState() {
        /** Ask by default: a restart offers its sessions back without acting on its own. */
        var mode by enum(AutoResumeMode.Ask)

        /** Launch id → [LaunchRecord.encode]. */
        var launches by map<String, String>()
    }

    private val core: MoonlightService get() = MoonlightService.getInstance()

    /** Relaunches after exit, per launch — capped by [MAX_EXIT_RESUMES]. Not persisted: a restart is a fresh start. */
    private val exitResumes = ConcurrentHashMap<String, Int>()

    /** Resumed launches still owed a nudge, if they turn out to have stalled. */
    private val armed = ConcurrentHashMap.newKeySet<String>()

    /** Set as the window or the IDE closes: terminals dying then is not the operator's doing. */
    @Volatile
    private var closing = false

    private var restored = false

    fun mode(): AutoResumeMode = state.mode

    fun setMode(mode: AutoResumeMode) {
        state.mode = mode
    }

    fun records(): List<LaunchRecord> =
        state.launches.mapNotNull { (id, raw) -> LaunchRecord.decode(id, raw) }

    fun record(launchId: String): LaunchRecord? = state.launches[launchId]?.let { LaunchRecord.decode(launchId, it) }

    /** Launches remembered here with no live terminal — what "Resume" can bring back. */
    fun resumable(): List<LaunchRecord> = records().filter { core.terminals.live(it.launchId) == null }

    private fun save(record: LaunchRecord) {
        if (state.launches[record.launchId] == record.encode()) return
        // A fresh map: assignment is what the state tracks as a change.
        state.launches = state.launches.toMutableMap().apply { put(record.launchId, record.encode()) }
    }

    fun forget(launchId: String) {
        if (launchId !in state.launches) return
        state.launches = state.launches.toMutableMap().apply { remove(launchId) }
        armed.remove(launchId)
        exitResumes.remove(launchId)
    }

    fun start() {
        val app = ApplicationManager.getApplication()
        app.messageBus.connect(this).subscribe(ProjectCloseListener.TOPIC, object : ProjectCloseListener {
            override fun projectClosing(project: Project) {
                if (project == this@SessionLaunches.project) closing = true
            }
        })
        app.messageBus.connect(this).subscribe(AppLifecycleListener.TOPIC, object : AppLifecycleListener {
            override fun appWillBeClosed(isRestart: Boolean) {
                closing = true
            }
        })
        app.messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun sessionsChanged() = follow()
            override fun sessionAlert(sessionId: String, alert: String?) = alerted(sessionId, alert)
        })
        restoreOnOpen()
    }

    // ---- Launching -----------------------------------------------------------------

    /** Start a fresh session pinned to `launchId`, and remember it. Call on the EDT. */
    fun launch(launchId: String, cwd: String?): OwnedTerminal {
        save(LaunchRecord(launchId, cwd, null, null, null))
        return open(launchId, cwd, null)
    }

    /**
     * Bring a remembered launch back on the conversation it was in. `nudge` arms the one-shot
     * continue prompt, for a session that stopped mid-work. Call on the EDT.
     */
    fun resume(record: LaunchRecord, nudge: Boolean = record.wasWorking): OwnedTerminal {
        core.terminals.live(record.launchId)?.let { return it }
        if (nudge) armed += record.launchId
        val terminal = open(record.launchId, record.cwd, ClaudeTerminal.Resume(record.conversation))
        if (nudge) scheduleNudge(record.launchId, terminal)
        return terminal
    }

    /**
     * Resume a conversation by its id — a session this window launched, or any ended one the
     * operator picked. One this window never launched gets a launch of its own, so it is
     * followed and delivered to like any other. Call on the EDT.
     */
    fun resumeConversation(conversation: String, cwd: String?): OwnedTerminal {
        val known = records().firstOrNull { it.conversation == conversation || it.launchId == conversation }
        if (known != null) return resume(known, nudge = false)
        val record = LaunchRecord(UUID.randomUUID().toString(), cwd, conversation, null, null)
        save(record)
        return resume(record, nudge = false)
    }

    private fun open(launchId: String, cwd: String?, resume: ClaudeTerminal.Resume?): OwnedTerminal {
        val terminal = ClaudeTerminal.launch(project, launchId, cwd, resume, watcher)
        core.terminals.adopt(launchId, terminal)
        return terminal
    }

    private val watcher = object : ClaudeTerminal.Watcher {
        override fun exited(terminal: ClaudeTerminal) {
            // Terminals die as the window closes too; wait a beat so that is not read as a
            // crash and relaunched into an IDE on its way out.
            AppExecutorUtil.getAppScheduledExecutorService().schedule({
                ApplicationManager.getApplication().invokeLater({ onExit(terminal) }, project.disposed)
            }, SETTLE_MS, TimeUnit.MILLISECONDS)
        }

        override fun closed(terminal: ClaudeTerminal) {
            if (closing || project.isDisposed) return
            // Only the launch's current terminal speaks for it: an old tab closed by a relaunch does not.
            if (core.terminals.peek(terminal.launchId) !== terminal) return
            forget(terminal.launchId)
        }
    }

    private fun onExit(terminal: ClaudeTerminal) {
        if (closing || project.isDisposed) return
        if (core.terminals.peek(terminal.launchId) !== terminal) return
        val record = record(terminal.launchId) ?: return
        val relaunches = exitResumes[record.launchId] ?: 0
        when (decideOnExit(mode(), record.status, relaunches)) {
            ExitDecision.Relaunch -> {
                exitResumes[record.launchId] = relaunches + 1
                terminal.close()
                notify("${label(record)} stopped while working — relaunched it on the same conversation (${relaunches + 1}/$MAX_EXIT_RESUMES).")
                resume(record, nudge = true)
            }
            ExitDecision.Offer -> offer(
                if (record.wasWorking) "${label(record)} stopped while working." else "${label(record)} exited.",
                listOf(record),
                old = terminal,
            )
        }
    }

    // ---- Following -----------------------------------------------------------------

    /** Keep each live launch's record on the conversation it is in, and its last status. */
    private fun follow() {
        if (project.isDisposed || closing) return
        val known = core.sessions()
        for (record in records()) {
            if (core.terminals.live(record.launchId) == null) continue // a dead launch keeps its last state
            save(record.follow(known))
        }
    }

    // ---- Nudging -------------------------------------------------------------------

    /**
     * Send the continue prompt once Claude is up. The TUI's readiness is not observable
     * through the terminal API, so the delay is desktop's wait ceiling; a stall alert for
     * the conversation sends it sooner.
     */
    private fun scheduleNudge(launchId: String, terminal: OwnedTerminal) {
        AppExecutorUtil.getAppScheduledExecutorService().schedule({ nudge(launchId, terminal) }, NUDGE_DELAY_MS, TimeUnit.MILLISECONDS)
    }

    private fun alerted(sessionId: String, alert: String?) {
        if (alert != INCOMPLETE) return
        val record = records().firstOrNull { it.launchId in armed && (it.conversation == sessionId || it.launchId == sessionId) } ?: return
        core.terminals.live(record.launchId)?.let { nudge(record.launchId, it) }
    }

    private fun nudge(launchId: String, terminal: OwnedTerminal) {
        if (!armed.remove(launchId) || !terminal.isAlive) return
        (terminal as? ClaudeTerminal)?.send(AUTO_RESUME_PROMPT, bracketedPaste = true) ?: terminal.sendLine(AUTO_RESUME_PROMPT)
    }

    // ---- Restoring -----------------------------------------------------------------

    /** On project open: the launches that were running when the IDE last closed. */
    private fun restoreOnOpen() {
        if (restored) return
        restored = true
        val pending = records()
        if (pending.isEmpty()) return
        ApplicationManager.getApplication().executeOnPooledThread {
            // Wait for the backend's view, so each record resumes the conversation it moved to.
            runCatching { core.refresh().get(REFRESH_WAIT_S, TimeUnit.SECONDS) }
            ApplicationManager.getApplication().invokeLater({
                val known = core.sessions()
                val records = resumable().map { it.follow(known) }.onEach(::save)
                if (records.isEmpty()) return@invokeLater
                when (mode()) {
                    AutoResumeMode.Automatic -> {
                        records.forEach { resume(it) }
                        notify("Resumed ${records.size} Claude session(s) that were running here when the IDE closed.")
                    }
                    AutoResumeMode.Ask -> offer("${records.size} Claude session(s) were running here when the IDE closed.", records)
                    AutoResumeMode.Off -> Unit // still listed under "Resume Session"
                }
            }, project.disposed)
        }
    }

    // ---- Telling the operator -----------------------------------------------------------

    private fun label(record: LaunchRecord): String =
        "Session \"${record.title ?: shortId(record.conversation ?: record.launchId)}\""

    private fun notify(text: String) = MoonlightService.notify(text, NotificationType.INFORMATION, project)

    /** A sticky offer to resume — it waits for the operator, who decides. */
    private fun offer(text: String, records: List<LaunchRecord>, old: ClaudeTerminal? = null) {
        val names = records.joinToString("<br>") { "• " + label(it) + if (it.wasWorking) " — was working" else "" }
        val notification = NotificationGroupManager.getInstance().getNotificationGroup(GROUP)
            .createNotification("MoonlightCode", "$text<br>$names", NotificationType.INFORMATION)
        notification.addAction(NotificationAction.createSimpleExpiring(if (records.size == 1) "Resume" else "Resume all") {
            old?.close()
            records.forEach { resume(it) }
        })
        notification.addAction(NotificationAction.createSimpleExpiring("Forget") {
            records.forEach { forget(it.launchId) }
        })
        notification.notify(project)
        lastOffer?.expire()
        lastOffer = notification
    }

    private var lastOffer: Notification? = null

    override fun dispose() = Unit

    companion object {
        private const val GROUP = "MoonlightCode Sessions"
        private const val INCOMPLETE = "Incomplete"

        /** How long an exit waits before it is read as a crash rather than the IDE closing. */
        private const val SETTLE_MS = 1_500L

        /** Desktop's wait ceiling for a relaunched TUI before the continue prompt is typed. */
        private const val NUDGE_DELAY_MS = 8_000L

        private const val REFRESH_WAIT_S = 10L

        fun of(project: Project): SessionLaunches = project.service()
    }
}
