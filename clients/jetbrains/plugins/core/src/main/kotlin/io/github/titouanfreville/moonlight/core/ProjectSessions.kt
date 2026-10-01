package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.StoragePathMacros
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ModuleRootEvent
import com.intellij.openapi.roots.ModuleRootListener
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.util.concurrency.AppExecutorUtil
import java.util.concurrent.Callable
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.openapi.wm.ex.ToolWindowManagerListener
import com.intellij.ui.content.ContentManagerEvent
import com.intellij.ui.content.ContentManagerListener

/**
 * What one project window knows about which session is its own: the operator's pin, the
 * agent-host tabs linked to sessions, and which of those tabs is in front right now.
 *
 * Stored in the workspace file — per checkout, never shared through VCS — matching VS
 * Code's `workspaceState`: a pin survives a restart, so the operator is not asked again.
 */
@Service(Service.Level.PROJECT)
@State(name = "MoonlightSessions", storages = [Storage(StoragePathMacros.WORKSPACE_FILE)])
class ProjectSessions(private val project: Project) :
    SimplePersistentStateComponent<ProjectSessions.PinState>(PinState()), Disposable {

    class PinState : BaseState() {
        var pinnedSession by string()
        var panelSessions by map<String, String>()
    }

    /**
     * The agent-host tab in front, recomputed on the EDT as tool windows and tabs change,
     * and read from any thread. Cached because `ToolWindowManager` is EDT-bound and the
     * poll that resolves the active session is not.
     */
    @Volatile
    private var panelKey: String? = null

    private val watched = HashSet<String>()

    /**
     * The project's folders — content roots, plus the base directory for a project with none
     * configured. Cached and refreshed when the roots change, because every surface asks for
     * them while drawing, and reading roots needs a read lock that a paint must not take.
     */
    @Volatile
    private var folders: List<String> = listOfNotNull(project.basePath)

    fun folders(): List<String> = folders

    fun pinnedSession(): String? = state.pinnedSession

    fun setPinnedSession(sessionId: String?) {
        state.pinnedSession = sessionId
    }

    fun activePanelKey(): String? = panelKey

    fun panelSession(key: String): String? = state.panelSessions[key]

    /** Link (or unlink, with `null`) the session to the tab in front. False when no agent host is in front. */
    fun linkActivePanel(sessionId: String?): Boolean {
        val key = panelKey ?: return false
        // A fresh map rather than an in-place edit: assignment is what the state tracks as
        // a change, so an edited-in-place map would not be saved.
        state.panelSessions = state.panelSessions.toMutableMap().apply {
            if (sessionId == null) remove(key) else put(key, sessionId)
        }
        return true
    }

    /** Follow tool-window and tab changes, so switching tabs switches which session the UI is about. */
    fun track() {
        val connection = project.messageBus.connect(this)
        connection.subscribe(ToolWindowManagerListener.TOPIC, object : ToolWindowManagerListener {
            override fun stateChanged(toolWindowManager: ToolWindowManager) = recompute()
        })
        connection.subscribe(ModuleRootListener.TOPIC, object : ModuleRootListener {
            override fun rootsChanged(event: ModuleRootEvent) = refreshFolders()
        })
        refreshFolders()
        ApplicationManager.getApplication().invokeLater({ if (!project.isDisposed) recompute() }, project.disposed)
    }

    private fun recompute() {
        val manager = ToolWindowManager.getInstance(project)
        // The last active one, not only the focused one: clicking into the editor to read
        // code the agent wrote should not make the status bar forget which agent it was.
        val id = manager.activeToolWindowId ?: manager.lastActiveToolWindowId
        val window = id?.let(manager::getToolWindow)
        window?.let(::watchTabs)
        val next = panelKey(id, window?.contentManagerIfCreated?.selectedContent?.displayName)
        if (next != panelKey) {
            panelKey = next
            MoonlightService.getInstance().activeSessionMayHaveChanged()
        }
    }

    /** Selecting another tab inside the same tool window changes nothing the manager reports. */
    private fun watchTabs(window: ToolWindow) {
        if (panelKey(window.id, "probe") == null || !watched.add(window.id)) return
        window.contentManager.addContentManagerListener(object : ContentManagerListener {
            override fun selectionChanged(event: ContentManagerEvent) = recompute()
        })
    }

    /** Re-read the content roots in a background read action that yields to writes. */
    private fun refreshFolders() {
        ReadAction.nonBlocking(Callable { ProjectRootManager.getInstance(project).contentRoots.map { it.path } })
            .expireWith(this)
            .finishOnUiThread(ModalityState.any()) { roots ->
                val next = (roots + listOfNotNull(project.basePath)).distinct()
                if (next != folders) {
                    folders = next
                    // Which window owns a session, and which session is active, both follow from these.
                    MoonlightService.getInstance().activeSessionMayHaveChanged()
                }
            }
            .submit(AppExecutorUtil.getAppExecutorService())
    }

    override fun dispose() = Unit
}

/** The folders that make up a project window — see [ProjectSessions.folders]. Any thread. */
fun projectFolders(project: Project): List<String> =
    if (project.isDisposed) emptyList() else project.service<ProjectSessions>().folders()
