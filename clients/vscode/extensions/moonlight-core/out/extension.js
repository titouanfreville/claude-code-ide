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
exports.activate = activate;
exports.deactivate = deactivate;
/**
 * MoonlightCode Core — the one backend connection the other extensions share.
 *
 * It contributes no UI. It exists so that the feature extensions can ship and be
 * installed separately without each one opening its own connection, running its own
 * poll loop, and forming its own opinion about what the fleet is doing.
 */
const vscode = __importStar(require("vscode"));
const controlApi = __importStar(require("moonlight-control-client"));
const daemon_install_1 = require("./daemon-install");
const gate_state_1 = require("./gate-state");
const terminals_1 = require("./terminals");
/**
 * How often the backend is polled. One loop for every extension — the point of
 * putting this in core rather than in each feature.
 */
const POLL_INTERVAL_MS = 5000;
/**
 * What the operator has configured about starting the daemon.
 *
 * Read per call rather than captured at activation, so changing the setting takes
 * effect on the next poll instead of requiring a window reload — the point of a
 * setting over an environment variable is that you can change your mind without
 * restarting the editor.
 */
function daemonOptions() {
    const config = vscode.workspace.getConfiguration('moonlight');
    const path = config.get('daemon.path') ?? undefined;
    return {
        // An empty string in settings.json reads as "unset", not as a path to nothing.
        path: path?.trim() ? path.trim() : undefined,
        autostart: config.get('daemon.autostart') ?? true,
    };
}
function activate(context) {
    const changed = new vscode.EventEmitter();
    context.subscriptions.push(changed);
    let sessions = [];
    let gating = [];
    let usage;
    let backendError;
    const terminals = new terminals_1.SessionTerminals(context);
    // A daemon downloaded on demand, once it has been verified. Held here rather than
    // re-derived per tick so the five-second poll does not stat the cache forever, and
    // so a single in-flight download is shared by every tick that arrives during it.
    let downloadedDaemon;
    let installing;
    /**
     * Fetch the daemon the first time `PATH` turns up empty.
     *
     * Only on `no-binary`: an operator who pointed at their own build, or turned
     * autostart off, has already answered the question this would be asking.
     */
    const installDaemon = () => {
        if (downloadedDaemon || installing) {
            return;
        }
        installing = (async () => {
            const result = await (0, daemon_install_1.ensureDownloadedDaemon)(context.globalStorageUri.fsPath, controlApi.daemonBinaryName());
            if (result.kind === 'installed' || result.kind === 'cached') {
                downloadedDaemon = result.binary;
            }
            else if (result.kind === 'failed') {
                console.warn(`[moonlight] daemon download failed: ${result.error}`);
            }
        })().finally(() => {
            installing = undefined;
        });
    };
    const refresh = async () => {
        try {
            // Usage rides the same tick: the server caches the account quota, so the
            // extra call costs a directory read rather than a round-trip to Anthropic.
            //
            // It is fetched SEPARATELY and allowed to fail. Sessions and gating are the
            // load-bearing pair — if either is unreachable the backend genuinely is, and
            // the status bar must say so. Usage is decoration. Putting all three in one
            // `Promise.all` made a backend that simply predates `/control/usage` report
            // as entirely down: one 404 rejected the whole tick, so no session list, no
            // gating status, and every dependent extension concluded it had no backend —
            // a total blackout caused by a missing quota reading.
            const [nextSessions, nextGating] = await Promise.all([
                controlApi.discoverableSessions(),
                controlApi.gatingStatus(),
            ]);
            // Keeps the previous reading rather than blanking it, so an endpoint that
            // blips does not make the figures flicker.
            const nextUsage = await controlApi.usage().catch(() => usage);
            // Only fire when something actually moved: this ticks every five seconds, and
            // waking every dependent each time would make them redraw for nothing.
            const same = backendError === undefined &&
                JSON.stringify(nextSessions) === JSON.stringify(sessions) &&
                JSON.stringify(nextGating) === JSON.stringify(gating) &&
                JSON.stringify(nextUsage) === JSON.stringify(usage);
            sessions = nextSessions;
            gating = nextGating;
            usage = nextUsage;
            backendError = undefined;
            controlApi.daemonReachable();
            if (!same) {
                changed.fire();
            }
        }
        catch (err) {
            const message = err instanceof Error ? err.message : String(err);
            // A window that opens is expected to make sure the daemon is running: it is
            // what governs sessions, so reporting "no backend" and leaving it at that means
            // the fleet runs ungoverned while this extension looks like it is working.
            // Racing another window is safe — the loser exits (see apps/daemon).
            const options = daemonOptions();
            const started = controlApi.ensureDaemon({
                ...options,
                // An explicit setting always wins: a download must never quietly replace the
                // build an operator named.
                path: options.path ?? downloadedDaemon,
            });
            if (started.kind === 'no-binary' && options.autostart !== false && !options.path) {
                installDaemon();
            }
            // Say *why* there is no backend when the reason is a choice or a missing
            // binary. "Connection refused" alone sends an operator looking for a crash when
            // the answer is that this window was told not to start one.
            const reported = started.kind === 'no-binary'
                ? `${message} (moonlightd not found)`
                : started.kind === 'disabled'
                    ? `${message} (autostart is off — start moonlightd yourself, or enable moonlight.daemon.autostart)`
                    : message;
            // A backend that isn't running is an ordinary state, not a failure to report
            // loudly — the status extension renders it. Keep the last known lists so a
            // brief blip doesn't blank every view.
            if (backendError !== reported) {
                backendError = reported;
                changed.fire();
            }
        }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_INTERVAL_MS);
    context.subscriptions.push({ dispose: () => clearInterval(timer) });
    // ---------------------------------------------------------------------------
    // The gate: holds, and the plans behind them.
    //
    // This rides the event stream rather than the poll, because a hold is not a fact
    // that can wait five seconds for its turn — the session is stopped until someone
    // answers it. The poll stays the authority for everything else.
    // ---------------------------------------------------------------------------
    const heldChanged = new vscode.EventEmitter();
    context.subscriptions.push(heldChanged);
    const gate = new gate_state_1.GateState();
    /** Re-read the outstanding holds from the server, after a desync or at activation. */
    const resyncHolds = async () => {
        try {
            gate.resync(await controlApi.pendingApprovals());
            heldChanged.fire();
        }
        catch {
            // An unreachable backend is already reported by the poll loop; a second,
            // louder complaint from here would only duplicate it.
        }
    };
    const stream = controlApi.subscribeEvents({
        onEvent: (event) => {
            if (gate.apply(event)) {
                heldChanged.fire();
            }
        },
        onDesync: () => void resyncHolds(),
    });
    context.subscriptions.push(stream);
    void resyncHolds();
    // Survives a window reload: the operator should not have to re-pin every time.
    const PIN_KEY = 'moonlight.pinnedSession';
    const PANEL_PINS_KEY = 'moonlight.panelSessions';
    let pinned = context.workspaceState.get(PIN_KEY);
    let panelPins = context.workspaceState.get(PANEL_PINS_KEY) ?? {};
    /**
     * A handle for the focused agent panel. `TabInputWebview` exposes only a view
     * type, so the tab's label is what separates two panels of the same kind.
     */
    const activePanelKey = () => {
        const tab = vscode.window.tabGroups.activeTabGroup?.activeTab;
        if (!tab || !(tab.input instanceof vscode.TabInputWebview)) {
            return undefined;
        }
        return `${tab.input.viewType}::${tab.label}`;
    };
    // Switching tabs switches which session the UI is about, so redraw on tab change.
    context.subscriptions.push(vscode.window.tabGroups.onDidChangeTabGroups(() => changed.fire()), vscode.window.tabGroups.onDidChangeTabs(() => changed.fire()));
    const activeSession = () => {
        const known = sessions;
        // 1. The session linked to the panel in front of you. Most specific, and the
        //    only answer that stays right when several agent panels are open at once.
        const key = activePanelKey();
        const forPanel = key ? panelPins[key] : undefined;
        if (forPanel && known.some((s) => s.session_id === forPanel)) {
            return { sessionId: forPanel, how: 'panel' };
        }
        // 2. A session we launched — the id is certain, not inferred. Only when there is
        //    exactly one, since several owned terminals are as ambiguous as none.
        const owned = known.filter((s) => terminals.has(s.session_id));
        if (owned.length === 1) {
            return { sessionId: owned[0].session_id, how: 'owned' };
        }
        // 3. The operator's workspace-wide pin, while it still exists.
        if (pinned && known.some((s) => s.session_id === pinned)) {
            return { sessionId: pinned, how: 'pinned' };
        }
        // 3. Exactly one session running in this workspace. A guess, labelled as one —
        //    and only when there is no competition, because naming the wrong session as
        //    governed is worse than admitting we don't know.
        const folders = (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
        const here = known.filter((s) => s.root && folders.some((f) => s.root === f || s.root.startsWith(`${f}/`)));
        const running = here.filter((s) => s.status === 'Running');
        const candidates = running.length > 0 ? running : here;
        return candidates.length === 1
            ? { sessionId: candidates[0].session_id, how: 'sole' }
            : undefined;
    };
    return {
        version: 2,
        activeSession,
        setPinnedSession: (sessionId) => {
            pinned = sessionId;
            void context.workspaceState.update(PIN_KEY, sessionId);
            changed.fire();
        },
        activePanelKey,
        pinToActivePanel: (sessionId) => {
            const key = activePanelKey();
            if (!key) {
                return false;
            }
            panelPins = { ...panelPins };
            if (sessionId) {
                panelPins[key] = sessionId;
            }
            else {
                delete panelPins[key];
            }
            void context.workspaceState.update(PANEL_PINS_KEY, panelPins);
            changed.fire();
            return true;
        },
        sessions: () => sessions,
        gating: () => gating,
        usage: () => usage,
        sessionUsage: (sessionId) => usage?.sessions.find((s) => s.session_id === sessionId),
        backendError: () => backendError,
        refresh,
        onDidChange: changed.event,
        heldApproval: (sessionId) => gate.approval(sessionId),
        heldApprovals: () => gate.approvals(),
        onDidChangeHeldApprovals: heldChanged.event,
        proposedPlan: (sessionId) => gate.plan(sessionId),
        terminals,
    };
}
function deactivate() { }
//# sourceMappingURL=extension.js.map