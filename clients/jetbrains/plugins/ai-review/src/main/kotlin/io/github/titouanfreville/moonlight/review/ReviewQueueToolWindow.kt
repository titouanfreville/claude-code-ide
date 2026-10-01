package io.github.titouanfreville.moonlight.review

import com.intellij.icons.AllIcons
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.DataKey
import com.intellij.openapi.actionSystem.DataSink
import com.intellij.openapi.actionSystem.UiDataProvider
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.ComboBox
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.ui.CollectionListModel
import com.intellij.ui.ColoredListCellRenderer
import com.intellij.ui.JBSplitter
import com.intellij.ui.PopupHandler
import com.intellij.ui.SimpleListCellRenderer
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.StatusText
import io.github.titouanfreville.moonlight.client.ReviewQueueItem
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.client.shortId
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightDataKeys
import io.github.titouanfreville.moonlight.core.MoonlightListener
import io.github.titouanfreville.moonlight.core.MoonlightToolWindowTab
import java.awt.BorderLayout
import java.awt.Font
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicBoolean
import javax.swing.DefaultComboBoxModel
import javax.swing.JComponent
import javax.swing.JList
import javax.swing.JPanel
import javax.swing.ListSelectionModel

object ReviewDataKeys {
    /** The queue item a UI element is about. */
    @JvmField
    val QUEUE_ITEM: DataKey<ReviewQueueItem> = DataKey.create("moonlight.review.queueItem")
}

/**
 * The review queue: what a session changed that nobody has looked at yet, with the selected
 * file's diff below. Double-click opens the full diff, where comments are written.
 *
 * The tab is global — one per window, every session — so it says which session it is
 * reviewing: the session you are working in by default, followed as it changes, or any
 * other picked from the selector.
 */
class ReviewTab : MoonlightToolWindowTab {
    override val title: String = TITLE

    override fun createComponent(project: Project, toolWindow: ToolWindow, parent: Disposable): JComponent =
        QueuePanel(project, toolWindow).also { Disposer.register(parent, it) }

    companion object {
        const val TITLE = "Review"
    }
}

/** What the selector can show. */
private sealed interface Scope {
    /**
     * The session you are working in, followed as it changes — or, while this window has no
     * single active session (several sessions share its root), every session it owns.
     */
    data object Active : Scope

    /** Every session this window owns. */
    data object Project : Scope

    data class Session(val id: String) : Scope

    data object All : Scope
}

/** One selector row: the scope, and what it reads as right now. */
private data class ScopeChoice(val scope: Scope, val label: String)

private class QueuePanel(private val project: Project, private val toolWindow: ToolWindow) :
    JPanel(BorderLayout()), UiDataProvider, Disposable {

    private val core: MoonlightApi get() = MoonlightApi.getInstance()

    private val model = CollectionListModel<ReviewQueueItem>()
    private val list = JBList(model).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
        cellRenderer = ItemRenderer { id -> if (showsOneSession()) null else sessionName(id) }
        emptyText.text = "Nothing pending review"
    }
    private val preview = JBTextArea().apply {
        isEditable = false
        font = Font(Font.MONOSPACED, Font.PLAIN, JBUI.Fonts.label().size)
        border = JBUI.Borders.empty(4)
    }
    private val scopes = DefaultComboBoxModel<ScopeChoice>()
    private val selector = ComboBox(scopes).apply {
        renderer = object : SimpleListCellRenderer<ScopeChoice>() {
            override fun customize(list: JList<out ScopeChoice>, value: ScopeChoice?, index: Int, selected: Boolean, hasFocus: Boolean) {
                text = value?.label ?: ""
            }
        }
        toolTipText = "Which session's changes this tab reviews — Send review goes to it"
    }

    /** What the operator picked; [Scope.Active] until they pick something else. */
    private var scope: Scope = Scope.Active

    /** Set while the selector is rebuilt, so rebuilding is not read as a pick. */
    private var rebuilding = false

    /** The whole fleet's queue, as last read; the list shows the slice [scope] selects. */
    private var all: List<ReviewQueueItem> = emptyList()
    private val loading = AtomicBoolean()

    init {
        val manager = ActionManager.getInstance()
        val toolbar = manager.createActionToolbar("MoonlightReviewQueue", manager.getAction("Moonlight.Review.Toolbar") as ActionGroup, true)
        toolbar.targetComponent = this
        val header = JPanel(BorderLayout(JBUI.scale(6), 0)).apply {
            border = JBUI.Borders.empty(2, 6)
            add(JBLabel("Reviewing:"), BorderLayout.WEST)
            add(selector, BorderLayout.CENTER)
        }
        add(JPanel(BorderLayout()).apply {
            add(toolbar.component, BorderLayout.NORTH)
            add(header, BorderLayout.SOUTH)
        }, BorderLayout.NORTH)
        add(JBSplitter(true, 0.45f).apply {
            firstComponent = JBScrollPane(list)
            secondComponent = JBScrollPane(preview)
        }, BorderLayout.CENTER)

        selector.addActionListener {
            if (rebuilding) return@addActionListener
            val picked = selector.selectedItem as? ScopeChoice ?: return@addActionListener
            scope = picked.scope
            render()
        }
        list.addListSelectionListener {
            if (it.valueIsAdjusting) return@addListSelectionListener
            val item = list.selectedValue
            // Showing several sessions: comments go to the session of the file picked.
            if (item != null && !item.ignored && ReviewState.of(project).target != item.sessionId) {
                ReviewState.of(project).setTarget(item.sessionId)
            }
            loadPreview(item)
        }
        list.addMouseListener(object : MouseAdapter() {
            override fun mouseClicked(e: MouseEvent) {
                if (e.clickCount == 2) list.selectedValue?.takeIf { !it.ignored }?.let { ReviewOps.openDiff(project, it) }
            }
        })
        PopupHandler.installPopupMenu(list, "Moonlight.Review.QueuePopup", "MoonlightReviewQueuePopup")

        project.messageBus.connect(this).subscribe(ReviewListener.TOPIC, ReviewListener { reload() })
        ApplicationManager.getApplication().messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            // The active session can change without the queue changing; re-slice either way.
            override fun sessionsChanged() = reload()
        })
        reload()
    }

    /** Re-read the queue. Coalesced: the poll can fire faster than a slow backend answers. */
    fun reload() {
        if (!loading.compareAndSet(false, true)) return
        ApplicationManager.getApplication().executeOnPooledThread {
            // Without diffs: the list needs file names, and the fleet's diffs together run to
            // megabytes. The selected file's diff is fetched on its own.
            val result = runCatching { core.control.reviewQueue(diffs = false) }
            ApplicationManager.getApplication().invokeLater({
                loading.set(false)
                result.onSuccess {
                    all = it
                    render()
                }.onFailure { e ->
                    model.removeAll()
                    list.emptyText.setText("Review queue unavailable: ${e.message}", StatusText.DEFAULT_ATTRIBUTES)
                }
            }, project.disposed)
        }
    }

    /** The one session [scope] stands for now, or `null` when it shows several. */
    private fun resolved(): String? = when (val s = scope) {
        Scope.Active -> core.activeSession(project)?.sessionId
        is Scope.Session -> s.id
        Scope.Project, Scope.All -> null
    }

    private fun showsOneSession(): Boolean = resolved() != null

    private fun ownsHere(sessionId: String): Boolean = core.ownsSession(project, sessionId)

    private fun sessionName(id: String): String {
        val sessions = core.sessions()
        return sessions.firstOrNull { it.sessionId == id }?.let { sessionLabel(it, sessions) }
            ?: all.firstOrNull { it.sessionId == id }?.sessionTitle
            ?: shortId(id)
    }

    private fun render() {
        val session = resolved()
        rebuildSelector()
        val shown = when {
            scope == Scope.All -> all
            session != null -> all.filter { it.sessionId == session }
            // This project — chosen, or what Active falls back to without one active session.
            else -> all.filter { ownsHere(it.sessionId) }
        }.sortedWith(compareBy<ReviewQueueItem> { it.ignored }.thenBy { it.sessionId }) // warnings after the work
        // Comments written here go to the session shown — not to whichever diff was opened last.
        if (session != null && ReviewState.of(project).target != session) ReviewState.of(project).setTarget(session)
        retitle(shown.count { !it.ignored })
        list.emptyText.text = "Nothing pending review"
        val selected = list.selectedValue?.let { it.sessionId to it.filePath }
        if (shown != model.items) {
            model.replaceAll(shown)
            val again = shown.indexOfFirst { (it.sessionId to it.filePath) == selected }
            if (again >= 0) list.selectedIndex = again else preview.text = ""
        }
    }

    /**
     * Active first, then every adopted session with something queued, then all. Rebuilt on
     * each read so the counts are current; the operator's pick survives the rebuild.
     */
    private fun rebuildSelector() {
        val sessions = core.sessions()
        val counts = all.filter { !it.ignored }.groupingBy { it.sessionId }.eachCount()
        fun counted(id: String) = "${sessionName(id)} (${counts[id] ?: 0})"
        val mine = counts.filterKeys(::ownsHere)
        val projectLabel = "This project — ${mine.size} session(s) (${mine.values.sum()})"

        val active = core.activeSession(project)?.sessionId
        val choices = buildList {
            add(ScopeChoice(Scope.Active, if (active != null) "Active session — ${counted(active)}" else "No single active session · $projectLabel"))
            add(ScopeChoice(Scope.Project, projectLabel))
            val ids = (sessions.filter { it.adopted }.map { it.sessionId } + counts.keys).distinct()
                .filter { (counts[it] ?: 0) > 0 || it == (scope as? Scope.Session)?.id }
                .sortedByDescending { counts[it] ?: 0 }
            ids.forEach { add(ScopeChoice(Scope.Session(it), counted(it))) }
            add(ScopeChoice(Scope.All, "All sessions (${counts.values.sum()})"))
        }
        if (choices == (0 until scopes.size).map(scopes::getElementAt)) return
        rebuilding = true
        try {
            scopes.removeAllElements()
            choices.forEach(scopes::addElement)
            selector.selectedItem = choices.firstOrNull { it.scope == scope } ?: choices.first()
        } finally {
            rebuilding = false
        }
    }

    /** The tab says how much is waiting, so the count reads without opening it. */
    private fun retitle(count: Int) {
        val content = toolWindow.contentManagerIfCreated?.contents?.firstOrNull { it.component === this } ?: return
        content.displayName = if (count > 0) "${ReviewTab.TITLE} ($count)" else ReviewTab.TITLE
    }

    /** One file's diff, read when it is selected. Stale answers are dropped. */
    private fun loadPreview(item: ReviewQueueItem?) {
        if (item == null) {
            preview.text = ""
            return
        }
        if (item.ignored) {
            preview.text = "Git-ignored: ${item.ignoredFiles} file(s) written under ${item.filePath}\n\n" +
                "Ignored files are never pushed, so they are not reviewed — no diffs. This is a heads-up that " +
                "they changed (build output, dependencies, a local .env…).\nMark reviewed to clear it."
            return
        }
        preview.text = "Loading the diff…"
        ApplicationManager.getApplication().executeOnPooledThread {
            val text = runCatching {
                val full = core.control.reviewQueue(item.sessionId, item.filePath, diffs = true).firstOrNull()
                when {
                    full == null -> "This file has left the queue."
                    full.diffOmitted -> "The diff is too large to preview here. Double-click the file to open it in the diff view."
                    full.diff.isEmpty() -> "No diff to show: the baseline or the file could not be read."
                    else -> full.diff
                }
            }.getOrElse { "Diff unavailable: ${it.message}" }
            ApplicationManager.getApplication().invokeLater({
                if (list.selectedValue?.let { it.sessionId to it.filePath } != item.sessionId to item.filePath) return@invokeLater
                preview.text = text
                preview.caretPosition = 0
            }, project.disposed)
        }
    }

    override fun uiDataSnapshot(sink: DataSink) {
        val item = list.selectedValue
        if (item != null) sink[ReviewDataKeys.QUEUE_ITEM] = item
        // The selected file's session, else the one the tab is reviewing.
        (item?.sessionId ?: resolved())?.let { sink[MoonlightDataKeys.SESSION_ID] = it }
    }

    override fun dispose() = Unit
}

/** `sessionOf` names a row's session when the list shows several, else returns `null`. */
private class ItemRenderer(private val sessionOf: (String) -> String?) : ColoredListCellRenderer<ReviewQueueItem>() {
    override fun customizeCellRenderer(list: JList<out ReviewQueueItem>, value: ReviewQueueItem, index: Int, selected: Boolean, hasFocus: Boolean) {
        val path = Path.of(value.filePath)
        if (value.ignored) {
            // A warning, not work: what changed under an ignored path, without a diff.
            icon = AllIcons.General.Warning
            append(path.fileName.toString() + if (value.filePath.endsWith("/")) "/" else "")
            append("  ${path.parent ?: ""}", SimpleTextAttributes.GRAYED_ATTRIBUTES)
            val who = sessionOf(value.sessionId)?.let { "$it · " } ?: ""
            append("  — ${who}git-ignored · ${value.ignoredFiles} file(s) changed, not reviewed", SimpleTextAttributes.GRAY_ITALIC_ATTRIBUTES)
            toolTipText = value.filePath
            return
        }
        icon = null
        append(path.fileName.toString())
        append("  ${path.parent ?: ""}", SimpleTextAttributes.GRAYED_ATTRIBUTES)
        val tags = buildList {
            sessionOf(value.sessionId)?.let(::add)
            if (value.created) add("new file")
            if (value.fromHead) add("baseline from HEAD")
            add("${value.touches} write(s) · ${value.tool}")
        }
        append("  — ${tags.joinToString(" · ")}", SimpleTextAttributes.GRAY_ITALIC_ATTRIBUTES)
        toolTipText = value.filePath
    }
}
