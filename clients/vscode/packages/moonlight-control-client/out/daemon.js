"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.stateAnchor = stateAnchor;
exports.discoveryPath = discoveryPath;
exports.controlBaseUrl = controlBaseUrl;
exports.resolveDaemonBinary = resolveDaemonBinary;
exports.spawnEnv = spawnEnv;
exports.ensureDaemon = ensureDaemon;
exports.daemonReachable = daemonReachable;
/**
 * Starting `moonlightd` when nothing else has.
 *
 * Every IDE that opens is expected to make sure the daemon is running, because the
 * daemon is what governs sessions — an editor that merely reports "no backend" leaves
 * the fleet ungoverned while looking like it is doing its job. The Zed fork does the
 * same thing in `moonlight_ui::daemon`; the resolution order and the environment
 * handed to the child are kept identical on purpose.
 *
 * Racing is safe: the daemon claims a Unix socket before it publishes its port, and a
 * second one exits rather than repointing clients at itself (see
 * `apps/daemon/src/main.rs`). So two windows opening at once cost one wasted spawn,
 * never a split fleet.
 */
const childProcess = __importStar(require("child_process"));
const fs = __importStar(require("fs"));
const os = __importStar(require("os"));
const path = __importStar(require("path"));
/** Name of the daemon executable on `PATH`. */
const DAEMON_BIN = 'moonlightd';
/** Points the autostart at a specific build — for a sandbox run, mainly. */
const DAEMON_BIN_VAR = 'MOONLIGHT_DAEMON_BIN';
/** Mirrors `moonlight_core::support`'s `MOONLIGHT_HOME`. */
const HOME_OVERRIDE_VAR = 'MOONLIGHT_HOME';
/** Attempts made back-to-back before the autostart slows down. */
const EAGER_ATTEMPTS = 5;
/** How long to hold off after {@link EAGER_ATTEMPTS} failures. */
const BACKOFF_MS = 30_000;
/**
 * The root MoonlightCode anchors its state to — `MOONLIGHT_HOME` when set and
 * absolute, else the home directory. Same rule as `moonlight_core::support::anchor`;
 * a client that ignored the override would read the discovery file of a daemon it is
 * not talking to.
 */
function stateAnchor() {
    const override = process.env[HOME_OVERRIDE_VAR];
    if (override && path.isAbsolute(override)) {
        return override;
    }
    return os.homedir();
}
/** Where a daemon publishes its port. */
function discoveryPath() {
    return path.join(stateAnchor(), '.moonlight', 'control.json');
}
/**
 * Reads `~/.moonlight/control.json`, written by either `moonlightd` (headless) or
 * the desktop app's embedded control API — same file, same shape, so a client never
 * has to know which of the two is actually running.
 *
 * It lives here rather than beside the request helper because the event stream needs
 * the same answer, and two readers of the discovery file would eventually disagree
 * about which daemon this window is talking to.
 */
function controlBaseUrl() {
    try {
        const raw = fs.readFileSync(discoveryPath(), 'utf8');
        const parsed = JSON.parse(raw);
        if (typeof parsed.port !== 'number') {
            return undefined;
        }
        return `http://127.0.0.1:${parsed.port}`;
    }
    catch {
        return undefined;
    }
}
function isExecutableFile(candidate) {
    try {
        if (!fs.statSync(candidate).isFile()) {
            return false;
        }
        fs.accessSync(candidate, fs.constants.X_OK);
        return true;
    }
    catch {
        return false;
    }
}
/**
 * The daemon to start, or `undefined` when there is none to be found.
 *
 * Takes its inputs as arguments so each branch is testable without a filesystem the
 * test has to fake. Unlike the Zed fork there is no sibling binary to prefer — the
 * extension host is Node, not a build output — so an explicit override and `PATH` are
 * the only two answers.
 */
function resolveDaemonBinary(override, searchPath, exists = isExecutableFile) {
    if (override) {
        if (exists(override)) {
            return override;
        }
        // Deliberately no fallback: an operator who named a daemon wants that one, and
        // starting a different one silently is how a sandbox ends up governed by the
        // installed build.
        return undefined;
    }
    for (const dir of (searchPath ?? '').split(path.delimiter)) {
        if (!dir) {
            continue;
        }
        const candidate = path.join(dir, DAEMON_BIN);
        if (exists(candidate)) {
            return candidate;
        }
    }
    return undefined;
}
/** The environment a started daemon needs to govern the same state as this window. */
function spawnEnv() {
    const override = process.env[HOME_OVERRIDE_VAR];
    const env = { ...process.env };
    if (override && path.isAbsolute(override)) {
        env[HOME_OVERRIDE_VAR] = override;
    }
    else {
        // An override this window is not itself using must not reach the child: it would
        // open a different database and bind a different socket.
        delete env[HOME_OVERRIDE_VAR];
    }
    return env;
}
let attempts = 0;
let holdUntil = 0;
/**
 * Start a daemon if the caller's last request failed.
 *
 * Call this from the poll loop's error path rather than once at activation: a daemon
 * that dies under a running window has to come back too, and the poll loop is already
 * the thing that finds out.
 */
function ensureDaemon(options = {}, now = Date.now()) {
    // Checked before the backoff: an operator who turned autostart off is not waiting
    // out a retry window, and counting attempts we are never going to make would report
    // a backoff that means nothing.
    if (options.autostart === false) {
        return { kind: 'disabled' };
    }
    if (now < holdUntil) {
        return { kind: 'backing-off' };
    }
    const binary = resolveDaemonBinary(options.path ?? process.env[DAEMON_BIN_VAR], process.env.PATH);
    if (!binary) {
        recordAttempt(now);
        return { kind: 'no-binary' };
    }
    try {
        // Detached with no stdio: the daemon logs to its own file, and inheriting this
        // window's pipes would keep it tied to an extension host that is about to exit.
        const child = childProcess.spawn(binary, [], {
            detached: true,
            stdio: 'ignore',
            env: spawnEnv(),
        });
        child.unref();
        recordAttempt(now);
        return { kind: 'started', pid: child.pid, binary };
    }
    catch (err) {
        recordAttempt(now);
        return { kind: 'failed', error: err instanceof Error ? err.message : String(err) };
    }
}
/**
 * Called when the backend answers again, so the next outage starts from an eager
 * retry rather than mid-backoff.
 */
function daemonReachable() {
    attempts = 0;
    holdUntil = 0;
}
/**
 * Counts every attempt that did not end in a reachable daemon — not just the ones
 * that threw. A daemon that starts and immediately dies is a failure too, and
 * counting only errors would retry it every tick for as long as the window is open.
 */
function recordAttempt(now) {
    attempts += 1;
    if (attempts >= EAGER_ATTEMPTS) {
        holdUntil = now + BACKOFF_MS;
    }
}
//# sourceMappingURL=daemon.js.map