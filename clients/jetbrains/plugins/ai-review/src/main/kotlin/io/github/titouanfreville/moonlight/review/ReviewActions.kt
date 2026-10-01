package io.github.titouanfreville.moonlight.review

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.actionSystem.ToggleAction
import com.intellij.openapi.project.DumbAware
import io.github.titouanfreville.moonlight.client.ReviewQueueItem
import io.github.titouanfreville.moonlight.core.MoonlightDataKeys
import io.github.titouanfreville.moonlight.core.MoonlightToolWindow

/** Base for review actions: background update, available while indexing. */
abstract class ReviewAction : AnAction(), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        e.presentation.isEnabledAndVisible = e.project != null
    }
}

class ReviewSessionChangesAction : ReviewAction() {
    override fun actionPerformed(e: AnActionEvent) {
        ReviewOps.openSessionReview(e.project ?: return, e.getData(MoonlightDataKeys.SESSION_ID))
    }
}

class OpenReviewQueueAction : ReviewAction() {
    override fun actionPerformed(e: AnActionEvent) {
        MoonlightToolWindow.select(e.project ?: return, ReviewTab.TITLE)
    }
}

class RefreshReviewAction : ReviewAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val state = ReviewState.of(e.project ?: return)
        state.target?.let(state::reload)
        state.changed()
    }
}

class SubmitReviewAction : ReviewAction() {
    override fun update(e: AnActionEvent) {
        val project = e.project
        e.presentation.isVisible = project != null
        e.presentation.isEnabled = project != null && ReviewOps.submitTarget(project) != null
    }

    override fun actionPerformed(e: AnActionEvent) {
        ReviewOps.submit(e.project ?: return)
    }
}

class ReopenReviewAction : ReviewAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val session = e.getData(MoonlightDataKeys.SESSION_ID) ?: ReviewOps.submitTarget(project)
        if (session == null) {
            ReviewOps.notify(project, "Pick a session first — select it in the MoonlightCode tool window, or make it the active session.")
            return
        }
        ReviewOps.reopenFromComments(project, session)
    }
}

class ToggleResolvedAction : ToggleAction(), DumbAware {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun isSelected(e: AnActionEvent): Boolean = e.project?.let { ReviewState.of(it).showResolved } ?: true

    override fun setSelected(e: AnActionEvent, state: Boolean) {
        val review = ReviewState.of(e.project ?: return)
        if (review.showResolved != state) review.toggleResolved()
    }
}

/** Comment on the selection, or the caret line, of a file under review. */
open class AddReviewCommentAction(private val wholeFile: Boolean = false) : ReviewAction() {
    override fun update(e: AnActionEvent) {
        val project = e.project
        val editor = e.getData(CommonDataKeys.EDITOR)
        // Shown only where it can work: an editor on a file opened for review.
        e.presentation.isEnabledAndVisible =
            project != null && editor != null && ReviewState.of(project).locate(editor.document) != null
    }

    override fun actionPerformed(e: AnActionEvent) {
        ReviewOps.addComment(e.project ?: return, e.getData(CommonDataKeys.EDITOR) ?: return, wholeFile)
    }
}

class AddFileReviewCommentAction : AddReviewCommentAction(wholeFile = true)

/** An action on the queue item selected in the review tool window. */
abstract class QueueItemAction : ReviewAction() {
    /** Whether the action means anything for a git-ignored warning — only settling it does. */
    open val appliesToIgnored: Boolean = false

    override fun update(e: AnActionEvent) {
        val item = e.getData(ReviewDataKeys.QUEUE_ITEM)
        e.presentation.isVisible = e.project != null
        e.presentation.isEnabled = e.project != null && item != null && (appliesToIgnored || !item.ignored)
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val item = e.getData(ReviewDataKeys.QUEUE_ITEM) ?: return
        perform(project, item) { ReviewState.of(project).changed() }
    }

    abstract fun perform(project: com.intellij.openapi.project.Project, item: ReviewQueueItem, then: () -> Unit)
}

class OpenQueueDiffAction : QueueItemAction() {
    override fun perform(project: com.intellij.openapi.project.Project, item: ReviewQueueItem, then: () -> Unit) =
        ReviewOps.openDiff(project, item)
}

class MarkReviewedAction : QueueItemAction() {
    override val appliesToIgnored: Boolean = true

    override fun perform(project: com.intellij.openapi.project.Project, item: ReviewQueueItem, then: () -> Unit) =
        ReviewOps.markReviewed(project, item, then)
}

class IgnoreFileAction : QueueItemAction() {
    override fun perform(project: com.intellij.openapi.project.Project, item: ReviewQueueItem, then: () -> Unit) =
        ReviewOps.ignore(project, item, then)
}

class RejectChangeAction : QueueItemAction() {
    override fun perform(project: com.intellij.openapi.project.Project, item: ReviewQueueItem, then: () -> Unit) =
        ReviewOps.reject(project, item, then)
}
