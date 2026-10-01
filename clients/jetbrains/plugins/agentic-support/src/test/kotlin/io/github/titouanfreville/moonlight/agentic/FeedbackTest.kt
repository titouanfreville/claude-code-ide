package io.github.titouanfreville.moonlight.agentic

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `feedback.test.ts`. */
class FeedbackTest {
    private val plan = "# Migrate the store\nMove the rows first.\n\n# Delete the old table\nOnly after the backfill."

    @Test
    fun `a plan splits on headings, not on blank lines`() {
        val blocks = planBlocks(plan)
        assertEquals(2, blocks.size)
        assertEquals("Migrate the store", blockTitle(blocks[0]))
        assertEquals("Delete the old table", blockTitle(blocks[1]))
    }

    @Test
    fun `prose with no heading is a single block`() {
        assertEquals(listOf("just do the thing"), planBlocks("just do the thing"))
        assertEquals("just do the thing", blockTitle("just do the thing"))
    }

    @Test
    fun `an empty plan has no blocks`() {
        assertEquals(emptyList<String>(), planBlocks("   \n\n"))
    }

    /** Without the heading the agent receives opinions with no referent. */
    @Test
    fun `each comment is quoted under the section it lands on`() {
        val feedback = reviewFeedback(plan, mapOf(1 to "back this up first"), PlanVerdict.Refine)
        assertTrue("## Delete the old table" in feedback)
        assertTrue("  back this up first" in feedback)
        assertFalse("## Migrate the store" in feedback)
    }

    @Test
    fun `comments arrive in the order the plan reads`() {
        val feedback = reviewFeedback(plan, mapOf(1 to "second note", 0 to "first note"), PlanVerdict.Refine)
        assertTrue(feedback.indexOf("first note") < feedback.indexOf("second note"))
    }

    @Test
    fun `the verdict changes the instruction the comments arrive under`() {
        val comments = mapOf(0 to "why this order?")
        assertTrue("keep these in mind" in reviewFeedback(plan, comments, PlanVerdict.Approve))
        assertTrue("needs answering" in reviewFeedback(plan, comments, PlanVerdict.OpenQuestion))
        assertTrue("approach is right" in reviewFeedback(plan, comments, PlanVerdict.Refine))
        assertTrue("not the right one" in reviewFeedback(plan, comments, PlanVerdict.NoGo))
    }

    @Test
    fun `one comment reads as singular, two as plural`() {
        assertTrue("1 question needs answering" in reviewFeedback(plan, mapOf(0 to "a"), PlanVerdict.OpenQuestion))
        assertTrue("2 questions need answering" in reviewFeedback(plan, mapOf(0 to "a", 1 to "b"), PlanVerdict.OpenQuestion))
    }

    @Test
    fun `a comment with no surviving block is left out`() {
        assertFalse("stale" in reviewFeedback("# only one", mapOf(7 to "stale"), PlanVerdict.Refine))
    }

    @Test
    fun `a wordless refusal still carries a reason`() {
        assertEquals("[verdict: no-go] $DEFAULT_REJECT_REASON", denialReason(plan, emptyMap(), PlanVerdict.NoGo))
    }

    @Test
    fun `only approve approves`() {
        assertEquals(listOf(PlanVerdict.Approve), PlanVerdict.values().filter { it.approves })
    }

    /** `refine` and `no-go` ask for opposite things; the verdict must survive as data. */
    @Test
    fun `every verdict leads with a machine-readable tag`() {
        for (verdict in PlanVerdict.values()) {
            assertTrue(reviewFeedback(plan, mapOf(0 to "a note"), verdict).startsWith("[verdict: ${verdict.tag}]"), verdict.name)
        }
    }

    @Test
    fun `a wordless refusal still says which verdict it was`() {
        val refine = denialReason(plan, emptyMap(), PlanVerdict.Refine)
        val noGo = denialReason(plan, emptyMap(), PlanVerdict.NoGo)
        assertNotEquals(refine, noGo)
        assertTrue(refine.startsWith("[verdict: refine]"))
        assertTrue(noGo.startsWith("[verdict: no-go]"))
    }
}
