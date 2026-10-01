package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.Disposable
import com.intellij.openapi.extensions.ExtensionPointName
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.ui.content.ContentFactory
import javax.swing.JComponent

/**
 * A tab of the one MoonlightCode tool window.
 *
 * The feature plugins ship and install separately, but they are one product: sessions in one
 * window, the review in another, read as two tools. Core owns the window and each plugin adds
 * its tab through this extension point (`io.github.titouanfreville.moonlight.core.toolWindowTab`),
 * so whatever is installed lands in the same place — ordered by the extension's `order`.
 */
interface MoonlightToolWindowTab {
    /** The tab's title, and its key: [MoonlightToolWindow.select] finds a tab by it. */
    val title: String

    /** Build the tab. Anything that must be released with it is registered on `parent`. */
    fun createComponent(project: Project, toolWindow: ToolWindow, parent: Disposable): JComponent

    companion object {
        val EP: ExtensionPointName<MoonlightToolWindowTab> =
            ExtensionPointName.create("io.github.titouanfreville.moonlight.core.toolWindowTab")
    }
}

class MoonlightToolWindowFactory : ToolWindowFactory, DumbAware {
    /** Core alone contributes nothing to show — the window appears with the first tab. */
    override suspend fun isApplicableAsync(project: Project): Boolean = MoonlightToolWindowTab.EP.extensionList.isNotEmpty()

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val factory = ContentFactory.getInstance()
        for (tab in MoonlightToolWindowTab.EP.extensionList) {
            val lifetime = Disposer.newDisposable("MoonlightCode tab ${tab.title}")
            val content = factory.createContent(tab.createComponent(project, toolWindow, lifetime), tab.title, false)
            content.isCloseable = false
            Disposer.register(content, lifetime)
            toolWindow.contentManager.addContent(content)
        }
    }
}

object MoonlightToolWindow {
    const val ID: String = "MoonlightCode"

    /** Open the MoonlightCode window on the tab titled `title`. */
    fun select(project: Project, title: String) {
        val window = ToolWindowManager.getInstance(project).getToolWindow(ID) ?: return
        window.activate {
            val manager = window.contentManager
            manager.contents.firstOrNull { it.tabName == title }?.let(manager::setSelectedContent)
        }
    }
}
