package io.github.titouanfreville.moonlight.core

import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.EDT
import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.IdeFocusManager
import com.intellij.openapi.wm.ToolWindowManager
import com.intellij.terminal.frontend.toolwindow.TerminalToolWindowTab
import com.intellij.terminal.frontend.toolwindow.TerminalToolWindowTabsManager
import com.intellij.terminal.frontend.view.TerminalViewSessionState
import io.github.titouanfreville.moonlight.client.isSafeSessionId
import io.github.titouanfreville.moonlight.client.launchCommand
import io.github.titouanfreville.moonlight.client.mcpConfigFlag
import io.github.titouanfreville.moonlight.client.resumeCommand
import io.github.titouanfreville.moonlight.client.shortId
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.concurrent.CompletableFuture
import java.util.concurrent.atomic.AtomicBoolean

/**
 * A terminal tab MoonlightCode opened for a launch, in the IDE's own Terminal tool window.
 *
 * Built on the terminal plugin's tab API — `TerminalToolWindowTabsManager` and
 * `TerminalView` (2025.3+, IJPL-211122) — which replaces the deprecated
 * `TerminalToolWindowManager.createShellWidget`. JetBrains marks it `@ApiStatus.Experimental`:
 * it is the only non-deprecated way to open a terminal tab, and the verifier reports it.
 *
 * Claude Code is the tab's **own process**, run through the operator's login shell
 * (`$SHELL -l -i -c`) so it is found through their `PATH`, version managers and aliases. Being
 * the process is what makes its exit observable — the tab's session terminates when Claude
 * does — and that is what auto-resume watches. The price: the tab ends with Claude rather
 * than dropping back to a prompt.
 */
class ClaudeTerminal private constructor(
    override val project: Project,
    override val name: String,
    val launchId: String,
) : OwnedTerminal {
    /** Completed on the EDT once the tab exists; the endpoint round-trip comes first. */
    private val tab = CompletableFuture<TerminalToolWindowTab>()
    private val exited = AtomicBoolean()

    /**
     * Alive while starting, then until Claude exits or the tab closes. A dead terminal is
     * what makes delivery fall through to the honest path rather than writing into a corpse.
     */
    override val isAlive: Boolean
        get() {
            if (!tab.isDone) return true
            val view = tab.getNow(null)?.view ?: return false
            return view.coroutineScope.isActive && view.sessionState.value != TerminalViewSessionState.Terminated
        }

    override fun sendLine(text: String) = send(text, bracketedPaste = false)

    /**
     * Paste text and press Enter, once Claude is running: a tab is created before its
     * process starts, and text sent into a session that has not started has nowhere to go.
     *
     * With `bracketedPaste`, the text arrives as one paste rather than as typed keys, so an
     * embedded newline is part of the message instead of a premature Enter.
     */
    fun send(text: String, bracketedPaste: Boolean) {
        tab.thenAccept { tab ->
            val view = tab.view
            view.coroutineScope.launch {
                view.sessionState.first { it == TerminalViewSessionState.Running }
                withContext(Dispatchers.EDT) {
                    val builder = view.createSendTextBuilder().shouldExecute()
                    if (bracketedPaste) builder.useBracketedPasteMode()
                    builder.send(text)
                }
            }
        }
    }

    override fun show() {
        tab.thenAccept { tab ->
            ApplicationManager.getApplication().invokeLater({
                val window = ToolWindowManager.getInstance(project).getToolWindow(TERMINAL_TOOL_WINDOW) ?: return@invokeLater
                window.show {
                    window.contentManager.setSelectedContent(tab.content)
                    IdeFocusManager.getInstance(project).requestFocus(tab.view.preferredFocusableComponent, true)
                }
            }, project.disposed)
        }
    }

    /** Close the tab — a relaunch replaces a dead one rather than leaving it beside the new one. */
    fun close() {
        tab.thenAccept { tab ->
            ApplicationManager.getApplication().invokeLater({
                tab.content.manager?.removeContent(tab.content, true)
            }, project.disposed)
        }
    }

    /** What happens to a launch's terminal, as its owner needs to hear it. */
    interface Watcher {
        /** Claude exited; the tab may still be open on its last output. Called on the EDT. */
        fun exited(terminal: ClaudeTerminal)

        /** The tab was closed while Claude was still running. Called on the EDT. */
        fun closed(terminal: ClaudeTerminal)
    }

    companion object {
        /**
         * Open a terminal tab for `launchId` and run Claude Code in it — a fresh session pinned
         * to the id (`--session-id`), or, with `resume`, the conversation it was last in.
         *
         * Must be called on the EDT. The session's MCP endpoint is bound first (a loopback
         * round-trip off the EDT), because its URL goes into the command line.
         */
        fun launch(
            project: Project,
            launchId: String,
            cwd: String?,
            resume: Resume?,
            watcher: Watcher,
        ): ClaudeTerminal {
            val terminal = ClaudeTerminal(project, "Claude (${shortId(resume?.conversation ?: launchId)})", launchId)
            ApplicationManager.getApplication().executeOnPooledThread {
                val command = terminal.command(resume)
                if (command == null) {
                    terminal.tab.completeExceptionally(IllegalStateException("refused to launch"))
                    return@executeOnPooledThread
                }
                ApplicationManager.getApplication().invokeLater({ terminal.open(cwd, command, watcher) }, project.disposed)
            }
            return terminal
        }
    }

    /** What to resume: the conversation, or `null` for a launch that never wrote one. */
    data class Resume(val conversation: String?)

    private fun open(cwd: String?, command: String, watcher: Watcher) {
        val created = TerminalToolWindowTabsManager.getInstance(project).createTabBuilder()
            .workingDirectory(cwd)
            .shellCommand(loginShell(command))
            .tabName(name)
            .requestFocus(true)
            .createTab()
        tab.complete(created)
        show()
        val view = created.view
        view.coroutineScope.launch {
            view.sessionState.first { it == TerminalViewSessionState.Terminated }
            if (exited.compareAndSet(false, true)) withContext(Dispatchers.EDT) { watcher.exited(this@ClaudeTerminal) }
        }
        // The tab ended. If Claude had already terminated, it is an exit the watcher above may
        // not have seen — a tab can close with its process, cancelling the scope first.
        // Otherwise it was closed with Claude still running: the operator ended it on purpose.
        view.coroutineScope.coroutineContext[Job]?.invokeOnCompletion {
            val terminated = view.sessionState.value == TerminalViewSessionState.Terminated
            if (terminated && !exited.compareAndSet(false, true)) return@invokeOnCompletion
            if (!terminated && exited.get()) return@invokeOnCompletion
            ApplicationManager.getApplication().invokeLater({
                if (terminated) watcher.exited(this) else watcher.closed(this)
            }, project.disposed)
        }
    }

    /**
     * The command line, with this launch's `moonlight` MCP server wired in.
     *
     * Best-effort, like the desktop app: a backend too old to bind an endpoint still gets a
     * session, with a warning that says what is missing — a session launched without it is
     * gated but cannot propose a plan or request a phase, and would not say why.
     */
    private fun command(resume: Resume?): String? {
        // The id is ours, but it is interpolated into a shell line, so it is checked rather
        // than trusted: an id that is not the shape we mint means something upstream changed.
        if (!isSafeSessionId(launchId)) {
            MoonlightService.notify("MoonlightCode: refusing to launch — unexpected session id shape ($launchId).", NotificationType.ERROR, project)
            return null
        }
        var flags = ""
        try {
            val url = MoonlightService.getInstance().control.mcpEndpoint(launchId)
            val flag = mcpConfigFlag(url)
            if (flag == null) {
                // A non-loopback or non-http endpoint is not something we put on a command line.
                MoonlightService.notify(
                    "MoonlightCode: refusing an unexpected MCP endpoint ($url). The session starts without the moonlight verbs.",
                    NotificationType.WARNING,
                    project,
                )
            } else {
                flags = flag
            }
        } catch (e: Exception) {
            MoonlightService.notify(
                "MoonlightCode: no MCP endpoint for this session (${e.message}). It starts without the moonlight verbs — it cannot propose a plan or request a phase.",
                NotificationType.WARNING,
                project,
            )
        }
        // Both export the launch id, which is what keeps this session's verbs working if it
        // later `/resume`s or `/clear`s into another conversation — and across a relaunch.
        return if (resume == null) launchCommand(launchId, flags) else resumeCommand(launchId, resume.conversation, flags)
    }
}

/** The operator's login, interactive shell running `command` — their environment, not the IDE's. */
internal fun loginShell(command: String): List<String> {
    val shell = System.getenv("SHELL")?.takeIf { it.isNotBlank() } ?: "/bin/sh"
    return listOf(shell, "-l", "-i", "-c", command)
}
