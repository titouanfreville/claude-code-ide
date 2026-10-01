package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * A terminal, a panel link and a pin all name the conversation they were made on. After the
 * process `/resume`s, they have to follow its launch — or the status bar loses the session
 * and a review finds no terminal to deliver into.
 */
class LaunchFollowTest {
    private fun session(id: String, launch: String? = null) =
        DiscoverableSession(id, id, "/w", true, SessionStatus.Running, Phase.Plan, 0, launch)

    @Test
    fun `a known conversation names itself`() {
        assertEquals("213d1944", currentConversation("213d1944", listOf(session("213d1944", launch = "a05a347e"))))
    }

    @Test
    fun `a launch that moved resolves to the conversation it is in now`() {
        assertEquals("213d1944", currentConversation("a05a347e", listOf(session("213d1944", launch = "a05a347e"))))
    }

    @Test
    fun `an id nothing knows yet stays itself`() {
        assertEquals("a05a347e", currentConversation("a05a347e", listOf(session("other"))))
    }
}
