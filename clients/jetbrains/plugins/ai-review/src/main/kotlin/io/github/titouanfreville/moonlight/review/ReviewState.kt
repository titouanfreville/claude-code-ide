package io.github.titouanfreville.moonlight.review

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.editor.Document
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.Project
import com.intellij.util.messages.Topic
import io.github.titouanfreville.moonlight.client.CommentThread
import io.github.titouanfreville.moonlight.client.DiffSide
import io.github.titouanfreville.moonlight.client.ReviewComment
import io.github.titouanfreville.moonlight.client.toThreads
import io.github.titouanfreville.moonlight.core.MoonlightApi
import java.util.Collections
import java.util.WeakHashMap
import java.util.concurrent.ConcurrentHashMap

/**
 * What one project window knows about the review in progress.
 *
 * Everything a review *produces* lives in the daemon's tracker — the same store the desktop
 * cockpit and VS Code read — so a review survives closing the IDE and the surfaces cannot
 * disagree about what was said. This holds only what makes those records drawable here: which
 * files are under review for which session, which in-memory documents are the "before"
 * sides, and the last comments read for each session.
 */
@Service(Service.Level.PROJECT)
class ReviewState(private val project: Project) {
    /**
     * The session the review acts on — the one whose diff was opened last. Deliberately not
     * the active session: comments go to the agent whose diff they were written on, and
     * conflating the two would deliver a review of one session's code to another.
     */
    @Volatile
    var target: String? = null
        private set

    /** Whether settled threads are drawn. On by default: a resolved thread is the record of why the code looks as it does. */
    @Volatile
    var showResolved: Boolean = true
        private set

    private val comments = ConcurrentHashMap<String, List<ReviewComment>>()
    private val filesUnderReview = ConcurrentHashMap<String, String>()

    /** Before-side documents, by identity. Weak: they belong to diff windows that come and go. */
    private val baselines: MutableMap<Document, Pair<String, String>> = Collections.synchronizedMap(WeakHashMap())

    fun setTarget(sessionId: String) {
        target = sessionId
        reload(sessionId)
    }

    fun toggleResolved(): Boolean {
        showResolved = !showResolved
        changed()
        return showResolved
    }

    /** Note that `path` is under review for `sessionId`, so its editors carry the threads. */
    fun register(sessionId: String, path: String) {
        filesUnderReview[path] = sessionId
    }

    fun registerBaseline(document: Document, sessionId: String, path: String) {
        baselines[document] = sessionId to path
        filesUnderReview.putIfAbsent(path, sessionId)
    }

    /** Which review, file and side a document belongs to, or `null` when it is not under review. */
    fun locate(document: Document): ReviewLocation? {
        baselines[document]?.let { (session, path) -> return ReviewLocation(session, path, DiffSide.Before) }
        val path = FileDocumentManager.getInstance().getFile(document)?.path ?: return null
        val session = filesUnderReview[path] ?: return null
        return ReviewLocation(session, path, DiffSide.After)
    }

    fun comments(sessionId: String): List<ReviewComment> = comments[sessionId].orEmpty()

    fun threads(sessionId: String): List<CommentThread> = toThreads(comments(sessionId))

    /** The session a stored comment belongs to — a thread does not carry it. */
    fun sessionOf(comment: ReviewComment): String? =
        comments.entries.firstOrNull { (_, list) -> list.any { it.id == comment.id } }?.key

    fun counts(): ReviewCounts = target?.let { reviewCounts(comments(it)) } ?: ReviewCounts(0, 0)

    /**
     * Re-read a session's comments from the tracker, then redraw. Rebuilt rather than patched,
     * so the editor always shows what is stored — including the agent's answers and comments
     * another surface wrote.
     */
    fun reload(sessionId: String) {
        ApplicationManager.getApplication().executeOnPooledThread {
            val fresh = try {
                MoonlightApi.getInstance().control.comments(sessionId)
            } catch (_: Exception) {
                return@executeOnPooledThread // the backend's state is reported elsewhere
            }
            if (comments.put(sessionId, fresh) != fresh) changed()
        }
    }

    /** Tell every review surface to re-read — after a queue item was settled, say. */
    fun changed() {
        ApplicationManager.getApplication().invokeLater({ project.messageBus.syncPublisher(ReviewListener.TOPIC).reviewChanged() }, project.disposed)
    }

    companion object {
        fun of(project: Project): ReviewState = project.service()
    }
}

data class ReviewLocation(val sessionId: String, val path: String, val side: DiffSide)

/** The review's comments, target or display changed. Delivered on the EDT. */
fun interface ReviewListener {
    fun reviewChanged()

    companion object {
        @JvmField
        @Topic.ProjectLevel
        val TOPIC: Topic<ReviewListener> = Topic(ReviewListener::class.java, Topic.BroadcastDirection.NONE)
    }
}
