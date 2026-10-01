package io.github.titouanfreville.moonlight.review

import com.intellij.diff.DiffContentFactory
import com.intellij.diff.DiffManager
import com.intellij.diff.DiffRequestPanel
import com.intellij.diff.contents.DiffContent
import com.intellij.diff.contents.DocumentContent
import com.intellij.diff.requests.SimpleDiffRequest
import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.DataSink
import com.intellij.openapi.actionSystem.UiDataProvider
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.fileEditor.FileEditorState
import com.intellij.openapi.fileTypes.FileTypeManager
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.testFramework.LightVirtualFile
import com.intellij.ui.ColoredTreeCellRenderer
import com.intellij.ui.JBColor
import com.intellij.ui.JBSplitter
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.treeStructure.Tree
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.UIUtil
import com.intellij.util.ui.tree.TreeUtil
import io.github.titouanfreville.moonlight.client.BaselineView
import io.github.titouanfreville.moonlight.client.ReviewQueueItem
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightDataKeys
import java.awt.BorderLayout
import java.beans.PropertyChangeListener
import java.nio.file.Path
import javax.swing.JComponent
import javax.swing.JPanel
import javax.swing.JTree
import javax.swing.tree.DefaultMutableTreeNode
import javax.swing.tree.DefaultTreeModel
import javax.swing.tree.TreeSelectionModel

/**
 * One file a review covers. `item` is the queue entry when the file came from the queue;
 * a review reopened from its comments has only the path.
 */
data class ReviewedFile(val path: String, val item: ReviewQueueItem?)

/**
 * A session's review as one editor tab: every changed file in a tree on the left — grouped by
 * directory, like a folder diff — and the selected file's diff on the right. Comments are
 * written in the diff (select lines, right-click), and drawn there as threads.
 */
class ReviewDiffFile(
    val sessionId: String,
    val title: String,
    @Volatile var files: List<ReviewedFile>,
) : LightVirtualFile("Review: $title") {
    /** The file to show first — set before (re)opening. */
    @Volatile
    var focus: String? = null

    override fun isWritable(): Boolean = false

    companion object {
        /** Open — or bring forward and refresh — the review tab for `sessionId`. Call on the EDT. */
        fun open(project: Project, sessionId: String, title: String, files: List<ReviewedFile>, focus: String? = null) {
            val manager = FileEditorManager.getInstance(project)
            val existing = manager.openFiles.filterIsInstance<ReviewDiffFile>().firstOrNull { it.sessionId == sessionId }
            val file = existing ?: ReviewDiffFile(sessionId, title, files)
            file.files = files
            file.focus = focus ?: files.firstOrNull()?.path
            manager.openFile(file, true)
            // An editor already open re-reads its file list.
            manager.getEditors(file).filterIsInstance<ReviewDiffEditor>().forEach { it.reload() }
        }
    }
}

class ReviewDiffEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean = file is ReviewDiffFile
    override fun acceptRequiresReadAction(): Boolean = false
    override fun createEditor(project: Project, file: VirtualFile): FileEditor = ReviewDiffEditor(project, file as ReviewDiffFile)
    override fun getEditorTypeId(): String = "moonlight-review-diff"
    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR
}

/** A tree row: a directory, or a file with what happened to it. */
private sealed interface Row {
    data class Dir(val label: String) : Row

    /** `deleted` is read once, when the tree is built — not on every paint. */
    data class File(val file: ReviewedFile, val name: String, val deleted: Boolean) : Row {
        val created: Boolean get() = file.item?.created == true
    }
}

internal class ReviewDiffEditor(private val project: Project, private val file: ReviewDiffFile) :
    UserDataHolderBase(), FileEditor {

    private val diff: DiffRequestPanel = DiffManager.getInstance().createRequestPanel(project, this, null)
    private val root = DefaultMutableTreeNode()
    private val model = DefaultTreeModel(root)
    private val tree = Tree(model).apply {
        isRootVisible = false
        showsRootHandles = true
        selectionModel.selectionMode = TreeSelectionModel.SINGLE_TREE_SELECTION
        cellRenderer = RowRenderer()
    }
    private var shown: String? = null

    private val panel = object : JPanel(BorderLayout()), UiDataProvider {
        override fun uiDataSnapshot(sink: DataSink) {
            sink[MoonlightDataKeys.SESSION_ID] = file.sessionId
            selected()?.file?.item?.let { sink[ReviewDataKeys.QUEUE_ITEM] = it }
        }
    }

    init {
        val manager = ActionManager.getInstance()
        val toolbar = manager.createActionToolbar("MoonlightReviewDiff", manager.getAction("Moonlight.Review.DiffToolbar") as ActionGroup, true)
        toolbar.targetComponent = panel
        val left = JPanel(BorderLayout()).apply {
            add(toolbar.component, BorderLayout.NORTH)
            add(JBScrollPane(tree), BorderLayout.CENTER)
        }
        val hint = JBLabel("Select lines in the diff, right-click › MoonlightCode: Add Review Comment… — or use the gutter mark on a thread to answer it.").apply {
            border = JBUI.Borders.empty(4, 8)
            foreground = UIUtil.getContextHelpForeground()
        }
        val right = JPanel(BorderLayout()).apply {
            add(hint, BorderLayout.NORTH)
            add(diff.component, BorderLayout.CENTER)
        }
        panel.add(JBSplitter(false, 0.25f).apply {
            firstComponent = left
            secondComponent = right
            setHonorComponentsMinimumSize(false)
        }, BorderLayout.CENTER)

        tree.addTreeSelectionListener { selected()?.let { show(it.file) } }
        reload()
    }

    private fun selected(): Row.File? = (tree.lastSelectedPathComponent as? DefaultMutableTreeNode)?.userObject as? Row.File

    /** Rebuild the tree from the file's list, and show its focus. */
    fun reload() {
        root.removeAllChildren()
        val files = file.files
        // Paths relative to the deepest directory every file shares, so rows read short.
        val common = files.map { Path.of(it.path).parent ?: Path.of("/") }.reduceOrNull { a, b ->
            var p: Path? = a
            while (p != null && !b.startsWith(p)) p = p.parent
            p ?: Path.of("/")
        }
        val dirs = HashMap<String, DefaultMutableTreeNode>()
        for (f in files.sortedBy { it.path }) {
            val rel = common?.relativize(Path.of(f.path))?.toString() ?: f.path
            val dir = rel.substringBeforeLast('/', "")
            val parent = if (dir.isEmpty()) root else dirs.getOrPut(dir) {
                DefaultMutableTreeNode(Row.Dir(dir)).also { root.add(it) }
            }
            parent.add(DefaultMutableTreeNode(Row.File(f, rel.substringAfterLast('/'), !java.io.File(f.path).exists())))
        }
        model.reload()
        TreeUtil.expandAll(tree)
        val focus = file.focus
        val target = TreeUtil.treeNodeTraverser(root).filter(DefaultMutableTreeNode::class.java)
            .firstOrNull { (it.userObject as? Row.File)?.file?.path == focus }
            ?: TreeUtil.treeNodeTraverser(root).filter(DefaultMutableTreeNode::class.java).firstOrNull { it.userObject is Row.File }
        target?.let { TreeUtil.selectNode(tree, it) }
    }

    /** Fetch the file's baseline off the EDT, then show its diff. */
    private fun show(f: ReviewedFile) {
        if (shown == f.path) return
        shown = f.path
        ApplicationManager.getApplication().executeOnPooledThread {
            val baseline = try {
                MoonlightApi.getInstance().control.baseline(file.sessionId, f.path)
            } catch (e: Exception) {
                BaselineView.Unavailable(e.message ?: "unreachable")
            }
            val local = LocalFileSystem.getInstance().refreshAndFindFileByPath(f.path)
            ApplicationManager.getApplication().invokeLater({
                if (shown != f.path) return@invokeLater // another file was picked meanwhile
                diff.setRequest(diffRequest(project, file.sessionId, file.title, f.path, baseline, local))
            }, project.disposed)
        }
    }

    override fun getComponent(): JComponent = panel
    override fun getPreferredFocusedComponent(): JComponent = tree
    override fun getName(): String = "Review"
    override fun getFile(): VirtualFile = file
    override fun setState(state: FileEditorState) = Unit
    override fun isModified(): Boolean = false
    override fun isValid(): Boolean = true
    override fun addPropertyChangeListener(listener: PropertyChangeListener) = Unit
    override fun removePropertyChangeListener(listener: PropertyChangeListener) = Unit
    override fun dispose() = Disposer.dispose(diff)
}

/**
 * One file's diff: the stored before side on the left, read-only; the real file on the right,
 * editable — fixing what you are reviewing is allowed, and the comments follow the file. Both
 * sides are registered with [ReviewState], which is what draws threads on them and lets the
 * comment actions find them. Call on the EDT.
 */
internal fun diffRequest(
    project: Project,
    sessionId: String,
    title: String,
    path: String,
    baseline: BaselineView,
    local: VirtualFile?,
): SimpleDiffRequest {
    val state = ReviewState.of(project)
    val factory = DiffContentFactory.getInstance()
    val fileType = FileTypeManager.getInstance().getFileTypeByFileName(Path.of(path).fileName.toString())
    val (text, beforeTitle) = when (baseline) {
        is BaselineView.Content -> baseline.text to "Before"
        is BaselineView.FromHead -> baseline.text to "Before (from VCS HEAD — may include earlier uncommitted edits)"
        BaselineView.Created -> "" to "Before (file created by the session)"
        is BaselineView.Unavailable -> "" to "Before (unavailable: ${baseline.reason})"
    }
    val before = factory.create(project, text, fileType)
    (before as? DocumentContent)?.document?.let {
        it.setReadOnly(true)
        state.registerBaseline(it, sessionId, path)
    }
    state.register(sessionId, path)
    val after: DiffContent = local?.let { factory.create(project, it) } ?: factory.createEmpty()
    return SimpleDiffRequest("$title — ${Path.of(path).fileName}", before, after, beforeTitle, if (local == null) "After (deleted)" else "After")
}

private class RowRenderer : ColoredTreeCellRenderer() {
    override fun customizeCellRenderer(tree: JTree, value: Any?, selected: Boolean, expanded: Boolean, leaf: Boolean, row: Int, hasFocus: Boolean) {
        when (val r = (value as? DefaultMutableTreeNode)?.userObject) {
            is Row.Dir -> {
                icon = AllIcons.Nodes.Folder
                append(r.label, SimpleTextAttributes.REGULAR_ATTRIBUTES)
            }
            is Row.File -> {
                icon = FileTypeManager.getInstance().getFileTypeByFileName(r.name).icon
                // The colours the VCS views use: added green, deleted grey, modified blue.
                val attrs = when {
                    r.deleted -> SimpleTextAttributes(SimpleTextAttributes.STYLE_STRIKEOUT, JBColor.GRAY)
                    r.created -> SimpleTextAttributes(SimpleTextAttributes.STYLE_PLAIN, JBColor.namedColor("FileColor.Green", JBColor(0x297A36, 0x6A9955)))
                    else -> SimpleTextAttributes(SimpleTextAttributes.STYLE_PLAIN, JBColor(0x2457A8, 0x6897BB))
                }
                append(r.name, attrs)
                when {
                    r.deleted -> append("  deleted", SimpleTextAttributes.GRAYED_ATTRIBUTES)
                    r.created -> append("  new", SimpleTextAttributes.GRAYED_ATTRIBUTES)
                }
            }
        }
    }
}
