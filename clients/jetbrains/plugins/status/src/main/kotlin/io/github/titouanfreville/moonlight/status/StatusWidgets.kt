package io.github.titouanfreville.moonlight.status

import com.intellij.icons.AllIcons
import com.intellij.notification.NotificationType
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionPlaces
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.text.StringUtil
import com.intellij.openapi.wm.CustomStatusBarWidget
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.ui.JBColor
import com.intellij.ui.components.JBLabel
import com.intellij.util.ui.JBUI
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.core.MoonlightService
import io.github.titouanfreville.moonlight.core.projectFolders
import java.awt.Cursor
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.Icon
import javax.swing.JComponent

/**
 * The two status-bar widgets. Separate widgets, as in VS Code: "is anything gated" and
 * "how much allowance is left" are unrelated questions, and folding both into one would
 * lose one of them to truncation.
 */

/** The warning background — the one slot in the status bar that is read without reading. */
private val WARNING_BACKGROUND = JBColor.namedColor("Banner.warningBackground", JBColor(0xFFF4D6, 0x4A3F22))

/** A label drawn from a view: icon, text, warning background, multi-line tooltip. */
private abstract class MoonlightWidget(protected val project: Project) : CustomStatusBarWidget {
    protected val label = JBLabel().apply {
        border = JBUI.Borders.empty(0, 6)
        cursor = Cursor.getPredefinedCursor(Cursor.HAND_CURSOR)
    }

    override fun getComponent(): JComponent = label

    override fun install(statusBar: StatusBar) {
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
                override fun sessionsChanged() = render()
                override fun holdsChanged() = render()
            })
        label.addMouseListener(object : MouseAdapter() {
            override fun mouseClicked(e: MouseEvent) = onClick(e)
        })
        render()
    }

    protected fun draw(icon: Icon?, text: String, warn: Boolean, tooltip: List<String>) {
        label.icon = icon
        label.text = text
        label.isOpaque = warn
        label.background = if (warn) WARNING_BACKGROUND else null
        // Swing tooltips are HTML: escaped line by line, and leading spaces kept so an
        // indented bullet still reads as one.
        label.toolTipText = "<html>" + tooltip.joinToString("<br>") {
            StringUtil.escapeXmlEntities(it).replace("  ", "&nbsp;&nbsp;")
        } + "</html>"
        label.isVisible = true
        label.repaint()
    }

    abstract fun render()

    abstract fun onClick(e: MouseEvent)

    override fun dispose() = Unit
}

private class GatingWidget(project: Project) : MoonlightWidget(project) {
    private var action = GatingAction.Refresh

    override fun ID(): String = ID

    override fun render() {
        if (project.isDisposed) return
        val core = MoonlightApi.getInstance()
        val view = gatingView(
            core.backendError(),
            core.gating(),
            core.sessions(),
            core.activeSession(project),
            core.activePanelKey(project),
            projectFolders(project),
        )
        action = view.action
        draw(iconFor(view.icon), view.text, view.warn, view.tooltip)
    }

    override fun onClick(e: MouseEvent) {
        val id = action.actionId
        if (id == null) {
            MoonlightApi.getInstance().refresh()
            return
        }
        // The actions live in Session Control, which may not be installed — checked rather
        // than assumed, because a click that silently does nothing reads as broken.
        val target = ActionManager.getInstance().getAction(id)
        if (target == null) {
            MoonlightService.notify(
                "This needs MoonlightCode Session Control, which is not installed in this IDE. Install it beside MoonlightCode Core — or do this from VS Code or the desktop app meanwhile.",
                NotificationType.WARNING,
                project,
            )
            return
        }
        ActionManager.getInstance().tryToExecute(target, e, label, ActionPlaces.STATUS_BAR_PLACE, true)
    }

    private fun iconFor(icon: GatingIcon): Icon = when (icon) {
        GatingIcon.NoBackend -> AllIcons.General.Error
        GatingIcon.Unlocked -> AllIcons.Ide.Readwrite
        GatingIcon.Question -> AllIcons.General.QuestionDialog
        GatingIcon.Frozen -> AllIcons.Ide.Readonly
        GatingIcon.Governed -> AllIcons.General.InspectionsOK
    }

    companion object {
        const val ID = "MoonlightGating"
    }
}

private class UsageWidget(project: Project) : MoonlightWidget(project) {
    override fun ID(): String = ID

    override fun render() {
        if (project.isDisposed) return
        val core = MoonlightApi.getInstance()
        val view = usageView(core.backendError(), core.usage(), core.activeSession(project)?.sessionId, System.currentTimeMillis())
        if (view == null) {
            label.isVisible = false
            return
        }
        draw(null, view.text, view.warn, view.tooltip)
    }

    override fun onClick(e: MouseEvent) {
        MoonlightApi.getInstance().refresh()
    }

    companion object {
        const val ID = "MoonlightUsage"
    }
}

class GatingWidgetFactory : StatusBarWidgetFactory {
    override fun getId(): String = GatingWidget.ID
    override fun getDisplayName(): String = "MoonlightCode Gating"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = GatingWidget(project)
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}

class UsageWidgetFactory : StatusBarWidgetFactory {
    override fun getId(): String = UsageWidget.ID
    override fun getDisplayName(): String = "MoonlightCode Claude Usage"
    override fun isAvailable(project: Project): Boolean = true
    override fun createWidget(project: Project): StatusBarWidget = UsageWidget(project)
    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}
