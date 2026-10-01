package io.github.titouanfreville.moonlight.sessions

import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionPlaces
import com.intellij.openapi.actionSystem.DataProvider
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.IconLoader
import com.intellij.openapi.util.text.StringUtil
import com.intellij.openapi.wm.ToolWindow
import com.intellij.ui.AnimatedIcon
import com.intellij.ui.ColoredTreeCellRenderer
import com.intellij.ui.JBColor
import com.intellij.ui.JBSplitter
import com.intellij.ui.PopupHandler
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.treeStructure.Tree
import com.intellij.util.ui.EmptyIcon
import com.intellij.util.ui.JBUI
import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.client.shortId
import io.github.titouanfreville.moonlight.core.HeldApproval
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightDataKeys
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.core.MoonlightToolWindowTab
import java.awt.BorderLayout
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.Icon
import javax.swing.JPanel
import javax.swing.JTree
import javax.swing.SwingUtilities
import javax.swing.ToolTipManager
import javax.swing.event.TreeExpansionEvent
import javax.swing.event.TreeExpansionListener
import javax.swing.tree.DefaultMutableTreeNode
import javax.swing.tree.DefaultTreeModel
import javax.swing.tree.TreePath

/**
 * The MoonlightCode tool window: every session, what it is doing, and what you can do
 * about it.
 *
 * Governed and ungoverned are **two lists, not two groups** — a governed session can be
 * stopped, an ungoverned one cannot, and the gate allows it everything. A header you can
 * collapse would bury exactly that distinction, so each list has its own always-visible
 * header, count and empty state, and the icon vocabularies are disjoint: which list a row
 * belongs to is legible from the icon column alone.
 */
class SessionsTab : MoonlightToolWindowTab {
    override val title: String = TITLE

    override fun createComponent(project: Project, toolWindow: ToolWindow, parent: com.intellij.openapi.Disposable): javax.swing.JComponent =
        SessionsPanel(project, toolWindow).also { Disposer.register(parent, it) }

    companion object {
        const val TITLE = "Sessions"
    }
}

private class SessionsPanel(private val project: Project, private val toolWindow: ToolWindow) :
    JPanel(BorderLayout()), DataProvider, com.intellij.openapi.Disposable {

    private val governed = SessionList(project, Scope.Governed) { lastUsed = it }
    private val unadopted = SessionList(project, Scope.Unadopted) { lastUsed = it }
    private var lastUsed: SessionList = governed
    private val baseIcon: Icon = toolWindow.icon ?: IconLoader.getIcon("/icons/moonlight.svg", SessionsPanel::class.java)

    init {
        val toolbar = ActionManager.getInstance().createActionToolbar(
            "MoonlightSessions",
            ActionManager.getInstance().getAction("Moonlight.Sessions.Toolbar") as ActionGroup,
            true,
        )
        toolbar.targetComponent = this
        add(toolbar.component, BorderLayout.NORTH)

        val splitter = JBSplitter(true, 0.6f).apply {
            firstComponent = governed
            secondComponent = unadopted
        }
        add(splitter, BorderLayout.CENTER)

        ApplicationManager.getApplication().messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun sessionsChanged() = redraw()
            override fun holdsChanged() = redraw()
        })
        project.messageBus.connect(this).subscribe(SessionViewListener.TOPIC, SessionViewListener { redraw() })
        redraw()
    }

    private fun redraw() {
        if (project.isDisposed) return
        governed.rebuild()
        unadopted.rebuild()
        // The badge is reserved for holds: the one number worth interrupting for, and it
        // shows on the stripe icon when the whole tool window is shut. An ungoverned session
        // is not an alert — nothing is blocked — so it is never badged.
        val waiting = MoonlightApi.getInstance().heldApprovals().size
        toolWindow.setIcon(if (waiting > 0) com.intellij.execution.runners.ExecutionUtil.getLiveIndicator(baseIcon) else baseIcon)
        // The tab, not `toolWindow.title`: setting the title renames whichever tab is
        // selected — which blanked this one's name, and would rename Review's too.
        toolWindow.contentManagerIfCreated?.contents?.firstOrNull { it.component === this }?.displayName =
            if (waiting > 0) "${SessionsTab.TITLE} ($waiting waiting)" else SessionsTab.TITLE
    }

    override fun getData(dataId: String): Any? = when {
        SELECTED_NODE.`is`(dataId) -> lastUsed.selectedNode()
        // Shared through core, so Agentic Support knows which session's hold to answer.
        MoonlightDataKeys.SESSION_ID.`is`(dataId) -> (lastUsed.selectedNode() as? Node.Session)?.session?.sessionId
        else -> null
    }

    override fun dispose() = Unit
}

/** One of the two lists. */
private class SessionList(
    private val project: Project,
    private val scope: Scope,
    private val onUse: (SessionList) -> Unit,
) : JPanel(BorderLayout()) {
    private val header = JBLabel().apply {
        border = JBUI.Borders.empty(4, 8)
        font = JBUI.Fonts.label().asBold()
    }
    private val root = DefaultMutableTreeNode()
    private val model = DefaultTreeModel(root)
    private val tree = Tree(model).apply {
        isRootVisible = false
        showsRootHandles = true
        cellRenderer = SessionRenderer()
    }

    /**
     * Groups the operator collapsed. Everything else is expanded, because a collapsed group
     * hides exactly the badges the list exists to surface — and it is remembered by the
     * group's stable key, since the list rebuilds on every poll.
     */
    private val collapsed = HashSet<String>()
    private var rebuilding = false

    init {
        ToolTipManager.sharedInstance().registerComponent(tree)
        tree.addTreeExpansionListener(object : TreeExpansionListener {
            override fun treeExpanded(event: TreeExpansionEvent) {
                if (!rebuilding) keyOf(event.path)?.let(collapsed::remove)
            }

            override fun treeCollapsed(event: TreeExpansionEvent) {
                if (!rebuilding) keyOf(event.path)?.let(collapsed::add)
            }
        })
        tree.addTreeSelectionListener { onUse(this) }
        tree.addMouseListener(object : MouseAdapter() {
            override fun mousePressed(e: MouseEvent) {
                onUse(this@SessionList)
                // A right-click acts on the row under the pointer, not on whatever was selected.
                if (SwingUtilities.isRightMouseButton(e)) {
                    val row = tree.getClosestRowForLocation(e.x, e.y)
                    if (row >= 0 && !tree.isRowSelected(row)) tree.setSelectionRow(row)
                }
            }

            override fun mouseClicked(e: MouseEvent) {
                if (e.clickCount != 2 || !SwingUtilities.isLeftMouseButton(e)) return
                val node = selectedNode() as? Node.Session ?: return
                // A held row's double-click answers it; any other follows the session.
                val id = if (node.held != null) "Moonlight.SessionControl.ReviewPlan" else "Moonlight.SessionControl.SetActive"
                val action = ActionManager.getInstance().getAction(id) ?: return
                ActionManager.getInstance().tryToExecute(action, e, tree, ActionPlaces.TOOLWINDOW_CONTENT, true)
            }
        })
        PopupHandler.installPopupMenu(tree, "Moonlight.Sessions.Popup", ActionPlaces.TOOLWINDOW_POPUP)
        add(header, BorderLayout.NORTH)
        add(JBScrollPane(tree), BorderLayout.CENTER)
    }

    fun selectedNode(): Node? = (tree.lastSelectedPathComponent as? DefaultMutableTreeNode)?.userObject as? Node

    fun rebuild() {
        val core = MoonlightApi.getInstance()
        val all = core.sessions()
        val sessions = sessionsFor(scope, all)
        val held = core.heldApprovals().associateBy { it.sessionId }
        val following = core.activeSession(project)?.sessionId
        val selectedKey = selectedNode()?.let(::keyOf)

        header.text = (if (scope == Scope.Governed) "Governed" else "Not adopted") + if (sessions.isNotEmpty()) "  ${sessions.size}" else ""
        emptyText(core.backendError(), all.isNotEmpty())

        fun row(s: DiscoverableSession, groupId: String?) =
            DefaultMutableTreeNode(Node.Session(s, held[s.sessionId], s.sessionId == following, sessions, groupId), false)

        rebuilding = true
        try {
            root.removeAllChildren()
            val store = GroupStore.of(project)
            when (val grouping = groupSessions(sessions, effectiveMode(scope, SessionControlSettings.of(project).groupBy()), store.custom())) {
                is Grouping.Flat -> grouping.sessions.forEach { root.add(row(it, null)) }
                is Grouping.Grouped -> grouping.groups.forEach { group ->
                    val node = DefaultMutableTreeNode(Node.Group(group, scope), true)
                    group.sessions.forEach { node.add(row(it, group.customId)) }
                    root.add(node)
                }
            }
            model.reload()
            for (i in 0 until root.childCount) {
                val child = root.getChildAt(i) as DefaultMutableTreeNode
                val key = (child.userObject as? Node.Group)?.group?.key ?: continue
                if (key !in collapsed) tree.expandPath(TreePath(arrayOf(root, child)))
            }
            selectedKey?.let(::select)
        } finally {
            rebuilding = false
        }
    }

    /**
     * An empty list cannot explain itself — "no sessions" and "no daemon" render as the same
     * blank panel — so the empty text says which it is, with the fix on a link.
     */
    private fun emptyText(backendError: String?, hasSessions: Boolean) {
        val text = tree.emptyText
        text.clear()
        val core = MoonlightApi.getInstance()
        when {
            scope == Scope.Unadopted -> if (backendError == null) text.appendText("Every detected session is governed.")
            backendError != null -> {
                text.appendLine("No MoonlightCode backend is answering.")
                text.appendLine("Without the daemon nothing here is governed.", SimpleTextAttributes.GRAYED_ATTRIBUTES, null)
                text.appendLine("Retry now", SimpleTextAttributes.LINK_PLAIN_ATTRIBUTES) { core.refresh() }
            }
            hasSessions -> {
                text.appendLine("No session is governed yet.")
                text.appendLine("The sessions below run with nothing gating them.", SimpleTextAttributes.GRAYED_ATTRIBUTES, null)
                text.appendLine("Adopt one to put it under a workflow phase.", SimpleTextAttributes.GRAYED_ATTRIBUTES, null)
            }
            else -> {
                text.appendLine("No Claude Code sessions detected.")
                text.appendLine("A session appears once it writes its first transcript line.", SimpleTextAttributes.GRAYED_ATTRIBUTES, null)
                text.appendLine("Start a governed session", SimpleTextAttributes.LINK_PLAIN_ATTRIBUTES) { SessionOps.startSession(project) }
            }
        }
    }

    private fun select(key: String) {
        val all = root.depthFirstEnumeration().toList().filterIsInstance<DefaultMutableTreeNode>()
        val match = all.firstOrNull { (it.userObject as? Node)?.let(::keyOf) == key } ?: return
        tree.selectionPath = TreePath(match.path)
    }

    private fun keyOf(path: TreePath): String? = ((path.lastPathComponent as? DefaultMutableTreeNode)?.userObject as? Node)?.let(::keyOf)

    private fun keyOf(node: Node): String = when (node) {
        is Node.Group -> node.group.key
        is Node.Session -> "session:${node.session.sessionId}"
    }
}

/** `[icon] label   description   badge` — each slot carries exactly one thing. */
private class SessionRenderer : ColoredTreeCellRenderer() {
    override fun customizeCellRenderer(
        tree: JTree,
        value: Any?,
        selected: Boolean,
        expanded: Boolean,
        leaf: Boolean,
        row: Int,
        hasFocus: Boolean,
    ) {
        when (val node = (value as? DefaultMutableTreeNode)?.userObject as? Node) {
            is Node.Group -> {
                icon = if (node.group.custom) AllIcons.Nodes.Folder else AllIcons.Nodes.Module
                append(node.group.label)
                append("  ${node.group.sessions.size}", SimpleTextAttributes.GRAYED_ATTRIBUTES)
                toolTipText = node.group.tooltip
            }
            is Node.Session -> {
                val s = node.session
                icon = iconFor(s, node.held != null)
                // The label in bold is the only way a row reads as "this is the one you are in"
                // without spending the icon or badge slot on it.
                append(sessionLabel(s, node.among), if (node.following) SimpleTextAttributes.REGULAR_BOLD_ATTRIBUTES else SimpleTextAttributes.REGULAR_ATTRIBUTES)
                append("  ${describe(s, node.held)}", if (node.held != null) WARNING else SimpleTextAttributes.GRAYED_ATTRIBUTES)
                val b = badge(s, node.held != null)
                append("  ${b.text}", attributesFor(b.tone))
                toolTipText = tooltip(s, node.held, node.following)
            }
            null -> Unit
        }
    }

    /**
     * Two disjoint vocabularies. Governed rows get the expressive status set, including the
     * one icon that moves. Ungoverned rows all get the same open padlock whatever they are
     * doing — for a session nothing gates, "is it mid-turn?" is not the fact you need, and
     * borrowing the governed icons is what made the two lists look alike.
     */
    private fun iconFor(s: DiscoverableSession, held: Boolean): Icon = when {
        !s.adopted -> AllIcons.Ide.Readwrite
        // A hold outranks whatever the status says: the session is stopped, waiting on you.
        held -> AllIcons.General.BalloonWarning
        else -> when (s.status) {
            SessionStatus.Running -> AnimatedIcon.Default.INSTANCE
            SessionStatus.WaitingInput -> AllIcons.General.Balloon
            SessionStatus.Done -> AllIcons.RunConfigurations.TestPassed
            SessionStatus.Errored -> AllIcons.General.Error
            SessionStatus.Paused -> AllIcons.Actions.Pause
            SessionStatus.Idle -> EmptyIcon.ICON_16
        }
    }

    private fun attributesFor(tone: BadgeTone): SimpleTextAttributes = when (tone) {
        BadgeTone.Warning -> WARNING
        BadgeTone.Info -> SimpleTextAttributes(SimpleTextAttributes.STYLE_BOLD, JBColor.BLUE)
        BadgeTone.Error -> SimpleTextAttributes(SimpleTextAttributes.STYLE_BOLD, JBColor.RED)
        BadgeTone.Muted -> SimpleTextAttributes.GRAYED_ATTRIBUTES
        BadgeTone.Normal -> SimpleTextAttributes.REGULAR_ATTRIBUTES
    }

    private fun tooltip(s: DiscoverableSession, held: HeldApproval?, following: Boolean): String {
        val esc = StringUtil::escapeXmlEntities
        val lines = mutableListOf("<b>${esc(s.title ?: shortId(s.sessionId))}</b>")
        if (held != null) {
            val waited = ((System.currentTimeMillis() - held.sinceMs) / 1000).coerceAtLeast(0)
            lines += "<b>Waiting on you</b> — ${esc(held.what)} · ${waited}s"
        }
        lines += if (s.adopted) {
            "governed — phase <b>${s.phase}</b>${if (s.phase.frozen) " (project writes denied)" else ""}"
        } else {
            "<b>not adopted</b> — the gate allows this session everything"
        }
        lines += "status: ${s.status}"
        if (s.unreviewedFiles > 0) lines += "${s.unreviewedFiles} file(s) written, none reviewed"
        s.root?.let { lines += "root: <code>${esc(it)}</code>" }
        lines += "id: <code>${esc(s.sessionId)}</code>"
        if (following) lines += "<i>The status bar follows this session.</i>"
        return "<html>${lines.joinToString("<br>")}</html>"
    }

    private companion object {
        val WARNING = SimpleTextAttributes(SimpleTextAttributes.STYLE_BOLD, JBColor.ORANGE)
    }
}
