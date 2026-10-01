package io.github.titouanfreville.moonlight.status

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.HookStatusEntry
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.core.ActiveSession
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class GatingViewTest {
    private val hooksOn = listOf(HookStatusEntry("PreToolUse", "*", true, "moonlightd hook"))
    private val folders = listOf("/work/app")

    private fun session(
        id: String,
        adopted: Boolean = true,
        phase: Phase = Phase.AutoImplement,
        status: SessionStatus = SessionStatus.Idle,
        root: String? = "/work/app",
    ) = DiscoverableSession(id, "Session $id", root, adopted, status, phase, 0)

    private fun view(
        sessions: List<DiscoverableSession>,
        active: ActiveSession?,
        hooks: List<HookStatusEntry> = hooksOn,
        error: String? = null,
        panelKey: String? = null,
    ) = gatingView(error, hooks, sessions, active, panelKey, folders)

    @Test
    fun `no backend says so, warns, and retries on click`() {
        val v = view(emptyList(), null, error = "connection refused")
        assertEquals(GatingIcon.NoBackend, v.icon)
        assertTrue(v.warn)
        assertEquals(GatingAction.Refresh, v.action)
        assertTrue(v.tooltip.contains("connection refused"))
    }

    /** A phase means nothing without hooks to enforce it — the most misleading thing to show. */
    @Test
    fun `missing hooks outrank any phase`() {
        val s = session("s1", phase = Phase.Plan)
        val v = view(listOf(s), ActiveSession("s1", ActiveSession.How.Owned), hooks = listOf(HookStatusEntry("PreToolUse", "*", false, "")))
        assertEquals("Not gated", v.text)
        assertTrue(v.warn)
    }

    @Test
    fun `no hooks reported at all is not gated either`() {
        assertEquals("Not gated", view(listOf(session("s1")), null, hooks = emptyList()).text)
    }

    @Test
    fun `nothing adopted asks for an adoption, not for a session`() {
        val v = view(listOf(session("s1", adopted = false)), null)
        assertEquals("No adopted session", v.text)
        assertEquals(GatingAction.Adopt, v.action)
    }

    @Test
    fun `adopted sessions but none identified asks which, without alarm`() {
        val v = view(listOf(session("s1"), session("s2")), null)
        assertEquals("Which session?", v.text)
        assertFalse(v.warn)
        assertEquals(GatingAction.SetActive, v.action)
    }

    /** Hooks on, so the bar could read "gated", while this session never is. */
    @Test
    fun `an unadopted active session is ungoverned`() {
        val v = view(listOf(session("s1", adopted = false), session("s2")), ActiveSession("s1", ActiveSession.How.Pinned))
        assertEquals("Ungoverned", v.text)
        assertTrue(v.warn)
        assertEquals(GatingAction.Adopt, v.action)
    }

    @Test
    fun `a frozen phase warns and says writes are denied`() {
        val v = view(listOf(session("s1", phase = Phase.Plan)), ActiveSession("s1", ActiveSession.How.Owned))
        assertEquals(GatingIcon.Frozen, v.icon)
        assertEquals("Plan", v.text)
        assertTrue(v.warn)
        assertTrue(v.tooltip.any { "DENIED" in it })
    }

    @Test
    fun `a writing phase is calm`() {
        val v = view(listOf(session("s1")), ActiveSession("s1", ActiveSession.How.Owned))
        assertEquals(GatingIcon.Governed, v.icon)
        assertEquals("AutoImplement", v.text)
        assertFalse(v.warn)
        assertEquals(GatingAction.SetPhase, v.action)
    }

    /** A guess is labelled in the bar itself, not only in a tooltip nobody hovers. */
    @Test
    fun `a guessed session says so in the bar`() {
        assertEquals("AutoImplement · guess", view(listOf(session("s1")), ActiveSession("s1", ActiveSession.How.Sole)).text)
    }

    /** The positive evidence the inference went wrong: something else here is working. */
    @Test
    fun `another session mid-turn here turns the answer into a question`() {
        val shown = session("s1", status = SessionStatus.Idle)
        val busy = session("s2", adopted = false, status = SessionStatus.Running)
        val v = view(listOf(shown, busy), ActiveSession("s1", ActiveSession.How.Pinned))
        assertEquals("AutoImplement · which session?", v.text)
        assertTrue(v.warn)
        assertEquals(GatingAction.SetActive, v.action)
        assertTrue(v.tooltip.any { "NOT adopted" in it })
    }

    @Test
    fun `a session busy in another project is not evidence`() {
        val shown = session("s1")
        val elsewhere = session("s2", status = SessionStatus.Running, root = "/other/repo")
        assertEquals("AutoImplement", view(listOf(shown, elsewhere), ActiveSession("s1", ActiveSession.How.Owned)).text)
    }

    @Test
    fun `a panel link names the tab it came from`() {
        val v = view(listOf(session("s1")), ActiveSession("s1", ActiveSession.How.Panel), panelKey = "aichat::Refactor auth")
        assertEquals("Session identified: linked to the AI Chat tab \"Refactor auth\".", v.tooltip.last())
    }
}
