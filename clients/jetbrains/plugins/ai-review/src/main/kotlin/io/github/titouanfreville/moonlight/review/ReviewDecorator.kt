package io.github.titouanfreville.moonlight.review

import com.intellij.icons.AllIcons
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorCustomElementRenderer
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.editor.Inlay
import com.intellij.openapi.editor.colors.EditorFontType
import com.intellij.openapi.editor.event.EditorFactoryEvent
import com.intellij.openapi.editor.event.EditorFactoryListener
import com.intellij.openapi.editor.markup.GutterIconRenderer
import com.intellij.openapi.editor.markup.HighlighterLayer
import com.intellij.openapi.editor.markup.HighlighterTargetArea
import com.intellij.openapi.editor.markup.RangeHighlighter
import com.intellij.openapi.editor.markup.TextAttributes
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.Key
import com.intellij.ui.JBColor
import com.intellij.util.ui.JBUI
import io.github.titouanfreville.moonlight.client.CommentAuthor
import io.github.titouanfreville.moonlight.client.CommentThread
import io.github.titouanfreville.moonlight.client.ReviewComment
import io.github.titouanfreville.moonlight.core.MoonlightListener
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.Rectangle
import java.awt.RenderingHints
import javax.swing.Icon

/**
 * Draws review threads into every editor that shows a file under review — the diff's two
 * sides, and the same file opened normally — the JetBrains counterpart of VS Code's native
 * comment threads.
 *
 * Public API only: a gutter mark per thread (click for the thread and its actions) and a
 * block inlay under the line carrying a short summary, so a thread reads in place without
 * opening anything. The platform's own review UI (`collaboration-tools`) is internal API, so
 * it is not used.
 */
@Service(Service.Level.PROJECT)
class ReviewDecorator(private val project: Project) : Disposable {
    private val decorated = HashSet<Editor>()

    fun start() {
        EditorFactory.getInstance().addEditorFactoryListener(object : EditorFactoryListener {
            override fun editorCreated(event: EditorFactoryEvent) {
                if (event.editor.project == project) decorate(event.editor)
            }

            override fun editorReleased(event: EditorFactoryEvent) {
                clear(event.editor)
                decorated.remove(event.editor)
            }
        }, this)
        project.messageBus.connect(this).subscribe(ReviewListener.TOPIC, ReviewListener { redecorate() })
        // An agent's answer arrives through the tracker; the fleet poll is the cue to look.
        ApplicationManager.getApplication().messageBus.connect(this).subscribe(MoonlightListener.TOPIC, object : MoonlightListener {
            override fun sessionsChanged() {
                ReviewState.of(project).target?.let(ReviewState.of(project)::reload)
            }
        })
        EditorFactory.getInstance().allEditors.filter { it.project == project }.forEach(::decorate)
    }

    /** Redraw every editor currently showing a reviewed file. */
    fun redecorate() {
        decorated.toList().forEach(::decorate)
        EditorFactory.getInstance().allEditors.filter { it.project == project && it !in decorated }.forEach(::decorate)
    }

    private fun decorate(editor: Editor) {
        if (editor.isDisposed) return
        clear(editor)
        val state = ReviewState.of(project)
        val location = state.locate(editor.document) ?: return
        decorated += editor
        val document = editor.document
        val marks = mutableListOf<Any>()
        for (anchor in anchorsFor(state.threads(location.sessionId), location.path, location.side, document.lineCount, state.showResolved)) {
            if (document.lineCount == 0) break
            val start = document.getLineStartOffset(anchor.line)
            val end = document.getLineEndOffset(anchor.line)
            val highlighter = editor.markupModel.addRangeHighlighter(
                start, end, HighlighterLayer.LAST, TextAttributes(), HighlighterTargetArea.LINES_IN_RANGE,
            )
            highlighter.gutterIconRenderer = ThreadGutter(project, anchor.thread, anchor.fileWide)
            marks += highlighter
            editor.inlayModel.addBlockElement(end, true, false, 0, ThreadSummary(anchor.thread, anchor.fileWide))?.let { marks += it }
        }
        editor.putUserData(MARKS, marks)
    }

    private fun clear(editor: Editor) {
        editor.getUserData(MARKS)?.forEach { mark ->
            when (mark) {
                is RangeHighlighter -> mark.dispose()
                is Inlay<*> -> Disposer.dispose(mark)
            }
        }
        editor.putUserData(MARKS, null)
    }

    override fun dispose() {
        decorated.forEach(::clear)
        decorated.clear()
    }

    companion object {
        private val MARKS = Key.create<List<Any>>("moonlight.review.marks")

        fun of(project: Project): ReviewDecorator = project.service()
    }
}

class ReviewStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        ApplicationManager.getApplication().invokeLater({ ReviewDecorator.of(project).start() }, project.disposed)
    }
}

/** The flags a comment carries, in words — who wrote it and where it stands. */
fun commentFlags(c: ReviewComment): String {
    val flags = mutableListOf<String>()
    // An answer was never "queued for delivery" — it travels the other way.
    if (c.author == CommentAuthor.Agent) flags += "reply" else if (c.sent) flags += "delivered" else if (!c.resolved) flags += "queued"
    if (c.resolved) flags += "resolved"
    // Anchor drift, not elapsed time, is what makes a queued review stale.
    if (c.outdated) flags += "⚠ outdated"
    return flags.joinToString(" · ")
}

fun authorName(c: ReviewComment): String = if (c.author == CommentAuthor.Agent) "Agent" else "You"

private class ThreadGutter(
    private val project: Project,
    private val thread: CommentThread,
    private val fileWide: Boolean,
) : GutterIconRenderer() {
    override fun getIcon(): Icon = when {
        thread.root.resolved -> AllIcons.RunConfigurations.TestPassed
        thread.replies.any { it.author == CommentAuthor.Agent } -> AllIcons.General.Balloon
        else -> AllIcons.Toolwindows.ToolWindowMessages
    }

    override fun getTooltipText(): String {
        val where = if (fileWide) "File comment" else "Lines ${thread.root.startLine}–${thread.root.endLine}"
        return "<html><b>$where</b> · ${commentFlags(thread.root)}<br>${escape(thread.root.body.take(300))}" +
            (if (thread.replies.isNotEmpty()) "<br><i>${thread.replies.size} repl${if (thread.replies.size == 1) "y" else "ies"}</i>" else "") +
            "<br><br>Click to open the thread.</html>"
    }

    override fun getClickAction(): AnAction = object : AnAction() {
        override fun actionPerformed(e: AnActionEvent) = ThreadPopup.show(project, thread, e)
    }

    override fun isNavigateAction(): Boolean = true
    override fun getAlignment(): Alignment = Alignment.RIGHT
    override fun equals(other: Any?): Boolean = other is ThreadGutter && other.thread == thread
    override fun hashCode(): Int = thread.hashCode()
}

/**
 * A few lines under the code: who said what. Collapsed to one line once resolved — the
 * record stays, the noise goes.
 */
private class ThreadSummary(private val thread: CommentThread, private val fileWide: Boolean) : EditorCustomElementRenderer {
    private fun lines(): List<String> {
        val prefix = if (fileWide) "File · " else ""
        if (thread.root.resolved) return listOf("✓ ${prefix}Resolved — ${thread.root.body.lineSequence().first().take(120)}")
        val all = (listOf(thread.root) + thread.replies).map { c ->
            "${authorName(c)}${commentFlags(c).let { if (it.isEmpty()) "" else " ($it)" }}: ${c.body.lineSequence().first().take(140)}"
        }
        val shown = all.take(MAX_LINES)
        return listOf(prefix + shown.first()) + shown.drop(1) + if (all.size > MAX_LINES) listOf("… ${all.size - MAX_LINES} more — click the gutter mark") else emptyList()
    }

    override fun calcWidthInPixels(inlay: Inlay<*>): Int {
        val metrics = inlay.editor.contentComponent.getFontMetrics(inlay.editor.colorsScheme.getFont(EditorFontType.ITALIC))
        return lines().maxOf(metrics::stringWidth) + JBUI.scale(PADDING * 4)
    }

    override fun calcHeightInPixels(inlay: Inlay<*>): Int = inlay.editor.lineHeight * lines().size + JBUI.scale(PADDING * 2)

    override fun paint(inlay: Inlay<*>, g: Graphics, targetRegion: Rectangle, textAttributes: TextAttributes) {
        val g2 = g.create() as Graphics2D
        try {
            g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
            val pad = JBUI.scale(PADDING)
            g2.color = if (thread.root.resolved) RESOLVED_BACKGROUND else BACKGROUND
            g2.fillRoundRect(targetRegion.x + pad, targetRegion.y + pad / 2, targetRegion.width - pad * 2, targetRegion.height - pad, pad * 2, pad * 2)
            g2.font = inlay.editor.colorsScheme.getFont(EditorFontType.ITALIC)
            g2.color = JBColor.namedColor("Label.infoForeground", JBColor.GRAY)
            val line = inlay.editor.lineHeight
            val ascent = inlay.editor.ascent
            lines().forEachIndexed { i, text ->
                g2.drawString(text, targetRegion.x + pad * 2, targetRegion.y + pad + i * line + ascent)
            }
        } finally {
            g2.dispose()
        }
    }

    private companion object {
        const val MAX_LINES = 3
        const val PADDING = 4
        val BACKGROUND = JBColor(0xEEF3FF, 0x2B3240)
        val RESOLVED_BACKGROUND = JBColor(0xF2F2F2, 0x2D2F31)
    }
}

internal fun escape(text: String): String =
    text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace("\n", "<br>")
