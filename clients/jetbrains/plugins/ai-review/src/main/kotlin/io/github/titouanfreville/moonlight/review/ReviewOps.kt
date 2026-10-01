package io.github.titouanfreville.moonlight.review

import com.intellij.notification.NotificationType
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.ui.popup.PopupStep
import com.intellij.openapi.ui.popup.util.BaseListPopupStep
import com.intellij.openapi.vfs.LocalFileSystem
import io.github.titouanfreville.moonlight.client.CommentScope
import io.github.titouanfreville.moonlight.client.CommentThread
import io.github.titouanfreville.moonlight.client.FeedbackDelivery
import io.github.titouanfreville.moonlight.client.ReviewQueueItem
import io.github.titouanfreville.moonlight.client.sessionLabel
import io.github.titouanfreville.moonlight.core.MoonlightApi
import io.github.titouanfreville.moonlight.core.MoonlightService
import java.nio.file.Path

/**
 * What the review surfaces do — open diffs, write and answer comments, send the review,
 * settle files in the queue. Every call to the daemon runs off the EDT, under a progress
 * task, and every outcome the operator could misread is said in words.
 */
object ReviewOps {
    private val core: MoonlightApi get() = MoonlightApi.getInstance()

    /**
     * Run `work` off the EDT, then `done` on it. A failure is reported rather than
     * swallowed: a review action that silently did nothing reads as one that worked.
     */
    fun <T> background(project: Project, title: String, work: () -> T, done: (T) -> Unit = {}) {
        ProgressManager.getInstance().run(object : Task.Backgroundable(project, title, false) {
            private var result: Result<T>? = null

            override fun run(indicator: ProgressIndicator) {
                result = runCatching(work)
            }

            override fun onSuccess() {
                result?.fold(done) { e -> notify(project, "$title failed: ${e.message}", NotificationType.ERROR) }
            }
        })
    }

    fun notify(project: Project, text: String, type: NotificationType = NotificationType.INFORMATION) =
        MoonlightService.notify(text, type, project)

    // ---- Opening diffs -------------------------------------------------------------

    /**
     * Review a session's changes: every queued file, one diff chain.
     *
     * `requested` names the session when the caller knows it; otherwise the rules in
     * [resolveReviewSession] pick, and the operator is asked when they cannot.
     */
    fun openSessionReview(project: Project, requested: String? = null) {
        background(project, "Loading the review queue", { core.control.reviewQueue(diffs = false) }) { queue ->
            // Ignored paths are warnings, not files to diff.
            val bySession = queue.filter { !it.ignored }.groupBy { it.sessionId }
            if (bySession.isEmpty()) {
                notify(project, "Nothing to review — no session has unreviewed changes.")
                return@background
            }
            val chosen = resolveReviewSession(requested, core.activeSession(project)?.sessionId, bySession.keys)
            if (chosen != null) {
                confirmAndOpen(project, chosen, bySession.getValue(chosen))
                return@background
            }
            pickSession(project, bySession) { confirmAndOpen(project, it, bySession.getValue(it)) }
        }
    }

    private fun pickSession(project: Project, bySession: Map<String, List<ReviewQueueItem>>, chosen: (String) -> Unit) {
        val all = core.sessions()
        val step = object : BaseListPopupStep<String>("Which session's changes do you want to review?", bySession.keys.toList()) {
            override fun getTextFor(value: String): String {
                val items = bySession.getValue(value)
                val known = all.firstOrNull { it.sessionId == value }
                // Two sessions can share a title; reviewing the wrong one's diff sends
                // comments to an agent that never wrote the code.
                val label = known?.let { sessionLabel(it, all) } ?: items.first().sessionTitle ?: value
                val working = if (known != null && !known.status.reviewable) " — still working (${known.status})" else ""
                return "$label · ${items.size} file(s)$working"
            }

            override fun onChosen(selectedValue: String, finalChoice: Boolean): PopupStep<*>? =
                doFinalStep { chosen(selectedValue) }
        }
        JBPopupFactory.getInstance().createListPopup(step).showCenteredInCurrentWindow(project)
    }

    /** An agent still writing these files makes the review a moving target — say so before opening. */
    private fun confirmAndOpen(project: Project, sessionId: String, items: List<ReviewQueueItem>) {
        if (!confirmReviewable(project, sessionId)) return
        openFiles(project, sessionId, items.first().sessionTitle ?: sessionId, items.map { ReviewedFile(it.filePath, it) })
    }

    private fun confirmReviewable(project: Project, sessionId: String): Boolean {
        val owner = core.sessions().firstOrNull { it.sessionId == sessionId } ?: return true
        if (owner.status.reviewable) return true
        return Messages.showOkCancelDialog(
            project,
            "\"${owner.title ?: owner.sessionId}\" is still working (${owner.status}). Comments you write may be about code it is replacing.",
            "Session Still Working",
            "Review Anyway",
            Messages.getCancelButton(),
            Messages.getWarningIcon(),
        ) == Messages.OK
    }

    /** One file from the queue. */
    /** One file from the queue: its session's whole review, on that file. */
    fun openDiff(project: Project, item: ReviewQueueItem) {
        if (item.ignored) return
        if (!confirmReviewable(project, item.sessionId)) return
        background(project, "Loading the review", { core.control.reviewQueue(item.sessionId, diffs = false) }) { queue ->
            val files = queue.filter { !it.ignored }.map { ReviewedFile(it.filePath, it) }.ifEmpty { listOf(ReviewedFile(item.filePath, item)) }
            openFiles(project, item.sessionId, item.sessionTitle ?: item.sessionId, files, focus = item.filePath)
        }
    }

    /**
     * Reopen a finished review from the comments written on it — the queue has moved on,
     * but the conversation is still in the tracker.
     */
    fun reopenFromComments(project: Project, sessionId: String) {
        background(project, "Loading review comments", { core.control.comments(sessionId) }) { comments ->
            val files = reviewFilesFromComments(comments)
            if (files.isEmpty()) {
                notify(project, "No file comments to reopen for this session.")
                return@background
            }
            openFiles(project, sessionId, core.sessions().firstOrNull { it.sessionId == sessionId }?.title ?: sessionId, files.map { ReviewedFile(it, null) })
        }
    }

    /** Open the session's review tab: the files in a tree, the selected one's diff beside it. */
    private fun openFiles(project: Project, sessionId: String, title: String, files: List<ReviewedFile>, focus: String? = null) {
        ReviewState.of(project).setTarget(sessionId)
        ReviewDiffFile.open(project, sessionId, title, files, focus)
    }

    // ---- Comments ------------------------------------------------------------------

    /**
     * Comment on the selection (or caret line) of an editor showing a reviewed file — either
     * side of the diff. `wholeFile` writes a file comment instead.
     */
    fun addComment(project: Project, editor: Editor, wholeFile: Boolean) {
        val state = ReviewState.of(project)
        val location = state.locate(editor.document)
        if (location == null) {
            notify(project, "This file is not under review. Open it from the review queue (MoonlightCode: Review Session Changes) to comment on it.", NotificationType.WARNING)
            return
        }
        val document = editor.document
        val selection = editor.selectionModel
        val range = if (wholeFile) null else {
            val startLine = document.getLineNumber(selection.selectionStart)
            val endLine = document.getLineNumber(selection.selectionEnd)
            val atColumnZero = selection.hasSelection() && selection.selectionEnd == document.getLineStartOffset(endLine)
            commentRange(startLine, endLine, atColumnZero)
        }
        // What the lines read when the comment was written — how the tracker later tells
        // an outdated comment from a current one.
        val anchorText = range?.let {
            document.getText(com.intellij.openapi.util.TextRange(document.getLineStartOffset(it.first - 1), document.getLineEndOffset(it.last - 1)))
        }
        val where = range?.let { if (it.first == it.last) "line ${it.first}" else "lines ${it.first}–${it.last}" } ?: "the whole file"
        val body = Messages.showMultilineInputDialog(project, "Comment on $where (${location.side.name.lowercase()} side):", "Review Comment", "", null, null)
            ?.trim()?.takeIf { it.isNotEmpty() } ?: return
        background(project, "Saving comment", {
            core.control.addComment(
                location.sessionId,
                if (range == null) CommentScope.File else CommentScope.Line,
                location.path,
                location.side,
                range?.first ?: 0,
                range?.last ?: 0,
                body,
                anchorText,
            )
        }) { state.reload(location.sessionId) }
    }

    private fun sessionFor(project: Project, thread: CommentThread): String? =
        ReviewState.of(project).sessionOf(thread.root).also {
            if (it == null) notify(project, "This thread is no longer in the review — refresh and try again.", NotificationType.WARNING)
        }

    fun reply(project: Project, thread: CommentThread) {
        val session = sessionFor(project, thread) ?: return
        val body = Messages.showMultilineInputDialog(project, "Reply:", "Reply to Review Thread", "", null, null)
            ?.trim()?.takeIf { it.isNotEmpty() } ?: return
        val root = thread.root
        background(project, "Saving reply", {
            core.control.addComment(session, root.scope, root.path, root.side, root.startLine, root.endLine, body, parentId = root.id)
        }) { ReviewState.of(project).reload(session) }
    }

    fun setResolved(project: Project, thread: CommentThread, resolved: Boolean) {
        val session = sessionFor(project, thread) ?: return
        background(project, if (resolved) "Resolving thread" else "Reopening thread", {
            core.control.updateComment(session, thread.root.id, resolved = resolved)
        }) { ReviewState.of(project).reload(session) }
    }

    /** Edit the thread's opening comment. Editing re-queues it for the next review batch. */
    fun edit(project: Project, thread: CommentThread) {
        val session = sessionFor(project, thread) ?: return
        val body = Messages.showMultilineInputDialog(project, "Edit comment (it is queued again for the next review):", "Edit Review Comment", thread.root.body, null, null)
            ?.trim()?.takeIf { it.isNotEmpty() && it != thread.root.body } ?: return
        background(project, "Saving comment", {
            core.control.updateComment(session, thread.root.id, body = body)
        }) { ReviewState.of(project).reload(session) }
    }

    fun delete(project: Project, thread: CommentThread) {
        val session = sessionFor(project, thread) ?: return
        val what = if (thread.replies.isEmpty()) "this comment" else "this thread and its ${thread.replies.size} repl${if (thread.replies.size == 1) "y" else "ies"}"
        if (Messages.showYesNoDialog(project, "Delete $what? This cannot be undone.", "Delete Review Thread", Messages.getQuestionIcon()) != Messages.YES) return
        background(project, "Deleting thread", {
            // Replies first: a reply left behind without its root is dropped from view but
            // would still sit in the tracker.
            thread.replies.forEach { core.control.deleteComment(session, it.id) }
            core.control.deleteComment(session, thread.root.id)
        }) { ReviewState.of(project).reload(session) }
    }

    // ---- Sending -------------------------------------------------------------------

    /** The session a submit goes to: the one whose diff was opened, else the active one. */
    fun submitTarget(project: Project): String? =
        ReviewState.of(project).target ?: core.activeSession(project)?.sessionId

    /**
     * Deliver every unsent, unresolved comment as one message.
     *
     * Reported honestly: the backend delivers when it owns the session's terminal, this
     * plugin delivers when *it* does, and otherwise nothing does. A comment is marked
     * delivered only on a path that actually wrote the text — a queued review can be sent
     * again, whereas one wrongly marked delivered is simply lost.
     */
    fun submit(project: Project, sessionId: String? = submitTarget(project)) {
        if (sessionId == null) {
            notify(project, "No review to send — open a session's changes first.", NotificationType.WARNING)
            return
        }
        background(project, "Sending review", { core.control.submitReview(sessionId) }) { outcome ->
            if (outcome == null) {
                notify(project, "Nothing to send — no pending comments.")
                return@background
            }
            val n = outcome.commentCount
            val pointer = reviewPointer(outcome.reviewPath, n)
            if (outcome.feedback is FeedbackDelivery.Queued) {
                // Writing to a pty is not proof the agent was at its prompt to read it, so
                // the message stays one click away either way.
                Delivery.handOff(project, pointer, outcome.reviewPath, "Review delivered — $n comment(s) as one message.", NotificationType.INFORMATION)
                ReviewState.of(project).reload(sessionId)
                return@background
            }
            deliverOurselves(project, sessionId, pointer, outcome.reviewPath, outcome.commentIds, n)
        }
    }

    private fun deliverOurselves(project: Project, sessionId: String, pointer: String, reviewPath: String?, ids: List<String>, n: Int) {
        val terminal = core.terminals.get(sessionId)?.takeIf { it.isAlive }
        if (terminal == null) {
            Delivery.copy(pointer)
            Delivery.handOff(
                project,
                pointer,
                reviewPath,
                "Review of $n comment(s) saved but NOT delivered — it stays queued. MoonlightCode does not own a terminal for this session, so it could not tell the agent. The message is on your clipboard: paste it into the session.",
                NotificationType.WARNING,
            )
            return
        }
        terminal.sendLine(pointer)
        terminal.show()
        background(project, "Recording delivery", { core.control.markDelivered(sessionId, ids) }) {
            Delivery.handOff(project, pointer, reviewPath, "Review delivered — $n comment(s) sent to ${terminal.name}.", NotificationType.INFORMATION)
            ReviewState.of(project).reload(sessionId)
        }
    }

    // ---- Settling queue items --------------------------------------------------------

    fun markReviewed(project: Project, item: ReviewQueueItem, then: () -> Unit) =
        background(project, "Marking reviewed", { core.control.accept(item.sessionId, item.filePath) }) { then() }

    fun ignore(project: Project, item: ReviewQueueItem, then: () -> Unit) =
        background(project, "Ignoring file", { core.control.setIgnored(item.sessionId, item.filePath, true) }) { then() }

    /**
     * Reject a file: drop it from the queue and steer the session. Only the first is
     * guaranteed, so the second is reported rather than assumed.
     */
    fun reject(project: Project, item: ReviewQueueItem, then: () -> Unit) {
        val message = Messages.showMultilineInputDialog(
            project,
            "What should the session do about ${Path.of(item.filePath).fileName}? This is sent to it as feedback.",
            "Reject Change",
            "",
            null,
            null,
        ) ?: return
        background(project, "Rejecting change", { core.control.reject(item.sessionId, item.filePath, message.trim()) }) { outcome ->
            when (val feedback = outcome.feedback) {
                is FeedbackDelivery.Undeliverable ->
                    notify(project, "Rejected — but your feedback was NOT delivered to the session: ${feedback.reason}", NotificationType.WARNING)
                FeedbackDelivery.Queued ->
                    if (message.isNotBlank()) notify(project, "Rejected — feedback delivered to the session.")
            }
            then()
        }
    }
}
