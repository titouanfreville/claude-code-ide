package io.github.titouanfreville.moonlight.status

import io.github.titouanfreville.moonlight.client.AccountUsage
import io.github.titouanfreville.moonlight.client.SessionUsage
import io.github.titouanfreville.moonlight.client.UsageResponse
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class UsageViewTest {
    private val now = 10_000_000L
    private val quota = AccountUsage(52.0, null, "40m", 12.5, null)

    private fun session(ctx: Double?, updatedMs: Long = now) =
        SessionUsage("s1", "Session", "opus", "", 1000, "1h12m", ctx, 30_000, 200_000, 0.4, updatedMs)

    /** Zero is read as headroom; an unknown figure must never look like one. */
    @Test
    fun `an unknown figure renders as a dash, never as zero`() {
        assertEquals("—", pct(null))
        assertEquals("0%", pct(0.0))
        assertEquals("42%", pct(42.0))
        assertEquals("42.5%", pct(42.5))
    }

    @Test
    fun `no backend hides the widget - the gating one already says so`() {
        assertNull(usageView("down", UsageResponse(quota, emptyList()), null, now))
    }

    @Test
    fun `before the first reading it says it does not know yet`() {
        assertEquals("usage —", usageView(null, null, null, now)!!.text)
    }

    @Test
    fun `account windows always show, session figures only for a known session`() {
        assertEquals("5h 52% ↻ 40m · wk 12.5%", usageView(null, UsageResponse(quota, listOf(session(30.0))), null, now)!!.text)
        assertEquals(
            "5h 52% ↻ 40m · wk 12.5% · ctx 30% · 1h12m",
            usageView(null, UsageResponse(quota, listOf(session(30.0))), "s1", now)!!.text,
        )
    }

    @Test
    fun `a missing quota is unknown, not empty`() {
        val v = usageView(null, UsageResponse(null, emptyList()), null, now)!!
        assertEquals("5h — · wk —", v.text)
        assertTrue(v.tooltip.any { "unknown, not zero" in it })
    }

    @Test
    fun `pressure on any shown window warns`() {
        assertFalse(usageView(null, UsageResponse(quota, emptyList()), null, now)!!.warn)
        assertTrue(usageView(null, UsageResponse(quota, listOf(session(91.0))), "s1", now)!!.warn)
        assertTrue(underPressure(quota.copy(weeklyPct = 90.0), null))
    }

    @Test
    fun `a quiet session's figures are marked as memories`() {
        val stale = usageView(null, UsageResponse(quota, listOf(session(30.0, updatedMs = now - STALE_MS - 60_000))), "s1", now)!!
        assertTrue(stale.tooltip.any { "gone quiet" in it })
    }

    @Test
    fun `a known session with no snapshot explains why`() {
        val v = usageView(null, UsageResponse(quota, emptyList()), "s1", now)!!
        assertTrue(v.tooltip.any { "only registers for sessions it starts" in it })
    }
}
