package io.github.titouanfreville.moonlight.core

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.PathManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.wm.IdeFocusManager
import com.intellij.util.concurrency.AppExecutorUtil
import io.github.titouanfreville.moonlight.client.ControlApi
import io.github.titouanfreville.moonlight.client.DaemonLauncher
import io.github.titouanfreville.moonlight.client.DaemonOptions
import io.github.titouanfreville.moonlight.client.DaemonStartResult
import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.EngineEvent
import io.github.titouanfreville.moonlight.client.EventStream
import io.github.titouanfreville.moonlight.client.EventStreamHandlers
import io.github.titouanfreville.moonlight.client.HookStatusEntry
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.client.UsageResponse
import io.github.titouanfreville.moonlight.client.daemonBinaryName
import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * MoonlightCode Core — the one backend connection every MoonlightCode plugin shares.
 *
 * It contributes no UI. It exists so the feature plugins can ship and be installed
 * separately without each opening its own connection and forming its own opinion about
 * what the fleet is doing. One instance serves every project window in the IDE.
 */
@Service(Service.Level.APP)
class MoonlightService : MoonlightApi, Disposable {
    override val version: Int = 1
    override val control: ControlApi = ControlApi()
    override val terminals: SessionTerminals = SessionTerminals { sessions }

    private val launcher = DaemonLauncher()
    private val gate = GateState()

    @Volatile private var sessions: List<DiscoverableSession> = emptyList()
    @Volatile private var gating: List<HookStatusEntry> = emptyList()
    @Volatile private var usage: UsageResponse? = null
    @Volatile private var backendError: String? = null

    private val refreshLock = Any()
    private var poll: ScheduledFuture<*>? = null
    private var stream: EventStream? = null

    // A daemon downloaded on demand, once verified. Held here so the poll does not re-stat
    // the cache every tick, and so one in-flight download is shared by every tick during it.
    @Volatile private var downloadedDaemon: String? = null
    @Volatile private var installing = false

    /**
     * Earliest time a failed install may be retried. Without it a 404 or a checksum
     * mismatch re-fetched the whole asset on every five-second tick — and a nightly-pinned
     * build's 404 is permanent, because the nightly release is recreated on each push.
     */
    @Volatile private var retryInstallAfter = 0L

    /** Start polling and streaming. Idempotent; called from every project's startup. */
    @Synchronized
    fun start() {
        if (poll != null) return
        poll = AppExecutorUtil.getAppScheduledExecutorService()
            .scheduleWithFixedDelay({ refreshBlocking() }, 0, POLL_INTERVAL_MS, TimeUnit.MILLISECONDS)
        // The gate rides the event stream rather than the poll: a hold is not a fact that
        // can wait five seconds for its turn — the session is stopped until someone answers.
        stream = EventStream(object : EventStreamHandlers {
            override fun onEvent(event: EngineEvent) {
                if (event is EngineEvent.SessionAlert) onEdt { publisher().sessionAlert(event.session, event.alert) }
                if (gate.apply(event)) fireHolds()
            }

            override fun onDesync(reason: String) = resyncHolds()
        }).start()
        resyncHolds()
    }

    override fun dispose() {
        synchronized(this) {
            poll?.cancel(false)
            poll = null
            stream?.close()
            stream = null
        }
    }

    override fun sessions(): List<DiscoverableSession> = sessions
    override fun gating(): List<HookStatusEntry> = gating
    override fun usage(): UsageResponse? = usage
    override fun backendError(): String? = backendError

    override fun refresh(): CompletableFuture<Unit> =
        CompletableFuture.supplyAsync({ refreshBlocking() }, AppExecutorUtil.getAppExecutorService())

    private fun refreshBlocking() {
        synchronized(refreshLock) {
            try {
                // Sessions and gating are the load-bearing pair: if either is unreachable the
                // backend genuinely is, and the status bar must say so.
                val nextSessions = control.discoverableSessions()
                val nextGating = control.gatingStatus()
                // Usage is fetched separately and allowed to fail. A backend that predates
                // `/control/usage` must not report as entirely down over a missing quota
                // reading — and the previous figure is kept rather than blanked, so a blip
                // does not make it flicker.
                val nextUsage = try {
                    control.usage()
                } catch (_: Exception) {
                    usage
                }
                // Only fire when something moved: this ticks every five seconds, and waking
                // every surface each time would make them redraw for nothing.
                val same = backendError == null && nextSessions == sessions && nextGating == gating && nextUsage == usage
                sessions = nextSessions
                gating = nextGating
                usage = nextUsage
                backendError = null
                launcher.reachable()
                if (!same) fireSessions()
            } catch (e: Exception) {
                val reported = explainOutage(e.message ?: e.toString())
                // An unreachable backend is an ordinary state, rendered by the status plugin.
                // The last known lists are kept so a blip does not blank every view.
                if (backendError != reported) {
                    backendError = reported
                    fireSessions()
                }
            }
        }
    }

    /**
     * Start a daemon, and say *why* there is no backend when the reason is a choice or a
     * missing binary — "connection refused" alone sends an operator looking for a crash when
     * the answer is that this IDE was told not to start one.
     *
     * An IDE that opens is expected to make sure the daemon is running: it is what governs
     * sessions, so reporting "no backend" and stopping there would leave the fleet
     * ungoverned while looking like it is working. Racing another IDE is safe.
     */
    private fun explainOutage(message: String): String {
        val settings = MoonlightSettings.getInstance()
        val options = DaemonOptions(path = settings.daemonPath() ?: downloadedDaemon, autostart = settings.autostart())
        val started = launcher.ensure(options)
        if (started == DaemonStartResult.NoBinary && options.autostart && settings.daemonPath() == null) {
            installDaemon()
        }
        return when (started) {
            DaemonStartResult.NoBinary -> "$message (moonlightd not found)"
            DaemonStartResult.Disabled ->
                "$message (autostart is off — start moonlightd yourself, or enable it in Settings ▸ Tools ▸ MoonlightCode)"
            else -> message
        }
    }

    /**
     * Fetch the daemon the first time `PATH` turns up empty. Only then: an operator who
     * pointed at their own build, or turned autostart off, has already answered.
     */
    private fun installDaemon() {
        if (downloadedDaemon != null || installing || System.currentTimeMillis() < retryInstallAfter) return
        installing = true
        AppExecutorUtil.getAppExecutorService().execute {
            try {
                when (val result = ensureDownloadedDaemon(daemonStorage(), daemonBinaryName())) {
                    is InstallResult.Installed -> downloadedDaemon = result.binary.toString()
                    is InstallResult.Cached -> downloadedDaemon = result.binary.toString()
                    is InstallResult.Failed -> {
                        retryInstallAfter = System.currentTimeMillis() + INSTALL_RETRY_MS
                        // Surfaced, not just logged: this path exists for people who have
                        // nothing but the plugin, and a reason buried in idea.log is one they
                        // never see.
                        notify("MoonlightCode could not download moonlightd: ${result.error}", NotificationType.WARNING)
                    }
                    else -> Unit
                }
            } finally {
                installing = false
            }
        }
    }

    /** Re-read the outstanding holds from the server, after a desync or at start. */
    private fun resyncHolds() {
        AppExecutorUtil.getAppExecutorService().execute {
            try {
                gate.resync(control.pendingApprovals())
                fireHolds()
            } catch (_: Exception) {
                // An unreachable backend is already reported by the poll; a second, louder
                // complaint from here would only duplicate it.
            }
        }
    }

    override fun heldApproval(sessionId: String): HeldApproval? = gate.approval(sessionId)
    override fun heldApprovals(): List<HeldApproval> = gate.approvals()
    override fun proposedPlan(sessionId: String): String? = gate.plan(sessionId)

    override fun activeSession(project: Project): ActiveSession? {
        val known = sessions
        val state = project.service<ProjectSessions>()
        // 1. The session linked to the agent-host tab in front of you — the only answer
        //    that stays right when several agent tabs are open at once.
        // Links and pins name the conversation they were made on; one whose process has
        // since `/resume`d or `/clear`ed is followed through its launch.
        val forPanel = state.activePanelKey()?.let(state::panelSession)?.let { currentConversation(it, known) }
        if (forPanel != null && known.any { it.sessionId == forPanel }) {
            return ActiveSession(forPanel, ActiveSession.How.Panel)
        }
        // 2. A session we launched in this window. Only when there is exactly one, since
        //    several owned terminals are as ambiguous as none.
        val ownedHere = terminals.ownedIn(project)
        val owned = known.filter { it.sessionId in ownedHere }
        if (owned.size == 1) return ActiveSession(owned[0].sessionId, ActiveSession.How.Owned)
        // 3. The operator's project-wide pin, while it still exists.
        val pinned = state.pinnedSession()?.let { currentConversation(it, known) }
        if (pinned != null && known.any { it.sessionId == pinned }) {
            return ActiveSession(pinned, ActiveSession.How.Pinned)
        }
        // 4. Exactly one session in this project — a guess, labelled as one, and only when
        //    there is no competition: naming the wrong session as governed is worse than
        //    admitting we don't know.
        val folders = projectFolders(project)
        val here = known.filter { s -> s.root?.let { root -> folders.any { isInside(root, it) } } == true }
        val running = here.filter { it.status == SessionStatus.Running }
        val candidates = running.ifEmpty { here }
        return if (candidates.size == 1) ActiveSession(candidates[0].sessionId, ActiveSession.How.Sole) else null
    }

    override fun setPinnedSession(project: Project, sessionId: String?) {
        project.service<ProjectSessions>().setPinnedSession(sessionId)
        fireSessions()
    }

    override fun activePanelKey(project: Project): String? = project.service<ProjectSessions>().activePanelKey()

    override fun pinToActivePanel(project: Project, sessionId: String?): Boolean {
        val linked = project.service<ProjectSessions>().linkActivePanel(sessionId)
        if (linked) fireSessions()
        return linked
    }

    override fun owningProject(sessionId: String): Project? {
        val root = sessions.firstOrNull { it.sessionId == sessionId }?.root
        val owners = ProjectManager.getInstance().openProjects.filter { candidate ->
            !candidate.isDisposed && ownsSession(sessionId, activeSession(candidate)?.sessionId, root, projectFolders(candidate))
        }
        return pickOwner(owners, focusedProject())
    }

    /** The project window the operator was last in, if any. */
    fun focusedProject(): Project? = IdeFocusManager.getGlobalInstance().lastFocusedFrame?.project

    /** Called by a project when something about *which* session is active moved. */
    internal fun activeSessionMayHaveChanged() = fireSessions()

    private fun fireSessions() = onEdt { publisher().sessionsChanged() }

    private fun fireHolds() = onEdt { publisher().holdsChanged() }

    private fun publisher(): MoonlightListener =
        ApplicationManager.getApplication().messageBus.syncPublisher(MoonlightListener.TOPIC)

    /**
     * Listeners are told on the EDT so every surface can redraw without hopping threads
     * itself. `any()` modality: these are redraws, not model changes, and a modal dialog
     * open somewhere must not freeze the status bar behind it.
     */
    private fun onEdt(action: () -> Unit) {
        val app = ApplicationManager.getApplication()
        if (app.isDisposed) return
        app.invokeLater({ if (!app.isDisposed) action() }, ModalityState.any())
    }

    companion object {
        /** How often the backend is polled — one loop for every plugin and every window. */
        const val POLL_INTERVAL_MS: Long = 5000

        private const val INSTALL_RETRY_MS: Long = 10 * 60_000

        fun getInstance(): MoonlightService = ApplicationManager.getApplication().service()

        /** Where a downloaded daemon is cached — the IDE's system directory, not the config one. */
        fun daemonStorage(): Path = Path.of(PathManager.getSystemPath(), "moonlight")

        fun notify(content: String, type: NotificationType, project: Project? = null) {
            NotificationGroupManager.getInstance().getNotificationGroup("MoonlightCode")
                .createNotification(content, type)
                .notify(project)
        }
    }
}
