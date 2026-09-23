/**
 * Name of the daemon executable on `PATH`.
 *
 * Windows needs the `.exe`: `PATH` entries there hold `moonlightd.exe`, so a probe
 * for the extension-less name matches nothing and autostart reports `no-binary`
 * forever on an otherwise correct install.
 */
export declare function daemonBinaryName(platform?: string): string;
/**
 * The root MoonlightCode anchors its state to — `MOONLIGHT_HOME` when set and
 * absolute, else the home directory. Same rule as `moonlight_core::support::anchor`;
 * a client that ignored the override would read the discovery file of a daemon it is
 * not talking to.
 */
export declare function stateAnchor(): string;
/** Where a daemon publishes its port. */
export declare function discoveryPath(): string;
/**
 * Reads `~/.moonlight/control.json`, written by either `moonlightd` (headless) or
 * the desktop app's embedded control API — same file, same shape, so a client never
 * has to know which of the two is actually running.
 *
 * It lives here rather than beside the request helper because the event stream needs
 * the same answer, and two readers of the discovery file would eventually disagree
 * about which daemon this window is talking to.
 */
export declare function controlBaseUrl(): string | undefined;
/**
 * The daemon to start, or `undefined` when there is none to be found.
 *
 * Takes its inputs as arguments so each branch is testable without a filesystem the
 * test has to fake. Unlike the Zed fork there is no sibling binary to prefer — the
 * extension host is Node, not a build output — so an explicit override and `PATH` are
 * the only two answers.
 */
export declare function resolveDaemonBinary(override: string | undefined, searchPath: string | undefined, exists?: (candidate: string) => boolean, binaryName?: string): string | undefined;
/** The environment a started daemon needs to govern the same state as this window. */
export declare function spawnEnv(): Record<string, string | undefined>;
/** Why an autostart did not happen, for the caller to surface. */
export type DaemonStartResult = {
    kind: 'started';
    pid: number | undefined;
    binary: string;
} | {
    kind: 'no-binary';
} | {
    kind: 'backing-off';
} | {
    kind: 'disabled';
} | {
    kind: 'failed';
    error: string;
};
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
/**
 * Start a daemon if the caller's last request failed.
 *
 * Call this from the poll loop's error path rather than once at activation: a daemon
 * that dies under a running window has to come back too, and the poll loop is already
 * the thing that finds out.
 */
export declare function ensureDaemon(options?: DaemonOptions, now?: number): DaemonStartResult;
/**
 * Called when the backend answers again, so the next outage starts from an eager
 * retry rather than mid-backoff.
 */
export declare function daemonReachable(): void;
