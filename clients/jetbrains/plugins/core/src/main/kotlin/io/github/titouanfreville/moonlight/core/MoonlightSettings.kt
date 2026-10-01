package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.components.service

/**
 * The daemon settings — `moonlight.daemon.path` and `moonlight.daemon.autostart` in VS
 * Code terms. Application-level: which daemon to start is a fact about this machine, not
 * about a project.
 *
 * Read on every poll rather than captured at startup, so a change takes effect on the next
 * tick instead of needing a restart.
 */
@Service(Service.Level.APP)
@State(name = "MoonlightSettings", storages = [Storage("moonlight.xml")])
class MoonlightSettings : SimplePersistentStateComponent<MoonlightSettings.SettingsState>(SettingsState()) {
    class SettingsState : BaseState() {
        /**
         * Absolute path to the `moonlightd` this IDE should start. Unset means
         * `MOONLIGHT_DAEMON_BIN`, then the first `moonlightd` on `PATH`. Named but not
         * executable starts nothing — a named daemon is never swapped for a different one.
         */
        var daemonPath by string()

        /**
         * Start `moonlightd` when the backend is unreachable. Off for an operator who runs
         * their own; the IDE keeps talking to whichever daemon is running either way.
         */
        var autostart by property(true)
    }

    /** The configured path, with a blank field read as unset rather than as a path to nothing. */
    fun daemonPath(): String? = state.daemonPath?.trim()?.takeIf { it.isNotEmpty() }

    fun autostart(): Boolean = state.autostart

    companion object {
        fun getInstance(): MoonlightSettings = ApplicationManager.getApplication().service()
    }
}
