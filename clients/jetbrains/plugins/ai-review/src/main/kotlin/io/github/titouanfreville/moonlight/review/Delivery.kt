package io.github.titouanfreville.moonlight.review

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import java.awt.datatransfer.StringSelection

/**
 * The hand-off: the exact message to give the session, one click away.
 *
 * Shown after every submit, including a delivery we believe worked — typing into a terminal
 * is not proof the agent was at its prompt to read it, and the session may not be in a
 * terminal we own at all. A sticky notification rather than a dialog, so it waits for the
 * operator instead of interrupting them.
 */
object Delivery {
    private const val GROUP = "MoonlightCode Review"

    fun copy(text: String) = CopyPasteManager.getInstance().setContents(StringSelection(text))

    fun handOff(project: Project, pointer: String, reviewPath: String?, summary: String, type: NotificationType) {
        val notification = NotificationGroupManager.getInstance().getNotificationGroup(GROUP)
            .createNotification("MoonlightCode review", "${escape(summary)}<br><br><i>${escape(pointer)}</i>", type)
        notification.addAction(NotificationAction.createSimple("Copy message") { copy(pointer) })
        if (reviewPath != null) {
            notification.addAction(NotificationAction.createSimple("Open review file") {
                LocalFileSystem.getInstance().refreshAndFindFileByPath(reviewPath)?.let {
                    FileEditorManager.getInstance(project).openFile(it, true)
                }
            })
        }
        notification.notify(project)
    }
}
