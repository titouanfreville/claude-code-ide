package io.github.titouanfreville.moonlight.core

import com.intellij.openapi.project.Project
import io.github.titouanfreville.moonlight.client.DiscoverableSession
import java.util.concurrent.ConcurrentHashMap

/**
 * The registry behind [OwnedTerminals], keyed by **launch id** — the id minted when the
 * terminal was started.
 *
 * A launched process can change conversation (`/resume`, `/clear`), so the conversation
 * a terminal hosts is looked up through the daemon's `launch_id` on each session rather
 * than fixed at launch: a review for the resumed conversation still finds the terminal it
 * is running in.
 *
 * A closed terminal is dropped the moment it is looked up, which is what makes delivery
 * fall through to the honest path rather than writing into a corpse.
 */
class SessionTerminals(private val sessions: () -> List<DiscoverableSession>) : OwnedTerminals {
    private val owned = ConcurrentHashMap<String, OwnedTerminal>()

    override fun get(sessionId: String): OwnedTerminal? {
        val launch = if (owned.containsKey(sessionId)) {
            sessionId
        } else {
            sessions().firstOrNull { it.sessionId == sessionId }?.launchId ?: return null
        }
        return live(launch)
    }

    /** The terminal registered for a launch, alive or not — who speaks for it now. */
    fun peek(launch: String): OwnedTerminal? = owned[launch]

    /** The launch's terminal, if it is still running. */
    fun live(launch: String): OwnedTerminal? {
        val terminal = owned[launch] ?: return null
        if (!terminal.isAlive || terminal.project.isDisposed) {
            owned.remove(launch, terminal)
            return null
        }
        return terminal
    }

    /** Launched through the project's [SessionLaunches], so it is remembered and can be resumed. */
    override fun start(project: Project, sessionId: String, cwd: String?): OwnedTerminal =
        SessionLaunches.of(project).launch(sessionId, cwd)

    override fun adopt(sessionId: String, terminal: OwnedTerminal) {
        owned[sessionId] = terminal
    }

    /** The conversations the launches in `project` are in now. */
    override fun ownedIn(project: Project): Set<String> {
        val known = sessions()
        return owned.keys
            .filter { launch -> live(launch)?.project == project }
            .mapTo(HashSet()) { launch -> currentConversation(launch, known) }
    }
}

/**
 * The conversation an id names now: itself while it is a known conversation, else the
 * conversation its launch moved to, else itself (not yet detected).
 */
fun currentConversation(id: String, known: List<DiscoverableSession>): String =
    known.firstOrNull { it.sessionId == id }?.sessionId
        ?: known.firstOrNull { it.launchId == id }?.sessionId
        ?: id
