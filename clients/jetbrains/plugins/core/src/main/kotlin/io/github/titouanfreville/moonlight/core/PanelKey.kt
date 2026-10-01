package io.github.titouanfreville.moonlight.core

/**
 * A handle for the agent host in front of the operator, used to link a session to it.
 *
 * A Claude session in a JetBrains IDE lives in one of two places: a **Terminal** tab —
 * including the ones Anthropic's Claude Code plugin opens and the ones MoonlightCode
 * starts — or the **AI Chat** tool window, which hosts Claude over ACP beside Junie. Both
 * are keyed the same way, `<host>::<tab name>`, because a tab's display name is all the
 * public API exposes about which tab it is.
 *
 * The same fragility as VS Code's agent-panel key: renaming a tab breaks its link, which
 * is why the link is a convenience over the workspace pin rather than a replacement.
 */

/** The Terminal tool window's id. */
const val TERMINAL_TOOL_WINDOW = "Terminal"

/**
 * Tool window ids of AI Assistant's chat. Treated as optional — an IDE without AI
 * Assistant simply never matches — and never used for anything but reading the selected
 * tab's name.
 */
val AI_CHAT_TOOL_WINDOWS: Set<String> = setOf("AIAssistant")

/** The key for the host tab in front of the operator, or `null` when it is not an agent host. */
fun panelKey(toolWindowId: String?, selectedTabName: String?): String? {
    val host = when (toolWindowId) {
        TERMINAL_TOOL_WINDOW -> "terminal"
        in AI_CHAT_TOOL_WINDOWS -> "aichat"
        else -> return null
    }
    if (selectedTabName.isNullOrBlank()) return null
    return "$host::$selectedTabName"
}

/** Words for where a panel key points, for tooltips: `the AI Chat tab "X"`. */
fun describePanelKey(key: String): String {
    val host = key.substringBefore("::")
    val tab = key.substringAfter("::")
    return when (host) {
        "aichat" -> "the AI Chat tab \"$tab\""
        "terminal" -> "the terminal tab \"$tab\""
        else -> "\"$tab\""
    }
}
