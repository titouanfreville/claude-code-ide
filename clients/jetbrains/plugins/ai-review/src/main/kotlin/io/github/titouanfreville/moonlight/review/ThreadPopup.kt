package io.github.titouanfreville.moonlight.review

import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.popup.JBPopup
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.ui.awt.RelativePoint
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.panels.VerticalLayout
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.UIUtil
import io.github.titouanfreville.moonlight.client.CommentThread
import java.awt.BorderLayout
import java.awt.FlowLayout
import java.awt.event.MouseEvent
import javax.swing.JButton
import javax.swing.JPanel

/**
 * One thread, read in full, with what can be done to it: reply, resolve or reopen, edit,
 * delete. A thread is a conversation, not a verdict — the agent answers through the
 * tracker, and the reviewer answers here.
 */
object ThreadPopup {
    fun show(project: Project, thread: CommentThread, event: AnActionEvent) {
        lateinit var popup: JBPopup
        fun act(block: () -> Unit) = JButton().apply {
            addActionListener {
                popup.cancel()
                block()
            }
        }

        val messages = JPanel(VerticalLayout(JBUI.scale(8))).apply {
            border = JBUI.Borders.empty(8)
            for (c in listOf(thread.root) + thread.replies) {
                add(JBLabel("<html><b>${authorName(c)}</b> <font color='gray'>${commentFlags(c)}</font><br>${escape(c.body)}</html>").apply {
                    setAllowAutoWrapping(true)
                })
            }
        }
        val where = if (thread.root.scope.name == "File") "Whole file" else "Lines ${thread.root.startLine}–${thread.root.endLine}"
        val buttons = JPanel(FlowLayout(FlowLayout.LEFT)).apply {
            add(act { ReviewOps.reply(project, thread) }.apply { text = "Reply…" })
            add(act { ReviewOps.setResolved(project, thread, !thread.root.resolved) }.apply { text = if (thread.root.resolved) "Reopen" else "Resolve" })
            add(act { ReviewOps.edit(project, thread) }.apply { text = "Edit…" })
            add(act { ReviewOps.delete(project, thread) }.apply { text = "Delete" })
        }
        val panel = JPanel(BorderLayout()).apply {
            add(JBLabel(where).apply {
                border = JBUI.Borders.empty(6, 8, 0, 8)
                foreground = UIUtil.getContextHelpForeground()
            }, BorderLayout.NORTH)
            add(JBScrollPane(messages).apply {
                border = JBUI.Borders.empty()
                preferredSize = JBUI.size(460, minOf(360, 60 + 70 * (1 + thread.replies.size)))
            }, BorderLayout.CENTER)
            add(buttons, BorderLayout.SOUTH)
        }
        popup = JBPopupFactory.getInstance().createComponentPopupBuilder(panel, buttons)
            .setTitle("Review thread")
            .setResizable(true)
            .setMovable(true)
            .setRequestFocus(true)
            .createPopup()
        val mouse = event.inputEvent as? MouseEvent
        if (mouse != null) popup.show(RelativePoint(mouse)) else popup.showInFocusCenter()
    }
}
