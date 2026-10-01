package io.github.titouanfreville.moonlight.agentic

import com.intellij.ide.BrowserUtil
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.testFramework.LightVirtualFile
import com.intellij.ui.JBColor
import com.intellij.ui.components.ActionLink
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import com.intellij.ui.components.panels.VerticalLayout
import com.intellij.util.ui.HTMLEditorKitBuilder
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.UIUtil
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.client.shortId
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.planmd.PlanMarkdown
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.Rectangle
import java.beans.PropertyChangeListener
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.JComponent
import javax.swing.JEditorPane
import javax.swing.JPanel
import javax.swing.Scrollable
import javax.swing.SwingConstants
import javax.swing.event.HyperlinkEvent

/**
 * The plan gate as an editor tab — the counterpart of `plan-view.ts`.
 *
 * A tab rather than a dialog or a toast: a plan is read, not dispatched from a one-line
 * prompt, and the session is *stopped* until it is answered — the answer deserves the
 * editor area, and must not vanish on its own.
 *
 * Comments are anchored: each belongs to a section of the plan, and the agent is told
 * which. The verdicts are live only while a plan hold is outstanding — `PlanProposed` also
 * fires for sessions that finished hours ago, so a plan on screen is not evidence that
 * anything is waiting, and a button that silently does nothing is how an operator comes
 * to believe they approved something they did not.
 *
 * Swing, updated in place: a refresh changes the status and the buttons, and rebuilds the
 * sections only when the plan text itself changed — so a hold changing anywhere in the
 * fleet can never wipe a comment being typed, the bug the VS Code webview had to fence.
 */
class PlanReviewFile(val sessionId: String, title: String) : LightVirtualFile(title) {
    init {
        isWritable = false
    }
}

/** One review tab per session, reused, so reopening focuses the tab instead of stacking them. */
@Service(Service.Level.PROJECT)
class PlanReviews(private val project: Project) {
    private val files = HashMap<String, PlanReviewFile>()

    fun open(sessionId: String, focus: Boolean) {
        val core = MoonlightApi.getInstance()
        val file = files.getOrPut(sessionId) {
            val label = core.sessions().firstOrNull { it.sessionId == sessionId }?.let { sessionLabel(it, core.sessions()) }
                ?: shortId(sessionId)
            PlanReviewFile(sessionId, "Plan review — $label")
        }
        FileEditorManager.getInstance(project).openFile(file, focus)
    }

    companion object {
        fun of(project: Project): PlanReviews = project.service()
    }
}

class PlanReviewEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean = file is PlanReviewFile
    override fun acceptRequiresReadAction(): Boolean = false
    override fun createEditor(project: Project, file: VirtualFile): FileEditor = PlanReviewEditor(project, file as PlanReviewFile)
    override fun getEditorTypeId(): String = "moonlight-plan-review"
    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR
}

private class PlanReviewEditor(project: Project, private val file: PlanReviewFile) : UserDataHolderBase(), FileEditor {
    private val panel = PlanReviewPanel(project, file.sessionId, this)

    override fun getComponent(): JComponent = panel
    override fun getPreferredFocusedComponent(): JComponent = panel
    override fun getName(): String = "Plan review"
    override fun getFile(): VirtualFile = file
    override fun setState(state: FileEditorState) = Unit
    override fun isModified(): Boolean = false
    override fun isValid(): Boolean = true
    override fun addPropertyChangeListener(listener: PropertyChangeListener) = Unit
    override fun removePropertyChangeListener(listener: PropertyChangeListener) = Unit
    override fun dispose() = Unit
}

private class PlanReviewPanel(
    private val project: Project,
    private val sessionId: String,
    parent: Disposable,
) : JPanel(BorderLayout()) {
    private val core = MoonlightApi.getInstance()
    private val status = JBLabel().apply {
        border = JBUI.Borders.empty(10, 12)
        verticalAlignment = SwingConstants.TOP
    }
    private val sections = WidthTrackingPanel()
    private val buttons = PlanVerdict.values().associateWith { verdict ->
        JButton(verdict.label).apply { addActionListener { decide(verdict) } }
    }

    /** One comment box per section, in plan order. Kept across refreshes of the same plan. */
    private val comments = mutableListOf<JBTextArea>()
    private var shownPlan: String? = null
    private var sending = false

    init {
        add(status, BorderLayout.NORTH)
        add(JBScrollPane(sections).apply { border = JBUI.Borders.empty() }, BorderLayout.CENTER)
        add(JPanel(FlowLayout(FlowLayout.LEFT)).apply {
            border = JBUI.Borders.customLineTop(JBColor.border())
            buttons.values.forEach(::add)
        }, BorderLayout.SOUTH)

        ApplicationManager.getApplication().messageBus.connect(parent).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun holdsChanged() = render()
            override fun sessionsChanged() = render()
        })
        render()
    }

    private fun render() {
        if (project.isDisposed) return
        val held = core.heldApproval(sessionId)?.takeIf(::isPlanHold)
        val plan = core.proposedPlan(sessionId) ?: held?.plan ?: ""
        if (plan != shownPlan) rebuild(plan)

        val armed = held != null && !sending
        buttons.values.forEach { it.isEnabled = armed }
        val who = core.sessions().firstOrNull { it.sessionId == sessionId }?.let { sessionLabel(it, core.sessions()) } ?: shortId(sessionId)
        status.text = when {
            sending -> "<html>Sending your verdict to <b>${PlanMarkdown.escape(who)}</b>…</html>"
            held != null -> "<html><b>${PlanMarkdown.escape(who)}</b> is waiting on your verdict. " +
                "Comment on any section, then choose a verdict below.</html>"
            else -> "<html><b>${PlanMarkdown.escape(who)}</b> is not currently held on a plan — this plan is for reading.</html>"
        }
    }

    /** Rebuild the sections for a new plan text, keeping comments by section position. */
    private fun rebuild(plan: String) {
        shownPlan = plan
        val previous = comments.map { it.text }
        comments.clear()
        sections.removeAll()
        val blocks = planBlocks(plan)
        if (blocks.isEmpty()) {
            sections.add(JBLabel("No plan proposed yet.").apply { foreground = UIUtil.getContextHelpForeground() })
        }
        blocks.forEachIndexed { index, block ->
            val area = JBTextArea(previous.getOrElse(index) { "" }, 3, 60).apply {
                lineWrap = true
                wrapStyleWord = true
                border = JBUI.Borders.compound(JBUI.Borders.customLine(JBColor.border()), JBUI.Borders.empty(4))
                emptyText.text = "Comment on this section…"
            }
            comments += area
            sections.add(section(block, area))
        }
        sections.revalidate()
        sections.repaint()
    }

    /**
     * One section: the rendered markdown, and a quiet "+ Comment" that opens the box. A plan
     * is mostly read, not annotated — an input under every section turns a document into a
     * form — so the box opens only where the operator has something to say.
     */
    private fun section(block: String, area: JBTextArea): JComponent {
        val open = area.text.isNotBlank()
        val box = JPanel(BorderLayout()).apply {
            isOpaque = false
            isVisible = open
            border = JBUI.Borders.emptyTop(4)
            add(area, BorderLayout.CENTER)
        }
        val link = ActionLink("+ Comment") {
            box.isVisible = true
            (it.source as JComponent).isVisible = false
            area.requestFocusInWindow()
        }.apply { isVisible = !open }
        return JPanel(BorderLayout()).apply {
            isOpaque = false
            border = JBUI.Borders.compound(
                BorderFactory.createMatteBorder(0, JBUI.scale(2), 0, 0, if (open) JBColor.namedColor("Component.focusColor", JBColor.BLUE) else JBColor.border()),
                JBUI.Borders.empty(2, 10, 6, 0),
            )
            add(markdownPane(block), BorderLayout.CENTER)
            add(JPanel(VerticalLayout(0)).apply {
                isOpaque = false
                add(JPanel(FlowLayout(FlowLayout.LEFT, 0, 2)).apply {
                    isOpaque = false
                    add(link)
                })
                add(box)
            }, BorderLayout.SOUTH)
        }
    }

    private fun markdownPane(block: String): JEditorPane {
        val kit = HTMLEditorKitBuilder().withWordWrapViewFactory().build()
        val code = JBColor.namedColor("Editor.background", JBColor(0xF5F5F5, 0x2B2B2B))
        kit.styleSheet.addRule("pre { background-color: #${hex(code)}; padding: 6px; }")
        kit.styleSheet.addRule("code { font-family: monospace; }")
        kit.styleSheet.addRule("th, td { padding: 2px 8px; border: 1px solid #${hex(JBColor.border())}; }")
        return JEditorPane().apply {
            editorKit = kit
            isEditable = false
            isOpaque = false
            putClientProperty(JEditorPane.HONOR_DISPLAY_PROPERTIES, true)
            font = UIUtil.getLabelFont()
            text = "<html><body>${PlanMarkdown.render(block)}</body></html>"
            // Only targets the renderer already allowed are links; of those, only web URLs
            // go anywhere — a relative path in a plan names a file, not a page.
            addHyperlinkListener { e ->
                val url = e.description ?: return@addHyperlinkListener
                if (e.eventType == HyperlinkEvent.EventType.ACTIVATED && Regex("^https?://", RegexOption.IGNORE_CASE).containsMatchIn(url)) {
                    BrowserUtil.browse(url)
                }
            }
        }
    }

    private fun decide(verdict: PlanVerdict) {
        val plan = shownPlan ?: return
        val notes = comments.withIndex()
            .filter { it.value.text.isNotBlank() }
            .associate { it.index to it.value.text.trim() }
        sending = true
        render()
        ApplicationManager.getApplication().executeOnPooledThread {
            val outcome = decidePlan(core.control, sessionId, plan, notes, verdict)
            ApplicationManager.getApplication().invokeLater({
                sending = false
                when (outcome) {
                    is GateOutcome.Ok -> {
                        comments.forEach { it.text = "" }
                        Holds.info(project, outcome.summary)
                    }
                    // The session is still stopped — say so plainly rather than letting the
                    // tab fall quiet and look like the verdict went through.
                    is GateOutcome.Failed -> Holds.error(project, "The verdict did not reach the session (${outcome.error}). It is still waiting.")
                }
                core.refresh()
                render()
            }, project.disposed)
        }
    }

    private fun hex(color: java.awt.Color): String = "%02x%02x%02x".format(color.red, color.green, color.blue)
}

/** A panel that takes the scroll pane's width, so the markdown wraps instead of scrolling sideways. */
private class WidthTrackingPanel : JPanel(VerticalLayout(JBUI.scale(10))), Scrollable {
    init {
        border = JBUI.Borders.empty(4, 12, 12, 12)
    }

    override fun getPreferredScrollableViewportSize(): Dimension = preferredSize
    override fun getScrollableUnitIncrement(visibleRect: Rectangle, orientation: Int, direction: Int): Int = JBUI.scale(16)
    override fun getScrollableBlockIncrement(visibleRect: Rectangle, orientation: Int, direction: Int): Int = visibleRect.height
    override fun getScrollableTracksViewportWidth(): Boolean = true
    override fun getScrollableTracksViewportHeight(): Boolean = false
}
