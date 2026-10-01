package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Json
import io.github.titouanfreville.moonlight.client.SessionStatus

/**
 * The auto-resume rules with no IDE in them — a port of the desktop app's per-project
 * auto-resume (`session_monitor.rs`). Pure, because a wrong answer here either loses a
 * session silently or relaunches one the operator meant to quit.
 */

/** What a project does with the sessions it launched when they go away. */
enum class AutoResumeMode(val label: String) {
    /** Never relaunch; an exit only offers a Resume button. */
    Off("Off — offer Resume when a session exits"),

    /** On IDE start, ask whether to bring back the sessions that were running here. */
    Ask("Ask on IDE start"),

    /** Bring sessions back on IDE start, and relaunch one that died mid-work. */
    Automatic("Automatic — resume on IDE start and after a crash"),
}

/**
 * The prompt typed into a resumed session that was working when it stopped — desktop's
 * `AUTO_RESUME_PROMPT`, word for word, so every surface nudges the same way.
 */
const val AUTO_RESUME_PROMPT: String =
    "You appear to have stopped mid-task. Re-read your recent context and continue where you left off until the work is complete."

/** Relaunches allowed per launch after process exits, so a Claude that dies on launch cannot loop. */
const val MAX_EXIT_RESUMES: Int = 3

/** What to do when a launched Claude process exits with its tab still open. */
enum class ExitDecision { Relaunch, Offer }

/**
 * An exit while working (`Running` at the last reading) is a crash, an OOM, a kill: under
 * [AutoResumeMode.Automatic] it is relaunched, up to [MAX_EXIT_RESUMES] times. An exit at
 * the prompt is the operator quitting — never relaunched, only offered, because quitting
 * on purpose should stick. Stricter than desktop, which relaunches on any exit.
 */
fun decideOnExit(mode: AutoResumeMode, statusAtExit: SessionStatus?, relaunches: Int): ExitDecision =
    if (mode == AutoResumeMode.Automatic && statusAtExit == SessionStatus.Running && relaunches < MAX_EXIT_RESUMES) {
        ExitDecision.Relaunch
    } else {
        ExitDecision.Offer
    }

/**
 * A launch this project started, as remembered across IDE restarts.
 *
 * `conversation` follows the launch through `/resume` and `/clear` — it is what gets
 * resumed; `null` until the daemon has seen the launch write a transcript. `status` is
 * the last one read, which is how a restart knows the session was mid-work.
 */
data class LaunchRecord(
    val launchId: String,
    val cwd: String?,
    val conversation: String?,
    val status: SessionStatus?,
    val title: String?,
) {
    val wasWorking: Boolean get() = status == SessionStatus.Running

    fun encode(): String = Json.write(
        mapOf("cwd" to cwd, "conversation" to conversation, "status" to status?.name, "title" to title),
    )

    /** The record brought up to date with what the daemon reports for this launch now. */
    fun follow(known: List<DiscoverableSession>): LaunchRecord {
        // The conversation the launch is in now beats the one it was minted as: after a
        // `/resume`, both are known, and only the first is still running in this process.
        val current = known.firstOrNull { it.launchId == launchId }
            ?: known.firstOrNull { it.sessionId == launchId }
            ?: return this
        return copy(conversation = current.sessionId, status = current.status, title = current.title ?: title)
    }

    companion object {
        /** `null` for a record this version cannot read — dropped rather than guessed at. */
        fun decode(launchId: String, raw: String): LaunchRecord? = try {
            val fields = Json.parse(raw) as? Map<*, *> ?: return null
            LaunchRecord(
                launchId = launchId,
                cwd = fields["cwd"] as? String,
                conversation = fields["conversation"] as? String,
                status = (fields["status"] as? String)?.let { name -> SessionStatus.entries.firstOrNull { it.name == name } },
                title = fields["title"] as? String,
            )
        } catch (_: Exception) {
            null
        }
    }
}
