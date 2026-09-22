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
 * MoonlightCode Status — the at-a-glance answer to "is anything actually governed?"
 *
 * This is the fail-open visibility problem the whole client family started from: the
 * hooks can be uninstalled, or the backend can be down, and Claude Code carries on
 * perfectly happily with nothing gating it. Silence looks identical to safety, so
 * this says which one you are in.
 */
const vscode = __importStar(require("vscode"));
const controlApi = __importStar(require("moonlight-control-client"));
const core_1 = require("./core");
const usage_1 = require("./usage");
/** Whether `session` was started under one of this window's workspace folders. */
function inThisWorkspace(session) {
    const folders = (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath);
    return Boolean(session.root && folders.some((f) => session.root === f || session.root.startsWith(`${f}/`)));
}
/**
 * Sessions in this workspace that are mid-turn while `shown` is not.
 *
 * Claude Code exposes no session id to other extensions, so which session you are
 * typing to is inferred. This is the one piece of positive evidence available that
 * the inference went wrong: something else here is working, and the session being
 * reported on is sitting idle.
 */
function workingElsewhere(sessions, shown) {
    if (shown.status === 'Running') {
        return [];
    }
    return sessions.filter((s) => s.session_id !== shown.session_id && s.status === 'Running' && inThisWorkspace(s));
}
async function activate(context) {
    const core = await (0, core_1.requireCore)();
    if (!core) {
        return;
    }
    const item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    // Clicking the indicator goes to the review itself, not to a file list.
    context.subscriptions.push(item);
    // The usage readout is a separate item, on the other side of the bar: it answers
    // "how much allowance is left", which is unrelated to whether anything is gated,
    // and folding both into one item would mean losing one of them to truncation.
    // `usage()` is optional on the API so a newer status extension still runs against
    // an older core — it just has nothing to show.
    const renderUsage = (0, usage_1.registerUsageItem)(context, () => ({ usage: core.usage?.(), sessionId: core.activeSession()?.sessionId }), () => core.refresh(), () => core.backendError());
    const render = () => {
        renderUsage();
        const error = core.backendError();
        if (error) {
            item.text = '$(circle-slash) MoonlightCode';
            item.backgroundColor = new vscode.ThemeColor('statusBarItem.warningBackground');
            item.tooltip = `No MoonlightCode backend reachable, so nothing is gated.\n${error}`;
            item.show();
            return;
        }
        const hooks = core.gating();
        const hooksOn = hooks.length > 0 && hooks.every((h) => h.installed);
        const active = core.activeSession();
        const session = active
            ? core.sessions().find((s) => s.session_id === active.sessionId)
            : undefined;
        const warn = new vscode.ThemeColor('statusBarItem.warningBackground');
        const lines = [];
        // Clicking should do the thing the bar is complaining about, so the command is
        // chosen per state rather than fixed.
        item.command = 'moonlight.sessionControl.setPhase';
        if (!hooksOn) {
            // Necessary but not sufficient — and the most misleading thing this bar can
            // say, because a phase means nothing without hooks to enforce it.
            item.text = '$(unlock) Not gated';
            item.backgroundColor = warn;
            lines.push('Hooks are NOT installed — no session is gated, whatever its phase.');
            lines.push('Run `moonlightd hooks install`.');
        }
        else if (!core.sessions().some((s) => s.adopted)) {
            // Nothing is adopted anywhere, so "which session?" would be the wrong
            // question — there is no governed session to be pointing at.
            item.text = '$(unlock) No adopted session';
            item.backgroundColor = warn;
            item.command = 'moonlight.sessionControl.adopt';
            lines.push('Hooks are installed, but no session is adopted — so nothing is gated.', '', 'An unadopted session is never denied anything, whatever phase it reports.', 'Click here to adopt one.');
        }
        else if (!session) {
            item.text = '$(question) Which session?';
            item.backgroundColor = undefined;
            item.command = 'moonlight.sessionControl.setActive';
            lines.push('Sessions are adopted, but MoonlightCode cannot tell which one you are', 'working in.', '', 'Claude Code does not expose a session id to other extensions, so this is only', 'certain for a session MoonlightCode started or that you adopted from this', 'panel. Click here to say which it is.');
        }
        else if (!session.adopted) {
            // The trap: hooks on, so the bar could read "gating ACTIVE", while this
            // session is never gated at all.
            item.text = '$(unlock) Ungoverned';
            item.backgroundColor = warn;
            item.command = 'moonlight.sessionControl.adopt';
            lines.push(`"${session.title ?? session.session_id}" is NOT adopted, so it is never gated.`, 'Run "MoonlightCode: Adopt a Claude Code Session".');
        }
        else {
            const frozen = controlApi.FROZEN_PHASES.includes(session.phase);
            // Another session in this workspace is mid-turn while the one we are reporting
            // on is not. That is positive evidence we are describing the wrong session —
            // and "Plan" shown for a governed session you are not talking to reads as "this
            // conversation is governed", which is the exact opposite of the truth when the
            // one answering you is unadopted.
            const busyElsewhere = workingElsewhere(core.sessions(), session);
            const guessed = active?.how === 'sole';
            if (busyElsewhere.length > 0) {
                item.text = `$(question) ${session.phase} · which session?`;
                item.backgroundColor = warn;
                item.command = 'moonlight.sessionControl.setActive';
                lines.push(`Showing "${session.title ?? session.session_id}" (${session.status}), but`, `${busyElsewhere.length} other session(s) here are mid-turn:`, ...busyElsewhere.map((s) => `  • ${controlApi.sessionLabel(s, core.sessions())} — ${s.status}${s.adopted ? '' : ', NOT adopted'}`), '', 'The phase above may describe a session you are not talking to. Click to say', 'which one you are in.');
            }
            else {
                // The phase *is* the headline: it is the thing that decides whether the agent
                // in front of you can write.
                const mark = frozen ? '$(lock)' : '$(shield)';
                // A guess is labelled in the bar, not only in a tooltip nobody hovers.
                item.text = guessed ? `${mark} ${session.phase} · guess` : `${mark} ${session.phase}`;
                item.backgroundColor = frozen ? warn : undefined;
                lines.push(`"${session.title ?? session.session_id}"`, frozen
                    ? `${session.phase} — project writes DENIED (AI-workspace dirs stay writable)`
                    : `${session.phase} — project writes allowed`, `status: ${session.status}`, '', 'Click to change phase.');
            }
        }
        if (active) {
            lines.push('', active.how === 'panel'
                ? 'Session identified: linked to this agent panel.'
                : active.how === 'owned'
                    ? 'Session identified: MoonlightCode started it.'
                    : active.how === 'pinned'
                        ? 'Session identified: pinned for this workspace.'
                        : 'Session identified: only one running here — a guess. Pin one to be sure.');
        }
        item.tooltip = lines.join('\n');
        item.show();
    };
    render();
    context.subscriptions.push(core.onDidChange(render));
}
function deactivate() { }
//# sourceMappingURL=extension.js.map