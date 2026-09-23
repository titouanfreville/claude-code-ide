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
import * as childProcess from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

/**
 * Name of the daemon executable on `PATH`.
 *
 * Windows needs the `.exe`: `PATH` entries there hold `moonlightd.exe`, so a probe
 * for the extension-less name matches nothing and autostart reports `no-binary`
 * forever on an otherwise correct install.
 */
export function daemonBinaryName(platform: string = process.platform): string {
  return platform === 'win32' ? 'moonlightd.exe' : 'moonlightd';
}

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
export function stateAnchor(): string {
  const override = process.env[HOME_OVERRIDE_VAR];
  if (override && path.isAbsolute(override)) {
    return override;
  }
  return os.homedir();
}

/** Where a daemon publishes its port. */
export function discoveryPath(): string {
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
export function controlBaseUrl(): string | undefined {
  try {
    const raw = fs.readFileSync(discoveryPath(), 'utf8');
    const parsed = JSON.parse(raw) as { port?: number };
    if (typeof parsed.port !== 'number') {
      return undefined;
    }
    return `http://127.0.0.1:${parsed.port}`;
  } catch {
    return undefined;
  }
}

function isExecutableFile(candidate: string): boolean {
  try {
    if (!fs.statSync(candidate).isFile()) {
      return false;
    }
    fs.accessSync(candidate, fs.constants.X_OK);
    return true;
  } catch {
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
export function resolveDaemonBinary(
  override: string | undefined,
  searchPath: string | undefined,
  exists: (candidate: string) => boolean = isExecutableFile,
  binaryName: string = daemonBinaryName()
): string | undefined {
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
    const candidate = path.join(dir, binaryName);
    if (exists(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

/** The environment a started daemon needs to govern the same state as this window. */
export function spawnEnv(): Record<string, string | undefined> {
  const override = process.env[HOME_OVERRIDE_VAR];
  const env = { ...process.env };
  if (override && path.isAbsolute(override)) {
    env[HOME_OVERRIDE_VAR] = override;
  } else {
    // An override this window is not itself using must not reach the child: it would
    // open a different database and bind a different socket.
    delete env[HOME_OVERRIDE_VAR];
  }
  return env;
}

/** Why an autostart did not happen, for the caller to surface. */
export type DaemonStartResult =
  | { kind: 'started'; pid: number | undefined; binary: string }
  | { kind: 'no-binary' }
  | { kind: 'backing-off' }
  | { kind: 'disabled' }
  | { kind: 'failed'; error: string };

/**
 * What the operator has said about autostart, from editor settings.
 *
 * Settings rather than environment because the extension host reads `process.env` of
 * the editor process, which on macOS is launchd's environment, not a shell's — so
 * `MOONLIGHT_DAEMON_BIN` only lands if the editor was started from a terminal that
 * exported it, or via `launchctl setenv`. Neither is a reasonable thing to ask of
 * someone who just wants this window to point at their own build.
 */
export interface DaemonOptions {
  /**
   * Which binary to start. Wins over {@link DAEMON_BIN_VAR} and `PATH`. Same
   * all-or-nothing rule as the env override: named but missing starts nothing.
   */
  path?: string;
  /**
   * Whether this window may start a daemon at all. `false` suits an operator who runs
   * one themselves — from a terminal, a debugger, or a service manager — where an
   * editor spawning a second one on a five-second timer means their process loses the
   * socket race and their build is not the one governing anything.
   *
   * It does not disable *using* a daemon: the window still talks to whichever one is
   * running. It only stops this window from starting one.
   */
  autostart?: boolean;
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
export function ensureDaemon(
  options: DaemonOptions = {},
  now: number = Date.now()
): DaemonStartResult {
  // Checked before the backoff: an operator who turned autostart off is not waiting
  // out a retry window, and counting attempts we are never going to make would report
  // a backoff that means nothing.
  if (options.autostart === false) {
    return { kind: 'disabled' };
  }
  if (now < holdUntil) {
    return { kind: 'backing-off' };
  }

  const binary = resolveDaemonBinary(
    options.path ?? process.env[DAEMON_BIN_VAR],
    process.env.PATH
  );
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
  } catch (err) {
    recordAttempt(now);
    return { kind: 'failed', error: err instanceof Error ? err.message : String(err) };
  }
}

/**
 * Called when the backend answers again, so the next outage starts from an eager
 * retry rather than mid-backoff.
 */
export function daemonReachable(): void {
  attempts = 0;
  holdUntil = 0;
}

/**
 * Counts every attempt that did not end in a reachable daemon — not just the ones
 * that threw. A daemon that starts and immediately dies is a failure too, and
 * counting only errors would retry it every tick for as long as the window is open.
 */
function recordAttempt(now: number): void {
  attempts += 1;
  if (attempts >= EAGER_ATTEMPTS) {
    holdUntil = now + BACKOFF_MS;
  }
}
