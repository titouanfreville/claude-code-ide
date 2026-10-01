package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class AutoResumeTest {
    @Test
    fun `a crash mid-work is relaunched under Automatic, up to the cap`() {
        assertEquals(ExitDecision.Relaunch, decideOnExit(AutoResumeMode.Automatic, SessionStatus.Running, 0))
        assertEquals(ExitDecision.Relaunch, decideOnExit(AutoResumeMode.Automatic, SessionStatus.Running, MAX_EXIT_RESUMES - 1))
        assertEquals(ExitDecision.Offer, decideOnExit(AutoResumeMode.Automatic, SessionStatus.Running, MAX_EXIT_RESUMES))
    }

    @Test
    fun `quitting at the prompt is never relaunched`() {
        for (status in listOf(SessionStatus.Idle, SessionStatus.WaitingInput, SessionStatus.Done, null)) {
            assertEquals(ExitDecision.Offer, decideOnExit(AutoResumeMode.Automatic, status, 0), "$status")
        }
    }

    @Test
    fun `only Automatic relaunches on its own`() {
        assertEquals(ExitDecision.Offer, decideOnExit(AutoResumeMode.Ask, SessionStatus.Running, 0))
        assertEquals(ExitDecision.Offer, decideOnExit(AutoResumeMode.Off, SessionStatus.Running, 0))
    }

    private fun session(id: String, launch: String?, status: SessionStatus = SessionStatus.Idle) =
        DiscoverableSession(id, "t-$id", "/w", true, status, Phase.Plan, 0, launch)

    @Test
    fun `a record follows its launch to the conversation it resumed into`() {
        val record = LaunchRecord("a05a347e", "/w", "a05a347e", SessionStatus.Idle, null)
        val known = listOf(session("a05a347e", null), session("213d1944", "a05a347e", SessionStatus.Running))
        val followed = record.follow(known)
        assertEquals("213d1944", followed.conversation)
        assertEquals(SessionStatus.Running, followed.status)
        assertEquals(true, followed.wasWorking)
    }

    @Test
    fun `a launch the daemon has not seen keeps what it knew`() {
        val record = LaunchRecord("a05a347e", "/w", null, null, null)
        assertEquals(record, record.follow(listOf(session("other", null))))
    }

    @Test
    fun `records round-trip, and an unreadable one is dropped`() {
        val record = LaunchRecord("a05a347e", "/w", "213d1944", SessionStatus.Running, "Fix the build")
        assertEquals(record, LaunchRecord.decode("a05a347e", record.encode()))
        assertEquals(LaunchRecord("x", null, null, null, null), LaunchRecord.decode("x", "{}"))
        assertNull(LaunchRecord.decode("x", "not json"))
    }
}
