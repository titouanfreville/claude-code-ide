package io.github.titouanfreville.moonlight.client

import java.io.File
import java.nio.file.Files
import java.nio.file.Path

/**
 * Finding and starting `moonlightd` — a port of `daemon.ts` in the VS Code client, with
 * the resolution order and the environment handed to the child kept identical, so an
 * operator gets the same daemon whichever editor started it.
 *
 * Racing is safe: the daemon claims a Unix socket before it publishes its port, and a
 * second one exits rather than repointing clients at itself (see `apps/daemon`). So two
 * IDEs opening at once cost one wasted spawn, never a split fleet.
 */

/** The operating systems the path and binary rules distinguish. */
enum class Os {
    Mac, Linux, Windows, Other;

    companion object {
        fun current(osName: String = System.getProperty("os.name") ?: ""): Os {
            val name = osName.lowercase()
            return when {
                name.startsWith("mac") || name.startsWith("darwin") -> Mac
                name.startsWith("linux") -> Linux
                name.startsWith("windows") -> Windows
                else -> Other
            }
        }
    }
}

/**
 * Name of the daemon executable on `PATH`.
 *
 * Windows needs the `.exe`: a probe for the extension-less name matches nothing there,
 * and autostart reports "no binary" forever on an otherwise correct install.
 */
fun daemonBinaryName(os: Os = Os.current()): String = if (os == Os.Windows) "moonlightd.exe" else "moonlightd"

/** Points the autostart at a specific build — for a sandbox run, mainly. */
private const val DAEMON_BIN_VAR = "MOONLIGHT_DAEMON_BIN"

/** Mirrors `moonlight_core::support`'s `MOONLIGHT_HOME`. */
private const val HOME_OVERRIDE_VAR = "MOONLIGHT_HOME"

/** Attempts made back-to-back before the autostart slows down. */
private const val EAGER_ATTEMPTS = 5

/** How long to hold off after [EAGER_ATTEMPTS] failures. */
private const val BACKOFF_MS = 30_000L

/**
 * The root MoonlightCode anchors its state to — `MOONLIGHT_HOME` when set and absolute,
 * else the home directory. Same rule as `moonlight_core::support::anchor`; a client that
 * ignored the override would read the discovery file of a daemon it is not talking to.
 */
fun stateAnchor(
    env: Map<String, String> = System.getenv(),
    home: String = System.getProperty("user.home"),
): String {
    val override = env[HOME_OVERRIDE_VAR]
    return if (!override.isNullOrEmpty() && Path.of(override).isAbsolute) override else home
}

/** Where a daemon publishes its port. */
fun discoveryPath(anchor: String = stateAnchor()): Path = Path.of(anchor, ".moonlight", "control.json")

/**
 * Reads `~/.moonlight/control.json`, written by either `moonlightd` or the desktop app's
 * embedded control API — same file, same shape, so a client never has to know which of
 * the two is running.
 *
 * Read on every call rather than cached: a restarted daemon binds a new ephemeral port,
 * and a cached one would keep this IDE talking to a socket nobody holds.
 */
fun controlBaseUrl(file: Path = discoveryPath()): String? = try {
    val parsed = Json.parse(Files.readString(file)) as? Map<*, *>
    (parsed?.get("port") as? Number)?.let { "http://127.0.0.1:${it.toLong()}" }
} catch (_: Exception) {
    null
}

private fun isExecutableFile(candidate: String): Boolean = File(candidate).let { it.isFile && it.canExecute() }

/**
 * The daemon to start, or `null` when there is none to be found.
 *
 * Takes its inputs as arguments so each branch is testable without a filesystem to fake.
 * An explicit override and `PATH` are the only two answers.
 */
fun resolveDaemonBinary(
    override: String?,
    searchPath: String?,
    exists: (String) -> Boolean = ::isExecutableFile,
    binaryName: String = daemonBinaryName(),
    delimiter: String = File.pathSeparator,
): String? {
    if (!override.isNullOrEmpty()) {
        // Deliberately no fallback: an operator who named a daemon wants that one, and
        // starting a different one silently is how a sandbox ends up governed by the
        // installed build.
        return override.takeIf(exists)
    }
    for (dir in (searchPath ?: "").split(delimiter)) {
        if (dir.isEmpty()) continue
        val candidate = Path.of(dir, binaryName).toString()
        if (exists(candidate)) return candidate
    }
    return null
}

/** The environment a started daemon needs to govern the same state as this IDE. */
fun spawnEnv(env: Map<String, String> = System.getenv()): Map<String, String> {
    val out = env.toMutableMap()
    val override = env[HOME_OVERRIDE_VAR]
    if (override.isNullOrEmpty() || !Path.of(override).isAbsolute) {
        // An override this IDE is not itself using must not reach the child: it would
        // open a different database and bind a different socket.
        out.remove(HOME_OVERRIDE_VAR)
    }
    return out
}

/** Why an autostart did or did not happen, for the caller to surface. */
sealed interface DaemonStartResult {
    data class Started(val pid: Long?, val binary: String) : DaemonStartResult
    data object NoBinary : DaemonStartResult
    data object BackingOff : DaemonStartResult
    data object Disabled : DaemonStartResult
    data class Failed(val error: String) : DaemonStartResult
}

/**
 * What the operator has said about autostart, from the IDE settings.
 *
 * Settings rather than environment because an IDE launched from the Dock inherits
 * launchd's environment, not a shell's — so `MOONLIGHT_DAEMON_BIN` only lands if the IDE
 * was started from a terminal that exported it.
 */
data class DaemonOptions(
    /** Which binary to start. Wins over `MOONLIGHT_DAEMON_BIN` and `PATH`; named but missing starts nothing. */
    val path: String? = null,
    /**
     * Whether this IDE may start a daemon at all. Turning it off does not disable *using*
     * a daemon — only starting one — so an operator running their own is not displaced by
     * a spawn on the poll loop.
     */
    val autostart: Boolean = true,
)

/**
 * Starts a daemon when the caller's last request failed, with a backoff.
 *
 * An instance rather than module state (as in the TS client) so tests get a fresh one.
 * Called from the poll loop's error path rather than once at startup: a daemon that dies
 * under a running IDE has to come back too, and the poll loop is what finds out.
 */
class DaemonLauncher(
    private val env: Map<String, String> = System.getenv(),
    private val spawn: (binary: String, env: Map<String, String>) -> Long? = ::spawnDetached,
) {
    private var attempts = 0
    private var holdUntil = 0L

    @Synchronized
    fun ensure(options: DaemonOptions = DaemonOptions(), now: Long = System.currentTimeMillis()): DaemonStartResult {
        // Checked before the backoff: an operator who turned autostart off is not waiting
        // out a retry window, and counting attempts that will never be made would report a
        // backoff that means nothing.
        if (!options.autostart) return DaemonStartResult.Disabled
        if (now < holdUntil) return DaemonStartResult.BackingOff

        val binary = resolveDaemonBinary(options.path ?: env[DAEMON_BIN_VAR], env["PATH"])
        if (binary == null) {
            recordAttempt(now)
            return DaemonStartResult.NoBinary
        }
        return try {
            val pid = spawn(binary, spawnEnv(env))
            recordAttempt(now)
            DaemonStartResult.Started(pid, binary)
        } catch (e: Exception) {
            recordAttempt(now)
            DaemonStartResult.Failed(e.message ?: e.toString())
        }
    }

    /** The backend answered again: the next outage starts from an eager retry, not mid-backoff. */
    @Synchronized
    fun reachable() {
        attempts = 0
        holdUntil = 0
    }

    /**
     * Counts every attempt that did not end in a reachable daemon, not just the ones that
     * threw. A daemon that starts and immediately dies is a failure too, and counting only
     * errors would retry it every tick for as long as the IDE is open.
     */
    private fun recordAttempt(now: Long) {
        attempts += 1
        if (attempts >= EAGER_ATTEMPTS) holdUntil = now + BACKOFF_MS
    }
}

/**
 * Start the daemon with no stdio attached.
 *
 * The daemon logs to its own file, and pipes held by the IDE would tie it to a process
 * that is about to exit. The JVM does not kill its children on exit, so the daemon
 * outlives the IDE — which is the point: it governs sessions whether or not an editor is
 * open. (There is no portable `setsid` from the JVM; an IDE started from a terminal and
 * killed with Ctrl-C takes its process group with it, daemon included.)
 */
private fun spawnDetached(binary: String, env: Map<String, String>): Long? {
    val builder = ProcessBuilder(binary)
        .redirectInput(ProcessBuilder.Redirect.from(File(if (Os.current() == Os.Windows) "NUL" else "/dev/null")))
        .redirectOutput(ProcessBuilder.Redirect.DISCARD)
        .redirectError(ProcessBuilder.Redirect.DISCARD)
    builder.environment().apply {
        clear()
        putAll(env)
    }
    return builder.start().pid()
}
