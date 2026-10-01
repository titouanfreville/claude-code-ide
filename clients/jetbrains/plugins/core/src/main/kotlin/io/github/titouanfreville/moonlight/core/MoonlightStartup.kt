package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity

/**
 * Starts the shared connection when the first project opens, and has each project follow
 * its own agent-host tabs. Starting is idempotent, so every project calls it.
 */
class MoonlightStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        MoonlightService.getInstance().start()
        project.service<ProjectSessions>().track()
        // After tracking, so a restored session lands in a window that knows its own folders.
        SessionLaunches.of(project).start()
    }
}
