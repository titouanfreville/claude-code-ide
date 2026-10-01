package io.github.titouanfreville.moonlight.sessions

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightListener

/**
 * Adopts the new sessions this window owns, when the operator has asked for it.
 *
 * Runs off core's shared poll rather than a timer of its own — the poll is what learns a
 * session exists, and a second loop would only add a way for the two to disagree about
 * what is adopted.
 */
@Service(Service.Level.PROJECT)
class AutoAdopter(private val project: Project) : Disposable {
    private val attempted = HashSet<String>()

    /**
     * Re-entrancy guard. The handler refreshes core, which fires the change it is handling;
     * without the guard a second owned session in the same poll was adopted twice.
     */
    private var adopting = false

    fun start() {
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
                override fun sessionsChanged() = onSessions()
            })
    }

    private fun onSessions() {
        if (adopting || project.isDisposed || !SessionControlSettings.of(project).autoAdopt()) return
        val core = MoonlightApi.getInstance()
        val sessions = core.sessions()
        staleAttempts(attempted, sessions).forEach(attempted::remove)
        // Owned by exactly one window (core picks it), so two windows on the same folder do
        // not both adopt — and a session no window claims is left for the operator.
        val taking = sessionsToAdopt(sessions, { core.ownsSession(project, it) }, attempted)
        if (taking.isEmpty()) return
        adopting = true
        taking.forEach { attempted += it.sessionId }
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                for (session in taking) {
                    try {
                        core.control.adopt(session.sessionId)
                        SessionOps.announceAutoAdopted(project, session)
                    } catch (e: Exception) {
                        // Left in `attempted`: a failing endpoint retried every five seconds is
                        // a request storm, and the operator can still adopt by hand.
                        SessionOps.warn(project, "MoonlightCode could not auto-adopt a session: ${e.message}")
                    }
                }
                // Once, after the whole pass.
                core.refresh().get()
            } finally {
                ApplicationManager.getApplication().invokeLater { adopting = false }
            }
        }
    }

    override fun dispose() = Unit
}

class SessionControlStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        project.service<AutoAdopter>().start()
    }
}
