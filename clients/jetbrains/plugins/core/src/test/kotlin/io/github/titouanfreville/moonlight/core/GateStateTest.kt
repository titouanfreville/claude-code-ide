package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.EngineEvent
import io.github.titouanfreville.moonlight.client.PLAN_HOLD
import io.github.titouanfreville.moonlight.client.PendingApproval
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `gate-state.test.ts`. */
class GateStateTest {
    private fun ask(session: String, what: String, tool: String? = null) = EngineEvent.ApprovalRequested(session, what, tool)

    /**
     * A transcript from a session that finished hours ago republishes its plan; an approve
     * button with no hook behind it gets clicked and believed.
     */
    @Test
    fun `a plan proposal supplies text but arms nothing`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Old plan"))
        assertEquals("# Old plan", gate.plan("s1"))
        assertNull(gate.approval("s1"))
        assertEquals(emptyList<HeldApproval>(), gate.approvals())
    }

    @Test
    fun `an approval request arms the verdict and picks up the plan already seen`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Do it"))
        gate.apply(ask("s1", "approve plan"), now = 1_000)
        val held = gate.approval("s1")!!
        assertEquals("approve plan", held.what)
        assertEquals("# Do it", held.plan)
        assertEquals(1_000, held.sinceMs)
    }

    /** The hook can be released by anyone — a client still offering a verdict offers it on nothing. */
    @Test
    fun `a session that stops waiting is no longer held`() {
        val gate = GateState()
        gate.apply(ask("s1", "rm -rf /"))
        assertFalse(gate.apply(EngineEvent.SessionStateChanged("s1", "WaitingInput")))
        assertNotNull(gate.approval("s1"))
        assertTrue(gate.apply(EngineEvent.SessionStateChanged("s1", "Running")))
        assertNull(gate.approval("s1"))
    }

    @Test
    fun `events that change nothing report no change`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Same"))
        assertFalse(gate.apply(EngineEvent.PlanProposed("s1", "# Same")))
        assertFalse(gate.apply(EngineEvent.SessionStateChanged("s2", "Running")))
        assertFalse(gate.apply(EngineEvent.SessionRemoved("s2")))
    }

    @Test
    fun `a resync drops holds the server no longer reports, and keeps the plans`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Do it"))
        gate.apply(ask("s1", "approve plan"))
        gate.resync(listOf(PendingApproval("s2", "approve plan", "# Other", null, 5)))
        assertNull(gate.approval("s1"))
        assertEquals("# Do it", gate.plan("s1"), "a plan stays readable after its hold ends")
        assertEquals("# Other", gate.approval("s2")?.plan)
    }

    @Test
    fun `holds are listed longest-waiting first`() {
        val gate = GateState()
        gate.resync(
            listOf(
                PendingApproval("late", "a", null, null, 900),
                PendingApproval("early", "b", null, null, 100),
            )
        )
        assertEquals(listOf("early", "late"), gate.approvals().map { it.sessionId })
    }

    /** The plan cache outlives its hold; a Bash approval must not arrive carrying it. */
    @Test
    fun `a tool approval after a plan does not carry that plan`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Do the thing"))
        gate.apply(ask("s1", PLAN_HOLD))
        assertEquals("# Do the thing", gate.approval("s1")?.plan)
        gate.apply(ask("s1", "run `npx js-yaml`"))
        val held = gate.approval("s1")!!
        assertEquals("run `npx js-yaml`", held.what)
        assertNull(held.plan)
    }

    @Test
    fun `the plan stays readable on its own after the hold moves on`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Plan"))
        gate.apply(ask("s1", "run `ls`"))
        assertEquals("# Plan", gate.plan("s1"))
    }

    @Test
    fun `a plan arriving after its hold still reaches the hold`() {
        val gate = GateState()
        gate.apply(ask("s1", PLAN_HOLD))
        assertNull(gate.approval("s1")?.plan)
        gate.apply(EngineEvent.PlanProposed("s1", "# Late"))
        assertEquals("# Late", gate.approval("s1")?.plan)
    }

    @Test
    fun `a repeated plan still fills a hold that is missing it`() {
        val gate = GateState()
        gate.apply(EngineEvent.PlanProposed("s1", "# Same"))
        gate.apply(ask("s1", PLAN_HOLD))
        val fresh = GateState()
        fresh.apply(ask("s1", PLAN_HOLD))
        fresh.apply(EngineEvent.PlanProposed("s1", "# Same"))
        fresh.apply(EngineEvent.PlanProposed("s1", "# Same"))
        assertEquals("# Same", fresh.approval("s1")?.plan)
        assertEquals("# Same", gate.approval("s1")?.plan)
    }

    @Test
    fun `a late plan does not attach itself to a command approval`() {
        val gate = GateState()
        gate.apply(ask("s1", "run `ls`"))
        gate.apply(EngineEvent.PlanProposed("s1", "# Unrelated"))
        assertNull(gate.approval("s1")?.plan)
        assertEquals("# Unrelated", gate.plan("s1"))
    }
}
