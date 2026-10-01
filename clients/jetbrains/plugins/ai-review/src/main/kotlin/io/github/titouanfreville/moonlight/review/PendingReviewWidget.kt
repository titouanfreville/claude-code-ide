package io.github.titouanfreville.moonlight.review

import com.intellij.icons.AllIcons
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.CustomStatusBarWidget
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.ui.components.JBLabel
import com.intellij.util.ui.JBUI
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.core.MoonlightToolWindow
import java.awt.Cursor
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.JComponent

/**
 * The review's one-glance state: "Send review (n)" while comments wait, else "n changed"
 * for the active session's unreviewed files. Hidden when there is neither — an idle
 * counter is noise.
 */
private class PendingReviewWidget(private val project: Project) : CustomStatusBarWidget {
    private val label = JBLabel().apply {
        border = JBUI.Borders.empty(0, 6)
        cursor = Cursor.getPredefinedCursor(Cursor.HAND_CURSOR)
        isVisible = false
    }
    private var click: () -> Unit = {}

    override fun ID(): String = ID

    override fun getComponent(): JComponent = label

    override fun install(statusBar: StatusBar) {
        project.messageBus.connect(this).subscribe(ReviewListener.TOPIC, ReviewListener { render() })
        ApplicationManager.getApplication().messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun sessionsChanged() = render()
        })
        label.addMouseListener(object : MouseAdapter() {
            override fun mouseClicked(e: MouseEvent) = click()
        })
        render()
    }

    private fun render() {
        if (project.isDisposed) return
        val counts = ReviewState.of(project).counts()
        if (counts.pending > 0) {
            label.icon = AllIcons.Toolwindows.ToolWindowMessages
            label.text = "Send review (${counts.pending})"
            label.toolTipText = "<html>${counts.pending} comment(s) will be delivered to the session as one message." +
                (if (counts.resolved > 0) "<br>${counts.resolved} resolved." else "") + "</html>"
            click = { ReviewOps.submit(project) }
            label.isVisible = true
            return
        }
        // Only the session you are working in, read from the fleet poll — the same count VS
        // Code shows. Another session's files belong to whoever has it in front of them; the
        // Review tab lists the whole fleet.
        val core = MoonlightApi.getInstance()
        val active = core.activeSession(project)?.sessionId
        val mine = active?.let { id -> core.sessions().firstOrNull { it.sessionId == id } }
        val changed = mine?.unreviewedFiles ?: 0
        if (mine == null) {
            // No single active session — several share this window's root. Say what changed
            // across them rather than nothing; the Review tab sorts out which is which.
            val total = core.sessions().filter { core.ownsSession(project, it.sessionId) }.sumOf { it.unreviewedFiles }
            label.isVisible = total > 0
            label.icon = AllIcons.Actions.Diff
            label.text = "$total changed"
            label.toolTipText = "<html>$total file(s) written by this project's sessions are waiting for review.<br><br>Click to open the Review tab.</html>"
            click = { MoonlightToolWindow.select(project, ReviewTab.TITLE) }
            return
        }
        if (changed == 0) {
            label.isVisible = false
            return
        }
        label.icon = AllIcons.Actions.Diff
        label.text = "$changed changed"
        label.toolTipText = "<html>\"${escape(mine.title ?: mine.sessionId)}\" has written $changed file(s) nobody has reviewed." +
            "<br><br>Click to open the diffs.</html>"
        click = { ReviewOps.openSessionReview(project, mine.sessionId) }
        label.isVisible = true
    }

    override fun dispose() = Unit

    companion object {
        const val ID = "MoonlightPendingReview"
    }
}

class PendingReviewWidgetFactory : StatusBarWidgetFactory {
    override fun getId(): String = PendingReviewWidget.ID
    override fun getDisplayName(): String = "MoonlightCode Review"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = PendingReviewWidget(project)
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}
