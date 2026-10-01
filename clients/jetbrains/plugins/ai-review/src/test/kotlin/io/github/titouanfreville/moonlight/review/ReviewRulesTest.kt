package io.github.titouanfreville.moonlight.review

import io.github.titouanfreville.moonlight.client.CommentAuthor
import io.github.titouanfreville.moonlight.client.CommentScope
import io.github.titouanfreville.moonlight.client.DiffSide
import io.github.titouanfreville.moonlight.client.ReviewComment
import io.github.titouanfreville.moonlight.client.toThreads
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

/** Ported from `target.test.ts`, plus thread placement and counting. */
class ReviewRulesTest {
    private fun queued(vararg ids: String) = ids.toSet()

    /** The reported bug: the status bar knew the session, the click asked anyway. */
    @Test
    fun `a named session with changes is used without asking`() {
        assertEquals("a", resolveReviewSession("a", "b", queued("a", "b", "c")))
    }

    @Test
    fun `with nothing named, the active session wins over a picker`() {
        assertEquals("b", resolveReviewSession(null, "b", queued("a", "b", "c")))
    }

    @Test
    fun `a lone session with changes needs no choosing`() {
        assertEquals("a", resolveReviewSession(null, null, queued("a")))
    }

    @Test
    fun `several sessions and no hint is the one case worth asking about`() {
        assertNull(resolveReviewSession(null, null, queued("a", "b")))
    }

    @Test
    fun `a named session with nothing queued falls through to the active one`() {
        assertEquals("b", resolveReviewSession("gone", "b", queued("b", "c")))
    }

    @Test
    fun `a named session with nothing queued still falls through to a lone candidate`() {
        assertEquals("only", resolveReviewSession("gone", null, queued("only")))
    }

    @Test
    fun `an active session with nothing queued does not suppress the picker`() {
        assertNull(resolveReviewSession(null, "b", queued("a", "c")))
    }

    @Test
    fun `an empty queue never yields a session`() {
        assertNull(resolveReviewSession("a", "b", queued()))
    }

    private fun comment(
        id: String,
        path: String = "/w/a.kt",
        scope: CommentScope = CommentScope.Line,
        side: DiffSide = DiffSide.After,
        start: Int = 3,
        end: Int = 5,
        resolved: Boolean = false,
        sent: Boolean = false,
        parent: String? = null,
    ) = ReviewComment(id, scope, path, side, start, end, "body", null, sent, resolved, false, CommentAuthor.Operator, parent)

    @Test
    fun `reopening takes the distinct files that carry comments`() {
        val files = reviewFilesFromComments(listOf(comment("1", "a.ts"), comment("2", "a.ts"), comment("3", "b.ts", CommentScope.File)))
        assertEquals(listOf("a.ts", "b.ts"), files)
    }

    @Test
    fun `resolved threads still count as worth reopening`() {
        assertEquals(listOf("a.ts"), reviewFilesFromComments(listOf(comment("1", "a.ts", resolved = true))))
    }

    @Test
    fun `a review-scoped comment is not a file to open`() {
        assertEquals(emptyList<String>(), reviewFilesFromComments(listOf(comment("1", "", CommentScope.Review))))
        assertEquals(emptyList<String>(), reviewFilesFromComments(emptyList()))
    }

    @Test
    fun `a thread sits at the end of its range, on its own side only`() {
        val threads = toThreads(listOf(comment("1", end = 5), comment("2", side = DiffSide.Before, end = 2)))
        val after = anchorsFor(threads, "/w/a.kt", DiffSide.After, 100, true)
        assertEquals(listOf(4), after.map { it.line })
        assertEquals(listOf(1), anchorsFor(threads, "/w/a.kt", DiffSide.Before, 100, true).map { it.line })
    }

    @Test
    fun `a file comment sits on the first line, and an outdated range is clamped`() {
        val threads = toThreads(listOf(comment("1", scope = CommentScope.File, start = 0, end = 0), comment("2", end = 400)))
        val anchors = anchorsFor(threads, "/w/a.kt", DiffSide.After, 10, true)
        assertEquals(listOf(0 to true, 9 to false), anchors.map { it.line to it.fileWide })
    }

    @Test
    fun `resolved threads can be hidden, other files never show`() {
        val threads = toThreads(listOf(comment("1", resolved = true), comment("2", path = "/w/b.kt")))
        assertEquals(emptyList<ThreadAnchor>(), anchorsFor(threads, "/w/a.kt", DiffSide.After, 100, false))
    }

    @Test
    fun `pending counts what the next submit sends, resolved what is settled`() {
        val counts = reviewCounts(listOf(comment("1"), comment("2", sent = true), comment("3", resolved = true), comment("4", parent = "1")))
        assertEquals(ReviewCounts(pending = 2, resolved = 1), counts)
    }

    @Test
    fun `the pointer names the review file when there is one`() {
        assertEquals(
            "Please review the code review at /r/review.md and address each of the 3 comment(s).",
            reviewPointer("/r/review.md", 3),
        )
        assertEquals("A code review with 1 comment(s) is waiting in MoonlightCode.", reviewPointer(null, 1))
    }

    @Test
    fun `a selection covers the lines it shows`() {
        assertEquals(3..3, commentRange(2, 2, selectionEndsAtColumnZero = false))
        assertEquals(3..5, commentRange(2, 4, selectionEndsAtColumnZero = false))
        // Dragged to the start of the next line: that line is not part of it.
        assertEquals(3..4, commentRange(2, 4, selectionEndsAtColumnZero = true))
    }
}
