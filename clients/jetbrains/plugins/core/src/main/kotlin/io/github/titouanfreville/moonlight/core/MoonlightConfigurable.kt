package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.options.BoundConfigurable
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.AlignX
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import com.intellij.ui.dsl.builder.panel

/** Settings ▸ Tools ▸ MoonlightCode. */
class MoonlightConfigurable : BoundConfigurable("MoonlightCode") {
    override fun createPanel(): DialogPanel {
        val settings = MoonlightSettings.getInstance()
        return panel {
            group("Daemon") {
                row("moonlightd path:") {
                    textField()
                        .bindText({ settings.state.daemonPath ?: "" }, { settings.state.daemonPath = it.trim().ifEmpty { null } })
                        .align(AlignX.FILL)
                        .comment(
                            "Leave empty to use <code>MOONLIGHT_DAEMON_BIN</code>, then the first <code>moonlightd</code> on " +
                                "<code>PATH</code>. Set it to start your own build. A path that is set but not executable " +
                                "starts nothing — a named daemon is never silently swapped for a different one."
                        )
                }
                row {
                    checkBox("Start moonlightd when the backend is unreachable")
                        .bindSelected({ settings.state.autostart }, { settings.state.autostart = it })
                        .comment(
                            "Turn this off if you run the daemon yourself. The IDE keeps using whichever daemon is running; " +
                                "it just stops starting one. With this off and no daemon running, sessions are ungoverned: " +
                                "the hooks fail open."
                        )
                }
            }
        }
    }

    override fun apply() {
        super.apply()
        // Take effect now rather than on the next tick.
        MoonlightService.getInstance().refresh()
    }
}
