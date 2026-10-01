package io.github.titouanfreville.moonlight.review

import io.github.titouanfreville.moonlight.client.CommentScope
import io.github.titouanfreville.moonlight.client.CommentThread
import io.github.titouanfreville.moonlight.client.DiffSide
import io.github.titouanfreville.moonlight.client.ReviewComment

/**
 * The review rules with no IDE in them — ports of `target.ts`, plus the placement and
 * counting the VS Code Comments API did for free. Pure, because these are the parts that
 * have to be right and fail quietly when they are not.
 */

/**
 * The session whose diffs to open, or `null` when the operator has to be asked.
 *
 * In order: a caller that already knows (the status bar, a reopen link); the session the
 * operator is working in; the only session with anything to review. A named session with
 * nothing queued is skipped rather than honoured — the count that named it is a poll
 * behind, and opening an empty review on a stale count is worse than asking.
 */
fun resolveReviewSession(requested: String?, active: String?, withChanges: Set<String>): String? = when {
    requested != null && requested in withChanges -> requested
    active != null && active in withChanges -> active
    withChanges.size == 1 -> withChanges.first()
    else -> null
}

/**
 * The files to reopen for a finished review, from the comments written on it. Resolved
 * threads count — they are often what the operator came back to check — but review-scoped
 * comments are about the review, not a file, and carry no path to open.
 */
fun reviewFilesFromComments(comments: List<ReviewComment>): List<String> =
    comments.filter { it.scope != CommentScope.Review && it.path.isNotEmpty() }.map { it.path }.distinct()

/** Where a thread sits in an editor: a 0-based line, and whether it is about the whole file. */
data class ThreadAnchor(val thread: CommentThread, val line: Int, val fileWide: Boolean)

/**
 * The threads that belong in one editor — one side of one file — and the line each sits on.
 *
 * Lines are 1-based on the wire and 0-based in an editor. A thread is drawn at the *end* of
 * its range, where the reader finishes the code it is about; a file-wide thread sits on the
 * first line. Clamped to the document, because the file may have shrunk since the comment
 * was written — an outdated thread is still shown, at the nearest line that exists.
 */
fun anchorsFor(
    threads: List<CommentThread>,
    path: String,
    side: DiffSide,
    lineCount: Int,
    showResolved: Boolean,
): List<ThreadAnchor> = threads
    .filter { it.root.path == path && it.root.side == side && it.root.scope != CommentScope.Review }
    .filter { showResolved || !it.root.resolved }
    .map { thread ->
        val fileWide = thread.root.scope == CommentScope.File
        val wire = if (fileWide) 1 else thread.root.endLine
        ThreadAnchor(thread, (wire - 1).coerceIn(0, (lineCount - 1).coerceAtLeast(0)), fileWide)
    }

/**
 * Where a review stands: what the next submit will send, and what is already settled.
 * Counted together because `3 queued` alone cannot say whether the other comments were dealt
 * with or never existed.
 */
data class ReviewCounts(val pending: Int, val resolved: Int)

fun reviewCounts(comments: List<ReviewComment>): ReviewCounts =
    ReviewCounts(pending = comments.count { !it.sent && !it.resolved }, resolved = comments.count { it.resolved })

/**
 * The one-line message handed to the session: a pointer to the review file the daemon wrote.
 * One line on purpose — it is typed into a terminal, and a pointer has nothing to truncate.
 */
fun reviewPointer(reviewPath: String?, count: Int): String =
    if (reviewPath != null) "Please review the code review at $reviewPath and address each of the $count comment(s)."
    else "A code review with $count comment(s) is waiting in MoonlightCode."

/**
 * The range a new comment covers: the selection when there is one, else the caret line.
 * 0-based in, 1-based out — the wire's convention.
 */
fun commentRange(selectionStartLine: Int, selectionEndLine: Int, selectionEndsAtColumnZero: Boolean): IntRange {
    // A selection made by dragging to the start of the next line covers one line fewer
    // than its end says; counting that line would anchor the comment below the code.
    val end = if (selectionEndsAtColumnZero && selectionEndLine > selectionStartLine) selectionEndLine - 1 else selectionEndLine
    return (selectionStartLine + 1)..(end + 1)
}
