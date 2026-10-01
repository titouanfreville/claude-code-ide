package io.github.titouanfreville.moonlight.core

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class PanelKeyTest {
    @Test
    fun `a terminal tab and an AI Chat tab are both agent hosts`() {
        assertEquals("terminal::Claude (c391cd75)", panelKey(TERMINAL_TOOL_WINDOW, "Claude (c391cd75)"))
        assertEquals("aichat::Refactor auth", panelKey("AIAssistant", "Refactor auth"))
    }

    @Test
    fun `anything else in front is not a panel`() {
        assertNull(panelKey("Project", "moonlight"))
        assertNull(panelKey(null, null))
        // A host with no tab selected has nothing to link to.
        assertNull(panelKey(TERMINAL_TOOL_WINDOW, null))
        assertNull(panelKey(TERMINAL_TOOL_WINDOW, "  "))
    }

    @Test
    fun `a key reads back as words`() {
        assertEquals("the AI Chat tab \"Refactor auth\"", describePanelKey("aichat::Refactor auth"))
        assertEquals("the terminal tab \"Local\"", describePanelKey("terminal::Local"))
    }
}
